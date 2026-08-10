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
    /// The fleet is the default. Empty ⇒ skein's original model: one sandbox per box, each its own
    /// microVM, still fully supported for anyone who wants a VM boundary between boxes.
    ///
    /// It became the default because the alternative does not scale on one machine: a microVM's
    /// memory is a *reservation* whether the box is working or idle, and reservations sum. Eight
    /// boxes at ~18.6 GB each do not fit in 36 GB; eight boxes sharing one ceiling do.
    ///
    /// Defaulted rather than left empty for a second reason, learned the hard way: an empty value
    /// means "legacy" and every field here has a serde default, so one partial config write silently
    /// unmade the whole fleet. A default that names the usual sandbox degrades to a working fleet
    /// instead of to a different architecture.
    #[serde(default = "default_fleet_sandbox")]
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
    /// Root filesystem size for the fleet sandbox, e.g. "60g". Empty ⇒ sbx's default of 20 GB.
    ///
    /// Not a flag: sbx reads this from `DOCKER_SANDBOXES_ROOT_SIZE` in the environment of the
    /// process that starts its daemon, so skein passes it to `sbx create` rather than putting it in
    /// the argv. **It is fixed when the sandbox is created** — changing it later means recreating
    /// the sandbox, which discards every box's VM-local checkout, so snapshot first.
    ///
    /// 20 GB is one shared disk for the whole fleet, and it is the ceiling that binds first: eight
    /// boxes had used 7.8 GB of it with two `target/` directories doing most of that. A per-box VM
    /// never had to share.
    #[serde(default)]
    pub fleet_disk: String,
    /// The most disk **one** box may use, e.g. "10g". Empty ⇒ unlimited.
    ///
    /// The fleet's disk is one filesystem shared by every box, so unlike memory there is no kernel
    /// ceiling standing between them: a box that fills it fills it for everyone, and the first
    /// symptom is another box's build failing with ENOSPC. A per-box override lives beside the box's
    /// other durable state, so raising one box's allowance is not a decision about all of them.
    #[serde(default = "default_box_disk_max")]
    pub box_disk_max: String,
    /// The `user.name` every box commits as. Empty ⇒ read from the repo's host clone at start.
    ///
    /// A box's checkout is a fresh clone into a private HOME, so it inherits neither the host's
    /// global gitconfig nor anything a previous box set — and the first commit fails with `Author
    /// identity unknown`, at the moment the work is finished rather than when the box was built.
    #[serde(default)]
    pub git_name: String,
    /// The `user.email` every box commits as. Empty ⇒ read from the repo's host clone at start.
    #[serde(default)]
    pub git_email: String,
    /// The hard memory cap for ONE box (cgroup `memory.max`). Empty ⇒ derived from
    /// [`Config::fleet_memory`].
    ///
    /// This is the protection the shared model needs, and the only one that is not optional: without
    /// it a single runaway box exhausts the VM and the kernel starts killing whichever process it
    /// likes — which is every *other* box's agent as readily as the guilty one. Capped below the
    /// fleet total, a runaway box can only kill itself.
    #[serde(default)]
    pub box_memory_max: String,
    /// The soft memory limit for one box (cgroup `memory.high`). Empty ⇒ derived.
    ///
    /// Deliberately below `memory.max`: past this the kernel throttles the box and reclaims rather
    /// than killing it, so a build that briefly wants more gets slower instead of dying. The gap
    /// between the two is the difference between a slow box and a lost turn.
    #[serde(default)]
    pub box_memory_high: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sync_gateway_url: String,
    /// Run an agent inside the fleet sandbox and talk to it over a held-open connection. Off by
    /// default.
    ///
    /// With it on, the small frequent calls (liveness, resources, disk) go over one connection skein
    /// keeps open instead of a fresh `sbx exec` each time. That is not about volume — `Gate` already
    /// keeps those under one a second, flat in box count — but about which of them survives a
    /// struggling sandbox: a stalled service path hangs *new* exec calls while *established* streams
    /// keep flowing, so the board goes blind while the boxes it watches are fine.
    ///
    /// Off by default because it is a second way into the sandbox, and a fleet that has not been
    /// given one should not acquire it by upgrading. Every call falls back to `sbx exec` when the
    /// agent does not answer, so turning it on cannot make skein less able to reach a box.
    #[serde(default)]
    pub fleet_agent: bool,
    /// Pin the agent's **host** port instead of letting skein choose and re-choose one. 0 ⇒ choose.
    ///
    /// Normally skein publishes a port, checks that the agent actually answers on it, and moves to
    /// another when it does not — which it must, because an sbx port mapping outlives the sandbox it
    /// was made for and keeps being reported as published while every connection through it is
    /// refused (docker/sbx-releases#297), a state every resize produces.
    ///
    /// Pin it when something else needs to know the number in advance. A pinned port is tried and
    /// never silently replaced: healing onto a different one would make the pin a suggestion.
    #[serde(default)]
    pub fleet_agent_port: u16,
}

/// The sandbox a fleet lives in unless told otherwise. One name, because a host with two fleets has
/// deliberately configured the second one.
fn default_fleet_sandbox() -> String {
    "skein-fleet".to_string()
}

fn default_box_disk_max() -> String {
    "10g".to_string()
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

/// Read `config.json`, keeping "absent" and "present but unreadable" apart.
///
/// The distinction is the whole point. `Ok(None)` is a first run and not a problem. `Err` is a file
/// that exists and could not be turned into a [`Config`] — and that case used to be indistinguishable
/// from the first, because the error was thrown away and defaults returned in its place. One field
/// serde could not deserialise discarded *every* setting in the file, and since most defaults match
/// what a working install already had, the only visible symptom was whichever setting happened to
/// differ. `fleet_agent` defaults to `false`, so the transport silently stopped being installed while
/// the file said `true` and was right.
fn read_config() -> Result<Option<Config>, String> {
    let path = config_json();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// What is wrong with `config.json`, or `None` when it parses (or is simply not there yet).
///
/// Exposed so the board and `skein doctor` can say it out loud. A config skein cannot read is
/// invisible from the outside: every setting reads back as its default, and a default is
/// indistinguishable from a choice.
pub fn config_error() -> Option<String> {
    read_config().err()
}

/// Load skein's app settings (defaults if the file is absent or unreadable).
pub fn load_config() -> Config {
    match read_config() {
        Ok(Some(c)) => c,
        Ok(None) => Config::default(),
        Err(why) => {
            // Once per process. This is called on nearly every request, and a line per call would
            // bury the one line that matters under thousands of copies of itself.
            static TOLD: std::sync::Once = std::sync::Once::new();
            TOLD.call_once(|| {
                eprintln!(
                    "skein: cannot read your settings ({why}) — every setting is falling back to \
                     its default until that file parses, including `fleet_agent`, which defaults \
                     to off. The file is left alone; fix that one line and restart."
                );
            });
            Config::default()
        }
    }
}

/// Persist skein's app settings to `~/.skein/config.json`.
pub fn save_config(c: &Config) -> Result<(), String> {
    if !valid_runtime(&c.default_agent) {
        return Err(format!("unsupported default runtime {:?}", c.default_agent));
    }
    // Refuse rather than overwrite, because this is the moment the settings are lost for good:
    // `load_config` has just handed the caller defaults for a file it could not parse, so writing
    // that struct back replaces every setting in the file with a default nobody chose — and the
    // file is the only copy. One toggle in the cockpit would have been enough.
    if let Err(why) = read_config() {
        return Err(format!(
            "not saving over settings skein cannot read ({why}). Saving now would replace every \
             setting in that file with a default. Fix or move the file, then save again."
        ));
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
            ai_enrichment: false,
            fleet_sandbox: default_fleet_sandbox(),
            fleet_memory: default_fleet_memory(),
            fleet_cpus: String::new(),
            fleet_disk: String::new(),
            box_disk_max: default_box_disk_max(),
            git_name: String::new(),
            git_email: String::new(),
            box_memory_max: String::new(),
            box_memory_high: String::new(),
            sync_gateway_url: String::new(),
            fleet_agent: false,
            fleet_agent_port: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, tempdir};

    /// The exact shape that caused this: valid JSON, the setting the user wanted plainly visible,
    /// and one *other* field serde cannot deserialise.
    const ONE_BAD_FIELD: &str = r#"{
        "fleet_agent": true,
        "fleet_agent_port": "8317",
        "ssh_key": "~/.ssh/id_ed25519"
    }"#;

    #[test]
    fn a_config_that_does_not_parse_is_reported_rather_than_silently_defaulted() {
        let _guard = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        fs::write(config_json(), ONE_BAD_FIELD).unwrap();

        // What the old code did, and why it was so hard to see: `fleet_agent` reads back false
        // while the file says true, because one unrelated field discarded the whole object.
        assert!(!load_config().fleet_agent);
        let why = config_error().expect("an unparseable config must be reportable");
        // serde_json names the type and the position rather than the field, so the locator is what
        // makes this fixable — "somewhere in your config" would leave the user no better off than
        // the silence it replaced.
        assert!(
            why.contains("line 3") && why.contains("expected u16"),
            "the complaint must locate the failure, not just name the file: {why}"
        );

        env::remove_var("SKEIN_HOME");
    }

    /// The data-loss guard. Before this, `load_config` handed the settings screen a struct full of
    /// defaults and the next POST wrote them back — one toggle in the cockpit replaced every
    /// setting in the file with a default nobody chose, and the file was the only copy.
    #[test]
    fn saving_never_overwrites_settings_skein_could_not_read() {
        let _guard = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        fs::write(config_json(), ONE_BAD_FIELD).unwrap();

        let err = save_config(&Config::default()).expect_err("must refuse, not overwrite");
        assert!(
            err.contains("line 3"),
            "refusing is only useful if it says what to fix: {err}"
        );
        assert_eq!(
            fs::read_to_string(config_json()).unwrap(),
            ONE_BAD_FIELD,
            "the user's settings must still be on disk, byte for byte"
        );

        env::remove_var("SKEIN_HOME");
    }

    /// A first run must not be mistaken for a broken file: there is nothing to protect yet, and
    /// refusing here would mean skein could never write its first config.
    #[test]
    fn an_absent_config_is_not_an_error_and_saves_normally() {
        let _guard = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);

        assert!(config_error().is_none());
        let want = Config {
            fleet_agent: true,
            ..Config::default()
        };
        save_config(&want).unwrap();
        assert!(load_config().fleet_agent);
        assert!(config_error().is_none());

        env::remove_var("SKEIN_HOME");
    }
}
