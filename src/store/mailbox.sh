#!/usr/bin/env bash
# mailbox.sh — thin file-per-message hand-off between a project's parallel sandboxes.
#
# One sandbox = one branch, but work crosses boxes: "A finished feat/export → B should review it",
# "whoever is free, pick up the task". The mailbox is the coordination surface: a flat dir of
# JSON messages under the shared .claude store, so any box sees any message. No daemon, no broker —
# just files on the shared mount, which keeps it runtime-independent. Each message is addressed to one
# vmid, "broadcast" (this project only), "all-projects" (this project + every other project skein
# manages, relayed in by the host — see skein's relay_cross_project_mail), or "project:<repo-id>"
# (exactly one other named project, never delivered locally). A message carries a seenBy list so a
# reader shows it once then stops.
#
# Delivery is TURN-BOUNDARY, not a background watcher: `inbox` is wired from UserPromptSubmit (shows
# unread mail as additional context at the start of a turn) and `stop-check` from Stop (blocks the
# stop with exit 2 + stderr if mail arrived mid-turn, so it can't be missed just because nobody asked).
# Both mark seenBy the instant they deliver, so the same message never blocks twice. This is the fix
# for the one *silent* failure mode skein cannot tolerate: a message that sits unread because nothing
# ever re-checked after SessionStart.
#
# Installed by skein into <store>/skein/bin, so the store is two levels up from this script.
#
# Usage:
#   mailbox.sh send --to <vmid|broadcast|all-projects|project:<id>> \
#                   --kind <review-request|handoff|note|journal|...> \
#                   (--body "text" | --body-file <path|->) [--branch <branch>]
#   mailbox.sh inbox        # unread for THIS box (UserPromptSubmit hook); prints them, marks seen
#   mailbox.sh stop-check   # same delivery, but for the Stop hook: silent exit 0 if nothing unread,
#                           # else prints to stderr and exit 2 (blocks the stop so it can't be missed)
#   mailbox.sh list         # all messages (read-only, no seen mutation)
#   mailbox.sh prune [days] # delete fully-seen messages older than N days (default 14)
set -uo pipefail

here="$(cd "$(dirname "$0")/../.." && pwd)"   # .claude (script lives in .claude/skein/bin)
command -v jq >/dev/null 2>&1 || {
  echo "[skein-mailbox] jq is unavailable; mailbox delivery is disabled (visible in /api/health)" >&2
  exit 0
}
box="$here/mailbox"
mkdir -p "$box"
vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"   # slash-safe identity (matches the registry/journal shard keys)
cmd="${1:-inbox}"; shift || true

ts() { date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?'; }

# Delete only messages whose intended recipients have all seen them. A direct message is complete
# once its target has seen it; a broadcast is complete once every currently registered box other
# than its sender has seen it. Corrupt/ambiguous messages are retained for inspection.
prune_seen() { # $1 = age in days, $2 = noisy (0/1)
  local days="${1:-30}" noisy="${2:-0}" cutoff boxes mt f
  cutoff="$(date -u -d "-$days days" +%s 2>/dev/null || echo 0)"
  [ "$cutoff" -gt 0 ] || return 0
  boxes="$(jq -c 'keys' "$here/sandboxes.json" 2>/dev/null || echo '[]')"
  for f in "$box"/*.json; do
    [ -e "$f" ] || break
    mt="$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null || echo 0)"
    [ "$mt" -lt "$cutoff" ] || continue
    if jq -e --argjson boxes "$boxes" '
      (.seenBy // []) as $seen | (.from // "") as $from | (.to // "broadcast") as $to
      | if ($to == "broadcast" or $to == "all-projects") then
          ([$boxes[] | select(. != $from)]) as $recipients
          | ($recipients | length) > 0 and (($recipients - $seen) | length) == 0
        else $seen | index($to) != null end
    ' "$f" >/dev/null 2>&1; then
      rm -f "$f" "$f.lock" 2>/dev/null || true
      [ "$noisy" = "1" ] && echo "pruned $(basename "$f")"
    fi
  done
}

case "$cmd" in
  send)
    to="broadcast"; kind="note"; body=""; body_file=""; branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
    while [ $# -gt 0 ]; do
      case "$1" in
        --to) to="$2"; shift 2;;
        --kind) kind="$2"; shift 2;;
        --body) body="$2"; shift 2;;
        --body-file) body_file="$2"; shift 2;;
        --branch) branch="$2"; shift 2;;
        *) echo "unknown arg: $1" >&2; exit 2;;
      esac
    done
    # --body-file avoids the caller's shell mangling backticks/$/! in --body TEXT — read the raw
    # bytes from a file (or stdin, "-") instead of a shell-interpolated argument.
    if [ -n "$body_file" ]; then
      if [ "$body_file" = "-" ]; then
        body="$(cat 2>/dev/null)"
      else
        body="$(cat "$body_file" 2>/dev/null)"
      fi
    fi
    [ -z "$body" ] && { echo "send: --body or --body-file required" >&2; exit 2; }
    id="$(date -u +%s%N 2>/dev/null || echo 0)-$vmid-$$"
    f="$box/$id.json"
    tmp="$(mktemp "$box/.mail.XXXXXX" 2>/dev/null)" || exit 1
    jq -n --arg from "$vmid" --arg to "$to" --arg kind "$kind" --arg branch "$branch" \
          --arg body "$body" --arg t "$(ts)" \
          '{from:$from, to:$to, kind:$kind, branch:$branch, body:$body, ts:$t, seenBy:[], relayedTo:[], originProject:""}' \
          > "$tmp" 2>/dev/null && mv "$tmp" "$f" \
          && echo "sent → $to [$kind]: $body" || { rm -f "$tmp"; echo "send failed" >&2; exit 1; }
    ;;

  inbox|stop-check)
    if [ "$cmd" = "stop-check" ]; then
      payload="$(cat 2>/dev/null || true)"
      # A Stop hook that already caused a continuation must not block recursively.
      printf '%s' "$payload" | jq -e '.stop_hook_active == true' >/dev/null 2>&1 && exit 0
    fi
    found=0
    out=""
    for f in "$box"/*.json; do
      [ -e "$f" ] || break
      # Lock/read/mark/rename before surfacing. If marking fails, emit nothing: showing an unmarked
      # message would make it repeat forever and could trap Stop in a continuation loop.
      line="$({
        flock -w 5 8 || exit 1
        jq -e --arg v "$vmid" '
          (.from // "") != $v
          and ((.to // "broadcast") as $to | $to == "broadcast" or $to == "all-projects" or $to == $v)
          and (((.seenBy // []) | index($v)) == null)
        ' "$f" >/dev/null 2>&1 || exit 1
        tmp="$(mktemp "$box/.mail-seen.XXXXXX" 2>/dev/null)" || exit 1
        jq --arg v "$vmid" '.seenBy = ((.seenBy // []) + [$v] | unique)' "$f" >"$tmp" 2>/dev/null \
          && mv "$tmp" "$f" || { rm -f "$tmp"; exit 1; }
        jq -r '"  • [\(.kind // "note")] from \(.from // "?") on \(.branch // "?"): \(.body // "")"' "$f" 2>/dev/null
      } 8>"$f.lock")" || { rm -f "$f.lock" 2>/dev/null || true; continue; }
      rm -f "$f.lock" 2>/dev/null || true
      if [ "$found" -eq 0 ] && [ "$cmd" = "inbox" ]; then
        echo "[mailbox] unread hand-off for $vmid:"
      fi
      found=$((found+1))
      if [ "$cmd" = "inbox" ]; then
        printf '%s\n' "$line"
      else
        out="$out$line
"
      fi
    done
    prune_seen 30 0
    if [ "$cmd" = "stop-check" ] && [ "$found" -gt 0 ]; then
      {
        echo "[mailbox] you have $found unread hand-off(s) — read them before stopping:"
        printf '%s' "$out"
      } >&2
      exit 2
    fi
    [ "$found" -eq 0 ] && exit 0
    ;;

  list)
    for f in "$box"/*.json; do
      [ -e "$f" ] || { echo "(empty)"; break; }
      jq -r '"[\(.ts)] \(.from)→\(.to) [\(.kind)] \(.branch): \(.body)  seenBy=\(.seenBy // [])"' "$f" 2>/dev/null || true
    done
    ;;

  prune)
    days="${1:-14}"
    prune_seen "$days" 1
    ;;

  *) echo "usage: mailbox.sh {send|inbox|stop-check|list|prune}" >&2; exit 2;;
esac
exit 0
