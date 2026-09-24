//! Disk usage across the fleet, and the directories nothing accounts for: strays from departed
//! boxes, orphaned and stale build output. Reported and offered, never deleted here.

use super::*;

/// How much disk each box is using, in MiB — every box at once, in one round trip.
///
/// Measured rather than enforced, and that distinction is the honest part of this: the fleet's disk
/// is a single filesystem shared by every box, so nothing in the kernel stops one from filling it.
/// A limit here is a number skein checks and reports, not a wall the box hits — which is why it can
/// be changed while a box runs, and why it is worth having at all: the alternative is finding out
/// when some *other* box's build dies with ENOSPC and no indication of who took the space.
///
/// `du -sxm`, one process for the lot: measured at 0.2s for a 3.2 GB tree, so a board tick can pay
/// for it. `-x` keeps it on the sandbox's own filesystem — a box's store is a host mount, and
/// walking virtiofs to count bytes that are not on this disk would be both slow and wrong.
pub fn fleet_disk_usage() -> std::collections::HashMap<String, u64> {
    // No `fleet_sandbox()` call here at all: the name was read only to be tested for emptiness,
    // and the measurement below is a local `du` under `fleet_root()` rather than anything
    // addressed to a sandbox.
    //
    // Five minutes, not thirty seconds, and the number is the whole fix.
    //
    // This is a full recursive `stat` of every entry under the fleet root — measured on a live
    // fleet at **383,606 files** — and `board::load_views` calls it, which is what `/api/boxes`
    // answers, which the page polls every two seconds. At 30s that is ~767k stat calls a minute for
    // as long as one cockpit tab is open, growing with the fleet rather than with what is being
    // asked. It is by a distance the most expensive recurring thing skein does.
    //
    // What it feeds is a per-box megabyte figure on a row and a warning near a disk limit. Neither
    // is a number anybody watches move; a box does not fill a disk between two board ticks. Ten
    // times less often is the same answer for a tenth of the machine.
    //
    // The right long-term answer is not a bigger interval — it is not walking the tree at all
    // (a filesystem quota, or a walk only for the box whose row is open). That is a design
    // question and is recorded as one; this is the part that is free.
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        Duration::from_secs(300)
    };
    DISK_GATE
        .get(fresh, move || {
            // In-fleet the fleet root is a local path, so the `du` is a walk and the `sbx exec`
            // around it was only ever transport (SKEIN-60). Still **one pass for the whole fleet**
            // — `local_disk_usage` lists the root once and walks what it finds, so this stays
            // `Scale::PerPass`. A walk per box, driven from the board's row loop, is the shape that
            // took the branch fallback to twelve forks a tick (SKEIN-49).
            // The walk, in this process. The `sbx exec du` beside it was the host's way of
            // reaching the same tree, and it is what made this signal cost a fork (SKEIN-576).
            Some(local_disk_usage(&fleet_root()))
        })
        .unwrap_or_default()
}

/// [`disk_usage_script`] as a walk: what `du -sxm <root>/*/` answers, without a process.
///
/// Each rule here is one of `du`'s flags, kept rather than reimplemented loosely — a disk figure
/// that disagrees with the one the host-driven path produced would show up as boxes changing size
/// at the moment skein moved, which reads as a skein bug rather than a change of method:
///
///   * **`-x`** — stay on the fleet root's own filesystem. A box's store is a host mount, and
///     walking virtiofs to count bytes that are not on this disk is both slow and wrong. Compared
///     by device id, which is what `-x` compares.
///   * **`-m`** — MiB, rounded **up**, per box. `du` reports whole units and rounds up, so a box
///     holding one byte reads as 1 rather than 0.
///   * **blocks, not lengths** — `du` counts allocated blocks (512 bytes each), so a sparse file
///     costs what it occupies rather than what it claims. `len()` would over-report every one.
///   * **hardlinks once** — `du` counts an inode the first time it meets it. Tracked per box, which
///     is where `du -s` per box would also count them.
///   * **`2>/dev/null || true`** — an unreadable directory is skipped and the walk goes on. That is
///     not a rare case: boxes create unreadable directories in ordinary work, and one of them used
///     to throw away the disk figures for the entire fleet.
///
/// And one rule that is not a flag but the glob itself: **`<root>/*/` does not match a leading
/// dot.** `read_dir` does, so for as long as this walk kept every directory it named it answered
/// with a key `du` had never reported — `.skein`, which is the substrate and not a box (SKEIN-735).
/// That is not a tidiness point. `health::disk_verdict` takes the three biggest entries of
/// this map and offers `skein stop <box>` on each, and on 2026-09-05 the substrate held 19.2 GB of
/// build directories, so the fix line a full fleet would have printed named a box that does not
/// exist and a command that cannot work on it. What is actually in there is reported by
/// [`substrate_strays`], which can say the true thing about it.
fn local_disk_usage(root: &str) -> std::collections::HashMap<String, u64> {
    use std::os::unix::fs::MetadataExt;
    let mut out = std::collections::HashMap::new();
    // One listing of the root, then a walk per entry it names — the same single pass `du
    // <root>/*/` makes, and the reason this is not driven from the board's per-box loop.
    let Ok(entries) = std::fs::read_dir(root) else {
        return out; // no fleet root yet is an empty answer, exactly as a `du` that printed nothing
    };
    let on_disk = std::fs::metadata(root).map(|m| m.dev()).ok();
    for entry in entries.flatten() {
        let path = entry.path();
        // `<root>/*/` is directories only — the glob does not match plain files.
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // The glob's own rule, and the reason this map is boxes only. Two things now say a
        // dotted entry is not a box, and it is worth knowing that they are independent: `du`'s
        // glob never reported one (which is why the walk must not), and `util::valid_name`
        // refuses to make one. The second of those was asserted here long before it was true —
        // it rejected a leading `-` and an all-dots name and nothing else about dots, so
        // `valid_name(".skein")` was `true` until SKEIN-742 measured it and made the rule real.
        // [`live_box_names`] filters the same character for both reasons.
        if name.starts_with('.') {
            continue;
        }
        const MIB: u64 = 1024 * 1024;
        out.insert(name, tree_bytes(&path, on_disk).div_ceil(MIB));
    }
    out
}

/// `du -s` over one directory, in bytes: every rule in [`local_disk_usage`]'s doc, applied once.
///
/// Its own function because [`substrate_strays`] has to size a directory too, and a second walk
/// written beside this one is a second set of answers to "how big is that" — the disagreement
/// `local_disk_usage`'s doc argues against, in the one place a reader compares two figures skein
/// printed on the same page.
///
/// `on_disk` is the device the walk must stay on (`du -x`); `None` skips that filter, for a caller
/// that could not stat its own root.
fn tree_bytes(path: &std::path::Path, on_disk: Option<u64>) -> u64 {
    use std::os::unix::fs::MetadataExt;
    // The directory's own blocks count too — `du -s <dir>` includes the directory it was pointed
    // at, not only what is under it. Verified against `du -sx --block-size=512` on a fixture tree;
    // without this every box reads one directory short.
    let mut bytes: u64 = 0;
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    if let Ok(meta) = std::fs::metadata(path) {
        seen.insert((meta.dev(), meta.ino()));
        bytes += meta.blocks() * 512;
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // Unreadable is skipped, never fatal.
        let Ok(kids) = std::fs::read_dir(&dir) else {
            continue;
        };
        for kid in kids.flatten() {
            // `DirEntry::metadata` does not traverse a symlink, which is the behaviour wanted
            // here: `du` does not follow them either, and following one out of the tree would
            // count another box's bytes against this one.
            let Ok(meta) = kid.metadata() else {
                continue;
            };
            if on_disk.is_some_and(|dev| meta.dev() != dev) {
                continue; // `-x`
            }
            if !seen.insert((meta.dev(), meta.ino())) {
                continue; // a hardlink already counted
            }
            bytes += meta.blocks() * 512;
            if meta.is_dir() {
                stack.push(kid.path());
            }
        }
    }
    bytes
}

// ──────────── the substrate: what skein installs beside the boxes, and what nobody claims ────────
//
// `<fleet root>/.skein` is the one entry in the fleet root that is not a box, and on 2026-09-05 it
// held **19.2 GB of build directories nothing in skein had made and nothing in skein would remove**
// — `target-phase3-agent`, `target-wave2b-queues` and four more, four days old, on a filesystem at
// 88%. The owner found them by asking what was using the disk.
//
// **They were not left by destroyed boxes**, which is what they look like. Four registers say so:
// no such name is in any `sandboxes.json`, in any `history.jsonl`, in `$SKEIN_HOME/places/`, or in
// `$SKEIN_HOME/boxes/`. And no ordinary box could have written them in the first place —
// `src/box-session.sh:1227` puts every non-privileged box behind a cover and `:1262` binds `.skein`
// back **read-only**, so the only box in this fleet that can write here is the one workshop box.
// They were made by agent lanes running inside it, each told by its own brief to set
// `CARGO_TARGET_DIR` to a path under `.skein`. `grep -rn CARGO_TARGET_DIR src/` still returns
// nothing: this is a convention with no owner, and skein is not adopting it — `.skein` is
// unwritable from the boxes such a variable would be for, and skein has no opinion about cargo.
//
// So what skein can say truthfully is not "this belonged to a box I destroyed". It is: **I know
// what I install here, I know which boxes exist, and this directory is neither.** That is what the
// two derivations below are, and both of them refuse rather than answer when they come up empty.
//
// **Nothing here deletes.** Reporting is the whole of it, and the command is the reader's to run —
// "inform and offer, never perform", which is the same rule `HealthCheck::destroys` already carries
// for the fixes `skein doctor` will not drive.

/// `<fleet root>/.skein` — the one entry in the fleet root that is not a box.
///
/// `bootstrap.sh` calls it `skein_dir`, and this is that name, so an install's two halves are
/// searchable as one thing. Not [`crate::substrate::substrate_dir`], which is the package-request
/// queue *inside* it.
pub fn skein_dir() -> String {
    format!("{}/.skein", fleet_root())
}

/// The directories skein itself makes directly under [`skein_dir`], **by asking the functions that
/// place them** rather than by keeping a list of their names here.
///
/// A list would be the shape SKEIN-647 was bought with: it goes stale silently, and a rename would
/// turn one of skein's own directories into something this file offers to delete. Asked this way, a
/// rename moves the answer with it, because the function IS the definition.
///
/// **Directories only, and that is what makes the derivation complete rather than merely long.**
/// Every one of them is placed by a function above or in a module `fleet` already depends on;
/// every other name directly in `.skein` is a *file* (`box-session.sh`, `skein-server`,
/// `server.door`, `skein-home`), and `substrate_strays` never looks at files. A name under one of
/// them is not in this question at all — `server.tmux` stopped being asked about here when it
/// moved under `private/` (SKEIN-529), the same way the tokens already had. That matters for more
/// than tidiness: a
/// live fleet carries `.skein/fleet-agent.py` and `.skein/fleet-agent.token`, which nothing in this
/// tree spells any more, and offering to delete a credential to save 64 bytes would be the worst
/// version of this feature. `substrate_names_the_code_spells_are_all_accounted_for` is the guard
/// that keeps the split honest when another directory is added.
fn installed_substrate_dirs() -> std::collections::BTreeSet<String> {
    let dirs = [
        fleet_private_dir(),
        skein_source_path(),
        skein_toolchain_path(),
        crate::substrate::substrate_dir(),
        crate::gitgate::gitgate_dir(),
        // A box's questions for its owner (SKEIN-1061), the third request queue.
        crate::asks::asks_dir(),
        // skein's box plugin, installed beside the launcher (SKEIN-1056).
        crate::runtime::plugin_dir(),
        // `detached/<session>.sh` is a script named for a session, so the session is a placeholder
        // and only its parent is the directory skein makes.
        detached_script_path("any")
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default(),
    ];
    dirs.iter()
        .filter_map(|p| p.rsplit('/').next())
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect()
}

/// Every name that could belong to a box right now, from **two** registers rather than one.
///
/// The fleet root's own directories are the first: a box's tree is `<root>/<name>`
/// ([`box_root`]), and `sandbox::destroy_script` removes it with `rm -rf`, so a directory here is a
/// box that has not been destroyed. `$SKEIN_HOME/places/` is the second: skein writes a placement
/// record when it starts a box and `forget_place` removes it when it destroys one.
///
/// Both, because the cost of the two failures is not the same. Missing a name means a directory
/// belonging to a live box is called unattributed and somebody is offered a command that would
/// delete work; carrying a name that is no longer a box means one stray goes unreported and 4 GB
/// stays on the disk. The first is the failure worth two reads.
pub(super) fn live_box_names() -> std::collections::BTreeSet<String> {
    let mut names: std::collections::BTreeSet<String> = std::fs::read_dir(fleet_root())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        // `.skein` is the substrate itself, and `util::valid_name` now refuses a name that
        // begins with a dot — so nothing dropped here can be a box. This comment made that
        // claim before it was true (SKEIN-742): the rule it cited rejected an all-dots name and
        // said nothing about a leading one. It is load-bearing here, because `substrate_strays`
        // subtracts this list to decide what under `.skein` is unattributed, and a live box
        // missing from it gets its own build output offered to the owner under an `rm -rf`.
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.extend(
        crate::place::placed_boxes(&fleet_sandbox())
            .into_iter()
            .map(|(name, _)| name),
    );
    names
}

/// A directory in the substrate that skein does not install and no box accounts for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stray {
    pub name: String,
    /// What [`tree_bytes`] makes of it — the same walk the per-box figures come from.
    pub bytes: u64,
}

/// Directories in [`skein_dir`] that skein did not install and no live box accounts for.
///
/// **`Err` rather than an empty answer whenever a derivation came up empty**, and that is the whole
/// discipline of this function. A sweep that reports zero because it recognised nothing is
/// indistinguishable from a sweep that reports zero because there is nothing — that is SKEIN-647,
/// where a hardcoded alternation of fixture names answered `0` on a box carrying 195 matching
/// processes and nobody could tell. There are three ways to come up empty here and each says so:
/// no `.skein` at all, no installed directory recognised, no box name found.
///
/// A directory whose name *mentions* a live box is credited to it and left alone, mention being
/// deliberately loose: the convention that made the strays spells `target-<name>`, but matching
/// `target-` here would be the hardcoded list again, in the other direction and with a rename's
/// worth of silence behind it. A loose match errs towards saying nothing, and that is the safe
/// direction — an unreported stray costs 4 GB, while a live box's build in a `rm -rf` costs its
/// work.
///
/// Biggest first, because the only reason anybody runs this is that a disk is full.
pub fn substrate_strays() -> Result<Vec<Stray>, String> {
    let dir = skein_dir();
    let installed = installed_substrate_dirs();
    if installed.is_empty() {
        return Err(format!(
            "skein could not work out which directories under {dir} are its own, so it will not \
             guess which are not — refusing rather than reporting that nothing is stranded"
        ));
    }
    let live = live_box_names();
    if live.is_empty() {
        return Err(format!(
            "skein can see no boxes at all — neither a directory in {root} nor a placement record \
             in {places} — so every directory under {dir} would read as belonging to nobody. \
             Refusing rather than reporting that as an answer",
            root = fleet_root(),
            places = skein_home().join("places").display(),
        ));
    }
    use std::os::unix::fs::MetadataExt;
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("{dir} could not be listed, so nothing can be said about it: {e}"))?;
    let on_disk = std::fs::metadata(&dir).map(|m| m.dev()).ok();
    let mut strays: Vec<Stray> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            let mine = installed.contains(&name);
            let a_box = live.iter().any(|b| name.contains(b.as_str()));
            (!mine && !a_box).then(|| Stray {
                bytes: tree_bytes(&entry.path(), on_disk),
                name,
            })
        })
        .collect();
    strays.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.name.cmp(&b.name)));
    Ok(strays)
}

/// What skein keeps about one box **outside the box's own tree**, in the order it must be removed.
///
/// `sandbox::destroy_script` runs `rm -rf <fleet root>/<box>` and `sandbox::forget_box_files` drops
/// the store's live status and launch files. Neither reaches any of these, because none of them is
/// under either path:
///
/// * [`box_declared`] — `privileged`, `git-scope`, `disk`, `identity`. **First on the list, and the
///   reason this function exists.** These are not bytes on a disk, they are skein's *decisions*
///   about a box, and a name is all that binds them to one: a box re-created under a destroyed
///   box's name read `privileged` and came up as the workshop box — every isolation bind skipped
///   and the fleet agent's token readable — because a file four directories away still said `1`.
///   It goes first so a drop-box that will not delete cannot leave it behind.
/// * the substrate, gitgate and asks **drop-boxes**, `requests/<box>/`, made by the launcher
///   outside the box's mount namespace at every start (`src/box-session.sh`, the `for asking in
///   substrate gitgate asks` loop) and bound read-write into that box alone. Addressed through
///   [`crate::substrate::box_requests_dir`], [`crate::gitgate::box_requests_dir`] and
///   [`crate::asks::box_requests_dir`] rather than spelled again here, so there is one path per
///   queue and not two.
///
/// **[`box_state`] is deliberately absent**, and that is a decision rather than an omission: it
/// holds the box's conversation, and `sandbox::forget_box_files`' own doc says that must outlive
/// the box.
fn box_side_state(name: &str) -> Vec<std::path::PathBuf> {
    vec![
        box_declared(name),
        std::path::PathBuf::from(crate::substrate::box_requests_dir(name)),
        std::path::PathBuf::from(crate::gitgate::box_requests_dir(name)),
        std::path::PathBuf::from(crate::asks::box_requests_dir(name)),
    ]
}

/// Forget what skein decided about a box that is gone, and the drop-boxes the launcher made for it.
///
/// Called by `sandbox::destroy_box_inner` once a teardown has succeeded. What it removes is
/// [`box_side_state`]; what it is *for* is the first entry of that list, and the property is worth
/// stating as the thing that can be checked: **a box created with a destroyed box's name inherits
/// none of the destroyed box's answers.** Removing the directories is only how that is achieved.
///
/// **Whether the box is gone is [`live_box_names`]'s answer, and not a second one written here.**
/// That function is the one [`substrate_strays`] already subtracts to decide what under `.skein`
/// belongs to nobody, it reads two registers rather than one, and it errs towards calling a box
/// live — which is the direction that matters here for the same reason it matters there. The two
/// failures are not the same size: refusing to sweep a box that is really gone leaves a stale
/// `privileged` for a name nothing is using yet, and sweeping a box that is really live silently
/// takes away its git scope, its disk allowance and who it commits as, in the middle of its work.
/// So a name the definition still accounts for is refused, said out loud, and left alone.
///
/// Every directory is attempted even when an earlier one fails, because they are independent and
/// the first is the one with teeth.
pub fn forget_departed_box(name: &str) -> Result<Vec<String>, String> {
    if !crate::util::valid_name(name) {
        return Err(format!("unusable box name {name:?}"));
    }
    if live_box_names().contains(name) {
        return Err(format!(
            "{name} still reads as a live box — a directory under {root}, or a placement record in \
             {places} — so what skein has decided about it is left alone. A name that can still \
             come back has to keep its answers; one that cannot must not.",
            root = fleet_root(),
            places = skein_home().join("places").display(),
        ));
    }
    let mut gone = Vec::new();
    let mut trouble = Vec::new();
    for dir in box_side_state(name) {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => gone.push(dir.display().to_string()),
            // Never made, or already swept. Both are the state this is trying to reach.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => trouble.push(format!("{} could not be removed: {e}", dir.display())),
        }
    }
    match trouble.is_empty() {
        true => Ok(gone),
        false => Err(trouble.join("; ")),
    }
}

/// [`substrate_strays`] as a line to put in front of a person, or `None` when there is nothing to
/// say.
///
/// The command is spelled out and **not run**. Every path in it is shell-quoted, for the reason the
/// `pkill` pattern above is: the fleet root comes from `$SKEIN_FLEET_ROOT`, an operator sets it, and
/// this string is meant to be pasted into a shell.
pub fn stray_advice(strays: &[Stray]) -> Option<String> {
    if strays.is_empty() {
        return None;
    }
    let dir = skein_dir();
    let gib = |bytes: u64| format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0));
    let named = strays
        .iter()
        .map(|s| format!("{} ({})", s.name, gib(s.bytes)))
        .collect::<Vec<_>>()
        .join(", ");
    let total: u64 = strays.iter().map(|s| s.bytes).sum();
    let paths = strays
        .iter()
        .map(|s| sh_quote(&format!("{dir}/{}", s.name)))
        .collect::<Vec<_>>()
        .join(" ");
    let subject = match strays.len() {
        1 => "1 directory".to_string(),
        n => format!("{n} directories"),
    };
    let verb = match strays.len() {
        1 => "is",
        _ => "are",
    };
    Some(format!(
        "{subject} in {dir} — {named}, {total} in all — {verb} neither skein's own nor any box's. \
         skein did not create them and does not remove them; if they are yours to delete:\n\
         \x20   rm -rf {paths}",
        total = gib(total),
    ))
}

// ──────── build output inside a live box, for a source tree that is no longer there ───────────
//
// [`substrate_strays`] above answers "what under `.skein` belongs to nobody", and on the fleet this
// was measured against it correctly answers **nothing**: the 19.2 GB it was written for was cleared
// by hand on 2026-09-09 and no box has put anything there since. The bytes moved rather than went
// away. 30.5 GiB of cargo build output now sits *inside* live boxes, under `<fleet root>/<box>/`,
// where `substrate_strays` never looks and where every existing figure counts it as the box's own
// and therefore as wanted.
//
// Most of it is wanted. The part that is not is build output for a **worktree that has been
// removed** — an agent lane makes `<box>/wt-1140`, points a build at `<box>/target-wt1140`, the
// lane ends and the worktree goes, and the build directory stays because nothing ever connected
// the two. One such directory was 5.88 GiB, twelve days old, on a filesystem that hit 100% three
// times in a day.
//
// **The connection nothing had is written down in the build output itself.** Cargo emits a `.d`
// file beside every artefact — ordinary `make` syntax, the outputs before the colon and every
// source file after it — and the sources are spelled as the absolute paths of the tree the build
// ran against. So a build directory *states* which tree it is for, and whether that tree is on
// disk is a question with an answer.
//
// That is the whole of the evidence, and it is deliberately not any of these:
//
// * **not the name.** `target-wt1140` and `target-private` sit side by side in one box, the same
//   twelve days old, one 5.88 GiB and dead and one 3.99 GiB and live. Nothing about either name
//   says which. A lane's cleanup deleted live fixtures by matching names on the day this was
//   written; matching `target-` here would be the same mistake with `rm -rf` behind it.
// * **not the age.** Both of those were last written on the same day.
// * **not cargo's `.fingerprint` directory**, which looks like a register of what the next build
//   will reuse and is not: cargo writes one per generation and removes none, so slate-One's 2,657
//   fingerprints cover 6,762 artefacts including every superseded one. Measured before it was
//   believed, which is why it is written down here rather than tried again.
//
// A build directory is found the same way — by asking cargo rather than by matching a name.
// `.rustc_info.json` is written by cargo into the root of a target directory and nowhere else, so
// a directory carrying one is a target directory whatever it is called, and a directory called
// `target-anything` without one is not examined at all.
//
// **Nothing here deletes**, the same as everything above it: the `rm -rf` is spelled out for a
// reader who can see the evidence beside it, and skein does not run it.

/// Cargo's own marker file, written into the root of a target directory and nowhere else.
///
/// Named once because it is the definition of "this is a build directory" that this file uses in
/// place of a name match.
const CARGO_TARGET_MARKER: &str = ".rustc_info.json";

/// A build directory inside a live box, and the source tree its own dependency files name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedBuild {
    /// The build directory itself, absolute.
    pub path: String,
    /// What [`tree_bytes`] makes of it — the same walk every other figure here comes from.
    pub bytes: u64,
    /// The source tree its `.d` files name, which is **not on disk**. This is the evidence, and it
    /// is carried rather than recomputed so that whatever prints the report prints the reason.
    pub built_from: String,
    /// How many of its dependency-file references name [`built_from`]…
    pub naming: usize,
    /// …out of this many that name anything inside the box at all. The pair is reported because a
    /// build directory can have been pointed at a second tree after the first went, and a reader
    /// deciding whether to delete 5.88 GiB should see that rather than be told a verdict.
    pub of: usize,
}

/// Every directory under `dir` that carries [`CARGO_TARGET_MARKER`], not descending into one once
/// found — a target directory's own subdirectories are its contents, not more target directories.
///
/// Bounded at `depth` because a box holds whole checkouts of other people's repositories and this
/// walk is on the path of a disk check, not of a build.
fn cargo_build_dirs(dir: &std::path::Path, depth: usize, found: &mut Vec<std::path::PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for kid in entries.flatten() {
        let path = kid.path();
        if !path.is_dir() {
            continue;
        }
        match path.join(CARGO_TARGET_MARKER).exists() {
            true => found.push(path),
            false => cargo_build_dirs(&path, depth - 1, found),
        }
    }
}

/// The source trees a build directory's `.d` files name, and how many references each one has.
///
/// A `.d` file is `make` syntax — `<output>: <source> <source> …` — and the sources cargo writes
/// for a path-dependency are absolute. Only paths under `inside` are counted, because a reference
/// to the shared toolchain says nothing about which tree this build was for; and each is reduced to
/// its first component under `inside`, which is the worktree, not the file.
///
/// **A reference to the build directory's own ancestry is dropped.** `<box>/tree/target` names
/// `<box>/tree`, and a build directory cannot be orphaned by the tree it lives in — that tree is
/// there, or the build directory would not be either.
fn build_sources(
    build: &std::path::Path,
    inside: &std::path::Path,
) -> std::collections::BTreeMap<String, usize> {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    let mut stack = vec![build.to_path_buf()];
    let prefix = format!("{}/", inside.display());
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for kid in entries.flatten() {
            let path = kid.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("d") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for word in text.split([' ', '\t', '\n', '\r']) {
                let word = word.trim_end_matches(':');
                let Some(rest) = word.strip_prefix(&prefix) else {
                    continue;
                };
                let Some(first) = rest.split('/').next().filter(|s| !s.is_empty()) else {
                    continue;
                };
                let root = format!("{prefix}{first}");
                if build.starts_with(&root) {
                    continue; // the tree this build directory lives in
                }
                *counts.entry(root).or_default() += 1;
            }
        }
    }
    counts
}

/// Build directories inside live boxes whose dependency files name a source tree that is gone.
///
/// **`Err` rather than an empty answer whenever a derivation came up empty**, which is
/// [`substrate_strays`]'s discipline and is here for the same reason: a sweep that reports nothing
/// because it recognised nothing reads exactly like a sweep that reports nothing because there is
/// nothing (SKEIN-647). There is one way to come up empty here — no box at all — and it says so.
///
/// Note what is *not* a refusal: a box with no build directory, and a build directory whose `.d`
/// files name no tree, are both simply silent. Neither is a failed derivation — there is genuinely
/// nothing to say — and a build directory skein cannot explain is one it must not offer to delete.
///
/// Biggest first, because the only reason anybody runs this is that a disk is full.
pub fn orphaned_builds() -> Result<Vec<OrphanedBuild>, String> {
    let root = fleet_root();
    let live = live_box_names();
    if live.is_empty() {
        return Err(format!(
            "skein can see no boxes at all — neither a directory in {root} nor a placement record \
             in {places} — so it cannot say whose build output anything is. Refusing rather than \
             reporting that nothing is stranded",
            places = skein_home().join("places").display(),
        ));
    }
    use std::os::unix::fs::MetadataExt;
    let mut stranded = Vec::new();
    for name in &live {
        let box_dir = std::path::Path::new(&root).join(name);
        if !box_dir.is_dir() {
            continue; // a placement record for a box whose tree is not here
        }
        let on_disk = std::fs::metadata(&box_dir).map(|m| m.dev()).ok();
        let mut builds = Vec::new();
        cargo_build_dirs(&box_dir, 4, &mut builds);
        for build in builds {
            let sources = build_sources(&build, &box_dir);
            let of: usize = sources.values().sum();
            let mut gone: Vec<(&String, &usize)> = sources
                .iter()
                .filter(|(tree, _)| !std::path::Path::new(tree).exists())
                .collect();
            gone.sort_by(|a, b| b.1.cmp(a.1));
            let Some((tree, naming)) = gone.first() else {
                continue;
            };
            stranded.push(OrphanedBuild {
                bytes: tree_bytes(&build, on_disk),
                path: build.display().to_string(),
                built_from: (*tree).clone(),
                naming: **naming,
                of,
            });
        }
    }
    stranded.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    Ok(stranded)
}

/// [`orphaned_builds`] as a line to put in front of a person, or `None` when there is nothing to
/// say.
///
/// The evidence is in the sentence and not only in the struct: a reader is told which tree the
/// build was for, that it is not on disk, and how much of the directory says so — because
/// `naming` short of `of` means the directory was pointed at something else afterwards, and that
/// is the reader's call rather than this function's.
///
/// The command is spelled out and **not run**, and every path in it is shell-quoted: the fleet
/// root comes from `$SKEIN_FLEET_ROOT`, an operator sets it, and this string is meant to be
/// pasted into a shell.
pub fn orphaned_build_advice(builds: &[OrphanedBuild]) -> Option<String> {
    if builds.is_empty() {
        return None;
    }
    let gib = |bytes: u64| format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0));
    let total: u64 = builds.iter().map(|b| b.bytes).sum();
    let lines = builds
        .iter()
        .map(|b| {
            format!(
                "\x20   {} ({}) was built from {}, which is not on disk — {} of its {} source \
                 references name it",
                b.path,
                gib(b.bytes),
                b.built_from,
                b.naming,
                b.of,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let paths = builds
        .iter()
        .map(|b| sh_quote(&b.path))
        .collect::<Vec<_>>()
        .join(" ");
    let subject = match builds.len() {
        1 => "1 build directory".to_string(),
        n => format!("{n} build directories"),
    };
    let verb = match builds.len() {
        1 => "names a source tree",
        _ => "name source trees",
    };
    Some(format!(
        "{subject} in the fleet {verb} that is no longer there, {total} in all:\n{lines}\n\
         skein did not create them and does not remove them; if they are yours to delete:\n\
         \x20   rm -rf {paths}",
        total = gib(total),
    ))
}

// ─────────── compiler output cargo has superseded, where the only handle is its age ────────────
//
// The two sweeps above can each PROVE what they name is dead: a directory under `.skein` that is
// neither skein's own nor any live box's, and a build directory whose own dependency files name a
// tree that is not on disk. This one cannot, and saying so is half of what it is for.
//
// Cargo never garbage-collects. Rebuild a crate under a changed feature set or a new compiler and
// the old `.rlib`, `.rmeta` and binary stay beside the new ones under a different `-<16 hex>`
// metadata hash, for ever. Nothing in either file says which generation the next build will use.
// **Cargo's own `.fingerprint` register does not answer it either**, which the comment above
// records having measured rather than assumed: one fingerprint directory per generation and none
// removed, so every superseded artefact resolves to a live fingerprint and a derivation built on
// them reports nothing dead at all. A superseded generation is also genuinely reusable — check the
// old branch back out and cargo links it rather than rebuilding.
//
// So the only handle left is **how long since anything read or wrote the file**, which is what
// `cargo-sweep` uses and what every other rule in this module argues against — "a directory being
// old is not on its own evidence of death". It is admitted here, and only here, because of what
// being wrong costs: the artefact is regenerated from source nobody touched, so the price of
// deleting a generation that was still wanted is a **slower rebuild and never lost work**. That is
// the whole of the argument, and it does not transfer to anything else this module names.
//
// Two rules keep the admission narrow, and both are the lesson of the lanes that deleted live
// fixtures by matching names:
//
// * **Age only ever qualifies output cargo has already claimed.** The walk starts at a directory
//   carrying [`CARGO_TARGET_MARKER`] and never anywhere else, so a twelve-day-old directory called
//   `target-notes` that cargo did not make is not examined, not counted and not offered. Age is
//   never the thing that selects.
// * **Whole days, counted as `find -mtime +N` counts them.** The figure skein reports and the
//   command it prints have to name the same files, or a reader checks the second against the first
//   and finds skein wrong about its own offer.
//
// **Nothing here deletes**, the same as everything above it.

/// A build directory inside a live box, and how much of it nothing has touched lately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBuild {
    /// The build directory itself, absolute.
    pub path: String,
    /// Bytes held by regular files older than the threshold — what the offer would reclaim, and
    /// **not** the directory's size. The two differ by everything a build has touched since.
    pub bytes: u64,
    /// How many such files.
    pub files: usize,
    /// What [`tree_bytes`] makes of the whole directory, carried so the report can say what share
    /// of a live build this is. A directory that is 99% stale and one that is 4% stale are
    /// different decisions and a reader should not have to go and measure to tell them apart.
    pub of: u64,
    /// The age of the oldest qualifying file, in whole days. The evidence, such as it is: `6` on a
    /// five-day threshold is a rounding, `12` is a fortnight nobody has been near.
    pub oldest_days: u64,
}

/// Build directories inside live boxes holding compiler output nothing has touched in
/// `older_than_days` whole days.
///
/// **`Err` rather than an empty answer whenever a derivation came up empty**, which is
/// [`substrate_strays`]' and [`orphaned_builds`]' discipline and is here for the same reason
/// (SKEIN-647). There are two ways to come up empty and each says so: no box at all, and a
/// threshold of zero — which would qualify every file skein can see, including the one cargo wrote
/// a second ago, and so is a refusal rather than an offer of everything.
///
/// Note what is *not* a refusal: a box with no build directory, and a build directory nothing is
/// old enough in, are both simply silent. There is genuinely nothing to say.
///
/// Biggest first, because the only reason anybody runs this is that a disk is full.
pub fn stale_builds(older_than_days: u32) -> Result<Vec<StaleBuild>, String> {
    let root = fleet_root();
    let live = live_box_names();
    if live.is_empty() {
        return Err(format!(
            "skein can see no boxes at all — neither a directory in {root} nor a placement record \
             in {places} — so it cannot say whose build output anything is. Refusing rather than \
             reporting that nothing is stale",
            places = skein_home().join("places").display(),
        ));
    }
    if older_than_days == 0 {
        return Err(
            "a threshold of zero days would qualify every compiler artefact in the fleet, \
             including the one cargo wrote a second ago. Refusing rather than offering the whole \
             of every build directory — set `stale_build_days` to the number of days you want, or \
             leave it at zero to have skein say nothing about age at all"
                .to_string(),
        );
    }
    use std::os::unix::fs::MetadataExt;
    let mut stale = Vec::new();
    for name in &live {
        let box_dir = std::path::Path::new(&root).join(name);
        if !box_dir.is_dir() {
            continue; // a placement record for a box whose tree is not here
        }
        let on_disk = std::fs::metadata(&box_dir).map(|m| m.dev()).ok();
        let mut builds = Vec::new();
        cargo_build_dirs(&box_dir, 4, &mut builds);
        for build in builds {
            let (bytes, files, oldest_days) = untouched_for(&build, on_disk, older_than_days);
            if files == 0 {
                continue;
            }
            stale.push(StaleBuild {
                of: tree_bytes(&build, on_disk),
                path: build.display().to_string(),
                bytes,
                files,
                oldest_days,
            });
        }
    }
    stale.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    Ok(stale)
}

/// Bytes, count and greatest age of the regular files under `build` that nothing has written in
/// `days` whole days.
///
/// **Regular files only, because `find -type f -delete` is what the offer prints.** Counting a
/// directory's own blocks here would put bytes in the figure that the command would not reclaim,
/// and a reader who runs it and measures again would find skein had overstated the saving.
///
/// The walk is [`tree_bytes`]' — same device filter, same hardlink set, same silence on an
/// unreadable directory — because a second walk written beside it is a second answer to "how big
/// is that".
///
/// A file whose mtime is in the *future* is age zero rather than a negative one: a clock that went
/// backwards must not make every artefact in the fleet eligible for deletion.
fn untouched_for(build: &std::path::Path, on_disk: Option<u64>, days: u32) -> (u64, usize, u64) {
    use std::os::unix::fs::MetadataExt;
    let now = std::time::SystemTime::now();
    let mut bytes = 0u64;
    let mut files = 0usize;
    let mut oldest = 0u64;
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    if let Ok(meta) = std::fs::metadata(build) {
        seen.insert((meta.dev(), meta.ino()));
    }
    let mut stack = vec![build.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(kids) = std::fs::read_dir(&dir) else {
            continue;
        };
        for kid in kids.flatten() {
            let Ok(meta) = kid.metadata() else {
                continue;
            };
            if on_disk.is_some_and(|dev| meta.dev() != dev) {
                continue; // `-x`
            }
            if !seen.insert((meta.dev(), meta.ino())) {
                continue; // a hardlink already counted
            }
            if meta.is_dir() {
                stack.push(kid.path());
                continue;
            }
            if !meta.is_file() {
                continue; // `-type f`
            }
            let Ok(age) = meta.modified().and_then(|m| {
                now.duration_since(m)
                    .or(Ok(std::time::Duration::from_secs(0)))
            }) else {
                continue;
            };
            // Whole days, truncated — `find -mtime +N` discards the fraction too, and the figure
            // and the command have to name one set of files.
            let whole = age.as_secs() / 86_400;
            if whole <= u64::from(days) {
                continue;
            }
            bytes += meta.blocks() * 512;
            files += 1;
            oldest = oldest.max(whole);
        }
    }
    (bytes, files, oldest)
}

/// [`stale_builds`] as a line to put in front of a person, or `None` when there is nothing to say.
///
/// **The sentence says out loud that this evidence is the weak one**, and that is a requirement
/// rather than a courtesy. [`orphaned_build_advice`] above can tell a reader *why* a directory is
/// dead and invite them to check it; this one can only say that nothing has been near these files,
/// which is a fact about attention and not about need. A reader who cannot tell the two offers
/// apart will weigh them the same, and the whole reason an age is admissible here is that its
/// failure mode is cheap — so the failure mode is in the sentence.
///
/// The command is spelled out and **not run**, every path in it is shell-quoted, and its `-mtime`
/// carries the same number the figures were derived from.
pub fn stale_build_advice(builds: &[StaleBuild], older_than_days: u32) -> Option<String> {
    if builds.is_empty() || older_than_days == 0 {
        return None;
    }
    let gib = |bytes: u64| format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0));
    let total: u64 = builds.iter().map(|b| b.bytes).sum();
    let lines = builds
        .iter()
        .map(|b| {
            format!(
                "\x20   {} — {} of its {} in {} files nothing has written in {} days, the oldest \
                 {} days",
                b.path,
                gib(b.bytes),
                gib(b.of),
                b.files,
                older_than_days,
                b.oldest_days,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let paths = builds
        .iter()
        .map(|b| sh_quote(&b.path))
        .collect::<Vec<_>>()
        .join(" ");
    let subject = match builds.len() {
        1 => "1 build directory".to_string(),
        n => format!("{n} build directories"),
    };
    Some(format!(
        "{subject} in the fleet holds {total} of compiler output nothing has touched in \
         {older_than_days} days, which cargo will never remove by itself:\n{lines}\n\
         This is age-based evidence and it is the weaker kind — nothing in these files says a \
         generation is dead, only that nothing has read or written one for {older_than_days} days, \
         so being wrong here costs a slower rebuild and never work. The threshold is yours: \
         `stale_build_days` in Settings, and 0 turns this off. skein does not run this:\n\
         \x20   find {paths} -mindepth 1 -type f -mtime +{older_than_days} -delete",
        total = gib(total),
    ))
}

/// `du` over every box, and the `|| true` is the entire point of this being its own function.
///
/// `du` exits nonzero if it could not read so much as one directory anywhere in the tree, while
/// still printing correct totals for everything it *could* read. `exec` treats a nonzero exit as a
/// failed call, so without this a single unreadable directory — in any box, at any depth — threw
/// away the disk figures for the whole fleet and every box reported nothing.
///
/// That is not hypothetical and not rare: boxes create unreadable directories in the course of
/// ordinary work (a test asserting behaviour on an unreadable store leaves one behind), and one is
/// enough. The failure was invisible for a long time because the row chip stays silent below 80% of
/// a box's allowance, so "no disk figure" and "nothing worth saying" looked identical.
///
/// Partial output is the right answer here. A total is worth having even when one subtree could not
/// be walked, and a `du` that printed nothing at all still parses to an empty map.
///
/// The root is quoted and the glob is not, which is the whole of the shape (FLEET-7). `fleet_root`
/// reads `$SKEIN_FLEET_ROOT`, and this file already treats that as untrusted where it builds a
/// `pkill` pattern — "a `+`, `[`, `(`, `*`, `?` or `|` in it left the pattern matching something
/// OTHER". Raw here, it was one interpolation of the same value in a file that quotes every other.
/// **Test-only now** (SKEIN-576): `local_disk_usage` is what production walks, and this shell
/// form is the oracle it is checked against — the same `du -sxm <root>/*/` question, asked the
/// way a host used to ask it, so a walk that drifts from `du` fails rather than redefining the
/// answer.
#[cfg(test)]
fn disk_usage_script(root: &str) -> String {
    format!("du -sxm {}/*/ 2>/dev/null || true", sh_quote(root))
}

/// Turn `du -sxm` output into MiB per box. Separate so it can be tested against the real thing.
/// **Test-only now** (SKEIN-576), with [`disk_usage_script`], as half of that oracle.
#[cfg(test)]
fn parse_disk_usage(out: &str) -> std::collections::HashMap<String, u64> {
    out.lines()
        .filter_map(|line| {
            let (mb, path) = line.trim().split_once(char::is_whitespace)?;
            let name = path.trim().trim_end_matches('/').rsplit('/').next()?;
            Some((name.to_string(), mb.trim().parse().ok()?))
        })
        .collect()
}

/// See [`crate::util::Gate`]: remembered, asked by one caller at a time, and asked less often while the
/// sandbox is failing to answer — a `du` over every box is the most expensive question skein asks
/// on a tick, and the last thing a struggling sandbox should be handed more of.
pub(super) static DISK_GATE: crate::util::Gate<std::collections::HashMap<String, u64>> =
    crate::util::Gate::new();

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::testkit::walk;
    use crate::testutil::*;

    /// One unreadable directory must not blank the disk figures for the whole fleet.
    ///
    /// This is the bug as it actually happened, reproduced against real `du`. A box left a directory
    /// it could not read — an ordinary thing for a box to do — and `du` exited 1 while still
    /// printing correct totals for every other box. `exec` reads a nonzero exit as a failed call, so
    /// the totals were discarded and every box on the board reported no disk usage at all.
    ///
    /// It hid for a long time because the row chip stays silent below 80% of a box's allowance:
    /// "skein has no disk figure for this box" and "this box is nowhere near its limit" render
    /// identically. It only surfaced once the figure was shown unconditionally on hover.
    #[test]
    fn a_directory_it_cannot_read_does_not_erase_everyone_elses_disk_usage() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        for b in ["web-main", "api"] {
            std::fs::create_dir_all(root.join(b).join("tree")).unwrap();
            std::fs::write(root.join(b).join("tree/f"), vec![0u8; 4096]).unwrap();
        }
        // The shape that broke it: readable enough to be descended into, then a directory that is
        // not. `du` reports what it can and exits nonzero.
        let shut = root.join("web-main/secret");
        std::fs::create_dir_all(&shut).unwrap();
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(disk_usage_script(&root.display().to_string()))
            .output()
            .expect("sh to run the disk script");
        // Restored before any assertion can fail, or the temp dir cannot be cleaned up.
        let _ = std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755));

        assert!(
            out.status.success(),
            "a nonzero exit is read as a failed call, and the whole fleet's figures are dropped"
        );
        let got = parse_disk_usage(&String::from_utf8_lossy(&out.stdout));
        assert!(
            got.contains_key("api"),
            "the box with nothing wrong lost its figure too: {got:?}"
        );
        assert!(
            got.contains_key("web-main"),
            "the box with the unreadable directory still has a total: {got:?}"
        );
    }

    /// The walk answers what `du -sxm` answers, compared against the real `du` on the same tree.
    ///
    /// Derived rather than asserted, because "it does what du does" is the kind of claim that is
    /// true when written and false a year later. The tree carries the four cases where a loose
    /// reimplementation drifts, each of which was checked against real `du` output before this was
    /// written: a **sparse** file (du counts blocks, so `len()` over-reports it by a factor of
    /// thousands), a **hardlink** (du counts the inode once — `du --count-links` was 56 blocks
    /// against 48 without), a **symlink** pointing outside the tree (not followed, or another
    /// box's bytes land on this one), and the **box directory's own blocks**, which `du -s`
    /// includes and a walk of its children alone misses.
    ///
    /// A fifth case was added by SKEIN-735, and it is the one that had already drifted: a **dotted
    /// directory**. `du` is given a glob and a glob does not match a leading dot, while `read_dir`
    /// returns every entry — so `.skein`, the substrate, was a key in the walk's answer and never
    /// in `du`'s. The fixture planted no dotted directory, so the disagreement could not show
    /// here; planting one made this assertion fail before the walk was fixed.
    #[test]
    fn the_local_walk_answers_what_du_answers() {
        if std::process::Command::new("sh")
            .args(["-c", "command -v du"])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            crate::testutil::skip("no `du` to compare against");
            return;
        }
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        for b in ["web-main", "api"] {
            std::fs::create_dir_all(root.join(b).join("tree/nested")).unwrap();
            std::fs::write(root.join(b).join("tree/f"), vec![7u8; 40_000]).unwrap();
            std::fs::write(root.join(b).join("tree/nested/g"), vec![9u8; 12_000]).unwrap();
        }
        // A sparse file: 8 MiB of apparent length occupying almost no blocks. This is the case
        // that separates counting blocks from counting lengths.
        let sparse = std::fs::File::create(root.join("web-main/tree/sparse")).unwrap();
        sparse.set_len(8 * 1024 * 1024).unwrap();
        drop(sparse);
        std::fs::hard_link(
            root.join("web-main/tree/f"),
            root.join("web-main/tree/linked"),
        )
        .unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("web-main/tree/out")).unwrap();
        // And the fifth case, which is not a flag but the glob: `.skein`, the substrate. `du` is
        // given `<root>/*/` and a shell glob does not match a leading dot, so this directory is
        // never in the oracle's answer and must not be in the walk's. Planted with bytes in it, so
        // a walk that keeps it disagrees by a whole entry rather than by a rounding.
        std::fs::create_dir_all(root.join(".skein/target-phase3-agent")).unwrap();
        std::fs::write(
            root.join(".skein/target-phase3-agent/o"),
            vec![3u8; 300_000],
        )
        .unwrap();

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(disk_usage_script(&root.display().to_string()))
            .output()
            .expect("sh to run the disk script");
        let want = parse_disk_usage(&String::from_utf8_lossy(&out.stdout));
        let got = local_disk_usage(&root.display().to_string());

        assert!(!want.is_empty(), "the comparison has nothing in it");
        assert_eq!(
            got, want,
            "the in-fleet walk and `du -sxm` disagree. They are the same figure shown in the same \
             place, so a box that changed size the day skein moved inside reads as a skein bug \
             rather than a change of method.\n  walk: {got:?}\n  du:   {want:?}"
        );
    }

    /// The substrate is not a box, so it is not one of the names the disk map offers to stop.
    ///
    /// Its own test beside [`the_local_walk_answers_what_du_answers`] because that one begins by
    /// returning when there is no `du` to compare against, and a skip is indistinguishable from a
    /// pass in the report — this is the same property asked in a way no missing program can
    /// silence. It also asks it as the thing a person is hurt by, which is a name in the map,
    /// rather than as agreement between two maps.
    ///
    /// **What would make it fail**: dropping the `starts_with('.')` skip from `local_disk_usage`.
    /// Watched: `.skein` came back as a key and the first assertion went red.
    #[test]
    fn the_substrate_is_not_one_of_the_boxes_the_disk_map_names() {
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        std::fs::create_dir_all(root.join("web-main/tree")).unwrap();
        std::fs::write(root.join("web-main/tree/f"), vec![7u8; 40_000]).unwrap();
        // The substrate, holding the shape that cost 19.2 GB: a build directory no box owns. It
        // is bigger than the box, so a walk that keeps it does not merely include it — it puts it
        // first, which is the position `health::biggest_first` reads.
        std::fs::create_dir_all(root.join(".skein/target-phase3-agent")).unwrap();
        std::fs::write(
            root.join(".skein/target-phase3-agent/o"),
            vec![3u8; 4_000_000],
        )
        .unwrap();

        let got = local_disk_usage(&root.display().to_string());
        assert!(
            !got.contains_key(".skein"),
            "the substrate is a key in the per-box map, so the disk fix line can name `.skein` as \
             one of the largest boxes and offer `skein stop .skein`, which is not a box and not a \
             command that works: {got:?}"
        );
        assert_eq!(
            got.get("web-main").copied(),
            Some(1),
            "dropping the substrate dropped the box with it — 40 KB is one MiB once `du -m` has \
             rounded up, so this is the whole answer for that box: {got:?}"
        );
    }

    // ───────── the substrate sweep: `skein_dir`, `substrate_strays`, `stray_advice` ─────────

    /// What a real fleet's `.skein` holds, **stated by the fixture rather than asked of the code**.
    ///
    /// It was `installed_substrate_dirs()` for one draft, and that draft's own falsifier proved the
    /// mistake: dropping `skein_toolchain_path()` from that function dropped `toolchain` from the
    /// fixture too, so the test that exists to catch exactly that went green. A fixture derived
    /// from the thing under test cannot disagree with it. These six are read off the live fleet
    /// (`ls /boxes/.skein`, 2026-09-09), and
    /// `skeins_own_substrate_directories_are_never_offered_for_deletion` asserts the code still
    /// names the same set, so drift fails rather than hides.
    const SUBSTRATE_DIRS: &[&str] = &[
        "asks",
        "detached",
        "gitgate",
        "plugin",
        "private",
        "src",
        "substrate",
        "toolchain",
    ];

    /// A fleet root shaped like a real one: skein's own directories under `.skein`, the boxes
    /// named, and whatever else the test plants beside them.
    ///
    /// `extra` gets a file in it, so a stray has a size and "found nothing" cannot pass for
    /// "found something empty".
    fn plant_fleet(root: &std::path::Path, boxes: &[&str], extra: &[&str]) {
        for name in SUBSTRATE_DIRS {
            std::fs::create_dir_all(root.join(".skein").join(name)).unwrap();
        }
        for name in boxes {
            std::fs::create_dir_all(root.join(name).join("tree")).unwrap();
        }
        for name in extra {
            let dir = root.join(".skein").join(name).join("debug");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("libthing.rlib"), vec![7u8; 40_000]).unwrap();
        }
    }

    /// Pin both variables at one fixture, and hand back the root.
    ///
    /// `$SKEIN_FLEET_ROOT` because `fleet_root()` refuses an unpinned test rather than answering
    /// `/boxes` — which on this machine is the owner's live fleet, and this sweep *reads whole
    /// directory trees and prints a `rm -rf` naming them* (SKEIN-530/685/690). `$SKEIN_HOME`
    /// because `live_box_names` reads `places/` through `config::skein_home`, which refuses for the
    /// same reason. `EnvPins` puts both back on the panicking path as well as the passing one.
    fn pinned_fleet(
        dir: &crate::testutil::TempDir,
    ) -> (std::path::PathBuf, crate::testutil::EnvPins) {
        let root = (dir.as_ref() as &std::path::Path).join("fleet");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all((dir.as_ref() as &std::path::Path).join("home")).unwrap();
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_FLEET_ROOT", &root);
        pins.set(
            "SKEIN_HOME",
            (dir.as_ref() as &std::path::Path).join("home"),
        );
        (root, pins)
    }

    /// A build directory nobody claims is named, with its size.
    ///
    /// **What would make this fail**: crediting an unrecognised directory to a live box, or
    /// skipping anything the installed set does not carry — either way `strays` comes back empty.
    /// Watched: with the `!mine && !a_box` filter inverted, this returned the six substrate
    /// directories and not `target-gone`.
    #[test]
    fn a_substrate_directory_no_box_accounts_for_is_named_with_its_size() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &["target-gone"]);

        let strays = substrate_strays().expect("a fleet with a box and a substrate is answerable");

        assert_eq!(
            strays.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["target-gone"],
            "the sweep did not name the one directory that belongs to nobody: {strays:?}"
        );
        assert!(
            strays[0].bytes >= 40_000,
            "the stray is reported at {} bytes, which cannot include the 40,000-byte file planted \
             in it — a size nobody can act on is not a report",
            strays[0].bytes
        );
    }

    /// A directory named for a box that is still here is left alone — **and the same directory is
    /// named the moment the box is gone**, which is what makes the first half an assertion rather
    /// than a coincidence.
    ///
    /// This is the direction a happy-path test never sees and the one that deletes somebody's work:
    /// `target-web-main` is 4 GB of a live box's build, and calling it an orphan puts it in a
    /// `rm -rf` a person is invited to paste.
    ///
    /// **What would make this fail**: dropping the live-box credit — the first assertion goes red
    /// immediately. Watched, by removing `let a_box = …` and passing `false`: `target-web-main` was
    /// named while `web-main` was still standing.
    #[test]
    fn a_directory_named_for_a_live_box_is_left_alone_until_that_box_is_gone() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &["target-web-main"]);

        let while_live = substrate_strays().expect("a fleet with a box is answerable");
        assert!(
            while_live.is_empty(),
            "`target-web-main` was called an orphan while `web-main` is a directory in the fleet \
             root — this is the report that would have somebody delete a running box's build: \
             {while_live:?}"
        );

        // The box goes, exactly as `destroy_script`'s `rm -rf <box root>` takes it. Nothing else
        // changes, so what follows is about the box's absence and nothing else.
        std::fs::remove_dir_all(root.join("web-main")).unwrap();
        std::fs::create_dir_all(root.join("other-main")).unwrap();

        let once_gone = substrate_strays().expect("a fleet with a box is answerable");
        assert_eq!(
            once_gone
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["target-web-main"],
            "the same directory is still unnamed after the box it was named for was destroyed, so \
             the silence above proved nothing: {once_gone:?}"
        );
    }

    /// skein's own directories are never offered for deletion, beside a stray that is.
    ///
    /// Both halves in one test on purpose: "nothing was named" would pass if the sweep were broken
    /// altogether, so the stray is here to prove the sweep ran.
    ///
    /// **What would make this fail**: a directory skein installs dropping out of
    /// `installed_substrate_dirs` — say a rename that this file's derivation stops following.
    /// Watched, by removing `skein_toolchain_path()` from the list: `toolchain` was offered for
    /// deletion, which is the fleet's compiler.
    #[test]
    fn skeins_own_substrate_directories_are_never_offered_for_deletion() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &["target-gone"]);

        let installed = installed_substrate_dirs();
        assert!(
            !installed.is_empty(),
            "nothing was derived as skein's own, so every directory in the substrate would read as \
             unattributed"
        );
        assert_eq!(
            installed.iter().map(String::as_str).collect::<Vec<_>>(),
            SUBSTRATE_DIRS,
            "the code and the fixture no longer agree about which directories skein installs. \
             Whichever moved, the other has to follow — a fixture that asked the code would have \
             gone green here while the sweep offered one of skein's own directories for deletion"
        );
        let strays = substrate_strays().expect("a fleet with a box is answerable");
        for name in &installed {
            assert!(
                !strays.iter().any(|s| &s.name == name),
                "{name} is a directory skein installs and the sweep offered it for deletion: \
                 {strays:?}"
            );
        }
        assert_eq!(strays.len(), 1, "the sweep did not run at all: {strays:?}");
    }

    /// **Refuse rather than report zero**, in all three ways there are to come up empty.
    ///
    /// SKEIN-647's lesson, applied to an answer instead of a pattern: a sweep that says "nothing is
    /// stranded" because it recognised nothing looks exactly like one that says it because nothing
    /// is. The only way to tell them apart is for the second not to be sayable.
    ///
    /// **What would make this fail**: any of the three refusals becoming an ordinary answer.
    /// Watched on the no-boxes arm, by making its guard unreachable: it returned
    /// `[Stray { name: "target-gone", .. }]` — an answer that reads exactly like a good one and was
    /// produced by a derivation that had found no box at all. On a fleet where that derivation is
    /// what broke, every live box's build directory is in that list.
    #[test]
    fn the_sweep_refuses_rather_than_saying_nothing_is_stranded() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);

        // A substrate, and not one box anywhere: every directory in it would read as nobody's.
        plant_fleet(&root, &[], &["target-gone"]);
        let why = substrate_strays().expect_err("no box means no answer");
        assert!(
            why.contains("Refusing") && why.contains("places"),
            "the refusal does not say what could not be derived, so nobody can fix it: {why}"
        );

        // A box, and no substrate at all — not a fleet root skein has installed into.
        let bare = (dir.as_ref() as &std::path::Path).join("bare");
        std::fs::create_dir_all(bare.join("web-main")).unwrap();
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_FLEET_ROOT", &bare);
        let why = substrate_strays().expect_err("no substrate means no answer");
        assert!(
            why.contains(".skein"),
            "the refusal does not name the directory it could not read: {why}"
        );
    }

    /// **The sweep asks [`live_box_names`] whether the box is gone, and that is the only place the
    /// question is answered** (SKEIN-736).
    ///
    /// Both directions in one test, because either alone is worth little. A sweep that removed
    /// nothing would pass the first assertion; a sweep with no guard at all would pass the second.
    /// The interesting half is the first: the two failures are not the same size. Refusing to
    /// sweep a box that is really gone leaves a stale `privileged` under a name nothing is using
    /// yet; sweeping a box that is really live takes its git scope, its disk allowance and who it
    /// commits as away in the middle of its work.
    ///
    /// **What would make this fail**: deleting the `live_box_names().contains(name)` guard from
    /// [`forget_departed_box`]. Watched, by making that condition unreachable — the call came back
    /// `Ok` naming both of a live box's directories — *"a box with a tree in the fleet root was
    /// swept as departed"*, followed by its `declared/web-main` and its
    /// `.skein/substrate/requests/web-main`.
    #[test]
    fn what_skein_decided_about_a_box_survives_exactly_as_long_as_the_box_does() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        set_box_privileged("web-main", true).unwrap();
        std::fs::create_dir_all(crate::substrate::box_requests_dir("web-main")).unwrap();

        let why = forget_departed_box("web-main")
            .expect_err("a box with a tree in the fleet root was swept as departed");
        assert!(
            why.contains("still reads as a live box"),
            "the refusal does not say why it refused, so nobody can tell it from a failure: {why}"
        );
        assert!(
            box_is_privileged("web-main"),
            "a LIVE box lost the decisions skein had made about it, which is the direction that \
             costs somebody their work rather than some bytes"
        );

        // The box goes, exactly as `sandbox::destroy_script`'s `rm -rf <box root>` takes it.
        // Nothing else changes, so what follows is about its absence and nothing else.
        std::fs::remove_dir_all(root.join("web-main")).unwrap();
        let gone = forget_departed_box("web-main").expect("a box that is gone is answerable");
        assert!(
            !box_is_privileged("web-main"),
            "the same call that refused a live box also refuses one that is gone, so the refusal \
             above proved nothing"
        );
        assert_eq!(
            gone.len(),
            2,
            "the sweep did not remove both the declared answers and the drop-box that existed: \
             {gone:?}"
        );
        assert!(
            !std::path::Path::new(&crate::substrate::box_requests_dir("web-main")).exists(),
            "the destroyed box's substrate drop-box outlived it"
        );
    }

    /// The offer is a line and a command, and **running it is the reader's move**.
    ///
    /// The property is not about the wording: it is that the directory is still there afterwards.
    /// "Inform and offer, never perform" is only checkable as an absence of the performing.
    ///
    /// **What would make this fail**: `stray_advice` (or the sweep) removing what it found — the
    /// existence assertion goes red. Watched, with a `remove_dir_all` added to `stray_advice`.
    #[test]
    fn the_offer_hands_over_a_command_and_deletes_nothing_itself() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &["target-gone"]);

        assert_eq!(
            stray_advice(&[]),
            None,
            "there is nothing to say about nothing"
        );

        let strays = substrate_strays().expect("a fleet with a box is answerable");
        let offer = stray_advice(&strays).expect("a stray was found, so there is something to say");
        let path = root.join(".skein/target-gone");
        assert!(
            offer.contains(&path.display().to_string()),
            "the command does not name the directory it is about, so it cannot be pasted: {offer}"
        );
        assert!(
            offer.contains("rm -rf"),
            "the offer names a problem and no way out of it: {offer}"
        );
        // One stray, and the sentence has to read like it. A count is the first thing anybody
        // looks at on a line about a full disk, and "1 directories ... are" reads as a bug in the
        // thing reporting the bug.
        assert!(
            offer.starts_with("1 directory ") && offer.contains(" is neither"),
            "the offer does not agree with itself about how many it found: {offer}"
        );
        assert!(
            path.is_dir(),
            "{} was removed by a report — the one thing this must never do",
            path.display()
        );
    }

    // ───── build output for a tree that is gone: `orphaned_builds`, `orphaned_build_advice` ─────

    /// A cargo build directory inside a live box, built from `tree` under that box.
    ///
    /// The marker file is what makes it a build directory as far as [`cargo_build_dirs`] is
    /// concerned, and the `.d` file is written the way cargo writes one — `make` syntax, outputs
    /// before the colon, absolute source paths after it. Both halves are the fixture because both
    /// halves are the derivation.
    fn plant_build(box_dir: &std::path::Path, build: &str, built_from: &str, refs: usize) {
        let dir = box_dir.join(build);
        let deps = dir.join("debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::write(dir.join(CARGO_TARGET_MARKER), "{}").unwrap();
        std::fs::write(deps.join("libthing.rlib"), vec![7u8; 40_000]).unwrap();
        let src = box_dir.join(built_from);
        let sources = (0..refs)
            .map(|i| format!("{}/src/f{i}.rs", src.display()))
            .collect::<Vec<_>>()
            .join(" ");
        std::fs::write(
            deps.join("thing-0123456789abcdef.d"),
            format!("/target/debug/deps/libthing.rlib: {sources}\n"),
        )
        .unwrap();
    }

    /// A build directory whose sources are not on disk is named, with the tree that is missing.
    ///
    /// **What would make this fail**: dropping the `!Path::exists(tree)` filter in
    /// `orphaned_builds`, so that no tree is ever "gone" and the sweep has nothing to report.
    /// Watched — see the report; the assertion below failed with `[]`.
    #[test]
    fn a_build_directory_whose_source_tree_is_gone_is_named_with_that_tree() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        // `wt-1140` is never created, which is the whole of what makes this build output dead.
        plant_build(&root.join("web-main"), "target-wt1140", "wt-1140", 9);

        let found = orphaned_builds().expect("a fleet with a box is answerable");

        assert_eq!(
            found
                .iter()
                .map(|b| b.built_from.as_str())
                .collect::<Vec<_>>(),
            vec![root.join("web-main/wt-1140").display().to_string()],
            "the sweep did not name the build output of a worktree that is gone, so nothing would \
             ever reclaim it: {found:?}"
        );
        assert!(
            found[0].bytes >= 40_000,
            "the build directory is reported at {} bytes, which cannot include the 40,000-byte \
             artefact planted in it — a size nobody can act on is not a report",
            found[0].bytes
        );
        assert_eq!(
            (found[0].naming, found[0].of),
            (9, 9),
            "the count of references that name the missing tree is wrong, and that count is what a \
             reader weighs a `rm -rf` against: {found:?}"
        );
    }

    /// A build directory whose tree is still there is left alone — **and the same directory is
    /// named the moment that tree goes**, which is what makes the first half an assertion rather
    /// than a coincidence.
    ///
    /// This is the direction that deletes somebody's work. On the fleet this was measured against,
    /// one box held `target-wt1140` (5.9 GiB, dead) and `target-private` (4.0 GiB, live) side by
    /// side, written on the same day, and neither the name nor the age tells them apart.
    ///
    /// **What would make this fail**: inverting that existence filter to report trees that *are*
    /// on disk. Watched — see the report; the first assertion named the live directory.
    #[test]
    fn a_build_directory_whose_source_tree_is_still_there_is_left_alone_until_it_goes() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        let box_dir = root.join("web-main");
        // `plant_fleet` made `web-main/tree`, so this build directory's sources are all present.
        std::fs::create_dir_all(box_dir.join("tree/src")).unwrap();
        plant_build(&box_dir, "target-private", "tree", 9);

        let while_live = orphaned_builds().expect("a fleet with a box is answerable");
        assert!(
            while_live.is_empty(),
            "a build directory was called orphaned while the tree it names is on disk — this is \
             the report that puts a live box's work in a `rm -rf` a person is invited to paste: \
             {while_live:?}"
        );

        std::fs::remove_dir_all(box_dir.join("tree")).unwrap();

        let once_gone = orphaned_builds().expect("a fleet with a box is answerable");
        assert_eq!(
            once_gone
                .iter()
                .map(|b| b.path.as_str())
                .collect::<Vec<_>>(),
            vec![box_dir.join("target-private").display().to_string()],
            "the same directory is still unnamed after the tree it was built from was removed, so \
             the silence above proved nothing: {once_gone:?}"
        );
    }

    /// A directory that merely *looks* like build output is never examined, and never offered.
    ///
    /// The point of the test is the thing it refuses to do. `target-keepme` here is named exactly
    /// like the real strays, is exactly as old, and names a tree that does not exist — everything a
    /// name match or an age check would fire on. It carries no [`CARGO_TARGET_MARKER`], so cargo
    /// never made it, so skein says nothing about it.
    ///
    /// **What would make this fail**: finding build directories by a `target` name prefix instead
    /// of by cargo's marker. Watched — see the report; `target-keepme` was named for deletion.
    #[test]
    fn a_directory_that_only_looks_like_build_output_is_never_offered() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        let box_dir = root.join("web-main");
        // Real build output, so a sweep that found nothing at all cannot pass this test.
        plant_build(&box_dir, "target-wt1140", "wt-1140", 9);
        // Somebody's notes, under a name that matches every stray on the fleet.
        let decoy = box_dir.join("target-keepme");
        std::fs::create_dir_all(decoy.join("debug/deps")).unwrap();
        std::fs::write(
            decoy.join("debug/deps/thing-0123456789abcdef.d"),
            format!("out: {}/wt-gone/src/f.rs\n", box_dir.display()),
        )
        .unwrap();

        let found = orphaned_builds().expect("a fleet with a box is answerable");

        assert_eq!(
            found.iter().map(|b| b.path.as_str()).collect::<Vec<_>>(),
            vec![box_dir.join("target-wt1140").display().to_string()],
            "a directory cargo never made was offered for deletion because its name looked like \
             one that cargo did — which is the failure that cost another lane its fixtures: \
             {found:?}"
        );
        assert!(
            decoy.is_dir(),
            "{} was removed by a report — the one thing this must never do",
            decoy.display()
        );
    }

    /// No boxes at all is a refusal, not an empty sweep.
    ///
    /// Same discipline as `the_sweep_refuses_rather_than_saying_nothing_is_stranded` above and for
    /// the same reason: with no box names derived, "nothing is stranded" would be indistinguishable
    /// from "I could not tell", and the second is the truth (SKEIN-647).
    ///
    /// **What would make this fail**: returning `Ok(vec![])` when `live_box_names` is empty.
    /// Watched — see the report; `expect_err` panicked on an `Ok([])`.
    #[test]
    fn a_fleet_with_no_boxes_refuses_rather_than_reporting_nothing_stranded() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        // A build directory whose tree is gone, and no box anywhere. The bytes are real and the
        // sweep still must not answer, because it cannot say whose they are.
        std::fs::create_dir_all(root.join(".skein")).unwrap();
        let why = orphaned_builds()
            .expect_err("no box means the sweep cannot say whose build output anything is");
        assert!(
            why.contains("no boxes at all") && why.contains("Refusing"),
            "the refusal does not say what could not be derived, so a reader cannot tell it from \
             a clean fleet: {why}"
        );
    }

    /// The offer hands over a command, carries the evidence, and deletes nothing itself.
    ///
    /// **What would make this fail**: dropping `built_from` from the sentence
    /// `orphaned_build_advice` builds, leaving a reader a `rm -rf` over 5.9 GiB and no reason.
    /// Watched — see the report; the "names the tree" assertion failed.
    #[test]
    fn the_build_offer_carries_its_evidence_and_deletes_nothing() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        let box_dir = root.join("web-main");
        plant_build(&box_dir, "target-wt1140", "wt-1140", 9);

        assert_eq!(
            orphaned_build_advice(&[]),
            None,
            "there is nothing to say about nothing"
        );

        let found = orphaned_builds().expect("a fleet with a box is answerable");
        let offer =
            orphaned_build_advice(&found).expect("one was found, so there is something to say");
        let gone = box_dir.join("wt-1140");
        assert!(
            offer.contains(&gone.display().to_string()),
            "the offer does not name the tree that is missing, so it asserts a directory is dead \
             without saying why: {offer}"
        );
        assert!(
            offer.contains("which is not on disk") && offer.contains("9 of its 9"),
            "the offer states no evidence a reader can check before pasting a `rm -rf`: {offer}"
        );
        assert!(
            offer.contains("rm -rf"),
            "the offer names a problem and no way out of it: {offer}"
        );
        assert!(
            offer.starts_with("1 build directory ") && offer.contains(" names a source tree"),
            "the offer does not agree with itself about how many it found: {offer}"
        );
        assert!(
            box_dir.join("target-wt1140").is_dir(),
            "the build directory was removed by a report — the one thing this must never do"
        );
    }

    // ───── compiler output nothing has touched: `stale_builds`, `stale_build_advice` ─────

    /// Push every regular file under `dir` back by `days` whole days and an hour.
    ///
    /// The extra hour is not slack, it is the arithmetic: [`untouched_for`] truncates to whole days
    /// exactly as `find -mtime` does, so a file aged by precisely `days * 86_400` is `days` old and
    /// a threshold of `days` does **not** match it. Aging by a little more makes the fixture's age
    /// the number the test names, whatever the clock does between the two calls.
    fn age_by(dir: &std::path::Path, days: u64) {
        let when =
            std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400 + 3_600);
        let times = std::fs::FileTimes::new()
            .set_modified(when)
            .set_accessed(when);
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for kid in std::fs::read_dir(&d).unwrap().flatten() {
                let path = kid.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(times)
                    .unwrap();
            }
        }
    }

    /// Compiler output nothing has written in a long time is named — and a build directory a lane
    /// is still using is not, **by the same call that named the first**.
    ///
    /// The two halves are one test on purpose. A sweep that reports nothing is indistinguishable
    /// from a sweep that cannot see anything (SKEIN-647), so the silence is only worth asserting
    /// beside a fixture the same call *does* name.
    ///
    /// **What would make each fail**: dropping the `whole <= days` filter in `untouched_for`, so
    /// every file in the fleet qualifies and the fresh directory is offered too; and inverting it,
    /// so nothing old ever is. Watched — see the report.
    #[test]
    fn compiler_output_nothing_has_touched_is_named_and_a_live_build_directory_is_not() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        let box_dir = root.join("web-main");
        // `plant_fleet` made `web-main/tree`, so neither of these is an orphan: the question here
        // is age and only age.
        std::fs::create_dir_all(box_dir.join("tree/src")).unwrap();
        plant_build(&box_dir, "target-fresh", "tree", 9);

        let nothing_old = stale_builds(5).expect("a fleet with a box is answerable");
        assert!(
            nothing_old.is_empty(),
            "a build directory a lane wrote a moment ago was offered for deletion on the strength \
             of its age — which is the offer that costs somebody their afternoon: {nothing_old:?}"
        );

        plant_build(&box_dir, "target-private", "tree", 9);
        age_by(&box_dir.join("target-private"), 12);

        let found = stale_builds(5).expect("a fleet with a box is answerable");
        assert_eq!(
            found.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            vec![box_dir.join("target-private").display().to_string()],
            "the sweep named the wrong set: the silence above proves nothing unless this call, \
             which differs only by a directory nobody has touched in twelve days, names it and \
             nothing else: {found:?}"
        );
        assert!(
            found[0].bytes >= 40_000 && found[0].files >= 2,
            "the reclaimable figure is {} bytes in {} files, which cannot include the \
             40,000-byte artefact planted in it — a size nobody can act on is not a report",
            found[0].bytes,
            found[0].files
        );
        assert_eq!(
            found[0].oldest_days, 12,
            "the age skein reports is not the age the fixture has, so the evidence in the offer is \
             not about these files: {found:?}"
        );
        assert!(
            found[0].of >= found[0].bytes,
            "the directory is reported as smaller than the part of it being offered: {found:?}"
        );
    }

    /// **The threshold is the owner's, and setting it two ways gives two answers.**
    ///
    /// This is the whole of SKEIN-974's "it must be configurable": not that a field exists, but
    /// that what skein offers follows it. Driven through `config::load_config` and a real
    /// `config.json` rather than by passing a literal, because a constant somebody edits and
    /// rebuilds would pass an assertion written against the argument alone.
    ///
    /// **What would make this fail**: `stale_builds` ignoring its argument for a hardcoded 5 — the
    /// thirty-day assertion then names the directory anyway. Watched — see the report.
    #[test]
    fn the_age_threshold_is_a_setting_and_changing_it_changes_what_is_offered() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        let box_dir = root.join("web-main");
        std::fs::create_dir_all(box_dir.join("tree/src")).unwrap();
        plant_build(&box_dir, "target-private", "tree", 9);
        age_by(&box_dir.join("target-private"), 12);

        let offered = |days: u32| {
            std::fs::write(
                skein_home().join("config.json"),
                format!(r#"{{"stale_build_days":{days}}}"#),
            )
            .unwrap();
            let setting = crate::config::load_config().stale_build_days;
            assert_eq!(
                setting, days,
                "the file said {days} and settings read {setting}"
            );
            stale_builds(setting)
                .expect("a fleet with a box is answerable")
                .len()
        };

        assert_eq!(
            offered(1),
            1,
            "a threshold of one day does not reach output twelve days untouched, so the setting \
             is not what decides"
        );
        assert_eq!(
            offered(30),
            0,
            "a threshold of thirty days still offers output twelve days untouched — the same \
             fleet, the same files, and the only thing that changed was the setting"
        );
        // And the default is the owner's number rather than whatever the last case wrote.
        std::fs::remove_file(skein_home().join("config.json")).unwrap();
        assert_eq!(
            crate::config::load_config().stale_build_days,
            5,
            "a fleet with no config.json does not get the five days SKEIN-974 settled on"
        );
    }

    /// A directory that merely *looks* like build output is never examined, **however old it is**.
    ///
    /// The companion to `a_directory_that_only_looks_like_build_output_is_never_offered`, and the
    /// one that matters most for this sweep: age is the loosest evidence in this module, so the
    /// thing it is allowed to qualify has to be pinned by something else. `notes` here is twelve
    /// days old and full of files, which is everything an age check on its own would fire on. It
    /// carries no [`CARGO_TARGET_MARKER`], so cargo never made it, so skein says nothing about it.
    ///
    /// **What would make this fail**: walking every directory in a box rather than starting from
    /// cargo's marker. Watched — see the report; `notes` was named for deletion.
    #[test]
    fn age_never_selects_a_directory_cargo_did_not_make() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        let box_dir = root.join("web-main");
        std::fs::create_dir_all(box_dir.join("tree/src")).unwrap();
        // Real, old build output, so a sweep that found nothing at all cannot pass this test.
        plant_build(&box_dir, "target-private", "tree", 9);
        // Somebody's twelve-day-old working notes, which no build would ever regenerate.
        let keep = box_dir.join("notes");
        std::fs::create_dir_all(keep.join("debug/deps")).unwrap();
        std::fs::write(keep.join("debug/deps/draft.rlib"), vec![7u8; 40_000]).unwrap();
        age_by(&box_dir, 12);

        let found = stale_builds(5).expect("a fleet with a box is answerable");

        assert_eq!(
            found.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            vec![box_dir.join("target-private").display().to_string()],
            "an old directory cargo never made was offered for deletion, which is the failure \
             every rule in this module exists to prevent — and here the only evidence was its \
             age: {found:?}"
        );
        assert!(
            keep.join("debug/deps/draft.rlib").is_file(),
            "a report removed somebody's file — the one thing this must never do"
        );
    }

    /// The two ways this sweep can come up empty are refusals, not answers.
    ///
    /// No box is [`orphaned_builds`]' refusal and the same one. A threshold of zero is this
    /// sweep's own: it would qualify every artefact in the fleet, so offering "everything" is the
    /// answer skein must not give, and `0` is instead how the owner turns the offer off.
    ///
    /// **What would make each fail**: returning `Ok(vec![])` for a fleet with no boxes, and
    /// treating `0` as an ordinary threshold. Watched — see the report; with `0` accepted the
    /// sweep returned the fresh build directory.
    #[test]
    fn no_boxes_and_a_zero_threshold_are_both_refusals() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        std::fs::create_dir_all(root.join(".skein")).unwrap();
        let why = stale_builds(5)
            .expect_err("no box means the sweep cannot say whose build output anything is");
        assert!(
            why.contains("no boxes at all") && why.contains("Refusing"),
            "the refusal does not say what could not be derived, so a reader cannot tell it from a \
             clean fleet: {why}"
        );

        plant_fleet(&root, &["web-main"], &[]);
        let box_dir = root.join("web-main");
        std::fs::create_dir_all(box_dir.join("tree/src")).unwrap();
        plant_build(&box_dir, "target-fresh", "tree", 9);
        let zero = stale_builds(0)
            .expect_err("zero days would qualify the artefact cargo wrote a second ago");
        assert!(
            zero.contains("zero days") && zero.contains("Refusing"),
            "the refusal does not say why zero is not a threshold: {zero}"
        );
        assert_eq!(
            stale_build_advice(
                &[StaleBuild {
                    path: box_dir.join("target-fresh").display().to_string(),
                    bytes: 40_000,
                    files: 1,
                    of: 40_000,
                    oldest_days: 12,
                }],
                0
            ),
            None,
            "the offer spoke on a fleet whose owner set the threshold to zero to switch it off"
        );
    }

    /// The offer says out loud that its evidence is the weak kind, hands over a command that names
    /// the same days the figures came from, and deletes nothing.
    ///
    /// **Why the wording is an assertion.** [`orphaned_build_advice`] can tell a reader *why* a
    /// directory is dead; this one cannot, and a reader who cannot tell the two offers apart will
    /// weigh them the same. The only thing that makes an age admissible here at all is that being
    /// wrong costs a rebuild rather than work, so that has to be in the sentence rather than in a
    /// doc comment nobody staring at a full disk will read.
    ///
    /// **What would make each fail**: dropping the "weaker kind" clause; printing a `-mtime` that
    /// is not the threshold the figures were derived from, so a reader checking the command finds
    /// a different set of files. Watched — see the report.
    #[test]
    fn the_stale_offer_says_its_evidence_is_the_weak_kind_and_deletes_nothing() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (root, _pins) = pinned_fleet(&dir);
        plant_fleet(&root, &["web-main"], &[]);
        let box_dir = root.join("web-main");
        std::fs::create_dir_all(box_dir.join("tree/src")).unwrap();
        plant_build(&box_dir, "target-private", "tree", 9);
        age_by(&box_dir.join("target-private"), 12);

        assert_eq!(
            stale_build_advice(&[], 5),
            None,
            "there is nothing to say about nothing"
        );

        let found = stale_builds(5).expect("a fleet with a box is answerable");
        let offer =
            stale_build_advice(&found, 5).expect("one was found, so there is something to say");

        assert!(
            offer.contains("weaker kind") && offer.contains("slower rebuild and never work"),
            "the offer does not say that this evidence is the weak one, so a reader weighs it the \
             same as a directory skein can prove is dead: {offer}"
        );
        assert!(
            offer.contains("the oldest 12 days") && offer.contains("nothing has touched in 5 days"),
            "the offer states no evidence a reader can check before deleting anything: {offer}"
        );
        assert!(
            offer.contains("stale_build_days"),
            "the offer never says the threshold is the reader's to change, so an offer they \
             disagree with has no answer but to ignore it: {offer}"
        );
        let command = offer
            .lines()
            .find(|l| l.trim_start().starts_with("find "))
            .unwrap_or_else(|| panic!("the offer names a problem and no way out of it: {offer}"));
        assert!(
            command.contains("-mtime +5") && command.contains("-type f"),
            "the command does not select the files the figures above it were derived from, so a \
             reader who runs it reclaims a different amount than skein said: {command}"
        );
        assert!(
            command.contains(&box_dir.join("target-private").display().to_string()),
            "the command does not name what the sentence above it named: {command}"
        );
        assert!(
            box_dir
                .join("target-private/debug/deps/libthing.rlib")
                .is_file(),
            "a report removed the artefact it was reporting — the one thing this must never do"
        );
    }

    /// Every `.skein/<name>` the code spells is either a directory the sweep knows as skein's own
    /// or a file the sweep never looks at, and **a new one that is neither fails here**.
    ///
    /// This is the guard that lets `substrate_strays` offer a `rm -rf` at all. Its shape is
    /// `tests/ui/harness/leaks.mjs`: derive the names from the call sites, print them, and refuse
    /// to run rather than pass when the derivation finds none — because a scan that has stopped
    /// matching anything is indistinguishable from a clean tree by its result alone (SKEIN-647).
    ///
    /// **What would make this fail**: a seventh directory under `.skein` that
    /// `installed_substrate_dirs` does not return. Watched, by adding one more `fleet_root()`-
    /// anchored path function spelling `attic` to this file: the scan found it, it was in neither
    /// set, and the test named it. The doc cannot quote that line — this scan reads comments too,
    /// which is how the experiment was caught a second time.
    #[test]
    fn substrate_names_the_code_spells_are_all_accounted_for() {
        let _lock = env_lock();
        let dir = crate::testutil::tempdir();
        let (_root, _pins) = pinned_fleet(&dir);

        /// The names skein installs in `.skein` that are FILES. Being a list is safe only because
        /// the scan below is what decides: a name the code gains and this does not carry fails, and
        /// a name this carries and the code has dropped fails too.
        ///
        /// `server.tmux` left this list with SKEIN-529, when the socket moved under `private/` on
        /// both sides, and came back with SKEIN-1020 for `review-github.token`'s reason:
        /// `start-door.sh` spells the OLD path, because a fleet serving since before the move still
        /// has its cockpit's session there and the upgrade has to find it and move it. It is the
        /// live socket on such a fleet, so it is skein's own and never a stray.
        /// `review-github.token` stays because `stale_sandbox_secrets` still spells the OLD path it
        /// exists to delete.
        const FILES: &[&str] = &[
            "box-session.sh",
            "fleet-size",
            "git-credential-skein",
            "review-github.token",
            "server-doorway.py",
            "server.door",
            "server.tmux",
            "skein",
            "skein-home",
            "skein-server",
            "skein-startup.sh",
            "start-door.sh",
        ];

        // The name that follows an anchor, up to the next character a filename cannot hold. A
        // trailing `.new` is the atomic-install temporary of the name beside it (`bootstrap.sh`
        // writes `$skein_dir/skein-home.new` and renames), not a name of its own.
        fn names_after(text: &str, anchor: &str, into: &mut std::collections::BTreeSet<String>) {
            let mut rest = text;
            while let Some(at) = rest.find(anchor) {
                rest = &rest[at + anchor.len()..];
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || ".-_".contains(*c))
                    .collect();
                if !name.is_empty() {
                    into.insert(name.trim_end_matches(".new").to_string());
                }
            }
        }

        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut spelled: std::collections::BTreeSet<String> = Default::default();
        // Rust spells it `format!("{}/.skein/<name>", fleet_root())`. The `{}` is what keeps a
        // fixture's absolute `/boxes/.skein/...` string — this file's own tests have several — out
        // of a scan that is meant to read production.
        let mut sources = Vec::new();
        walk(&repo.join("src"), &mut sources);
        for path in sources {
            if path.extension().is_some_and(|e| e == "rs") {
                names_after(
                    &std::fs::read_to_string(&path).unwrap_or_default(),
                    "{}/.skein/",
                    &mut spelled,
                );
            }
        }
        // The shell halves: the launcher, which spells the root as a variable, and the installer,
        // which keeps it in `skein_dir`. Anchored exactly, because both files also discuss the
        // HOST's `~/.skein` — a looser anchor reads `boxes` and `repos` out of a comment.
        for (file, anchors) in [
            (
                "src/box-session.sh",
                &[
                    "${SKEIN_FLEET_ROOT:-/boxes}/.skein/",
                    "$fleet_root_dir/.skein/",
                ][..],
            ),
            (
                "bootstrap.sh",
                &["$fleet_root/.skein/", "/boxes/.skein/", "$skein_dir/"][..],
            ),
        ] {
            let text = std::fs::read_to_string(repo.join(file)).expect(file);
            for anchor in anchors {
                names_after(&text, anchor, &mut spelled);
            }
        }

        eprintln!("substrate names the code spells: {spelled:?}");
        assert!(
            spelled.len() > 10,
            "the scan found {} names under `.skein` and the fleet has always had more than ten — \
             the way the code spells that path has changed and this test now reads nothing, which \
             is green about anything: {spelled:?}",
            spelled.len()
        );

        let installed = installed_substrate_dirs();
        let loose: Vec<&String> = spelled
            .iter()
            .filter(|n| !installed.contains(*n) && !FILES.contains(&n.as_str()))
            .collect();
        assert!(
            loose.is_empty(),
            "the code puts {loose:?} in `.skein` and `substrate_strays` classifies them as \
             neither its own nor a box's — so it would offer a `rm -rf` naming skein's own \
             installation. Add each to `installed_substrate_dirs` if it is a directory, or to \
             FILES here if it is a file."
        );
        for name in installed
            .iter()
            .map(String::as_str)
            .chain(FILES.iter().copied())
        {
            assert!(
                spelled.contains(name),
                "{name} is carried as something skein installs and nothing in the code spells it \
                 any more, so this test is asserting against a fleet that no longer exists. Found: \
                 {spelled:?}"
            );
        }
    }

    #[test]
    fn disk_usage_reads_dus_own_output_and_ignores_anything_else() {
        let got = parse_disk_usage(
            "3483\t/boxes/PROJ-S6/\n21\t/boxes/bridge-a-b-master/\ndu: cannot access 'x'\n\n",
        );
        assert_eq!(got.get("PROJ-S6"), Some(&3483));
        assert_eq!(got.get("bridge-a-b-master"), Some(&21));
        assert_eq!(got.len(), 2, "a stray line became a box: {got:?}");
    }

    /// **The fleet root is quoted and the glob is not** (FLEET-7).
    ///
    /// `fleet_root()` reads `$SKEIN_FLEET_ROOT`, which this file already treats as untrusted where
    /// it builds a `pkill` pattern — "a `+`, `[`, `(`, `*`, `?` or `|` in it left the pattern
    /// matching something OTHER" — and then interpolated raw here, the one such site in a file that
    /// quotes everything else. A `[` in the path is enough: bash reads it as a character class, the
    /// pattern matches nothing, the word is passed through unexpanded, `du` fails on a path that
    /// does not exist and `2>/dev/null` makes the whole thing look like a fleet using no disk.
    ///
    /// Run rather than read, because what has to hold is that the *glob still globs* after the
    /// quoting — a fix that quoted the whole word would pass a source-shaped assertion and report
    /// nothing for every fleet.
    #[test]
    fn a_fleet_root_with_a_glob_character_in_it_still_reports_every_boxs_disk() {
        let dir = crate::testutil::tempdir();
        // `[e]` matches the literal `e` as a class, so an unquoted pattern misses this directory
        // while a quoted one finds it — the smallest character that tells the two apart.
        let root = (dir.as_ref() as &std::path::Path).join("box[e]s");
        for name in ["web-main", "api-worker"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("blob"), vec![0u8; 4096]).unwrap();
        }
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(disk_usage_script(&root.display().to_string()))
            .output()
            .expect("bash");
        let got = parse_disk_usage(&String::from_utf8_lossy(&out.stdout));
        assert!(
            got.contains_key("web-main") && got.contains_key("api-worker"),
            "a `[` in the fleet root left every box unmeasured, and silently: {got:?}"
        );
    }

    /// **A space in the fleet root breaks the unquoted form too** (FLEET-7), by word-splitting
    /// rather than by glob-parsing: `du -sxm {root}/*/` split into two words at the space, `du`
    /// stat'd two paths that do not exist, and `2>/dev/null` hid the failure the same way the `[`
    /// case did. Quoting the root rejoins it into one word while leaving `*` outside the quotes
    /// free to still glob — the same distinction the sibling test above checks for `[`.
    #[test]
    fn a_fleet_root_with_a_space_in_it_still_reports_every_boxs_disk() {
        let dir = crate::testutil::tempdir();
        let root = (dir.as_ref() as &std::path::Path).join("box es");
        for name in ["web-main", "api-worker"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("blob"), vec![0u8; 4096]).unwrap();
        }
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(disk_usage_script(&root.display().to_string()))
            .output()
            .expect("bash");
        let got = parse_disk_usage(&String::from_utf8_lossy(&out.stdout));
        assert!(
            got.contains_key("web-main") && got.contains_key("api-worker"),
            "a space in the fleet root left every box unmeasured, and silently: {got:?}"
        );
    }
}
