//! What in one box can be made again — build output, download caches, the scratchpads of ended
//! sessions — how big it is, and the one place skein removes it, when the person asks (SKEIN-1341).
//!
//! `disk.rs` beside this reports and offers and never deletes, and that stays true. This is the
//! other half the owner asked for on 2026-10-26: a per-box clean behind a confirmation that names
//! every path and its size. So the rules here are about what may never be on that list:
//!
//!   * **Build output and scratchpads only from a stopped box.** A running box may be using them —
//!     a build in its target directory, the scratchpad of a session that is still going.
//!     [`clean_box`] refuses them unless the board's own liveness sweep says the box is stopped,
//!     and "cannot tell" is a refusal too. Download caches may be cleaned from a running box (the
//!     owner's answer, 2026-10-26): the tool downloads again what it needs.
//!   * **Nothing a symlink leads to.** The walks read `symlink_metadata` and never descend through
//!     a link, so `~/shared` (a link into the repo's store) and anything else a box linked in is
//!     neither measured nor offered. Every candidate is also checked to be on the box directory's
//!     own device and under it.
//!   * **No work in a checkout.** A build directory inside a git checkout is offered only when git
//!     says the path is ignored, which is false for a tracked path and for untracked work.
//!   * **No conversation.** Scratchpads are offered, transcripts never; and the scratchpad of the
//!     conversation a restart would continue is kept.
//!   * **What is removed is what was shown.** The clean takes the paths the person confirmed and
//!     removes only those that are still on a list worked out again at that moment.

use super::*;
use std::path::Path;

/// One thing in a box that can be removed and made again, with its size.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Cleanable {
    /// Absolute, as the fleet sees it.
    pub path: String,
    /// The same path for a person: `~/…` under the box's home, `<box>/…` elsewhere in it.
    pub shown: String,
    /// `build output`, `download cache` or `scratchpad`.
    pub kind: &'static str,
    /// Allocated bytes, by the walk every other disk figure in skein comes from.
    pub bytes: u64,
}

/// Download caches under a box's home, and what each one is. A fixed list on purpose: a box's
/// `~/.cache` also holds whatever its agent chose to put there — logs, patches, backups of files it
/// was editing — measured on a live fleet, so "everything under `~/.cache`" is not regenerable.
const DOWNLOAD_CACHES: &[&str] = &[
    ".cache/ms-playwright",
    ".cache/pip",
    ".cache/uv",
    ".cache/pnpm",
    ".npm/_cacache",
    ".cargo/registry",
    ".cargo/git",
];

/// How deep the build-output walk goes below each of a box's top directories. A checkout's
/// `target` is two levels down (`tree/target`), a worktree's under the box's `/var/tmp` three.
const BUILD_DEPTH: usize = 5;

/// Everything in box `name` that [`clean_box`] would remove, biggest first.
///
/// Reading only; a running box is measured like any other, so the person can see what a clean
/// would free before deciding to stop it.
pub fn box_cleanables(name: &str) -> Result<Vec<Cleanable>, String> {
    if !crate::util::valid_name(name) {
        return Err(format!("{name:?} is not a box name"));
    }
    let box_dir = std::path::Path::new(&fleet_root()).join(name);
    let Ok(meta) = std::fs::symlink_metadata(&box_dir) else {
        return Err(format!("{name} has no directory in this fleet"));
    };
    if !meta.is_dir() {
        return Err(format!("{name} has no directory in this fleet"));
    }
    use std::os::unix::fs::MetadataExt;
    let dev = meta.dev();
    let home = box_dir.join("home");
    let mut found: Vec<(std::path::PathBuf, &'static str)> = Vec::new();

    for rel in DOWNLOAD_CACHES {
        let path = home.join(rel);
        if real_all_the_way(&box_dir, &path, dev) {
            found.push((path, "download cache"));
        }
    }
    found.extend(
        ended_scratchpads(&box_dir, &home, dev)
            .into_iter()
            .map(|p| (p, "scratchpad")),
    );
    let skip: Vec<std::path::PathBuf> = found.iter().map(|(p, _)| p.clone()).collect();
    let mut builds = Vec::new();
    for top in ["home", "tree", "vartmp"] {
        build_dirs(&box_dir.join(top), dev, BUILD_DEPTH, &skip, &mut builds);
    }
    for build in builds {
        if !inside_a_checkout_and_kept(&box_dir, &build) {
            found.push((build, "build output"));
        }
    }

    let mut out: Vec<Cleanable> = found
        .into_iter()
        .map(|(path, kind)| Cleanable {
            shown: shown(&box_dir, &path),
            bytes: tree_bytes(&path, Some(dev)),
            path: path.display().to_string(),
            kind,
        })
        .filter(|c| c.bytes > 0 && real_all_the_way(&box_dir, Path::new(&c.path), dev))
        .collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    Ok(out)
}

/// The file cargo holds an exclusive `flock` on for the whole of a build, one per profile
/// directory: `<target>/debug/.cargo-lock`, `<target>/release/.cargo-lock`.
pub const CARGO_BUILD_LOCK: &str = ".cargo-lock";

/// Which of `build_dirs` a build is running in now (SKEIN-1391): cargo holds the lock on one of
/// its profile directories' [`CARGO_BUILD_LOCK`] files, as `/proc/locks` lists it.
///
/// **The lock, not the process.** A target directory's mtime, a process's working directory and
/// its open files all miss the gaps between two rustc runs; cargo's lock is held from the start of
/// the build to its end. And it is **only read, never taken**: a probe `flock` of the file would
/// make the box's own cargo print "Blocking waiting for file lock" for as long as skein held it.
///
/// `/proc/locks` names a file by its device and inode. Both are matched, since an inode number is
/// only unique on one filesystem. A directory skein cannot read, or a `/proc/locks` it cannot read,
/// is a build that is not running: the ask then stays the size it would have been without this.
///
/// **`/proc/locks` lists a lock under the pid that took it**, and leaves it out once that pid is
/// gone or is not visible in the reader's pid namespace. So a lock taken by one process and held
/// through a descriptor it handed to a child — `flock(1)` running a command — is held and not
/// listed (measured: its `fdinfo` showed it under pid 0). Cargo takes and holds its lock in its own
/// process for the whole build, so a running cargo is listed; skein reads from outside every box's
/// namespace, and a box's own tools from inside it, and both see the box's cargo.
pub fn live_builds(build_dirs: &[&str]) -> std::collections::BTreeSet<String> {
    let locks = std::fs::read_to_string("/proc/locks").unwrap_or_default();
    live_builds_in(&locks, build_dirs)
}

/// [`live_builds`] against the text of a `/proc/locks`.
pub(crate) fn live_builds_in(
    locks: &str,
    build_dirs: &[&str],
) -> std::collections::BTreeSet<String> {
    use std::os::unix::fs::MetadataExt;
    let held = held_locks(locks);
    let mut out = std::collections::BTreeSet::new();
    if held.is_empty() {
        return out;
    }
    for dir in build_dirs {
        let Ok(profiles) = std::fs::read_dir(dir) else {
            continue;
        };
        let running = profiles.flatten().any(|p| {
            std::fs::symlink_metadata(p.path().join(CARGO_BUILD_LOCK))
                .is_ok_and(|m| m.is_file() && held.contains(&(dev_pair(m.dev()), m.ino())))
        });
        if running {
            out.insert(dir.to_string());
        }
    }
    out
}

/// Every file `/proc/locks` lists, as ((major, minor), inode). The kernel writes each lock as
/// `1: FLOCK  ADVISORY  WRITE 1234 fd:01:5678 0 EOF`, the device's major and minor in hex and the
/// inode in decimal; a waiter's line is marked `->` and names the same file as the lock it waits on.
fn held_locks(text: &str) -> std::collections::BTreeSet<((u64, u64), u64)> {
    text.lines()
        .flat_map(|l| l.split_whitespace())
        .filter_map(|word| {
            let mut parts = word.split(':');
            let (major, minor, ino) = (parts.next()?, parts.next()?, parts.next()?);
            if parts.next().is_some() {
                return None;
            }
            Some((
                (
                    u64::from_str_radix(major, 16).ok()?,
                    u64::from_str_radix(minor, 16).ok()?,
                ),
                ino.parse::<u64>().ok()?,
            ))
        })
        .collect()
}

/// A `st_dev` as the kernel's (major, minor), decoded the way glibc's `major` and `minor` do.
fn dev_pair(dev: u64) -> (u64, u64) {
    (
        ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000),
        (dev & 0xff) | ((dev >> 12) & 0xffff_ff00),
    )
}

/// What [`clean_box`] did: what it removed and freed, and what it could not remove and why.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Cleaned {
    pub removed: Vec<String>,
    pub freed: u64,
    pub failed: Vec<(String, String)>,
}

/// Remove `paths` from box `name`, when the box is stopped and every one of them is still on
/// [`box_cleanables`]' list.
pub fn clean_box(name: &str, paths: &[String]) -> Result<Cleaned, String> {
    let running = crate::fleet::fleet_liveness().get(name).copied();
    clean_box_given(name, paths, running)
}

/// [`clean_box`] with the liveness answer passed in, so each refusal can be tested on a machine
/// with no fleet.
pub(crate) fn clean_box_given(
    name: &str,
    paths: &[String],
    running: Option<bool>,
) -> Result<Cleaned, String> {
    clean_box_between(name, paths, running, &|| {})
}

/// [`clean_box_given`] with `between` run after the last check and before each removal: the
/// window a running box could use to swap a directory in the path for a link. Tests put a swap
/// there; production passes nothing.
fn clean_box_between(
    name: &str,
    paths: &[String],
    running: Option<bool>,
    between: &dyn Fn(),
) -> Result<Cleaned, String> {
    if paths.is_empty() {
        return Err("nothing was chosen to remove".to_string());
    }
    let now = box_cleanables(name)?;
    let stale: Vec<&String> = paths
        .iter()
        .filter(|p| !now.iter().any(|c| &c.path == *p))
        .collect();
    if !stale.is_empty() {
        return Err(format!(
            "{name} changed since you looked, so skein removed nothing. Open its settings again \
             to see what can be cleaned now. (No longer on the list: {})",
            stale
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // The owner's answer (2026-10-26): a download cache may be cleaned while the box runs, since
    // the tool downloads it again; build output and scratchpads need the box stopped, since a
    // build or a session may be using them right now.
    let needs_stop = now
        .iter()
        .any(|c| paths.contains(&c.path) && c.kind != "download cache");
    if needs_stop {
        match running {
            Some(false) => {}
            Some(true) => {
                return Err(format!(
                    "{name} is running, so skein removed nothing. Its download caches can be \
                     cleaned now; build output and scratchpads need it stopped. Stop it, then \
                     clean again."
                ))
            }
            None => {
                return Err(format!(
                    "skein cannot tell whether {name} is running, so it removed nothing. Its \
                     download caches can be cleaned anyway; for build output and scratchpads, try \
                     again in a moment."
                ))
            }
        }
    }
    let box_dir = Path::new(&fleet_root()).join(name);
    let dev = {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(&box_dir)
            .map(|m| m.dev())
            .map_err(|e| format!("{name} has no directory in this fleet ({e})"))?
    };
    let mut done = Cleaned::default();
    for c in now.iter().filter(|c| paths.contains(&c.path)) {
        // Again, at the moment of removal: a link put into the chain since the list was made
        // must not lead the removal out of this box.
        if !real_all_the_way(&box_dir, Path::new(&c.path), dev) {
            done.failed.push((
                c.path.clone(),
                "part of this path is now a link, so skein left it alone".to_string(),
            ));
            continue;
        }
        between();
        match remove_without_following(&box_dir, Path::new(&c.path), dev) {
            Ok(()) => {
                done.removed.push(c.path.clone());
                done.freed += c.bytes;
            }
            Err(e) => done.failed.push((c.path.clone(), e.to_string())),
        }
    }
    Ok(done)
}

/// What a repo's shared download cache holds that a clean may remove, relative to it (SKEIN-1350).
///
/// Named one by one, as [`DOWNLOAD_CACHES`] is, and not "everything under the cache": cargo's part
/// is a whole `CARGO_HOME`, so it can also hold a `config.toml` or a `credentials.toml` a box wrote
/// with `cargo login`, and neither is something a tool downloads again.
const REPO_CACHES: &[&str] = &[
    "cargo/registry",
    "cargo/git",
    "npm/_cacache",
    "npm/_npx",
    "pip",
    "uv",
    "pnpm",
];

/// A repo's shared download cache (SKEIN-1350): where it is, what of it a clean may remove, and
/// which of the repo's boxes would stop that.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoCache {
    /// The cache directory, `repos/<id>/store/cache` (`kit::repo_cache_dir`).
    pub path: String,
    /// What a clean may remove, biggest first. `shown` is relative to the cache.
    pub items: Vec<Cleanable>,
    /// The repo's boxes that are running. A clean waits until there are none.
    pub running: Vec<String>,
    /// The repo's boxes skein cannot tell about, which stop a clean just as a running one does.
    pub unknown: Vec<String>,
}

/// Repo `id`'s shared download cache, measured, or `None` when it has none: its store is kept
/// elsewhere, so its boxes keep their own caches.
///
/// Reading only. Its size is counted toward the repo and toward no box, because it is on the
/// store's disk and outside every box's directory, which is all a box's allowance measures.
pub fn repo_cache(id: &str) -> Result<Option<RepoCache>, String> {
    repo_cache_given(id, &crate::fleet::fleet_liveness())
}

/// [`repo_cache`] with the liveness answer passed in.
fn repo_cache_given(
    id: &str,
    liveness: &std::collections::HashMap<String, bool>,
) -> Result<Option<RepoCache>, String> {
    let repo = crate::repos::load_repos()
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("{id} is not a registered repo"))?;
    let Some(dir) = crate::kit::repo_cache_dir(Path::new(&repo.store)) else {
        return Ok(None);
    };
    let mut items = Vec::new();
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = std::fs::symlink_metadata(&dir) {
        if meta.is_dir() {
            let dev = meta.dev();
            for rel in REPO_CACHES {
                let path = dir.join(rel);
                if !real_all_the_way(&dir, &path, dev) {
                    continue;
                }
                let bytes = tree_bytes(&path, Some(dev));
                if bytes > 0 {
                    items.push(Cleanable {
                        path: path.display().to_string(),
                        shown: rel.to_string(),
                        kind: "download cache",
                        bytes,
                    });
                }
            }
        }
    }
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    let (mut running, mut unknown) = (Vec::new(), Vec::new());
    for name in boxes_of_repo(id) {
        match liveness.get(&name) {
            Some(false) => {}
            Some(true) => running.push(name),
            None => unknown.push(name),
        }
    }
    Ok(Some(RepoCache {
        path: dir.display().to_string(),
        items,
        running,
        unknown,
    }))
}

/// Every box directory in the fleet that belongs to repo `id`, by the rule the board uses
/// (`repos::repo_for_box`: the longest registered id the name starts with).
fn boxes_of_repo(id: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(fleet_root()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| crate::util::valid_name(n))
        .filter(|n| crate::repos::repo_for_box(n).is_some_and(|r| r.id == id))
        .collect();
    out.sort();
    out
}

/// Remove `paths` from repo `id`'s shared download cache, when none of its boxes is running and
/// every one of them is still on [`repo_cache`]'s list.
///
/// **Only with every box of the repo stopped**, which is the stricter of the two rules the item
/// allowed. A box's own download cache may go while it runs, since only that box reads it; this
/// one is read by every box of the repo, a removal does not take cargo's lock, and a build in a
/// sibling may be compiling out of `cargo/registry/src` at that moment.
pub fn clean_repo_cache(id: &str, paths: &[String]) -> Result<Cleaned, String> {
    clean_repo_cache_given(id, paths, &crate::fleet::fleet_liveness())
}

/// [`clean_repo_cache`] with the liveness answer passed in, so each refusal can be tested on a
/// machine with no fleet.
pub(crate) fn clean_repo_cache_given(
    id: &str,
    paths: &[String],
    liveness: &std::collections::HashMap<String, bool>,
) -> Result<Cleaned, String> {
    if paths.is_empty() {
        return Err("nothing was chosen to remove".to_string());
    }
    let Some(now) = repo_cache_given(id, liveness)? else {
        return Err(format!(
            "{id} has no shared download cache: its store is kept elsewhere, so its boxes keep \
             their own. Clean them in each box's settings."
        ));
    };
    let stale: Vec<&String> = paths
        .iter()
        .filter(|p| !now.items.iter().any(|c| &c.path == *p))
        .collect();
    if !stale.is_empty() {
        return Err(format!(
            "{id}'s download cache changed since you looked, so skein removed nothing. Open the \
             repo again to see what can be cleaned now."
        ));
    }
    if !now.running.is_empty() {
        return Err(format!(
            "skein removed nothing: {} {} running, and every box of {id} shares this cache. \
             Stop {}, then clean again.",
            now.running.join(", "),
            if now.running.len() == 1 { "is" } else { "are" },
            if now.running.len() == 1 { "it" } else { "them" },
        ));
    }
    if !now.unknown.is_empty() {
        return Err(format!(
            "skein cannot tell whether {} {} running, so it removed nothing. Try again in a \
             moment.",
            now.unknown.join(", "),
            if now.unknown.len() == 1 { "is" } else { "are" },
        ));
    }
    let dir = Path::new(&now.path);
    let dev = {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(dir)
            .map(|m| m.dev())
            .map_err(|e| format!("{id}'s download cache is gone ({e})"))?
    };
    let mut done = Cleaned::default();
    for c in now.items.iter().filter(|c| paths.contains(&c.path)) {
        if !real_all_the_way(dir, Path::new(&c.path), dev) {
            done.failed.push((
                c.path.clone(),
                "part of this path is now a link, so skein left it alone".to_string(),
            ));
            continue;
        }
        match remove_without_following(dir, Path::new(&c.path), dev) {
            Ok(()) => {
                done.removed.push(c.path.clone());
                done.freed += c.bytes;
            }
            Err(e) => done.failed.push((c.path.clone(), e.to_string())),
        }
    }
    Ok(done)
}

/// The shared download cache of box `name`'s repo, as [`repo_cache`] measures it, for the box's own
/// `skein_clean` (SKEIN-1356). `None` when the box has no repo or the repo's store is kept
/// elsewhere. `running` and `unknown` name the repo's OTHER boxes only, so a box is never held back
/// by itself: its agent knows what it is running.
///
/// `measured` holds each repo's cache as measured once, keyed by repo id, so a pass over many boxes
/// of one repo walks its cache once.
pub fn repo_cache_for_box(
    name: &str,
    liveness: &std::collections::HashMap<String, bool>,
    measured: &mut std::collections::BTreeMap<String, Option<RepoCache>>,
) -> Result<Option<RepoCache>, String> {
    let Some(repo) = crate::repos::repo_for_box(name) else {
        return Ok(None);
    };
    let mut found = match measured.get(&repo.id) {
        Some(m) => m.clone(),
        None => {
            let m = repo_cache_given(&repo.id, liveness)?;
            measured.insert(repo.id.clone(), m.clone());
            m
        }
    };
    if let Some(rc) = found.as_mut() {
        let (running, unknown) = others_of_box(name, liveness);
        rc.running = running;
        rc.unknown = unknown;
    }
    Ok(found)
}

/// The other boxes of box `name`'s repo that are running, and those skein cannot tell about, by
/// the rule [`clean_repo_cache`] uses. Cheap: a directory listing and a liveness map, no walk.
pub fn others_of_box(
    name: &str,
    liveness: &std::collections::HashMap<String, bool>,
) -> (Vec<String>, Vec<String>) {
    let (mut running, mut unknown) = (Vec::new(), Vec::new());
    let Some(repo) = crate::repos::repo_for_box(name) else {
        return (running, unknown);
    };
    for other in boxes_of_repo(&repo.id) {
        if other == name {
            continue;
        }
        match liveness.get(&other) {
            Some(false) => {}
            Some(true) => running.push(other),
            None => unknown.push(other),
        }
    }
    (running, unknown)
}

/// Remove the directory `path` under `box_dir` in a way that cannot follow a link anywhere in it.
///
/// Checking the path and then removing it by name is not enough once a running box may be cleaned:
/// the box can swap `~/.cache` for a link between the check and the removal, and a removal by path
/// resolves every component again and follows the link into another box. So each component is
/// opened from the one before it with `O_NOFOLLOW`, checked to be on the box's device, and the
/// leaf is removed through the held parent descriptor by [`remove_tree_at`], which never follows a
/// link either.
fn remove_without_following(box_dir: &Path, path: &Path, dev: u64) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let refused = |why: &str| std::io::Error::other(why.to_string());
    let rel = path
        .strip_prefix(box_dir)
        .map_err(|_| refused("not inside this box"))?;
    let parts: Vec<std::ffi::CString> = rel
        .components()
        .map(|c| match c {
            std::path::Component::Normal(p) => std::ffi::CString::new(p.as_bytes())
                .map_err(|_| refused("a path component holds a NUL")),
            _ => Err(refused("not a plain path")),
        })
        .collect::<Result<_, _>>()?;
    let Some((leaf, chain)) = parts.split_last() else {
        return Err(refused("refusing to remove the box directory itself"));
    };
    let top = std::ffi::CString::new(box_dir.as_os_str().as_bytes())
        .map_err(|_| refused("the box path holds a NUL"))?;
    // SAFETY: a NUL-terminated path and flags; the result is checked before it is owned.
    let mut at = owned(unsafe {
        libc::open(
            top.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })?;
    on_device(&at, dev)?;
    for part in chain {
        at = open_dir_at(&at, part)
            .map_err(|_| refused("part of this path is now a link, so skein left it alone"))?;
        on_device(&at, dev)?;
    }
    let st = stat_at(&at, leaf)?;
    if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(refused("this is now a link, so skein left it alone"));
    }
    remove_tree_at(&at, leaf, dev)
}

fn owned(fd: libc::c_int) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a descriptor this process just opened and nothing else owns.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

fn open_dir_at(
    parent: &std::os::fd::OwnedFd,
    name: &std::ffi::CStr,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::AsRawFd;
    // SAFETY: a live descriptor, a NUL-terminated name, and flags.
    owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}

fn stat_at(parent: &std::os::fd::OwnedFd, name: &std::ffi::CStr) -> std::io::Result<libc::stat> {
    use std::os::fd::AsRawFd;
    // SAFETY: an all-zero `stat` is a valid value to be overwritten.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a live descriptor, a NUL-terminated name, and a buffer of the right type.
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(st)
}

fn on_device(fd: &std::os::fd::OwnedFd, dev: u64) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: as in `stat_at`.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a live descriptor and a buffer of the right type.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if st.st_dev as u64 != dev {
        return Err(std::io::Error::other(
            "part of this path is on another filesystem, so skein left it alone",
        ));
    }
    Ok(())
}

/// Remove `name` under the directory `parent`, recursively, never following a link: a link is
/// unlinked as itself, a directory is opened with `O_NOFOLLOW` from its parent's descriptor, and
/// a directory on another device (a mount point) is left alone.
fn remove_tree_at(
    parent: &std::os::fd::OwnedFd,
    name: &std::ffi::CStr,
    dev: u64,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let st = stat_at(parent, name)?;
    if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
        let dir = open_dir_at(parent, name)?;
        on_device(&dir, dev)?;
        for kid in entries(&dir)? {
            remove_tree_at(&dir, &kid, dev)?;
        }
        // SAFETY: a live descriptor and a NUL-terminated name.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    } else {
        // SAFETY: as above; without AT_REMOVEDIR a link is removed as itself.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The names in the directory `dir`, without `.` and `..`.
fn entries(dir: &std::os::fd::OwnedFd) -> std::io::Result<Vec<std::ffi::CString>> {
    use std::os::fd::AsRawFd;
    // SAFETY: `dup` of a live descriptor; `fdopendir` takes ownership of the copy.
    let copy = unsafe { libc::dup(dir.as_raw_fd()) };
    if copy < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `copy` is a directory descriptor this process owns.
    let stream = unsafe { libc::fdopendir(copy) };
    if stream.is_null() {
        let e = std::io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `copy` is still ours to close.
        unsafe { libc::close(copy) };
        return Err(e);
    }
    let mut out = Vec::new();
    loop {
        // SAFETY: `stream` is open until the `closedir` below.
        let ent = unsafe { libc::readdir(stream) };
        if ent.is_null() {
            break;
        }
        // SAFETY: `d_name` is NUL-terminated within the entry `readdir` returned.
        let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            out.push(name.to_owned());
        }
    }
    // SAFETY: closes the stream and the copy it owns.
    unsafe { libc::closedir(stream) };
    Ok(out)
}

/// Whether every component from `box_dir` down to `path` is a real directory on `dev`, never a
/// link. Checking only the last component is not enough: `symlink_metadata` follows every
/// component before the last, so a box that made its own `~/.cache` a link to another box's home
/// or into the store would have had that directory offered, and removed, as its own.
fn real_all_the_way(box_dir: &Path, path: &Path, dev: u64) -> bool {
    let Ok(rel) = path.strip_prefix(box_dir) else {
        return false;
    };
    if !is_real_dir(box_dir, dev) {
        return false;
    }
    let mut at = box_dir.to_path_buf();
    for part in rel.components() {
        match part {
            std::path::Component::Normal(p) => at.push(p),
            _ => return false,
        }
        if !is_real_dir(&at, dev) {
            return false;
        }
    }
    true
}

/// A directory, not a link to one, on the box directory's own device. About the last component
/// only: callers either walk down from the box directory one component at a time, or use
/// [`real_all_the_way`].
fn is_real_dir(path: &std::path::Path, dev: u64) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && m.dev() == dev)
}

/// Every session scratchpad under the box's home except the one a restart would continue. They
/// are at [`MODEL_SCRATCH`]`/claude-<uid>/<project>/<session>/`.
///
/// The kept one is named by the conversation itself: the newest transcript in the same project
/// under `~/.claude/projects/` is what `--continue` resumes, and its file name is the session id
/// the scratchpad directory is named for. Where no transcript can be read, the newest scratchpad
/// directory is kept instead, so an unreadable conversation never costs the live one its files.
fn ended_scratchpads(box_dir: &Path, home: &Path, dev: u64) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let scratch = home.join(MODEL_SCRATCH);
    if !real_all_the_way(box_dir, &scratch, dev) {
        return out;
    }
    for uid_dir in real_subdirs(&scratch, dev) {
        for project in real_subdirs(&uid_dir, dev) {
            let sessions = real_subdirs(&project, dev);
            let slug = project
                .file_name()
                .map(|n| n.to_owned())
                .unwrap_or_default();
            let keep = newest(
                std::fs::read_dir(home.join(".claude/projects").join(&slug))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "jsonl")),
            )
            .and_then(|p| p.file_stem().map(|s| s.to_owned()))
            .or_else(|| {
                newest(sessions.iter().cloned()).and_then(|p| p.file_name().map(|s| s.to_owned()))
            });
            out.extend(
                sessions
                    .into_iter()
                    .filter(|s| s.file_name().map(|n| n.to_owned()) != keep),
            );
        }
    }
    out
}

fn real_subdirs(dir: &std::path::Path, dev: u64) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_real_dir(p, dev))
        .collect();
    out.sort();
    out
}

fn newest(paths: impl Iterator<Item = std::path::PathBuf>) -> Option<std::path::PathBuf> {
    paths
        .filter_map(|p| {
            let at = std::fs::symlink_metadata(&p)
                .and_then(|m| m.modified())
                .ok()?;
            Some((at, p))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
        .map(|(_, p)| p)
}

/// Cargo target directories (they carry [`CARGO_TARGET_MARKER`]) and `node_modules`, under `dir`,
/// never through a link, never into one already found or into `skip`.
fn build_dirs(
    dir: &std::path::Path,
    dev: u64,
    depth: usize,
    skip: &[std::path::PathBuf],
    found: &mut Vec<std::path::PathBuf>,
) {
    if depth == 0 || !is_real_dir(dir, dev) {
        return;
    }
    for path in real_subdirs(dir, dev) {
        if skip.iter().any(|s| path.starts_with(s)) {
            continue;
        }
        let is_build = path.file_name().is_some_and(|n| n == "node_modules")
            || std::fs::symlink_metadata(path.join(CARGO_TARGET_MARKER)).is_ok_and(|m| m.is_file());
        match is_build {
            true => found.push(path),
            false => build_dirs(&path, dev, depth - 1, skip, found),
        }
    }
}

/// Whether `build` sits inside a git checkout that would keep it: true unless git says the path
/// is ignored there. Outside every checkout this is false — nothing there is anybody's tracked work.
///
/// git runs here at fleet scope over a checkout a box wrote, so it is told to read nothing from
/// that checkout that could run a program: no fsmonitor, no hooks, and only `check-ignore`, which
/// runs no filter, diff driver or credential helper. Any answer but "ignored" keeps the directory.
fn inside_a_checkout_and_kept(box_dir: &std::path::Path, build: &std::path::Path) -> bool {
    let mut at = build.parent();
    while let Some(dir) = at {
        if !dir.starts_with(box_dir) || dir == box_dir {
            return false;
        }
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            let Ok(rel) = build.strip_prefix(dir) else {
                return true;
            };
            let ignored = std::process::Command::new("git")
                .args([
                    "-c",
                    "core.fsmonitor=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "--no-optional-locks",
                    "-C",
                ])
                .arg(dir)
                .args(["check-ignore", "-q", "--"])
                .arg(rel)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            return !ignored;
        }
        at = dir.parent();
    }
    false
}

fn shown(box_dir: &std::path::Path, path: &std::path::Path) -> String {
    let home = box_dir.join("home");
    if let Ok(rel) = path.strip_prefix(&home) {
        return format!("~/{}", rel.display());
    }
    let name = box_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match path.strip_prefix(box_dir) {
        Ok(rel) => format!("{name}/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::path::{Path, PathBuf};

    fn put(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; 16 * 1024]).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-C",
            ])
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    /// A stopped box laid out the way a real one is, with one of each thing that must be offered
    /// and one of each that must not. Returns (fleet root, box dir, pins).
    fn planted(dir: &TempDir) -> (PathBuf, PathBuf, EnvPins) {
        let base: &Path = dir.as_ref();
        let root = base.join("fleet");
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", &root);
        pins.set("SKEIN_HOME", base.join("skein"));
        let b = root.join("web-main");
        let home = b.join("home");
        // offered: a download cache, an ignored build directory in the checkout, a build
        // directory outside every checkout, and the scratchpad of an ended session
        put(&home.join(".cache/ms-playwright/chromium-1/chrome"));
        put(&b.join("tree/target/.rustc_info.json"));
        put(&b.join("tree/target/debug/big"));
        put(&b.join("vartmp/lane/target/.rustc_info.json"));
        put(&home.join(".cache/skein/claude/claude-1000/-proj/old-session/scratchpad/notes"));
        // kept: what the agent put in ~/.cache, the live session's scratchpad and its transcript,
        // a tracked build directory, untracked node_modules, and a build directory behind a link
        put(&home.join(".cache/agent-notes.log"));
        put(&home.join(".cache/skein/claude/claude-1000/-proj/live-session/scratchpad/lane-rules"));
        put(&home.join(".claude/projects/-proj/old-session.jsonl"));
        put(&home.join(".claude/projects/-proj/live-session.jsonl"));
        let now = std::time::SystemTime::now();
        std::fs::File::options()
            .write(true)
            .open(home.join(".claude/projects/-proj/old-session.jsonl"))
            .unwrap()
            .set_modified(now - std::time::Duration::from_secs(86_400))
            .unwrap();
        put(&b.join("tree/vendor-build/.rustc_info.json"));
        put(&b.join("tree/web/node_modules/left-pad/index.js"));
        std::fs::write(b.join("tree/.gitignore"), "target/\n").unwrap();
        git(&b.join("tree"), &["init", "-q"]);
        git(
            &b.join("tree"),
            &["add", ".gitignore", "vendor-build/.rustc_info.json"],
        );
        git(&b.join("tree"), &["commit", "-q", "-m", "fixture"]);
        let store = base.join("store");
        put(&store.join("target/.rustc_info.json"));
        std::os::unix::fs::symlink(&store, home.join("shared")).unwrap();
        (root, b, pins)
    }

    /// **A clean offers only what can be made again, and removes only that** (SKEIN-1341).
    ///
    /// What would make it fail: dropping the git check (the tracked `vendor-build` and the
    /// untracked `node_modules` are then offered), following links (the store's `target` is then
    /// offered), offering all of `~/.cache` (the agent's log), or keeping no scratchpad (the live
    /// session's goes). Watched: with `inside_a_checkout_and_kept` returning false always, the
    /// offered set gained both checkout paths and this failed.
    #[test]
    fn a_stopped_box_offers_only_what_can_be_made_again() {
        let _lock = env_lock();
        let dir = tempdir();
        let (_root, b, _pins) = planted(&dir);
        let offered = box_cleanables("web-main").expect("a box with a directory is answerable");
        let mut shown: Vec<(&str, &str)> =
            offered.iter().map(|c| (c.kind, c.shown.as_str())).collect();
        shown.sort();
        assert_eq!(
            shown,
            vec![
                ("build output", "web-main/tree/target"),
                ("build output", "web-main/vartmp/lane/target"),
                ("download cache", "~/.cache/ms-playwright"),
                (
                    "scratchpad",
                    "~/.cache/skein/claude/claude-1000/-proj/old-session"
                ),
            ],
            "the clean offered the wrong set: {offered:?}"
        );
        assert!(offered.iter().all(|c| c.bytes >= 16 * 1024), "{offered:?}");

        let paths: Vec<String> = offered.iter().map(|c| c.path.clone()).collect();
        let done = clean_box_given("web-main", &paths, Some(false)).expect("a stopped box cleans");
        assert_eq!(done.removed.len(), 4, "{done:?}");
        assert!(done.failed.is_empty(), "{done:?}");
        for gone in &paths {
            assert!(!Path::new(gone).exists(), "{gone} is still there");
        }
        let home = b.join("home");
        for kept in [
            home.join(".cache/agent-notes.log"),
            home.join(".cache/skein/claude/claude-1000/-proj/live-session/scratchpad/lane-rules"),
            home.join(".claude/projects/-proj/old-session.jsonl"),
            b.join("tree/vendor-build/.rustc_info.json"),
            b.join("tree/web/node_modules/left-pad/index.js"),
            home.join("shared/target/.rustc_info.json"),
        ] {
            assert!(kept.exists(), "the clean removed {}", kept.display());
        }
    }

    /// **Nothing reached through a link anywhere in its path is offered or removed.**
    ///
    /// A box owns its home, so it can make `~/.cache` a link to another box's home, or
    /// `~/.cache/skein` a link to somewhere else. Both targets are on the same device.
    ///
    /// What would make it fail: checking only the last component of a candidate, which is what
    /// `symlink_metadata` alone does — it follows every component before the last. Watched: with
    /// `real_all_the_way` reduced to `is_real_dir(path, dev)`, the other box's `pip` and its
    /// scratchpad were offered, and the clean removed them.
    #[test]
    fn nothing_behind_a_link_in_the_path_is_offered_or_removed() {
        let _lock = env_lock();
        let dir = tempdir();
        let base: &Path = dir.as_ref();
        let root = base.join("fleet");
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", &root);
        pins.set("SKEIN_HOME", base.join("skein"));
        let victim = root.join("other-main").join("home").join(".cache");
        put(&victim.join("pip/wheel"));
        put(&victim.join("ms-playwright/chromium-1/chrome"));
        put(&victim.join("skein/claude/claude-1000/-proj/older/scratchpad/notes"));
        put(&victim.join("skein/claude/claude-1000/-proj/newer/scratchpad/notes"));
        let older = victim.join("skein/claude/claude-1000/-proj/older");
        std::fs::File::open(&older)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400))
            .unwrap();

        // `~/.cache` itself is a link to the other box's.
        let whole = root.join("whole-main/home");
        std::fs::create_dir_all(&whole).unwrap();
        std::os::unix::fs::symlink(&victim, whole.join(".cache")).unwrap();
        // `~/.cache` is real, and `~/.cache/skein` is a link to the other box's.
        let part = root.join("part-main").join("home").join(".cache");
        std::fs::create_dir_all(&part).unwrap();
        std::os::unix::fs::symlink(victim.join("skein"), part.join("skein")).unwrap();

        for name in ["whole-main", "part-main"] {
            let offered = box_cleanables(name).expect("a box with a directory is answerable");
            assert!(
                offered.is_empty(),
                "{name} offered what a link in its path leads to: {offered:?}"
            );
        }
        for (name, path) in [
            ("whole-main", whole.join(".cache/pip")),
            ("whole-main", whole.join(".cache/ms-playwright")),
            (
                "part-main",
                part.join("skein/claude/claude-1000/-proj/older"),
            ),
        ] {
            let asked = vec![path.display().to_string()];
            let refused = clean_box_given(name, &asked, Some(false));
            assert!(
                refused.is_err(),
                "{name}: a clean of {path:?} went ahead: {refused:?}"
            );
        }
        for kept in [
            victim.join("pip/wheel"),
            victim.join("ms-playwright/chromium-1/chrome"),
            victim.join("skein/claude/claude-1000/-proj/older/scratchpad/notes"),
        ] {
            assert!(
                kept.exists(),
                "a clean through a link removed {}",
                kept.display()
            );
        }
    }

    /// **A link swapped into the path after the last check does not lead the removal out of
    /// the box.**
    ///
    /// A running box may have its download caches cleaned, and it can swap `~/.cache` for a link
    /// to another box's between the check and the removal. The hook does that swap in exactly
    /// that window.
    ///
    /// What would make it fail: removing by path (`std::fs::remove_dir_all(&c.path)`), which
    /// resolves `~/.cache` again and follows the new link. Watched: with that in place of
    /// `remove_without_following`, the other box's `pip/wheel` was removed and this failed.
    #[test]
    fn a_link_swapped_in_before_the_removal_is_not_followed() {
        let _lock = env_lock();
        let dir = tempdir();
        let (root, b, _pins) = planted(&dir);
        let home = b.join("home");
        put(&home.join(".cache/pip/mine"));
        let other = root.join("other-main").join("home").join(".cache");
        put(&other.join("pip/wheel"));
        let target = home.join(".cache/pip");
        let asked = vec![target.display().to_string()];
        let swap = || {
            std::fs::rename(home.join(".cache"), home.join(".cache-moved")).unwrap();
            std::os::unix::fs::symlink(&other, home.join(".cache")).unwrap();
        };
        let done = clean_box_between("web-main", &asked, Some(true), &swap)
            .expect("a download cache may be cleaned from a running box");
        assert!(
            other.join("pip/wheel").exists(),
            "the removal followed the link into another box: {done:?}"
        );
        assert!(done.removed.is_empty(), "{done:?}");
        assert_eq!(
            done.failed
                .iter()
                .map(|(p, _)| p.clone())
                .collect::<Vec<_>>(),
            asked,
            "the path is not reported under Could not remove: {done:?}"
        );
    }

    /// **Build output and scratchpads are refused from a running box, or one skein cannot see;
    /// download caches are not; and a list that moved removes nothing.**
    ///
    /// What would make it fail: a clean that removes build output or a scratchpad when the box is
    /// running or its state is unknown, one that refuses a download cache from a running box, or
    /// one that removes the paths still on the list when one asked for is not. Watched: with
    /// `needs_stop` forced false, the running box's build output was removed and this failed.
    #[test]
    fn a_clean_refuses_rather_than_removing_what_might_be_in_use() {
        let _lock = env_lock();
        let dir = tempdir();
        let (_root, b, _pins) = planted(&dir);
        let paths: Vec<String> = box_cleanables("web-main")
            .unwrap()
            .into_iter()
            .map(|c| c.path)
            .collect();
        let running = clean_box_given("web-main", &paths, Some(true)).unwrap_err();
        assert!(running.contains("Stop it, then clean again"), "{running}");
        let unknown = clean_box_given("web-main", &paths, None).unwrap_err();
        assert!(unknown.contains("removed nothing"), "{unknown}");
        for alone in [
            b.join("tree/target"),
            b.join("home/.cache/skein/claude/claude-1000/-proj/old-session"),
        ] {
            let one = vec![alone.display().to_string()];
            for state in [Some(true), None] {
                assert!(
                    clean_box_given("web-main", &one, state).is_err(),
                    "{} was removed from a box that is running or unknown ({state:?})",
                    alone.display()
                );
            }
        }
        let mut moved = paths.clone();
        moved.push(b.join("tree/vendor-build").display().to_string());
        let changed = clean_box_given("web-main", &moved, Some(false)).unwrap_err();
        assert!(changed.contains("changed since you looked"), "{changed}");
        for p in &paths {
            assert!(Path::new(p).exists(), "a refused clean removed {p}");
        }
        let cache = b.join("home/.cache/ms-playwright").display().to_string();
        let done = clean_box_given("web-main", std::slice::from_ref(&cache), Some(true))
            .expect("a download cache is cleaned from a running box");
        assert_eq!(done.removed, vec![cache.clone()], "{done:?}");
        assert!(!Path::new(&cache).exists());
        assert!(b.join("tree/target/debug/big").exists());
    }

    /// A fleet with two repos, `web` and `webapp`, each with a managed store and a shared download
    /// cache holding something of every kind, and boxes `web-main`, `web-two` and `webapp-main`.
    /// Returns (skein home, web's cache dir, pins).
    fn repo_caches(dir: &TempDir) -> (PathBuf, PathBuf, EnvPins) {
        let base: &Path = dir.as_ref();
        let home = base.join("skein");
        let root = base.join("fleet");
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", &root);
        pins.set("SKEIN_HOME", &home);
        let mut repos = Vec::new();
        for id in ["web", "webapp"] {
            let store = home.join(format!("repos/{id}/store/data"));
            std::fs::create_dir_all(&store).unwrap();
            repos.push(crate::repos::Repo {
                id: id.into(),
                store: store.display().to_string(),
                ..Default::default()
            });
            let cache = home.join(format!("repos/{id}/store/cache"));
            for rel in [
                "cargo/registry/cache/x.crate",
                "cargo/git/db/x/pack",
                "npm/_cacache/index-v5/x",
                "npm/_npx/0123/node_modules/x/cli.js",
                "pip/http/x",
                "uv/wheels/x",
                "pnpm/v10/files/x",
            ] {
                put(&cache.join(rel));
            }
            // cargo's part is a whole CARGO_HOME, so it can hold what a tool never downloads again
            put(&cache.join("cargo/credentials.toml"));
            put(&cache.join("cargo/config.toml"));
        }
        crate::repos::save_repos(&repos).unwrap();
        for b in ["web-main", "web-two", "webapp-main"] {
            std::fs::create_dir_all(root.join(b).join("home")).unwrap();
        }
        (home.clone(), home.join("repos/web/store/cache"), pins)
    }

    fn live(pairs: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
        pairs.iter().map(|(n, b)| (n.to_string(), *b)).collect()
    }

    /// **A repo's shared download cache is cleaned only with every box of that repo stopped, and
    /// only of what a tool downloads again** (SKEIN-1350).
    ///
    /// What would make it fail: letting a running box through (the first refusal is then `Ok`),
    /// treating "cannot tell" as stopped (the second), counting a box of `webapp` as one of `web`'s
    /// (`starts_with("web")` without the dash; the stopped-`web` clean is then refused for
    /// `webapp-main`), or listing all of `cargo/` (its `credentials.toml` is then removed).
    /// Watched: with the running check removed, the first `expect_err` panicked on `Ok`.
    #[test]
    fn a_repo_cache_is_cleaned_only_with_all_its_boxes_stopped() {
        let _lock = env_lock();
        let dir = tempdir();
        let (_home, cache, _pins) = repo_caches(&dir);
        let all = live(&[
            ("web-main", false),
            ("web-two", false),
            ("webapp-main", true),
        ]);
        let seen = repo_cache_given("web", &all)
            .unwrap()
            .expect("web has a repo cache");
        let mut shown: Vec<&str> = seen.items.iter().map(|c| c.shown.as_str()).collect();
        shown.sort();
        assert_eq!(
            shown,
            vec![
                "cargo/git",
                "cargo/registry",
                "npm/_cacache",
                "npm/_npx",
                "pip",
                "pnpm",
                "uv"
            ]
        );
        assert!(
            seen.running.is_empty() && seen.unknown.is_empty(),
            "{seen:?}"
        );
        let paths: Vec<String> = seen.items.iter().map(|c| c.path.clone()).collect();

        let one_running = live(&[("web-main", false), ("web-two", true)]);
        let err = clean_repo_cache_given("web", &paths, &one_running).expect_err("a box runs");
        assert!(
            err.contains("web-two is running") && err.contains("Stop it"),
            "{err}"
        );
        let untold = live(&[("web-main", false)]);
        let err = clean_repo_cache_given("web", &paths, &untold).expect_err("cannot tell");
        assert!(
            err.contains("cannot tell whether web-two is running"),
            "{err}"
        );
        assert!(cache.join("npm/_npx/0123/node_modules/x/cli.js").exists());

        let done = clean_repo_cache_given("web", &paths, &all).expect("all of web's boxes stopped");
        assert_eq!(done.removed.len(), 7, "{done:?}");
        assert!(done.failed.is_empty(), "{done:?}");
        for gone in &paths {
            assert!(!Path::new(gone).exists(), "{gone} is still there");
        }
        assert!(cache.join("cargo/credentials.toml").exists());
        assert!(cache.join("cargo/config.toml").exists());
        let other = cache
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("webapp/store/cache");
        assert!(
            other.join("npm/_npx/0123/node_modules/x/cli.js").exists(),
            "cleaning web's cache touched webapp's"
        );
    }

    /// **Nothing reached through a link is offered or removed, from a box or from the repo
    /// cache** (SKEIN-1350). A box of the repo can write the repo's cache, so it can put a link
    /// there; and a box owns its home, so it can make `~/.npm` or `~/.cargo/registry` a link into
    /// the repo cache. Neither may lead a clean out of the place it was asked to clean: a box's
    /// clean must never remove the cache every box of the repo reads, and the repo's clean must
    /// never remove what a link in the cache points at.
    ///
    /// What would make it fail: `real_all_the_way` checking only the last component (the box's
    /// `~/.cargo/registry` link is then offered as its own and removed into the repo cache), or
    /// the repo listing skipping that check (the `pip` link's target is then removed). Watched:
    /// with `real_all_the_way` returning true, the box's clean offered both links into the repo
    /// cache and the first `assert` failed.
    #[test]
    fn no_clean_follows_a_link_into_or_out_of_the_repo_cache() {
        use std::os::unix::fs::symlink;
        let _lock = env_lock();
        let dir = tempdir();
        let (_home, cache, _pins) = repo_caches(&dir);
        let base: &Path = dir.as_ref();
        let home = base.join("fleet/web-main/home");
        symlink(cache.join("npm"), home.join(".npm")).unwrap();
        std::fs::create_dir_all(home.join(".cargo")).unwrap();
        symlink(cache.join("cargo/registry"), home.join(".cargo/registry")).unwrap();
        let offered = box_cleanables("web-main").unwrap();
        assert!(
            offered.is_empty(),
            "a box's clean offered a path through a link into the repo cache: {offered:?}"
        );
        let through = vec![
            home.join(".npm/_cacache").display().to_string(),
            home.join(".cargo/registry").display().to_string(),
        ];
        let err = clean_box_given("web-main", &through, Some(false)).expect_err("not on the list");
        assert!(err.contains("removed nothing"), "{err}");
        assert!(cache.join("npm/_cacache/index-v5/x").exists());
        assert!(cache.join("cargo/registry/cache/x.crate").exists());

        // A link inside the repo cache, to something that is nobody's cache.
        let elsewhere = base.join("elsewhere");
        put(&elsewhere.join("keep"));
        std::fs::remove_dir_all(cache.join("pip")).unwrap();
        symlink(&elsewhere, cache.join("pip")).unwrap();
        let all = live(&[("web-main", false), ("web-two", false)]);
        let seen = repo_cache_given("web", &all).unwrap().unwrap();
        assert!(
            seen.items.iter().all(|c| c.shown != "pip"),
            "the repo cache offered a link: {seen:?}"
        );
        let err = clean_repo_cache_given("web", &[cache.join("pip").display().to_string()], &all)
            .expect_err("not on the list");
        assert!(err.contains("removed nothing"), "{err}");
        assert!(elsewhere.join("keep").exists());
    }

    /// A process that takes an exclusive `flock` on the file it is given and holds it itself, as cargo
    /// does. Not `flock(1)` handing its descriptor to a child: `/proc/locks` lists a lock under the pid
    /// that took it, and hides it once that pid is gone, so a lock `flock(1)` took and then left to
    /// `sleep` is held and not listed (measured: `fdinfo` showed it under pid 0).
    const HOLD_A_LOCK: &str = "import fcntl, sys, time\n\
                               f = open(sys.argv[1], 'w')\n\
                               fcntl.flock(f, fcntl.LOCK_EX)\n\
                               time.sleep(60)\n";

    /// A child holding a lock, killed and reaped however the test ends.
    struct Holder(std::process::Child);

    impl Drop for Holder {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// `path` as `/proc/locks` writes it: `fd:01:5678`.
    fn as_proc_locks(path: &Path) -> String {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).unwrap();
        let (major, minor) = dev_pair(m.dev());
        format!("{major:02x}:{minor:02x}:{}", m.ino())
    }

    /// Whether `/proc/locks` lists `file`, waiting up to ten seconds for it to read `want`.
    fn proc_locks_lists(file: &str, want: bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let text = std::fs::read_to_string("/proc/locks").unwrap_or_default();
            let listed = text.split_whitespace().any(|w| w == file);
            if listed == want || std::time::Instant::now() > deadline {
                return listed;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// **A build is live while something holds the `flock` on its profile directory's
    /// `.cargo-lock`, and not live once that ends** (SKEIN-1391), read from the real `/proc/locks`
    /// of a real lock: a child process takes an exclusive `flock` on `target/debug/.cargo-lock` and
    /// holds it, as cargo does ([`HOLD_A_LOCK`]).
    ///
    /// The lock is asserted present in `/proc/locks` before `live_builds` is asked, and absent
    /// after the child is killed before it is asked again: an absence never seen as a presence
    /// proves nothing. A second target directory with an unlocked `.cargo-lock` is never live.
    ///
    /// What would make it fail: looking for the lock in the target directory's root
    /// (`<target>/.cargo-lock`) rather than one level down in each profile directory; or a
    /// `live_builds` that answers from anything but the lock (the second tick would still say live).
    /// Watched: with `p.path().join(CARGO_BUILD_LOCK)` made `Path::new(dir).join(CARGO_BUILD_LOCK)`,
    /// the first assertion on `live_builds` failed.
    #[test]
    fn a_build_is_live_exactly_while_its_cargo_lock_is_held() {
        let dir = tempdir();
        let base: &Path = dir.as_ref();
        let target = base.join("target");
        let idle = base.join("idle-target");
        for t in [&target, &idle] {
            std::fs::create_dir_all(t.join("debug")).unwrap();
            std::fs::write(t.join(CARGO_TARGET_MARKER), "{}").unwrap();
            std::fs::write(t.join("debug").join(CARGO_BUILD_LOCK), "").unwrap();
        }
        let lock = target.join("debug").join(CARGO_BUILD_LOCK);
        let named = as_proc_locks(&lock);
        let dirs = [target.to_str().unwrap(), idle.to_str().unwrap()];
        assert!(
            !proc_locks_lists(&named, false),
            "{named} was locked before the test locked it"
        );

        let child = std::process::Command::new("python3")
            .arg("-c")
            .arg(HOLD_A_LOCK)
            .arg(&lock)
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("python3");
        let mut holder = Holder(child);
        assert!(
            proc_locks_lists(&named, true),
            "the child's flock on {} never appeared in /proc/locks as {named}",
            lock.display()
        );
        let live = live_builds(&dirs);
        assert_eq!(
            live.into_iter().collect::<Vec<_>>(),
            vec![target.display().to_string()],
            "a build holding its cargo lock was not live, or an idle one was"
        );

        let _ = holder.0.kill();
        let _ = holder.0.wait();
        assert!(
            !proc_locks_lists(&named, false),
            "the lock outlived the child that held it"
        );
        assert!(
            live_builds(&dirs).is_empty(),
            "a build whose lock was released is still live"
        );
    }

    /// **A lock is matched by device and inode together**, so a lock on another filesystem's file
    /// with the same inode number is not this build's.
    ///
    /// What would make it fail: matching the inode alone in `live_builds_in` (the line naming
    /// device `ff:ff` would make the build live). Watched: with the device dropped from the match
    /// and from `held_locks`' key, the first assertion failed.
    #[test]
    fn a_lock_on_another_device_with_the_same_inode_is_not_this_build() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempdir();
        let target = Path::new(dir.as_ref() as &Path).join("target");
        std::fs::create_dir_all(target.join("release")).unwrap();
        let lock = target.join("release").join(CARGO_BUILD_LOCK);
        std::fs::write(&lock, "").unwrap();
        let ino = std::fs::metadata(&lock).unwrap().ino();
        let (major, minor) = dev_pair(std::fs::metadata(&lock).unwrap().dev());
        assert_ne!(
            (major, minor),
            (0xff, 0xff),
            "the fixture's own device is ff:ff"
        );
        let dirs = [target.to_str().unwrap()];
        let other = format!("1: FLOCK  ADVISORY  WRITE 4242 ff:ff:{ino} 0 EOF\n");
        assert!(
            live_builds_in(&other, &dirs).is_empty(),
            "a lock on another device's inode {ino} made this build live"
        );
        let this = format!("1: FLOCK  ADVISORY  WRITE 4242 {major:02x}:{minor:02x}:{ino} 0 EOF\n");
        assert_eq!(live_builds_in(&this, &dirs).len(), 1, "{this}");
    }
}
