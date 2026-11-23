//! Which fleet may run the boxes on this volume.
//!
//! Each fleet has a VM-local id at `<fleet root>/.skein/fleet-id`. The volume names its owner in
//! `fleet-owner`, a JSON record of `fleet_id`, `epoch` and `machine`.
//!
//! **The commit point is one `symlink(2)`**, at `fleet-owner.d/<epoch>`. It is exclusive, and it
//! carries its whole content, so two claimants of one epoch have one winner and no claim can be
//! torn. The highest epoch is the owner. `fleet-owner` is replaced by rename after the claim and
//! is healed from it, so a kill between the two steps still leaves one answer.
//!
//! Adoption and a move are the same act: a claim of the next epoch, from no owner or from the one
//! the caller expects.

use crate::util::{fleet_root, in_test, write_atomic};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const OWNER_FILE: &str = "fleet-owner";
const CLAIMS_DIR: &str = "fleet-owner.d";
const FLEET_ID_FILE: &str = "fleet-id";
/// Written by `bootstrap.sh` in receive mode. While it is here this fleet never adopts a volume.
const RECEIVE_MARKER: &str = "receive-mode";
/// How `skein fleet-owner check` reports a fence, so `bootstrap.sh` can tell it from anything else.
pub const FENCED_EXIT: i32 = 3;

/// Inside `.skein/private`, while this fleet's server may not write its log to the volume.
const LOG_HOLD: &str = "fenced";

/// The fleet a volume belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub fleet_id: String,
    pub epoch: u64,
    pub machine: String,
}

/// The points of a flip a process can die between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    BeforeClaim,
    Claimed,
    Recorded,
}

impl Step {
    pub const ALL: [Step; 3] = [Step::BeforeClaim, Step::Claimed, Step::Recorded];
}

/// Why a flip did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum FlipError {
    /// The volume is not held by the owner the caller expected.
    Lost(Option<Owner>),
    /// The volume could not be read or written.
    Failed(String),
}

fn token_ok(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn claim_text(owner: &Owner) -> String {
    format!("v1:{}:{}", owner.fleet_id, owner.machine)
}

fn parse_claim(epoch: u64, text: &str) -> Option<Owner> {
    let mut parts = text.strip_prefix("v1:")?.split(':');
    let (fleet_id, machine) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && token_ok(fleet_id) && token_ok(machine)).then(|| Owner {
        fleet_id: fleet_id.to_string(),
        epoch,
        machine: machine.to_string(),
    })
}

fn highest_claim(home: &Path) -> Result<Option<Owner>, String> {
    let dir = home.join(CLAIMS_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading {}: {e}", dir.display())),
    };
    let mut best: Option<Owner> = None;
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
        let Some(epoch) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        if best.as_ref().is_some_and(|b| b.epoch >= epoch) {
            continue;
        }
        let path = entry.path();
        let target =
            fs::read_link(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        best = Some(
            target
                .to_str()
                .and_then(|text| parse_claim(epoch, text))
                .ok_or_else(|| format!("{} is not an owner claim", path.display()))?,
        );
    }
    Ok(best)
}

/// The volume's owner, or `None` on a volume nothing has adopted.
pub fn read_owner(home: &Path) -> Result<Option<Owner>, String> {
    let claimed = highest_claim(home)?;
    let path = home.join(OWNER_FILE);
    let recorded = match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Owner>(&text) {
            Ok(owner) => Some(owner),
            // A claim outranks a record, so a damaged record is only fatal when it is all there is.
            Err(_) if claimed.is_some() => None,
            Err(e) => return Err(format!("{} is not an owner record: {e}", path.display())),
        },
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    Ok(match (claimed, recorded) {
        (Some(c), Some(r)) => Some(if r.epoch > c.epoch { r } else { c }),
        (c, r) => c.or(r),
    })
}

fn write_record(home: &Path, owner: &Owner) -> Result<(), String> {
    let json = serde_json::to_vec(owner).map_err(|e| format!("encoding the owner: {e}"))?;
    write_atomic(&home.join(OWNER_FILE), home, &json)
}

/// Bring `fleet-owner` into line with the claim that decided it.
fn heal_record(home: &Path, owner: &Owner) {
    let current = fs::read_to_string(home.join(OWNER_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Owner>(&text).ok());
    if current.as_ref() != Some(owner) {
        let _ = write_record(home, owner);
    }
}

/// Take the epoch for `owner`. `false` means another claimant already holds it.
fn claim_epoch(home: &Path, owner: &Owner) -> Result<bool, String> {
    let dir = home.join(CLAIMS_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let path = dir.join(owner.epoch.to_string());
    match std::os::unix::fs::symlink(claim_text(owner), &path) {
        Ok(()) => {
            if let Ok(handle) = fs::File::open(&dir) {
                let _ = handle.sync_all();
            }
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("claiming {}: {e}", path.display())),
    }
}

/// Make `to` the owner, if the volume is still held by `expected`.
///
/// A fleet that already owns the volume gets `Ok`, which is what makes a killed flip resumable.
pub fn flip(
    home: &Path,
    expected: Option<&Owner>,
    to: &str,
    machine: &str,
) -> Result<Owner, FlipError> {
    flip_observed(home, expected, to, machine, &mut |_| {})
}

/// [`flip`], calling `step` at each point a process can die.
pub fn flip_observed(
    home: &Path,
    expected: Option<&Owner>,
    to: &str,
    machine: &str,
    step: &mut dyn FnMut(Step),
) -> Result<Owner, FlipError> {
    if !token_ok(to) || !token_ok(machine) {
        return Err(FlipError::Failed(format!(
            "{to:?} on {machine:?} cannot be recorded as an owner"
        )));
    }
    let current = read_owner(home).map_err(FlipError::Failed)?;
    if let Some(held) = current.as_ref().filter(|held| held.fleet_id == to) {
        heal_record(home, held);
        return Ok(held.clone());
    }
    if current.as_ref() != expected {
        return Err(FlipError::Lost(current));
    }
    let next = Owner {
        fleet_id: to.to_string(),
        epoch: current.as_ref().map_or(0, |held| held.epoch) + 1,
        machine: machine.to_string(),
    };
    step(Step::BeforeClaim);
    if !claim_epoch(home, &next).map_err(FlipError::Failed)? {
        return match read_owner(home).map_err(FlipError::Failed)? {
            Some(held) if held.fleet_id == to => Ok(held),
            other => Err(FlipError::Lost(other)),
        };
    }
    step(Step::Claimed);
    // The claim has decided it. A record that cannot be written is healed by the next reader.
    let _ = write_record(home, &next);
    step(Step::Recorded);
    Ok(next)
}

/// This process's `<fleet root>/.skein`. `None` in a test that has not pinned a fleet root.
fn fleet_dir() -> Option<PathBuf> {
    let pinned = std::env::var_os("SKEIN_FLEET_ROOT").is_some_and(|root| !root.is_empty());
    (pinned || !in_test()).then(|| PathBuf::from(fleet_root()).join(".skein"))
}

fn fleet_id_in(dot: &Path) -> Option<String> {
    let text = fs::read_to_string(dot.join(FLEET_ID_FILE)).ok()?;
    let id = text.trim();
    token_ok(id).then(|| id.to_string())
}

/// This fleet's id, minted once. Linked into place, so two first starts agree on one.
fn ensure_fleet_id(dot: &Path) -> Result<String, String> {
    if let Some(id) = fleet_id_in(dot) {
        return Ok(id);
    }
    let id = format!("fleet-{:016x}", crate::util::unguessable());
    let temp = dot.join(format!(".fleet-id.{id}"));
    fs::write(&temp, format!("{id}\n")).map_err(|e| format!("writing {}: {e}", temp.display()))?;
    let linked = fs::hard_link(&temp, dot.join(FLEET_ID_FILE));
    let _ = fs::remove_file(&temp);
    match linked {
        Ok(()) => Ok(id),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => fleet_id_in(dot)
            .ok_or_else(|| format!("{} is unreadable", dot.join(FLEET_ID_FILE).display())),
        Err(e) => Err(format!("recording this fleet's id: {e}")),
    }
}

/// A label for the owner record. Identity is the fleet id; this is what a person reads.
fn this_machine() -> String {
    let name: String = fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name
    }
}

fn fenced(home: &Path, dot: &Path, owner: &Owner) -> String {
    format!(
        "this fleet does not own the volume at {home}.\n\
         It belongs to fleet {id} on {machine} (epoch {epoch}); this fleet is {mine}. Two fleets \
         on one volume would run the same boxes, so nothing was started.\n\
         If that fleet is still running, use it, or stop it first.\n\
         If it is gone, for example because this sandbox was rebuilt and lost its id, press Take \
         over this fleet in the cockpit, or run this inside this sandbox and start skein again:\n  \
         {skein} fleet-owner claim {id}",
        home = home.display(),
        id = owner.fleet_id,
        machine = owner.machine,
        epoch = owner.epoch,
        mine = fleet_id_in(dot).unwrap_or_else(|| "new and has no id yet".to_string()),
        skein = dot.join("skein").display(),
    )
}

fn receiving(home: &Path, dot: &Path) -> String {
    format!(
        "this sandbox was installed in receive mode, so it does not use the volume at {} until a \
         move makes it the owner.\n\
         If you meant it to be a normal install, remove {} and run bootstrap.sh again.",
        home.display(),
        dot.join(RECEIVE_MARKER).display()
    )
}

fn unreadable(home: &Path, why: &str) -> String {
    format!(
        "cannot tell which fleet owns the volume at {}: {why}.\n\
         Nothing was started. If no other fleet is using this volume, remove that file and start \
         skein again; the first fleet to start adopts it.",
        home.display()
    )
}

/// Refuse if this fleet is not the volume's owner.
///
/// With `adopt`, a volume nothing owns becomes this fleet's. A failure to adopt is a warning: a
/// volume with no owner keeps working as it did before fences existed. A box never adopts, because
/// what it sees at `$SKEIN_HOME` is its own cover.
pub fn ensure_owner(home: &Path, adopt: bool) -> Result<(), String> {
    let Some(dot) = fleet_dir() else {
        return Ok(());
    };
    if !home.is_dir() {
        return Ok(());
    }
    check(home, &dot, adopt)
}

fn check(home: &Path, dot: &Path, adopt: bool) -> Result<(), String> {
    let in_receive_mode = dot.join(RECEIVE_MARKER).exists();
    match read_owner(home).map_err(|why| unreadable(home, &why))? {
        None if in_receive_mode => Err(receiving(home, dot)),
        None => {
            let box_process = std::env::var_os("SKEIN_BOX").is_some_and(|b| !b.is_empty());
            if adopt && dot.is_dir() && !box_process {
                return adopt_volume(home, dot);
            }
            Ok(())
        }
        Some(owner) if fleet_id_in(dot).as_deref() == Some(owner.fleet_id.as_str()) => {
            heal_record(home, &owner);
            let _ = fs::remove_file(dot.join(RECEIVE_MARKER));
            Ok(())
        }
        Some(owner) => Err(fenced(home, dot, &owner)),
    }
}

fn adopt_volume(home: &Path, dot: &Path) -> Result<(), String> {
    let id = match ensure_fleet_id(dot) {
        Ok(id) => id,
        Err(why) => {
            eprintln!("skein: this fleet did not adopt its volume yet ({why})");
            return Ok(());
        }
    };
    match flip(home, None, &id, &this_machine()) {
        Ok(_) => Ok(()),
        Err(FlipError::Lost(Some(owner))) => Err(fenced(home, dot, &owner)),
        Err(FlipError::Lost(None)) => Ok(()),
        Err(FlipError::Failed(why)) => {
            eprintln!("skein: this fleet did not adopt its volume yet ({why})");
            Ok(())
        }
    }
}

/// `skein fleet-owner claim <id>`: take the volume from the fleet named, which must be its owner.
///
/// The id is what makes this deliberate. A line copied from an old refusal cannot take the volume
/// from a fleet that has claimed it since.
pub fn claim(home: &Path, from: &str) -> Result<String, String> {
    let dot = fleet_dir().ok_or("no fleet root is set")?;
    let owner = read_owner(home)
        .map_err(|why| unreadable(home, &why))?
        .ok_or_else(|| {
            format!(
                "the volume at {} has no owner, so there is nothing to claim. The next fleet to \
                 start adopts it.",
                home.display()
            )
        })?;
    if owner.fleet_id != from {
        return Err(format!(
            "the volume at {} belongs to {}, not {from}, so nothing changed. Check that fleet is \
             gone before claiming it.",
            home.display(),
            owner.fleet_id
        ));
    }
    fs::create_dir_all(&dot).map_err(|e| format!("mkdir {}: {e}", dot.display()))?;
    let mine = ensure_fleet_id(&dot)?;
    let now = flip(home, Some(&owner), &mine, &this_machine()).map_err(|e| match e {
        FlipError::Lost(Some(held)) => format!(
            "fleet {} claimed the volume first, so this fleet did not",
            held.fleet_id
        ),
        FlipError::Lost(None) => "the volume no longer has an owner".to_string(),
        FlipError::Failed(why) => why,
    })?;
    let _ = fs::remove_file(dot.join(RECEIVE_MARKER));
    Ok(format!(
        "this fleet ({}) now owns {} at epoch {}. Fleet {from} is refused from here on.",
        now.fleet_id,
        home.display(),
        now.epoch
    ))
}

/// What this fleet is to the volume, read without writing anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// No owner yet, or no fleet here to compare.
    Free,
    Owner(Owner),
    /// Another fleet owns the volume.
    Fenced(Owner),
    /// In receive mode, and not the owner.
    Receiving,
    Unreadable(String),
}

/// This process's standing on the volume at `home`.
pub fn standing(home: &Path) -> Standing {
    let Some(dot) = fleet_dir() else {
        return Standing::Free;
    };
    if !home.is_dir() {
        return Standing::Free;
    }
    match read_owner(home) {
        Err(why) => Standing::Unreadable(why),
        Ok(Some(owner)) if fleet_id_in(&dot).as_deref() == Some(owner.fleet_id.as_str()) => {
            Standing::Owner(owner)
        }
        Ok(_) if dot.join(RECEIVE_MARKER).exists() => Standing::Receiving,
        Ok(Some(owner)) => Standing::Fenced(owner),
        Ok(None) => Standing::Free,
    }
}

/// This fleet's id, if it has one. Never mints.
pub fn fleet_id_here() -> Option<String> {
    fleet_id_in(&fleet_dir()?)
}

/// The CLI line that does what the cockpit's button does.
pub fn claim_command(owner: &Owner) -> Option<String> {
    Some(format!(
        "{} fleet-owner claim {}",
        fleet_dir()?.join("skein").display(),
        owner.fleet_id
    ))
}

/// Stop the log reader writing to the volume. VM-local, and written before the server prints.
pub fn hold_log() {
    if let Some(dot) = fleet_dir() {
        let private = dot.join("private");
        if fs::create_dir_all(&private).is_ok() {
            let _ = fs::write(private.join(LOG_HOLD), "");
        }
    }
}

/// Let the log reader write to the volume again.
pub fn release_log() {
    if let Some(dot) = fleet_dir() {
        let _ = fs::remove_file(dot.join("private").join(LOG_HOLD));
    }
}

/// What a take-over did.
#[derive(Debug, PartialEq, Eq)]
pub enum Took {
    /// This fleet is now the owner.
    Now(Owner),
    /// It already was, so nothing changed.
    Already(Owner),
}

/// Why a take-over was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum TakeOverError {
    /// The volume is no longer held by the owner the person was shown.
    Stale(Option<Owner>),
    Failed(String),
}

/// Take the volume from the owner the person was shown, and only from that owner.
pub fn take_over(home: &Path, fleet_id: &str, epoch: u64) -> Result<Took, TakeOverError> {
    let dot = fleet_dir().ok_or_else(|| TakeOverError::Failed("no fleet root is set".into()))?;
    let current = read_owner(home).map_err(TakeOverError::Failed)?;
    let Some(owner) = current else {
        return Err(TakeOverError::Stale(None));
    };
    if fleet_id_in(&dot).as_deref() == Some(owner.fleet_id.as_str()) {
        return Ok(Took::Already(owner));
    }
    if owner.fleet_id != fleet_id || owner.epoch != epoch {
        return Err(TakeOverError::Stale(Some(owner)));
    }
    fs::create_dir_all(&dot).map_err(|e| TakeOverError::Failed(format!("{e}")))?;
    let mine = ensure_fleet_id(&dot).map_err(TakeOverError::Failed)?;
    let now = flip(home, Some(&owner), &mine, &this_machine()).map_err(|e| match e {
        FlipError::Lost(held) => TakeOverError::Stale(held),
        FlipError::Failed(why) => TakeOverError::Failed(why),
    })?;
    let _ = fs::remove_file(dot.join(RECEIVE_MARKER));
    Ok(Took::Now(now))
}

/// The owner a restored volume names until a person takes it over. No fleet has this id, so every
/// fleet is fenced from it, and the fleet the backup came from may still be running.
pub const RESTORED_FLEET: &str = "restored-backup";
const RESTORED_MACHINE: &str = "a-backup";

/// Make a restored volume wait for a take-over. Safe to repeat on an interrupted restore.
pub fn mark_restored(home: &Path) -> Result<(), String> {
    // `flip` answers Ok when the volume already names this owner, which is the repeat.
    match flip(home, None, RESTORED_FLEET, RESTORED_MACHINE) {
        Ok(_) => Ok(()),
        Err(FlipError::Lost(_)) => Err("this restore already has another owner".to_string()),
        Err(FlipError::Failed(why)) => Err(why),
    }
}

/// One line for `skein doctor` and `skein fleet-owner`.
pub fn describe(home: &Path) -> String {
    let mine = fleet_dir().and_then(|dot| fleet_id_in(&dot));
    match read_owner(home) {
        Err(why) => format!("unreadable: {why}"),
        Ok(None) => "no owner yet; the next fleet to start adopts this volume".to_string(),
        Ok(Some(owner)) if mine.as_deref() == Some(owner.fleet_id.as_str()) => {
            format!("this fleet ({}, epoch {})", owner.fleet_id, owner.epoch)
        }
        Ok(Some(owner)) => format!(
            "FENCED: owned by {} on {} (epoch {}); this fleet is {}. To take it over once that \
             fleet is gone: skein fleet-owner claim {}",
            owner.fleet_id,
            owner.machine,
            owner.epoch,
            mine.as_deref().unwrap_or("new and has no id yet"),
            owner.fleet_id
        ),
    }
}
