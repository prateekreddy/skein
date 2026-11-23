//! `skein migrate receive`: restore a backup into an empty directory, resuming if run again.
//!
//! Each unit is extracted into a stage inside the target and moved into place only after its hash
//! holds. The receipt records finished units, so a rerun steps over them without reading them.

use super::available_kb;
use super::bundle::{
    extract, hex, set_mtime, BundleReader, Closing, DirMeta, Manifest, Stop, Trailer, UnitHead,
    FORMAT, KIND_END, KIND_MANIFEST, KIND_UNIT,
};
use super::plan::plain;
use super::send::{docker_text, left_out_text};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufReader, Read, Seek};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Where a restore keeps its own state, beside `fleet/` and `home/`.
const WORK_DIR: &str = ".skein-receive";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Done {
    index: u64,
    name: String,
    sha256: String,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct Receipt {
    bundle: String,
    done: Vec<Done>,
    /// The unit being moved into place, so a crash there can finish it on the next run.
    publishing: Option<u64>,
    complete: bool,
}

fn receipt_path(into: &Path) -> PathBuf {
    into.join(WORK_DIR).join("receipt.json")
}

fn save_receipt(into: &Path, receipt: &Receipt) -> Result<(), String> {
    let path = receipt_path(into);
    let next = path.with_extension("json.next");
    let text = serde_json::to_vec(receipt).map_err(|e| e.to_string())?;
    fs::write(&next, text).map_err(|e| format!("writing {}: {e}", next.display()))?;
    fs::rename(&next, &path).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// A restore killed before its first receipt reached its name leaves only its own directory, and
/// at most that unfinished receipt. Nothing of the backup is in place, so it starts again.
fn never_started(into: &Path) -> bool {
    let names = |dir: &Path| -> Option<Vec<std::ffi::OsString>> {
        fs::read_dir(dir)
            .ok()?
            .map(|entry| entry.ok().map(|e| e.file_name()))
            .collect()
    };
    let work = into.join(WORK_DIR);
    let unfinished = receipt_path(into).with_extension("json.next");
    names(into).is_some_and(|found| found == [WORK_DIR])
        && fs::symlink_metadata(&work).is_ok_and(|m| m.is_dir())
        && names(&work).is_some_and(|found| {
            found
                .iter()
                .all(|name| Some(name.as_os_str()) == unfinished.file_name())
        })
}

/// The receipt to continue from, after refusing a target this bundle did not start.
fn open_target(into: &Path, bundle: &str) -> Result<Receipt, String> {
    match fs::symlink_metadata(into) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "{} is not a plain directory, so nothing was restored.",
                into.display()
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(into).map_err(|e| format!("creating {}: {e}", into.display()))?;
        }
        Err(e) => return Err(format!("reading {}: {e}", into.display())),
    }
    let empty = fs::read_dir(into)
        .map_err(|e| format!("listing {}: {e}", into.display()))?
        .next()
        .is_none();
    if empty || never_started(into) {
        if empty {
            fs::create_dir(into.join(WORK_DIR))
                .map_err(|e| format!("preparing the target: {e}"))?;
        }
        let receipt = Receipt {
            bundle: bundle.to_string(),
            ..Receipt::default()
        };
        save_receipt(into, &receipt)?;
        return Ok(receipt);
    }
    let not_ours = || {
        format!(
            "{} is not empty and holds no restore of this backup, so nothing was restored. Name \
             an empty directory.",
            into.display()
        )
    };
    if !fs::symlink_metadata(into.join(WORK_DIR)).is_ok_and(|m| m.is_dir()) {
        return Err(not_ours());
    }
    let text = fs::read(receipt_path(into)).map_err(|_| not_ours())?;
    let receipt: Receipt = serde_json::from_slice(&text).map_err(|_| not_ours())?;
    if receipt.bundle != bundle {
        return Err(format!(
            "{} holds part of a different backup, so nothing was restored. Name an empty \
             directory.",
            into.display()
        ));
    }
    Ok(receipt)
}

/// Move a staged tree into the target. `again` allows replacing what an interrupted publish of
/// this same unit already moved.
fn merge(from: &Path, to: &Path, again: bool) -> Result<(), String> {
    let from_meta =
        fs::symlink_metadata(from).map_err(|e| format!("reading {}: {e}", from.display()))?;
    let to_meta = match fs::symlink_metadata(to) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return fs::rename(from, to).map_err(|e| format!("placing {}: {e}", to.display()));
        }
        Err(e) => return Err(format!("reading {}: {e}", to.display())),
    };
    if from_meta.is_dir() && to_meta.is_dir() {
        if again {
            let mode = to_meta.permissions().mode() | 0o700;
            fs::set_permissions(to, fs::Permissions::from_mode(mode))
                .map_err(|e| format!("reopening {}: {e}", to.display()))?;
        }
        let names = fs::read_dir(from).map_err(|e| format!("listing {}: {e}", from.display()))?;
        for name in names {
            let name = name
                .map_err(|e| format!("listing {}: {e}", from.display()))?
                .file_name();
            merge(&from.join(&name), &to.join(&name), again)?;
        }
        return Ok(());
    }
    if again && !from_meta.is_dir() && !to_meta.is_dir() {
        return fs::rename(from, to).map_err(|e| format!("placing {}: {e}", to.display()));
    }
    Err(format!(
        "refused {}: two units of this backup claim it",
        to.display()
    ))
}

fn apply_dirs(into: &Path, mut dirs: Vec<DirMeta>) -> Result<(), String> {
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.path.components().count()));
    for dir in dirs {
        let path = into.join(&dir.path);
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            return Err(format!("{} is no longer a directory", path.display()));
        }
        set_mtime(&path, dir.mtime).map_err(|e| format!("restoring {}: {e}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(dir.mode))
            .map_err(|e| format!("restoring {}: {e}", path.display()))?;
    }
    Ok(())
}

fn clear_stage(stage: &Path) -> Result<(), String> {
    match fs::remove_dir_all(stage) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("clearing {}: {e}", stage.display())),
    }
}

/// Take one unit from the bundle into the target.
fn take_unit<R: Read + Seek>(
    reader: &mut BundleReader<R>,
    into: &Path,
    index: u64,
    name: &str,
    what: &str,
    receipt: &mut Receipt,
) -> Result<Trailer, Stop> {
    let stage = into.join(WORK_DIR).join("stage");
    clear_stage(&stage).map_err(Stop::Bad)?;
    fs::create_dir(&stage).map_err(|e| Stop::Bad(format!("preparing the stage: {e}")))?;
    let mut data = reader.unit(what);
    let extracted = extract(&mut data, &stage, what);
    let (entries, dirs) = match extracted {
        Ok(done) => done,
        Err(stop) => {
            let _ = clear_stage(&stage);
            return Err(stop);
        }
    };
    let trailer = match data.verify(entries) {
        Ok(trailer) => trailer,
        Err(stop) => {
            let _ = clear_stage(&stage);
            return Err(stop);
        }
    };
    let again = receipt.publishing == Some(index);
    receipt.publishing = Some(index);
    save_receipt(into, receipt).map_err(Stop::Bad)?;
    for top in ["fleet", "home"] {
        let staged = stage.join(top);
        if fs::symlink_metadata(&staged).is_ok() {
            merge(&staged, &into.join(top), again).map_err(Stop::Bad)?;
        }
    }
    apply_dirs(into, dirs).map_err(Stop::Bad)?;
    clear_stage(&stage).map_err(Stop::Bad)?;
    receipt.publishing = None;
    receipt.done.push(Done {
        index,
        name: name.to_string(),
        sha256: trailer.sha256.clone(),
    });
    save_receipt(into, receipt).map_err(Stop::Bad)?;
    Ok(trailer)
}

fn parse<T: serde::de::DeserializeOwned>(payload: &[u8], what: &str) -> Result<T, Stop> {
    serde_json::from_slice(payload).map_err(|_| Stop::Bad(format!("{what} cannot be read")))
}

struct Outcome {
    taken: usize,
    skipped: usize,
    closing: Closing,
}

fn walk_units<R: Read + Seek>(
    reader: &mut BundleReader<R>,
    manifest: &Manifest,
    into: &Path,
    receipt: &mut Receipt,
    finished: &mut usize,
) -> Result<Outcome, Stop> {
    let (mut taken, mut skipped) = (0, 0);
    let mut seen = Vec::new();
    for (index, line) in manifest.units.iter().enumerate() {
        let index = index as u64;
        let what = format!("unit {} ({})", index + 1, plain(&line.name));
        let (payload, _) = reader.frame(KIND_UNIT, &what)?;
        let head: UnitHead = parse(&payload, &what)?;
        if head.index != index || head.name != line.name {
            return Err(Stop::Bad(format!("{what} is out of place in this file")));
        }
        let held = receipt.done.iter().find(|d| d.index == index).cloned();
        let trailer = if let Some(held) = held {
            let trailer = reader.skip_unit(&what)?;
            if held.sha256 != trailer.sha256 || held.name != line.name {
                return Err(Stop::Bad(format!(
                    "{what} differs from the copy already restored"
                )));
            }
            skipped += 1;
            trailer
        } else {
            let trailer = take_unit(reader, into, index, &line.name, &what, receipt)?;
            taken += 1;
            trailer
        };
        *finished += 1;
        seen.push((line.name.clone(), trailer));
    }
    let (payload, _) = reader.frame(KIND_END, "the closing manifest")?;
    let closing: Closing = parse(&payload, "the closing manifest")?;
    let agrees = closing.units.len() == seen.len()
        && closing.units.iter().zip(&seen).all(|(unit, (name, t))| {
            unit.name == *name
                && unit.sha256 == t.sha256
                && unit.files == t.files
                && unit.bytes == t.bytes
        });
    if !agrees {
        return Err(Stop::Bad(
            "the closing manifest disagrees with the units in this file".into(),
        ));
    }
    Ok(Outcome {
        taken,
        skipped,
        closing,
    })
}

/// A restored volume belongs to no fleet until a person takes it over. It gets a new cockpit token
/// of its own, because the old one is never in a backup and a take-over needs a signed-in page.
fn prepare_for_take_over(home: &Path) -> Result<(), String> {
    fs::create_dir_all(home).map_err(|e| format!("creating {}: {e}", home.display()))?;
    crate::fence::mark_restored(home)?;
    crate::apiauth::mint_at(home)
}

/// Restore the backup at `file` into `into`, which must be empty or hold this backup's own
/// unfinished restore.
pub fn migrate_receive(file: &str, into: &str) -> Result<String, String> {
    if super::seal::is_sealed(Path::new(file)) {
        return Err(format!(
            "{file} is sealed, so nothing was restored: run `skein migrate unseal <sealed-file> \
             <bundle>` and receive the bundle it writes."
        ));
    }
    let into = Path::new(into);
    let bundle = fs::File::open(file).map_err(|e| format!("opening {file}: {e}"))?;
    let size = bundle.metadata().map_or(0, |m| m.len());
    let flatten = |stop: Stop| match stop {
        Stop::Cut(why) | Stop::Bad(why) => format!("{file}: {why}. Nothing was restored."),
    };
    let mut reader =
        BundleReader::open(BufReader::with_capacity(64 * 1024, bundle)).map_err(flatten)?;
    let (payload, digest) = reader
        .frame(KIND_MANIFEST, "the manifest")
        .map_err(flatten)?;
    let manifest: Manifest = parse(&payload, "the manifest").map_err(flatten)?;
    if manifest.format != FORMAT {
        return Err(format!(
            "{file}: its manifest says format {} and this skein reads format {FORMAT}. Nothing \
             was restored.",
            manifest.format
        ));
    }
    let id = hex(&digest);
    let parent = into
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let probe = if into.exists() { into } else { parent };
    let mut receipt = open_target(into, &id)?;
    let need_kb = size.div_ceil(1024);
    if receipt.done.is_empty() {
        match available_kb(probe) {
            Some(free) if free < need_kb => {
                return Err(format!(
                    "restoring needs about {need_kb} KB and {} has {free} KB free. Nothing was \
                     restored.",
                    probe.display()
                ))
            }
            Some(_) => {}
            None => {
                return Err(format!(
                    "the free space at {} could not be read. Nothing was restored.",
                    probe.display()
                ))
            }
        }
    }

    // Before the first unit lands, so a restore killed partway is never a volume a fleet adopts.
    prepare_for_take_over(&into.join("home"))?;

    let total = manifest.units.len();
    let mut finished = 0;
    match walk_units(&mut reader, &manifest, into, &mut receipt, &mut finished) {
        Ok(outcome) => {
            receipt.complete = true;
            save_receipt(into, &receipt)?;
            Ok(format!(
                "Restored into {}\n{total} of {total} unit(s) verified by SHA-256 ({} restored \
                 now, {} already done).\nThe restored fleet is in fleet/ and skein's state in \
                 home/. It was not started, and nothing outside this directory was written.\n\
                 A fleet started on it waits for you to press Take over in its cockpit, so only do \
                 that once the fleet it came from is stopped.\n\
                 Git worktrees here still point at their original paths, so do not run git \
                 inside one until it is back at that path: it would act on the original.\n\n\
                 NOT in this backup:\n  {}\n{}Credentials were never in it, so sign in again \
                 after moving to it.",
                into.display(),
                outcome.taken,
                outcome.skipped,
                docker_text(&manifest.docker),
                left_out_text(&outcome.closing.left_out),
            ))
        }
        Err(Stop::Cut(why)) => Err(format!(
            "{file} is incomplete: {why}. {finished} of {total} unit(s) are verified and in \
             place in {}. Run this again with a complete copy of the same backup and it \
             continues from there.\n{}",
            into.display(),
            docker_text(&manifest.docker),
        )),
        Err(Stop::Bad(why)) => Err(format!(
            "refused: {why}. {finished} of {total} unit(s) before it are verified and in place in \
             {}. The damaged unit was discarded.",
            into.display()
        )),
    }
}
