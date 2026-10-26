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
}
