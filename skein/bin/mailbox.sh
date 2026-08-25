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
# Where the OWNER's messages are, and the reason there are two directories rather than one.
#
# `$box` above is in the shared store: every box can write it, so a message found there is one any
# box could have written — including one that filled in `from: "skein"`, which needs no script and
# no trickery, just a file. `$own` is under this box's state directory, which the launcher binds
# READ-ONLY into this namespace: nothing in here can put a message there, so anything found there
# was put there by skein (architecture §9.5 R10).
#
# So provenance is not a field. It is which directory the message was read out of.
own="${SKEIN_STATE:-}/inbox"
# The owner's inbox cannot be written from in here, which is the point — so "already delivered" is
# remembered in this box's own HOME instead. Private per box: another box cannot silence the owner
# by marking its messages seen, and a box tampering with its own only repeats or misses its own mail.
seen_file="${HOME:-/tmp}/.skein-mail-seen"
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
# Loudly here, and not `exit 0` as the hooks do: this is a command the agent runs and reads the
# answer of, so silence would look like an empty inbox — the one reading a lost identity must never
# produce. What it would cost otherwise is worse than a misfiled file: mail addressed to this box
# would go undelivered because the address no longer matches, every outgoing message would claim to
# come from a box that does not exist, and `seenBy` would fill with the sandbox's name.
if [ -n "${SKEIN_BOX:-}" ]; then
  vmid="$SKEIN_BOX"
elif [ ! -e "${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh" ]; then
  vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
else
  echo "[skein-mailbox] this box cannot establish which box it is (no SKEIN_BOX in a shared sandbox); mail is neither sent nor delivered" >&2
  exit 0
fi
vmid="${vmid//\//-}"   # slash-safe identity (matches the registry/journal shard keys)

# The one registry key that is NOT a box. In a shared sandbox `SANDBOX_VM_ID` names the SANDBOX,
# and the identity chain above says why: before SKEIN-224 the hooks fell through to it, so a
# registry somewhere holds a key by that name which no box has ever answered to. It cannot mark
# anything seen, so counted as a recipient it holds every broadcast in that store open for ever —
# measured on the owner's fleet, where 7 of sync's 11 messages are broadcasts that can never
# complete (SKEIN-259).
#
# Guarded by the same fact the identity chain turns on, and for the same reason: with no launcher
# this is a legacy box alone in its VM, where that name IS this box's own and is a real recipient.
# Empty means "exclude nothing", which is what the legacy world needs.
if [ -e "${SKEIN_FLEET_ROOT:-/boxes}/.skein/box-session.sh" ]; then
  notabox="${SANDBOX_VM_ID:-}"
else
  notabox=""
fi
notabox="${notabox//\//-}"
cmd="${1:-inbox}"; shift || true

ts() { date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?'; }

# Delete only messages whose intended recipients have all seen them. A direct message is complete
# once its target has seen it; a broadcast is complete once every currently registered box other
# than its sender has seen it. Corrupt/ambiguous messages are retained for inspection.
prune_seen() { # $1 = age in days, $2 = noisy (0/1)
  local days="${1:-30}" noisy="${2:-0}" cutoff boxes mt f
  cutoff="$(date -u -d "-$days days" +%s 2>/dev/null || echo 0)"
  [ "$cutoff" -gt 0 ] || return 0
  # Every registered box EXCEPT the sandbox's own name — see `notabox` above.
  boxes="$(jq -c --arg notabox "$notabox" '[keys[] | select($notabox == "" or . != $notabox)]' \
    "$here/sandboxes.json" 2>/dev/null || echo '[]')"
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
        # **The name is shown with the fact that nobody checked it.** A name rendered as a name is
        # how one box speaks as another — or as you.
        jq -r '"  • [\(.kind // "note")] from \(.from // "?") (a box; this name is not checked) on \(.branch // "?"): \(.body // "")"' "$f" 2>/dev/null
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
    # And the owner's, from the directory no box can write. Read after the shared ones so that a
    # flood of box mail cannot push them off a screen — they are last, which is where the eye ends.
    if [ -d "$own" ]; then
      for f in "$own"/*.json; do
        [ -e "$f" ] || break
        id="$(basename "$f")"
        # Delivered-once is remembered here rather than in the file, because the file is read-only
        # to this box by design. `grep -Fx` so an id is matched whole and not as a prefix.
        grep -Fxq "$id" "$seen_file" 2>/dev/null && continue
        jq -e --arg v "$vmid" '
          ((.to // "broadcast") as $to | $to == "broadcast" or $to == "all-projects" or $to == $v)
        ' "$f" >/dev/null 2>&1 || continue
        line="$(jq -r '"  • [\(.kind // "note")] from you: \(.body // "")"' "$f" 2>/dev/null)" || continue
        printf '%s\n' "$id" >> "$seen_file" 2>/dev/null || true
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
    fi
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
