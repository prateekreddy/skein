#!/usr/bin/env bash
# box-codex-hook.sh — translate provider-neutral probe stdout into Codex's hook response contract.
# Usage: box-codex-hook.sh <EventName> <probe-script-name> [probe args...]
set -uo pipefail

event="${1:-}"
probe="${2:-}"
[ -n "$event" ] && [ -n "$probe" ] || { echo "codex hook adapter: event and probe are required" >&2; exit 2; }
shift 2

case "$probe" in
  *[!A-Za-z0-9._-]*|.*|*/*) echo "codex hook adapter: invalid probe name" >&2; exit 2 ;;
esac

payload="$(cat 2>/dev/null || true)"
base="$(cd "$(dirname "$0")" 2>/dev/null && pwd)"
output="$(printf '%s' "$payload" | bash "$base/$probe" "$@")"
status=$?

# Exit 2 + stderr is meaningful for blocking hooks (for example mailbox stop-check). Do not emit a
# success envelope in that case; preserve the provider-neutral probe's outcome exactly.
[ "$status" -eq 0 ] || exit "$status"

if [ -z "$output" ]; then
  printf '{}\n'
  exit 0
fi

# These Codex events can inject model-visible context. Other events accept a top-level systemMessage;
# none of Skein's normal side-effect probes currently produce stdout there, but keep the adapter
# well-defined if a future probe does.
case "$event" in
  SessionStart|UserPromptSubmit|SubagentStart|PostToolUse)
    jq -cn --arg event "$event" --arg context "$output" \
      '{hookSpecificOutput:{hookEventName:$event,additionalContext:$context}}'
    ;;
  *)
    jq -cn --arg message "$output" '{systemMessage:$message}'
    ;;
esac
