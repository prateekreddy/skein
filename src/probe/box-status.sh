#!/usr/bin/env bash
# skein box-status.sh — record THIS box's agent turn-state into the shared store so skein's fleet
# view can show working / waiting / needs-input / blocked / error / compacting / ended.
# SHIPPED AND INSTALLED BY SKEIN (not the repo) — written into <store>/skein/bin/ and wired from
# <store>/settings.json. Modes (the hook event that drives each, in parens):
#   working      (UserPromptSubmit) you gave it work; new turn → reset the sub-agent counter, and
#                                    refresh the registry's branch (see refresh_branch below)
#   agent-start  (PreToolUse Task)  it delegated to a sub-agent; still WORKING; counter++
#   agent-stop   (SubagentStop)     a sub-agent finished; counter--; still working
#   waiting      (Stop)             the turn ended, your move; also refreshes the registry's branch
#   notify-blocked  (Notification, matcher permission_prompt|elicitation_dialog|agent_needs_input)
#                                    needs your call → blocked
#   notify-waiting  (Notification, matcher idle_prompt)
#                                    the turn went idle → waiting, your move
#   notify-ignore   (Notification, matcher auth_success|elicitation_complete|elicitation_response|agent_completed)
#                                    informational only → keep the current state
#   (all three: if sub-agents are in flight it's just waiting on THEM → working, regardless of type)
#
#   IMPORTANT: the Notification hook's stdin JSON carries no field that names which of the above
#   fired (confirmed against Claude Code's hooks docs — there is no `notification_type` on the
#   payload). Claude Code disambiguates *before* invoking the hook, via each hook entry's own
#   `matcher` in settings.json — so the modes above are selected by which matcher routed here, wired
#   as separate Notification entries in `settings_with_probe` (lib.rs), never by reading the payload.
#   error        (StopFailure)      the turn died on an API error; .error_type → the detail (rate_limit…)
#   compacting   (PreCompact)       context compaction running — busy, not stuck
#   compacted    (PostCompact)      compaction done → back to working
#   ended        (SessionEnd)       the session terminated; .reason → the detail (logout, exit, …)
# Why the counter: while the box waits on its own foreground sub-agents Claude Code fires a
# Notification — without the counter that flips the row to "needs me" even though it's actively working.
# It's reset at every turn boundary (working/waiting/error/ended) so it self-heals each turn.
# Writes <store>/status/<vmid>.json = {"status":..,"detail":..?,"ts":..}. Idempotent, fail-soft, and
# prints NOTHING to stdout (a UserPromptSubmit hook's stdout would be injected into the prompt).
set -uo pipefail

mode="${1:-}"
[ -n "$mode" ] || exit 0
payload="$(cat 2>/dev/null || true)"   # the hook's JSON on stdin (used by notify/error/ended)

cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
[ -d "$store" ] || exit 0

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"

dir="$store/status"
mkdir -p "$dir" 2>/dev/null || exit 0
cfile="$dir/$vmid.agents"   # in-flight sub-agent counter (parallel Tasks → may exceed 1)

# Adjust the counter under a lock so parallel Task/SubagentStop hooks don't clobber it.
# $1 = "reset" | a signed integer delta. Clamps at 0. No stdout.
adjust() {
  (
    flock -w 5 9 || exit 0
    if [ "$1" = "reset" ]; then
      n=0
    else
      n=$(cat "$cfile" 2>/dev/null || echo 0)
      case "$n" in '' | *[!0-9-]*) n=0 ;; esac
      n=$((n + ($1)))
    fi
    [ "$n" -lt 0 ] && n=0
    printf '%s' "$n" >"$cfile" 2>/dev/null
  ) 9>"$cfile.lock" 2>/dev/null || true
}

inflight() {
  local n
  n=$(cat "$cfile" 2>/dev/null || echo 0)
  case "$n" in '' | *[!0-9]*) echo 0 ;; *) echo "$n" ;; esac
}

# Pull a string field out of the hook payload (empty if jq/field absent).
field() { command -v jq >/dev/null 2>&1 && printf '%s' "$payload" | jq -r --arg k "$1" '.[$k] // ""' 2>/dev/null || true; }

# Refresh this box's branch in the shared registry (sandboxes.json) on every turn boundary. Without
# this, the board's branch column is whatever sandbox-bootstrap.sh captured once at SessionStart (or
# the launch spec, captured once at creation) — stale the instant a box does its own `git checkout`
# mid-session (e.g. branch-per-slice work). Shares sandbox-bootstrap.sh's exact lock file and merge
# style so a concurrent registration write can't race this update. No stdout; fail-soft.
refresh_branch() {
  command -v jq >/dev/null 2>&1 || return 0
  local b reg
  b="$(git -C "$root" rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
  [ -n "$b" ] && [ "$b" != "HEAD" ] || return 0
  reg="$store/sandboxes.json"
  (
    flock -w 5 9 || exit 0
    [ -s "$reg" ] || echo '{}' >"$reg"
    tmp="$(mktemp "${TMPDIR:-/tmp}/sbxreg.XXXXXX")" || exit 0
    if jq --arg v "$vmid" --arg b "$b" \
          '.[$v] = ((.[$v] // {}) + {branch:$b})' \
          "$reg" >"$tmp" 2>/dev/null; then mv "$tmp" "$reg"; else rm -f "$tmp"; fi
  ) 9>"$store/.sandboxes.lock" 2>/dev/null || true
}

write_status() { # $1 = status key, $2 = optional human detail
  local tmp
  tmp="$(mktemp "$dir/.st.XXXXXX" 2>/dev/null)" || return 0
  if [ -n "${2:-}" ] && command -v jq >/dev/null 2>&1; then
    jq -n --arg s "$1" --arg d "$2" --arg t "$ts" '{status:$s,detail:$d,ts:$t}' >"$tmp" 2>/dev/null
  elif [ -n "${2:-}" ]; then
    printf '{"status":"%s","detail":"%s","ts":"%s"}\n' "$1" "$2" "$ts" >"$tmp" 2>/dev/null
  else
    printf '{"status":"%s","ts":"%s"}\n' "$1" "$ts" >"$tmp" 2>/dev/null
  fi
  mv "$tmp" "$dir/$vmid.json" 2>/dev/null || rm -f "$tmp" 2>/dev/null
}

case "$mode" in
  working)
    adjust reset
    refresh_branch
    write_status working
    ;;
  agent-start)
    adjust 1
    write_status working
    ;;
  agent-stop)
    adjust -1
    write_status working
    ;;
  notify-blocked)
    if [ "$(inflight)" -gt 0 ]; then
      write_status working # waiting on its own sub-agents, not on you
    else
      write_status blocked
    fi
    ;;
  notify-waiting)
    if [ "$(inflight)" -gt 0 ]; then
      write_status working
    else
      write_status waiting
    fi
    ;;
  notify-ignore)
    : # informational only; leave the current status alone
    ;;
  error)
    adjust reset
    et="$(field error_type)"
    if [ -n "$et" ]; then write_status error "API error: ${et//_/ }"; else write_status error "API error"; fi
    ;;
  compacting)
    write_status compacting
    ;;
  compacted)
    write_status working
    ;;
  ended)
    adjust reset
    r="$(field reason)"
    if [ -n "$r" ]; then write_status ended "session ended: ${r//_/ }"; else write_status ended "session ended"; fi
    ;;
  waiting)
    adjust reset
    refresh_branch
    write_status waiting
    ;;
  *)
    # explicit pass-through (e.g. a caller that hands a literal status)
    write_status "$mode"
    ;;
esac
exit 0
