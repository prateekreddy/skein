//! Making the fleet sandbox: the create line and its mounts, sizing it from what the host has,
//! asking the warden to create it, and the refusals that price a destroy before it happens.

use super::*;

/// The `sbx create` argv for the fleet sandbox.
///
/// `shell` rather than an agent: nothing runs in the sandbox itself — every agent runs inside a box's
/// namespace, started by `box-session.sh`. The mounts are [`fleet_mounts`]: `~/.skein/repos`, the
/// *parent* of every managed repo's store, so adding one later needs no recreate — plus any store
/// `--store` put outside it. A box's checkout is not mounted at all, because boxes clone from the
/// remote onto VM-local disk (measured ~5× faster to write and ~14× faster to read than a virtiofs
/// mount, which matters for a build).
///
/// Memory and CPUs come from [`Config`], and both are ceilings the boxes share rather than one
/// reservation each — which is what makes them safe to set generously. CPUs default to every host
/// core but one, so the machine keeps answering while the fleet compiles.
///
/// **The cockpit's port is published here**, which is the whole of what used to be a fourth line
/// somebody ran by hand. `sbx create` takes `-p/--publish` — read off `sbx create --help`, not
/// assumed — so the one thing the sandbox genuinely cannot do for itself is done by the same
/// command that makes the sandbox, at the only moment nothing is racing for the number.
///
/// The trade this makes, said out loud: a host port already in use now fails the *create* rather
/// than a step after it. That is the better failure. A create that succeeded and a publish that was
/// skipped left a fleet that looks installed, serves nothing a browser can reach, and says so
/// nowhere — which is the state the fourth line produced every time somebody forgot it.
pub fn create_argv(sandbox: &str, mounts: &[String]) -> Vec<String> {
    let config = load_config();
    let mut argv = vec!["create".to_string(), "--name".into(), sandbox.to_string()];
    let memory = config.fleet_memory.trim();
    if !memory.is_empty() {
        argv.push("-m".into());
        argv.push(memory.to_string());
    }
    let cpus = config.fleet_cpus.trim().to_string();
    let cpus = if cpus.is_empty() {
        host_cpus_less_one()
    } else {
        cpus
    };
    if !cpus.is_empty() {
        argv.push("--cpus".into());
        argv.push(cpus);
    }
    // Before the agent, because sbx's own usage is `sbx create [flags] AGENT PATH [PATH...]` and a
    // flag after `shell` is an argument to the `shell` subcommand instead.
    let port = server_sandbox_port();
    argv.push("-p".into());
    argv.push(format!("{port}:{port}"));
    // **The one thing in the fleet sandbox that survives its own restart.**
    //
    // pid 1 is `tini`; there is no systemd, no cron, no systemctl — measured in a live fleet. So a
    // sandbox that stops and starts comes back with the whole install intact on disk and nothing
    // serving: no supervisor, no doorway, the cockpit's port unbound, and the mapping published
    // above forwarding into nothing. `commands.startup` is the only hook in reach that runs at every
    // start, and it is the same mechanism skein already trusts for boxes.
    //
    // Before the agent for the reason directly above, which this file learned about `-p` the
    // expensive way.
    argv.push("--kit".into());
    argv.push(fleet_kit_dir().to_string_lossy().into_owned());
    argv.push("shell".into());
    argv.extend(mounts.iter().cloned());
    argv
}

/// The `sbx create` line for THIS installation, as a person would type it.
///
/// **The mount set is the whole of it.** sbx fixes mounts at create, and no verb adds one to a
/// sandbox that exists (checked against `sbx --help`, which lists 25 — an earlier version of this
/// comment reasoned from a remembered nine) — so a line short of a path cannot be
/// repaired, and the box it breaks comes up looking healthy with no store, no hooks and no probe.
/// [`fleet_serve_mounts`] rather than [`fleet_mounts`], because a fleet skein runs inside needs the
/// volume root itself and not merely the two directories beneath it.
///
/// Rendered through the same `Act::Create` the warden prompt uses, so the line printed by
/// `skein doctor` and the line skein would ask somebody to approve cannot come apart. That is also
/// why this lives here rather than in the CLI: a second renderer beside the first is exactly the
/// drift the prompt was built to avoid, and it kept `bin/skein` out of `warden_client`.
pub fn create_line(sandbox: &str) -> Result<String, String> {
    let mounts = fleet_serve_mounts();
    // The kit before the line that names it. This one is going to be READ and typed by a person, so
    // a `--kit` pointing at a directory nothing has written is a command that fails in their hands
    // for a reason they did not cause. Best-effort: a fleet they cannot create at all is worse than
    // one whose restarts they have to repair by hand, so a kit that could not be written is said and
    // not fatal.
    if let Err(e) = ensure_fleet_kit() {
        eprintln!("skein: could not write the fleet kit ({e}) — the create line below still names \
                   it, and the fleet will not put its own door back after a restart until it exists");
    }
    Ok(crate::warden_client::Act::Create {
        sandbox: sandbox.to_string(),
        argv: create_argv(sandbox, &mounts),
        env: create_env(),
    }
    .command())
}

/// Why the fleet's lifecycle cannot be driven from in here, what destroying it costs, and the lines
/// to run on the host instead. `None` only if no line could be worked out at all.
///
/// **Create and destroy both kill skein** — create because the sandbox does not exist yet, destroy
/// because it will not afterwards — so `docs/architecture.md` §7.5 puts fleet lifecycle outside the
/// fleet permanently. A resize IS a destroy and a create ([`resize_fleet`]), which is what made this
/// urgent: in-fleet the destroy *succeeds*, takes the machine this process is on with it, and the
/// last thing the browser renders is `resize failed:` — a failure message at the moment the
/// irreversible half worked (SKEIN-467).
///
/// **It lives here, in the library, rather than in either binary** (SKEIN-679). The cockpit's
/// rebuild route and `skein resize` are one person meeting one wall from two directions, and the
/// wall has to say one thing; the CLI stating it in its own voice would be a second message
/// drifting from the first the moment either is edited. That is the argument [`create_line`] makes
/// for itself just above — "a second renderer beside the first is exactly the drift the prompt was
/// built to avoid" — applied to the sentence around the line rather than to the line.
///
/// Refused rather than attempted, and refused with the lines to run out there. The `sbx` lines are
/// rendered by [`crate::warden_client::Act`] — the same renderer `skein doctor` prints and the
/// warden's own approval prompt uses — so what somebody is told to type and what skein would have
/// run cannot drift apart.
///
/// `replacing` is whether this act stands on a sandbox that is already there: a rebuild has to
/// remove the old one first, a first create has nothing to remove, and printing `sbx rm -f` for the
/// second would be a line that destroys whatever else answers to that name. So [`destroy_costs`]
/// and the destroy line arrive together or not at all.
pub fn fleet_lifecycle_refusal(what: &str, replacing: bool) -> Option<String> {
    // **Always** (SKEIN-576). This used to return `None` on a host, where the destroy could be
    // driven from here; skein runs inside the fleet, so a destroy takes the machine this process
    // is on and the answer is the line to run out there. `Option` is kept because the caller still
    // has to distinguish "no line could be worked out" from a refusal it can print.
    let sandbox = crate::place::fleet_sandbox();
    let mut why = format!(
        "skein is running inside the fleet sandbox, so it cannot {what} it from here: the sandbox \
         is the machine this process is on, and destroying it takes skein down before anything is \
         left to bring the boxes back. Fleet lifecycle lives on the host (docs/architecture.md \
         \u{a7}7.5)."
    );
    if replacing {
        why.push_str(&format!("\n\n{}", destroy_costs(&sandbox)));
        why.push_str(&format!(
            "\n\nOn the host, destroy it first: `{}`.",
            crate::warden_client::Act::Destroy {
                sandbox: sandbox.clone()
            }
            .command()
        ));
    }
    match create_line(&sandbox) {
        Ok(line) => why.push_str(&format!(" Then create it on the host with: `{line}`.")),
        // The mounts are fixed at create and are worked out from this installation, so a line that
        // could not be worked out is not one to guess at — see `bin/skein.rs`, which says the same.
        Err(e) => why.push_str(&format!(
            " The `sbx create` line could not be worked out from here — {e} — so run `skein \
             doctor` and copy the one it prints."
        )),
    }
    Some(why)
}

/// What `sbx rm -f <sandbox>` costs, in boxes, and the step that would make it safe.
///
/// **The old refusal's only mention of boxes was "nothing left to bring the boxes back"**, which
/// reads as availability — as though the boxes were somewhere else and skein had merely lost its
/// grip on them (SKEIN-445). It is data loss. Every box's checkout is VM-local, which is the thing
/// that makes builds fast and the entire reason [`resize_fleet`] exists, so the destroy line takes
/// every box's uncommitted and unpushed work with the sandbox. [`resize_fleet`]'s own doc is the
/// standard this is written to: "Nothing is destroyed until every box is safely on the host. A
/// partial copy is not a partial resize, it is lost work."
///
/// **The count is the number that makes it real**, so it is counted rather than gestured at, and it
/// comes from [`census_placed_boxes`] — the same census a resize refuses on. Three answers, and the
/// third is the one that matters: a census that FAILED is not a fleet with no boxes in it
/// (SKEIN-347). Reading a refusal as zero is how a checkout gets destroyed by a message that said
/// nothing was at stake, so it is reported as what it is.
///
/// **Inform and offer; never perform, never withhold** — the owner's decision on SKEIN-445 and
/// SKEIN-679, and each third of it is load-bearing. Stating the loss is not enough on its own;
/// doing the save uninvited would make a refusal copy gigabytes as a side effect of being read; and
/// holding the destroy line back would make somebody whose boxes are clean argue with the UI. So
/// the save is named as an act the person chooses, and the line follows it either way.
///
/// **What this sentence needed from SKEIN-680 now exists, so it names it.** [`save_boxes`] is the
/// act, `skein save` is its verb in a terminal and the fleet settings pane carries the button —
/// both landed with this sentence, because a fix line naming a verb the CLI has not got is worse
/// than none (`tests/fix_lines.rs`) and so is a button that is not there.
fn destroy_costs(sandbox: &str) -> String {
    let scale = match census_placed_boxes(sandbox) {
        // A fleet with nothing in it is a destroy somebody can run without thinking, and saying so
        // is the same duty as saying the opposite.
        Ok(boxes) if boxes.is_empty() => "No box is placed in it, so there is no checkout to \
             lose — but everything else inside the sandbox goes with it."
            .to_string(),
        Ok(boxes) => format!(
            "{} box{} would lose {}: {}.",
            boxes.len(),
            if boxes.len() == 1 { "" } else { "es" },
            if boxes.len() == 1 {
                "its checkout"
            } else {
                "their checkouts"
            },
            boxes
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Err(why) => format!(
            "skein could not count the boxes in it ({why}) — and could not ask is not the same as \
             nothing to lose, so read this as every box."
        ),
    };
    format!(
        "Destroying it is not a restart: every box's checkout lives inside that sandbox and \
         nowhere else — which is what makes builds fast — so `sbx rm -f` deletes it along with \
         every uncommitted and unpushed change in it. {scale} Nothing here has copied any of it \
         out, and nothing will as a side effect of showing you this.\n\nSave that work first if \
         any of it matters: `skein save` copies every box's whole tree onto the host and tells you \
         where each one went, and the cockpit's fleet settings offers the same as a button. It \
         destroys nothing, stops no box, and can be run while agents are working — it is a step of \
         its own, and it is the one to take before the line below."
    )
}

/// The environment `sbx create` needs for what its argv cannot carry — today, the sandbox's disk.
///
/// sbx takes memory and CPUs as flags but disk as an environment variable, read from the *create
/// command's* own environment: `DOCKER_SANDBOXES_ROOT_SIZE=40g sbx run claude` is the documented
/// form, and this is the same thing for `sbx create`. Root defaults to 20 GB.
///
/// This used to claim the value came from the *daemon's* environment, so it only landed if the
/// daemon happened to start with the create. That is wrong, and the running fleet is the proof: it
/// carries the configured 60g on a `vdb` of exactly 60G under a daemon that had been up for hours.
/// The correction matters because the false version made the disk setting look unreliable, and
/// invited working around it.
///
/// What is true is the second half: **the size is fixed for the life of the sandbox.** sbx has no
/// resize verb at all (`sbx --help`) — so changing a disk
/// means a new sandbox, exactly as changing memory does. [`resize_fleet`] is that path for both.
///
/// One shared disk is the fleet's real ceiling. Memory stopped summing when boxes started sharing a
/// sandbox; disk started summing for exactly the same reason.
///
/// There is a **second** disk this does not set: `DOCKER_SANDBOXES_DOCKER_SIZE`, the Docker data
/// disk at `/var/lib/docker`, 50 GB by default and sparse. It is where every box's `docker build`
/// output actually lands, so on a fleet — one Docker daemon shared by every box — it sums the same
/// way the root does, and skein neither sizes nor measures it.
pub fn create_env() -> Vec<(String, String)> {
    let disk = load_config().fleet_disk.trim().to_string();
    if disk.is_empty() {
        return Vec::new();
    }
    vec![("DOCKER_SANDBOXES_ROOT_SIZE".to_string(), disk)]
}

/// Every host directory the fleet sandbox must be able to see.
///
/// [`fleet_workspace`] covers repos skein manages, whose mirror and store both live under it. It
/// does **not** cover a store kept somewhere else — `skein add <git-url> --store …` still takes any
/// path for the store, even though the repo itself must now be a remote (`registrable_source`).
/// Such a store sits anywhere on the host, so it is mounted explicitly or the box cannot read it,
/// and provisioning fails for a reason that reads as a skein bug rather than a missing mount.
///
/// Deduped against the workspace, and against each other: mounting a path twice is not obviously
/// harmless, and mounting a *parent* of it is what keeps a later repo from needing a recreate.
///
/// And filtered by [`volume_exposure`], which is the rule that keeps every credential skein holds
/// out of every box: the two directories under `~/.skein` that boxes are given are mounted, and the
/// volume itself is not.
pub fn fleet_mounts() -> Vec<String> {
    let workspace = fleet_workspace();
    // The box-state parent too: boxes keep their conversation there, on the host, so it survives the
    // sandbox rather than only surviving a planned resize.
    let mut mounts = vec![workspace.clone(), box_state_root()];
    for repo in load_repos() {
        // The store, and NOT the checkout. A repo's working tree used to be mounted so that boxes
        // could clone from it and read the gitignored files `shared-paths.txt` names; they clone
        // from the mirror now (which is under the workspace above), and what the manifest names is
        // surfaced out of the store's own `shared-rw/` by `sandbox-bootstrap.sh`. Nothing on the
        // host seeds that directory any more — the two calls that did went with local-path repos,
        // and `start_box` records why. Nothing left in a box has any use for the tree its user
        // works in, so it is not in the sandbox at all — which is a stronger statement than the
        // read-only bind it replaces.
        for path in [repo.store.clone()] {
            let path = path.trim().to_string();
            if path.is_empty() {
                continue;
            }
            if volume_exposure(&path).is_some() {
                eprintln!(
                    "skein: {} points {path:?} at skein's own volume, so it is not mounted and \
                     that repo's boxes will not see it — a box given it would read the API token, \
                     the GitHub credentials and every other box's state. Move it outside {}.",
                    repo.id,
                    skein_home().display()
                );
                continue;
            }
            if mounts.iter().any(|m| under(&path, m)) {
                continue;
            }
            mounts.push(path);
        }
    }
    mounts
}

/// Would mounting `path` hand a box skein's own volume?
///
/// The volume holds the API token, the GitHub PATs and their token files, the fleet agent's token —
/// which runs commands as the sandbox in *any* box's namespace — the git grants, the tracker
/// connections and every box's declared state. Two directories under it are shared with boxes on
/// purpose: [`fleet_workspace`], because a repo's work clone and store live there, and
/// [`box_state_root`], because a box writes its own conversation. **Everything else is private**,
/// and this asks the allow-list question rather than the deny-list one, so a secret written
/// tomorrow at a path nobody thought to add here is private without anybody adding it.
///
/// Three shapes are refused, and all three are reachable with `skein add`, whose `--store` and
/// local-path arguments are host paths a person types:
///
///   * the volume root itself — `--store ~/.skein`;
///   * anything *containing* it — `--store ~`, or `/`, which mounts the volume as a side effect of
///     mounting its parent, and would not look like a mount of the volume at all;
///   * anything inside it that is not under one of the two shared directories — `--store
///     ~/.skein/github-pats`, which is a mount of exactly the credential files.
///
/// Refusing costs that repo's boxes their store, loudly, at provisioning. Mounting it costs the
/// fleet every credential it has, silently.
///
/// **And `skein add --store` asks this same question, at the moment a person can still act on the
/// answer** (SKEIN-943). It used to accept any absolute path and scaffold a store there, so `--store
/// ~/.skein/thing` succeeded and every box of that repo then came up with no store, looking healthy.
/// Two copies of this rule would drift, which is the defect's whole shape, so there is one and both
/// sides call it; `None` means mountable.
///
/// Asked of the path as typed **and** of where it resolves to — `..` taken through the file system
/// and every symlink followed, as far as the path exists. The textual answer alone let
/// `~/.skein/repos/../github-pats` through as "under repos", and a symlink outside the volume
/// pointing into it through as "not under the volume at all"; either is a mount of the credentials.
pub fn volume_exposure(path: &str) -> Option<VolumeExposure> {
    let home = skein_home().to_string_lossy().into_owned();
    let shared = [fleet_workspace(), box_state_root()];
    let resolved = (|| {
        let shared = [
            resolve_host_path(&shared[0])?,
            resolve_host_path(&shared[1])?,
        ];
        exposure_among(
            &resolve_host_path(path)?,
            &resolve_host_path(&home)?,
            &shared,
        )
    })();
    resolved.or_else(|| exposure_among(path, &home, &shared))
}

/// Which way a path would hand a box the volume — the two shapes a refusal has to word differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeExposure {
    /// The volume root, or a directory holding it: `--store ~` mounts the volume as a side effect.
    Contains,
    /// Inside the volume, and under neither of the two directories boxes are given.
    Inside,
}

/// The rule itself, over paths already in one form (all textual, or all resolved).
fn exposure_among(path: &str, home: &str, shared: &[String]) -> Option<VolumeExposure> {
    // The root, or an ancestor of it.
    if under(home, path) {
        return Some(VolumeExposure::Contains);
    }
    // Inside it, and not one of the two directories boxes are given.
    (under(path, home) && !shared.iter().any(|s| under(path, s))).then_some(VolumeExposure::Inside)
}

/// Where an absolute host path really points: the longest prefix that exists is canonicalised (which
/// takes `..` through symlinks correctly), and the part that does not exist yet — a store `add` is
/// about to create — is appended with `.` and `..` applied to it. `None` for a relative path, which
/// has no one answer and which `ensure_store` refuses anyway.
fn resolve_host_path(path: &str) -> Option<String> {
    let mut existing = std::path::PathBuf::from(path);
    if !existing.is_absolute() {
        return None;
    }
    let mut missing = Vec::new();
    let base = loop {
        if let Ok(real) = std::fs::canonicalize(&existing) {
            break real;
        }
        missing.push(
            existing
                .components()
                .next_back()?
                .as_os_str()
                .to_os_string(),
        );
        if !existing.pop() {
            return None;
        }
    };
    let mut out = base;
    for part in missing.iter().rev() {
        match part.to_str() {
            Some("..") => {
                out.pop();
            }
            Some(".") => {}
            _ => out.push(part),
        }
    }
    Some(out.to_string_lossy().into_owned())
}

/// Is `path` inside `dir` (or `dir` itself)? Textual, because both are host absolute paths skein
/// wrote or normalised, and the answer is needed before any sandbox exists to ask.
pub(super) fn under(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// Ask the host warden to create the fleet sandbox.
///
/// **skein does not run `sbx create` any more**, and that is delivery step 3 rather than a
/// refactor: create terminates skein — it does not exist before the fleet does — so fleet lifecycle
/// cannot live inside the fleet, permanently (§8). Routing it through the warden while skein is
/// still on the host is what exercises both callers before anything moves.
///
/// The confirmation moved with it, and moved to a better place. `sbx create` asks before it mounts
/// host directories, and skein used to answer that itself when it had a terminal and capture it when
/// it did not — which meant the server created fleets with nobody consulted. Now the question is put
/// to a person at the warden, on the host, every time (§8.1).
///
/// **A warden that cannot be reached does not silently fall back to running it here.** That would
/// re-open the path this exists to close, and the fallback would be the one taken on exactly the day
/// something was wrong. It fails with the line to run by hand instead, which is the escape hatch
/// that does not undermine the rule.
fn create_through_warden(sandbox: &str, mounts: &[String]) -> Result<(), String> {
    use crate::warden_client::{perform, Act, Performed};
    // Written before the argv that names it, and fatal here where it is best-effort in
    // [`create_line`]: this path is building a fleet rather than describing one, and a sandbox
    // created against a kit that does not exist is a sandbox that will not serve after its first
    // restart — with nothing at the time of the create to say so.
    ensure_fleet_kit()?;
    // The environment travels as part of the request rather than being set here: it is the warden's
    // process that runs the command, so a fleet configured for a bigger disk would otherwise be
    // recreated at sbx's default 20 GB because the variable stayed behind. It is also shown in the
    // approval — `DOCKER_SANDBOXES_ROOT_SIZE=200g` is most of what that command does.
    match perform(&Act::Create {
        sandbox: sandbox.to_string(),
        argv: create_argv(sandbox, mounts),
        env: create_env(),
    }) {
        // The settle is `disturbing`'s, not this function's: `Remembered::SandboxListing` names
        // the fact, and both of its readers are invalidated together. Doing it here as well would
        // be a second place to keep in step, and the one that gets forgotten is whichever caller
        // is added next.
        Performed::Warden(_) => Ok(()),
        // **Still an `Err`, and the doc on `Performed::Prompt` is about the surface rather than
        // about this.** `ensure_fleet`'s contract is that the sandbox exists when it returns, and
        // every caller — every box start — depends on that; returning `Ok` for a fleet nobody has
        // made yet would be the one lie this function must not tell. What changed is what the
        // sentence *says*: it used to be a dead end with a command bolted on the back, and it is
        // now the three-part prompt — the command, why skein wants it, and what declining costs.
        //
        // Declining being a *supported outcome* lives one layer out, where a person is actually
        // asked (SKEIN-452). Down here there is nobody to ask and nothing yet declined.
        Performed::Prompt(prompt) => Err(prompt.render()),
        // Neither of these is "it did not happen", and a create started over one that may already
        // exist is how two fleets end up sharing a name. Deliberately NOT a prompt: offering the
        // line after an undecided answer offers a *second* create.
        Performed::Uncertain(answered) => Err(format!(
            "creating fleet sandbox {sandbox}: {}",
            answered.detail()
        )),
    }
}

/// What this machine actually has, so a fleet can be sized against it rather than against a number
/// someone typed once.
///
/// Every field is the HOST's — the quantities `sbx create` is about to take a share of, invisible
/// from inside afterwards. Which is exactly why it answers all-zero in-fleet rather than measuring:
/// from in there the same three readings describe the sandbox, and a share of a share is not a
/// proposal, it is a shrink nobody asked for. Reported in MB because that is what the arithmetic
/// below wants; the UI turns them back into GB, which is how the flags are spelled.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HostCapacity {
    pub cpus: u64,
    /// Total RAM. `0` when it could not be read — reported as unknown rather than guessed, because a
    /// proposal derived from a wrong total is worse than no proposal.
    pub memory_mb: u64,
    /// Free space on [`HostCapacity::disk_path`], which is where the sandbox's disk image grows.
    pub disk_free_mb: u64,
    pub disk_total_mb: u64,
    pub disk_path: String,
}

/// This machine, measured.
pub fn host_capacity() -> HostCapacity {
    // **In-fleet these readings are the SANDBOX's, and reporting them as the host's is worse than
    // reporting nothing.** `available_parallelism`, `/proc/meminfo` and `df /` all answer about the
    // machine the process is on, and in-fleet that is the fleet sandbox: measured here at 11 CPUs
    // and 25.8 GiB against a 12-core host. `proposed_fleet_size` would then offer 70% of the
    // fleet's own share as "70% of your machine", and a fleet resized from that proposal shrinks
    // every time somebody accepts it.
    //
    // Zero is not a special case invented for this — it is what every field already means by
    // "could not be read", and [`HostCapacity::memory_mb`] says why that is the right answer:
    // "reported as unknown rather than guessed, because a proposal derived from a wrong total is
    // worse than no proposal". Same rule `bootstrap.sh` applies to an ambiguous `$SKEIN_HOME`.
    //
    // The honest fix for the DIALOG is not here: a host-sized proposal cannot be made from inside,
    // so the cockpit has to ask the person or be told by the host. This function's job is to stop
    // supplying a confident wrong number to it.
    // **Zeros, always** (SKEIN-576). This describes the machine the fleet sandbox is created ON,
    // and skein is inside that sandbox: the numbers it could read here are the sandbox's own, which
    // is the one thing they must not be. A confident wrong number feeding the create's sizing is
    // worse than nothing, and nothing is what a caller checks for.
    HostCapacity {
        cpus: 0,
        memory_mb: 0,
        disk_free_mb: 0,
        disk_total_mb: 0,
        disk_path: String::new(),
    }
}

/// A size for the fleet, proposed from what the host has. Every field is a string in sbx's own
/// spelling, so the dialog shows exactly what will be passed.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FleetSize {
    pub memory: String,
    pub cpus: String,
    pub disk: String,
    pub box_disk_max: String,
}

/// What skein would ask for, given this host — the numbers the confirmation dialog starts on.
///
/// Deliberately a *proposal* and not a default that silently applies. A fleet sandbox is the largest
/// thing skein creates on someone's machine, and until now it was created by a side effect of
/// launching a first box, at whatever `fleet_memory` happened to say — 26g, a number chosen for a
/// different machine. On a 16 GB laptop that is most of the RAM, decided by nobody.
///
/// The shares: memory is 70% of the host, which leaves the host itself working while the fleet is
/// busy; CPUs are all but one, so a saturated fleet still leaves a core to type in; disk is half the
/// free space capped at 60 GB, because the image is sparse and grows into what it is given.
pub fn proposed_fleet_size(host: &HostCapacity) -> FleetSize {
    let gb = |mb: u64| format!("{}g", (mb / 1024).max(1));
    // `configured_field`, not the loaded `Config`: `fleet_memory` reads back this build's 26g on a
    // machine nobody has configured, so deferring to it would defer to a number chosen elsewhere.
    let memory =
        crate::config::configured_field("fleet_memory").unwrap_or_else(|| match host.memory_mb {
            0 => default_fleet_memory_hint(),
            total => gb((total * 7 / 10).max(4096)),
        });
    // From the capacity passed in, not a fresh probe: this function's whole contract is "given this
    // machine", and a proposal that measured a different one would be untestable and, on a host
    // whose CPU count skein was told rather than read, wrong.
    let cpus = crate::config::configured_field("fleet_cpus")
        .unwrap_or_else(|| host.cpus.saturating_sub(1).max(1).to_string());
    let disk =
        crate::config::configured_field("fleet_disk").unwrap_or_else(|| match host.disk_free_mb {
            0 => "20g".to_string(),
            free => gb((free / 2).clamp(20 * 1024, 60 * 1024)),
        });
    FleetSize {
        memory,
        cpus,
        disk,
        box_disk_max: load_config().box_disk_max,
    }
}

/// The memory to propose when the host will not say how much it has. Named rather than inlined so
/// the one place a guess survives is obvious.
fn default_fleet_memory_hint() -> String {
    "8g".to_string()
}

/// Every host CPU but one, so the host stays responsive while the fleet is busy. Empty when the
/// count cannot be read, which leaves the flag off and sbx's own default in charge.
fn host_cpus_less_one() -> String {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1).to_string())
        .unwrap_or_default()
}

/// The host directory the fleet sandbox mounts: the parent of every repo's store.
///
/// sbx mounts a workspace at its **host absolute path** (verified in a real sandbox — the host's
/// `/Users/…/.skein/repos` is that same path inside the guest). That is worth more than it looks:
/// `repo.store` is a host path, and it resolves unchanged inside the fleet sandbox, so nothing in
/// skein has to translate one. Mounting the parent rather than each store is what lets a repo be
/// added later without recreating the sandbox.
pub fn fleet_workspace() -> String {
    skein_home().join("repos").to_string_lossy().into_owned()
}

/// Is the fleet sandbox already there?
///
/// `None` when sbx could not be asked at all — which is not the same as "absent", and must not be,
/// or a wedged daemon would have skein try to create a sandbox that already exists.
///
/// **In-fleet, one sandbox is answerable without asking anything: the one this process is standing
/// in.** `fleet_boxes` returns `None` in here by design (`sbx ls` is a question about the host's
/// machine), and every caller that read that as "cannot tell" then declined to act — `skein doctor`
/// printed its own live sandbox as *"cannot tell if it exists"*, and `volume::migrate`'s refusal
/// stopped firing, so a volume could be moved out from under running boxes.
///
/// Any OTHER name still answers `None` in-fleet, and that is not a hedge: from inside one sandbox
/// there is no way to see another, so "absent" would be a guess and `None` is the truth.
pub fn fleet_exists(sandbox: &str) -> Option<bool> {
    // `Some(true)` for the sandbox this process is inside, and `None` for every other name — which
    // is the honest pair rather than a collapsed tri-state: this skein knows its own fleet exists
    // because it is running in it, and cannot see the machine to answer about any other. Asking
    // about another fleet is what the warden's sighting is for (`create_fleet_operation`).
    (sandbox == fleet_sandbox()).then_some(true)
}

/// **Creating the fleet sandbox, as an Operation with a doer** (§2.4, SKEIN-576).
///
/// The mirror of [`publish_cockpit_port`], and the difference between them is the whole point of
/// [`crate::operation::Doer`] being a field: this one *has* a doer, so a reachable warden performs
/// it and only an unreachable one falls back to a person. `Act::Publish` has no doer at all, so it
/// is always the person. Two operations, one shape, and the difference is data rather than two
/// spellings of the same decision.
///
/// **The check is asked of the warden, not of `sbx`.** In the fleet `sbx ls` cannot answer — it is
/// a question about the *machine*, and this process is not standing on it — so `fleet_exists`
/// returns `None` there for every name. The warden IS on that machine and reports its sandboxes
/// (`Sighting::sandboxes`), which makes "does this fleet exist" answerable from inside for the
/// first time. When no warden answers, the honest state is `unknown`: not "it is absent", which
/// would drive a create over a fleet that may be running.
///
/// That is also why `may_drive` does the right thing here without a special case. No warden means
/// no sighting means `unknown` means no drive — and §2.4's rule that `unknown` may never drive a
/// doer is exactly the rule that stops a blind create.
pub fn create_fleet_operation(sandbox: &str, mounts: &[String]) -> crate::operation::Operation {
    use crate::operation::{Check, Class, Doer, Operation};
    let act = crate::warden_client::Act::Create {
        sandbox: sandbox.to_string(),
        argv: create_argv(sandbox, mounts),
        env: create_env(),
    };
    let recipe = vec![act.command()];
    let check = match crate::warden_client::sighting() {
        Some(seen) => match seen.sandboxes.iter().any(|s| s == sandbox) {
            true => Check::Satisfied(format!("the warden can see {sandbox}")),
            false => Check::Unsatisfied(format!(
                "the warden sees {} and not {sandbox}",
                match seen.sandboxes.is_empty() {
                    true => "no sandboxes".to_string(),
                    false => seen.sandboxes.join(", "),
                }
            )),
        },
        None => Check::Unknown(
            "no warden answered, so nothing here can see whether that sandbox exists — `sbx` is \
             host-only and this skein is not on the host"
                .to_string(),
        ),
    };
    Operation {
        id: crate::warden_client::operation_id("create-fleet", sandbox, &recipe),
        desired: format!("the fleet sandbox {sandbox} exists"),
        check,
        recipe,
        // Creating a fleet that is already there is not a second fleet — the check is what makes it
        // idempotent, and it is asked every time.
        class: Class::Idempotent,
        doer: Some(Doer::Warden),
    }
}

/// **Ask for the fleet sandbox to be created.** The explicit act, reached from the cockpit.
///
/// This used to live inside [`ensure_fleet`], which made creating a fleet a side effect of starting
/// a box. That was the wrong caller from the beginning and in-fleet it was unreachable, because
/// `ensure_fleet` asks about the fleet this process is *inside* — a question that answers itself.
/// Architecture §7.5 puts fleet lifecycle outside the fleet permanently, and that argument is about
/// where the **doer** runs; it never said who may ask. §2.3 already has `http` reaching "GitHub,
/// and the warden", so asking from inside is the design rather than a loophole.
///
/// **Ask the warden; with no warden, refuse and print.** Never `sbx` here. `docs/delivery.md:143`
/// is explicit about why that is not a missing convenience: *"an unreachable warden does not fall
/// back to running `sbx` here, because that fallback would be taken on exactly the day something
/// was wrong."*
///
/// The attempt lease comes with it. Two people pressing the button, or a retry over a create that
/// takes minutes, are the same hazard the lease was written for — and it is a worse hazard here
/// than it was on a box start, because a person pressing a button that appears to do nothing
/// presses it again.
pub fn request_fleet_create(sandbox: &str, mounts: &[String]) -> Result<String, String> {
    let op = create_fleet_operation(sandbox, mounts);
    if let crate::operation::Check::Satisfied(said) = &op.check {
        return Ok(format!(
            "the fleet sandbox {sandbox} is already there — {said}"
        ));
    }
    // `unknown` lands here, which is the point: no warden answered, so nothing may act, and what a
    // person gets is the line to run themselves.
    if !op.may_drive() {
        return Err(format!(
            "skein cannot create {sandbox} from here and will not guess.\n{}",
            op.render()
        ));
    }
    let outcome = crate::attempt::attempt(
        &skein_home().join("attempts"),
        &format!("create-{sandbox}"),
        // Longer than the create's own 900s budget: the lease bounds how long a DEAD holder blocks
        // the work, and reclaiming while a create is still running is precisely the second copy
        // this prevents.
        Duration::from_secs(1800),
        || {
            // A sandbox that did not exist a moment ago makes four remembered answers wrong at
            // once: it is not in the listing, no sweep has seen its boxes, no `du` has walked its
            // disk, and its memory and CPU totals are the previous sandbox's or nobody's.
            disturbing(
                &[
                    Remembered::SandboxListing,
                    Remembered::BoxLiveness,
                    Remembered::BoxDisk,
                    Remembered::FleetResources,
                ],
                || create_through_warden(sandbox, mounts),
            )
        },
    )?;
    if let crate::attempt::Outcome::InFlight(theirs) = outcome {
        return Err(format!(
            "the fleet sandbox {sandbox} is already being created — that started {} ago and takes \
             a few minutes. Wait for it rather than starting a second one; if it never finishes, \
             it is given up on automatically.",
            theirs.age()
        ));
    }
    // A fresh sandbox is serving on a port nothing outside it can reach yet, and skein cannot
    // publish that mapping — `Act::Publish` has no doer by §9.4. What it can do is say so once,
    // with the line to run, and only when the doorway actually holds the port.
    let mut said = format!("the fleet sandbox {sandbox} was created");
    match cockpit_port_advice(sandbox) {
        Ok(publish) => said.push_str(&format!(
            ".\nReaching its cockpit from your machine is one command, and it is yours to run:\n{}",
            publish.render()
        )),
        Err(why) => said.push_str(&format!(".\n{why}")),
    }
    Ok(said)
}

/// Create the fleet sandbox if it is missing, open the cockpit's door in it, then install the
/// launcher.
///
/// Idempotent: safe to call before every launch, which is how a sandbox the user removed by hand
/// comes back rather than leaving every box unstartable.
///
/// The door is here rather than beside the server because this is the only function that
/// runs before a box can exist — see [`ensure_fleet_door`], and §9.4's squat, which is a race
/// against the *first* box and not against the server.
pub fn ensure_fleet(sandbox: &str) -> Result<(), String> {
    if !valid_name(sandbox) {
        return Err("invalid fleet sandbox name".into());
    }
    // **The sandbox exists because this process is inside it**, so the check below is an identity
    // check rather than a question about the world. It used to be a question — `fleet_exists` read
    // `sbx ls`, which `sbx.rs` correctly refuses ("a question about the *machine*, and skein is not
    // standing on it") — and every box start failed on a question whose answer was the reason the
    // question was asked.
    //
    // Everything after it is still done, and must be: the substrate, the fleet root, the door, the
    // launcher.
    //
    // **Nothing here creates a fleet** (SKEIN-576). It used to, under an attempt lease, and that
    // was the wrong caller from the beginning: this function asks about *this* fleet, which is
    // trivially satisfied because the process is inside it — so the branch was not merely
    // unreachable, it was answering a question nobody had asked. Creating a fleet is an explicit
    // act by a person ([`request_fleet_create`], reached from the cockpit), never a side effect of
    // starting a box.
    //
    // Two arms, not three. [`fleet_exists`] answers `Some(true)` for the sandbox this process is
    // standing in and `None` for every other name; there is no `Some(false)`, because calling a
    // sandbox on a machine skein cannot see "absent" would be a guess dressed as an answer. So the
    // refusal below says both things a reader needs — that this is not the fleet running here, and
    // that if it does not exist somewhere else, making one is somebody's deliberate act.
    if fleet_exists(sandbox) != Some(true) {
        return Err(format!(
            "{sandbox} is not the fleet sandbox this skein is running inside, and from in here \
             there is no way to see another. If it does not exist, creating one is a deliberate \
             act and not something starting a box does for you — ask for it from the cockpit's \
             fleet pane, which puts the request to the warden and shows you the line to run if no \
             warden answers."
        ));
    }
    ensure_substrate(sandbox)?;
    ensure_fleet_root(sandbox)?;
    // Before the launcher is installed, which is the earliest a box in this sandbox could exist —
    // and that ordering is the item (§9.4's squat). The cockpit's port has to be held from the
    // moment the sandbox does, not from the moment a person publishes its port: the mapping
    // a serve publishes outlives skein, so a box that took the port first *is* the cockpit, and
    // the browser hands it the fleet token on its first request.
    //
    // Reported rather than fatal, and the reason is measured rather than chosen: the doorway needs
    // python3, and `box-session.sh` says out loud that a box without python3 still starts (it
    // loses shared logins). Refusing every launch on a fleet whose image has no python would be a
    // bigger outage than the exposure, which needs a *published* mapping before it is reachable at
    // all — and the mapping is now a person's act, taken with the door already open (§9.4).
    if let Err(e) = ensure_fleet_door(sandbox) {
        eprintln!(
            "skein: the cockpit's door is not open in {sandbox} ({e}); a box in this fleet can \
             bind :{} before skein does, which is architecture §9.4's squat",
            server_sandbox_port()
        );
    }
    // After the substrate (which may have just installed the runtimes) and before any box starts,
    // so a rebuilt sandbox has its login back before the first box seeds from it.
    sync_fleet_login(sandbox);
    ensure_known_hosts(sandbox);
    // Here as well as in `heal_fleet`, because a sandbox this call has just *created* would
    // otherwise run its whole first life with containers outside the ceiling: the server that made
    // it is already running, so the next restart is the earliest healing would reach it. Reported
    // rather than fatal for the same reason as there — it costs the merged pool its enforcement,
    // not the fleet its boxes.
    if let Err(e) = install_docker_config(sandbox) {
        eprintln!(
            "skein: could not point dockerd at the workload cgroup ({e}); containers in {sandbox} \
             stay outside the ceiling"
        );
    }
    // Best-effort and unreported, like every other repair here: a sandbox that will not delete a
    // file has already failed something louder. This is the sweep that used to ride on the agent
    // install (`stale_sandbox_secrets`); it runs here now, which is where a tidy-up of a previous
    // build belongs.
    for stale in stale_sandbox_secrets() {
        let _ = own_sandbox(sandbox).exec(
            &forget_credential_script(&sh_quote(&stale)),
            Duration::from_secs(30),
        );
    }
    install_launcher(sandbox)
}

/// Create the fleet root and hand it to the sandbox user.
///
/// [`fleet_root`] defaults to `/boxes` — at the filesystem root, where a non-root user cannot mkdir.
/// Everything after this point (the launcher, every box root, every tmux socket) is created with a
/// plain `mkdir -p` by the sandbox user, so all of it fails until this runs once. The integration
/// test never caught it precisely because `$SKEIN_FLEET_ROOT` points it at a writable temp dir —
/// the seam that makes the launch path testable is also the seam that hid its first real step.
pub fn ensure_fleet_root(sandbox: &str) -> Result<(), String> {
    let root = sh_quote(&fleet_root());
    // Escalate only when there is something to escalate for. `/boxes` sits at the filesystem root
    // where an unprivileged mkdir cannot reach, so in production this still falls through to sudo
    // exactly as before — but a fleet root anywhere writable is now made without it.
    //
    // That is not a tidiness argument, it is a testability one. `$SKEIN_FLEET_ROOT` points the
    // integration test at a temp dir specifically so the launch path can be exercised without a
    // sandbox, and reaching for sudo regardless made the whole test unrunnable anywhere sudo is not
    // available — which now includes every box, since a box is a user namespace and sudo cannot work
    // in one. The test that guards `box-session.sh` was therefore red exactly where that file is
    // edited. `mkdir -p` on an existing directory succeeds, so the second `-w` is what keeps an
    // unwritable-but-present root falling through rather than being called done.
    let script = format!(
        "[ -w {root} ] && exit 0; \
         mkdir -p {root} 2>/dev/null && [ -w {root} ] && exit 0; \
         sudo mkdir -p {root} && sudo chown \"$(id -u):$(id -g)\" {root} && chmod 755 {root}"
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(60))
        .map(|_| ())
        .map_err(|e| format!("preparing the fleet root {root}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::testkit::*;
    use crate::testutil::*;

    /// With no warden reachable, making a fleet says what to type — not just that it failed.
    ///
    /// The dead end this replaced is quoted in SKEIN-312: `warden_client`'s "start it with
    /// `skein-warden`" was the whole of what a person got, and running a warden means trusting
    /// skein with a privileged executable on your host, which not everyone will do. So the absence
    /// of a warden has to be an ordinary path, and an ordinary path has to say what to do next.
    ///
    /// Asserted on the THREE PARTS rather than on the text, because the third is the one that gets
    /// dropped: a command with no stated cost of declining is not a choice, it is an instruction.
    #[test]
    fn making_a_fleet_with_no_warden_says_what_to_type_and_what_declining_costs() {
        let _env = env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        // A port nothing listens on: the "no warden at all" case, which is the default install.
        std::env::set_var("SKEIN_WARDEN", "127.0.0.1:1");
        // **A fixture, not `/boxes`.** `/boxes` is `fleet_root()`'s DEFAULT, so on any machine
        // running skein it is a live fleet, and a test that pins it there is aimed at real
        // infrastructure to make a string deterministic. Nothing this test asserts carries the
        // root: the argv's only path is `--kit`, which `create_argv` takes from `fleet_kit_dir()`
        // — `skein_home().join("fleet-kit")`, under the pinned `$SKEIN_HOME` — and `create_env()`
        // reads the configured disk and nothing else. It was pinned here before `fleet_root()`
        // had a refusal of its own, because the wrong value would otherwise have been silent
        // (SKEIN-644); the refusal exists now (SKEIN-690) and this pin is what it asks for.
        std::env::set_var("SKEIN_FLEET_ROOT", skein_home.join("fleet"));

        let why = create_through_warden("skein-fleet", &["/tmp/x".to_string()])
            .expect_err("there is no warden on port 1, so this cannot have been performed");

        // The command a person would actually type, spelled as `sbx` takes it. Asserted against
        // the REAL `create_argv` and not a fixture, which is how this caught `Act::command`
        // prepending a verb to an argv that already had one — `sbx create skein-fleet create
        // --name skein-fleet …`, a line that fails if typed.
        assert!(
            why.contains("sbx 'create' '--name' 'skein-fleet'"),
            "the refusal does not name a runnable command:\n{why}"
        );
        assert!(
            !why.contains("'skein-fleet' 'create'"),
            "the command names the verb twice, so typing it fails — the prompt's whole job is a \
             line that runs:\n{why}"
        );
        assert!(
            why.contains("Why:"),
            "the refusal does not say what skein wants it FOR:\n{why}"
        );
        assert!(
            why.contains("If you don't:"),
            "the refusal does not say what declining costs — which is the part that turns an \
             instruction back into a choice, and the part that gets dropped:\n{why}"
        );
        std::env::remove_var("SKEIN_WARDEN");
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The create line skein hands a person names the volume, and names every repo that lives
    /// outside it.
    ///
    /// SKEIN-462: mounts are fixed at create and no verb adds one afterwards (`sbx --help`) — so a
    /// line that leaves a path out cannot be
    /// repaired, and the box it breaks comes up looking healthy with no store. The two ways to get
    /// this wrong are both covered here: printing [`fleet_mounts`], which omits the volume root, or
    /// printing only the volume, which omits a store kept outside it.
    ///
    /// Asserted through `Act::Create::command` — the renderer the warden prompt uses — because the
    /// point is the text a person pastes, not the vector behind it.
    #[test]
    fn the_create_line_names_the_volume_and_every_repo_outside_it() {
        let _env = env_lock();
        let home = crate::testutil::tempdir();
        // A real sibling path, with no `..` in it: `under` compares strings, so a path spelled
        // through the volume's own directory reads as being inside it and is dropped from the
        // mount set. That is a fixture trap rather than the thing under test.
        let elsewhere = crate::testutil::tempdir();
        let elsewhere = elsewhere.join("adopted-in-place");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). Both paths
        // asserted below are the volume and a store kept beside it, so the root is not the
        // subject — but `fleet_mounts` reads it, and an unset one put the live fleet in the mount
        // set of a line this test then read.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        std::fs::write(
            home.join("repos.json"),
            format!(
                r#"[{{"id":"adopted","source":"{p}","source_tree":"{p}","store":"{p}/.claude"}}]"#,
                p = elsewhere.display()
            ),
        )
        .unwrap();

        assert_eq!(
            crate::repos::load_repos().len(),
            1,
            "the fixture repo did not load, so this would assert nothing about mounts"
        );
        let line = create_line("skein-fleet").expect("the create line");
        std::env::remove_var("SKEIN_HOME");

        // As its OWN argument, quoted, and not merely as a substring. A first draft of this
        // asserted `line.contains(&volume)` and could not fail: `fleet_mounts` yields
        // `<volume>/repos`, which contains the volume's path, so the wrong mount set passed it.
        // The sabotage found that, not the author.
        let arg = |p: &str| format!("'{p}'");
        let volume = home.to_string_lossy().to_string();
        assert!(
            line.contains(&arg(&volume)),
            "the create line does not mount the volume {volume} itself — only paths beneath it — \
             so skein's own server could not read its token or the box state:\n{line}"
        );
        let store = elsewhere.join(".claude").to_string_lossy().to_string();
        assert!(
            line.contains(&arg(&store)),
            "the create line does not name {store}, a store kept outside the volume, so its \
             boxes come up with no store — and mounts cannot be added after a create:\n{line}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// In-fleet, "does the fleet sandbox exist" is answered by standing in it, not by asking sbx.
    ///
    /// `fleet_exists` reads `sbx ls`, which `sbx.rs` correctly refuses in-fleet — "a question about
    /// the *machine*, and in-fleet skein is not standing on it" — by returning `None`. That `None`
    /// propagated, and [`ensure_fleet`] turned it into `cannot tell whether the fleet sandbox
    /// exists`, so **every box start in-fleet failed on a question whose answer is the reason it was
    /// asked**.
    ///
    /// Asserted as an absence, which is the only shape available without a sandbox: what the later
    /// steps make of a machine that is not one is not this test's question. Only the message is.
    #[test]
    fn in_fleet_does_not_ask_sbx_whether_the_sandbox_it_is_inside_exists() {
        let _g = env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // The seam, and it is not optional here: in-fleet every step below now runs LOCALLY, so a
        // fleet root left at `/boxes` has this test sudo one into existence on whatever machine it
        // runs on. It did, once, before this line.
        std::env::set_var("SKEIN_FLEET_ROOT", root.as_ref() as &std::path::Path);
        // An `sbx ls` that answers nothing, so a build that still consulted it cannot pass by luck.
        std::env::set_var("SKEIN_LS_CMD", "exit 1");

        let said = match ensure_fleet("skein-fleet") {
            Ok(()) => String::new(),
            Err(why) => why,
        };

        std::env::remove_var("SKEIN_LS_CMD");
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");

        assert!(
            !said.contains("cannot tell whether the fleet sandbox exists"),
            "in-fleet skein asked sbx whether the sandbox it is running inside exists, and could \
             not be told — which is every box start in this deployment: {said}"
        );
    }

    /// The fleet is sized against the machine it is going onto.
    ///
    /// It used to be sized by `fleet_memory`, whose default is a number chosen for the machine this
    /// was written on. On a 16 GB laptop that default is most of the RAM, applied by a first box
    /// launch, to a sandbox whose memory cannot be changed afterwards without rebuilding it.
    #[test]
    fn a_fleet_is_proposed_from_what_the_machine_actually_has() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let host = HostCapacity {
            cpus: 8,
            memory_mb: 16 * 1024,
            disk_free_mb: 100 * 1024,
            disk_total_mb: 500 * 1024,
            disk_path: "/".into(),
        };
        let want = proposed_fleet_size(&host);
        // 70% of 16 GB, leaving the host itself able to work while the fleet is busy.
        assert_eq!(want.memory, "11g", "{want:?}");
        // All but one: a saturated fleet still leaves a core to type in.
        assert_eq!(want.cpus, "7", "{want:?}");
        // Half the free space, and the cap: sparse or not, 300 GB of headroom is not a proposal.
        assert_eq!(want.disk, "50g", "{want:?}");

        // A machine that will not say how much memory it has gets a modest number rather than a
        // number derived from zero — which is what "70% of unknown" would be.
        let blind = HostCapacity {
            memory_mb: 0,
            disk_free_mb: 0,
            ..host.clone()
        };
        let want = proposed_fleet_size(&blind);
        assert_eq!(want.memory, default_fleet_memory_hint());
        assert_eq!(want.disk, "20g", "sbx's own default, not a guess: {want:?}");

        // What is already configured wins over any proposal: this dialog also opens on a fleet that
        // has been sized before, and overwriting that with an arithmetic default would silently undo
        // a decision someone made.
        let mut config = load_config();
        config.fleet_memory = "26g".into();
        config.fleet_cpus = "3".into();
        save_config(&config).unwrap();
        let want = proposed_fleet_size(&host);
        assert_eq!((want.memory.as_str(), want.cpus.as_str()), ("26g", "3"));

        std::env::remove_var("SKEIN_HOME");
    }

    /// **The create is asked of the warden, and nothing here ever runs `sbx`** — in either
    /// deployment, and whether or not a warden answers (SKEIN-576).
    ///
    /// This is the failure mode the whole design guards, so it is asserted on the transcript and
    /// not on the outcome. A test that checked "the fleet ends up created" passes under exactly the
    /// silent fallback `docs/delivery.md:143` rules out: *"an unreachable warden does not fall back
    /// to running `sbx` here, because that fallback would be taken on exactly the day something was
    /// wrong."* The day something is wrong is the day the fallback runs a privileged command with
    /// nobody consulted.
    ///
    /// Both halves are here because they fail differently. With a warden, the risk is that skein
    /// asks AND does it itself. Without one, the risk is that skein quietly does it itself.
    ///
    /// **What makes this fail**: calling `sbx` anywhere on this path; or driving the create when
    /// the check came back `unknown`, which is what no-warden produces — `sbx ls` cannot answer
    /// from inside the fleet, so an absent warden means nothing can see whether the sandbox is
    /// there, and a create on that is a create over a fleet that may be running.
    #[test]
    fn creating_a_fleet_is_asked_of_the_warden_and_never_run_here() {
        use std::io::{Read, Write};
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home.join("fleet"));
        // **In-fleet, which is the deployment this is about and the one where the assertion below
        // is exactly true.** Host-side there IS an `sbx` on this path and it is not a fallback: the
        // door check after a create reads the doorway's stamp through `sbx exec`, which is a read
        // and not a privileged mutation. Asserting "no `sbx` at all" there would be asserting
        // something false; asserting "no `sbx create`" would pass while a create ran under another
        // spelling. In the fleet there is no `sbx` to run at all, so the strong form is the honest
        // one — and it is the deployment the warden exists for.
        // Which fleet this skein is standing in: `Place` refuses to address any other sandbox from
        // inside, and the door check below addresses this one by name.
        std::fs::write(
            home.join("config.json"),
            "{\n  \"fleet_sandbox\": \"skein-fleet\"\n}\n",
        )
        .unwrap();

        // An `sbx` that records every call and succeeds at everything. If any code on this path
        // decides to "just run it", the log says so.
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("sbx.log");
        std::fs::write(
            bin.join("sbx"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(bin.join("sbx"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        env.set("PATH", format!("{}:{path}", bin.display()));

        // ---- 1. A warden that answers ----
        //
        // Two requests, in order: `GET /v1/fleet` for the check, then `POST /v1/create`. Answering
        // the listing with no sandboxes is what makes the check `unsatisfied` rather than
        // `satisfied`, which is the only state that may drive a doer.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let heard = seen.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut sock, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = match req.starts_with("GET /v1/fleet") {
                    true => "{\"sandboxes\":[],\"capabilities\":[\"create\"]}".to_string(),
                    false => "{\"state\":\"ran\",\"said\":\"created\"}".to_string(),
                };
                heard.lock().unwrap().push(req);
                let _ = sock.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        env.set("SKEIN_WARDEN", format!("127.0.0.1:{port}"));

        let said = request_fleet_create("skein-fleet", &[]);
        let _ = server.join();
        let asked = seen.lock().unwrap().clone();
        assert!(
            asked.iter().any(|r| r.starts_with("GET /v1/fleet")),
            "the warden was never asked what it can see, so the check was not made: {asked:?}"
        );
        assert!(
            asked.iter().any(|r| r.starts_with("POST /v1/create")),
            "the create was never requested of the warden: {asked:?}"
        );
        assert!(
            said.is_ok(),
            "a warden that said it ran was not believed: {said:?}"
        );
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            ran.trim().is_empty(),
            "skein ran `sbx` itself while a warden was answering:\n{ran}"
        );

        // ---- 2. No warden at all ----
        //
        // A port nothing is on. The check cannot be made, so nothing may be driven, and what comes
        // back is the refusal with the line in it.
        let closed = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let dead = closed.local_addr().unwrap().port();
        drop(closed);
        env.set("SKEIN_WARDEN", format!("127.0.0.1:{dead}"));

        let why = request_fleet_create("skein-fleet", &[])
            .expect_err("a create went ahead with no warden to perform it");
        assert!(
            why.contains("sbx 'create'") || why.contains("sbx create"),
            "the refusal did not carry the line to run by hand:\n{why}"
        );
        assert!(
            why.contains("unknown"),
            "the refusal did not say the check could not be made, which is WHY it refused:\n{why}"
        );
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            ran.trim().is_empty(),
            "skein fell back to running `sbx` when the warden was unreachable — the exact \
             fallback docs/delivery.md rules out:\n{ran}"
        );
    }

    #[test]
    fn a_repos_store_is_reachable_at_the_same_path_inside_the_fleet_sandbox() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let workspace = fleet_workspace();
        let store = home.join("repos/web/store/.claude");
        assert!(
            store.starts_with(&workspace),
            "{} must sit under the mounted workspace {workspace}",
            store.display()
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // The sandbox is agentless and mounts the store parent, not any one repo — the two properties
    // that let it host boxes from every repo without being recreated when one is added.
    #[test]
    fn the_fleet_sandbox_is_agentless_and_mounts_the_store_parent() {
        let _g = env_lock();
        std::env::set_var("SKEIN_HOME", tempdir());
        let argv = create_argv("skein-fleet", &["/h/.skein/repos".to_string()]);
        assert_eq!(&argv[..3], ["create", "--name", "skein-fleet"]);
        assert_eq!(
            &argv[argv.len() - 2..],
            ["shell", "/h/.skein/repos"],
            "agentless, and the workspace is the store parent"
        );
        // Both are ceilings the boxes SHARE rather than one reservation each, which is what makes
        // them safe to set generously — and why a default is better here than deferring to sbx's.
        let flags = argv.join(" ");
        assert!(flags.contains("-m 26g"), "{flags}");
        assert!(
            flags.contains("--cpus "),
            "CPUs default to every host core but one, so the host keeps answering: {flags}"
        );
        assert!(
            !argv.contains(&"--clone".to_string()),
            "the sandbox is not a checkout; boxes clone from the remote onto VM-local disk"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // A repo skein manages keeps its mirror and its store under the one mounted workspace, and that
    // is the case the design was built around. A repo pointed at a store the user already had —
    // `--store`, which still takes any path — keeps that store anywhere on the host instead.
    // Missing that mount does not fail loudly: the clone succeeds, the session starts, and the box
    // comes up with no store to link, no hooks, and no probe, looking entirely healthy.
    #[test]
    fn a_repo_that_lives_outside_the_workspace_is_still_mounted() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let managed = home.join("repos/web");
        let mounts = {
            let workspace = fleet_workspace();
            // Managed: both paths are already covered by the workspace, so neither is mounted twice.
            assert!(under(
                &managed.join("store/.claude").to_string_lossy(),
                &workspace
            ));
            assert!(under(&managed.join("work").to_string_lossy(), &workspace));
            // A store the user already had: outside it, so it must be named explicitly.
            assert!(!under("/Users/y/dev/skein-shared/.claude", &workspace));
            vec![workspace]
        };
        assert_eq!(mounts.len(), 1, "the workspace is always mounted first");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The fleet's disk is one shared filesystem, and sbx takes its size from the environment rather
    /// than from `sbx create`'s argv — so a knob that only reached the argv would set nothing at all.
    #[test]
    fn the_fleets_disk_size_travels_in_the_environment_not_the_argv() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        save_config(&Config::default()).unwrap();
        assert!(
            create_env().is_empty(),
            "unset must leave sbx on its own default rather than pinning one skein invented"
        );

        save_config(&Config {
            fleet_disk: "60g".into(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            create_env(),
            vec![("DOCKER_SANDBOXES_ROOT_SIZE".to_string(), "60g".to_string())]
        );
        let argv = create_argv("skein-fleet", &[]);
        assert!(
            !argv.iter().any(|a| a.contains("60g")),
            "sbx create has no disk flag; putting one in the argv would be rejected: {argv:?}"
        );
        // Restored, or the next test to take `env_lock` inherits a SKEIN_HOME naming a
        // directory this test's guard has already removed — and writes through it, which
        // recreates the tree as a leak nobody owns.
        std::env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn one_mount_covers_everything_beneath_it() {
        assert!(under("/a/b", "/a"), "a child is covered");
        assert!(under("/a", "/a"), "the directory itself is covered");
        assert!(
            under("/a/b", "/a/"),
            "a trailing slash is not a different path"
        );
        assert!(
            !under("/ab", "/a"),
            "a prefix is not a parent — /ab would go unmounted while looking covered"
        );
    }

    /// Everything under a repo fixture, as one registered repo whose work and store sit outside the
    /// volume — the ordinary shape, and the one the property below is measured against.
    fn repo_at(id: &str, work: &str, store: &str) -> crate::repos::Repo {
        crate::repos::Repo {
            read_prs: false,
            id: id.into(),
            source: work.into(),
            store: store.into(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }
    }

    /// The volume is private except for the two directories a box is deliberately given.
    ///
    /// This is what keeps the API token, the GitHub PATs, the git grants, the tracker connections
    /// and every box's declared state out of every box: `~/.skein/repos` and
    /// `~/.skein/boxes` are mounted, and `~/.skein` itself is not. Nothing said so, and the change
    /// that broke it would have read as a simplification — one mount instead of two.
    ///
    /// Stated as an allow-list over a **walk of the whole volume**, rather than as a list of secret
    /// paths. A deny-list is only as current as the last person to remember it: the file below is
    /// deliberately not enumerated, so a credential added tomorrow at a path nobody updated here is
    /// covered on the day it is written.
    #[test]
    fn the_volume_is_private_except_the_two_directories_a_box_is_given() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        // Real writers, so the paths are the ones production actually uses. The fleet agent's
        // token used to be one of them and is gone with the agent (SKEIN-521) — which weakens
        // nothing here, because the assertion is an allow-list over a WALK rather than a list of
        // secrets, so it covers whatever is on the volume on the day it runs.
        crate::apiauth::token().unwrap();
        crate::gitgate::set_write_credential("mine", "my token", &["owner/repo".into()]).unwrap();
        crate::gitgate::set_credential_token("mine", "ghp_secret").unwrap();
        crate::tracking::upsert_connection(
            Some("plane"),
            "Plane",
            "https://plane.example.com",
            Some("plane_secret"),
        )
        .unwrap();
        set_box_privileged("web-main", true).unwrap();
        crate::place::record_place("web-main", &crate::place::PlaceRecord::default()).unwrap();
        crate::config::save_config(&crate::config::Config::default()).unwrap();
        // The warden's pairing secret, at the home both ends derive from the volume root
        // (`warden_client::secret`, `warden/src/lib.rs` `home()`) — written by hand because its
        // real writer is the warden binary, a crate this one deliberately does not depend on.
        // §9.5 R5 is "a secret under the cover", and this walk is the cover's proof; deriving the
        // home from `skein_home()` is what makes the secret land inside it for every volume
        // location. `$SKEIN_WARDEN_HOME` can still point it elsewhere, and that is the operator
        // explicitly leaving the covered world — not a case this test speaks for.
        std::fs::create_dir_all(skein_home().join("warden")).unwrap();
        std::fs::write(skein_home().join("warden/secret"), "0123456789abcdef").unwrap();

        // One repo of each shape: managed (under the volume) and a store kept outside it.
        let outside = tempdir();
        std::fs::create_dir_all(outside.join("work")).unwrap();
        std::fs::create_dir_all(outside.join("store")).unwrap();
        let managed = skein_home().join("repos").join("managed");
        std::fs::create_dir_all(managed.join("work")).unwrap();
        std::fs::create_dir_all(managed.join("store/.claude")).unwrap();
        crate::repos::save_repos(&[
            repo_at(
                "adopted",
                &outside.join("work").to_string_lossy(),
                &outside.join("store").to_string_lossy(),
            ),
            repo_at(
                "managed",
                &managed.join("work").to_string_lossy(),
                &managed.join("store/.claude").to_string_lossy(),
            ),
        ])
        .unwrap();
        // And a box's own state, which IS shared with it.
        std::fs::create_dir_all(box_state("web-main")).unwrap();
        std::fs::write(box_state("web-main") + "/conversation.jsonl", "{}").unwrap();

        let mounts = fleet_mounts();
        let shared = [fleet_workspace(), box_state_root()];

        // No mount is the volume root, or holds it.
        let volume = skein_home();
        for mount in &mounts {
            assert!(
                !volume.starts_with(mount),
                "{mount} is mounted into the sandbox and contains the volume {}, so every box has \
                 the API token, the GitHub credentials and every other box's state",
                volume.display()
            );
        }

        // And nothing else under the volume is reachable through any mount.
        let mut seen = Vec::new();
        walk(&volume, &mut seen);
        assert!(
            seen.len() > 10,
            "the walk found {} paths, so this test proved nothing",
            seen.len()
        );
        for path in &seen {
            let Some(mount) = mounts.iter().find(|m| path.starts_with(m)) else {
                continue;
            };
            assert!(
                shared.iter().any(|s| path.starts_with(s)),
                "{} is reachable from a box through the mount {mount}, and it is not under {} or \
                 {} — the two directories boxes are meant to see",
                path.display(),
                shared[0],
                shared[1]
            );
        }
    }

    /// A repo pointed at the volume does not mount the volume.
    ///
    /// `skein add <path>` and `--store <path>` take host paths a person types, and three of them
    /// hand a box everything: the volume itself, a parent of it, and a directory of credentials
    /// inside it. Refusing costs that repo's boxes their store, at provisioning, out loud. Mounting
    /// it costs the fleet every credential it holds, silently — so the refusal is not a judgement
    /// about how likely the typo is.
    #[test]
    fn a_repo_pointed_at_the_volume_is_not_mounted() {
        let _g = env_lock();
        let parent = tempdir();
        let home = parent.join("volume");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SKEIN_HOME", &home);

        let hostile = [
            skein_home().to_string_lossy().into_owned(),
            skein_home()
                .join("github-pats")
                .to_string_lossy()
                .into_owned(),
            skein_home().join("declared").to_string_lossy().into_owned(),
            parent.to_string_lossy().into_owned(),
            "/".to_string(),
            // Outside the volume by its spelling, and inside it by a symlink (SKEIN-943).
            {
                std::os::unix::fs::symlink(&home, parent.join("link")).unwrap();
                parent
                    .join("link/github-pats")
                    .to_string_lossy()
                    .into_owned()
            },
        ];
        crate::repos::save_repos(
            &hostile
                .iter()
                .enumerate()
                .map(|(i, p)| repo_at(&format!("r{i}"), p, p))
                .collect::<Vec<_>>(),
        )
        .unwrap();

        assert_eq!(
            fleet_mounts(),
            vec![fleet_workspace(), box_state_root()],
            "a repo pointed at skein's own volume was mounted into every box"
        );
    }

    /// A repo's checkout is not in the sandbox at all — only its store, and its mirror under the
    /// workspace.
    ///
    /// The mount was there for two jobs and has neither left: a box cloned from the checkout (it
    /// clones from the mirror now) and read the gitignored files `shared-paths.txt` names (the box's
    /// own bootstrap surfaces those out of the store). Leaving it would have left every box able to
    /// read the working tree its user is typing in, for nothing.
    #[test]
    fn a_repo_puts_its_store_in_the_sandbox_and_not_its_checkout() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        // A legacy record from when a path could still be registered: `source` is a checkout and the
        // store is elsewhere, so neither is covered by the workspace and each is decided on its own.
        let elsewhere = tempdir();
        let work = elsewhere.join("code/thing");
        let store = elsewhere.join("shared/.claude");
        for d in [&work, &store] {
            std::fs::create_dir_all(d).unwrap();
        }
        crate::repos::save_repos(&[crate::repos::Repo {
            read_prs: false,
            id: "thing".into(),
            source: work.to_string_lossy().into_owned(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }])
        .unwrap();

        let mounts = fleet_mounts();
        assert!(
            mounts.iter().any(|m| under(&store.to_string_lossy(), m)),
            "a box cannot reach its own repo's store: {mounts:?}"
        );
        assert!(
            !mounts.iter().any(|m| under(&work.to_string_lossy(), m)),
            "the tree its user works in is in the sandbox: {mounts:?}"
        );
        // And the mirror, which is what a box actually needs, is reachable — it lives under the
        // workspace, so this holds without anybody adding a mount for it.
        let mirror = crate::repos::mirror_path("thing");
        assert!(
            mounts.iter().any(|m| under(&mirror.to_string_lossy(), m)),
            "a box cannot reach the mirror it clones from: {mounts:?}"
        );
    }

    /// A fleet skein cannot see is not a fleet it creates — and starting a box never creates one.
    ///
    /// This used to be about a tri-state. `fleet_exists` answered `Option<bool>` off `sbx ls`, and
    /// a binary check made "the daemon is wedged" and "the fleet is absent" indistinguishable — an
    /// ambiguity the reconciler answered by creating a fleet that already existed, over a sandbox
    /// with every box's work on it. The assertion was that the `None` arm stayed a refusal.
    ///
    /// There is no `sbx ls` to be wedged now (SKEIN-576): `fleet_exists` is an identity check
    /// against the sandbox this process is standing in, so `Some(false)` cannot arise and there is
    /// no third way of saying no. **What survives is the property the tri-state was protecting**,
    /// and it survives in a stronger form: `ensure_fleet` creates nothing, for any name, ever.
    ///
    /// Observed through the execution seam rather than a fake `sbx` on `$PATH`. That fake was the
    /// oracle for "what crossed the process boundary", and with no hop to intercept it would be
    /// bypassed — the commands would run here, for real (SKEIN-592). The seam records every
    /// fleet-scope argv and substitutes a no-op, so "not one create" is read off what skein
    /// actually tried to run.
    ///
    /// **What would make this fail**: putting a create back in this path — under a lease, behind a
    /// retry, anywhere. The refusal would stop being a refusal and `spawned` would name it.
    #[test]
    fn a_fleet_skein_cannot_see_is_not_a_fleet_it_creates() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // A fixture fleet root, and here it is not only about the guard: `ensure_fleet` provisions,
        // and the second half below drives it for real through the execution seam. Left unset this
        // reached for `/boxes` — SKEIN-530's class exactly, and the reason `util::fleet_root` now
        // refuses an unpinned test (SKEIN-690).
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();

        let spawned = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = spawned.clone();
        let _stood_in = crate::place::seam::install(Box::new(move |argv: &[String]| {
            seen.lock().unwrap().push(argv.join(" "));
            Some(vec!["true".to_string()])
        }));

        // A sandbox that is not the one this process is standing in: unseeable from in here, so it
        // is refused rather than made.
        let why = ensure_fleet("some-other-fleet").unwrap_err();
        assert!(
            why.contains("not the fleet sandbox this skein is running inside"),
            "a name skein cannot see was answered as though it could: {why}"
        );
        assert!(
            why.contains("deliberate act"),
            "the refusal does not say who creates a fleet: {why}"
        );

        // **The refusal spent nothing.** Asserted as an empty log rather than as the absence of a
        // "create" word: skein's provisioning scripts talk about creating things in their own
        // prose, so a substring search over what was sent matches sentences rather than commands.
        // Nothing ran at all is the property anyway — an unanswerable check must not reach for the
        // machine before it declines.
        assert!(
            spawned.lock().unwrap().is_empty(),
            "a fleet skein cannot see was reached for before it was refused: {:?}",
            spawned.lock().unwrap()
        );

        // And provisioning its own fleet, which does exist, reaches for no `sbx` — the only tool
        // that can make a sandbox, and one that is host-only and not here.
        let _ = ensure_fleet("skein-fleet");
        let asked = spawned.lock().unwrap().join("\n");
        assert!(
            !asked.is_empty(),
            "the second half provisioned nothing, so its assertion is empty"
        );
        assert!(
            !asked.split_whitespace().any(|word| word == "sbx"),
            "making a box startable reached for `sbx`, which is host-only and not here:\n{asked}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// Two people asking for the fleet at once create it once.
    ///
    /// The race the attempt lease exists for, and it is not hypothetical. It used to be two box
    /// *starts*, because `ensure_fleet` created as a side effect of launching a box; that caller is
    /// gone (SKEIN-576) and the race came with the create rather than staying behind. **It is a
    /// worse race here than it was there**: the check fails for every second of the minutes a
    /// create takes, and a person watching a button that appears to have done nothing presses it
    /// again.
    ///
    /// Genuinely concurrent, and the fake warden's create sleeps so the two overlap; a serialised
    /// pair would pass against no lease at all. The assertion is about what crossed the process
    /// boundary — exactly one create reaching the warden — and about the second caller being told
    /// what is happening rather than being told the work is owed.
    ///
    /// Two mechanisms could produce "one create" here and only one of them is under test. The
    /// warden has an outcome store keyed by operation id and a doorway that admits one at a time
    /// (§8.2, §8.5), so a *real* warden would collapse these two on its own — and `attempt.rs`'s
    /// lease would then be untested. The stub deliberately has neither: it counts and answers, so
    /// what stops the second create is skein's lease and nothing else.
    #[test]
    fn two_requests_at_once_create_the_fleet_once() {
        let _g = env_lock();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // Pinned because this reaches a `Place`: unset, `$SKEIN_FLEET_ROOT` defaults to
        // `/boxes`, which on a developer's machine is a live fleet (SKEIN-530).
        std::env::set_var("SKEIN_FLEET_ROOT", &home);
        use std::os::unix::fs::PermissionsExt;
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("sbx");
        // `sbx ls` still answers, because that is how skein decides the sandbox is absent — which is
        // also what a caller sees while another one is midway through creating it.
        std::fs::write(&fake, "#!/bin/sh\necho '[]'\nexit 0\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));

        // A warden that counts and takes its time, and does nothing else. See the note above.
        let creates = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let counted = std::sync::Arc::clone(&creates);
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let counted = std::sync::Arc::clone(&counted);
                std::thread::spawn(move || {
                    use std::io::{Read, Write};
                    let mut raw = [0u8; 4096];
                    let read = stream.read(&mut raw).unwrap_or(0);
                    if String::from_utf8_lossy(&raw[..read]).contains("/v1/create") {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    let body = r#"{"state":"ran","ok":true,"said":"made"}"#;
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                });
            }
        });
        std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));

        let started: Vec<_> = (0..2)
            .map(|_| std::thread::spawn(|| request_fleet_create("skein-fleet", &[])))
            .collect();
        let outcomes: Vec<Result<String, String>> =
            started.into_iter().map(|t| t.join().unwrap()).collect();
        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_WARDEN");
        // **And the fleet root, which this test used to leave set.** The env lock serialises the
        // tests that take it; it does not put back what one of them changed. A `$SKEIN_FLEET_ROOT`
        // left pointing at this temp directory makes every later test that reads the DEFAULT read
        // this one's instead — `probes::no_probe_files_a_signal_under_the_sandboxs_name` asserts
        // `/boxes/.skein/box-session.sh` and failed on it, in the full run only, while passing
        // alone. Pre-existing, and surfaced here because deleting the agent's tests changed which
        // test runs next.
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");

        let creates = creates.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            creates, 1,
            "two requests asked the warden to create the fleet {creates} times"
        );
        // And the one that lost says what is happening. "not there" would send it round again.
        let told: Vec<&String> = outcomes.iter().filter_map(|r| r.as_ref().err()).collect();
        assert!(
            told.iter().any(|why| why.contains("already being created")),
            "the second request was not told the first was under way: {outcomes:?}"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// The host's capacity is not measured, because what is measurable here is not the host.
    ///
    /// `available_parallelism`, `/proc/meminfo` and `df /` all answer about the machine the process
    /// is standing on, and that is the SANDBOX — 11 CPUs and 25.8 GiB on the fleet this was written
    /// on, against a 12-core host. `proposed_fleet_size` would then offer 70% of the fleet's own
    /// share as 70% of the machine, and a fleet resized from that proposal shrinks every time
    /// somebody accepts it.
    ///
    /// Zero is the existing vocabulary for "could not be read", not a new one: `HostCapacity` says
    /// a wrong total is worse than no proposal.
    ///
    /// It used to have a second half, asserting that host-driven skein still measured something —
    /// there is no host-driven skein (SKEIN-576), and that half went with it. The falsifiability it
    /// bought is bought instead by asserting **every** field, including the disk path: a
    /// `host_capacity` that started reading this machine again would fill them, and the one that
    /// reads most obviously wrong is the path, which would name a directory inside the sandbox.
    ///
    /// **What would make this fail**: putting `available_parallelism()` (or the `df` walk) back
    /// into `host_capacity`.
    #[test]
    fn the_capacity_dialog_is_not_offered_a_share_of_a_share() {
        let inside = host_capacity();
        assert_eq!(
            (
                inside.cpus,
                inside.memory_mb,
                inside.disk_free_mb,
                inside.disk_total_mb
            ),
            (0, 0, 0, 0),
            "skein measured the sandbox it is inside and offered it as the host's capacity: \
             {inside:?}"
        );
        assert_eq!(
            inside.disk_path, "",
            "a path was reported for a machine skein cannot see, and it names one in here: \
             {inside:?}"
        );
    }

    /// **The refusal says what `sbx rm -f` costs, counts it, and still hands over the line**
    /// (SKEIN-445, SKEIN-467, SKEIN-679).
    ///
    /// What this replaces: in-fleet, pressing Rebuild destroyed the sandbox skein was running in,
    /// so the destroy SUCCEEDED and the browser then rendered `resize failed:` — the word "failed"
    /// at the one moment it was most wrong, with the boxes already gone. `docs/architecture.md`
    /// §7.5: create and destroy both kill skein, so fleet lifecycle cannot live inside the fleet,
    /// permanently. The refusal that replaced it then had one clause about boxes — "nothing left to
    /// bring the boxes back" — which reads as availability, when what `sbx rm -f` does to a box is
    /// delete its only copy of work nobody pushed.
    ///
    /// It moved out of `bin/skein-server.rs` when `skein resize` started refusing through it too:
    /// one message on two surfaces, which is the point of it living in the library at all.
    ///
    /// **The assertions are on the claim, not the sentence.** Reword any of it; what has to survive
    /// is that the loss is stated, that the boxes at risk are counted and named, that a save is
    /// offered, and that the destroy line is still there for somebody whose boxes are clean.
    ///
    /// **What would make this fail**: dropping the count (proved — replacing `scale` with the empty
    /// string fired the count assertion); saying it for a *first create*, which hands somebody a
    /// warning about work that is not there and, worse, `sbx rm -f` for a sandbox they have not got;
    /// or a `None` from any path, since the caller reads `None` as permission.
    #[test]
    fn the_lifecycle_refusal_prices_the_destroy_in_boxes_and_keeps_the_line() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // **A fleet root of its own.** Unpinned it is `/boxes` — the machine's live fleet — and the
        // census cross-checks the disk, so this test would count the boxes of whatever machine ran
        // it (SKEIN-530).
        let root = tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", root.as_ref() as &std::path::Path);
        std::fs::write(
            (home.as_ref() as &std::path::Path).join("config.json"),
            r#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();
        placed("web-main");
        placed("api-worker");

        let rebuild = fleet_lifecycle_refusal("rebuild", true)
            .expect("in-fleet a rebuild destroys the machine skein is on, and it was allowed");
        assert!(
            rebuild.contains("cannot rebuild it from here"),
            "the refusal does not say what was refused: {rebuild}"
        );
        // The loss, in the two words that make it loss rather than downtime.
        assert!(
            rebuild.contains("uncommitted") && rebuild.contains("unpushed"),
            "the refusal does not say that destroying the sandbox deletes work: {rebuild}"
        );
        // The count, and the boxes it counted. The number is what makes it real; the names are what
        // let somebody check it against what they think is running.
        assert!(
            rebuild.contains("2 box"),
            "the refusal does not count what would be lost: {rebuild}"
        );
        for name in ["web-main", "api-worker"] {
            assert!(
                rebuild.contains(name),
                "the refusal counts boxes it will not name: {rebuild}"
            );
        }
        // The offer. Not the wording of it — the fact that a save is put to the person at all.
        assert!(
            rebuild.to_lowercase().contains("save"),
            "the refusal states the loss and offers nothing to do about it: {rebuild}"
        );
        // **And the offer names something a person can do**, on the surface they are reading it on.
        // A sentence saying a save "is a step of its own" with no verb and no button behind it is
        // an offer nobody can accept — which is what this said until [`save_boxes`] landed with it
        // (SKEIN-680). `tests/fix_lines.rs` is the other half: it fails the build if this names a
        // verb the CLI's dispatch has not got.
        assert!(
            rebuild.contains("`skein save`") && rebuild.contains("button"),
            "the refusal offers a save and names no way to take it, on either surface: {rebuild}"
        );
        // The destroy line as `Act::command` renders it, not as this test would spell it. That
        // renderer quotes every argument (`sbx 'rm' '-f' 'x'`), and a hand-written `rm -f` here
        // would be asserting against a spelling nothing produces.
        let destroy = crate::warden_client::Act::Destroy {
            sandbox: "skein-fleet".to_string(),
        }
        .command();
        assert!(
            rebuild.contains("On the host") && rebuild.contains(&destroy),
            "a refusal with no way forward is a dead end: {rebuild}"
        );
        assert!(
            rebuild.contains("skein-fleet"),
            "the lines name no sandbox, so they are not runnable: {rebuild}"
        );

        // A first create removes nothing, so it must neither print the destroy line nor tell
        // anybody they are about to lose work.
        let create = fleet_lifecycle_refusal("create", false).expect(
            "in-fleet skein cannot create the sandbox it is already inside, and it was let",
        );
        assert!(
            create.contains("cannot create it from here"),
            "the refusal does not say what was refused: {create}"
        );
        assert!(
            !create.contains(&destroy) && !create.contains("uncommitted"),
            "a first create was told to destroy something first: {create}"
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// **A census that could not be taken is not a fleet with nothing to lose** (SKEIN-347 again,
    /// on a second surface).
    ///
    /// [`placed_boxes`] cannot fail — a `read_dir` error and a record that will not parse both come
    /// back as zero boxes — and zero boxes is exactly what a person needs to be told when it is
    /// true. So the refusal counts with [`census_placed_boxes`], which refuses instead, and reports
    /// the refusal as itself. A message that answered "no box is placed in it" because it could not
    /// read `places` would be the destroy-costs-nothing reading of a fault, printed directly above
    /// the command that destroys everything.
    ///
    /// **What would make this fail**: counting with `placed_boxes`. Proved — swapping it in made
    /// this fixture report an empty fleet and fired the first assertion.
    #[test]
    fn a_refusal_that_could_not_count_the_boxes_says_so_rather_than_saying_none() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let root = tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", root.as_ref() as &std::path::Path);
        std::fs::write(
            (home.as_ref() as &std::path::Path).join("config.json"),
            r#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();
        placed("web-main");
        // The crash artifact the census exists for: present, zero-length, unparseable.
        std::fs::write(
            (home.as_ref() as &std::path::Path)
                .join("places")
                .join("web-main.json"),
            b"",
        )
        .unwrap();

        let rebuild =
            fleet_lifecycle_refusal("rebuild", true).expect("the refusal is always given");
        assert!(
            rebuild.contains("could not count") && rebuild.contains("read this as every box"),
            "an unreadable census was reported as a fleet with nothing in it: {rebuild}"
        );
        // And it is still a refusal somebody can act on.
        assert!(
            rebuild.contains("On the host"),
            "the refusal lost its way forward when the census failed: {rebuild}"
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }
}
