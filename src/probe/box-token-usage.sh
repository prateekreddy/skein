#!/usr/bin/env bash
# skein box-token-usage.sh — append this turn's token usage, tool-call counts, and wall-clock
# duration to a durable, append-only per-box log so cost/usage stays reviewable across the box's
# whole lifetime (and beyond — never deleted on box teardown), feeding the same cross-run "which
# steps eat tokens/time" learn-loop as journals/diffs.
#
# Wired from Stop. The Stop hook's own stdin JSON carries no usage field directly (confirmed against
# Claude Code's hooks docs), only `transcript_path` — a JSONL file, one JSON object per line, where
# each `type:"assistant"` entry's `.message.usage` carries {input_tokens, output_tokens,
# cache_read_input_tokens, cache_creation_input_tokens} and `.message.content` carries tool_use
# blocks ({type:"tool_use", name:...}). A single turn can include more than one assistant entry
# (tool-call round trips), so this sums/counts every NEW assistant entry appended since the last
# Stop, tracked via a per-vmid line-count offset (same idea as the mailbox's seenBy: never re-read
# what's already been accounted for). Scoped to the MAIN transcript only — sub-agent token/tool
# activity lives in separate transcript files and isn't aggregated here (a known gap, not silent: a
# turn's logged total is a floor, not the whole cost, whenever sub-agents ran).
#
# input_tokens is the NEW/uncached input only — prompt caching serves most of a long session's
# context from cache_read_input_tokens instead, so input_tokens alone looks tiny and is NOT a bug;
# summing all four fields is what approximates "tokens this turn actually cost".
#
# Turn duration comes from box-status.sh's mark_turn_start (UserPromptSubmit) writing
# telemetry/.turn-start/<vmid>; omitted (null) if that marker is missing (e.g. this hook predates
# that fix, or the turn started before skein's probe was installed).
#
# Fail-soft, prints nothing to stdout (Stop hook stdout isn't shown anyway).
set -uo pipefail

payload="$(cat 2>/dev/null || true)"
command -v jq >/dev/null 2>&1 || exit 0

cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
# Merged layout: when the repo ships its own .claude/, the kit links only skein/ into it — the
# shared store is that link's target parent, NOT the repo dir. Writing here without this hop
# would land signals in the box-local clone where the host can never see them.
if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; fi
[ -d "$store" ] || exit 0

vmid="${SANDBOX_VM_ID:-$(hostname 2>/dev/null || echo unknown)}"
vmid="${vmid//\//-}"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '?')"

transcript="$(printf '%s' "$payload" | jq -r '.transcript_path // ""' 2>/dev/null)"
[ -n "$transcript" ] && [ -r "$transcript" ] || exit 0

dir="$store/telemetry"
off_dir="$dir/.offsets"
mkdir -p "$off_dir" 2>/dev/null || exit 0
off_file="$off_dir/$vmid"

total_lines=$(wc -l < "$transcript" 2>/dev/null || echo 0)
case "$total_lines" in '' | *[!0-9]*) total_lines=0 ;; esac
prev=$(cat "$off_file" 2>/dev/null || echo 0)
case "$prev" in '' | *[!0-9]*) prev=0 ;; esac
[ "$total_lines" -gt "$prev" ] || exit 0

# Wall-clock turn duration, from the UserPromptSubmit-side marker (box-status.sh's mark_turn_start).
duration=""
start_marker="$dir/.turn-start/$vmid"
if [ -r "$start_marker" ]; then
  start_epoch="$(cat "$start_marker" 2>/dev/null || echo 0)"
  case "$start_epoch" in '' | *[!0-9]*) start_epoch=0 ;; esac
  now_epoch="$(date -u +%s 2>/dev/null || echo 0)"
  if [ "$start_epoch" -gt 0 ] && [ "$now_epoch" -ge "$start_epoch" ]; then
    duration=$((now_epoch - start_epoch))
  fi
fi

stats="$(tail -n "+$((prev + 1))" "$transcript" 2>/dev/null | jq -s '
    [ .[] | select(.type == "assistant") ] as $entries
  | ($entries | map(.message.usage // {})) as $u
  | ($entries | map(.message.content // []) | flatten
     | map(select(.type? == "tool_use") | .name)) as $tool_names
  | { entries: ($entries | length),
      input: ($u | map(.input_tokens // 0) | add // 0),
      output: ($u | map(.output_tokens // 0) | add // 0),
      cache_read: ($u | map(.cache_read_input_tokens // 0) | add // 0),
      cache_creation: ($u | map(.cache_creation_input_tokens // 0) | add // 0),
      tools: ($tool_names | group_by(.) | map({key: .[0], value: length}) | from_entries) }
' 2>/dev/null)"
printf '%s' "$total_lines" > "$off_file" 2>/dev/null || true
[ -n "$stats" ] || exit 0

# -c (compact, one line): this is a JSONL log — a pretty-printed multi-line object here would
# break every line-oriented reader (this script's own tail -n +K resume logic included, were it
# ever pointed at this file instead of a transcript).
jq -c -n --arg t "$ts" --argjson s "$stats" --arg d "$duration" \
  '($s.input + $s.output + $s.cache_read + $s.cache_creation) as $total
   | (if $d == "" then null else ($d | tonumber) end) as $duration
   | {ts:$t, input:$s.input, output:$s.output, cache_read:$s.cache_read,
      cache_creation:$s.cache_creation, total:$total, duration_secs:$duration, tools:$s.tools}
   | select($s.entries > 0)' 2>/dev/null >> "$dir/$vmid.jsonl" || true

exit 0
