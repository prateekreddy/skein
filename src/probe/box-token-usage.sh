#!/usr/bin/env bash
# skein box-token-usage.sh — append this turn's token usage to a durable, append-only per-box log
# so cost/usage stays reviewable across the box's whole lifetime (and beyond — never deleted on box
# teardown), feeding the same cross-run "which steps eat tokens" learn-loop as journals/diffs.
#
# Wired from Stop. The Stop hook's own stdin JSON carries no usage field directly (confirmed against
# Claude Code's hooks docs), only `transcript_path` — a JSONL file, one JSON object per line, where
# each `type:"assistant"` entry's `.message.usage` carries {input_tokens, output_tokens,
# cache_read_input_tokens, cache_creation_input_tokens}. A single turn can include more than one
# assistant entry (tool-call round trips), so this sums every NEW assistant entry appended since the
# last Stop, tracked via a per-vmid line-count offset (same idea as the mailbox's seenBy: never
# re-read what's already been accounted for). Scoped to the MAIN transcript only — sub-agent token
# spend lives in separate transcript files and isn't aggregated here (a known gap, not silent: a
# turn's logged total is a floor, not the whole cost, whenever sub-agents ran).
#
# input_tokens is the NEW/uncached input only — prompt caching serves most of a long session's
# context from cache_read_input_tokens instead, so input_tokens alone looks tiny and is NOT a bug;
# summing all four fields is what approximates "tokens this turn actually cost".
#
# Fail-soft, prints nothing to stdout (Stop hook stdout isn't shown anyway).
set -uo pipefail

payload="$(cat 2>/dev/null || true)"
command -v jq >/dev/null 2>&1 || exit 0

cwd="${CLAUDE_PROJECT_DIR:-$PWD}"
root="$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")"
store="$root/.claude"
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

usage="$(tail -n "+$((prev + 1))" "$transcript" 2>/dev/null | jq -s '
    [ .[] | select(.type == "assistant") | .message.usage ]
  | { input: (map(.input_tokens // 0) | add // 0),
      output: (map(.output_tokens // 0) | add // 0),
      cache_read: (map(.cache_read_input_tokens // 0) | add // 0),
      cache_creation: (map(.cache_creation_input_tokens // 0) | add // 0) }
' 2>/dev/null)"
printf '%s' "$total_lines" > "$off_file" 2>/dev/null || true
[ -n "$usage" ] || exit 0

# -c (compact, one line): this is a JSONL log — a pretty-printed multi-line object here would
# break every line-oriented reader (this script's own tail -n +K resume logic included, were it
# ever pointed at this file instead of a transcript).
jq -c -n --arg t "$ts" --argjson u "$usage" \
  '($u.input + $u.output + $u.cache_read + $u.cache_creation) as $total
   | {ts:$t, input:$u.input, output:$u.output, cache_read:$u.cache_read,
      cache_creation:$u.cache_creation, total:$total}
   | select(.total > 0)' 2>/dev/null >> "$dir/$vmid.jsonl" || true

exit 0
