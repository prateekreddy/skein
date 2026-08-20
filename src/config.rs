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
    ///
    /// **Off by default, and that is the correction of an old asymmetry.** There are three ways a box
    /// can get GitHub credentials — a GitHub App, a per-repo PAT, or this account-wide token — and the
    /// first two have always had to be configured on purpose. This one was simply *on*, so the
    /// broadest credential of the three was the one nobody chose. It also made itself felt: `gh`
    /// keeps its token in the system keyring on a modern Linux, so being on by default meant startup
    /// unlocked the user's keyring before they had said which path they wanted.
    ///
    /// Turning it off changes nothing for a fleet already running on it: the secret lives in sbx's
    /// own store, so it stays seeded and boxes keep pushing. It changes what a *new* fleet does —
    /// nothing, until someone picks a path — which is why the first-run checklist asks.
    #[serde(default)]
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
    /// Scope every box's GitHub credential to the repository it works on: write there, read
    /// everywhere, and a cockpit prompt for anything else. See [`crate::gitgate`].
    ///
    /// On by default, because the alternative is what this fleet had — one user token with `repo`,
    /// `admin:public_key` and `gist` reaching 460 repositories, held identically by every box. The
    /// per-box switch in box settings is the escape from this, not the other way round.
    ///
    /// Takes effect at a box's **next start**: the credential is placed as the box comes up.
    #[serde(default = "default_true")]
    pub scope_git_to_repo: bool,
    /// The GitHub App that mints per-repository write tokens, by id. Not a credential — the id is
    /// public, and the key it pairs with is a path (below) precisely so this file stays free of one.
    #[serde(default)]
    pub github_app_id: String,
    /// Path to the App's private key on the **host**. Empty ⇒ `~/.skein/github-app.pem`.
    ///
    /// A path rather than the key: `config.json` is written 0644 and round-trips through the browser
    /// on every settings save. The key is read by `openssl` and never enters skein's memory.
    #[serde(default)]
    pub github_app_key: String,
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
    /// Read pull requests in the review queue, and answer questions about them.
    ///
    /// **On by default**, unlike [`Config::ai_enrichment`], because the two spend on opposite terms.
    /// Enrichment is a background sweetener over a board that already tells you what you need, and
    /// it runs whether or not you asked — so it defaults off. A summary is only ever produced for a
    /// PR that is already in your queue, at most once per head commit, and without it the review
    /// queue does not do the job it exists for: reading thirty PRs a day yourself is the thing being
    /// replaced.
    ///
    /// `$SKEIN_REVIEW_AI=on|off` overrides. Off is not a broken state — every PR simply reads
    /// "not summarised" and stays at full attention. See [`crate::review`].
    #[serde(default = "default_true")]
    pub review_summaries: bool,
    /// The one sbx sandbox that hosts every box. **Naming it is the only supported shape.**
    ///
    /// Empty used to mean skein's original model — one microVM per box — and that is gone. It does not
    /// scale on one machine: a microVM's memory is a *reservation* whether the box is working or idle,
    /// and reservations sum. Eight boxes at ~18.6 GB each do not fit in 36 GB; eight sharing one
    /// ceiling do.
    ///
    /// Empty is now simply a fleet with no name, which nothing can start a box in — reported by
    /// `skein doctor` rather than quietly switching architectures. It is defaulted for exactly that
    /// reason, learned the hard way: every field here has a serde default, so one partial config write
    /// used to unmake the whole fleet by clearing this.
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
    /// Put dockerd's data on the same filesystem as the boxes, so [`Config::fleet_disk`] sizes
    /// everything and there is one number to raise instead of two.
    ///
    /// A sandbox otherwise carries two disks — the root the boxes live on, and a second one mounted
    /// at `/var/lib/docker` — each fixed at creation and each with its own ceiling. Two ceilings
    /// means guessing the split in advance and rebuilding the sandbox when the guess is wrong, which
    /// is the expensive way to be wrong: changing either size destroys and recreates the VM.
    ///
    /// The trade is deliberate and worth stating plainly, because it is a real one. Two disks are
    /// also two *firewalls*: a runaway `docker build` fills Docker's disk and cannot touch the
    /// boxes. Measured on this fleet — the root hit 100% while Docker's disk sat at 63% and every
    /// container kept running. Share them and one runaway takes out both. What is bought is that
    /// space is fungible: 200 GB of pool beats 100 GB each when the split was never knowable.
    ///
    /// Takes effect at dockerd's **next start**, which in practice means the next sandbox. Turning
    /// it on does not move anything: images and volumes on the old disk stay there, whole and
    /// untouched, and simply stop being visible to a dockerd now reading somewhere else.
    #[serde(default = "default_fleet_one_disk")]
    pub fleet_one_disk: bool,
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
    /// Run an agent inside the fleet sandbox and talk to it over a held-open connection. **On by
    /// default.**
    ///
    /// With it on, the small frequent calls (liveness, resources, disk) go over one connection skein
    /// keeps open instead of a fresh `sbx exec` each time. That is not about volume — `Gate` already
    /// keeps those under one a second, flat in box count — but about which of them survives a
    /// struggling sandbox: a stalled service path hangs *new* exec calls while *established* streams
    /// keep flowing, so the board goes blind while the boxes it watches are fine.
    ///
    /// It was off by default on the grounds that it is a second way into the sandbox and a fleet
    /// that had not been given one should not acquire it by upgrading. That reasoning survives only
    /// as far as the word *upgrading*: the transport is not a new exposure but a different way to
    /// reach a sandbox skein already enters at will, it is faster and it is the only one that keeps
    /// working through the daemon stall it was built for — so leaving it off meant every new fleet
    /// started on the fragile path and stayed there until someone read a doc comment.
    ///
    /// Set it `false` to opt out, and that removes the agent rather than merely ignoring it (see
    /// `heal_fleet_agent`). Every call still falls back to `sbx exec` whenever the agent does not
    /// answer, so neither setting can make skein unable to reach a box.
    #[serde(default = "default_true")]
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

/// Off by default, and it must stay that way for a fleet that already exists.
///
/// Turning this on points dockerd somewhere new at its next start, and the images and volumes on
/// the old disk stop being visible — not deleted, but gone as far as anything asking Docker is
/// concerned. Defaulting it on would do that to an existing fleet the next time its daemon
/// restarted, with nothing having asked for it. It is a choice worth making deliberately.
fn default_fleet_one_disk() -> bool {
    false
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
/// differ. `fleet_agent` defaulted to `false` then, so the transport silently stopped being installed
/// while the file said `true` and was right. That default is now `true`, which moves the symptom
/// rather than removing it: a fleet that opted out would get an agent it declined. Same lesson either
/// way — a default is indistinguishable from a choice, so the failure has to be *said*.
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

/// The settings file's path, but only once there is one.
///
/// skein does not write a `config.json` on a first run and should not: the file is a record of
/// choices, and an absent one means "every default this build has" — which is what lets a default
/// change reach an install that never had an opinion. So the places that name the path say whether
/// it is there, rather than pointing at a file that does not exist.
pub fn config_path_if_written() -> Option<String> {
    let path = config_json();
    path.exists()
        .then(|| crate::util::shorten(&path.to_string_lossy()))
}

/// The value someone actually *wrote* for `key`, or `None` when they never did.
///
/// Every field of [`Config`] has a serde default, which is what keeps old files working — and it
/// also means a loaded `Config` cannot tell a choice from a fallback. Usually that is fine: a
/// default is meant to be indistinguishable in use. It is not fine when the question is "has anyone
/// decided this yet?", which is exactly what sizing a new fleet asks: `fleet_memory` reads back
/// "26g" on a machine nobody has ever configured, because that is this build's default, and a
/// proposal that deferred to it would propose a number chosen for a different laptop.
///
/// So this reads the file, not the struct. Absent file, absent key, empty value and unreadable JSON
/// all answer `None` — in every one of them, nobody has said.
pub fn configured_field(key: &str) -> Option<String> {
    let text = fs::read_to_string(config_json()).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .get(key)?
        .clone();
    let value = match value {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
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
                     to on. The file is left alone; fix that one line and restart."
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
    with_lock(&config_lock(), || write_config(c))
}

/// Where the settings lock lives. Beside the file it guards, and hidden, because it is skein's own
/// bookkeeping rather than anything a person edits.
fn config_lock() -> std::path::PathBuf {
    skein_home().join(".config.lock")
}

/// Write the settings with the lock **already held**. Never call this without it.
fn write_config(c: &Config) -> Result<(), String> {
    let home = skein_home();
    fs::create_dir_all(&home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    let bytes = serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?;
    write_atomic(&config_json(), &home, &bytes)
}

/// Change the settings: read, apply, write — all under one lock.
///
/// **The read has to happen inside the lock**, which is the whole reason this exists rather than a
/// `lock(); save_config(load_config())` at each call site. Reading outside means acting on a value
/// that may already be stale, so the write puts back fields somebody else has since changed: two
/// cockpit tabs saving different settings, and one of them silently never happened.
///
/// `f` sees the current settings and mutates them in place. It returns whatever the caller wants
/// out of the transaction — usually `()`, sometimes the value it just set.
pub fn update_config<T>(f: impl FnOnce(&mut Config) -> Result<T, String>) -> Result<T, String> {
    with_lock(&config_lock(), || {
        // The same refusal `save_config` makes, for the same reason: `load_config` hands back
        // defaults for a file it could not parse, and writing those over the file loses every
        // setting in it.
        // `None` is a first run with no file yet, which is an empty opinion rather than an
        // unreadable one — it takes the defaults. An `Err` is the unreadable case, and is refused.
        let mut current = read_config()
            .map_err(|why| {
                format!(
                    "not saving over settings skein cannot read ({why}). Saving now would replace \
                     every setting in that file with a default. Fix or move the file, then save again."
                )
            })?
            .unwrap_or_default();
        let out = f(&mut current)?;
        if !valid_runtime(&current.default_agent) {
            return Err(format!(
                "unsupported default runtime {:?}",
                current.default_agent
            ));
        }
        write_config(&current)?;
        Ok(out)
    })
}

fn default_fleet_memory() -> String {
    "26g".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            seed_gh_secret: false,
            force_gh_secret: false,
            default_agent: default_agent(),
            base_branch: String::new(),
            confirm_destroy: true,
            ssh_key: String::new(),
            scope_git_to_repo: true,
            github_app_id: String::new(),
            github_app_key: String::new(),
            ai_enrichment: false,
            review_summaries: true,
            fleet_sandbox: default_fleet_sandbox(),
            fleet_memory: default_fleet_memory(),
            fleet_cpus: String::new(),
            fleet_disk: String::new(),
            fleet_one_disk: default_fleet_one_disk(),
            box_disk_max: default_box_disk_max(),
            git_name: String::new(),
            git_email: String::new(),
            box_memory_max: String::new(),
            box_memory_high: String::new(),
            sync_gateway_url: String::new(),
            fleet_agent: true,
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
        "fleet_agent": false,
        "fleet_agent_port": "8317",
        "ssh_key": "~/.ssh/id_ed25519"
    }"#;

    #[test]
    fn a_config_that_does_not_parse_is_reported_rather_than_silently_defaulted() {
        let _guard = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        fs::write(config_json(), ONE_BAD_FIELD).unwrap();

        // What the old code did, and why it was so hard to see: `fleet_agent` reads back its
        // default while the file says the opposite, because one unrelated field discarded the whole
        // object. The setting is written here as the non-default so the discard is visible at all —
        // a fixture agreeing with the default would pass whether or not the file was read.
        assert!(load_config().fleet_agent);
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
    /// refusing here would mean skein could never write its first config. It is also where the
    /// in-sandbox transport is decided for a new install — see
    /// `a_new_install_gets_the_faster_transport_without_being_asked` in `fleet`.
    /// Two writers, and no update is lost.
    ///
    /// **Counting, not two different fields**, and that distinction is the whole test. The obvious
    /// version — thread A sets the memory, thread B sets the cpus, assert both stuck — passes
    /// against the unlocked code, because each thread writes the same value every round and the
    /// last writer of each field usually happens to be the right one. It was written that way
    /// first, checked against the broken shape, and passed. A test that cannot fail is worse than
    /// no test: it is a claim.
    ///
    /// Incrementing one field is the sensitive form. Every lost interleave is a lost `+1` that no
    /// later round puts back, so the final count is arithmetic rather than a race: with the lock
    /// held across read and write it is exactly `2 * ROUNDS`, and without it, it is not.
    #[test]
    fn two_writers_lose_nothing_between_them() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        save_config(&Config {
            fleet_cpus: "0".into(),
            ..Config::default()
        })
        .unwrap();

        const ROUNDS: usize = 200;
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let hands: Vec<_> = (0..2)
            .map(|_| {
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..ROUNDS {
                        update_config(|c| {
                            let n: usize = c.fleet_cpus.parse().unwrap_or(0);
                            c.fleet_cpus = (n + 1).to_string();
                            Ok(())
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in hands {
            h.join().unwrap();
        }

        assert_eq!(
            load_config().fleet_cpus,
            (ROUNDS * 2).to_string(),
            "updates were lost between two writers"
        );
    }

    #[test]
    fn an_absent_config_is_not_an_error_and_saves_normally() {
        let _guard = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);

        assert!(config_error().is_none());
        // Saved as the non-default, so this proves a round trip rather than agreeing with a default
        // it never had to read.
        let want = Config {
            fleet_agent: false,
            ..Config::default()
        };
        save_config(&want).unwrap();
        assert!(!load_config().fleet_agent);
        assert!(config_error().is_none());

        env::remove_var("SKEIN_HOME");
    }
}
