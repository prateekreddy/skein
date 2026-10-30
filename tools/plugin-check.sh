#!/usr/bin/env bash
#
# skein's plugin, checked by the Claude Code build that loads it (docs/decisions/skein-is-a-mod.md).
#
#   tools/plugin-check.sh
#
# Four checks, each by the CLI itself rather than by anything that imitates it:
#
#   1. `claude plugin validate --strict src/plugin`: the manifest, hooks.json and the hooks module's
#      source, read the way the engine reads them, warnings counted as errors.
#   2. `claude plugin test src/plugin`: every `tests/*.test.ts`, against the engine.
#   3. A headless `claude -p /skein` with the plugin, offline and on a throwaway home: the module
#      loads under `-p` and answers, with `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1` as skein's own
#      headless start sets it (`src/runtime.rs`). With `CLAUDE_CODE_PLUGIN_DIR_WATCH=1` that run
#      also lays this build's API declarations beside the plugin, in `.claude-plugin/types/`.
#   4. `tsc` over the hooks module, its tests and its `$.state` contract, against those
#      declarations: the types of the build that runs, not of the one the module was written on.
#
# Steps 3 and 4 work on a copy, so nothing is written into the checkout. `$CLAUDE_CODE_VERSION`,
# when set, must be the version `claude --version` reports: CI sets it from the one pin in
# `.github/workflows/ci.yml`, so the check cannot pass on a CLI other than the one it names.
#
# Not a gate in `tools/gates.sh`, for the reason gitleaks is not: a contributor's machine need not
# have Claude Code or TypeScript, and a gate that cannot run everywhere fails for a reason that is
# not the change. Every skein box has both `claude` and `npx`, so a lane runs it there.
#
# Exit 0 when all four pass, 1 when one fails, 2 when a tool it needs is missing.

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
plugin="$root/src/plugin"
tsc=${TSC:-tsc}

for tool in claude "$tsc"; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "plugin-check: $tool is not on PATH" >&2
    exit 2
  }
done

have=$(claude --version | awk '{print $1}')
if [ -n "${CLAUDE_CODE_VERSION:-}" ] && [ "$have" != "$CLAUDE_CODE_VERSION" ]; then
  echo "plugin-check: claude is $have, and this check was asked for $CLAUDE_CODE_VERSION" >&2
  exit 1
fi
echo "plugin-check: Claude Code $have"

work=$(mktemp -d "${TMPDIR:-/tmp}/skein-plugin-check.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/home" "$work/plugin"
cp -R "$plugin/." "$work/plugin/"

# A home of its own, and a key that is visibly not one: nothing here signs in or reads the
# caller's ~/.claude, and `/skein` is answered by the module without a model call.
export HOME="$work/home" CLAUDE_CODE_TMPDIR="$work/home" DISABLE_AUTOUPDATER=1
export ANTHROPIC_API_KEY=skein-test-not-a-key
# Off, so the build reads no feature flags from the network: whether mods load for installed
# plugins is a flag (`tengu_plugin_hooks_modules`), and a check that answered to whatever the flag
# service said today would not be a check of this tree. Unfetched, the build's default stands.
export DISABLE_TELEMETRY=1

echo "plugin-check: validate"
claude plugin validate --strict "$plugin"

echo "plugin-check: test"
claude plugin test "$plugin"

echo "plugin-check: claude -p /skein"
said=$(cd "$work/home" && SKEIN_BOX=plugin-check CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 \
  CLAUDE_CODE_PLUGIN_DIR_WATCH=1 claude -p /skein --plugin-dir "$work/plugin" </dev/null)
echo "$said"
case "$said" in
  *"skein: version "*", in box plugin-check"*) ;;
  *)
    echo "plugin-check: the hooks module did not answer /skein under claude -p" >&2
    exit 1
    ;;
esac

declared="$work/plugin/.claude-plugin/types/claude-code/index.d.ts"
if ! head -n 1 "$declared" 2>/dev/null | grep -qF "Written by Claude Code $have."; then
  echo "plugin-check: Claude Code $have laid no API declarations at $declared" >&2
  exit 1
fi

echo "plugin-check: tsc against Claude Code $have's declarations"
# tsc exits 2 on a type error, which here would read as a missing tool: a failure is a 1.
"$tsc" -p "$work/plugin" || exit 1

echo "plugin-check: ok"
