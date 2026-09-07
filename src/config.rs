//! skein's own settings and the paths they live at (`~/.skein`).
//!
//! Deliberately free of credentials: `config.json` is written 0644 and is the exact object the
//! settings screen GETs and POSTs, so anything secret lives elsewhere (see [`crate::tracking`]).

use crate::runtime::{default_agent, valid_runtime};
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

/// The marker that says this process is a test run, and the reason it is an environment variable.
///
/// `cfg!(test)` is **false inside this library when it is linked into a `tests/*.rs` integration
/// binary** — the library is compiled once, without `--cfg test`, and every integration binary
/// links that build. So a `cfg!(test)` guard is absent from exactly the suites that drive the most
/// fleet machinery. (`fleet::fleet_disk_usage`'s `if cfg!(test)` already has that asymmetry, and
/// its cache is therefore live under every `tests/*.rs`.)
///
/// `.cargo/config.toml` sets it in the `[env]` table, so a plain `cargo test` in this tree carries
/// it with nothing to remember — which is the point, since the failure this guards was a `cargo
/// test` run by somebody who had not been told to export anything. `tests/harness.rs` asserts it
/// arrives in an integration binary, where `cfg!(test)` cannot.
pub const TEST_MARKER: &str = "SKEIN_TEST";

/// Is this a test process? [`TEST_MARKER`], or `cfg!(test)` for the crate's own unit tests, which
/// have it whether or not cargo was invoked from this tree.
pub fn in_test() -> bool {
    cfg!(test) || env::var_os(TEST_MARKER).is_some_and(|v| !v.is_empty())
}

/// skein's home dir (`$SKEIN_HOME`, else `~/.skein`): holds `repos.json`, the embedded `kit/`, and
/// (for URL-added repos) `repos/<id>/{work,store}`.
///
/// **A test that has not pinned `$SKEIN_HOME` panics here rather than being answered.** The
/// fallback below is right in production and catastrophic in a test: on a developer box
/// [`volume_marker`] resolves to the fleet's real home, so a fixture box name becomes a directory
/// under the owner's live `~/.skein/boxes`, beside the state skein has *decided* about every real
/// box. That is not hypothetical — `resume_batch_holds_real_decisions_when_ai_on` wrote
/// `~/.skein/boxes/box-route/resume.log` on this box, twice, and a fixture name that collided with
/// a real box would have written into that box's decisions instead (SKEIN-626). The sibling test
/// directly above it pins the variable and says why; the next one down did not carry it, which is
/// why the answer is a guard here rather than one more pinned test.
pub fn skein_home() -> PathBuf {
    if let Some(h) = env::var_os("SKEIN_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(h);
    }
    assert!(
        !in_test(),
        "$SKEIN_HOME is unset in a test process (${TEST_MARKER}). Refusing to fall back to the \
         fleet volume marker or $HOME/.skein: on a developer box that is the real ~/.skein, and a \
         test that writes there writes into live box state (SKEIN-626). Set $SKEIN_HOME to this \
         test's own temp directory — and $SKEIN_FLEET_ROOT with it if what you are exercising \
         resolves a fleet path."
    );
    if let Some(h) = volume_marker() {
        return h;
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".skein")
}

/// The volume `bootstrap.sh` found, recorded beside the binaries it installed.
///
/// **Inside the sandbox, `$HOME/.skein` is the wrong answer and there is no second guess.** sbx
/// mounts the volume at its HOST absolute path while giving the sandbox a home of its own, so the
/// fallback below resolves to a container-local directory that is empty, is not the volume, and
/// does not survive the sandbox. The server never sees this because the supervisor passes
/// `$SKEIN_HOME` explicitly — but nothing passes it to the `skein` CLI, and every invocation of it
/// therefore read an empty home and reported an empty fleet. `skein repos` said "no repos yet"
/// about a fleet whose cockpit was showing them.
///
/// A file rather than re-deriving it: `bootstrap.sh` already does the `mountinfo` discovery, and
/// answering the same question two ways in two languages is how the two answers drift. It writes
/// what it found; this reads it.
///
/// Absent on a host, where the fleet root does not exist — so the fallback stays exactly what it
/// always was for a host-driven skein, which is the only deployment that has ever been right about
/// `$HOME`.
fn volume_marker() -> Option<PathBuf> {
    let root = env::var("SKEIN_FLEET_ROOT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/boxes".to_string());
    let text = fs::read_to_string(PathBuf::from(root).join(".skein/skein-home")).ok()?;
    let path = text.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub(crate) fn repos_json() -> PathBuf {
    skein_home().join("repos.json")
}

/// skein's app settings (`~/.skein/config.json`) — the toggles the cockpit exposes. Every field has a
/// serde default so old/partial files keep working as new settings are added. Matching `$SKEIN_*` env
/// vars still override these at runtime (env wins) for headless/CI use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Does this fleet intend its boxes to push as the account, rather than with a scoped
    /// credential?
    ///
    /// **The seeding it used to switch on is gone** — that was `sbx secret set -g` on the host, and
    /// architecture §13a deletes the machine-global store with it. What the field still decides is
    /// what `gitgate::box_credential` claims: on, a box holds the account token *if* one was seeded
    /// before that deletion (`repos::gh_secret_seeded` is the evidence, and it travels with the
    /// volume); off, it holds nothing. The label is what the first-run checklist reads as "boxes can
    /// push", so claiming it without the marker is how a fleet learns otherwise from a 403.
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
    /// Which model reads and reviews pull requests — summaries, questions, drafted comments, and
    /// the actual review. Empty means each call's own default (summaries stay on the cheap model,
    /// the writing calls on the stronger one). `$SKEIN_REVIEW_MODEL` overrides, and
    /// `$SKEIN_AI_MODEL` — the everything-override — still trumps both, as it always has.
    #[serde(default)]
    pub review_model: String,
    /// How many pull requests the review queue may ANALYSE per UTC day, across every repo — one
    /// fleet-wide budget, counted in [`crate::review`] at the moment a model call is actually
    /// made. One unit is one `(number, head_sha)` analysed, whether the visit produced a
    /// one-liner, a brief, or a brief plus a drafted review; a summary served from the disk cache
    /// costs nothing and counts nothing. The day rolls over at midnight UTC.
    ///
    /// **Default 100** — the ceiling as it was asked for, verbatim: "the allowance can be very
    /// high. Say for example not more than 100 PRs a day (cache misses, actual analysis)". Not a
    /// per-repo number and not a throttle: it exists to put a roof over the runaway case (the old
    /// client-side allowance had none — every button press refilled it, measured at up to 180
    /// calls/day) while never starving an ordinary day's queue. Zero means zero: no unasked or
    /// asked reads at all today, which is a supported state, not a broken one.
    #[serde(default = "default_review_reads_per_day")]
    pub review_reads_per_day: u32,

    /// May skein act on pull requests on its own — labels, updates, merges, deleted branches?
    ///
    /// **Off by default, and the only default in skein that leans this way.** Everything else
    /// defaults toward showing you more; what this one gates is not a reading but a merge, and a
    /// fleet that starts merging because a config file was absent is not one anybody would trust
    /// twice.
    ///
    /// `$SKEIN_PR_WORKFLOWS=on|off` overrides — so a fleet doing something you want stopped can be
    /// stopped from the command line that starts the server, without the cockpit and without
    /// finding the file. See [`crate::prwork`].
    #[serde(default)]
    pub pr_workflows: bool,
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
    /// Superseded by named `SyncConnection`s, which pair a gateway with the token that mints at it.
    /// Read once by the migration and then cleared; kept so a pre-connections `config.json` still
    /// parses. A credential was never here and never will be — this file is written 0644 and
    /// round-trips through the browser on every settings save.
    ///
    /// (This paragraph spent a while sitting above `ai_enrichment`, describing a different and dead
    /// setting, because nothing checks that a doc comment is above the thing it is about.
    /// `prose-check.py` verifies that named symbols exist, not that they are named in the right
    /// place.)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sync_gateway_url: String,
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
    // In-fleet skein cannot do this, and the reason is worth being exact about because half of it
    // still works. The **agent** is reachable: `sbx` forwards the host's into the sandbox, so
    // `$SSH_AUTH_SOCK` there is the host's agent and `ssh-add -l` lists the host's keys. The **key
    // file** is not — `~/.ssh/id_ed25519` is a path on the host, and the sandbox has its own `~`.
    //
    // So this would fail on the file, which is the right outcome by accident and the wrong message
    // for it: "ssh key not found" reads as a mistyped path. The person's move is to run `ssh-add`
    // on the host, where both the key and their agent are, and the forward carries it in from
    // there — exactly as it does for a host-driven skein, which also never handles the key itself.
    // Always, now that in-fleet is the only place skein runs: the key is a host path and this
    // process is not on the host. Everything below this line was the host arm and went with it.
    Err(format!(
        "{key} is a path on the host, and skein is running inside the fleet — it cannot read the \
         key. Run `ssh-add {key}` on the host instead: sbx forwards that agent into the sandbox, \
         and the key itself never enters it either way"
    ))
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
    let mut cfg = read_or_default();
    // **A fleet always has a name** (SKEIN-484). The field carries a serde default, so an ABSENT
    // key already resolves to `skein-fleet` — but a key present and empty did not, and `""` is
    // exactly what a partial config write leaves behind, which the field's own comment above
    // records as having "unmade the whole fleet" once already.
    //
    // Repaired here rather than refused, because refusing costs a person their settings at the
    // moment they are least able to fix them, and there is only one honest reading of an empty
    // fleet name: nobody chose it. Every one of the ~43 callers of `place::fleet_sandbox` gets the
    // invariant for free, and `board::load_views` DEPENDS on it — with no name it would build the
    // board from `placed_boxes("")`, which matches nothing, and show an empty fleet as a fact.
    if cfg.fleet_sandbox.trim().is_empty() {
        cfg.fleet_sandbox = default_fleet_sandbox();
    }
    cfg
}

/// The settings as they are on disk, before [`load_config`] repairs what cannot be meant.
fn read_or_default() -> Config {
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

fn default_review_reads_per_day() -> u32 {
    100
}

/// Every field here calls the SAME function its `#[serde(default = …)]` names.
///
/// The four booleans used to be spelled as a literal `true` while serde called `default_true`, which
/// is two spellings of one default and the classic way for the two to drift: a field whose serde
/// attribute changes leaves this impl quietly disagreeing, and the disagreement shows up as an
/// absent key behaving differently from a present one. Nothing tests the equivalence, so the fix is
/// to make there be nothing to test.
impl Default for Config {
    fn default() -> Self {
        Config {
            seed_gh_secret: false,
            pr_workflows: false,
            review_model: String::new(),
            default_agent: default_agent(),
            base_branch: String::new(),
            confirm_destroy: default_true(),
            ssh_key: String::new(),
            scope_git_to_repo: default_true(),
            github_app_id: String::new(),
            github_app_key: String::new(),
            ai_enrichment: false,
            review_summaries: default_true(),
            review_reads_per_day: default_review_reads_per_day(),
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
        }
    }
}

#[cfg(test)]
mod tests {
    /// **A fleet always has a name, whatever is in the file** (SKEIN-484).
    ///
    /// The serde default only covers an ABSENT key. A key present and EMPTY is what a partial
    /// config write leaves behind — the hazard `fleet_sandbox`'s own doc records as having "unmade
    /// the whole fleet" once — and it used to travel all the way to `board::load_views`, which
    /// would then build the board from `placed_boxes("")`: no matches, an empty fleet reported as
    /// a fact rather than as a failure to look.
    ///
    /// Asserted through `load_config` and the real file rather than on the struct, because the
    /// struct is not what anybody reads: the repair has to be where the ~43 callers of
    /// `place::fleet_sandbox` will get it.
    #[test]
    fn a_fleet_always_has_a_name_however_the_file_was_written() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", dir.as_ref() as &std::path::Path);
        let path = (dir.as_ref() as &std::path::Path).join("config.json");
        for written in [
            r#"{"fleet_sandbox":""}"#,
            r#"{"fleet_sandbox":"   "}"#,
            // And the case the serde default already covered, so the two cannot come apart.
            r#"{}"#,
        ] {
            std::fs::write(&path, written).unwrap();
            assert_eq!(
                load_config().fleet_sandbox.trim(),
                default_fleet_sandbox(),
                "a config written as {written} left the fleet unnamed"
            );
        }
        // Non-vacuity: a name that WAS chosen is kept, or the assertion above would pass on a
        // function that ignored the file entirely.
        std::fs::write(&path, r#"{"fleet_sandbox":"other-fleet"}"#).unwrap();
        assert_eq!(load_config().fleet_sandbox, "other-fleet");
        std::env::remove_var("SKEIN_HOME");
    }

    use super::*;
    use crate::testutil::{env_lock, tempdir};

    /// The exact shape that caused this: valid JSON, the setting the user wanted plainly visible,
    /// and one *other* field serde cannot deserialise.
    const ONE_BAD_FIELD: &str = r#"{
        "confirm_destroy": false,
        "review_reads_per_day": "12",
        "ssh_key": "~/.ssh/id_ed25519"
    }"#;

    #[test]
    fn a_config_that_does_not_parse_is_reported_rather_than_silently_defaulted() {
        let _guard = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        fs::write(config_json(), ONE_BAD_FIELD).unwrap();

        // What the old code did, and why it was so hard to see: `confirm_destroy` reads back its
        // default while the file says the opposite, because one unrelated field discarded the whole
        // object. The setting is written here as the non-default so the discard is visible at all —
        // a fixture agreeing with the default would pass whether or not the file was read.
        assert!(load_config().confirm_destroy);
        let why = config_error().expect("an unparseable config must be reportable");
        // serde_json names the type and the position rather than the field, so the locator is what
        // makes this fixable — "somewhere in your config" would leave the user no better off than
        // the silence it replaced.
        assert!(
            why.contains("line 3") && why.contains("expected u32"),
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

    /// A first run must not be mistaken for a broken file: there is nothing to protect yet, and
    /// refusing here would mean skein could never write its first config.
    #[test]
    fn an_absent_config_is_not_an_error_and_saves_normally() {
        let _guard = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);

        assert!(config_error().is_none());
        // Saved as the non-default, so this proves a round trip rather than agreeing with a default
        // it never had to read.
        let want = Config {
            confirm_destroy: false,
            ..Config::default()
        };
        save_config(&want).unwrap();
        assert!(!load_config().confirm_destroy);
        assert!(config_error().is_none());

        env::remove_var("SKEIN_HOME");
    }

    /// The key is on the host and the agent is the host's; only one of them is reachable in-fleet.
    ///
    /// `sbx` forwards the host's ssh-agent into the sandbox, so `$SSH_AUTH_SOCK` there IS the host's
    /// agent — the forward belongs to the sandbox rather than to skein, and does not change with
    /// where skein runs. What does not travel is the key *file*: `~/.ssh/id_ed25519` names a path on
    /// the host, and the sandbox has its own `~`.
    ///
    /// So `ssh-add` from inside would fail on the file, which is the right outcome reached by the
    /// wrong route — "ssh key not found" reads as a mistyped path, and sends somebody to fix a
    /// setting rather than to run one command where their key already is.
    #[test]
    fn a_key_on_the_host_is_not_loaded_from_inside_the_fleet() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env::set_var("SKEIN_SSH_KEY", "~/.ssh/id_ed25519");

        let why = ensure_ssh_key().expect_err("skein read a host key path from inside the sandbox");
        assert!(
            why.contains("on the host") && why.contains("ssh-add"),
            "the refusal does not say where to run it: {why}"
        );
        assert!(
            !why.contains("not found"),
            "it still reads as a mistyped path, which is the wrong thing to go and check: {why}"
        );

        // No key configured is a no-op in both, and stays one: the agent's existing keys are
        // forwarded as-is, and there is nothing for skein to do about them either way.
        env::remove_var("SKEIN_SSH_KEY");
        let mut cfg = load_config();
        cfg.ssh_key = String::new();
        save_config(&cfg).unwrap();
        assert!(ensure_ssh_key().is_ok());

        env::remove_var("SKEIN_HOME");
    }
}
