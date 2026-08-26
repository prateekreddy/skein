//! The per-runtime seam: one adapter table describing how each agent CLI is started, resumed and
//! named, plus the argv construction that follows from it.
//!
//! Every provider difference lives in `RUNTIME_ADAPTERS`. Adding a runtime is a row there, not a
//! branch in a caller — which is why the cockpit and CLI discover the choices from here instead of
//! hardcoding their own lists.

use crate::repos::agent_for_box;
use crate::util::*;
use serde::Serialize;

pub(crate) fn default_agent() -> String {
    "claude".into()
}

/// Public runtime metadata consumed by the CLI and cockpit. Runtime choices are deliberately
/// discovered from the core instead of duplicated in every client; adding another adapter therefore
/// makes it appear everywhere without another round of provider-specific UI conditionals.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub executable: &'static str,
    pub supports_resume: bool,
    pub supports_handoff: bool,
    pub adapted_statusline: bool,
}

/// Everything Skein needs to start or resume a native agent process. Lifecycle hook translation is
/// kept beside this registry below, while launch, attach, takeover, validation, health, and UI all
/// consume these definitions. Provider quirks belong here, not at their call sites.
pub(crate) struct RuntimeAdapter {
    pub(crate) info: RuntimeInfo,
    /// Idempotent provider setup run on attach and before first launch. It may provide defaults but
    /// must preserve explicit user configuration. Provider quirks remain centralized here.
    pub(crate) interactive_setup: &'static str,
    /// Best-effort, bounded native updater run immediately before creating a new agent process.
    /// Reattaching to a live tmux session skips it so an in-progress agent is never replaced.
    /// Emits the Claude-compatible status-line JSON model on stdout. `None` means the provider
    /// supplies its own command-driven status line and needs no browser footer adapter.
    pub(crate) statusline_input: Option<&'static str>,
    /// Runtime-native durable instruction file, relative to HOME. Skein adds one managed block.
    pub(crate) instruction_file: &'static str,
    /// Optional higher-precedence instruction file used only when the user already created it.
    pub(crate) instruction_override: &'static str,
    /// Shell command used to create this runtime's first persistent tmux process.
    pub(crate) interactive_start: &'static str,
    /// Shell command used when creating a provider-specific persistent tmux session.
    pub(crate) interactive_resume: &'static str,
    /// Headless command run inside an existing box; `{prompt}` is replaced with a shell-quoted value.
    pub(crate) headless_resume: &'static str,
    /// Best-effort, bounded provider-native transcript export. It emits Markdown to stdout and is
    /// used only for cross-runtime replacement; native transcript files never leave the source box.
    pub(crate) context_export: &'static str,
}

pub(crate) static RUNTIME_ADAPTERS: &[RuntimeAdapter] = &[
    RuntimeAdapter {
        info: RuntimeInfo {
            id: "claude",
            label: "Claude",
            executable: "claude",
            supports_resume: true,
            supports_handoff: true,
            adapted_statusline: false,
        },
        interactive_setup: ":",
        statusline_input: None,
        instruction_file: ".claude/CLAUDE.md",
        instruction_override: "",
        // Named after the box, because the name is the address. With the session registry shared
        // (see `share_paths` in box-session.sh) every box's agent is reachable via `SendMessage`,
        // and an unnamed session shows up in another box's `ListAgents` as a pid — which is not
        // something anyone can address, and not something the fleet has any other name for.
        interactive_start: "claude --name '{box}'",
        // `|| claude` is not belt-and-braces: a box can legitimately have nothing to continue — a
        // cross-runtime replacement box whose new agent was never spoken to, a box whose transcript
        // was cleared, a session killed before its first turn. There `claude --continue` exits with
        // "No conversation found", the tmux session dies with it, and every reconnect replayed that
        // same failure. Fall back to a fresh conversation (the takeover brief is on disk, so the new
        // agent still picks up the context). Mirrors Codex's `resume --last || codex` below.
        interactive_resume: "claude --name '{box}' --continue || claude --name '{box}'",
        headless_resume: "claude --continue --print {prompt} || claude --print {prompt}",
        context_export: r####"project="$HOME/.claude/projects/$(printf '%s' "$root" | sed 's#/#-#g')"; latest="$(find "$project" -type f -name '*.jsonl' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -n 1 | cut -d' ' -f2-)"; [ -n "$latest" ] && [ -r "$latest" ] && jq -r 'def text: if type == "string" then . elif type == "array" then map(if type == "string" then . elif .type == "text" then (.text // empty) else empty end) | join("\n") else "" end; select(.type == "user" or .type == "assistant") | (.message.role // .type) as $role | ((.message.content // empty) | text) as $body | select($body != "") | "### \($role)\n\n\($body)\n"' "$latest" 2>/dev/null | tail -c 200000 || true"####,
    },
    RuntimeAdapter {
        info: RuntimeInfo {
            id: "codex",
            label: "Codex",
            executable: "codex",
            supports_resume: true,
            supports_handoff: true,
            adapted_statusline: true,
        },
        // Skein's exact footer needs bars and projections that Codex's native item list cannot
        // express. Disable only the default Skein previously seeded; an explicit `/statusline`
        // choice remains authoritative and suppresses the adapted footer below.
        interactive_setup: r#"cfg="$HOME/.codex/config.toml"; mkdir -p "$HOME/.codex"; touch "$cfg"; old='status_line = ["context-used", "five-hour-limit", "weekly-limit", "used-tokens", "git-branch", "model-with-reasoning"]'; broken='status_line = null # skein custom statusline'; marker='status_line = [] # skein custom statusline'; if grep -Fqx "$broken" "$cfg"; then sed -i 's/^status_line = null # skein custom statusline$/status_line = [] # skein custom statusline/' "$cfg"; elif grep -Fqx "$old" "$cfg"; then sed -i '/^status_line = \[/c\status_line = [] # skein custom statusline' "$cfg"; elif ! grep -Eq '^[[:space:]]*(tui\.)?status_line[[:space:]]*=' "$cfg"; then if grep -Eq '^[[:space:]]*\[tui\][[:space:]]*$' "$cfg"; then sed -i "/^[[:space:]]*\[tui\][[:space:]]*$/a $marker" "$cfg"; else printf '\n[tui]\n%s\n' "$marker" >> "$cfg"; fi; fi; root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"; store="$root/.claude"; if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; elif [ -L "$store" ]; then store="$(readlink -f "$store")"; fi; installer="$store/skein/bin/install-codex-hooks.sh"; [ ! -r "$installer" ] || bash "$installer" "$store""#,
        // Codex records the same live data used by `/status` in token_count events. Select limits
        // by window duration (5h/7d), not provider-specific limit names, and emit Claude's schema so
        // both providers share the renderer below. The marker makes `/statusline` an opt-out.
        statusline_input: Some(r####"grep -Fq 'status_line = [] # skein custom statusline' "$HOME/.codex/config.toml" || exit 0; latest="$(find "$HOME/.codex/sessions" -type f -name '*.jsonl' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -n 1 | cut -d' ' -f2-)"; [ -n "$latest" ] && [ -r "$latest" ] || exit 0; jq -s '([.[] | select(.type == "event_msg" and .payload.type == "token_count") | .payload]) as $tokens | ($tokens | last) as $t | (([$tokens[] | select((.rate_limits.limit_name // "") == "")] | last) // $t) as $quota | ([.[] | select(.type == "turn_context") | .payload] | last) as $turn | def window($minutes): ([$quota.rate_limits.primary, $quota.rate_limits.secondary, $quota.rate_limits.individual_limit] | map(select(. != null and .window_minutes == $minutes)) | first); ($t.info.last_token_usage.total_tokens // 0) as $used | ($t.info.model_context_window // 0) as $total | {context_window: (if $total > 0 then {used_percentage: (($used * 100) / $total), total_input_tokens: $used, context_window_size: $total} else null end), rate_limits: {five_hour: ((window(300)) as $w | if $w then {used_percentage: $w.used_percent, resets_at: $w.resets_at} else null end), seven_day: ((window(10080)) as $w | if $w then {used_percentage: $w.used_percent, resets_at: $w.resets_at} else null end)}, model: {display_name: ([($turn.model // empty), ($turn.effort // empty)] | map(select(length > 0)) | join(" "))}}' "$latest""####),
        instruction_file: ".codex/AGENTS.md",
        instruction_override: ".codex/AGENTS.override.md",
        // Skein installs a generated user-level hook set. Trusting this known set on launch avoids
        // an otherwise invisible first-run prompt while retaining Codex's workspace sandbox.
        // Codex documents --no-alt-screen specifically for retaining terminal scrollback. Under
        // tmux + xterm.js, alternate-screen wheel events otherwise become Up/Down and cycle prompt
        // history instead of scrolling the conversation.
        interactive_start: "codex --no-alt-screen --dangerously-bypass-hook-trust",
        interactive_resume: "codex --no-alt-screen --dangerously-bypass-hook-trust resume --last || codex --no-alt-screen --dangerously-bypass-hook-trust",
        headless_resume: "codex exec resume --last --dangerously-bypass-hook-trust {prompt} || codex exec --dangerously-bypass-hook-trust {prompt}",
        context_export: r####"latest="$(find "$HOME/.codex/sessions" -type f -name '*.jsonl' -printf '%T@ %p\n' 2>/dev/null | sort -nr | head -n 1 | cut -d' ' -f2-)"; [ -n "$latest" ] && [ -r "$latest" ] && jq -r 'select(.type == "response_item" and .payload.type == "message") | .payload as $m | (($m.content // []) | map(.text // .input_text // .output_text // empty) | join("\n")) as $body | select($body != "") | "### \($m.role // "agent")\n\n\($body)\n"' "$latest" 2>/dev/null | tail -c 200000 || true"####,
    },
];

/// Make tmux a persistence layer rather than visible UI. These are server-global because the box has
/// one Skein-owned tmux server; applying after detached session creation works on both first launch
/// and reconnect, and remains compatible with older boxes whose server already exists.
pub(crate) const TMUX_CONFIGURE: &str = "tmux set-option -g status off; tmux set-option -g mouse on; tmux set-option -g history-limit 100000; tmux set-option -g focus-events on; tmux set-option -g set-clipboard on; ";

pub(crate) const TMUX_AGENT_CONTRACT: &str = "inline-scrollback-v1";

pub(crate) fn runtime_adapter(id: &str) -> Option<&'static RuntimeAdapter> {
    RUNTIME_ADAPTERS
        .iter()
        .find(|runtime| runtime.info.id == id)
}

pub fn supported_runtimes() -> Vec<RuntimeInfo> {
    RUNTIME_ADAPTERS
        .iter()
        .map(|runtime| runtime.info.clone())
        .collect()
}

pub fn valid_runtime(id: &str) -> bool {
    runtime_adapter(id).is_some()
}

pub(crate) fn resolve_runtime(id: &str) -> &'static RuntimeAdapter {
    runtime_adapter(id).unwrap_or(&RUNTIME_ADAPTERS[0])
}

/// Shell that starts the level observer (box-pane.sh) beside the agent's tmux session.
///
/// Detached with `setsid` so it outlives this attach — a browser tab closing must not stop the box
/// reporting what its screen says — and `nice -n 19` so it can never compete with the agent or the
/// human's editor for CPU. Idempotent: the script takes a box-local lock and a second copy exits
/// immediately, so every reconnect can run this blindly. Fail-soft throughout: a box whose store
/// predates the script simply has no observer, and turn-state falls back to hook edges alone.
/// `sock` is the box's tmux socket under the shared model, empty when the sandbox is the box. It is
/// exported rather than passed as an argument so that a store whose `box-pane.sh` predates this
/// still starts and simply ignores it — the observer is fail-soft by design, and a box that loses
/// its observer loses turn-state detail, not its session.
pub(crate) fn pane_observer_start(tmux_name: &str, sock: &str) -> String {
    let socket_env = if sock.is_empty() {
        String::new()
    } else {
        format!("SKEIN_TMUX_SOCK={} ", sh_quote(sock))
    };
    format!(
        "obs=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)/.claude/skein/bin/box-pane.sh\"; \
         if [ -r \"$obs\" ]; then command -v setsid >/dev/null 2>&1 || setsid() {{ \"$@\"; }}; \
         ( {socket_env}setsid nice -n 19 bash \"$obs\" {tmux_name} >/dev/null 2>&1 & ) ; fi;"
    )
}

/// Wrap the command that becomes a tmux session's agent process so a *failure to start* leaves the
/// window alive as a shell with the provider's error still on screen. Without it the window exits
/// instantly and the attach right behind it dies on tmux's own "can't find session", the real cause
/// already scrolled away — the shape the cross-runtime replacement path kept hitting.
/// Contains no `$`: this string is embedded double-quoted in the outer shell, which would expand a
/// variable itself instead of leaving it for tmux's shell.
pub(crate) fn guarded_agent_command(agent: &str, command: &str) -> String {
    format!(
        "{command} || {{ echo; echo 'skein: {agent} could not start — see the error above; keeping this session as a shell'; exec bash -li; }}"
    )
}

/// Refresh the concise Skein-managed block in a runtime's native durable instruction file before
/// creating its agent process. Reattaching to a live tmux session skips this entire branch.
pub(crate) fn agent_instruction_setup(runtime: &RuntimeAdapter) -> String {
    let instruction = sh_quote(runtime.instruction_file);
    let override_ = sh_quote(runtime.instruction_override);
    format!(
        r#"root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"; store="$root/.claude"; if [ -L "$store/skein" ]; then store="$(dirname "$(readlink "$store/skein")")"; elif [ -L "$store" ]; then store="$(readlink -f "$store")"; fi; helper="$store/skein/bin/agent-guide.sh"; if [ -r "$helper" ]; then bash "$helper" "$store" {instruction} {override_} || echo 'skein: durable agent guidance could not be refreshed' >&2; else echo 'skein: agent guide helper is unavailable; restart the host server to refresh this store' >&2; fi"#
    )
}

/// Resolve `{box}` in a runtime's start command, on the host, where the box's name is known.
///
/// Deliberately not `$SKEIN_BOX` left for the box to expand. The command is embedded double-quoted
/// in the attach shell, and that shell is a fresh entry into the box's namespace — a different
/// process lineage from the session `box-session.sh` exported that variable into, so it would be
/// unset there. The obvious fallback, `$(hostname)`, is the trap: in a shared sandbox every box
/// reports `skein-fleet`, so all of them would answer to one name and `SendMessage` would have
/// nothing to address. A box is not a sandbox, in its newest costume.
pub(crate) fn for_box(command: &str, name: &str) -> String {
    command.replace("{box}", name)
}

pub(crate) fn agent_session_name(name: &str, runtime: &str) -> String {
    if runtime == agent_for_box(name) {
        "skein-agent".to_string()
    } else {
        format!("skein-agent-{runtime}")
    }
}
