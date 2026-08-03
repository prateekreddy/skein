//! skein's own settings and the paths they live at (`~/.skein`).
//!
//! Deliberately free of credentials: `config.json` is written 0644 and is the exact object the
//! settings screen GETs and POSTs, so anything secret lives elsewhere (see [`crate::tracking`]).

use crate::runtime::{default_agent, valid_runtime};
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// skein's home dir (`$SKEIN_HOME`, else `~/.skein`): holds `repos.json`, the embedded `kit/`, and
/// (for URL-added repos) `repos/<id>/{work,store}`.
pub fn skein_home() -> PathBuf {
    if let Some(h) = env::var_os("SKEIN_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(h);
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".skein")
}

pub(crate) fn repos_json() -> PathBuf {
    skein_home().join("repos.json")
}

/// skein's app settings (`~/.skein/config.json`) — the toggles the cockpit exposes. Every field has a
/// serde default so old/partial files keep working as new settings are added. Matching `$SKEIN_*` env
/// vars still override these at runtime (env wins) for headless/CI use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Seed the host `gh` token into sbx (global) at startup so boxes can fetch/push/open PRs.
    /// Off is the UI equivalent of `$SKEIN_NO_GH_SECRET`.
    #[serde(default = "default_true")]
    pub seed_gh_secret: bool,
    /// Overwrite an already-set sbx `github` secret with the current token (refresh on rotation).
    /// On is the UI equivalent of `$SKEIN_FORCE_GH_SECRET`.
    #[serde(default)]
    pub force_gh_secret: bool,
    /// Default agent for newly-added repos / boxes (the per-runtime seam). `claude` for now.
    #[serde(default = "default_agent")]
    pub default_agent: String,
    /// Base branch for `gh pr create` / merge when a repo doesn't specify one. Empty ⇒ repo default.
    /// UI equivalent of `$SKEIN_BASE`.
    #[serde(default)]
    pub base_branch: String,
    /// Confirm before a destructive **Destroy** (clone-mode boxes lose unpushed commits). The cockpit
    /// reads this to decide whether to prompt.
    #[serde(default = "default_true")]
    pub confirm_destroy: bool,
    /// Path to a private SSH key (host) to load into the host ssh-agent so sbx forwards it into boxes
    /// for SSH git push (`git@…`/`ssh://` remotes). Empty ⇒ rely on whatever's already in the agent.
    /// `$SKEIN_SSH_KEY` overrides. The key never enters a box — only the agent socket is forwarded.
    #[serde(default)]
    pub ssh_key: String,
    /// Default command a **verify** runs inside a box (`cargo test`). A repo's own `check` wins.
    /// Empty ⇒ verification is simply unavailable, which is the honest state until someone sets it.
    #[serde(default)]
    pub check_command: String,
    /// Superseded by named [`SyncConnection`]s, which pair a gateway with the token that mints at
    /// it. Read once by the migration and then cleared; kept so a pre-connections `config.json`
    /// still parses. A credential was never here and never will be — this file is written 0644 and
    /// round-trips through the browser on every settings save.
    /// Spend *rationed* Haiku calls on the Claude subscription to enrich the board: a one-line
    /// summary for a box with no journal, and a conservative safety gate on **Continue N**.
    ///
    /// Off by default because skein runs inside a box where `claude` is logged in, so these calls
    /// share the fleet's rate-limit window. `$SKEIN_AI=on|off` overrides. Lazy and cached per
    /// turn-end when on — never a per-tick sweep. See [`crate::ai`].
    #[serde(default)]
    pub ai_enrichment: bool,
    /// The one sbx sandbox that hosts every box, when several boxes share one.
    ///
    /// Empty ⇒ skein's original model: one sandbox per box, each its own microVM. That is the
    /// default and stays it, because switching is not free — a shared sandbox trades per-box
    /// memory *reservations* for a shared pool, and trades a VM boundary between boxes for a
    /// namespace one. Worth it when N reservations no longer fit; not worth it before.
    #[serde(default)]
    pub fleet_sandbox: String,
    /// Memory for the fleet sandbox (`sbx -m`), e.g. "26g".
    ///
    /// This is a ceiling shared by every box, not one reservation each — which is the whole point.
    /// sbx's own default is half the host, and the fleet wants a deliberate number instead: too low
    /// and a single `cargo build` takes the fleet down with it.
    #[serde(default = "default_fleet_memory")]
    pub fleet_memory: String,
    /// CPUs for the fleet sandbox (`sbx --cpus`). Empty ⇒ every host CPU but one.
    ///
    /// Leaving one back is what keeps the machine answering while the fleet is busy: `--cpus 0`
    /// means *all* of them, and a fleet compiling on every core makes the host's own UI stutter.
    #[serde(default)]
    pub fleet_cpus: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sync_gateway_url: String,
}

pub(crate) fn default_true() -> bool {
    true
}

/// Ensure the configured SSH key is loaded in the host ssh-agent, so sbx forwards it into boxes for
/// SSH git push. `$SKEIN_SSH_KEY` overrides the config. No key configured ⇒ no-op (the agent's
/// existing keys, if any, are forwarded as-is). The key itself never enters a box — only the agent
/// socket is forwarded (docs.docker.com/ai/sandboxes/security/credentials). Best-effort: returns Err
/// (logged by callers) but never panics. Idempotent — `ssh-add` of an already-loaded key is a no-op.
pub fn ensure_ssh_key() -> Result<(), String> {
    let key = env::var("SKEIN_SSH_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| load_config().ssh_key);
    let key = key.trim();
    if key.is_empty() {
        return Ok(());
    }
    let expanded = expand_tilde(key);
    if !Path::new(&expanded).exists() {
        return Err(format!("ssh key not found: {expanded}"));
    }
    let mut command = Command::new("ssh-add");
    command.arg(&expanded);
    let out = bounded_output(&mut command, "ssh-add", Duration::from_secs(15))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "ssh-add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub(crate) fn config_json() -> PathBuf {
    skein_home().join("config.json")
}

/// Load skein's app settings (defaults if the file is absent/malformed).
pub fn load_config() -> Config {
    fs::read_to_string(config_json())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Persist skein's app settings to `~/.skein/config.json`.
pub fn save_config(c: &Config) -> Result<(), String> {
    if !valid_runtime(&c.default_agent) {
        return Err(format!("unsupported default runtime {:?}", c.default_agent));
    }
    let home = skein_home();
    fs::create_dir_all(&home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    let bytes = serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?;
    write_atomic(&config_json(), &home, &bytes)
}

fn default_fleet_memory() -> String {
    "26g".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            seed_gh_secret: true,
            force_gh_secret: false,
            default_agent: default_agent(),
            base_branch: String::new(),
            confirm_destroy: true,
            ssh_key: String::new(),
            check_command: String::new(),
            ai_enrichment: false,
            fleet_sandbox: String::new(),
            fleet_memory: default_fleet_memory(),
            fleet_cpus: String::new(),
            sync_gateway_url: String::new(),
        }
    }
}
