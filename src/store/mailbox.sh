#!/usr/bin/env bash
# mailbox.sh — thin file-per-message hand-off between a project's parallel sandboxes.
#
# One sandbox = one branch, but work crosses boxes: "A finished feat/export → B should review it",
# "whoever is free, pick up the migration". The mailbox is the coordination surface: a flat dir of
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
box="$here/mailbox"
mkdir -p "$box"
vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"   # slash-safe identity (matches the registry/journal shard keys)
cmd="${1:-inbox}"; shift || true

ts() { date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?'; }

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
    jq -n --arg from "$vmid" --arg to "$to" --arg kind "$kind" --arg branch "$branch" \
          --arg body "$body" --arg t "$(ts)" \
          '{from:$from, to:$to, kind:$kind, branch:$branch, body:$body, ts:$t, seenBy:[], relayedTo:[], originProject:""}' \
          > "$f" 2>/dev/null && echo "sent → $to [$kind]: $body" || { echo "send failed" >&2; exit 1; }
    ;;

  inbox|stop-check)
    found=0
    out=""
    for f in "$box"/*.json; do
      [ -e "$f" ] || break
      to="$(jq -r '.to // "broadcast"' "$f" 2>/dev/null)"
      [ "$(jq -r '.from // ""' "$f" 2>/dev/null)" = "$vmid" ] && continue   # don't deliver to the sender
      [ "$to" = "broadcast" ] || [ "$to" = "all-projects" ] || [ "$to" = "$vmid" ] || continue
      [ "$(jq -r --arg v "$vmid" '(.seenBy // []) | index($v)' "$f" 2>/dev/null)" = "null" ] || continue
      if [ "$found" -eq 0 ] && [ "$cmd" = "inbox" ]; then
        echo "[mailbox] unread hand-off for $vmid:"
      fi
      found=$((found+1))
      line="$(jq -r '"  • [\(.kind)] from \(.from) on \(.branch): \(.body)"' "$f" 2>/dev/null)"
      if [ "$cmd" = "inbox" ]; then
        printf '%s\n' "$line"
      else
        out="$out$line
"
      fi
      # mark seen (flock per file so concurrent readers don't clobber seenBy)
      (
        flock -w 5 8 || exit 0
        tmp="$(mktemp "${TMPDIR:-/tmp}/mbx.XXXXXX")" || exit 0
        jq --arg v "$vmid" '.seenBy = ((.seenBy // []) + [$v] | unique)' "$f" > "$tmp" 2>/dev/null \
          && mv "$tmp" "$f" || rm -f "$tmp"
      ) 8>"$f.lock" 2>/dev/null || true
      rm -f "$f.lock" 2>/dev/null || true
    done
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
    cutoff="$(date -u -d "-$days days" +%s 2>/dev/null || echo 0)"
    for f in "$box"/*.json; do
      [ -e "$f" ] || break
      mt="$(stat -c %Y "$f" 2>/dev/null || echo 0)"
      [ "$mt" -lt "$cutoff" ] && rm -f "$f" && echo "pruned $(basename "$f")"
    done
    ;;

  *) echo "usage: mailbox.sh {send|inbox|stop-check|list|prune}" >&2; exit 2;;
esac
exit 0
