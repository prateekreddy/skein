//! `skein migrate send`: one new file holding this whole skein. It only reads the fleet.
//!
//! Skein's own state travels by allow-list, so a secret file added later stays behind by default.
//! A box travels whole except for credentials and the named tool caches. Bytes are read directly,
//! so no git hook, filter or config runs; only the opt-in build-output check asks git, sandboxed.

use super::bundle::{
    entry_cost, hex, write_body, write_entry_header, write_frame, Closing, DockerNote, EntryKind,
    LeftOut, Manifest, UnitHead, UnitLine, UnitWriter, FORMAT, KIND_END, KIND_MANIFEST, KIND_UNIT,
    MAGIC,
};
use super::plan::{
    declared_output, docker_names, holds_git_history, ignored_by_own_git, never_moves, plain,
    stays_inside, tool_caches, FLOOR_KB,
};
use super::{available_kb, human, migrating_path, moved_to, schema_of, SCHEMA};
use crate::config::skein_home;
use crate::util::{fleet_root, Sha256};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{BufWriter, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Skein's own files that travel. Anything else in the volume stays, and is named in the report.
const SETTINGS: &[&str] = &[
    "VERSION",
    "config.json",
    "repos.json",
    "connections.json",
    "substrate.json",
    "git-grants.json",
    "usage.json",
    "usage-own.jsonl",
    "usage-digests.json",
    "places",
    "declared",
    "gitgate",
    "asks",
];
/// The parts of `<fleet root>/.skein` that travel.
const FLEET_STATE: &[&str] = &["substrate", "gitgate", "asks"];
/// Git-ignored directories `--leave-out-build-output` may drop.
const BUILD_DIRS: &[&str] = &["target", ".target", "node_modules"];
/// Directories of a git directory whose entries are named by the person: a branch called `tokens`
/// is a ref, and a worktree called `tokens` is work.
const GIT_NAMED: &[&str] = &["refs", "logs", "worktrees", "modules"];
/// Bytes of framing per unit, on top of its tar, as an upper bound for the space check.
const UNIT_OVERHEAD: u64 = 64 * 1024;

/// What `skein migrate send` was asked to do beyond the default.
#[derive(Default)]
pub struct SendOptions {
    /// Leave out `target`, `.target` and `node_modules` where the box's own git ignores them.
    pub leave_out_build_output: bool,
    /// Carry on past files this user cannot read, and name them, instead of refusing.
    pub skip_unreadable: bool,
}

struct Root {
    source: PathBuf,
    tar: Vec<u8>,
}

struct Unit {
    name: String,
    roots: Vec<Root>,
    caches: HashSet<PathBuf>,
    build: HashSet<PathBuf>,
}

impl Unit {
    fn new(name: String) -> Unit {
        Unit {
            name,
            roots: Vec::new(),
            caches: HashSet::new(),
            build: HashSet::new(),
        }
    }
}

fn tar_path(prefix: &str, rest: &OsStr) -> Vec<u8> {
    let mut path = prefix.as_bytes().to_vec();
    path.push(b'/');
    path.extend_from_slice(rest.as_bytes());
    path
}

fn sorted_names(dir: &Path) -> Result<Vec<std::ffi::OsString>, String> {
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("listing {}: {e}", dir.display())),
    };
    let mut names = Vec::new();
    for entry in listing {
        names.push(
            entry
                .map_err(|e| format!("reading {}: {e}", dir.display()))?
                .file_name(),
        );
    }
    names.sort();
    Ok(names)
}

fn real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// The named tool caches of one box, as absolute paths reached through no link.
fn box_caches(box_dir: &Path) -> HashSet<PathBuf> {
    tool_caches()
        .filter(|rel| stays_inside(box_dir, Path::new(rel)))
        .map(|rel| box_dir.join(rel))
        .filter(|path| real_dir(path))
        .collect()
}

/// Git-ignored build directories of one checkout. Asks the box's git inside the sandbox.
fn box_build_output(tree: &Path, left: &mut LeftOut) -> HashSet<PathBuf> {
    let ignored = match ignored_by_own_git(tree) {
        Ok(ignored) => ignored,
        Err(why) => {
            left.build_output.push(format!(
                "(nothing left out of {}: {why})",
                plain(&tree.to_string_lossy())
            ));
            return HashSet::new();
        }
    };
    ignored
        .into_iter()
        .filter(|rel| {
            rel.file_name()
                .is_some_and(|name| BUILD_DIRS.iter().any(|dir| name == OsStr::new(dir)))
        })
        .map(|rel| tree.join(rel))
        .filter(|path| real_dir(path) && !holds_git_history(path))
        .collect()
}

/// Every unit, and what the allow-list left behind.
fn plan_units(options: &SendOptions, left: &mut LeftOut) -> Result<Vec<Unit>, String> {
    let home = skein_home();
    let root = PathBuf::from(fleet_root());
    let mut units = Vec::new();

    let mut settings = Unit::new("settings".to_string());
    for name in sorted_names(&home)? {
        let text = name.to_string_lossy();
        if SETTINGS.contains(&text.as_ref()) {
            settings.roots.push(Root {
                source: home.join(&name),
                tar: tar_path("home", &name),
            });
        } else if !matches!(text.as_ref(), "repos" | "boxes") {
            left.not_on_allow_list
                .push(format!("home/{}", plain(&text)));
        }
    }
    units.push(settings);

    let mut fleet_state = Unit::new("fleet-state".to_string());
    let dot_skein = root.join(".skein");
    for name in sorted_names(&dot_skein)? {
        let text = name.to_string_lossy();
        if FLEET_STATE.contains(&text.as_ref()) {
            fleet_state.roots.push(Root {
                source: dot_skein.join(&name),
                tar: tar_path("fleet/.skein", &name),
            });
        } else {
            left.not_on_allow_list
                .push(format!("fleet/.skein/{}", plain(&text)));
        }
    }
    units.push(fleet_state);

    let mut transcripts = Unit::new("transcripts".to_string());
    let boxes = home.join("boxes");
    for name in sorted_names(&boxes)? {
        let held = boxes.join(&name);
        if !real_dir(&held) {
            left.not_on_allow_list
                .push(format!("home/boxes/{}", plain(&name.to_string_lossy())));
            continue;
        }
        for part in sorted_names(&held)? {
            if part == "claude-projects" {
                let mut tar = tar_path("home/boxes", &name);
                tar.extend_from_slice(b"/claude-projects");
                transcripts.roots.push(Root {
                    source: held.join(&part),
                    tar,
                });
            } else {
                left.not_on_allow_list.push(format!(
                    "home/boxes/{}/{}",
                    plain(&name.to_string_lossy()),
                    plain(&part.to_string_lossy())
                ));
            }
        }
    }
    units.push(transcripts);

    let repos = home.join("repos");
    for name in sorted_names(&repos)? {
        let source = repos.join(&name);
        if !real_dir(&source) {
            left.not_on_allow_list
                .push(format!("home/repos/{}", plain(&name.to_string_lossy())));
            continue;
        }
        let mut unit = Unit::new(format!("repo:{}", plain(&name.to_string_lossy())));
        unit.roots.push(Root {
            source,
            tar: tar_path("home/repos", &name),
        });
        units.push(unit);
    }

    let mut loose = Unit::new("fleet-files".to_string());
    for name in sorted_names(&root)? {
        let text = name.to_string_lossy();
        let path = root.join(&name);
        if text.starts_with('.') {
            if text != ".skein" {
                left.not_on_allow_list
                    .push(format!("fleet/{}", plain(&text)));
            }
        } else if real_dir(&path) {
            let mut unit = Unit::new(format!("box:{}", plain(&text)));
            unit.caches = box_caches(&path);
            if options.leave_out_build_output {
                if real_dir(&path.join("tree")) {
                    unit.build = box_build_output(&path.join("tree"), left);
                }
                unit.build
                    .extend(declared_output(&path).into_iter().map(|rel| path.join(rel)));
            }
            unit.roots.push(Root {
                source: path,
                tar: tar_path("fleet", &name),
            });
            units.push(unit);
        } else if never_moves(&name, &root) {
            left.credentials.push(format!("fleet/{}", plain(&text)));
        } else {
            loose.roots.push(Root {
                source: path,
                tar: tar_path("fleet", &name),
            });
        }
    }
    units.push(loose);
    units.retain(|unit| !unit.roots.is_empty());
    Ok(units)
}

/// Open a file for reading without following a link or moving its access time where that is
/// allowed. `O_NOATIME` needs ownership, so a refusal falls back to a plain open.
fn open_source(path: &Path) -> std::io::Result<fs::File> {
    let open = |flags: i32| {
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | flags)
            .open(path)
    };
    match open(libc::O_NOATIME) {
        Err(e) if e.raw_os_error() == Some(libc::EPERM) => open(0),
        other => other,
    }
}

fn shown(tar: &[u8]) -> String {
    plain(&String::from_utf8_lossy(tar))
}

fn mtime_of(meta: &fs::Metadata) -> u64 {
    u64::try_from(meta.mtime()).unwrap_or(0)
}

/// What the walk hands each thing it decided to carry.
trait Sink {
    fn item(
        &mut self,
        source: &Path,
        tar: &[u8],
        meta: &fs::Metadata,
        left: &mut LeftOut,
    ) -> Result<(), String>;
}

/// Measures a unit and checks every file opens.
#[derive(Default)]
struct Scan {
    files: u64,
    bytes: u64,
    seen: HashSet<(u64, u64)>,
}

impl Sink for Scan {
    fn item(
        &mut self,
        source: &Path,
        tar: &[u8],
        meta: &fs::Metadata,
        left: &mut LeftOut,
    ) -> Result<(), String> {
        let kind = meta.file_type();
        let (link_len, size) = if kind.is_symlink() {
            let target = fs::read_link(source)
                .map_err(|e| format!("reading the link {}: {e}", source.display()))?;
            (target.as_os_str().len(), 0)
        } else if kind.is_file() {
            if open_source(source).is_err() {
                left.unreadable.push(shown(tar));
                return Ok(());
            }
            if meta.nlink() > 1 && !self.seen.insert((meta.dev(), meta.ino())) {
                (tar.len(), 0)
            } else {
                (0, meta.len())
            }
        } else {
            (0, 0)
        };
        self.files += 1;
        self.bytes += entry_cost(tar.len(), link_len, size);
        Ok(())
    }
}

/// Writes a unit's entries.
struct Emit<'a, 'w, W: Write> {
    out: &'a mut UnitWriter<'w, W>,
    entries: u64,
    links: HashMap<(u64, u64), Vec<u8>>,
}

impl<W: Write> Sink for Emit<'_, '_, W> {
    fn item(
        &mut self,
        source: &Path,
        tar: &[u8],
        meta: &fs::Metadata,
        left: &mut LeftOut,
    ) -> Result<(), String> {
        let failed = |e: std::io::Error| format!("writing {}: {e}", shown(tar));
        let kind = meta.file_type();
        let mode = meta.mode() & 0o7777;
        let mtime = mtime_of(meta);
        if kind.is_dir() {
            write_entry_header(self.out, tar, &EntryKind::Dir, mode, mtime).map_err(failed)?;
        } else if kind.is_symlink() {
            let target = fs::read_link(source)
                .map_err(|e| format!("reading the link {}: {e}", source.display()))?;
            let target = target.as_os_str().as_bytes();
            write_entry_header(self.out, tar, &EntryKind::Symlink(target), mode, mtime)
                .map_err(failed)?;
        } else {
            let key = (meta.dev(), meta.ino());
            if meta.nlink() > 1 {
                if let Some(first) = self.links.get(&key) {
                    let first = first.clone();
                    write_entry_header(self.out, tar, &EntryKind::HardLink(&first), mode, mtime)
                        .map_err(failed)?;
                    self.entries += 1;
                    return Ok(());
                }
            }
            let Ok(mut file) = open_source(source) else {
                left.unreadable.push(shown(tar));
                return Ok(());
            };
            let size = meta.len();
            write_entry_header(self.out, tar, &EntryKind::File(size), mode, mtime)
                .map_err(failed)?;
            let changed = write_body(self.out, &mut file, size).map_err(failed)?;
            if changed {
                left.changed_while_reading.push(shown(tar));
            }
            if meta.nlink() > 1 {
                self.links.insert(key, tar.to_vec());
            }
        }
        self.entries += 1;
        Ok(())
    }
}

/// Whether a git directory is what `dir` is, by the three things git itself requires.
fn is_git_dir(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join("HEAD")).is_ok_and(|m| m.is_file())
        && real_dir(&dir.join("objects"))
        && real_dir(&dir.join("refs"))
}

/// Whether a git config holds a password or token inside a URL.
fn url_holds_credential(config: &[u8]) -> bool {
    let text = String::from_utf8_lossy(config);
    text.split("://").skip(1).any(|rest| {
        let authority = rest
            .split(|c: char| c == '/' || c.is_whitespace())
            .next()
            .unwrap_or("");
        authority
            .rsplit_once('@')
            .is_some_and(|(user, _)| user.contains(':'))
    })
}

struct Walk<'a> {
    unit: &'a Unit,
    left: &'a mut LeftOut,
    url_credentials: Option<&'a mut Vec<String>>,
    /// Files left out as credentials, so a hard link under another name stays behind too.
    secrets: &'a mut HashSet<(u64, u64)>,
    /// True on the copy, once `secrets` is complete.
    secrets_known: bool,
}

impl Walk<'_> {
    fn go(
        &mut self,
        sink: &mut dyn Sink,
        source: &Path,
        tar: &[u8],
        device: u64,
        names_are_git: bool,
    ) -> Result<(), String> {
        let meta = match fs::symlink_metadata(source) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.left.changed_while_reading.push(shown(tar));
                return Ok(());
            }
            Err(_) => {
                self.left.unreadable.push(shown(tar));
                return Ok(());
            }
        };
        let kind = meta.file_type();
        if !(kind.is_dir() || kind.is_file() || kind.is_symlink()) {
            self.left.sockets_and_devices += 1;
            return Ok(());
        }
        sink.item(source, tar, &meta, self.left)?;
        if !kind.is_dir() {
            return Ok(());
        }
        let Ok(names) = sorted_names(source) else {
            self.left.unreadable.push(shown(tar));
            return Ok(());
        };
        let git_dir = is_git_dir(source);
        if git_dir {
            if let Some(found) = &mut self.url_credentials {
                let config = source.join("config");
                let small =
                    fs::symlink_metadata(&config).is_ok_and(|m| m.is_file() && m.len() < 1 << 20);
                if small && fs::read(&config).is_ok_and(|text| url_holds_credential(&text)) {
                    found.push(format!("{}/config", shown(tar)));
                }
            }
        }
        for name in names {
            let child = source.join(&name);
            let mut child_tar = tar.to_vec();
            child_tar.push(b'/');
            child_tar.extend_from_slice(name.as_bytes());
            let named_by_git =
                names_are_git || (git_dir && GIT_NAMED.iter().any(|dir| name == OsStr::new(dir)));
            let child_meta = fs::symlink_metadata(&child).ok();
            if !named_by_git && never_moves(&name, source) {
                self.left.credentials.push(shown(&child_tar));
                if let Some(meta) = child_meta.as_ref().filter(|m| m.is_file()) {
                    self.secrets.insert((meta.dev(), meta.ino()));
                }
                continue;
            }
            if self.secrets_known
                && child_meta.as_ref().is_some_and(|m| {
                    m.is_file() && m.nlink() > 1 && self.secrets.contains(&(m.dev(), m.ino()))
                })
            {
                self.left.credentials.push(shown(&child_tar));
                continue;
            }
            if self.unit.caches.contains(&child) {
                self.left.tool_caches.push(shown(&child_tar));
                continue;
            }
            if self.unit.build.contains(&child) {
                self.left.build_output.push(shown(&child_tar));
                continue;
            }
            if child_meta.is_some_and(|m| m.is_dir() && m.dev() != device) {
                self.left.other_filesystem.push(shown(&child_tar));
                continue;
            }
            self.go(sink, &child, &child_tar, device, named_by_git)?;
        }
        Ok(())
    }

    fn unit(&mut self, sink: &mut dyn Sink) -> Result<(), String> {
        for root in &self.unit.roots {
            let Ok(meta) = fs::symlink_metadata(&root.source) else {
                continue;
            };
            self.go(sink, &root.source, &root.tar, meta.dev(), false)?;
        }
        Ok(())
    }
}

/// Refuse unless the bundle fits. The floor applies only on the disk the fleet itself uses.
fn room(need_kb: u64, free_kb: Option<u64>, shares_fleet_disk: bool) -> Result<String, String> {
    let floor = if shares_fleet_disk { FLOOR_KB } else { 0 };
    let Some(free) = free_kb else {
        return Err(format!(
            "needs {} but the free space at the destination could not be read. Nothing was \
             written.",
            human(need_kb)
        ));
    };
    let said = if floor > 0 {
        format!(
            "Space: needs {}, {} free, and {} must stay free because this disk also holds the \
             fleet.",
            human(need_kb),
            human(free),
            human(floor)
        )
    } else {
        format!("Space: needs {}, {} free.", human(need_kb), human(free))
    };
    if need_kb + floor > free {
        return Err(format!("{said} It does not fit. Nothing was written."));
    }
    Ok(said)
}

/// `path` as an absolute path with whatever of it exists resolved.
fn resolved(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    if let Ok(real) = absolute.canonicalize() {
        return real;
    }
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf())
            .join(name),
        _ => absolute,
    }
}

fn check_destination(file: &Path) -> Result<PathBuf, String> {
    if fs::symlink_metadata(file).is_ok() {
        return Err(format!(
            "{} already exists, and a backup never overwrites a file. Name a new one.",
            file.display()
        ));
    }
    let target = resolved(file);
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", file.display()))?;
    if !parent.is_dir() {
        return Err(format!("{} is not a directory", parent.display()));
    }
    for source in [skein_home(), PathBuf::from(fleet_root())] {
        let source = resolved(&source);
        if target.starts_with(&source) {
            return Err(format!(
                "{} is inside {}, which this backup reads. Name a destination outside it.",
                target.display(),
                source.display()
            ));
        }
    }
    Ok(target)
}

fn shares_fleet_disk(parent: &Path) -> bool {
    let Ok(here) = fs::metadata(parent) else {
        return true;
    };
    [skein_home(), PathBuf::from(fleet_root())]
        .iter()
        .any(|source| fs::metadata(source).is_ok_and(|m| m.dev() == here.dev()))
}

fn docker_note() -> DockerNote {
    match docker_names() {
        Ok((volumes, images)) => DockerNote {
            included: false,
            answered: true,
            why_unknown: String::new(),
            volumes,
            images,
        },
        Err(why) => DockerNote {
            included: false,
            answered: false,
            why_unknown: plain(&why),
            volumes: Vec::new(),
            images: Vec::new(),
        },
    }
}

/// The plain-language lines about Docker, shared with `receive`.
pub(super) fn docker_text(docker: &DockerNote) -> String {
    if !docker.answered {
        return format!(
            "Docker was NOT backed up, and Docker did not answer ({}), so which volumes exist is \
             unknown.",
            docker.why_unknown
        );
    }
    let mut text = String::from("Docker was NOT backed up.");
    if docker.volumes.is_empty() {
        text.push_str(" Docker reported no volumes.");
    } else {
        text.push_str(&format!(
            " These volumes are not in this file: {}.",
            docker.volumes.join(", ")
        ));
    }
    if !docker.images.is_empty() {
        text.push_str(&format!(
            " Nor are these locally built images: {}.",
            docker.images.join(", ")
        ));
    }
    text
}

fn listed(items: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut text = items
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > SHOWN {
        text.push_str(&format!(", and {} more", items.len() - SHOWN));
    }
    text
}

/// The plain-language lines about what stayed behind, shared with `receive`.
pub(super) fn left_out_text(left: &LeftOut) -> String {
    let mut text = String::new();
    let mut line = |label: &str, items: &[String]| {
        if !items.is_empty() {
            text.push_str(&format!("  {label} ({}): {}\n", items.len(), listed(items)));
        }
    };
    line("Credentials, which never travel", &left.credentials);
    line("Tool caches, which are rebuilt", &left.tool_caches);
    line("Build output, as you asked", &left.build_output);
    line(
        "Not on the list of skein state that travels",
        &left.not_on_allow_list,
    );
    line("UNREADABLE, so NOT backed up", &left.unreadable);
    line(
        "On another filesystem, so NOT backed up",
        &left.other_filesystem,
    );
    line(
        "Changed while being read, so these copies may be inconsistent",
        &left.changed_while_reading,
    );
    if left.sockets_and_devices > 0 {
        text.push_str(&format!(
            "  Sockets and devices, which cannot be copied: {}\n",
            left.sockets_and_devices
        ));
    }
    text
}

fn created_stamp() -> String {
    let mut seed = Sha256::new();
    let now = chrono::Utc::now();
    seed.update(now.to_rfc3339().as_bytes());
    seed.update(&std::process::id().to_be_bytes());
    let tag = hex(&seed.finish()[..4]);
    format!("{} {tag}", now.to_rfc3339())
}

/// Back the whole skein up to `file`, which must not exist yet.
pub fn migrate_send(file: &str, options: &SendOptions) -> Result<String, String> {
    if let Some(moved) = moved_to() {
        return Err(format!(
            "this installation was moved to {moved}, so what is here is the old copy. Set \
             SKEIN_HOME to it and run this again. Nothing was written."
        ));
    }
    if migrating_path(&skein_home()).exists() {
        return Err(format!(
            "{} holds a half-finished move, so it is not a whole installation. Nothing was \
             written.",
            skein_home().display()
        ));
    }
    let target = check_destination(Path::new(file))?;
    let parent = target.parent().unwrap_or(Path::new("/")).to_path_buf();

    let mut planned = LeftOut::default();
    let units = plan_units(options, &mut planned)?;
    if units.is_empty() {
        return Err(format!(
            "found nothing to back up in {} or {}. Nothing was written.",
            skein_home().display(),
            fleet_root()
        ));
    }
    let mut scanned = planned.clone();
    let mut secrets = HashSet::new();
    let mut lines = Vec::new();
    let mut url_credentials = Vec::new();
    let mut need = 1024 * 1024u64;
    for unit in &units {
        let mut scan = Scan::default();
        Walk {
            unit,
            left: &mut scanned,
            url_credentials: Some(&mut url_credentials),
            secrets: &mut secrets,
            secrets_known: false,
        }
        .unit(&mut scan)?;
        need += scan.bytes + UNIT_OVERHEAD;
        lines.push(UnitLine {
            name: unit.name.clone(),
            files: scan.files,
            bytes: scan.bytes,
            sha256: String::new(),
        });
    }
    if !url_credentials.is_empty() {
        return Err(format!(
            "these git configs hold a password or token inside a remote URL, and a backup never \
             carries a credential: {}. Remove it from the URL, then run this again. Nothing was \
             written.",
            listed(&url_credentials)
        ));
    }
    if !scanned.unreadable.is_empty() && !options.skip_unreadable {
        return Err(format!(
            "{} item(s) cannot be read by this user: {}. Run this as root, or pass \
             --skip-unreadable to leave them out and have them named. Nothing was written.",
            scanned.unreadable.len(),
            listed(&scanned.unreadable)
        ));
    }
    let space = room(
        need.div_ceil(1024),
        available_kb(&parent),
        shares_fleet_disk(&parent),
    )?;

    let docker = docker_note();
    let manifest = Manifest {
        format: FORMAT,
        volume_schema: schema_of(&skein_home()).unwrap_or(SCHEMA),
        skein_commit: env!("SKEIN_BUILD_REVISION").to_string(),
        arch: std::env::consts::ARCH.to_string(),
        os: std::env::consts::OS.to_string(),
        created: created_stamp(),
        encrypted: false,
        units: lines,
        docker: docker.clone(),
    };

    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&target)
        .map_err(|e| format!("creating {}: {e}", target.display()))?;
    let mut out = BufWriter::with_capacity(1 << 20, created);
    let cut = |e: std::io::Error| {
        format!(
            "writing {} failed ({e}). What was written is valid up to its last complete unit, and \
             the fleet was not changed.",
            target.display()
        )
    };
    out.write_all(MAGIC).map_err(cut)?;
    out.write_all(&FORMAT.to_be_bytes()).map_err(cut)?;
    let payload = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    write_frame(&mut out, KIND_MANIFEST, &payload).map_err(cut)?;

    let mut left = planned;
    let mut finished = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        let head = UnitHead {
            index: index as u64,
            name: unit.name.clone(),
        };
        let payload = serde_json::to_vec(&head).map_err(|e| e.to_string())?;
        write_frame(&mut out, KIND_UNIT, &payload).map_err(cut)?;
        let mut writer = UnitWriter::new(&mut out);
        let mut emit = Emit {
            out: &mut writer,
            entries: 0,
            links: HashMap::new(),
        };
        Walk {
            unit,
            left: &mut left,
            url_credentials: None,
            secrets: &mut secrets,
            secrets_known: true,
        }
        .unit(&mut emit)
        .map_err(|why| {
            format!(
                "{why}. {} is valid up to its last complete unit, and the fleet was not changed.",
                target.display()
            )
        })?;
        let entries = emit.entries;
        let trailer = writer.finish(entries).map_err(cut)?;
        finished.push(UnitLine {
            name: unit.name.clone(),
            files: trailer.files,
            bytes: trailer.bytes,
            sha256: trailer.sha256,
        });
    }
    let closing = Closing {
        units: finished,
        left_out: left.clone(),
    };
    let payload = serde_json::to_vec(&closing).map_err(|e| e.to_string())?;
    write_frame(&mut out, KIND_END, &payload).map_err(cut)?;
    let file = out.into_inner().map_err(|e| cut(e.into_error()))?;
    file.sync_all().map_err(cut)?;

    let written = file.metadata().map_or(0, |m| m.len());
    let entries: u64 = closing.units.iter().map(|u| u.files).sum();
    Ok(format!(
        "Backed up to {}\n{space}\nWritten: {}, {} unit(s), {entries} entries, each unit checked \
         by SHA-256.\n\nIncluded: every box directory (checkouts, git history, stashes, worktrees, \
         uncommitted and ignored files, homes), conversations, repository stores and mirrors, and \
         skein's settings and queues.\n\nNOT included:\n  {}\n{}\nThis file is NOT encrypted. \
         Conversations and files such as .env can hold secrets, so keep it somewhere private.\n\
         The fleet was only read. Restore with: skein migrate receive {} --into <empty-dir>",
        target.display(),
        human(written.div_ceil(1024)),
        closing.units.len(),
        docker_text(&docker),
        left_out_text(&left),
        target.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_that_does_not_fit_is_refused_and_says_both_numbers() {
        assert!(room(100, Some(100), false).is_ok());
        // Fails if the comparison stops counting what is needed.
        let refused = room(101, Some(100), false).unwrap_err();
        assert!(refused.contains("needs 101 KB, 100 KB free"), "{refused}");
        // Fails if the floor is dropped on the disk the fleet runs from.
        assert!(room(100, Some(FLOOR_KB + 99), true).is_err());
        assert!(room(100, Some(FLOOR_KB + 100), true).is_ok());
        // Fails if an unreadable free figure is read as room.
        assert!(room(1, None, false).is_err());
    }

    #[test]
    fn only_a_password_in_a_url_counts_as_a_url_credential() {
        assert!(url_holds_credential(
            b"[remote \"o\"]\n\turl = https://x-access-token:abc@github.example/r.git\n"
        ));
        // Fails if the check matches any `@` or any `:` in a URL.
        assert!(!url_holds_credential(
            b"url = ssh://git@host.example:22/r.git\n"
        ));
        assert!(!url_holds_credential(
            b"url = https://host.example:8443/r.git\n"
        ));
    }
}
