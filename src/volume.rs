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

/// Where a volume records the path it was written at.
///
/// A separate marker from `moved-to`, which answers the opposite question — that one is left on the
/// volume somebody *left*, this one is on the volume somebody is using. A single file could not say
/// both: an installation that was moved away from and then copied somewhere is two facts.
fn written_at_path(home: &Path) -> PathBuf {
    home.join("written-at")
}

/// The path this volume believes it lives at, canonical, or `None` on a volume from before the
/// marker existed.
pub fn written_at(home: &Path) -> Option<String> {
    fs::read_to_string(written_at_path(home))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// This process's volume path in the form the marker is compared against.
///
/// Canonical, so `/home/x/.skein`, `/home/x/./.skein` and a symlinked parent are one answer rather
/// than three ways to be told a volume has moved when it has not.
pub fn here() -> PathBuf {
    let home = skein_home();
    home.canonicalize().unwrap_or(home)
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
        Some(_) => opened_where_it_was_written(&home),
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

/// Is this volume being opened at the path it was written at — and if not, does that matter yet?
///
/// **The failure it closes is silent, which is the only reason it is a refusal.** `repos.json` holds
/// each repo's `store`, `source`, `source_tree` and `work` as absolute paths *under the volume*. Copy
/// a `.skein` somewhere else and point `$SKEIN_HOME` at the copy and it works perfectly — reading
/// and writing the **old** one, which the copy is quietly no longer. Everything looks healthy right
/// up until somebody deletes the original. Verified before this existed: a byte-identical copy
/// reported its store under the source path and `skein repos` said nothing.
///
/// `skein migrate` has always done this rewrite ([`repoint`]); setting `$SKEIN_HOME` never did, and
/// there is no reason anybody would expect the difference.
///
/// **Nothing stale is repaired rather than refused.** A volume with no repos registered, or whose
/// repos all keep their stores elsewhere on purpose, has nothing pointing at the old path — so the
/// marker is simply wrong and gets corrected. Refusing there would be stopping somebody over a fact
/// with no consequence, which is how a check earns the reputation that gets it switched off.
fn opened_where_it_was_written(home: &Path) -> Result<(), String> {
    let here = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let Some(recorded) = written_at(home) else {
        // A volume from before this marker. Adopting it is the whole of the upgrade, exactly as it
        // is for a volume with no VERSION — and there is nothing to compare against on one, so
        // treating its absence as suspicious would refuse every volume in existence.
        return remember_here(home);
    };
    // Resolved on both sides: a marker written by an older skein that recorded the literal path,
    // opened through a symlinked component, describes this very directory and would otherwise read
    // as a volume that had moved.
    if crate::util::resolved(&recorded) == crate::util::resolved(&here.to_string_lossy()) {
        return Ok(());
    }
    let stale = repoint(Path::new(&recorded), &here, false).unwrap_or(0);
    if stale == 0 {
        return remember_here(home);
    }
    Err(format!(
        "this volume was written at {recorded} and $SKEIN_HOME points at {}.\n\
         {stale} path(s) it records still name {recorded}, so skein here would read and write the \
         volume over there — and everything would look fine until that one was deleted.\n\
         If you moved or copied it here:   skein repoint\n\
         If you meant the original:        export SKEIN_HOME={recorded}",
        here.display()
    ))
}

/// `skein repoint` — make a volume that was moved by hand consistent with where it now is.
///
/// The repair half of [`opened_where_it_was_written`]. It is the same rewrite `skein migrate` does
/// at the end of a move ([`repoint`]), run without the move: the copying already happened, by
/// whatever means somebody used, and what is left is the paths the volume records about itself.
///
/// **Only paths under the recorded home are touched.** A store deliberately kept elsewhere — another
/// disk, a shared location — is not this command's business, and rewriting it would move somebody's
/// data in a way they did not ask for. That rule is [`repoint`]'s and is why this reuses it rather
/// than doing its own walk.
pub fn repoint_here() -> Result<String, String> {
    let home = skein_home();
    let here = home.canonicalize().unwrap_or_else(|_| home.clone());
    if !here.is_dir() {
        return Err(format!("there is no volume at {}", here.display()));
    }
    let Some(recorded) = written_at(&home) else {
        remember_here(&home)?;
        return Ok(format!(
            "{} did not say where it was written; it does now, and nothing needed repointing",
            here.display()
        ));
    };
    if crate::util::resolved(&recorded) == crate::util::resolved(&here.to_string_lossy()) {
        return Ok(format!(
            "{} is already where it says it was written; nothing to repoint",
            here.display()
        ));
    }
    let moved = repoint(Path::new(&recorded), &here, true)?;
    remember_here(&home)?;
    Ok(format!(
        "{moved} path(s) repointed from {recorded} to {}.\n\
         The volume at {recorded} is untouched — nothing was deleted, and it is still a whole \
         installation if you want to go back to it.",
        here.display()
    ))
}

/// Write the schema version onto a volume.
pub fn stamp(home: &Path) -> Result<(), String> {
    fs::create_dir_all(home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    write_atomic(&version_path(home), home, format!("{SCHEMA}\n").as_bytes())?;
    remember_here(home)
}

/// Record the path this volume is at, so opening it from anywhere else is answerable.
///
/// Written beside `VERSION` and for the same reason: a volume that cannot say anything about itself
/// leaves every question about it to be inferred, and the inference here — comparing the absolute
/// paths in `repos.json` — has nothing to work with on a volume with no repos yet.
pub fn remember_here(home: &Path) -> Result<(), String> {
    let canonical = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    write_atomic(
        &written_at_path(home),
        home,
        format!("{}\n", canonical.display()).as_bytes(),
    )
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
/// What does not travel, and it is not "nothing": **the instance-scoped secrets are dropped rather
/// than copied**, and every absolute path recorded under the old volume is rewritten to the new one.
/// See [`INSTANCE_SCOPED`] and [`repoint`] for why each is not optional.
///
/// Everything else travels, because nothing else under `$SKEIN_HOME` is disposable. Box checkouts
/// are on VM-local disk and reclonable from the mirror, caches and build output are in the sandbox —
/// none of them is here to skip.
///
/// **A store's contents travel bit-for-bit, and that is correct — established, not assumed.** The
/// worry is a path baked *inside* a store, where a stale entry sits beside a working one and the box
/// comes up fine while announcing a failure at every start. Nothing written today does that: every
/// hook command is `$CLAUDE_PROJECT_DIR/.claude/skein/bin/...`, resolved inside the box, and
/// `a_store_holds_no_path_into_the_volume_it_was_written_on` builds a store and searches it, so that
/// stays true rather than being a claim about the writers. The exception is the two markers a store
/// can be old enough to hold — see [`markers`] — and those are rewritten with the rest.
/// The deepest ancestor of `path` that exists — the filesystem that will hold it.
fn nearest_existing(path: &Path) -> PathBuf {
    let mut walk = path.to_path_buf();
    loop {
        if walk.exists() {
            return walk;
        }
        match walk.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => walk = parent.to_path_buf(),
            _ => return PathBuf::from("/"),
        }
    }
}

/// Moving the volume, as an Operation (§2.4) — because it is not something skein may do for you.
///
/// # Why this is an operation and not a function call
///
/// The volume is bind-mounted into the fleet sandbox at create time, and skein runs **inside** that
/// sandbox. So moving it is the shape architecture §7.5 names: an act that terminates its own
/// reconciler. `migrate` refuses whenever a fleet sandbox is named — and one always is
/// (`config::load_config` gives an empty name the default, SKEIN-484) — so the honest surface is
/// not a call that returns an error, it is an operation that reports and prints what to run.
///
/// **`class: Destructive`, so it is never auto-driven** even when the check is unsatisfied. Nothing
/// in skein performs this; a person does, on the host, where the process being interrupted is not
/// the one doing the interrupting.
///
/// # The recipe is warden acts, rendered by the warden's own renderer
///
/// The two `sbx` lines come from `warden_client::Act::command`, which is what the warden's approval
/// prompt prints and what `skein doctor` already offers — so what somebody is told to type and what
/// the warden would run cannot drift apart. The `mv` between them is the only step that is neither.
///
/// The id is derived the same way a warden request's is (`warden_client::operation_id`): from the
/// verb, the sandbox and the argv, so asking twice about the same move names one operation rather
/// than two. That property is the warden's at-most-once story and it is asserted there.
pub fn move_to(target: &str) -> crate::operation::Operation {
    use crate::operation::{Check, Class, Operation};
    let wanted = crate::util::expand_tilde(target);
    let here = skein_home();
    let sandbox = crate::config::load_config()
        .fleet_sandbox
        .trim()
        .to_string();

    let resolved = resolved_without_creating(Path::new(&wanted));
    let source = here.canonicalize().unwrap_or_else(|_| here.clone());
    let check = if resolved == source {
        Check::Satisfied(format!("the volume is already at {}", resolved.display()))
    } else if !here.is_dir() {
        // Not "no": skein cannot see its own volume, and moving something it cannot read is not a
        // thing to report as merely undone.
        Check::Unknown(format!(
            "there is nothing readable at {} to move",
            here.display()
        ))
    } else {
        Check::Unsatisfied(format!("the volume is at {}", source.display()))
    };

    let mut recipe = Vec::new();
    if !sandbox.is_empty() {
        recipe.push(format!(
            "{}   # stops skein with it",
            crate::warden_client::Act::Destroy {
                sandbox: sandbox.clone()
            }
            .command()
        ));
    }
    recipe.push(format!("mv {} {}", source.display(), resolved.display()));
    recipe.push(format!("export SKEIN_HOME={}", resolved.display()));
    match crate::fleet::create_line(&sandbox) {
        Ok(line) => recipe.push(format!("{line}   # with the volume at its new path")),
        // The mounts are fixed at create and are worked out from this installation, so a line that
        // could not be worked out is not one to guess at.
        Err(why) => recipe.push(format!(
            "# the `sbx create` line could not be worked out from here — {why} — so run \
             `skein doctor` and copy the one it prints"
        )),
    }

    Operation {
        id: crate::warden_client::operation_id("move-volume", &sandbox, &recipe),
        desired: format!("the volume is at {}", resolved.display()),
        check,
        recipe,
        class: Class::Destructive,
        // A person moves a volume. Nothing else may, and the class already withholds it — the
        // `None` says the same thing from the other side, so a later change to the class cannot
        // quietly hand this to a doer that was never written.
        doer: None,
    }
}

/// A path as it would resolve, **without creating any part of it**.
///
/// `canonicalize` needs the path to exist, and the checks in [`migrate`] need a resolved path to
/// answer "is one of these inside the other" honestly — `/vol` and `/vol/../vol` are the same
/// directory and only one of them says so. Creating the target to get that answer is what put a
/// directory on disk for every refused move.
///
/// So the deepest existing ancestor is canonicalised and the rest is re-appended. A target whose
/// parent does not exist either resolves as far as it can, which is enough: containment is decided
/// by the part that is real.
fn resolved_without_creating(path: &Path) -> PathBuf {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut walk = path.to_path_buf();
    loop {
        if let Ok(real) = walk.canonicalize() {
            let mut out = real;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }
        let Some(name) = walk.file_name().map(|n| n.to_os_string()) else {
            return path.to_path_buf();
        };
        tail.push(name);
        match walk.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => walk = parent.to_path_buf(),
            _ => return path.to_path_buf(),
        }
    }
}

pub fn migrate(target: &str) -> Result<String, String> {
    // Expanded here rather than by the caller: `skein migrate '~/vol'` quoted past the shell is the
    // same request as the unquoted one, and a directory literally named `~` is nobody's intent.
    let target = &PathBuf::from(crate::util::expand_tilde(target));
    let home = skein_home();
    let source = home.canonicalize().unwrap_or_else(|_| home.clone());
    if !source.is_dir() {
        return Err(format!("there is nothing at {} to move", source.display()));
    }
    // **Resolved without being created.** This used to `mkdir -p` the target here, above every
    // refusal below it — so a move that was going to be refused still left a directory behind, and
    // on the commonest refusal (the fleet is up, which is now always) it left one on every attempt.
    // A path is resolved by canonicalising the deepest ancestor that exists and re-appending the
    // rest, which answers the containment questions below without writing anything.
    let target = resolved_without_creating(target);

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
        // The whole operation, rendered — not a sentence about it. This is the one place a person
        // meets the refusal, so it is where the recipe belongs (§2.4: "always present, always
        // printable"), and the recipe's `sbx` lines come from the warden's own renderer so what
        // they are told to type cannot drift from what the warden would run.
        return Err(format!(
            "the fleet sandbox {sandbox} is up, and its boxes are reading the volume you are \
             moving — including this process, whose own state is on it. This is a job for the \
             host:\n\n{}\nEvery box's work is on the volume and travels with it.",
            move_to(&target.to_string_lossy()).render()
        ));
    }

    carry_the_volume_to(&source, &target)
}

/// **The move itself: everything [`move_to`]'s recipe asks a person to do, as code.**
///
/// Nothing in skein reaches this today, and that is the decision rather than an oversight
/// (SKEIN-574, `docs/parity.md` §7). [`migrate`]'s guard above always fires — skein runs inside the
/// sandbox whose boxes are reading this volume, so the fleet is always up — and a move is
/// architecture §7.5's shape one level down, an act that ends the process performing it.
///
/// It is kept, and kept exercised, for two reasons §7 states: it is what a person's `mv` is checked
/// against, and it is what a host-side doer would call the day one exists. Its own function so that
/// "kept exercised" is something a test can do without reaching around a refusal — a test that
/// defeated the guard would be testing a fleet that is not this one.
///
/// `source` is resolved and known to exist; `target` is resolved, does not contain or sit inside
/// `source`, holds no installation and no half-finished move. [`migrate`] establishes all of that
/// before this is called, and a future doer has to establish it too.
fn carry_the_volume_to(source: &Path, target: &Path) -> Result<String, String> {
    let need = used_kb(source).ok_or_else(|| {
        format!(
            "could not measure {} — something under it is unreadable, and a copy that does not \
             know its own size cannot be checked against the room for it",
            source.display()
        )
    })?;
    // Asked of the deepest part of the path that exists, because the target itself does not yet —
    // nothing is created until every refusal above has passed. `df` answers about a FILESYSTEM, and
    // a directory and its parent are on the same one until something is mounted between them, so
    // this is the same number by a route that writes nothing.
    let holder = nearest_existing(target);
    let have = available_kb(&holder)
        .ok_or_else(|| format!("could not ask how much room {} has", holder.display()))?;
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

    // The first thing this call creates, and it is deliberately after every refusal above: nothing
    // exists at the target until the move is actually going to be attempted.
    fs::create_dir_all(target).map_err(|e| format!("mkdir {}: {e}", target.display()))?;
    fs::write(
        migrating_path(target),
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
    command.arg("-a").arg(source.join(".")).arg(target);
    let out = crate::util::bounded_output(&mut command, "cp", Duration::from_secs(1800))?;
    if !out.status.success() {
        return Err(format!(
            "copying to {} failed, and the half-copy is left in place with its MIGRATING marker so \
             nothing mistakes it for an installation: {}",
            target.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // **Dropped, not copied.** These are scoped to one installation, and two volumes holding the
    // same token is exactly the "machine-global secret" the design says skein does not have —
    // untrue on day one if a migration duplicates one. They are re-minted on the next start, which
    // is what the warden's `kept_in` does for `warden/secret`. The fleet agent's token was the
    // other one this paragraph was written about, and it went with the agent (SKEIN-521).
    //
    // The port matters as much as the token and for a duller reason: a copied port sends the new
    // installation's agent at whatever is listening on the old one's.
    let mut dropped: Vec<&str> = Vec::new();
    for scoped in INSTANCE_SCOPED {
        let path = target.join(scoped);
        if path.exists() && fs::remove_file(&path).is_ok() {
            dropped.push(scoped);
        }
    }
    let repointed = repoint(source, target, true)?;
    stamp(target)?;
    // Only now: while this file is there, the target is not an installation.
    fs::remove_file(migrating_path(target)).map_err(|e| format!("finishing the move: {e}"))?;
    write_atomic(
        &moved_path(source),
        source,
        format!("{}\n", target.display()).as_bytes(),
    )?;

    Ok(format!(
        "moved {} to {} ({}).{}{}\n\nSet this before running skein again — it is the only thing \
         that points at a volume:\n  export SKEIN_HOME={}\n\nThe old copy is untouched at {}. \
         Delete it once a fleet has come up from the new one.",
        source.display(),
        target.display(),
        human(need),
        match dropped.as_slice() {
            [] => String::new(),
            some => format!(
                "\n\nNot carried, because they belong to one installation and are made again on \
                 the next start: {}.",
                some.join(", ")
            ),
        },
        match repointed {
            0 => String::new(),
            n => format!("\n\nRepointed {n} recorded path(s) from the old volume to this one."),
        },
        target.display(),
        source.display()
    ))
}

/// Files that belong to *an installation* rather than to the work it holds.
///
/// A migration copies the work and re-mints these. Two volumes holding the same fleet-agent token
/// would make "skein has no machine-global secret" untrue the moment anybody used the feature, and a
/// copied port aims the new installation's agent at whatever answers on the old one's.
///
/// `warden/secret` is the same kind as the token beside it: a pairing between one host's skein and
/// one host's warden (§9.5 R5 — its home is derived from the volume root so the cover holds over
/// it). It travelling to a second volume would be a machine-pairing secret moving like data; the
/// warden re-mints at the new home when it finds nothing (`warden/src/secret.rs`, `kept_in`),
/// exactly as the agent token's writers do.
pub const INSTANCE_SCOPED: &[&str] = &["fleet-agent.token", "fleet-agent.port", "warden/secret"];

/// Is `value` at or under `base`, and if so, what is below it?
///
/// Compared with [`resolved`] on both sides, so the two ways of spelling one directory match. The
/// suffix comes back from the RESOLVED value, which is what makes the rewritten path absolute and
/// unambiguous rather than half one spelling and half the other.
///
/// `starts_with` on the base plus a separator, never on the base alone: a sibling directory called
/// `~/.skein-old` shares the prefix and is a different installation.
fn below(value: &str, base: &str) -> Option<String> {
    let value = crate::util::resolved(value);
    let base = crate::util::resolved(base);
    if value == base {
        return Some(String::new());
    }
    value
        .strip_prefix(&format!("{base}/"))
        .map(|rest| format!("/{rest}"))
}

/// Rewrite the paths a moved installation records about itself.
///
/// **The failure this prevents is silent, which is why it is here and not a note.** `repos.json`
/// holds each repo's store as an *absolute* path under the volume — `~/.skein/repos/<id>/store/…` —
/// so a copied installation goes on reading and writing the **old** one. It works perfectly, for as
/// long as the old volume exists, and the new volume's copy of the store quietly stops being the one
/// anybody uses. Then somebody deletes the old volume, as the report above tells them to.
///
/// Only paths **under the old home** are touched. A store somebody deliberately put elsewhere — on
/// another disk, in a shared location — is not this move's business, and rewriting it would move
/// their data in a way nobody asked for.
///
/// `apply` is what makes this answerable *before* it is done. [`ensure_volume`] needs the count and
/// must not write — a volume that turns out to have nothing stale is one it repairs quietly, and a
/// volume that does is one it refuses. Two walks would be two things to keep in step, and the one
/// that only counted would be the one nobody exercised.
fn repoint(source: &Path, target: &Path, apply: bool) -> Result<usize, String> {
    let file = target.join("repos.json");
    let Ok(raw) = fs::read_to_string(&file) else {
        return Ok(0);
    };
    let old = source.to_string_lossy().to_string();
    let new = target.to_string_lossy().to_string();
    let mut repos: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|e| format!("reading {}: {e}", file.display()))?;
    let mut moved = 0usize;
    for repo in &mut repos {
        for key in ["store", "source", "source_tree", "work"] {
            let Some(value) = repo.get(key).and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(rest) = below(value, &old) else {
                continue;
            };
            let rewritten = format!("{new}{rest}");
            repo[key] = serde_json::Value::String(rewritten);
            moved += 1;
        }
    }
    if moved > 0 && apply {
        let bytes = serde_json::to_vec_pretty(&repos).map_err(|e| e.to_string())?;
        write_atomic(&file, target, &bytes)?;
    }
    // After the rewrite above, so the store paths it reads are the ones on **this** volume. Reading
    // the file from disk again would have been worse than redundant: when nothing needed rewriting,
    // the stores still name the old home, and "fixing" a marker there would write into the volume
    // this command promises not to touch.
    Ok(moved + markers(&old, &new, &repos, apply))
}

/// The same rewrite, for the two path markers a store can be *old enough* to hold.
///
/// Nothing skein writes today puts a volume path inside a store — every hook command is
/// `$CLAUDE_PROJECT_DIR/.claude/skein/bin/…`, resolved inside the box, and
/// `a_store_holds_no_path_into_the_volume_it_was_written_on` is what keeps that true. But
/// `sandbox-bootstrap.sh` still *reads* `skein/source` and `skein/mirror`, "for stores seeded before
/// the two things had separate names", and `skein/mirror` named a path under the volume. A store
/// that old travels bit-for-bit — correctly, for its contents — and then a box in the new
/// installation works from a mirror in the **old** one.
///
/// Bounded to the store directories the repo list names, and to a single line whose content is under
/// the old home. A marker pointing somewhere else is somebody's deliberate choice, exactly as in
/// [`repoint`].
fn markers(old: &str, new: &str, repos: &[serde_json::Value], apply: bool) -> usize {
    let mut moved = 0usize;
    for repo in repos {
        let Some(store) = repo.get("store").and_then(|v| v.as_str()) else {
            continue;
        };
        // Only stores on the new volume. One outside it is shared with the old installation, and one
        // still naming the old home is the old installation's own — neither is this move's to edit.
        if below(store, new).is_none_or(|rest| rest.is_empty()) {
            continue;
        }
        for name in ["skein/source", "skein/mirror"] {
            let file = Path::new(store).join(name);
            let Ok(body) = fs::read_to_string(&file) else {
                continue;
            };
            let line = body.lines().next().unwrap_or("").trim().to_string();
            if line.is_empty() {
                continue;
            }
            let Some(rest) = below(&line, old) else {
                continue;
            };
            let rewritten = format!("{new}{rest}\n");
            if !apply || fs::write(&file, rewritten).is_ok() {
                moved += 1;
            }
        }
    }
    moved
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

    /// **The move, driven the way a host-side doer would drive it.**
    ///
    /// Past the refusal rather than around it. `migrate` establishes the two arguments below and
    /// then always refuses, because skein runs inside the sandbox whose boxes are reading this
    /// volume (SKEIN-574, `docs/parity.md` §7) — so a test that called `migrate` here would be
    /// asserting the refusal, which `moving_the_volume_is_reported_with_its_recipe_and_never_driven`
    /// and `a_move_refuses_rather_than_half_doing_it` already do. What is checked below is the
    /// machinery §7 keeps: what a person's `mv` is
    /// measured against, and what a doer would call the day one exists.
    fn carry_to(target: &Path) -> Result<String, String> {
        let home = skein_home();
        let source = home.canonicalize().unwrap_or(home);
        carry_the_volume_to(&source, &resolved_without_creating(target))
    }

    /// Every file a fresh store is built with, so a path into the volume cannot hide in one.
    fn every_file(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match path.is_dir() {
                true => every_file(&path, out),
                false => out.push(path),
            }
        }
    }

    /// The other half of a move, and the one that would have broken quietly.
    ///
    /// `repos.json` records where each store is, and [`repoint`] rewrites those. This asks the
    /// opposite question: can a store hold a path back into the volume it was written on? If it can,
    /// a moved installation reads the OLD volume from inside a store that travelled correctly — and
    /// a hook command is the worst case, because a stale entry sits *beside* the working one, so the
    /// box comes up fine and announces a hook failure at every start, which reads as noise.
    ///
    /// Established by building a store and searching it, rather than by reading the writers and
    /// concluding: the writers are `kit::ensure_store` and `probes::ensure_probe_in` between them,
    /// and a claim about what they emit is exactly the kind that drifts. This is why no rewriter was
    /// written — every hook command is `$CLAUDE_PROJECT_DIR/.claude/skein/bin/…`, resolved inside
    /// the box at run time, and the only absolute path in the whole store is the one this test
    /// would catch if somebody added it.
    #[test]
    fn a_store_holds_no_path_into_the_volume_it_was_written_on() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let store = home.join("repos/demo/store/.claude");
        let made = crate::kit::ensure_store(&store);
        std::env::remove_var("SKEIN_HOME");
        made.expect("a store is built");

        let volume = home.to_string_lossy().into_owned();
        let mut files = Vec::new();
        every_file(&store, &mut files);
        assert!(
            files.len() > 10,
            "the store came up nearly empty: {files:?}"
        );
        let mut naming: Vec<String> = Vec::new();
        for file in &files {
            let Ok(body) = fs::read(file) else { continue };
            if String::from_utf8_lossy(&body).contains(&volume) {
                naming.push(file.strip_prefix(&store).unwrap().display().to_string());
            }
        }
        assert!(
            naming.is_empty(),
            "these files name the volume they were written on, so a moved installation would read \
             the old one from inside a store that travelled correctly: {naming:?}\n\
             a path a box needs belongs under $CLAUDE_PROJECT_DIR, which is resolved inside the box"
        );
    }

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
        let report = carry_to(&target).unwrap();
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
        // The PLACE, not the spelling. A move records where the volume now is with its symlinks
        // resolved, because that is the form every later comparison is made against — and under a
        // symlinked `$TMPDIR`, which is every macOS run, the raw string and the resolved one are two
        // names for one directory. Asserting the string made this test sharp about the wrong thing.
        assert_eq!(
            moved_to().map(|at| crate::util::resolved(&at)),
            Some(crate::util::resolved(&target.to_string_lossy()))
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

    /// An installation's own secrets are re-minted, and its recorded paths follow it.
    ///
    /// Two failures that are invisible until they matter. A copied fleet-agent token makes "skein
    /// has no machine-global secret" untrue the moment two volumes exist, and a copied port aims the
    /// new installation's agent at whatever answers on the old one's. And `repos.json` records each
    /// store as an absolute path *under the volume* — copied unchanged, the new installation reads
    /// and writes the **old** store, works perfectly for as long as the old volume exists, and then
    /// somebody deletes it, as this command's own report tells them to.
    #[test]
    fn a_moved_volume_re_mints_its_own_secrets_and_repoints_its_paths() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        populate(&home);
        // `warden/secret` is the one this list gained last: the warden's pairing with this host's
        // skein, which used to be copied as data because nothing named it here.
        assert!(
            INSTANCE_SCOPED.contains(&"warden/secret"),
            "the warden's pairing secret left the instance-scoped list, so a migration copies it"
        );
        for scoped in INSTANCE_SCOPED {
            let path = home.join(scoped);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"instance-scoped").unwrap();
        }
        // Not instance-scoped, and the distinction is the whole of `docs/delivery.md` §4.1a. The
        // fleet-agent token and port are re-minted because two volumes holding the same one make
        // "skein has no machine-global secret" untrue. `gh-secret-seeded` is the opposite case: the
        // secret it records lives in **sbx's store**, which outlives the volume — so losing only the
        // marker means the next server start reaches for `gh auth token` and asks to unlock a
        // keyring, for a secret that is already there. It has to travel.
        fs::write(home.join("gh-secret-seeded"), b"2026-01-01T00:00:00Z\n").unwrap();
        // Two repos: one whose store is under the volume, one deliberately elsewhere.
        let outside = tempdir().join("shared-store");
        fs::write(
            home.join("repos.json"),
            serde_json::to_vec_pretty(&serde_json::json!([
                {
                    "id": "inside",
                    "source": "https://example.com/x.git",
                    "source_tree": "",
                    "store": home.join("repos/inside/store/.claude").to_string_lossy(),
                    "agent": "claude",
                    "plane_project": "", "sync_connection": "",
                    "review_queue": true, "sync_gateway_url": ""
                },
                {
                    "id": "elsewhere",
                    "source": "https://example.com/y.git",
                    "source_tree": "",
                    "store": outside.to_string_lossy(),
                    "agent": "claude",
                    "plane_project": "", "sync_connection": "",
                    "review_queue": true, "sync_gateway_url": ""
                }
            ]))
            .unwrap(),
        )
        .unwrap();

        // A store old enough to hold the marker `sandbox-bootstrap.sh` still reads "for stores
        // seeded before the two things had separate names". It names the mirror under this volume,
        // and travelling bit-for-bit is exactly what makes it wrong afterwards.
        let legacy = home.join("repos/inside/store/.claude/skein");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(
            legacy.join("mirror"),
            format!("{}\n", home.join("repos/inside/mirror").display()),
        )
        .unwrap();
        // And one that points somewhere else entirely, which is somebody's deliberate choice.
        // A sibling that SHARES the volume's prefix: `~/.skein-old` beside `~/.skein` is a
        // different installation, and a prefix test without the separator would move it.
        let kept = std::path::PathBuf::from(format!("{}-elsewhere", home.display()))
            .join("their-own-checkout");
        fs::write(legacy.join("source"), format!("{}\n", kept.display())).unwrap();
        // A store outside the volume, whose marker names the OLD volume: it is shared with the
        // installation being left behind, so editing it would reach into what this move must not
        // touch.
        fs::create_dir_all(outside.join("skein")).unwrap();
        fs::write(
            outside.join("skein/mirror"),
            format!("{}\n", home.join("repos/elsewhere/mirror").display()),
        )
        .unwrap();

        let elsewhere_dir = tempdir();
        let target = elsewhere_dir.join("volume");
        let report = carry_to(&target).unwrap();

        for scoped in INSTANCE_SCOPED {
            assert!(
                !target.join(scoped).exists(),
                "{scoped} was copied — two volumes now hold one installation's secret"
            );
            // And the original keeps its own: the move deletes nothing on the source side.
            assert!(
                home.join(scoped).exists(),
                "{scoped} was taken from the old volume"
            );
        }
        assert!(
            report.contains("Not carried") && report.contains("fleet-agent.token"),
            "the report must say what was left behind: {report}"
        );
        // And the marker that is NOT instance-scoped travels, because what it records does. The
        // secret is in sbx's store, which the volume's move does not touch — so a target without
        // the marker sends the next server start to `gh auth token` and a keyring prompt, for a
        // credential that was already seeded. `docs/delivery.md` §4.1a names this one by hand.
        assert_eq!(
            fs::read_to_string(target.join("gh-secret-seeded")).ok(),
            fs::read_to_string(home.join("gh-secret-seeded")).ok(),
            "the gh-secret marker did not travel, so the moved fleet re-seeds a secret it has"
        );

        let moved: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(target.join("repos.json")).unwrap()).unwrap();
        let store = |id: &str| -> String {
            moved.iter().find(|r| r["id"] == id).unwrap()["store"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert!(
            crate::util::resolved(&store("inside"))
                .starts_with(&crate::util::resolved(&target.to_string_lossy())),
            "a store under the volume still points at the old one: {}",
            store("inside")
        );
        // A store somebody deliberately put elsewhere is not this move's business, and rewriting it
        // would move their data in a way nobody asked for.
        assert_eq!(
            store("elsewhere"),
            outside.to_string_lossy(),
            "a store outside the volume was rewritten"
        );
        assert!(report.contains("Repointed"), "{report}");

        // The legacy markers: the one that named this volume now names the new one, the one that
        // named somewhere else is untouched, and the store outside the volume was never opened.
        let moved_marker =
            fs::read_to_string(target.join("repos/inside/store/.claude/skein/mirror")).unwrap();
        assert_eq!(
            crate::util::resolved(moved_marker.trim()),
            crate::util::resolved(&target.join("repos/inside/mirror").to_string_lossy()),
            "a box in the new installation would work from a mirror in the old one"
        );
        assert_eq!(
            fs::read_to_string(target.join("repos/inside/store/.claude/skein/source"))
                .unwrap()
                .trim(),
            kept.to_string_lossy(),
            "a marker pointing outside the volume is somebody's deliberate choice \u{2014} and is \
             compared verbatim on purpose: it was never rewritten, so it must still read exactly \
             as it was written"
        );
        assert_eq!(
            fs::read_to_string(outside.join("skein/mirror")).unwrap().trim(),
            home.join("repos/elsewhere/mirror").to_string_lossy(),
            "a store shared with the old installation was written to by a move that promises not to"
        );
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

        // **And a refused move creates nothing.** `migrate` used to `mkdir -p` its target above
        // every refusal, so a target nobody moved onto was left on disk anyway — and the next
        // attempt then met a directory this one made. Asserted on a path that does NOT exist before
        // the call: the refusals above are all about targets that do, so none of them could see
        // this. The containment refusal is the one that can, because it is decided from the
        // resolved path — which is now worked out without creating anything.
        //
        // **What makes this fail**: moving the `create_dir_all` back to the top of `migrate`.
        let untouched = home.join("inner").join("deeper");
        let why = migrate(&untouched.to_string_lossy()).unwrap_err();
        assert!(why.contains("inside the volume"), "{why}");
        assert!(
            !untouched.exists() && !home.join("inner").exists(),
            "a refused move ({why}) created {} anyway",
            untouched.display()
        );
    }

    /// **Moving the volume is an operation skein reports and never performs** (SKEIN-574).
    ///
    /// The volume is bind-mounted at sandbox create and skein runs inside that sandbox, so a move
    /// is architecture §7.5's shape one level down: the act ends the process performing it. What is
    /// asserted here is that the refusal is not a dead end — it carries the recipe, the recipe's
    /// `sbx` lines are the warden's own rendering, and the operation says out loud that skein will
    /// not run it.
    ///
    /// **What makes this fail**: giving the operation `Class::Idempotent`, which would let anything
    /// that reads an unsatisfied check drive a move that kills the fleet; or composing the recipe
    /// here instead of from `Act::command`, which is how what a person is told drifts from what the
    /// warden would run.
    #[test]
    fn moving_the_volume_is_reported_with_its_recipe_and_never_driven() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). Nothing
        // asserted below carries the root, so a fixture is the whole of what this needs.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        populate(&home);
        let mut cfg = crate::config::load_config();
        cfg.fleet_sandbox = "skein-fleet".into();
        crate::config::save_config(&cfg).unwrap();

        let elsewhere = tempdir();
        let target = elsewhere.join("volume");
        let op = move_to(&target.to_string_lossy());

        // Destructive, so nothing drives it — and the check being unsatisfied is what makes that
        // assertion mean something, since a satisfied one would not be driven anyway.
        assert!(
            matches!(op.check, crate::operation::Check::Unsatisfied(_)),
            "the volume is not where it was asked to be, and the check says otherwise: {:?}",
            op.check
        );
        assert!(
            !op.may_drive(),
            "a move that stops the fleet — and this process with it — was offered to a driver"
        );

        // The recipe, and the halves that must come from elsewhere rather than be spelled here.
        let said = op.render();
        let destroy = crate::warden_client::Act::Destroy {
            sandbox: "skein-fleet".into(),
        }
        .command();
        assert!(
            said.contains(&destroy),
            "the destroy line is not the warden's own rendering, so it can drift from what the \
             warden would run: {said}"
        );
        assert!(
            said.contains(&format!("export SKEIN_HOME={}", target.display())),
            "the one variable that points at a volume is not in the recipe: {said}"
        );
        assert!(
            said.contains("skein never runs it for you"),
            "the recipe does not say who runs it: {said}"
        );

        // And the same question asked twice is one operation, not two — the property the warden's
        // at-most-once store depends on.
        assert_eq!(op.id, move_to(&target.to_string_lossy()).id);
        assert_ne!(
            op.id,
            move_to(&elsewhere.join("other").to_string_lossy()).id
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
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
        carry_to(&target).unwrap();

        // Still pointed at the old path, which is the mistake everybody makes once.
        let why = ensure_volume().unwrap_err();
        assert!(
            why.contains(&crate::util::resolved(&target.to_string_lossy())),
            "the refusal does not name where the volume went: {why}"
        );
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

    /// The guard still fires when the volume is reached through a symlink.
    ///
    /// **Written because macOS is the ordinary case and this suite had never run there.** `/var` is
    /// a symlink to `/private/var` and `$TMPDIR` lives under it, so a volume's marker says
    /// `/private/var/…` while the store paths in its own `repos.json` say `/var/…` — one directory,
    /// two spellings, no shared prefix. Every `starts_with` in `repoint` answered "no", and the two
    /// consequences were the opposite of harmless: `skein repoint` reported "0 paths repointed" and
    /// rewrote nothing, and `ensure_volume` found nothing stale and quietly ADOPTED the copy. The
    /// one check standing between somebody and a copied volume that keeps writing to the original
    /// disarmed itself, silently, on the platform skein is mostly run on.
    ///
    /// Reproduced here on any unix by building the same shape by hand, because a bug that only
    /// appears on a machine the tests are not run on is a bug that comes back.
    #[cfg(unix)]
    #[test]
    fn a_volume_reached_through_a_symlink_is_still_told_apart_from_its_copy() {
        let _g = env_lock();
        let scratch = tempdir();
        let real = scratch.join("real");
        fs::create_dir_all(&real).unwrap();
        // The `/var` → `/private/var` shape: the volume is opened through `link`, and everything
        // that canonicalises sees `real`.
        std::os::unix::fs::symlink(&real, scratch.join("link")).unwrap();
        let home = scratch.join("link").join("vol");
        fs::create_dir_all(&home).unwrap();

        std::env::set_var("SKEIN_HOME", &home);
        populate(&home);
        // A store spelled the way somebody reached it — through the link — which is what
        // `skein add` records. The marker will be written canonically, and those two strings share
        // no prefix at all.
        fs::write(
            home.join("repos.json"),
            serde_json::to_vec(&serde_json::json!([{
                "id": "inside",
                "source": "https://example.invalid/x.git",
                "source_tree": "",
                "store": home.join("repos/inside/store").to_string_lossy(),
            }]))
            .unwrap(),
        )
        .unwrap();
        ensure_volume().unwrap();
        assert!(
            written_at(&home).is_some(),
            "the fixture never got a marker, so it proves nothing"
        );

        // Copied by hand, exactly as somebody backing up would.
        let copy = scratch.join("copy");
        let copied = std::process::Command::new("cp")
            .arg("-a")
            .arg(&home)
            .arg(&copy)
            .status()
            .expect("copy the volume");
        assert!(copied.success());

        std::env::set_var("SKEIN_HOME", &copy);
        let why = ensure_volume().expect_err(
            "a copy reached through a symlink was adopted as though it stood on its own \u{2014} \
             which is the whole failure, because it goes on writing to the original",
        );
        assert!(
            why.contains("skein repoint") && why.contains("export SKEIN_HOME="),
            "the refusal must name both intents: {why}"
        );

        // And the repair works from here, which is the half that reported "0 paths repointed".
        let said = repoint_here().unwrap();
        assert!(
            said.contains("1 path(s) repointed"),
            "the store under the volume was not repointed: {said}"
        );
        let after: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(copy.join("repos.json")).unwrap()).unwrap();
        let store = after[0]["store"].as_str().unwrap();
        assert!(
            crate::util::resolved(store)
                .starts_with(&crate::util::resolved(&copy.to_string_lossy())),
            "the copy's store still names the original: {store}"
        );
        ensure_volume().expect("a repointed copy stands on its own");
        std::env::remove_var("SKEIN_HOME");
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

    /// The failure this was written for, walked into on a real installation: a `.skein` copied
    /// somewhere else and opened there.
    ///
    /// It worked perfectly before this — reading and writing the volume it was copied *from*, with
    /// nothing said, right up until somebody deleted the original. `skein migrate` had always done
    /// the rewrite; setting `$SKEIN_HOME` never did, and nobody would expect the difference.
    #[test]
    fn a_volume_opened_where_it_was_not_written_refuses_and_says_which_thing_you_meant() {
        let _g = crate::testutil::env_lock();
        let old = crate::testutil::tempdir();
        let new = crate::testutil::tempdir();
        let store = old.join("repos/demo/store");
        fs::create_dir_all(&store).unwrap();
        fs::write(
            old.join("repos.json"),
            format!(
                r#"[{{"id":"demo","store":"{}","work":"{}/repos/demo/work"}}]"#,
                store.display(),
                old.display()
            ),
        )
        .unwrap();

        // Opened where it was written: adopted, and it records where that is.
        std::env::set_var("SKEIN_HOME", old.as_ref() as &Path);
        ensure_volume().expect("a volume opened in place is fine");
        assert_eq!(
            written_at(&old).map(PathBuf::from),
            Some(old.canonicalize().unwrap()),
            "the volume did not record where it lives, so nothing can tell later"
        );

        // Copied elsewhere, bit for bit, and opened there.
        let copied = std::process::Command::new("cp")
            .arg("-a")
            .arg(old.as_ref() as &Path)
            .arg(new.join("vol"))
            .status()
            .expect("copy the volume");
        assert!(copied.success());
        let there = new.join("vol");
        std::env::set_var("SKEIN_HOME", &there);
        let why = ensure_volume().expect_err(
            "a copy that still names the original was opened as though it stood on its own",
        );
        // Both intents, because both are real and they want opposite things.
        assert!(
            why.contains("skein repoint"),
            "the refusal does not say how to make this copy stand on its own: {why}"
        );
        assert!(
            why.contains(&format!(
                "export SKEIN_HOME={}",
                old.canonicalize().unwrap().display()
            )),
            "the refusal does not say how to get back to the original: {why}"
        );
        // And how much is actually at stake, rather than a bare "moved".
        assert!(why.contains("2 path(s)"), "{why}");

        // The repair, and then it stands on its own.
        let report = repoint_here().expect("repoint the copy");
        assert!(report.contains("2 path(s) repointed"), "{report}");
        ensure_volume().expect("a repointed volume opens");
        let repos = fs::read_to_string(there.join("repos.json")).unwrap();
        assert!(
            repos.contains(&there.canonicalize().unwrap().display().to_string()),
            "the copy still does not name itself: {repos}"
        );
        assert!(
            !repos.contains(&old.canonicalize().unwrap().display().to_string()),
            "the copy still names the volume it came from: {repos}"
        );

        // The original is untouched — nothing was moved, only copied, and it is still whole.
        //
        // Asserted as the property rather than as a substring: what matters is that the original's
        // stores still name the ORIGINAL, whichever way that path is spelled. A `contains` over the
        // raw JSON was really asking "is it written in the canonical form", which the original has
        // no reason to be — it was never rewritten, so it holds whatever `skein add` recorded.
        let source: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(old.join("repos.json")).unwrap()).unwrap();
        for repo in &source {
            for key in ["store", "work"] {
                let Some(path) = repo.get(key).and_then(|v| v.as_str()) else {
                    continue;
                };
                assert!(
                    crate::util::resolved(path)
                        .starts_with(&crate::util::resolved(&old.to_string_lossy())),
                    "repointing the copy edited the original: {key} is {path}"
                );
            }
        }
        std::env::remove_var("SKEIN_HOME");
    }

    /// A wrong marker with nothing behind it is corrected, not thrown at somebody.
    ///
    /// A volume with no repos registered, or whose stores are all deliberately elsewhere, records a
    /// path that is merely out of date — there is no second copy for skein to read by mistake.
    /// Refusing there is stopping somebody over a fact with no consequence, which is how a check
    /// earns the reputation that gets it switched off.
    #[test]
    fn a_marker_that_is_wrong_about_nothing_is_fixed_rather_than_raised() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        fs::write(home.join("VERSION"), "1\n").unwrap();
        fs::write(home.join("written-at"), "/somewhere/that/never/was\n").unwrap();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &Path);

        ensure_volume().expect("a volume with nothing stale must open");
        assert_eq!(
            written_at(&home).map(PathBuf::from),
            Some(home.canonicalize().unwrap()),
            "the stale marker was left in place, so this refuses again next time"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// A volume from before the marker is adopted, exactly as one with no `VERSION` is.
    ///
    /// There is nothing to compare against on one, so treating the absence as suspicious would
    /// refuse every volume that exists today.
    #[test]
    fn a_volume_from_before_the_marker_is_adopted_rather_than_doubted() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        fs::write(home.join("VERSION"), "1\n").unwrap();
        fs::write(home.join("config.json"), "{}").unwrap();
        assert!(written_at(&home).is_none(), "the fixture is not old enough");
        std::env::set_var("SKEIN_HOME", home.as_ref() as &Path);

        ensure_volume().expect("an installation from before the marker must open");
        assert_eq!(
            written_at(&home).map(PathBuf::from),
            Some(home.canonicalize().unwrap()),
            "adopting it is the whole of the upgrade, and it did not happen"
        );
        std::env::remove_var("SKEIN_HOME");
    }
}
