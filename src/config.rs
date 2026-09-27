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
///
/// The root is [`crate::util::fleet_root`]'s, not a private `/boxes` default beside it: that copy
/// answered the live fleet to an unpinned test, safe only while [`skein_home`] asserts first (SKEIN-694).
fn volume_marker() -> Option<PathBuf> {
    let root = crate::util::fleet_root();
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
    /// The branch a box's clone starts from, and the head of the diff's base-ref ladder. Empty ⇒
    /// whatever the remote calls its own default.
    ///
    /// **The cockpit's Settings pane is the only way in** (SKEIN-649). This line used to call the
    /// field the UI equivalent of an environment variable, and to say it was the base for
    /// `gh pr create` and merge when a repo did not specify one. Every clause of that had stopped
    /// being true: the variable went with the box-level PR tools and nothing in the tree reads it
    /// any more, so setting it produced no error and no base branch; a merge reads the pull
    /// request's own `base` from GitHub (`prq::base_and_head`) and never this; and a repo has no
    /// base of its own to specify.
    ///
    /// The saved value is not trusted on its own. [`crate::fleet::base_branch`] asks the remote
    /// with `ls-remote --symref` and honours this only if the remote really has such a branch,
    /// which is how a base of `develop` is kept for the repos that have one without breaking the
    /// repos that do not; `diff::diff_base_refs` leads its ladder with `origin/<value>` for the
    /// same remote-first reason. Those two are what read this field; what reads the resolver is the
    /// clone a box comes up on (`fleet::clone_script`) and `reviewbox::open_at`.
    #[serde(default)]
    pub base_branch: String,
    /// Confirm before a destructive **Destroy** (clone-mode boxes lose unpushed commits). The cockpit
    /// reads this to decide whether to prompt.
    #[serde(default = "default_true")]
    pub confirm_destroy: bool,
    // No `ssh_key` (SKEIN-947). It named a key file on the host, which skein in the fleet cannot
    // read, so nothing ever loaded it. A person runs `ssh-add` on the host and `sbx` forwards that
    // agent. A config.json written before still carries the key and still loads, because serde
    // ignores a field this struct does not have:
    // `a_config_that_still_names_an_ssh_key_loads_and_keeps_its_other_settings`.
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
    /// Whose GitHub identity a pull-request review acts with: `"me"` or `"app"` (SKEIN-516).
    ///
    /// **`"me"` by default, and that is the owner's rule rather than a placeholder**: everything
    /// skein does on its own is done as the owner, with no "on behalf of", so a review reads and
    /// posts with the owner's own token (`crate::prq::host_token`). `"app"` is for team installs,
    /// where the review should act as the skein GitHub App instead: the session is handed
    /// `gitgate::mint_token(slug)`, and only when an App is configured.
    ///
    /// A string rather than an enum, on purpose. `config.json` is hand-editable and one field serde
    /// cannot read discards the whole file (`config_error`), so a typo here would cost every other
    /// setting. Anything that is not `"app"` reads as `"me"` — see [`Config::reviews_as_app`].
    #[serde(default = "default_review_identity")]
    pub review_identity: String,
    /// Spend *rationed* Haiku calls on the Claude subscription to enrich the board: a one-line
    /// summary for a box with no journal, and a conservative safety gate on **Continue N**.
    ///
    /// Off by default because skein runs inside a box where `claude` is logged in, so these calls
    /// share the fleet's rate-limit window. `$SKEIN_AI=off` holds it off, and nothing in the
    /// environment can switch it on ([`env_holds_off`]). Lazy and cached per turn-end when on — never a per-tick sweep. See [`crate::ai`].
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
    /// `$SKEIN_REVIEW_AI=off` holds it off ([`env_holds_off`]). Off is not a broken state — every PR simply reads
    /// "not summarised" and stays at full attention. See [`crate::review`].
    #[serde(default = "default_true")]
    pub review_summaries: bool,
    /// Which model reads and reviews pull requests — summaries, questions, drafted comments, and
    /// the actual review. Empty means each call's own default (summaries stay on the cheap model,
    /// the writing calls on the stronger one). `$SKEIN_REVIEW_MODEL` overrides, and
    /// `$SKEIN_AI_MODEL` — the everything-override — still trumps both, as it always has.
    #[serde(default)]
    pub review_model: String,
    /// Load skein's own plugin into every agent's session: the resource holds and the `skein_*`
    /// tools (box-plugin §2.1, SKEIN-1058). **On by default**, and fleet-wide on purpose: there is
    /// no per-box setting (owner's answer 4).
    ///
    /// Read on the host by [`crate::runtime::for_box`], which is what decides whether a start or
    /// resume passes `--plugin-dir`; a box cannot write this file, so a box cannot change it. It
    /// takes effect at a box's next session, because a running agent keeps the argv it started
    /// with. `$SKEIN_BOX_PLUGIN=off` holds it off ([`env_holds_off`]).
    #[serde(default = "default_true")]
    pub box_plugin: bool,
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
    /// `$SKEIN_PR_WORKFLOWS=off` holds it off — so a fleet doing something you want stopped can be
    /// stopped from the command line that starts the server, without the cockpit and without
    /// finding the file. It cannot hold it ON against the pause ([`env_holds_off`]). See
    /// [`crate::prwork`].
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
    /// How many whole days a compiler artefact may go untouched before a full disk offers it back,
    /// and **0 turns the offer off**.
    ///
    /// This is the one figure in skein's disk work that is a judgement rather than a derivation,
    /// which is exactly why it is a setting and not a constant. Everything else
    /// [`crate::fleet`] offers to delete it can *prove* dead — a directory under `.skein` that is
    /// neither skein's own nor any live box's ([`crate::fleet::substrate_strays`]), or a build
    /// directory whose own dependency files name a source tree that is no longer on disk
    /// ([`crate::fleet::orphaned_builds`]). Cargo's superseded generations cannot be proved dead at
    /// all: rebuild a crate under a changed feature set and the old `.rlib` stays beside the new one
    /// under a different metadata hash for ever, and nothing in either file says which is which —
    /// cargo's own `.fingerprint` register does not either, because it keeps one per generation and
    /// removes none. The only handle left is how long since something read or wrote the file.
    ///
    /// **Five days by default**, the owner's own number, and the reason it has to be yours to change
    /// is that its correctness is local: a fleet whose lanes run for a week is wrong to call five
    /// days dead, and one that rebuilds hourly could halve it. Being wrong in either direction costs
    /// a **slower rebuild and never work** — the artefact is regenerated from source that was never
    /// touched — which is what makes an age acceptable evidence here and nowhere else in this
    /// module. `0` is a supported state and not a broken one: skein then says nothing about age at
    /// all, and the two provable sweeps above are unaffected.
    ///
    /// Read by [`crate::health::disk_health`], and only when a filesystem is already past its line.
    /// Whole days, counted the way `find -mtime +N` counts them, so the figure skein reports and the
    /// command it prints name the same files.
    #[serde(default = "default_stale_build_days")]
    pub stale_build_days: u32,
    /// The `user.name` every box commits as. Empty ⇒ read from this host's **global** git config.
    ///
    /// A box's checkout is a fresh clone into a private HOME, so it inherits neither the host's
    /// global gitconfig nor anything a previous box set — and the first commit fails with `Author
    /// identity unknown`, at the moment the work is finished rather than when the box was built.
    ///
    /// "The repo's host clone" is what this said, and there is no host clone to read: a repo is a
    /// remote, and `crate::fleet::box_identity` asks git's global config. The scope in that
    /// sentence is the whole of SKEIN-541.
    #[serde(default)]
    pub git_name: String,
    /// The `user.email` every box commits as. Empty ⇒ read from this host's global git config.
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

/// Five whole days. See [`Config::stale_build_days`] for why this one is the owner's to change.
fn default_stale_build_days() -> u32 {
    5
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
/// differ. The case that found it was `fleet_agent`, a setting deleted with the in-sandbox agent
/// (SKEIN-573): it defaulted to `false` then, so the transport silently stopped being installed while
/// the file said `true` and was right. A default is indistinguishable from a choice, so the failure
/// has to be *said*.
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
/// Does `$name` hold a switch off?
///
/// **An environment variable may only turn things off** (the owner, 2026-09-27). It exists so a
/// fleet doing something you want stopped can be stopped from the command line that starts the
/// server; it must never defeat a switch the person turned off. `$SKEIN_PR_WORKFLOWS=on` beating
/// the pause button was the case that decided it: the button wrote `false`, the env read back
/// `true`, and the cockpit said "workflows resumed" while merges carried on. So a yes in the
/// environment means nothing, and each switch reads `setting && !env_holds_off(..)`.
pub fn env_holds_off(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref().map(str::trim),
        Some("off" | "0" | "false" | "no")
    )
}

/// The switches an environment variable can hold off, by the `Config` field each one holds.
pub const ENV_SWITCHES: [(&str, &str); 4] = [
    ("pr_workflows", "SKEIN_PR_WORKFLOWS"),
    ("ai_enrichment", "SKEIN_AI"),
    ("review_summaries", "SKEIN_REVIEW_AI"),
    ("box_plugin", "SKEIN_BOX_PLUGIN"),
];

/// Which settings the environment is holding right now: `Config` field → the variable holding it.
///
/// Only what is **in force**, so Settings can say "held by `$SKEIN_X`" beside a control exactly
/// when that control is not the one deciding, and say nothing otherwise. A note that is always
/// there reads as boilerplate and is right only when the variable happens to be set (SKEIN-1141
/// put one on each field; this makes it true).
pub fn held_by_env() -> std::collections::BTreeMap<&'static str, &'static str> {
    let mut held: std::collections::BTreeMap<&'static str, &'static str> = ENV_SWITCHES
        .iter()
        .filter(|(_, var)| env_holds_off(var))
        .map(|&(field, var)| (field, var))
        .collect();
    // The review model is a value, not a switch, so any value set holds it. `$SKEIN_AI_MODEL` wins
    // over `$SKEIN_REVIEW_MODEL` (`ai::binary_and_model`), so it is the one named when both are.
    let set = |var: &str| env::var(var).is_ok_and(|v| !v.trim().is_empty());
    if set("SKEIN_AI_MODEL") {
        held.insert("review_model", "SKEIN_AI_MODEL");
    } else if set("SKEIN_REVIEW_MODEL") {
        held.insert("review_model", "SKEIN_REVIEW_MODEL");
    }
    held
}

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
                    "skein: cannot read your settings ({why}) — every setting is at its default \
                     until that file parses. The file is left alone: fix it, and skein reads it \
                     again the next time it looks — no restart."
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
        // **A fleet always has a name on DISK too, not just in memory** (SKEIN-768). `load_config`
        // repairs a blank on the way out (see its own note), so `GET /api/settings` answered
        // `skein-fleet` for a file that held `""`: the pane, the board and `skein doctor` agreeing
        // on a name the file did not contain. These two lines and the two below are the same repair
        // on the way IN, and they are NOT duplicates of each other — deleting either one reopens a
        // different defect, and the test names which.
        //
        // **This one is for a closure that READS the name.** `api_fleet_create` returns
        // `config.fleet_sandbox` out of its closure and its caller refuses on an empty one
        // (`src/bin/skein-server/fleet.rs:270`); with a blank on disk that refusal fired on a fleet whose
        // every other reader had been handed `skein-fleet`. Repairing before `f` is the only thing
        // that can reach a closure's read — the repair below runs after the closure has already
        // answered.
        if current.fleet_sandbox.trim().is_empty() {
            current.fleet_sandbox = default_fleet_sandbox();
        }
        let out = f(&mut current)?;
        // **This one is for a closure that CLEARS the name**, which the repair above cannot help
        // with: it has already run by the time `f` writes. `api_set_settings` merges the posted body
        // onto the stored settings, so `{"fleet_sandbox":""}` arrives as a closure that empties the
        // field on purpose — the defect as reported — and only a repair after `f` keeps that off
        // the disk. It is what makes the invariant hold *whatever route wrote it*: `update_config`
        // is the sole writer of `config.json` outside test code (`save_config`'s 47 call sites in
        // `src/` are all under `#[cfg(test)]`), so with this line a blank cannot reach the file by
        // any route at all.
        //
        // Repaired rather than refused, for `load_config`'s reason: there is one honest reading of
        // an empty fleet name, and it is that nobody chose it.
        if current.fleet_sandbox.trim().is_empty() {
            current.fleet_sandbox = default_fleet_sandbox();
        }
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

fn default_review_identity() -> String {
    "me".to_string()
}

impl Config {
    /// Does a review act as the skein GitHub App rather than as the owner?
    ///
    /// Only an exact `"app"` says so. Every other value, a typo included, is the owner's default,
    /// because the alternative reading of a value nobody meant is a review acting as an identity
    /// nobody chose.
    pub fn reviews_as_app(&self) -> bool {
        self.review_identity.trim() == "app"
    }
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
            scope_git_to_repo: default_true(),
            github_app_id: String::new(),
            github_app_key: String::new(),
            review_identity: default_review_identity(),
            ai_enrichment: false,
            review_summaries: default_true(),
            box_plugin: default_true(),
            review_reads_per_day: default_review_reads_per_day(),
            fleet_sandbox: default_fleet_sandbox(),
            fleet_memory: default_fleet_memory(),
            fleet_cpus: String::new(),
            fleet_disk: String::new(),
            fleet_one_disk: default_fleet_one_disk(),
            box_disk_max: default_box_disk_max(),
            stale_build_days: default_stale_build_days(),
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
    /// **A review acts as the owner unless the owner says otherwise** (SKEIN-516) — for a config
    /// that never mentions the key, for the built-in default, and for a value nobody recognises.
    ///
    /// The concrete change that fails it: `default_review_identity` answering `"app"`, or
    /// `reviews_as_app` testing anything looser than the exact word.
    #[test]
    fn a_review_acts_as_the_owner_unless_the_config_says_app() {
        let absent: super::Config = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.review_identity, "me");
        assert!(!absent.reviews_as_app());
        assert!(!super::Config::default().reviews_as_app());
        let garbled: super::Config =
            serde_json::from_str(r#"{"review_identity":"application"}"#).unwrap();
        assert!(!garbled.reviews_as_app(), "an unknown word chose the App");
        let app: super::Config = serde_json::from_str(r#"{"review_identity":"app"}"#).unwrap();
        assert!(app.reviews_as_app());
    }

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
    use crate::testutil::{env_lock, env_pins, tempdir};

    /// **The volume marker asks `util::fleet_root` where the fleet is, so it inherits its refusal**
    /// (SKEIN-694).
    ///
    /// Called directly rather than through [`skein_home`], because through `skein_home` it cannot be
    /// observed: an unpinned `$SKEIN_HOME` panics first. That is exactly what made the private copy
    /// safe and fragile at once — so this asks the function itself, which is what a second caller
    /// would do.
    ///
    /// **What makes it fail:** giving `volume_marker` its own `$SKEIN_FLEET_ROOT`-else-`/boxes`
    /// again. The unpinned half then gets an answer — whatever the live fleet's marker says, or
    /// `None` — instead of the refusal, and the second assertion names it. The first half is the
    /// control: a marker in a pinned fleet root IS read, so the refusal below is not simply a
    /// function that never answers.
    #[test]
    fn the_volume_marker_refuses_an_unpinned_fleet_root_like_every_other_fleet_path() {
        let _g = env_lock();
        let root = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_FLEET_ROOT", &root);
        std::fs::create_dir_all(root.join(".skein")).unwrap();
        std::fs::write(root.join(".skein/skein-home"), "/the/volume/home\n").unwrap();
        assert_eq!(
            volume_marker(),
            Some(PathBuf::from("/the/volume/home")),
            "a marker in the pinned fleet root was not read, so the refusal below proves nothing"
        );

        env.unset("SKEIN_FLEET_ROOT");
        let answered = std::panic::catch_unwind(volume_marker);
        assert!(
            answered.is_err(),
            "volume_marker answered {answered:?} to a test process with no $SKEIN_FLEET_ROOT — it \
             has its own copy of the /boxes default again, and on a developer box that reads the \
             owner's live fleet"
        );
    }

    /// **The pane and the file cannot disagree about the fleet's name, whatever route wrote it**
    /// (SKEIN-768).
    ///
    /// Stated as an invariant rather than as the case that found it, because the case was only one
    /// route: `POST /api/settings -d '{"fleet_sandbox":""}'` answered 200 and left `""` in the file
    /// while `GET /api/settings` went on reporting `skein-fleet`, since the GET returns
    /// `load_config()` (`src/bin/skein-server/settings.rs:244`), which repairs a blank, and the POST writes
    /// through `update_config`, which read the raw file and never saw the repair. Asserting that one
    /// POST would leave every other writer free to reintroduce it.
    ///
    /// Both halves are read the way the two halves of the defect were: the served value through
    /// `load_config`, exactly as the handler does, and the stored value out of the JSON text — not
    /// through any reader in this module, or a reader that repairs could make the two agree by
    /// repairing them both.
    ///
    /// `update_config` is the whole of "whatever route wrote it": it is the only writer of
    /// `config.json` outside test code. Every one of the 47 `save_config(` call sites under `src/`
    /// is inside a `#[cfg(test)]` module —
    /// `grep -rn 'save_config(' --include=*.rs src/ | grep -v 'fn save_config'`, and each hit
    /// compared against its file's `#[cfg(test)]` line.
    ///
    /// The change that makes this fail, named before it was written and then made: move the repair
    /// in `update_config` from after the caller's closure to before it. That is the placement that
    /// looks equivalent and is not — it repairs what the closure reads and not what it writes, so
    /// the `api_set_settings` shape below (a closure that deliberately clears the field) puts `""`
    /// back on disk and the first assertion fails, naming the two values.
    #[test]
    fn the_pane_and_the_file_cannot_disagree_about_the_fleets_name() {
        let _g = env_lock();
        let home = tempdir();
        // Pinned as well as `$SKEIN_HOME`: `util::fleet_root` falls back to `/boxes`, which on this
        // machine is the owner's live fleet (SKEIN-530, SKEIN-685, SKEIN-690).
        let fleet = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", &fleet);

        // What the file itself says, straight out of its JSON. `None` is "no file, or no key".
        let file_says = || -> Option<String> {
            let text = fs::read_to_string(config_json()).ok()?;
            let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
            Some(parsed.get("fleet_sandbox")?.as_str()?.to_string())
        };
        let agree = |route: &str| {
            let served = load_config().fleet_sandbox;
            let stored = file_says();
            assert!(
                !served.trim().is_empty(),
                "after {route}, the settings pane would show no fleet name at all"
            );
            assert_eq!(
                Some(served.as_str()),
                stored.as_deref(),
                "after {route}, GET /api/settings says {served:?} and the file says {stored:?}"
            );
            served
        };

        // 1. The route that found it: `api_set_settings` merges the posted body onto the stored
        //    settings, so `{"fleet_sandbox":""}` reaches `update_config` as a closure that clears
        //    the field on purpose.
        update_config(|c| {
            c.fleet_sandbox = String::new();
            Ok(())
        })
        .unwrap();
        assert_eq!(
            agree("a POST that cleared the fleet name"),
            default_fleet_sandbox()
        );

        // 2. The same thing spelled with spaces, which JSON, a form field and a shell all make easy
        //    to send and which `trim` is the only reader that notices.
        update_config(|c| {
            c.fleet_sandbox = "   ".into();
            Ok(())
        })
        .unwrap();
        assert_eq!(
            agree("a POST of a whitespace-only fleet name"),
            default_fleet_sandbox()
        );

        // 3. A file that was ALREADY blank — hand-edited, or written by a build without this repair
        //    — and a route that never names the field (`api_fleet_create` writes only the sizes).
        //    Nothing in the closure can fix this one; only the write can.
        fs::write(config_json(), r#"{"fleet_sandbox":"","fleet_cpus":"2"}"#).unwrap();
        update_config(|c| {
            c.fleet_cpus = "4".into();
            Ok(())
        })
        .unwrap();
        assert_eq!(
            agree("a resize against a file that was already blank"),
            default_fleet_sandbox()
        );
        // ...and the route's own change landed, so the agreement above is not the agreement of a
        // write that never happened.
        assert_eq!(load_config().fleet_cpus, "4");

        // 4. A first run: no file at all, which is an empty opinion rather than an unreadable one.
        fs::remove_file(config_json()).unwrap();
        update_config(|_| Ok(())).unwrap();
        assert_eq!(
            agree("the first write of a fresh install"),
            default_fleet_sandbox()
        );

        // 5. Non-vacuity for the whole test: a name somebody CHOSE is what both of them say. Every
        //    assertion above would pass on an `update_config` that stamped `skein-fleet` over the
        //    field on every write, which would lose the setting instead of repairing it.
        update_config(|c| {
            c.fleet_sandbox = "other-fleet".into();
            Ok(())
        })
        .unwrap();
        assert_eq!(agree("a POST that named a fleet"), "other-fleet");
    }

    /// **And a closure that READS the fleet's name is never handed a blank one** (SKEIN-768).
    ///
    /// The sibling above is about what reaches the disk, and the repair that holds it runs after the
    /// caller's closure — too late to be of any use to a closure that reads the field. This is the
    /// other half, and it is a different defect rather than a restatement: `api_fleet_create`
    /// returns `config.fleet_sandbox.trim()` out of its closure and refuses on an empty one
    /// (`src/bin/skein-server/fleet.rs:270`, `no fleet sandbox is named (fleet_sandbox is empty)`), so
    /// with `""` on disk a create failed on a fleet that every other reader — the pane, the board,
    /// all ~43 `place::fleet_sandbox` callers — had been told was `skein-fleet`. Measured: against a
    /// server with one repair and not the other, that refusal was still reachable on the FIRST such
    /// call, and only stopped being reachable because the call itself repaired the file.
    ///
    /// A blank can no longer be written (that is the sibling), so what this covers is a file that
    /// already holds one: hand-edited, or left by a build older than this repair.
    ///
    /// The change that makes it fail, named before it was written and then made: delete the repair
    /// ABOVE `f` in `update_config`. It fails here and nowhere else — every route in the sibling
    /// stays green, because the repair after `f` still keeps the disk right. Deleting the repair
    /// BELOW `f` instead fails the sibling and leaves this one green. Neither can be removed
    /// quietly, which is the point of writing them as two tests.
    #[test]
    fn a_closure_that_reads_the_fleets_name_is_never_handed_a_blank_one() {
        let _g = env_lock();
        let home = tempdir();
        let fleet = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", &fleet);

        // `api_fleet_create`'s closure, spelled the way it spells it — and what its caller then
        // tests is exactly `sandbox.is_empty()`.
        let what_the_closure_saw =
            || update_config(|c| Ok(c.fleet_sandbox.trim().to_string())).unwrap();

        for written in [
            r#"{"fleet_sandbox":""}"#,
            r#"{"fleet_sandbox":"   "}"#,
            // The absent key too, so the serde default and this repair cannot come apart.
            r#"{"fleet_cpus":"2"}"#,
        ] {
            fs::write(config_json(), written).unwrap();
            assert_eq!(
                what_the_closure_saw(),
                default_fleet_sandbox(),
                "a config written as {written} would refuse a fleet create for want of a name the \
                 rest of skein can see"
            );
        }

        // Non-vacuity: a name somebody chose is what the closure is handed, not the default. Without
        // this the assertions above would pass on an `update_config` that gave every closure
        // `skein-fleet` regardless of the file — which would refuse nothing and create the wrong
        // fleet.
        fs::write(config_json(), r#"{"fleet_sandbox":"other-fleet"}"#).unwrap();
        assert_eq!(what_the_closure_saw(), "other-fleet");
    }

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
        // Bound after `home`, so the pin goes back before the directory it names is removed.
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
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

    /// **A config.json from before SKEIN-947 still loads, and keeps every other setting in it.**
    ///
    /// `ssh_key` left `Config`, but files written while it was there still carry it. Two ways to
    /// get that wrong, and each fails an assertion here: a `deny_unknown_fields` on `Config`
    /// fails the whole parse, so `confirm_destroy` reads back as its default and `config_error`
    /// reports it. A field that came back under the old name would be read again, and nothing in
    /// the struct says so, which is why the serialised form is checked for the name as well.
    #[test]
    fn a_config_that_still_names_an_ssh_key_loads_and_keeps_its_other_settings() {
        let _g = env_lock();
        let home = tempdir();
        // Restored from `Drop`, so a failing assertion below cannot leave `$SKEIN_HOME` pointing at
        // a directory that is gone for the next test in this process.
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        fs::write(
            config_json(),
            r#"{"confirm_destroy": false, "ssh_key": "~/.ssh/id_ed25519"}"#,
        )
        .unwrap();

        assert!(
            config_error().is_none(),
            "an old config with ssh_key in it no longer parses: {:?}",
            config_error()
        );
        assert!(
            !load_config().confirm_destroy,
            "the other settings in an old config were dropped because it names ssh_key"
        );
        let written = serde_json::to_string(&load_config()).unwrap();
        assert!(
            !written.contains("ssh_key"),
            "Config carries ssh_key again, and nothing in the fleet can act on it: {written}"
        );
    }
}
