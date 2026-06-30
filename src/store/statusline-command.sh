#!/usr/bin/env bash
# Claude Code status line: context window + rate limits, with usage bars that
# also show the projected end-of-window level. Refresh via statusLine.refreshIntervalMs.

input=$(cat)

# ── helpers ───────────────────────────────────────────────────────────────────

repeat_char() {
  local char="$1" n="$2" out="" i=0
  while [ "$i" -lt "$n" ]; do out="${out}${char}"; i=$(( i + 1 )); done
  printf '%s' "$out"
}

# make_bar <pct 0-100> <proj_pct 0-100> <width> : solid to pct, hatched to proj, empty after
make_bar() {
  local pct="${1:-0}" proj="${2:-0}" width="${3:-12}"
  pct=$(( pct > 100 ? 100 : pct ))
  proj=$(( proj > 100 ? 100 : proj ))
  local filled=$(( pct * width / 100 ))
  local pfilled=$(( proj * width / 100 ))
  [ "$pfilled" -lt "$filled" ] && pfilled=$filled
  repeat_char "█" "$filled"
  repeat_char "▒" "$(( pfilled - filled ))"
  repeat_char "░" "$(( width - pfilled ))"
}

RED='\033[0;31m'; YEL='\033[0;33m'; GRN='\033[0;32m'; DIM='\033[2m'; BLD='\033[1m'; RST='\033[0m'

color_for_pct() {  # green < 60, yellow 60-80, red >= 80
  local p="$1"
  if   [ "$p" -ge 80 ]; then printf '%s' "$RED"
  elif [ "$p" -ge 60 ]; then printf '%s' "$YEL"
  else                       printf '%s' "$GRN"
  fi
}

fmt_tokens() {  # 1234567 → 1.2M, 12345 → 12.3k
  local n="$1"
  if   [ "$n" -ge 1000000 ]; then awk "BEGIN{printf \"%.1fM\", $n/1000000}"
  elif [ "$n" -ge 1000 ];    then awk "BEGIN{printf \"%.1fk\", $n/1000}"
  else                            printf '%s' "$n"
  fi
}

fmt_left() {  # seconds → "3d15h" / "1h4m" / "12m" / "resetting"
  local s="$1"
  [ "$s" -le 0 ] && { printf 'resetting'; return; }
  local d=$(( s / 86400 )) h=$(( (s % 86400) / 3600 )) m=$(( (s % 3600) / 60 ))
  if   [ "$d" -gt 0 ]; then printf '%dd%dh left' "$d" "$h"
  elif [ "$h" -gt 0 ]; then printf '%dh%dm left' "$h" "$m"
  else                      printf '%dm left' "$m"
  fi
}

now=$(date +%s)

# limit_block <LABEL> <pct> <resets_at> <window_secs> : a "5H ▓▒░ 79%→100% 3d15h left" segment
limit_block() {
  local label="$1" pct="$2" reset="$3" window="$4"
  { [ -z "$pct" ] || [ -z "$reset" ]; } && return
  local int; int=$(printf '%.0f' "$pct")
  local left=$(( reset - now ))
  local elapsed=$(( window - left )); [ "$elapsed" -le 0 ] && elapsed=1

  # Project to end of window only once >=15% has elapsed; before that the rate
  # is too noisy (a tiny burst would extrapolate to "100%"), so show current.
  # Projected level can exceed 100% (e.g. 79%→165%) — that overshoot is the signal.
  # The number/colour use the true value; the bar itself clamps to full (make_bar).
  local proj=$int
  if [ "$int" -gt 0 ] && [ "$elapsed" -ge $(( window * 15 / 100 )) ]; then
    proj=$(awk "BEGIN{printf \"%.0f\", ($int/$elapsed)*$window}")
  fi

  # Colour by the worse of current vs projected — so "will blow the cap" reads red
  # even while current usage still looks calm.
  local worst=$(( proj > int ? proj : int ))
  local col; col=$(color_for_pct "$worst")
  local bar; bar=$(make_bar "$int" "$proj" 12)
  printf '%s' "${DIM}${label}${RST}  ${col}${bar}${RST} ${BLD}${int}%${RST}→${proj}% ${DIM}$(fmt_left "$left")${RST}"
}

# ── parse JSON ────────────────────────────────────────────────────────────────

ctx_used=$(  echo "$input" | jq -r '.context_window.used_percentage     // empty')
ctx_total=$( echo "$input" | jq -r '.context_window.context_window_size // 0')
ctx_tokens=$(echo "$input" | jq -r '.context_window.total_input_tokens  // 0')

five_pct=$(  echo "$input" | jq -r '.rate_limits.five_hour.used_percentage // empty')
five_reset=$(echo "$input" | jq -r '.rate_limits.five_hour.resets_at       // empty')
week_pct=$(  echo "$input" | jq -r '.rate_limits.seven_day.used_percentage // empty')
week_reset=$(echo "$input" | jq -r '.rate_limits.seven_day.resets_at       // empty')

# Persist the limit gauge so a running agent can poll it (statusline gets this on
# stdin only; the CLI does not write it to disk). Pure side-effect; display below
# is unchanged. Safe to delete this block.
printf '{"written_at":%s,"five_pct":%s,"five_reset":%s,"week_pct":%s,"week_reset":%s,"ctx_pct":%s}\n' \
  "$now" "${five_pct:-null}" "${five_reset:-null}" "${week_pct:-null}" "${week_reset:-null}" \
  "$(echo "$input" | jq -r '.context_window.used_percentage // null')" \
  > "${HOME:-/tmp}/.claude/rate-limits.json" 2>/dev/null

model=$(     echo "$input" | jq -r '.model.display_name // .model.id // "Claude"')
model="${model// context)/)}"   # "Opus 4.8 (1M context)" → "Opus 4.8 (1M)"
model="${model// (/(}"          # → "Opus 4.8(1M)"
cost=$(      echo "$input" | jq -r '.cost.total_cost_usd // empty')

# ── context block ─────────────────────────────────────────────────────────────

ctx_line=""
if [ -n "$ctx_used" ]; then
  ctx_pct=$(printf '%.0f' "$ctx_used")
  ctx_col=$(color_for_pct "$ctx_pct")
  ctx_bar=$(make_bar "$ctx_pct" "$ctx_pct" 12)
  ctx_line="${DIM}CTX${RST} ${ctx_col}${ctx_bar}${RST} ${BLD}${ctx_pct}%${RST} ${DIM}$(fmt_tokens "$ctx_tokens")/$(fmt_tokens "$ctx_total")${RST}"
fi

five_line=$(limit_block "5H" "$five_pct" "$five_reset" 18000)
week_line=$(limit_block "7D" "$week_pct" "$week_reset" 604800)
cost_line=""
[ -n "$cost" ] && cost_line="${DIM}\$$(printf '%.2f' "$cost")${RST}"
model_line="${DIM}${model}${RST}"

# ── assemble ──────────────────────────────────────────────────────────────────

parts=()
[ -n "$ctx_line"  ] && parts+=("$ctx_line")
[ -n "$five_line" ] && parts+=("$five_line")
[ -n "$week_line" ] && parts+=("$week_line")
[ -n "$cost_line" ] && parts+=("$cost_line")
parts+=("$model_line")

out=""; sep="${DIM} │ ${RST}"
for part in "${parts[@]}"; do
  if [ -z "$out" ]; then out="$part"; else out="${out}${sep}${part}"; fi
done

printf '%b\n' "$out"
