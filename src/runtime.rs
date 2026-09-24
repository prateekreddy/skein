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
        //
        // `--plugin-dir {plugin}` on every `claude` here, both halves of each `||`: skein's own
        // plugin, loaded from the read-only build for this session only (box-plugin §2.1, SKEIN-1056).
        // Passed by the launcher's argv rather than installed into settings, so a box cannot unload
        // it and no box can remove it from another. `{plugin}` is resolved by [`for_box`].
        interactive_start: "claude --name '{box}' --plugin-dir {plugin}",
        // `|| claude` is not belt-and-braces: a box can legitimately have nothing to continue — a
        // cross-runtime replacement box whose new agent was never spoken to, a box whose transcript
        // was cleared, a session killed before its first turn. There `claude --continue` exits with
        // "No conversation found", the tmux session dies with it, and every reconnect replayed that
        // same failure. Fall back to a fresh conversation (the takeover brief is on disk, so the new
        // agent still picks up the context). Mirrors Codex's `resume --last || codex` below.
        interactive_resume: "claude --name '{box}' --plugin-dir {plugin} --continue || claude --name '{box}' --plugin-dir {plugin}",
        headless_resume: "claude --plugin-dir {plugin} --continue --print {prompt} || claude --plugin-dir {plugin} --print {prompt}",
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
///
/// `{plugin}` is resolved here too, to [`plugin_dir`] shell-quoted, for the same reason: it is a
/// path under the fleet root, which the host knows and a fresh crossing into the box need not.
///
/// **And `{plugin}` names the narrower of the plugin's two variants when the fleet's switch is
/// off** ([`box_plugin_on`]): the one with the turn-state hooks and nothing else.
pub(crate) fn for_box(command: &str, name: &str) -> String {
    // The Settings switch's read site (SKEIN-1058). The plugin carries skein's turn-state hooks as
    // well as the resource holds and the `skein_*` tools (SKEIN-1062), and **the owner's rule** is
    // that off drops only the holds and the tools: turn state keeps loading. So off does not remove
    // the flag; it points it at [`turn_state_plugin_dir`], which carries the turn-state hooks alone.
    // Exactly one `--plugin-dir` either way, and both variants are the one plugin named `skein`.
    let plugin = match box_plugin_on() {
        true => plugin_dir(),
        false => turn_state_plugin_dir(),
    };
    command
        .replace("{box}", name)
        .replace("{plugin}", &sh_quote(&plugin))
}

/// Whether the fleet loads skein's plugin into agents' sessions: `$SKEIN_BOX_PLUGIN` when it is set
/// to a yes or a no, else the Settings switch ([`crate::config::Config::box_plugin`]). The same
/// precedence `ai::ai_enabled` gives `$SKEIN_AI`, which is what the Settings footer's
/// "`$SKEIN_*` env vars override these" promises. Read in skein-server, on the host.
pub(crate) fn box_plugin_on() -> bool {
    match std::env::var("SKEIN_BOX_PLUGIN").ok().as_deref() {
        Some("on" | "1" | "true" | "yes") => true,
        Some("off" | "0" | "false" | "no") => false,
        _ => crate::config::load_config().box_plugin,
    }
}

/// A runtime's headless command for one box, with `prompt` (already shell-quoted) in place.
pub(crate) fn headless(runtime: &RuntimeAdapter, name: &str, prompt: &str) -> String {
    for_box(runtime.headless_resume, name).replace("{prompt}", prompt)
}

/// Where skein's own box plugin is installed: under the fleet root's `.skein`, which every box
/// has bound read-only (box-plugin §2.1). Installed with the launcher on every start and heal, so
/// it is always the plugin of the skein build that is running.
pub(crate) fn plugin_dir() -> String {
    format!("{}/.skein/plugin", fleet_root())
}

/// The plugin's narrower variant, for a fleet whose switch is off: skein's manifest and its
/// turn-state hooks, with no resource holds, monitor or tools (SKEIN-1062). Beside [`plugin_dir`]
/// under the same read-only `.skein`, rather than inside it, so that loading one variant can never
/// load the other with it.
pub(crate) fn turn_state_plugin_dir() -> String {
    format!("{}/.skein/plugin-turn-state", fleet_root())
}

/// Every file of both plugin variants as `fleet::install_launcher` writes them: (absolute path,
/// bytes). The full plugin is [`PLUGIN_FILES`] with the turn-state hooks added to its
/// `hooks/hooks.json`; the narrow one is the same manifest with the turn-state hooks alone. Both
/// sets of hooks come from [`crate::probes::turn_state_hooks`], so the two cannot drift apart.
pub(crate) fn plugin_install() -> Vec<(String, String)> {
    let turn_state = crate::probes::turn_state_hooks();
    let pretty = |v: &serde_json::Value| {
        serde_json::to_string_pretty(v).expect("a hooks value serialises") + "\n"
    };
    let full = plugin_dir();
    let mut out: Vec<(String, String)> = PLUGIN_FILES
        .iter()
        .map(|(rel, body)| {
            let body = match *rel {
                "hooks/hooks.json" => pretty(&with_turn_state(body, &turn_state)),
                _ => body.to_string(),
            };
            (format!("{full}/{rel}"), body)
        })
        .collect();
    let narrow = turn_state_plugin_dir();
    for (rel, body) in PLUGIN_FILES {
        if *rel == ".claude-plugin/plugin.json" {
            out.push((format!("{narrow}/{rel}"), body.to_string()));
        }
    }
    out.push((format!("{narrow}/hooks/hooks.json"), pretty(&turn_state)));
    out
}

/// A plugin `hooks.json` with every turn-state entry appended after its own, event by event.
fn with_turn_state(own: &str, turn_state: &serde_json::Value) -> serde_json::Value {
    let mut merged: serde_json::Value =
        serde_json::from_str(own).expect("the plugin's own hooks.json parses");
    let into = merged["hooks"]
        .as_object_mut()
        .expect("the plugin's hooks.json has a hooks object");
    for (event, groups) in turn_state["hooks"].as_object().into_iter().flatten() {
        into.entry(event.clone())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .expect("a hook event is an array")
            .extend(groups.as_array().into_iter().flatten().cloned());
    }
    merged
}

/// The plugin's files, relative to [`plugin_dir`], as the build carries them. `src/plugin/` is the
/// source; `fleet::install_launcher` writes them beside the launcher through [`plugin_install`],
/// which adds skein's turn-state hooks to `hooks/hooks.json`.
pub(crate) const PLUGIN_FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("plugin/.claude-plugin/plugin.json"),
    ),
    ("hooks/hooks.json", include_str!("plugin/hooks/hooks.json")),
    (
        "monitors/monitors.json",
        include_str!("plugin/monitors/monitors.json"),
    ),
    (
        "bin/skein-resources",
        include_str!("plugin/bin/skein-resources"),
    ),
    // The in-box tools (box-plugin §2.3, SKEIN-1059): an MCP server over stdio, registered by the
    // plugin's own `.mcp.json`, so it loads and unloads with the plugin.
    (".mcp.json", include_str!("plugin/.mcp.json")),
    ("bin/skein-mcp", include_str!("plugin/bin/skein-mcp")),
];

pub(crate) fn agent_session_name(name: &str, runtime: &str) -> String {
    if runtime == agent_for_box(name) {
        "skein-agent".to_string()
    } else {
        format!("skein-agent-{runtime}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every `claude` the adapter can start carries skein's plugin**, both halves of every `||`,
    /// and the placeholder resolves to the read-only build's copy under the fleet root.
    ///
    /// What would make it fail: dropping `--plugin-dir {plugin}` from any of the three strings, or
    /// from the fallback half of one (a box whose `--continue` finds nothing would then start
    /// without the plugin); or `for_box` leaving `{plugin}` unresolved.
    #[test]
    fn every_claude_the_adapter_starts_loads_skeins_plugin() {
        let claude = runtime_adapter("claude").unwrap();
        for (what, command) in [
            ("interactive_start", claude.interactive_start),
            ("interactive_resume", claude.interactive_resume),
            ("headless_resume", claude.headless_resume),
        ] {
            let starts: Vec<&str> = command.split("||").map(str::trim).collect();
            for start in starts {
                assert!(
                    start.starts_with("claude ") && start.contains(" --plugin-dir {plugin}"),
                    "{what} starts a claude without skein's plugin: {start:?}"
                );
            }
        }

        let _lock = crate::testutil::env_lock();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_FLEET_ROOT", "/fleet-root-example");
        env.set("SKEIN_HOME", "/skein-home-example");
        // The switch at its default: no `config.json` there, and no override.
        env.unset("SKEIN_BOX_PLUGIN");
        let resolved = for_box(claude.interactive_resume, "web-main");
        assert!(!resolved.contains("{plugin}"), "{resolved}");
        assert_eq!(
            resolved
                .matches(
                    "claude --name 'web-main' --plugin-dir '/fleet-root-example/.skein/plugin'"
                )
                .count(),
            2,
            "{resolved}"
        );
    }

    /// **The fleet's switch decides which variant of the plugin a session's argv names**, for all
    /// three starts, and `$SKEIN_BOX_PLUGIN` overrides the stored value either way (SKEIN-1058).
    /// On names the full plugin; off names the turn-state one (SKEIN-1062). Every `claude` carries
    /// exactly one `--plugin-dir` either way.
    ///
    /// Written through the real `config.json` in a pinned `$SKEIN_HOME`, the file the Settings
    /// route writes. What would make it fail: `for_box` not reading the value (every "off" case
    /// still names the full plugin); the value's default flipped to off (the file with no key would
    /// lose it); the env override not consulted, or consulted the wrong way round; off dropping
    /// the flag, as it did before the turn-state hooks moved into the plugin; or half a flag left
    /// behind (`--plugin-dir` with no path, which `claude` would reject).
    #[test]
    fn the_fleets_switch_decides_which_plugin_the_argv_names() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_FLEET_ROOT", "/fleet-root-example");
        env.set("SKEIN_HOME", &home);
        env.unset("SKEIN_BOX_PLUGIN");
        let claude = runtime_adapter("claude").unwrap();
        let starts = [
            claude.interactive_start,
            claude.interactive_resume,
            claude.headless_resume,
        ];
        let config = |body: &str| {
            std::fs::write(home.join("config.json"), body).unwrap();
        };
        // (full, turn-state-only) flags in one resolved command.
        let carries = |command: &str| {
            let resolved = for_box(command, "web-main");
            let flags = resolved.matches("--plugin-dir").count();
            let full = resolved
                .matches("--plugin-dir '/fleet-root-example/.skein/plugin'")
                .count();
            let narrow = resolved
                .matches("--plugin-dir '/fleet-root-example/.skein/plugin-turn-state'")
                .count();
            assert_eq!(flags, full + narrow, "half a flag left behind: {resolved}");
            assert!(!resolved.contains("{plugin}"), "{resolved}");
            (full, narrow)
        };
        for (body, env_value, on) in [
            ("{}", None, true),
            (r#"{"box_plugin": true}"#, None, true),
            (r#"{"box_plugin": false}"#, None, false),
            (r#"{"box_plugin": true}"#, Some("off"), false),
            (r#"{"box_plugin": false}"#, Some("on"), true),
        ] {
            config(body);
            match env_value {
                Some(v) => env.set("SKEIN_BOX_PLUGIN", v),
                None => env.unset("SKEIN_BOX_PLUGIN"),
            };
            for command in starts {
                let halves = command.split("||").count();
                assert_eq!(
                    carries(command),
                    if on { (halves, 0) } else { (0, halves) },
                    "config {body}, $SKEIN_BOX_PLUGIN {env_value:?}: {}",
                    for_box(command, "web-main")
                );
            }
        }
        // Off changes the directory and nothing else.
        config(r#"{"box_plugin": false}"#);
        env.unset("SKEIN_BOX_PLUGIN");
        assert_eq!(
            for_box(claude.interactive_resume, "web-main"),
            "claude --name 'web-main' --plugin-dir '/fleet-root-example/.skein/plugin-turn-state' \
             --continue || claude --name 'web-main' --plugin-dir \
             '/fleet-root-example/.skein/plugin-turn-state'"
        );
    }

    /// **skein's turn-state hooks load whichever way the fleet's switch is set**, and off drops
    /// only the resource holds, the monitor and the `skein_*` tools (the owner's decision on
    /// SKEIN-1057, carried out by SKEIN-1062).
    ///
    /// Follows the argv to the bytes: for each switch value, the directory `for_box` names, then
    /// the files [`plugin_install`] puts there. What would make it fail: off dropping the flag
    /// (no directory is named, so no turn-state hook loads); the turn-state variant not installed,
    /// or installed without its hooks; either variant missing any of the 24 entries; the full one
    /// losing its resource hooks; or the narrow one carrying the holds, the monitor or the tools.
    #[test]
    fn turn_state_hooks_load_whichever_way_the_switch_is_set() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_FLEET_ROOT", "/fleet-root-example");
        env.set("SKEIN_HOME", &home);
        let claude = runtime_adapter("claude").unwrap();
        let installed = plugin_install();
        let file = |dir: &str, rel: &str| {
            let path = format!("{dir}/{rel}");
            installed
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, body)| body.clone())
        };
        let turn_state = crate::probes::turn_state_hooks();
        let groups = |hooks: &serde_json::Value, event: &str| -> Vec<serde_json::Value> {
            hooks["hooks"][event]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        for switch in ["on", "off"] {
            env.set("SKEIN_BOX_PLUGIN", switch);
            let resolved = for_box(claude.interactive_start, "web-main");
            let dir = resolved
                .split_once("--plugin-dir '")
                .and_then(|(_, rest)| rest.split_once('\''))
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_else(|| panic!("switch {switch}: the argv names no plugin: {resolved}"));

            let manifest = file(&dir, ".claude-plugin/plugin.json")
                .unwrap_or_else(|| panic!("switch {switch}: no plugin is installed at {dir}"));
            let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
            assert_eq!(manifest["name"], "skein", "switch {switch}");
            let hooks: serde_json::Value = serde_json::from_str(
                &file(&dir, "hooks/hooks.json")
                    .unwrap_or_else(|| panic!("switch {switch}: {dir} has no hooks")),
            )
            .unwrap();
            let mut count = 0;
            for (event, wanted) in turn_state["hooks"].as_object().unwrap() {
                let have = groups(&hooks, event);
                for group in wanted.as_array().unwrap() {
                    assert!(
                        have.contains(group),
                        "switch {switch}: {dir} does not load {event} {group}"
                    );
                    count += 1;
                }
            }
            assert_eq!(count, 24, "switch {switch}");

            let resources = hooks.to_string().contains("bin/skein-resources");
            let tools = file(&dir, ".mcp.json").is_some();
            let monitor = file(&dir, "monitors/monitors.json").is_some();
            match switch {
                "on" => assert!(resources && tools && monitor, "on lost the holds or tools"),
                _ => assert!(
                    !resources && !tools && !monitor,
                    "off still loads a hold ({resources}), the tools ({tools}) or the monitor \
                     ({monitor})"
                ),
            }
        }
    }

    /// **The plugin the build carries is the one its argv names**: a manifest, the three resource
    /// hooks the design gives it and no others, a monitor, and the in-box tools' MCP server. These
    /// are its own files in `src/plugin/`; the turn-state hooks join them at install
    /// ([`plugin_install`], tested by `turn_state_hooks_load_whichever_way_the_switch_is_set`).
    ///
    /// What would make it fail: a plugin file missing from `PLUGIN_FILES` (the install would put a
    /// plugin with no hooks under `.skein`), a resource hook event added or dropped, or the
    /// `PreToolUse` matcher losing `Bash`. It also keeps the resource hooks away from `Stop`, where
    /// the turn-state set's `mailbox.sh stop-check` runs. And it fails if `.mcp.json` names a
    /// server other than the shipped `bin/skein-mcp`.
    #[test]
    fn the_plugin_carries_its_three_hooks_and_its_monitor() {
        let file = |rel: &str| {
            PLUGIN_FILES
                .iter()
                .find(|(r, _)| *r == rel)
                .unwrap_or_else(|| panic!("the plugin ships no {rel}"))
                .1
        };
        let manifest: serde_json::Value =
            serde_json::from_str(file(".claude-plugin/plugin.json")).unwrap();
        assert_eq!(manifest["name"], "skein");

        let hooks: serde_json::Value = serde_json::from_str(file("hooks/hooks.json")).unwrap();
        let events: Vec<&str> = hooks["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            events,
            vec!["PreToolUse", "SessionStart", "UserPromptSubmit"],
            "the plugin's hook events moved"
        );
        let matcher = hooks["hooks"]["PreToolUse"][0]["matcher"].as_str().unwrap();
        assert!(
            matcher.split('|').any(|m| m == "Bash"),
            "the hold no longer reaches Bash: {matcher}"
        );
        for event in &events {
            let command = hooks["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert!(
                command.contains("${CLAUDE_PLUGIN_ROOT}/bin/skein-resources"),
                "{event} runs something the plugin does not ship: {command}"
            );
        }

        let monitors: serde_json::Value =
            serde_json::from_str(file("monitors/monitors.json")).unwrap();
        assert_eq!(monitors[0]["name"], "resources");
        assert!(
            monitors[0]["command"]
                .as_str()
                .unwrap()
                .ends_with("/bin/skein-resources\" monitor"),
            "{}",
            monitors[0]
        );
        assert!(file("bin/skein-resources").starts_with("#!/usr/bin/env python3"));

        // The tools: registered by the plugin, and run from the plugin's own copy.
        let mcp: serde_json::Value = serde_json::from_str(file(".mcp.json")).unwrap();
        let servers = mcp["mcpServers"].as_object().expect("no mcpServers");
        assert_eq!(servers.len(), 1, "{mcp}");
        let args = servers["skein"]["args"].as_array().unwrap();
        assert_eq!(
            args[0], "${CLAUDE_PLUGIN_ROOT}/bin/skein-mcp",
            "the tools run something the plugin does not ship: {mcp}"
        );
        assert!(file("bin/skein-mcp").starts_with("#!/usr/bin/env python3"));
    }
}
