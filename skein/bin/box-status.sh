#!/usr/bin/env bash
# skein box-status.sh — record THIS box's agent turn-state into the shared store so skein's fleet
# view can show working / waiting / needs-input / blocked / error / compacting / ended.
# SHIPPED AND INSTALLED BY SKEIN (not the repo) — written into <store>/skein/bin/ and wired from
# <store>/settings.json. Modes (the hook event that drives each, in parens):
#   working      (UserPromptSubmit) you gave it work; new turn → reset the sub-agent counter,
#                                    refresh the registry's branch, and mark the turn's start time
#                                    (see refresh_branch / mark_turn_start below)
#   working-tool (Codex PostToolUse) an approved tool completed → clear a permission-blocked state
#                                    without resetting the turn timer/counter
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
#   compacting   (PreCompact)       context compaction running — busy, not stuck; also writes down
#                                    where the box was, for `compacted` to restore
#   compacted    (PostCompact)      compaction done → back to wherever the box was before it
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
# Merged layout: when the repo ships its own .claude/, the kit links only skein/ into it — the
# shared store is that link's target parent, NOT the repo dir. Writing here without this hop
# would land signals in the box-local clone where the host can never see them.
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

# The BOX, not the VM. In a shared sandbox every box has the same SANDBOX_VM_ID, so keying a
# signal on it makes every box write one file and the board see none of them report.
# SKEIN_BOX is exported by box-session.sh, the only thing that knows which box a process is
# in. A legacy box has no SKEIN_BOX and is alone in its VM, where the two are the same name.
vmid="${SKEIN_BOX:-${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}}"
vmid="${vmid//\//-}"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"

dir="$store/status"
mkdir -p "$dir" 2>/dev/null || exit 0
cfile="$dir/$vmid.agents"   # in-flight sub-agent counter (parallel Tasks → may exceed 1)

# Heartbeat before any real work: one appended line proves the hook fired, so a box whose probes
# are broken (store not linked, jq missing, script failing) is distinguishable from a box that's
# merely quiet. Rotated at ~256KB (keep the newest half) so a long-lived box can't grow it forever.
hl="$store/hook-log"
if mkdir -p "$hl" 2>/dev/null; then
  hf="$hl/$vmid.jsonl"
  printf '{"ts":"%s","script":"box-status","event":"%s","ok":true}\n' "$ts" "$mode" >>"$hf" 2>/dev/null || true
  if [ "$(wc -c <"$hf" 2>/dev/null || echo 0)" -gt 262144 ]; then
    n="$(wc -l <"$hf" 2>/dev/null || echo 0)"
    tail -n "$((n / 2))" "$hf" >"$hf.tmp" 2>/dev/null && mv "$hf.tmp" "$hf" 2>/dev/null || rm -f "$hf.tmp" 2>/dev/null
  fi
fi

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

# Pull a simple top-level string field out of the hook payload. jq is installed by the Skein kit;
# a failed install is surfaced through the boot report and cockpit health banner.
field() {
  command -v jq >/dev/null 2>&1 && printf '%s' "$payload" | jq -r --arg k "$1" '.[$k] // ""' 2>/dev/null || true
}

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
    # mktemp in the registry's OWN directory: a temp under $TMPDIR (in-guest) makes the mv a
    # cross-device copy-then-unlink — NOT atomic — and the host reads this file every 2s; a reader
    # mid-copy sees a torn registry. Same-dir rename is atomic (matches write_status below and the
    # host's write_atomic).
    tmp="$(mktemp "$store/.sbxreg.XXXXXX")" || exit 0
    # also stamp lastSeen: without it the registry's timestamp is frozen at SessionStart, so the
    # board's age column reads hours-old on an actively working box whenever the status file is
    # missing — exactly when hooks are already in trouble, compounding the confusion.
    if jq --arg v "$vmid" --arg b "$b" --arg t "$ts" \
          '.[$v] = ((.[$v] // {}) + {branch:$b, lastSeen:$t})' \
          "$reg" >"$tmp" 2>/dev/null; then mv "$tmp" "$reg"; else rm -f "$tmp"; fi
  ) 9>"$store/.sandboxes.lock" 2>/dev/null || true
}

# Record when this turn started (epoch seconds), so box-token-usage.sh can compute wall-clock turn
# duration at Stop. Lives under telemetry/ (not status/) since it's consumed by the telemetry probe,
# not the turn-state one. No stdout; fail-soft.
mark_turn_start() {
  local tdir="$store/telemetry/.turn-start"
  mkdir -p "$tdir" 2>/dev/null || return 0
  date -u +%s > "$tdir/$vmid" 2>/dev/null || true
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

# Where the box already is, as its own file records it — for the modes that must decide from the
# current state rather than blindly overwrite it (`started`, `compacting`). Empty when there is no
# file yet, which every caller must treat as "unknown", never as a state.
current_status() {
  sed -n 's/.*"status"[[:space:]]*:[[:space:]]*"\([a-z-]*\)".*/\1/p' "$dir/$vmid.json" 2>/dev/null
}
# The human detail beside it, when there is one ("API error: rate limit"). Without this a state
# restored across a compaction would come back stripped of the very thing that says why.
current_detail() {
  command -v jq >/dev/null 2>&1 || return 0
  jq -r '.detail // ""' "$dir/$vmid.json" 2>/dev/null
}

case "$mode" in
  working)
    adjust reset
    refresh_branch
    mark_turn_start
    write_status working
    ;;
  working-tool)
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
  notify-auto)
    # Some Claude releases expose notification_type in the JSON payload, while others route only
    # through hook matchers. This unconditional fallback acts only for the former shape.
    nt="$(field notification_type)"
    case "$nt" in
      permission_prompt|elicitation_dialog|agent_needs_input)
        if [ "$(inflight)" -gt 0 ]; then write_status working; else write_status blocked; fi
        ;;
      idle_prompt)
        if [ "$(inflight)" -gt 0 ]; then write_status working; else write_status waiting; fi
        ;;
      auth_success|elicitation_complete|elicitation_response|agent_completed|"") : ;;
      *) : ;; # unknown future notification types are informational until an adapter classifies them
    esac
    ;;
  error)
    adjust reset
    et="$(field error_type)"
    if [ -n "$et" ]; then write_status error "API error: ${et//_/ }"; else write_status error "API error"; fi
    ;;
  compacting)
    # Write down where the box was before compaction took over the screen, so `compacted` can put
    # it back. Kept beside the status file rather than in it, because the board must read
    # `compacting` for the duration — this is a note to the next hook, not a claim about now.
    cur="$(current_status)"
    if [ -n "$cur" ] && [ "$cur" != "compacting" ]; then
      printf '%s\n%s\n' "$cur" "$(current_detail)" >"$dir/$vmid.precompact" 2>/dev/null || true
    fi
    write_status compacting
    ;;
  compacted)
    # A compaction does not change what the box is doing — it only interrupts it. So the honest
    # answer afterwards is wherever it was, which is what `compacting` wrote down.
    #
    # This was an unconditional `working`, which is right for an AUTO compaction (it fires mid-turn
    # and the turn resumes) and wrong for a manual `/compact`, which you type at the prompt while
    # the box waits on YOU. The board then read "working" on a box that wanted an instruction, until
    # the idle Notification a full minute later — or the pane observer — happened to correct it.
    prev=""
    pdetail=""
    if [ -r "$dir/$vmid.precompact" ]; then
      { IFS= read -r prev; IFS= read -r pdetail; } <"$dir/$vmid.precompact" 2>/dev/null || true
      # Consume it: a note left behind by a compaction that died before PostCompact must not be
      # restored onto some later one.
      rm -f "$dir/$vmid.precompact" 2>/dev/null || true
    fi
    case "$prev" in
      waiting | blocked | needs-input | needs-decision | error | ended | done | working) ;;
      # No note (an older skein wired PreCompact to a script that left none, or the hook never
      # fired) or a state this version does not know: fall back to the long-standing behaviour.
      *)
        prev=working
        pdetail=""
        ;;
    esac
    write_status "$prev" "$pdetail"
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
  started)
    # A new session cannot be an ended one. The SessionEnd hook faithfully records `ended` when you
    # exit the agent, and nothing used to clear it — so a box you restarted sat there reading
    # "ended" while its new session waited at the prompt.
    #
    # Only that transition, because SessionStart fires for several sources. The `compact` source
    # fires between PreCompact and PostCompact, where the state reads `compacting` and the note
    # holding the real state is already written — claiming `waiting` here would both lie about a
    # box mid-turn and be overwritten a moment later anyway.
    cur="$(current_status)"
    if [ "$cur" = "ended" ] || [ -z "$cur" ]; then
      adjust reset
      refresh_branch
      write_status waiting
    fi
    ;;
  *)
    # explicit pass-through (e.g. a caller that hands a literal status)
    write_status "$mode"
    ;;
esac
exit 0
