//! Archive, restore and resize: saving every box out of the sandbox, the census that must
//! succeed first, and putting the boxes back afterwards.

use super::*;

pub(super) fn box_archive(name: &str, run: &str) -> String {
    format!("{}/{run}.tar", box_state(name))
}

/// Copy one box out of the sandbox, whole, onto the host.
///
/// A **byte copy, not a reconstruction.** [`snapshot_box`] writes a git bundle, two patches and a
/// tarball of untracked files, from which the box is rebuilt on a fresh clone — that is the right
/// shape for a migration, where the destination is a different sandbox and often a different repo
/// state. It is the wrong shape here, where the destination is the same box in a rebuilt VM: the
/// reconstruction is slower (it re-clones), less faithful (the box comes back reassembled rather
/// than as it was), and it is where the fragility lives — the restore marker, the launch-spec
/// rewrite, the transcript realignment. `tar` has none of that, and carries `/tmp` besides, so a
/// resize is invisible to whatever the agent had half-finished there.
///
/// Uncompressed on purpose. The bulk of a box is git objects and `node_modules`, which are already
/// compressed; gzip would spend minutes of CPU to save little, where the write itself is seconds.
///
/// Sockets need no exclusion — `tar` skips them with a warning and carries on, which is what should
/// happen to a tmux socket whose server is about to die. `anchor.pid` does need one: it names a
/// process in a VM that will not exist, and restoring it would leave a box claiming an anchor that
/// was never there.
fn archive_box(fleet: &Place, name: &str, run: &str) -> Result<String, String> {
    let archive = box_archive(name, run);
    let mb = fleet
        .exec(&archive_script(name, &archive), Duration::from_secs(1800))
        .map_err(|e| format!("could not copy {name} out of the sandbox: {e}"))?;
    eprintln!("skein: {name} copied out ({} MiB)", mb.trim());
    Ok(archive)
}

/// The shell [`archive_box`] runs. Its own function so what the sandbox is asked to do is testable
/// without one — the exclusions here are the difference between a box that comes back and a box that
/// comes back claiming an anchor that does not exist.
///
/// **As root, and that is not a convenience.** A box is a general-purpose machine: it holds files
/// its own user cannot read — a fixture at mode 000, a root-owned build artifact, whatever a
/// container left behind — and `tar` exits non-zero on the first one it cannot open. Under the
/// `set -e` above that aborts the copy, and one unreadable file in one box then refuses the whole
/// fleet's resize. Which is what happened: 6,494 directories left by this project's own test suite,
/// each holding one deliberately unreadable file, and a resize of eight boxes stopped on the
/// seventh with the six already copied left to clean up.
///
/// Root also makes the copy *faithful*, which is the point of a byte copy: ownership and modes come
/// back as they were rather than as whoever ran the resize.
///
/// Deliberately NOT `--ignore-failed-read`. That turns the same situation into an archive missing
/// files nobody was told about, and a box restored short of its own contents is a worse outcome
/// than a resize that refused to start.
///
/// The archive is handed back to the invoking user afterwards, so the rest of the run — `du` here,
/// and the host reading it later — does not need root to touch what root has just written.
pub(super) fn archive_script(name: &str, archive: &str) -> String {
    format!(
        // **`rm -f` before the create, and it is not tidiness** (§9.5 R8). The archive lands in the
        // box's own state directory — it has to, because that is the one place mounted into the
        // sandbox that outlives the sandbox — and `tar -cf` FOLLOWS a symbolic link at its output
        // path. Anything that can plant one there gets root to write a tar file wherever it points.
        // The cover stops an ordinary box (its state is bound read-only inside its namespace) and
        // deliberately does not stop the workshop box, which sees every box's files by design.
        // `rm -f` unlinks the link rather than following it, so the create always writes a fresh
        // regular file.
        "set -e; mkdir -p {state}; sudo rm -f {archive}; \
         sudo tar -C {root} --exclude=./anchor.pid --warning=no-file-ignored -cf {archive} . ; \
         sudo chown \"$(id -u):$(id -g)\" {archive}; \
         du -sm {archive} | cut -f1",
        state = sh_quote(&box_state(name)),
        root = sh_quote(&box_root(name)),
        archive = sh_quote(archive),
    )
}

/// Put one box back into a freshly rebuilt sandbox, exactly as it was, and drop the copy.
///
/// **Nothing calls it since [`resize_fleet`] stopped rebuilding** (SKEIN-679), and it is `pub`
/// rather than deleted, for the reason [`snapshot_box`] is also `pub` with no production caller.
/// Three things pin it. `docs/architecture.md` §7.3 and `docs/inventory.md` name [`restore_script`]
/// as the mechanism resize keeps; `docs/parity.md` requires the byte copy not be removed — "nothing
/// here is removed; the requirement is that resize keeps carrying every box's work"; and the
/// escaping-archive test in this file is what establishes that `tar` refuses a member trying to
/// leave the directory it extracts into, which is a property nobody should have to re-derive.
///
/// **SKEIN-680 landed and still does not call it, which is worth saying precisely.** [`save_boxes`]
/// hands back [`restore_script`] per box, so what a person is told to type to bring a box back is
/// the text this function would run — the street is two-way, and the second half of it is a line
/// somebody pastes into a rebuilt sandbox rather than a call from here. There is nothing in-fleet
/// left to make that call: the sandbox a restore targets is one that has just been created on the
/// host, and this skein died with the old one.
///
/// The delete is the point of doing it here rather than leaving it to a caller: an archive is the
/// size of the box, so a resize that kept them would leave gigabytes on the host every time it ran —
/// measured, 16 GiB of boxes against 61 GiB free, which is two resizes before the host is full.
/// Once `tar -x` has succeeded the bytes are back where they belong and the copy is redundant.
///
/// `set -e` is what makes that safe: the `rm` is only reached if the extraction returned zero, so a
/// resize that fails partway keeps the only copy of the box it could not restore. That copy is then
/// deliberately left behind — the caller says where it is, because at that point it is the box.
pub fn restore_box(fleet: &Place, name: &str, archive: &str) -> Result<(), String> {
    fleet
        .exec(&restore_script(name, archive), Duration::from_secs(1800))
        .map(|_| ())
        .map_err(|e| format!("could not put {name} back: {e}"))
}

/// The shell [`restore_box`] runs. Its own function for the same reason [`archive_script`] is: the
/// ordering here — extract, *then* delete, under `set -e` — is the whole safety property.
fn restore_script(name: &str, archive: &str) -> String {
    format!(
        // Root on the way back too, and for the matching reason: the archive holds modes and owners
        // the invoking user cannot recreate, and an unprivileged extract would either fail on them
        // or quietly hand every file to whoever ran the resize. As root, `tar` restores the
        // ownership recorded in the archive, which is what makes this a copy rather than a rebuild.
        "set -e; sudo mkdir -p {root}; sudo tar -C {root} -xf {archive}; rm -f {archive}",
        root = sh_quote(&box_root(name)),
        archive = sh_quote(archive),
    )
}

/// One box's work, and where it went — or why it did not go anywhere.
///
/// **Flat, and every field a `String`, because two front doors read the same value** (SKEIN-680).
/// `skein save` prints these and `POST /api/fleet/save` serialises them; a `Result` per box would
/// be one shape in Rust and another on the wire, and the wire shape is the one the cockpit renders.
/// An empty `error` is a box that made it out; an empty `archive` is one that did not.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SavedBox {
    pub name: String,
    /// The archive on the host — empty when the copy failed.
    pub archive: String,
    /// The shell that puts this box back into a rebuilt sandbox — empty when there is nothing to
    /// put back. It is [`restore_script`], the same text [`restore_box`] runs, rather than a
    /// sentence about it: what somebody is told to type and what skein would run then cannot drift,
    /// which is the argument [`create_line`] makes for itself.
    pub restore: String,
    /// Why this box was not copied out — empty when it was.
    pub error: String,
}

/// Copy every box's work out of the sandbox and onto the host. **Its own act, and nothing else.**
///
/// # Why this exists
///
/// [`archive_box`] — the byte copy that is the whole of "your work is safe" — was reachable only as
/// phase 1 of [`resize_fleet`], which then destroys the fleet. That is the only reason a person
/// could not simply save their work: the mechanism was there, and the one door to it led through a
/// destroy. The owner was twice offered the other shape, where a refusal archives every box first
/// and *then* hands over the destroy line, and declined it twice in the same words — "provide
/// option to save work and ask user to save it by clicking the button and when clicked show where
/// it is saved". So the archive is never a side effect of anything. **Inform and offer; never
/// perform, never withhold** ([`destroy_costs`], SKEIN-445, SKEIN-679).
///
/// # What it must not do, which is most of what it is
///
/// **It destroys nothing, stops nothing, and does not need the fleet idle.** `tar` reads; a box
/// keeps running through its own copy and an agent keeps working. That is what makes this a backup
/// rather than a checkpoint inside a lifecycle operation, and it is why it can be pressed at any
/// time — including the moment somebody realises what `sbx rm -f` is about to cost them, which is
/// the moment it exists for.
///
/// # Reported per box, never in aggregate
///
/// One failure does not end the run, and **this is where a save differs from a resize**. A resize
/// aborts on the first box it cannot read, because the boxes already copied are about to be
/// destroyed and a partial copy is lost work — "that ordering is the entire safety property of this
/// function". Nothing is destroyed here, so seven boxes on the host is strictly better than none,
/// and the eighth is *named* rather than averaged away: it is the one whose work is still only
/// inside the sandbox, and it is the one the person needs to know about.
///
/// # What it refuses, and it refuses before writing a byte
///
/// A census that could not be taken, because a save that quietly skipped a box would report success
/// about the box it missed (SKEIN-347, the same reading `destroy_costs` refuses). A host with no
/// room ([`room_to_copy_out`]). A name that is not a box on disk, since an archive of nothing reads
/// exactly like a save. Naming boxes deliberately bypasses only the *census*: it is the one refusal
/// a person cannot clear in the moment, and a box they can point at is one they already know is
/// there.
pub fn save_boxes(only: &[String]) -> Result<Vec<SavedBox>, String> {
    let sandbox = fleet_sandbox();
    let wanted: Vec<String> = match only.is_empty() {
        true => census_placed_boxes(&sandbox)
            .map_err(|why| {
                format!(
                    "could not take a reliable census of the boxes in {sandbox} ({why}), and a \
                     save that silently skipped one would report success about the box it missed \
                     — nothing was copied out.\n  \
                     Fix that, or name the boxes to save: `skein save <box>`."
                )
            })?
            .into_iter()
            .map(|(name, _)| name)
            .collect(),
        false => only.to_vec(),
    };
    for name in &wanted {
        // Every path skein derives for a box hangs off [`box_root`], so this is the guard that
        // keeps a name out of the shell the copy runs — `archive_script` quotes as well, and
        // neither is meant to be load-bearing alone.
        if !valid_name(name) {
            return Err(format!(
                "{name:?} is not a box name — nothing was copied out"
            ));
        }
        // Checked against the disk rather than against `places`, and before anything is written.
        // **A checkout, not merely a directory**, which is [`census_placed_boxes`]'s own test for
        // what a box is: `tar` refuses a root that is not there, so a plain typo would be reported
        // as a copy that failed in tar's words — but it is perfectly happy to archive a directory
        // that exists and holds nothing, and that one comes back as a successful save of an empty
        // file. A save reported about work that is not in it is the one failure the report cannot
        // show you.
        if !std::path::Path::new(&box_root(name)).join("tree").is_dir() {
            return Err(format!(
                "there is no box named {name} in {sandbox} — {} holds no checkout, and an archive \
                 of nothing reads exactly like a save. Nothing was copied out.",
                box_root(name)
            ));
        }
    }
    if wanted.is_empty() {
        return Err(format!(
            "no box is placed in {sandbox}, so there is nothing to save"
        ));
    }
    let fleet = own_sandbox(&sandbox);
    room_to_copy_out(&fleet, "nothing was copied out")?;
    // Milliseconds, not seconds. [`archive_script`] unlinks its output path before writing it, so
    // two saves that shared a run id would take turns writing one file and both report success —
    // and two presses a second apart is what a person does when they are not sure the first landed.
    let run = format!("save-{}", Utc::now().format("%Y%m%dT%H%M%S%3fZ"));
    Ok(wanted
        .iter()
        .map(|name| match archive_box(&fleet, name, &run) {
            Ok(archive) => SavedBox {
                name: name.clone(),
                restore: restore_script(name, &archive),
                archive,
                error: String::new(),
            },
            Err(why) => SavedBox {
                name: name.clone(),
                archive: String::new(),
                restore: String::new(),
                error: why,
            },
        })
        .collect())
}

/// The one shell [`docker_state_at_risk`] runs, printing `volume <name>` and `image <tag>` lines.
///
/// Its own constant so it can be run against a stub `docker` in a test — the filtering *is* the
/// decision here, and an assertion about the Rust that reads the output would prove nothing about
/// which images and volumes actually reach it.
///
/// `echo asked` is the marker that distinguishes "Docker answered, and holds nothing worth saving"
/// from "Docker did not answer". Without it both are the empty string, and the safe reading of one
/// is the unsafe reading of the other.
const DOCKER_PROBE_SH: &str = "docker volume ls --format '{{.Name}}' 2>/dev/null \
     | grep -vx '[0-9a-f]\\{64\\}' | sed 's/^/volume /'; \
     docker image ls --digests --format '{{.Digest}} {{.Repository}}:{{.Tag}}' 2>/dev/null \
     | awk '$1==\"<none>\" && $2!=\"<none>:<none>\" {print \"image\", $2}'; \
     echo asked";

/// What a resize would destroy in `/var/lib/docker`, named so the person running it can decide.
///
/// A resize is `sbx rm -f` and `sbx create`, and `/var/lib/docker` is a **separate disk** made with
/// the sandbox and destroyed with it. [`archive_box`] copies [`fleet_root`] — the boxes' checkouts —
/// and nothing else, so every image and volume goes. That is fine for most of what is in there:
/// a pulled image comes back with `docker pull`, and a build cache is a cache. It is not fine for
/// the two kinds of thing nothing can recreate — an image that was **built here** and never pushed,
/// and a **named volume**, which exists precisely because someone wanted data to outlive a
/// container. Measured on this fleet: 45 GB of `/var/lib/docker`, including two locally-built
/// images and three named volumes.
///
/// Locally built is read as "has no repo digest". A digest is what an image gets by being pulled
/// from or pushed to a registry, so its absence means no registry has a copy. That over-reports a
/// pulled image someone has since retagged, and that is the right way to be wrong: this decides
/// whether to *ask*, and asking about something recoverable costs a sentence.
///
/// Anonymous volumes are excluded — a 64-hex name is one Docker made up for a container that did
/// not ask for a name, and treating those as precious would refuse every resize forever.
///
/// `Err` is "could not ask", not "nothing to lose", and the caller must not read it as the latter:
/// a wedged dockerd answers no question at all, and that is the state this fleet is most often in
/// when someone reaches for a resize.
fn docker_state_at_risk(fleet: &Place) -> Result<Vec<String>, String> {
    let out = fleet
        .exec(DOCKER_PROBE_SH, Duration::from_secs(60))
        .map_err(|e| format!("asking Docker what it is holding: {e}"))?;
    if !out.lines().any(|l| l.trim() == "asked") {
        return Err("Docker did not answer".into());
    }
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("volume ") || l.starts_with("image "))
        .map(str::to_string)
        .collect())
}

/// The refusal itself, as text — its own function so the wording is testable without a sandbox.
///
/// Long on purpose. This stops a command the person deliberately typed, so it has to say what would
/// go, why skein cannot carry it, how to carry it by hand, and how to proceed anyway. A refusal that
/// only says no gets worked around by the shortest available route, which here is `sbx rm -f`.
fn docker_refusal(at_risk: &[String]) -> String {
    // Capped: a fleet with forty volumes should not bury the last line, which is the one that says
    // how to proceed.
    const SHOWN: usize = 8;
    let listed: Vec<&str> = at_risk.iter().take(SHOWN).map(String::as_str).collect();
    let more = at_risk.len().saturating_sub(listed.len());
    format!(
        "a resize destroys /var/lib/docker, and it is holding {} thing{} nothing can put back — \
         resize aborted with the sandbox untouched.\n  {}{}\n  \
         That disk is created with the sandbox and destroyed with it, and skein's copy carries the \
         boxes' checkouts only. Images without a repo digest were built here and are on no \
         registry; named volumes are data someone asked to outlive a container.\n  \
         Save them first — `docker save -o <file> <image>` and, per volume, \
         `docker run --rm -v <volume>:/v -v {state}:/out alpine tar -C /v -cf /out/<volume>.tar .` \
         — writing to {state}, which is on the host and survives the rebuild.\n  \
         Or pass --drop-docker to resize anyway and lose them.",
        at_risk.len(),
        if at_risk.len() == 1 { "" } else { "s" },
        listed.join("\n  "),
        match more {
            0 => String::new(),
            n => format!("\n  …and {n} more"),
        },
        state = box_state_root(),
    )
}

/// Refuse a copy-out that would fill the host disk, before a byte of it is written.
///
/// The archives are the size of the boxes — every checkout, every `node_modules`, every `/tmp` —
/// and they land on the host's own disk. Measured here at 16 GiB of boxes against 61 GiB free,
/// which fits and is not comfortable. Running the host out of space *during* a resize would be the
/// worst possible moment for it: the sandbox is gone and the rescue is half-written.
///
/// A fifth over the measured size, because `du` counts what the boxes use and `tar` writes a little
/// more (headers, and no sparse-file handling).
///
/// **`refused` is what the caller calls its own refusal**, and the two are not interchangeable: a
/// resize has to say the sandbox is untouched, because the reader is being told a destroy was
/// aborted; a save threatened nothing and says only that nothing was copied out. One measurement,
/// two acts with different stakes ([`save_boxes`], SKEIN-680), and a refusal that borrowed the
/// other's sentence would reassure somebody about a sandbox that was never at risk.
fn room_to_copy_out(fleet: &Place, refused: &str) -> Result<(), String> {
    // `key value` lines rather than three bare numbers, because the third is absent whenever no
    // leftovers exist and positional parsing would then read the free space as the leftover size.
    let script = format!(
        "echo \"boxes $(du -sxm {root} 2>/dev/null | cut -f1)\"; \
         echo \"free $(df -Pm {state} | awk 'NR==2{{print $4}}')\"; \
         echo \"stale $(cat {state}/*/*.tar 2>/dev/null | wc -c | awk '{{print int($1/1048576)}}')\"",
        root = sh_quote(&fleet_root()),
        state = sh_quote(&box_state_root()),
    );
    let out = fleet.exec(&script, Duration::from_secs(300))?;
    let read = |key: &str| -> Option<u64> {
        out.lines().find_map(|l| {
            l.trim()
                .strip_prefix(&format!("{key} "))?
                .trim()
                .parse()
                .ok()
        })
    };
    let (Some(boxes), Some(free)) = (read("boxes"), read("free")) else {
        // Unmeasurable is not the same as too small, and refusing on it would make a resize
        // impossible for anyone whose `df` says something unexpected.
        eprintln!("skein: could not measure the space this needs; continuing");
        return Ok(());
    };
    let needed = boxes + boxes / 5;
    if free < needed {
        let stale = read("stale").unwrap_or(0);
        return Err(format!(
            "copying the boxes out needs about {needed} MiB and the host has {free} MiB free — \
             {refused}. The boxes are {boxes} MiB.{}",
            match stale {
                0 => " Freeing space, or `skein stop`ping boxes you do not need, makes room."
                    .to_string(),
                // **Every archive under there, not only a resize's**, since [`save_boxes`] leaves
                // its copies behind on purpose — they are what a person asked for. Which kind this
                // is decides whether deleting it is tidying or throwing away the only copy of a
                // box's work, and that is a question for the person rather than for a glob.
                mib => format!(
                    " {mib} MiB of that is held by archives already under {}: saves somebody asked \
                     for, or copies from a resize that did not finish. Check which before deleting \
                     them — for a box that never came back, the copy is the box.",
                    box_state_root()
                ),
            }
        ));
    }
    Ok(())
}

/// Copy every box out of the fleet sandbox, record the new size, and ask the warden to destroy it.
///
/// **It used to resize, and the second half of that could not run** (SKEIN-679). The sequence was
/// five phases — copy every box out, destroy, recreate, copy back, restart — and in this deployment
/// it ends at the destroy, which terminates the process performing it. The comment above the
/// destroy said so itself. What followed was `forget_place`, a rebuild, a restore-and-restart loop
/// and a list of boxes that did not come back, none of which a dead process executes; and the
/// rebuild reached for [`ensure_fleet`], whose own doc says "**Nothing here creates a fleet**", so
/// even given a surviving process it would have taken that function's refusal arm unconditionally.
/// Deleted rather than moved: putting the whole sequence behind the warden — the one party that
/// outlives the destroy — was offered and not chosen.
///
/// **Nothing reaches this in production now, and that is the shape rather than an oversight.**
/// `skein resize` refuses through [`fleet_lifecycle_refusal`] without calling it, and
/// `POST /api/fleet/resize` refuses through the same function before it gets here. What is kept is
/// the half a *save* is made of — the census, the space check, the Docker refusal, the login
/// capture and the byte copy — together with the rules in it that this fleet learned the hard way
/// and `tests/resize_rules.rs` pins. [`save_boxes`] is that half, reachable on its own terms now
/// (SKEIN-680), and it shares the census, the space check and the byte copy with this rather than
/// restating them.
///
/// sbx fixes memory, CPUs and disk at creation — on Apple silicon it is Virtualization.framework
/// underneath, where a VM's memory is fixed in its configuration and validated at start — so
/// changing any of them still means a new sandbox. Making that new sandbox is a person's act at the
/// host, and [`create_line`] renders what they type.
///
/// The boxes are copied **whole**, `/tmp` included, rather than reconstructed from a snapshot. See
/// [`archive_box`] for why: the box that comes back is the box that left, so nothing downstream has
/// to know a resize happened.
///
/// **Nothing is destroyed until every box is safely on the host.** A partial copy is not a partial
/// resize, it is lost work, and the boxes that would lose it are exactly the ones that could not be
/// read — so a single failure aborts with the sandbox still standing and every box still in it. That
/// ordering is the entire safety property of this function, and it is the half that survives.
pub fn resize_fleet(memory: &str, cpus: &str, disk: &str, drop_docker: bool) -> Result<(), String> {
    // Everything, and around the whole of it rather than around the destroy: a resize that fails
    // halfway leaves the sandbox in a state none of the four gates has seen, and a caller reading a
    // remembered answer then is reading a picture of a fleet that no longer exists.
    disturbing(
        &[
            Remembered::SandboxListing,
            Remembered::BoxLiveness,
            Remembered::BoxDisk,
            Remembered::FleetResources,
        ],
        || resize_fleet_inner(memory, cpus, disk, drop_docker),
    )
}

/// Which boxes are in `sandbox` — **or why that question could not be answered**.
///
/// [`placed_boxes`] cannot fail. `fs::read_dir(&dir).into_iter().flatten().flatten()`
/// (src/place/record.rs:272) turns a `read_dir` **error** into zero entries, and `read_place_record`
/// (place/record.rs:287) drops any record that will not parse; both come back as "this sandbox holds no
/// boxes", which is also exactly what an empty fleet looks like. That is harmless for a board,
/// which renders one row fewer, and fatal for [`resize_fleet`], whose stated safety property is
/// that nothing is destroyed until every box named here is on the host: a box missing from the
/// census is never archived, and then `sbx rm -f` takes its VM-local checkout with the sandbox
/// (SKEIN-347). EACCES on `~/.skein/places`, or one record truncated to zero bytes by a crash
/// between [`crate::util::write_atomic`]'s write and its rename, is all it takes.
///
/// So every failure here is an `Err`, and the only empty `Ok` is an absent `places` directory — a
/// skein that has never started a box.
///
/// **The census is then cross-checked against the disk**, because the first half only catches a
/// record skein could not read. A record never written, or removed by hand, leaves a box with no
/// evidence in `places` at all and nothing to raise an error about. The box roots are local —
/// `local_disk_usage` walks precisely this directory — so a second and independent source of truth
/// is one listing away: a directory under [`fleet_root`] holding a `tree` is a box, whether or not
/// `places` has heard of it. It used to be skipped host-driven, where that path lived inside the
/// sandbox and was not on the machine skein stood on; there is no such deployment (SKEIN-576) and
/// no arm to skip.
///
/// A leftover box root with no live box therefore stops a resize. That is the direction to be wrong
/// in: clearing it is one `ls` and a decision by a person, and the alternative is a `tree` full of
/// unpushed work destroyed by a command that then reports complete success.
pub(super) fn census_placed_boxes(sandbox: &str) -> Result<Vec<(String, PlaceRecord)>, String> {
    let dir = skein_home().join("places");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // Nothing has ever been placed. The one reading of "no boxes" that is a fact.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("listing {}: {e}", dir.display())),
    };
    let mut found: Vec<(String, PlaceRecord)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading an entry of {}: {e}", dir.display()))?;
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".json") else {
            continue; // the lock files and whatever else lives beside the records
        };
        let path = entry.path();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let record: PlaceRecord =
            serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        if record.sandbox == sandbox {
            found.push((name.to_string(), record));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    // **The census is cross-checked against the disk, always.** This used to be guarded by
    // `deployment::in_fleet()`, because host-driven the box roots lived inside the sandbox and
    // were not on the machine skein was standing on. Skein is standing in that sandbox now
    // (SKEIN-576), so the second source of truth is always one listing away.
    let root = fleet_root();
    let roots = match std::fs::read_dir(&root) {
        Ok(roots) => roots,
        // No fleet root at all is no boxes on disk, and agrees with an empty census.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(format!("listing {root}: {e}")),
    };
    let mut unaccounted: Vec<String> = Vec::new();
    for entry in roots.flatten() {
        let path = entry.path();
        // A box is a directory with a checkout in it. `.skein` — the launcher, the probes, the
        // server's own files — is not one, and neither is a stray file.
        if !path.join("tree").is_dir() {
            continue;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if !found.iter().any(|(placed, _)| *placed == name) {
            unaccounted.push(name);
        }
    }
    if !unaccounted.is_empty() {
        unaccounted.sort();
        return Err(format!(
            "{root} holds {} checkout{} that {} has no placement record for, so nothing could \
             carry {} out: {}",
            unaccounted.len(),
            if unaccounted.len() == 1 { "" } else { "s" },
            dir.display(),
            if unaccounted.len() == 1 { "it" } else { "them" },
            unaccounted.join(", ")
        ));
    }
    Ok(found)
}

fn resize_fleet_inner(
    memory: &str,
    cpus: &str,
    disk: &str,
    drop_docker: bool,
) -> Result<(), String> {
    let sandbox = fleet_sandbox();
    // A size with no unit is refused HERE rather than reinterpreted in `parse_mib`, and the
    // distinction matters: `parse_mib` mirrors what sbx itself does with a bare number (it reads
    // bytes), so changing it would make skein and sbx disagree about the same string. What is wrong
    // is not the parse — it is that `skein resize 26` silently means 26 bytes, which `memory_plan`
    // turns into zero for the boxes and every derived ceiling into its 512M floor, with nothing
    // said. Nobody typing a fleet size means bytes.
    if let Some(bare) = [memory.trim(), cpus.trim(), disk.trim()]
        .iter()
        .zip(["memory", "cpus", "disk"])
        .find(|(v, what)| *what != "cpus" && !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        .map(|(v, what)| format!("{what} {v:?}"))
    {
        return Err(format!(
            "{bare} has no unit, and a bare number is read as BYTES — which would set the fleet to \
             a few bytes and collapse every box's ceiling to its floor without saying so. Write the \
             unit: 26g, 512m."
        ));
    }
    // The census, and it may refuse. This list is what phase 1 copies out and phase 2 destroys the
    // sandbox around, so a box that is missing from it is a box whose work this function silently
    // deletes — see [`census_placed_boxes`]. Read on the same terms as the Docker check below:
    // could not ask is not the same as nothing to lose.
    let boxes = census_placed_boxes(&sandbox).map_err(|why| {
        format!(
            "could not take a reliable census of the boxes in {sandbox} ({why}), and a resize \
             destroys every box it did not copy out first — resize aborted with the sandbox \
             untouched.\n  \
             Fix that, or move the unreadable file aside, and try again."
        )
    })?;
    let fleet = own_sandbox(&sandbox);

    // ---- phase 1: get everything out, or change nothing ----
    // Space before work: the archives are the size of the boxes, and discovering the host is full
    // after the sandbox is gone would be the worst possible moment to discover it.
    room_to_copy_out(&fleet, "resize aborted with the sandbox untouched")?;
    // Then what the copy does NOT cover. `/var/lib/docker` is a disk of its own, destroyed with the
    // sandbox and carried by nothing, so a resize silently discards every locally-built image and
    // named volume in it — 45 GB of them on this fleet. Refused rather than warned: a warning is
    // read after the fact, and there is no after the fact for `sbx rm -f`.
    if !drop_docker {
        match docker_state_at_risk(&fleet) {
            Ok(at_risk) if !at_risk.is_empty() => return Err(docker_refusal(&at_risk)),
            Ok(_) => {}
            // Could not ask, which is not the same as nothing to lose — and a wedged dockerd is the
            // state this fleet is most often in when someone reaches for a resize. Refusing on
            // silence is the only reading that cannot destroy something nobody was told about.
            Err(why) => {
                return Err(format!(
                    "could not check what Docker is holding ({why}), and a resize destroys \
                     /var/lib/docker — resize aborted with the sandbox untouched.\n  \
                     Restart the daemon and try again, or pass --drop-docker to resize anyway and \
                     lose whatever is in there."
                ))
            }
        }
    }
    // The login, because it lives in the sandbox's HOME and the destroy takes it. Nothing here
    // rebuilds any more, so what the capture saves it for is the NEXT sandbox: [`ensure_fleet`]
    // restores the kept copy at the first box start in it. It has to happen BEFORE the destroy —
    // `ensure_fleet`'s own call runs after `sbx create`, when the sandbox is empty and there is
    // nothing left to save. Measured the hard way: a login made between two resizes was gone after
    // the second.
    //
    // And it must be `capture_`, not `sync_`: a read that FAILED used to come back as empty bytes,
    // which reads as "the sandbox has no login" — so the same loss the line above records happened
    // again whenever the exec timed out, this time with skein believing it had saved everything.
    // Refused rather than warned, for `docker_state_at_risk`'s reason a few lines up: a warning is
    // read after the fact, and there is no after the fact for a destroy.
    capture_fleet_login(&sandbox).map_err(|why| {
        format!(
            "could not save the fleet's login out of {sandbox} ({why}), and the destroy takes the \
             HOME it lives in — resize aborted with the sandbox untouched.\n  \
             Try again once the sandbox is answering."
        )
    })?;
    let run = format!("resize-{}", Utc::now().format("%Y%m%dT%H%M%SZ"));
    for (name, _) in &boxes {
        // The repo and the branch are not needed to *save* the box — the archive is the whole box —
        // and nothing here starts anything again. They are checked because they are what says where
        // a saved box came FROM: a box belonging to no registered repo, or on no recorded branch, is
        // one nobody can be told how to put back, and an archive nobody can act on is not a save.
        // Asked for every box before the first byte is written, because discovering it per box would
        // leave a fleet half saved.
        if repo_for_box(name).is_none() {
            return Err(format!(
                "box {name} belongs to no registered repo, so nothing could say where its work \
                 came from or put it back — resize aborted with the sandbox untouched"
            ));
        }
        if branch_of(name).unwrap_or_default().trim().is_empty() {
            return Err(format!(
                "box {name} has no recorded branch to come back on — resize aborted with the \
                 sandbox untouched"
            ));
        }
        archive_box(&fleet, name, &run)
            .map_err(|e| format!("{e} — resize aborted with the sandbox untouched"))?;
    }

    // ---- phase 2: the destructive part ----
    // `update_config`, not `load_config` + `save_config`. The pair reads the settings OUTSIDE the
    // lock and then writes a whole struct built from that reading, so anything somebody changed in
    // the cockpit between the two is silently put back — which is precisely the lost update
    // `update_config`'s own doc says it exists to prevent, and this was the last caller in the crate
    // still doing it by hand. It matters more here than anywhere: a resize is minutes long, and the
    // window is the whole of it.
    //
    // It is also the only place the new size goes now. Nothing here creates a sandbox, so what
    // these settings reach is [`create_line`] — the line a person runs at the host, which renders
    // them.
    crate::config::update_config(|config| {
        config.fleet_memory = memory.trim().to_string();
        config.fleet_cpus = cpus.trim().to_string();
        // Empty keeps the configured disk rather than resetting it to sbx's 20 GB: `skein resize
        // 32g` is a memory change, and it must not silently shrink the disk back on the way past.
        if !disk.trim().is_empty() {
            config.fleet_disk = disk.trim().to_string();
        }
        Ok(())
    })?;
    // Not the 30s action budget: tearing a microVM down is slower than a status query, and a
    // timeout here is reported as a failed destroy while the destroy carries on regardless.
    // Through the warden, for the same reason as the create and one of its own: destroy terminates
    // skein, so this is the operation that most obviously cannot live inside the thing it destroys.
    // A person at the host confirms it by typing the operation id (§8.1), which for "destroy every
    // box's sandbox" is the right amount of friction.
    match crate::warden_client::perform(&crate::warden_client::Act::Destroy {
        sandbox: sandbox.clone(),
    }) {
        crate::warden_client::Performed::Warden(_) => {}
        // The dead end this replaced: a refused or missing warden used to end a resize with a
        // sentence naming no command at all, at the one moment a person most needs one — every box
        // is already archived and the sandbox is still standing. The prompt carries the line, and
        // *this* sentence carries what the prompt cannot know: where the copies are.
        crate::warden_client::Performed::Prompt(prompt) => {
            return Err(format!(
                "{}\n\nEvery box's work is already copied out to its own state directory as \
                 {run}.tar, so nothing is lost by stopping here — the sandbox is untouched.",
                prompt.render()
            ));
        }
        // A destroy that may have happened is the one answer no command can be offered for: running
        // it again against a sandbox that is already gone is a different operation than the one
        // being retried.
        crate::warden_client::Performed::Uncertain(answered) => {
            return Err(format!(
                "could not tell whether {sandbox} was destroyed: {} — every box's work is copied \
                 out to its own state directory as {run}.tar, and those copies are the only thing \
                 that survives the sandbox either way",
                answered.detail()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::testkit::*;
    use crate::testutil::*;

    /// A resize destroys `/var/lib/docker`, so it has to know what is in there worth keeping.
    ///
    /// The filtering is the decision, so it is run for real against a stub `docker` rather than
    /// asserted about: what must survive the filter is an image nothing can re-pull and a volume
    /// someone named, and what must not is everything a `docker pull` or a rebuild puts back.
    /// Getting the second half wrong is not harmless — a resize that refuses over a dangling image
    /// refuses forever, and the way round it is `sbx rm -f`, which loses the boxes too.
    #[test]
    fn the_resize_asks_docker_only_about_what_it_could_not_put_back() {
        let dir = tempdir();
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // Real shapes: a pulled image carries a digest, one built here does not, a dangling layer
        // has neither name nor tag, and an anonymous volume is 64 hex characters Docker chose.
        std::fs::write(
            bin.join("docker"),
            "#!/bin/sh\ncase \"$1 $2\" in\n\
             \"volume ls\") printf 'thing-cargo\\nthing-target\\n\
             3f2a91b8c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1\\n' ;;\n\
             \"image ls\") printf 'sha256:aa11 pgvector/pgvector:pg16\\n\
             <none> thing-rust:local\\n<none> thing-ocr:local\\n<none> <none>:<none>\\n' ;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(
            bin.join("docker"),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(DOCKER_PROBE_SH)
            // **A PATH of its own, rather than the process's.** This used to append to
            // `env::var("PATH")`, and `board`, `ai` and `takeover` all rewrite that variable while
            // they run — so the probe inherited whichever one a neighbour was holding, found a
            // different `docker`, and failed on output it never asked for. Seen once under
            // `cargo test --tests` on 2026-09-03 and never alone, which is SKEIN-471's signature: a
            // test whose answer depends on what ran beside it.
            //
            // The lock was the other candidate and is the wrong tool here — it would serialise a
            // subprocess-running test against every other env test and the suite stopped finishing
            // in ten minutes. Not depending on the shared variable is both cheaper and stricter:
            // the fake `docker` and a system path are the whole of what this probe should see.
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .expect("run the probe");
        let lines: Vec<&str> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| Box::leak(l.to_string().into_boxed_str()) as &str)
            .collect();

        assert_eq!(
            lines,
            vec![
                "volume thing-cargo",
                "volume thing-target",
                "image thing-rust:local",
                "image thing-ocr:local",
                "asked",
            ],
            "kept: named volumes and images no registry has a copy of. dropped: the anonymous \
             volume, the pulled image, the dangling layer"
        );
    }

    /// The refusal has to be worth reading, because the alternative to reading it is `sbx rm -f`.
    #[test]
    fn the_refusal_names_what_would_go_and_how_to_proceed_anyway() {
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let at_risk: Vec<String> = ["image thing-rust:local", "volume thing-cargo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let said = docker_refusal(&at_risk);
        assert!(said.contains("thing-rust:local") && said.contains("thing-cargo"));
        assert!(
            said.contains("sandbox untouched"),
            "the first thing to know is that nothing has happened yet: {said}"
        );
        assert!(
            said.contains("docker save") && said.contains("tar -C /v"),
            "refusing without saying how to keep them just moves the problem: {said}"
        );
        assert!(
            said.contains("--drop-docker"),
            "a refusal with no way past it gets worked around outside skein: {said}"
        );
        // A fleet with forty volumes must not bury the line that says how to proceed.
        let many: Vec<String> = (0..40).map(|i| format!("volume v{i}")).collect();
        let long = docker_refusal(&many);
        assert!(long.contains("…and 32 more") && long.contains("--drop-docker"));
        std::env::remove_var("SKEIN_HOME");
    }

    /// The create does not write through a link somebody planted at the archive's path.
    ///
    /// `tar -cf` follows a symbolic link at its output path, and the output path is inside the box's
    /// own state directory — it has to be, since that is the one place mounted into the sandbox that
    /// outlives it. So anything able to plant a link there gets **root** to write a tar file
    /// wherever it points. The cover stops an ordinary box; the workshop box is exempt from the
    /// cover by design, which is exactly the actor this has to hold against.
    #[test]
    fn the_archive_is_not_written_through_a_link_left_at_its_path() {
        let _g = env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        // And the fleet root beside it, for the same reason: `util::fleet_root` refuses an
        // unpinned test rather than answering `/boxes`, which on any machine running skein is the
        // live fleet (SKEIN-690).
        std::env::set_var("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let archive = box_archive("web-main", "resize-x");
        let script = archive_script("web-main", &archive);
        let unlink = format!("sudo rm -f {}", sh_quote(&archive));
        assert!(
            script.contains(&unlink),
            "nothing unlinks the path first: {script}"
        );
        assert!(
            script.find(&unlink) < script.find("tar -C"),
            "the unlink happens after the archive is written, which is no unlink at all: {script}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// What `tar` does with an archive that tries to escape the directory it is extracted into.
    ///
    /// **Measured rather than assumed**, and pinned here because the restore is `sudo tar -xf` of an
    /// archive holding a box's own contents: the property that makes that safe belongs to tar and to
    /// the flags it is given, and it would vanish the day somebody added `-P`. Three escapes, all of
    /// them refused by GNU tar 1.35 with no flag asked for: a `..` member, an absolute member, and a
    /// symlink member with a file written through it.
    #[test]
    fn an_archive_cannot_write_outside_the_directory_it_is_extracted_into() {
        let home = tempdir();
        let out = home.join("out");
        let victim = home.join("victim");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&victim).unwrap();
        let archive = home.join("escaping.tar");

        // Built by hand with python's tarfile, because `tar` itself will not create these members:
        // it names them from a tree walk, which is why the archive step is not where this risk is.
        let built = std::process::Command::new("python3")
            .arg("-c")
            .arg(format!(
                "import tarfile, io\n\
                 t = tarfile.open({archive:?}, 'w')\n\
                 for name in ['../../victim/escaped', {absolute:?}]:\n\
                 \x20   d = b'escaped'\n\
                 \x20   i = tarfile.TarInfo(name); i.size = len(d)\n\
                 \x20   t.addfile(i, io.BytesIO(d))\n\
                 l = tarfile.TarInfo('link'); l.type = tarfile.SYMTYPE; l.linkname = {victim:?}\n\
                 t.addfile(l)\n\
                 d = b'through the link'\n\
                 f = tarfile.TarInfo('link/through'); f.size = len(d)\n\
                 t.addfile(f, io.BytesIO(d))\n\
                 t.close()\n",
                archive = archive.to_string_lossy(),
                absolute = victim.join("absolute").to_string_lossy(),
                victim = victim.to_string_lossy(),
            ))
            .status();
        match built {
            Ok(s) if s.success() => {}
            _ => {
                crate::testutil::skip("no python3 to build an escaping archive by hand");
                return;
            }
        }

        let extracted = std::process::Command::new("tar")
            .arg("-C")
            .arg(&out)
            .arg("-xf")
            .arg(&archive)
            .output()
            .expect("tar");

        // Nothing outside the destination, which is the whole claim.
        assert_eq!(
            std::fs::read_dir(&victim).unwrap().count(),
            0,
            "an archive wrote outside the directory it was extracted into: {}",
            String::from_utf8_lossy(&extracted.stderr)
        );
        // And it FAILS rather than half-succeeding quietly — which matters because the restore runs
        // under `set -e`, so a tampered archive aborts the resize instead of half-restoring a box.
        assert!(
            !extracted.status.success(),
            "tar accepted an escaping archive silently: {}",
            String::from_utf8_lossy(&extracted.stderr)
        );
    }

    /// An archive is the size of the box, so keeping them is how a resize fills the host: measured,
    /// 16 GiB of boxes against 61 GiB free is two resizes before the host is full. It must go once
    /// its bytes are back — and *only* then, or a failed restore would delete the only copy.
    #[test]
    fn the_copy_is_deleted_once_it_is_back_and_never_before() {
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        // And the fleet root beside it, for the same reason: `util::fleet_root` refuses an
        // unpinned test rather than answering `/boxes`, which on any machine running skein is the
        // live fleet (SKEIN-690).
        std::env::set_var("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let archive = box_archive("web-main", "resize-x");
        let script = restore_script("web-main", &archive);

        let (Some(extract), Some(delete)) = (script.find("tar -C"), script.find("rm -f")) else {
            panic!("a restore must both extract and delete: {script}");
        };
        assert!(
            extract < delete,
            "the copy is deleted after the extraction, never before: {script}"
        );
        assert!(
            script.starts_with("set -e;"),
            "and only if the extraction succeeded — without `set -e` a failed tar still reaches \
             the rm, which would delete the only copy of a box that did not come back: {script}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A box holds files its own user cannot read, so both halves of the copy run as root.
    ///
    /// `tar` exits non-zero on the first file it cannot open, and under the `set -e` that makes the
    /// delete safe, that aborts the copy — so one unreadable file in one box refuses the whole
    /// fleet's resize. Which is exactly what happened: this project's own test suite had left 6,494
    /// directories under a box's `/tmp`, each holding one file at mode 000, and a resize of eight
    /// boxes stopped on the seventh with six already copied out.
    ///
    /// Root on the way back too, or the extract either fails on those same modes or quietly hands
    /// every file to whoever ran the resize — and ownership surviving is what makes this a copy
    /// rather than a rebuild.
    #[test]
    fn the_copy_runs_as_root_at_both_ends_because_a_box_is_not_all_readable_by_one_user() {
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        // And the fleet root beside it, for the same reason: `util::fleet_root` refuses an
        // unpinned test rather than answering `/boxes`, which on any machine running skein is the
        // live fleet (SKEIN-690).
        std::env::set_var("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let archive = box_archive("web-main", "resize-x");
        let out = archive_script("web-main", &archive);
        let back = restore_script("web-main", &archive);

        assert!(
            out.contains("sudo tar -C"),
            "an unreadable file anywhere in the box would abort the resize: {out}"
        );
        assert!(
            back.contains("sudo tar -C"),
            "the archive holds owners and modes an unprivileged extract cannot restore: {back}"
        );
        // Root wrote it, so the rest of the run — `du` here, the host reading it later — would
        // otherwise be touching a file it does not own.
        assert!(
            out.contains("sudo chown"),
            "an archive left owned by root is one the invoking user cannot clean up: {out}"
        );
        // Never `--ignore-failed-read`: that trades a resize that refused to start for a box
        // restored short of its own contents, with nobody told which files went missing.
        for script in [&out, &back] {
            assert!(
                !script.contains("ignore-failed-read"),
                "a copy that silently drops what it could not read is worse than one that stops: \
                 {script}"
            );
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// **A census that FAILED is not a fleet with no boxes in it** (SKEIN-347).
    ///
    /// `resize_fleet`'s own doc calls the ordering — copy every box out, only then destroy — "the
    /// entire safety property of this function". That property rests entirely on the list being
    /// complete, and the list came from `placed_boxes`, which cannot fail: `read_dir(&dir)
    /// .into_iter().flatten().flatten()` (src/place/record.rs:272) turns a read error into zero entries,
    /// and a record that will not parse is dropped. Both arrive as "no boxes", which is also what
    /// an empty fleet looks like — so a box skein could not enumerate was never archived, and then
    /// `sbx rm -f` took its VM-local checkout and its unpushed work with the sandbox.
    ///
    /// One truncated record is enough, and truncated records are manufactured by this codebase:
    /// `write_atomic` had no fsync, and a crash after its rename leaves a zero-length file.
    #[test]
    fn a_placement_record_that_will_not_parse_fails_the_census_rather_than_shrinking_it() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // **An empty fleet root of its own**, so the census's disk half agrees with an empty
        // `places` and this test is about the record half alone. It used to switch the deployment
        // off instead, which skipped the disk half entirely; there is no deployment to switch
        // (SKEIN-576), and unpinned the root is `/boxes` — the machine's live fleet (SKEIN-530).
        let root = tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", root.as_ref() as &std::path::Path);

        placed("web-main");
        placed("api-worker");
        assert_eq!(
            census_placed_boxes("skein-fleet")
                .unwrap()
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["api-worker", "web-main"],
            "the census does not find the boxes that are there"
        );

        // The crash artifact: present, zero-length, unparseable.
        let record = (home.as_ref() as &std::path::Path)
            .join("places")
            .join("api-worker.json");
        std::fs::write(&record, b"").unwrap();
        let why = census_placed_boxes("skein-fleet")
            .expect_err("a truncated placement record was read as one box fewer");
        assert!(
            why.contains("api-worker"),
            "the refusal has to name the record it could not read: {why}"
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A directory the census could not LIST is the same failure one level up, and the one that
    /// takes every box at once rather than one of them.
    ///
    /// EACCES on `~/.skein/places` is not exotic: a `chmod` in the wrong place, a store restored
    /// with somebody else's ownership, a mount that came back read-protected. `read_dir`'s error
    /// flattened to an empty iterator, so the answer was "this sandbox holds no boxes" and a resize
    /// destroyed all of them.
    #[test]
    fn a_places_directory_that_cannot_be_listed_fails_the_census() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // **An empty fleet root of its own**, so the census's disk half agrees with an empty
        // `places` and this test is about the record half alone. It used to switch the deployment
        // off instead, which skipped the disk half entirely; there is no deployment to switch
        // (SKEIN-576), and unpinned the root is `/boxes` — the machine's live fleet (SKEIN-530).
        let root = tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", root.as_ref() as &std::path::Path);

        placed("web-main");
        let places = (home.as_ref() as &std::path::Path).join("places");
        std::fs::set_permissions(&places, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root ignores the mode bits, so the fixture would not be unreadable and this test would
        // assert nothing at all. Asked rather than assumed — `geteuid` would need a dependency to
        // learn what one `read_dir` already says.
        let root_can_still_read = std::fs::read_dir(&places).is_ok();
        let refused = census_placed_boxes("skein-fleet");
        // Restored before the assertion, so a failure does not leave the tempdir guard unable to
        // clean up after itself — `testutil`'s own doc records what an unreadable leftover costs.
        std::fs::set_permissions(&places, std::fs::Permissions::from_mode(0o700)).unwrap();
        if root_can_still_read {
            // Cleared before the refusal, so the panic `skip` raises under
            // `$SKEIN_TESTS_NO_SKIP` unwinds with both variables already put back.
            std::env::remove_var("SKEIN_FLEET_ROOT");
            std::env::remove_var("SKEIN_HOME");
            crate::testutil::skip(
                "this run is root, which ignores the mode bits — the fixture is not unreadable \
                 here, so the census would not be refused and nothing would be proved",
            );
            return;
        }
        assert!(
            refused.is_err(),
            "a places directory that could not be listed read as a fleet with no boxes"
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A fleet that has never started a box **is** empty, and that has to keep working.
    ///
    /// The one reading of "no boxes" that is a fact rather than a failure. Written beside the
    /// refusals because a census that refused here would refuse the very first resize anybody runs.
    #[test]
    fn a_fleet_that_never_placed_a_box_takes_an_empty_census() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // **An empty fleet root of its own**, so the census's disk half agrees with an empty
        // `places` and this test is about the record half alone. It used to switch the deployment
        // off instead, which skipped the disk half entirely; there is no deployment to switch
        // (SKEIN-576), and unpinned the root is `/boxes` — the machine's live fleet (SKEIN-530).
        let root = tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", root.as_ref() as &std::path::Path);
        assert!(!(home.as_ref() as &std::path::Path).join("places").exists());
        assert!(census_placed_boxes("skein-fleet").unwrap().is_empty());
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The second, independent source of truth: **a checkout on disk that `places` never heard of.**
    ///
    /// The record checks above only catch a record skein could not READ. A record that was never
    /// written, or one deleted by hand, leaves a box with no evidence in `places` at all and
    /// nothing to raise an error about — the census is complete, consistent, and short by one box
    /// whose tree is about to be destroyed. In-fleet the box roots are local, so `fleet_root()` is
    /// a listing away and disagrees out loud.
    #[test]
    fn a_checkout_with_no_placement_record_stops_the_census() {
        let _g = env_lock();
        let home = tempdir();
        let root = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let root_dir = root.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_FLEET_ROOT", root_dir);

        placed("web-main");
        for name in ["web-main", "ghost-branch"] {
            std::fs::create_dir_all(root_dir.join(name).join("tree")).unwrap();
        }
        // Not a box: `.skein` holds the launcher and the probes and has no checkout in it.
        std::fs::create_dir_all(root_dir.join(".skein")).unwrap();

        let why = census_placed_boxes("skein-fleet")
            .expect_err("a checkout with no placement record was counted as nothing at all");
        assert!(
            why.contains("ghost-branch"),
            "the refusal has to name the checkout nothing would have carried out: {why}"
        );
        assert!(
            !why.contains("web-main") && !why.contains(".skein"),
            "only the unaccounted checkout belongs in the refusal: {why}"
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The two refusals above are only worth anything if the resize actually asks them.
    ///
    /// Read from the source for `the_login_tick_saves_the_fleets_copy_after_healing_it`'s reason,
    /// spelled out there: the alternative to reading the source is not a better test, it is no
    /// test. Driving `resize_fleet_inner` for real means `sbx rm -f` against a live fleet, which is
    /// the one thing this work must not do — so what can be checked is that the destructive
    /// function reaches for the failing census and the failing capture rather than the silent ones
    /// beside them, and that it does so before the destroy.
    #[test]
    fn a_resize_takes_the_census_that_can_refuse_and_takes_it_before_the_destroy() {
        let body = fn_body(include_str!("resize.rs"), "fn resize_fleet_inner(");
        assert!(
            body.contains("census_placed_boxes(&sandbox)"),
            "the resize is back on a census that cannot fail loudly"
        );
        // `census_placed_boxes` ENDS in `placed_boxes`, so the naive search matches the fix and
        // the bug alike. The name is taken out of the text before asking after the bare call.
        assert!(
            !body
                .replace("census_placed_boxes", "the-census")
                .contains("placed_boxes("),
            "the resize still enumerates boxes with the census that reads an error as zero boxes"
        );
        assert!(
            body.contains("capture_fleet_login(&sandbox)"),
            "the resize is back on a login capture that reads a failed exec as no login"
        );
        let census = body.find("census_placed_boxes(&sandbox)").unwrap();
        let capture = body.find("capture_fleet_login(&sandbox)").unwrap();
        // The destroy goes through the warden-or-prompt rule now (`Act::Destroy`, SKEIN-455) rather
        // than calling `Warden::destroy` directly, so the needle moved. What is asserted did not:
        // the census and the login capture, both of which can still refuse, must come first.
        let destroy = body
            .find("Act::Destroy")
            .expect("the resize no longer destroys the sandbox here — re-read this test");
        assert!(
            census < destroy && capture < destroy,
            "the destroy runs before something that can still refuse it"
        );
    }

    /// **Nothing survives the destroy, so nothing is written as though it might** (SKEIN-679).
    ///
    /// Read from the source for the reason the test above gives: driving `resize_fleet_inner` for
    /// real means `sbx rm -f` against a live fleet. What was deleted is everything the process was
    /// going to do after the machine it runs on had gone — `forget_place`, the [`ensure_fleet`]
    /// rebuild, the restore-and-restart loop, the list of boxes that did not come back — and, with
    /// them, the sentence that told a person "`skein resize 8g` is safe to re-run — creating the
    /// sandbox is idempotent, so it retries only the step that failed". Both halves of that were
    /// false: there is no skein left to run it in, and `ensure_fleet` does not create.
    ///
    /// **What would make this fail**: putting any of them back. Proved by restoring the
    /// `ensure_fleet(&sandbox)` call, which fired the first assertion.
    #[test]
    fn the_resize_stops_at_the_destroy_and_promises_nothing_beyond_it() {
        let body = fn_body(include_str!("resize.rs"), "fn resize_fleet_inner(");
        for gone in [
            "ensure_fleet(",
            "restore_box(",
            "start_box(",
            "forget_place(",
        ] {
            assert!(
                !body.contains(gone),
                "the resize reaches {gone} after a destroy that ends this process: {body}"
            );
        }
        assert!(
            !body.contains("safe to re-run") && !body.contains("idempotent"),
            "the resize still promises a retry that has nothing left to run it: {body}"
        );
        // The destroy is the last thing that can happen, so what follows it is an unconditional
        // success and not a report about boxes nobody put back.
        let destroy = body
            .find("Act::Destroy")
            .expect("the resize no longer destroys");
        assert!(
            body[destroy..].trim_end().ends_with("Ok(())"),
            "something still runs after the destroy: {}",
            &body[destroy..]
        );
    }
}
