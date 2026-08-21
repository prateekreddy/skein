//! The durable volume — its schema version, and moving one.
//!
//! §5 of `docs/architecture.md`: the volume is the only thing that persists, and everything else is
//! reconstructible. `$SKEIN_HOME` was already a single relocatable root, which is what makes the
//! move itself close to a mount and an environment variable. This module is the rest of it — the
//! part that is not the move.
//!
//! **What the version covers, and what it deliberately does not.** `VERSION` is a statement about
//! the volume's *declared* files — `config.json`, `repos.json`, `places/`, `declared/`, the grants
//! and the package manifest. It is **not** a statement about the recorded ones: per-box status and
//! pane JSON are written by shell probes generated from the binary, so versioning those would tie
//! probe version to volume schema to binary version, three things that would then have to move
//! together. A probe writing a shape the reader does not know is already handled where it belongs,
//! by readers that treat an unparseable observation as absent.
//!
//! **The direction that matters is forward.** A volume written by a *newer* skein is refused
//! outright: half-reading it is how a fleet loses the fields it does not understand at the next
//! write. A volume written by an older one is adopted — there has only ever been one schema, so
//! the older-than branch is written for the day there are two.

use crate::config::skein_home;
use crate::util::write_atomic;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The volume layout this binary understands.
pub const SCHEMA: u32 = 1;

/// `VERSION` on the volume.
fn version_path(home: &Path) -> PathBuf {
    home.join("VERSION")
}

/// Written into the target before a copy starts and removed when it finishes, so that a move
/// interrupted halfway is a directory that says so rather than one that looks complete.
fn migrating_path(home: &Path) -> PathBuf {
    home.join("MIGRATING")
}

/// Left in the *source* after a successful move.
///
/// Not a pointer skein follows — following it would give "where is the volume" two answers, and the
/// whole point of `$SKEIN_HOME` is that it has one. It exists so that the mistake everybody makes
/// once, moving the volume and forgetting to set the variable, is a refusal naming the new path
/// instead of a second empty installation quietly filling up beside the real one.
fn moved_path(home: &Path) -> PathBuf {
    home.join("moved-to")
}

/// Where this installation says it moved to, if it did.
pub fn moved_to() -> Option<String> {
    fs::read_to_string(moved_path(&skein_home()))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The volume's schema version. `None` when there is no `VERSION` file — which is every
/// installation made before this existed, not an error.
pub fn schema_of(home: &Path) -> Option<u32> {
    fs::read_to_string(version_path(home))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Check the volume this process is about to use, and adopt it if it predates versioning.
///
/// Called once at startup by both binaries. Every refusal names the fix, because each of them is a
/// state somebody can be in for a perfectly good reason.
pub fn ensure_volume() -> Result<(), String> {
    let home = skein_home();
    if let Some(target) = moved_to() {
        return Err(format!(
            "this installation was moved to {target}, and $SKEIN_HOME still points at {}.\n\
             Set it and try again:  export SKEIN_HOME={target}\n\
             Or, if you meant to keep using this one:  rm {}",
            home.display(),
            moved_path(&home).display()
        ));
    }
    if migrating_path(&home).exists() {
        return Err(format!(
            "{} holds a half-finished move: something copied into it and did not finish, so what \
             is there is not a whole installation.\n\
             Delete it and move again, or remove {} if you know the copy completed.",
            home.display(),
            migrating_path(&home).display()
        ));
    }
    match schema_of(&home) {
        Some(found) if found > SCHEMA => Err(format!(
            "{} was written by a newer skein (volume schema {found}; this one understands {SCHEMA}).\n\
             Upgrade skein, or point $SKEIN_HOME at another volume. Reading it anyway would drop \
             every field this binary does not know at the next write.",
            home.display()
        )),
        // Unreachable while there is one schema, and written for the day there are two: an older
        // volume needs a migration to run, and inventing one is worse than saying there is none.
        Some(found) if found < SCHEMA => Err(format!(
            "{} is volume schema {found} and this skein wants {SCHEMA}, with no migration between \
             them. Use a skein that understands {found}.",
            home.display()
        )),
        Some(_) => Ok(()),
        // No VERSION: an installation made before versioning existed. Adopting it is the whole of
        // the upgrade — nothing about its layout differs from schema 1, which is what schema 1 was
        // defined to be.
        None => {
            if !home.is_dir() {
                return Ok(()); // nothing here yet; the first write makes it
            }
            stamp(&home)
        }
    }
}

/// Write the schema version onto a volume.
pub fn stamp(home: &Path) -> Result<(), String> {
    fs::create_dir_all(home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    write_atomic(&version_path(home), home, format!("{SCHEMA}\n").as_bytes())
}

/// Kilobytes available on the filesystem holding `dir`, and kilobytes `dir` itself occupies.
///
/// `df`/`du` rather than `statvfs`: skein has no libc dependency and this is the whole of what would
/// be used from one. `None` when the tool cannot answer, which is treated as "cannot check" rather
/// than as "there is room" — §5 asks for a free-space precondition on every write, and a
/// precondition that passes when it could not be evaluated is not one.
pub fn available_kb(dir: &Path) -> Option<u64> {
    let out = Command::new("df").arg("-Pk").arg(dir).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // `Filesystem 1024-blocks Used Available Capacity Mounted on` — the last line, 4th column.
    text.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()
}

/// Kilobytes `dir` occupies, or `None` when `du` could not read all of it.
pub fn used_kb(dir: &Path) -> Option<u64> {
    let mut command = Command::new("du");
    command.arg("-sk").arg(dir);
    let out = crate::util::bounded_output(&mut command, "du", Duration::from_secs(120)).ok()?;
    // A single unreadable path anywhere under it makes `du` exit nonzero while still printing a
    // total, and that total is an undercount. Refusing to answer is right: the number is about to
    // be used to decide whether a copy fits.
    out.status.success().then_some(())?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Copy this installation onto `target`, leaving the original in place.
///
/// **Nothing is deleted.** The old volume stays exactly as it was, and a `moved-to` beside it says
/// where it went. Deleting it is the one step that cannot be undone, and it is not skein's to take
/// on the same command that made the copy — a move that is discovered to be wrong an hour later
/// should be a matter of unsetting one environment variable.
///
/// What does not travel: nothing under `$SKEIN_HOME` is excluded, because nothing under it is
/// disposable. Box checkouts are on VM-local disk and reclonable from the mirror, caches and build
/// output are in the sandbox — none of them is here to skip.
pub fn migrate(target: &str) -> Result<String, String> {
    // Expanded here rather than by the caller: `skein migrate '~/vol'` quoted past the shell is the
    // same request as the unquoted one, and a directory literally named `~` is nobody's intent.
    let target = &PathBuf::from(crate::util::expand_tilde(target));
    let home = skein_home();
    let source = home.canonicalize().unwrap_or_else(|_| home.clone());
    if !source.is_dir() {
        return Err(format!("there is nothing at {} to move", source.display()));
    }
    fs::create_dir_all(target).map_err(|e| format!("mkdir {}: {e}", target.display()))?;
    let target = target.canonicalize().unwrap_or_else(|_| target.clone());

    if target == source {
        return Err(format!("{} is already the volume", target.display()));
    }
    // Either containing the other is a copy of a directory into itself, which `cp` will happily
    // start and never finish.
    for (inner, outer, how) in [
        (&target, &source, "inside the volume it would hold"),
        (&source, &target, "the parent of the volume it would hold"),
    ] {
        if inner.starts_with(outer) {
            return Err(format!("{} is {how}", target.display()));
        }
    }
    if migrating_path(&target).exists() {
        return Err(format!(
            "{} holds a half-finished move. Delete it and start again rather than copying over it.",
            target.display()
        ));
    }
    // Anything that makes the target an installation already — not just VERSION, because an
    // installation that predates versioning has no VERSION and is no less real.
    for marker in ["VERSION", "config.json", "repos.json", "boxes", "repos"] {
        if target.join(marker).exists() {
            return Err(format!(
                "{} already holds a skein installation ({marker} is there). Move onto an empty \
                 directory — merging two volumes is not something this can do safely.",
                target.display()
            ));
        }
    }
    // Boxes hold their state under the volume and their namespaces are alive: copying it out from
    // under them gives every running box a path that answers from the old copy.
    let sandbox = crate::config::load_config()
        .fleet_sandbox
        .trim()
        .to_string();
    if !sandbox.is_empty() && crate::fleet::fleet_exists(&sandbox) == Some(true) {
        return Err(format!(
            "the fleet sandbox {sandbox} is up, and its boxes are reading the volume you are \
             moving. Stop it first (`sbx stop {sandbox}`), move, set $SKEIN_HOME, and start it \
             again — every box's work is on the volume and travels with it."
        ));
    }

    let need = used_kb(&source).ok_or_else(|| {
        format!(
            "could not measure {} — something under it is unreadable, and a copy that does not \
             know its own size cannot be checked against the room for it",
            source.display()
        )
    })?;
    let have = available_kb(&target)
        .ok_or_else(|| format!("could not ask how much room {} has", target.display()))?;
    // A tenth over, because a copy needs a little more than the source measures: directory entries,
    // block rounding, and whatever is written while it runs.
    let want = need + need / 10;
    if have < want {
        return Err(format!(
            "{} has {} free and this needs about {} ({} plus a tenth). Free some room or choose \
             another target.",
            target.display(),
            human(have),
            human(want),
            human(need)
        ));
    }

    fs::write(
        migrating_path(&target),
        b"a skein volume is being copied here\n",
    )
    .map_err(|e| format!("marking the move: {e}"))?;
    // `cp -a`, and the part of it that is load-bearing is **links**, not modes: a plain `cp -r`
    // keeps 0600 on a token anyway, because a new file takes the source's mode through the umask.
    // What `-a` adds is that a symlink arrives as a symlink — the volume is full of them, and a
    // copy that turned each into a duplicate of what it pointed at would look correct until
    // something followed one out of the volume. Checked by copying with `-r -L`, which fails.
    // `<source>/.` copies the contents rather than the directory itself.
    let mut command = Command::new("cp");
    command.arg("-a").arg(source.join(".")).arg(&target);
    let out = crate::util::bounded_output(&mut command, "cp", Duration::from_secs(1800))?;
    if !out.status.success() {
        return Err(format!(
            "copying to {} failed, and the half-copy is left in place with its MIGRATING marker so \
             nothing mistakes it for an installation: {}",
            target.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    stamp(&target)?;
    // Only now: while this file is there, the target is not an installation.
    fs::remove_file(migrating_path(&target)).map_err(|e| format!("finishing the move: {e}"))?;
    write_atomic(
        &moved_path(&source),
        &source,
        format!("{}\n", target.display()).as_bytes(),
    )?;

    Ok(format!(
        "moved {} to {} ({}).\n\nSet this before running skein again — it is the only thing that \
         points at a volume:\n  export SKEIN_HOME={}\n\nThe old copy is untouched at {}. Delete it \
         once a fleet has come up from the new one.",
        source.display(),
        target.display(),
        human(need),
        target.display(),
        source.display()
    ))
}

/// Kilobytes as something a person reads.
fn human(kb: u64) -> String {
    match kb {
        k if k >= 1024 * 1024 => format!("{:.1} GB", k as f64 / (1024.0 * 1024.0)),
        k if k >= 1024 => format!("{:.1} MB", k as f64 / 1024.0),
        k => format!("{k} KB"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, tempdir};

    /// A populated volume: enough shapes that a copy which skipped one would be caught.
    fn populate(home: &Path) {
        fs::create_dir_all(home.join("boxes/web-main")).unwrap();
        fs::create_dir_all(home.join("declared/web-main")).unwrap();
        fs::create_dir_all(home.join("repos/web/mirror")).unwrap();
        fs::write(home.join("config.json"), "{}").unwrap();
        fs::write(home.join("repos.json"), "[]").unwrap();
        fs::write(home.join("boxes/web-main/conversation.jsonl"), "{}").unwrap();
        fs::write(home.join("declared/web-main/privileged"), "1").unwrap();
        // A symlink, because the volume is full of them — a store reached through one, a kit file
        // linked rather than copied — and a copy that turned links into duplicates would look
        // perfectly correct until something followed one out of the volume.
        #[cfg(unix)]
        std::os::unix::fs::symlink("../repos/web/mirror", home.join("boxes/web-main/mirror"))
            .unwrap();
        let token = home.join("api-token");
        fs::write(&token, "secret").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// The move carries everything, keeps the modes credentials depend on, and leaves the original
    /// exactly where it was.
    #[test]
    fn moving_a_volume_carries_it_whole_and_deletes_nothing() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        populate(&home);

        let elsewhere = tempdir();
        let target = elsewhere.join("volume");
        let report = migrate(&target.to_string_lossy()).unwrap();
        assert!(report.contains("export SKEIN_HOME="), "{report}");

        for rel in [
            "config.json",
            "repos.json",
            "boxes/web-main/conversation.jsonl",
            "declared/web-main/privileged",
            "repos/web/mirror",
            "api-token",
        ] {
            assert!(target.join(rel).exists(), "{rel} did not travel");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(target.join("api-token"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the API token arrived world-readable");
        }
        #[cfg(unix)]
        {
            let link = target.join("boxes/web-main/mirror");
            assert!(
                link.is_symlink(),
                "a symlink arrived as a copy of what it pointed at"
            );
            assert_eq!(
                fs::read_link(&link).unwrap().to_string_lossy(),
                "../repos/web/mirror",
                "the link was rewritten"
            );
        }
        assert_eq!(schema_of(&target), Some(SCHEMA), "the copy is unstamped");
        assert!(
            !migrating_path(&target).exists(),
            "the marker outlived the move, so nothing will use the volume"
        );

        // Nothing deleted, and the original says where it went.
        assert!(home.join("config.json").exists(), "the source was emptied");
        assert_eq!(
            moved_to().as_deref(),
            Some(target.to_string_lossy().as_ref())
        );
    }

    /// The two measurements the free-space precondition is built on actually answer.
    ///
    /// The precondition itself — refusing a target with no room — cannot be exercised without a
    /// full filesystem, so what is pinned here is the part that would break silently: a `df` or `du`
    /// whose output moved a column would make `migrate` refuse every move, or worse, measure
    /// nothing and pass. Both return `None` rather than a wrong number, and `None` is a refusal.
    #[test]
    fn the_free_space_check_can_measure_both_sides() {
        let dir = tempdir();
        fs::write(dir.join("a"), vec![b'x'; 200_000]).unwrap();
        let used = used_kb(&dir).expect("du could not measure a directory it can read");
        assert!(
            (150..=1024).contains(&used),
            "200 KB of file measured as {used} KB"
        );
        let free = available_kb(&dir).expect("df could not answer about a real filesystem");
        assert!(free > 0, "a writable filesystem reported no room at all");
        assert_eq!(human(1536), "1.5 MB");
        assert_eq!(human(2 * 1024 * 1024), "2.0 GB");
    }

    /// Every refusal, and each one names a state somebody can be in for a good reason.
    #[test]
    fn a_move_refuses_rather_than_half_doing_it() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        populate(&home);

        let elsewhere = tempdir();

        // Onto itself, and onto its own parent.
        assert!(migrate(&home.to_string_lossy())
            .unwrap_err()
            .contains("already the volume"));
        let inside = home.join("inner");
        assert!(migrate(&inside.to_string_lossy())
            .unwrap_err()
            .contains("inside the volume"));

        // Onto a directory that already holds an installation — including one made before
        // versioning, which has no VERSION to recognise it by.
        let occupied = elsewhere.join("occupied");
        fs::create_dir_all(&occupied).unwrap();
        fs::write(occupied.join("repos.json"), "[]").unwrap();
        let why = migrate(&occupied.to_string_lossy()).unwrap_err();
        assert!(why.contains("already holds a skein installation"), "{why}");

        // Onto a half-finished move.
        let halfway = elsewhere.join("halfway");
        fs::create_dir_all(&halfway).unwrap();
        fs::write(migrating_path(&halfway), "x").unwrap();
        let why = migrate(&halfway.to_string_lossy()).unwrap_err();
        assert!(why.contains("half-finished move"), "{why}");
    }

    /// A volume from a newer skein is refused, not half-read.
    ///
    /// The direction is the point. Reading a newer volume anyway works right up until the first
    /// write, which drops every field this binary does not know about — and the field it drops
    /// silently is as likely to be a grant or a placement as a cosmetic setting.
    #[test]
    fn a_volume_from_a_newer_skein_is_refused() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        populate(&home);

        fs::write(version_path(&home), format!("{}\n", SCHEMA + 1)).unwrap();
        let why = ensure_volume().unwrap_err();
        assert!(why.contains("newer skein"), "{why}");
        assert!(
            why.contains("Upgrade skein"),
            "the refusal must name the fix: {why}"
        );

        // And an installation from before versioning is adopted rather than refused: nothing about
        // its layout differs from schema 1, which is what schema 1 was defined to be.
        fs::remove_file(version_path(&home)).unwrap();
        ensure_volume().unwrap();
        assert_eq!(schema_of(&home), Some(SCHEMA));
    }

    /// A volume that was moved refuses to be used by the path that no longer holds it.
    #[test]
    fn the_volume_you_moved_away_from_does_not_quietly_get_used_again() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        populate(&home);
        let elsewhere = tempdir();
        let target = elsewhere.join("volume");
        migrate(&target.to_string_lossy()).unwrap();

        // Still pointed at the old path, which is the mistake everybody makes once.
        let why = ensure_volume().unwrap_err();
        assert!(why.contains(&target.to_string_lossy().to_string()), "{why}");
        assert!(why.contains("export SKEIN_HOME="), "{why}");

        // The new one is fine, and saying so is what makes the refusal above a signpost rather
        // than a wall.
        std::env::set_var("SKEIN_HOME", &target);
        ensure_volume().unwrap();

        // And the refusal is undoable by the sentence it prints.
        std::env::set_var("SKEIN_HOME", &home);
        fs::remove_file(moved_path(&home)).unwrap();
        ensure_volume().unwrap();
    }

    /// A half-finished copy is not an installation, and says so.
    #[test]
    fn a_half_copied_volume_is_refused_rather_than_read() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        populate(&home);
        fs::write(migrating_path(&home), "x").unwrap();
        let why = ensure_volume().unwrap_err();
        assert!(why.contains("half-finished move"), "{why}");
    }
}
