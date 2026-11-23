//! `skein migrate plan`: what moving this whole skein would involve. It only reads.
//!
//! Sizes come from directory listings and `lstat`, so no file is opened to measure it. A size that
//! could not be read is a range with an open end, never a zero.

use super::{available_kb, human};
use crate::config::skein_home;
use crate::fleet::{checkouts_on_disk, place_records, DOWNLOAD_CACHES};
use crate::util::fleet_root;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Room a move must leave free on the disk it lands on.
const FLOOR_KB: u64 = 10 * 1024 * 1024;
const DOCKER_TIMEOUT: Duration = Duration::from_secs(60);
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Where the sandboxed git binary is mounted.
const GIT_IN_SANDBOX: &str = "/.plan-git";
/// Tool caches outside `home/.cache`, relative to a box directory. Those inside it come from
/// [`DOWNLOAD_CACHES`], because the rest of `home/.cache` can hold an agent's own files.
const TOOL_CACHES: &[&str] = &[
    "home/.rustup",
    "home/.cargo/registry",
    "home/.npm",
    "home/.pnpm-store",
    ".pnpm-store",
    "pnpm-store",
];

/// A size in KiB: exact when `high == Some(low)`, and `high: None` when nothing bounds it above.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Size {
    low: u64,
    high: Option<u64>,
}

impl Size {
    const ZERO: Size = Size::known(0);

    const fn known(kb: u64) -> Size {
        Size {
            low: kb,
            high: Some(kb),
        }
    }

    const fn unknown() -> Size {
        Size { low: 0, high: None }
    }

    fn plus(self, other: Size) -> Size {
        Size {
            low: self.low + other.low,
            high: self.high.zip(other.high).map(|(a, b)| a + b),
        }
    }

    /// What is left of `self` once `part`, which lies inside it, is taken out.
    fn less(self, part: Size) -> Size {
        Size {
            low: part.high.map_or(0, |high| self.low.saturating_sub(high)),
            high: self.high.map(|high| high.saturating_sub(part.low)),
        }
    }

    fn show(self) -> String {
        match (self.low, self.high) {
            (low, Some(high)) if low == high => human(low),
            (low, Some(high)) if human(low) == human(high) => format!("about {}", human(low)),
            (0, None) => "unknown".to_string(),
            (low, None) => format!("unknown (at least {})", human(low)),
            (0, Some(high)) => format!("unknown (at most {})", human(high)),
            (low, Some(high)) => format!("unknown (between {} and {})", human(low), human(high)),
        }
    }
}

/// Whether `need` plus the floor fits in `free`, in words.
fn verdict(need: Size, free: Option<u64>) -> &'static str {
    let Some(free) = free else {
        return "cannot say, because the free space could not be read";
    };
    if need.low + FLOOR_KB > free {
        "does not fit"
    } else if need.high.is_some_and(|high| high + FLOOR_KB <= free) {
        "fits"
    } else {
        "cannot say, because some sizes are unknown"
    }
}

/// A name the walk neither enters nor measures.
fn holds_credentials(name: &OsStr, parent: &Path) -> bool {
    let name = name.to_string_lossy();
    matches!(name.as_ref(), "github-pats" | "git-tokens" | "tokens")
        || name.starts_with(".credentials")
        || name.ends_with(".pem")
        || (name == "private" && parent.file_name() == Some(OsStr::new(".skein")))
}

#[derive(Default)]
struct Walked {
    bytes: u64,
    complete: bool,
    holds_git: bool,
    unopened: u64,
}

impl Walked {
    fn size(&self) -> Size {
        let kb = self.bytes.div_ceil(1024);
        Size {
            low: kb,
            high: self.complete.then_some(kb),
        }
    }

    fn count(&mut self, meta: &fs::Metadata, links: &mut Links) {
        let bytes = meta.blocks() * 512;
        if meta.nlink() < 2 || meta.is_dir() || links.first_sight(meta, bytes) {
            self.bytes += bytes;
        }
    }
}

/// Hard-linked files met so far, so each counts once and a part can tell which of its files are
/// also linked from somewhere it did not walk.
#[derive(Default)]
struct Links {
    files: HashMap<(u64, u64), LinkedFile>,
    part: usize,
}

struct LinkedFile {
    found: u64,
    nlink: u64,
    bytes: u64,
    part: usize,
}

impl Links {
    fn first_sight(&mut self, meta: &fs::Metadata, bytes: u64) -> bool {
        let part = self.part;
        let mut first = false;
        self.files
            .entry((meta.dev(), meta.ino()))
            .and_modify(|file| file.found += 1)
            .or_insert_with(|| {
                first = true;
                LinkedFile {
                    found: 1,
                    nlink: meta.nlink(),
                    bytes,
                    part,
                }
            });
        first
    }

    /// KiB counted in `part` that a link outside everything walked still holds.
    fn held_elsewhere(&self, part: usize) -> Size {
        let bytes: u64 = self
            .files
            .values()
            .filter(|file| file.part == part && file.found < file.nlink)
            .map(|file| file.bytes)
            .sum();
        Size::known(bytes.div_ceil(1024))
    }
}

/// Measure `start` without following a link or leaving its filesystem. Anything it could not read
/// or would have had to cross into leaves the result incomplete.
fn walk(start: &Path, links: &mut Links) -> Walked {
    let mut walked = Walked {
        complete: true,
        ..Walked::default()
    };
    if let (Some(name), Some(parent)) = (start.file_name(), start.parent()) {
        if holds_credentials(name, parent) {
            walked.unopened = 1;
            return walked;
        }
    }
    let Ok(meta) = fs::symlink_metadata(start) else {
        walked.complete = false;
        return walked;
    };
    walked.count(&meta, links);
    let device = meta.dev();
    let mut dirs = if meta.is_dir() {
        vec![start.to_path_buf()]
    } else {
        Vec::new()
    };
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            walked.complete = false;
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                walked.complete = false;
                continue;
            };
            let name = entry.file_name();
            walked.holds_git |= name == ".git";
            if holds_credentials(&name, &dir) {
                walked.unopened += 1;
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                walked.complete = false;
                continue;
            };
            if meta.is_dir() {
                if meta.dev() != device {
                    walked.complete = false;
                    continue;
                }
                dirs.push(entry.path());
            }
            walked.count(&meta, links);
        }
    }
    walked
}

/// The git binary to mount: the first on `PATH`, or the one a box's wrapper script names.
fn real_git() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let found = std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())?;
    let mut head = [0u8; 512];
    let read = fs::File::open(&found).ok()?.read(&mut head).ok()?;
    if !head[..read].starts_with(b"#!") {
        return found.canonicalize().ok();
    }
    String::from_utf8_lossy(&head[..read])
        .lines()
        .find_map(|line| line.strip_prefix("skein_git="))
        .map(|named| PathBuf::from(named.trim()))
        .filter(|named| named.is_file())
}

/// The arguments that run `git ls-files` in a namespace holding only the system directories and
/// this tree, read-only, with no network.
///
/// A `-c` cannot cover a filter whose name the box chose, so the boundary is what holds. The `-c`
/// and environment settings keep the box's config from redirecting what is asked.
fn sandboxed_git_args(git: &Path, tree: &Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--setenv",
        "HOME",
        "/nonexistent",
        "--setenv",
        "GIT_CONFIG_NOSYSTEM",
        "1",
        "--setenv",
        "GIT_CONFIG_GLOBAL",
        "/dev/null",
        "--setenv",
        "GIT_OPTIONAL_LOCKS",
        "0",
        "--setenv",
        "GIT_NO_LAZY_FETCH",
        "1",
        "--ro-bind",
        "/usr",
        "/usr",
        "--dev",
        "/dev",
    ]
    .iter()
    .map(Into::into)
    .collect();
    for top in ["/bin", "/sbin", "/lib", "/lib32", "/lib64"] {
        let Ok(meta) = fs::symlink_metadata(top) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            if let Ok(target) = fs::read_link(top) {
                args.extend(["--symlink".into(), target.into(), top.into()]);
            }
        } else {
            args.extend(["--ro-bind".into(), top.into(), top.into()]);
        }
    }
    args.extend(["--ro-bind".into(), git.into(), GIT_IN_SANDBOX.into()]);
    args.extend(["--ro-bind".into(), tree.into(), tree.into()]);
    args.extend(["--setenv".into(), "GIT_WORK_TREE".into(), tree.into()]);
    args.extend(["--chdir".into(), tree.into(), "--".into()]);
    args.extend(
        [
            GIT_IN_SANDBOX,
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "safe.directory=*",
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ]
        .iter()
        .map(std::ffi::OsString::from),
    );
    args
}

/// What this tree's own git reports as ignored, as paths relative to the tree.
fn ignored_by_own_git(tree: &Path) -> Result<Vec<PathBuf>, String> {
    if !tree.is_absolute() || fs::symlink_metadata(tree).map_or(true, |m| !m.is_dir()) {
        return Err("its tree is not a plain directory".into());
    }
    if fs::symlink_metadata(tree.join(".git")).is_err() {
        return Err("its tree is not a git repository".into());
    }
    let git = real_git().ok_or("no git binary was found")?;
    let mut command = Command::new("bwrap");
    command
        .args(sandboxed_git_args(&git, tree))
        .stdin(Stdio::null());
    let out = crate::util::bounded_output(&mut command, "git", GIT_TIMEOUT)?;
    if !out.status.success() {
        return Err("its git did not answer".into());
    }
    let mut paths = Vec::new();
    for raw in out
        .stdout
        .split(|byte| *byte == 0)
        .filter(|r| !r.is_empty())
    {
        let text = std::str::from_utf8(raw).map_err(|_| "its git gave an unreadable path")?;
        let path = PathBuf::from(text.trim_end_matches('/'));
        if !stays_inside(tree, &path) {
            return Err("its git named a path outside its tree".into());
        }
        paths.push(path);
    }
    Ok(paths)
}

/// Whether `relative` names something under `tree` with no link on the way to it.
fn stays_inside(tree: &Path, relative: &Path) -> bool {
    let mut at = tree.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            return false;
        };
        at.push(name);
        if parts.peek().is_some()
            && fs::symlink_metadata(&at).map_or(true, |m| m.file_type().is_symlink())
        {
            return false;
        }
    }
    relative.components().next().is_some()
}

/// Every tool-cache path relative to a box directory.
fn tool_caches() -> impl Iterator<Item = String> {
    DOWNLOAD_CACHES
        .iter()
        .filter(|rel| rel.starts_with(".cache/"))
        .map(|rel| format!("home/{rel}"))
        .chain(TOOL_CACHES.iter().map(|rel| rel.to_string()))
}

/// What of a box can be made again.
struct Rebuild {
    ignored: Size,
    caches: Size,
    note: Option<String>,
}

/// The part of a box its own git ignores. A folder holding its own history is carried.
fn ignored_part(tree: &Path, links: &mut Links) -> (Size, Option<String>) {
    let ignored = match ignored_by_own_git(tree) {
        Ok(ignored) => ignored,
        Err(why) => {
            let inside = walk(tree, &mut Links::default()).size();
            let unknown = Size {
                low: 0,
                high: inside.high,
            };
            return (
                unknown,
                Some(format!("{why}, so how much of it is ignored is unknown")),
            );
        }
    };
    let mut sum = Size::ZERO;
    let mut with_history = 0;
    for path in ignored {
        let path = tree.join(path);
        if walk(&path, &mut Links::default()).holds_git {
            with_history += 1;
        } else {
            sum = sum.plus(walk(&path, links).size());
        }
    }
    let note = (with_history > 0).then(|| {
        format!("{with_history} ignored folder(s) hold their own git history, so they are carried")
    });
    (sum, note)
}

/// How much of a box directory is rebuildable. Only a checkout has git-ignored output.
fn rebuildable(box_dir: &Path, has_tree: bool) -> Rebuild {
    let mut links = Links::default();
    let (ignored, note) = if has_tree {
        ignored_part(&box_dir.join("tree"), &mut links)
    } else {
        (Size::ZERO, None)
    };
    links.part = 1;
    let mut caches = Size::ZERO;
    for rel in tool_caches() {
        let path = box_dir.join(&rel);
        if stays_inside(box_dir, Path::new(&rel)) && fs::symlink_metadata(&path).is_ok() {
            caches = caches.plus(walk(&path, &mut links).size());
        }
    }
    Rebuild {
        ignored: ignored.less(links.held_elsewhere(0)),
        caches: caches.less(links.held_elsewhere(1)),
        note,
    }
}

/// What stays carried in `home/.cache`: all of it but the tool caches, and skein's part of that.
fn cache_carried(box_dir: &Path) -> (Size, Size) {
    if !stays_inside(box_dir, Path::new("home/.cache/skein")) {
        return (Size::ZERO, Size::ZERO);
    }
    let cache = box_dir.join("home/.cache");
    if fs::symlink_metadata(&cache).map_or(true, |m| !m.is_dir()) {
        return (Size::ZERO, Size::ZERO);
    }
    let whole = walk(&cache, &mut Links::default()).size();
    let mut links = Links::default();
    let mut tools = Size::ZERO;
    for rel in tool_caches().filter(|rel| rel.starts_with("home/.cache/")) {
        let path = box_dir.join(&rel);
        if stays_inside(box_dir, Path::new(&rel)) && fs::symlink_metadata(&path).is_ok() {
            tools = tools.plus(walk(&path, &mut links).size());
        }
    }
    let skein = cache.join("skein");
    let own = if fs::symlink_metadata(&skein).is_ok() {
        walk(&skein, &mut Links::default()).size()
    } else {
        Size::ZERO
    };
    (whole.less(tools), own)
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Box,
    CheckoutNoRecord,
    RecordNoCheckout,
    NoTree,
    NotADirectory,
}

struct Entry {
    name: String,
    shape: Shape,
    carry: Size,
    ignored: Size,
    caches: Size,
    cache_carried: Size,
    skein_cache: Size,
    note: Option<String>,
    on_disk: bool,
}

/// One line of terminal text from a name something else chose.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

#[derive(Default)]
struct Docker {
    named: Vec<(String, Size, Vec<String>)>,
    anonymous: Vec<(String, Size, Vec<String>)>,
    images: Vec<(String, Size)>,
    untagged: usize,
}

fn docker(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("docker");
    command.args(args).stdin(Stdio::null());
    let out = crate::util::bounded_output(&mut command, "docker", DOCKER_TIMEOUT)?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err(plain(
            said.lines().next().unwrap_or("it exited with an error"),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Docker's decimal size text ("1.5GB", "0B") in KiB.
fn docker_kb(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let (number, unit) = text.split_at(split);
    let scale = match unit {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((number.parse::<f64>().ok()? * scale / 1024.0).ceil() as u64)
}

fn is_anonymous(volume: &str) -> bool {
    volume.len() == 64
        && volume
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Which containers mount each volume. Empty when Docker would not say.
fn volume_holders() -> BTreeMap<String, Vec<String>> {
    let mut holders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(ids) = docker(&["ps", "-aq"]) else {
        return holders;
    };
    let ids: Vec<&str> = ids.split_whitespace().collect();
    if ids.is_empty() {
        return holders;
    }
    let format = "{{.Name}}{{range .Mounts}}{{\"\\t\"}}{{.Type}}={{.Name}}{{end}}";
    let mut args = vec!["inspect", "--format", format];
    args.extend(&ids);
    let Ok(text) = docker(&args) else {
        return holders;
    };
    for line in text.lines() {
        let mut fields = line.split('\t');
        let container = fields.next().unwrap_or("").trim_start_matches('/');
        for mount in fields {
            if let Some(volume) = mount.strip_prefix("volume=") {
                holders
                    .entry(volume.to_string())
                    .or_default()
                    .push(plain(container));
            }
        }
    }
    holders
}

/// Each volume's size, by name. Empty when Docker would not say, which reads as unknown.
fn volume_sizes() -> BTreeMap<String, u64> {
    let Ok(text) = docker(&["system", "df", "-v", "--format", "{{json .}}"]) else {
        return BTreeMap::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
        return BTreeMap::new();
    };
    parsed["Volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|volume| {
            Some((
                volume["Name"].as_str()?.to_string(),
                docker_kb(volume["Size"].as_str()?)?,
            ))
        })
        .collect()
}

/// `Err` is Docker not answering, which is never the same as Docker holding nothing.
fn ask_docker() -> Result<Docker, String> {
    let volumes = docker(&["volume", "ls", "--format", "{{.Name}}"])?;
    let images = docker(&[
        "image",
        "ls",
        "--digests",
        "--format",
        "{{.Digest}}\t{{.Repository}}:{{.Tag}}\t{{.Size}}",
    ])?;
    let sizes = volume_sizes();
    let holders = volume_holders();
    let mut found = Docker::default();
    for name in volumes.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let size = sizes
            .get(name)
            .map_or(Size::unknown(), |kb| Size::known(*kb));
        let held = holders.get(name).cloned().unwrap_or_default();
        let list = if is_anonymous(name) {
            &mut found.anonymous
        } else {
            &mut found.named
        };
        list.push((plain(name), size, held));
    }
    for line in images.lines() {
        let mut fields = line.split('\t');
        let (Some(digest), Some(image)) = (fields.next(), fields.next()) else {
            continue;
        };
        if digest != "<none>" {
            continue;
        }
        if image == "<none>:<none>" {
            found.untagged += 1;
            continue;
        }
        let size = fields
            .next()
            .and_then(docker_kb)
            .map_or(Size::unknown(), Size::known);
        found.images.push((plain(image), size));
    }
    Ok(found)
}

fn survey(sandbox: &str) -> Result<(Vec<Entry>, u64), String> {
    let root = fleet_root();
    let checkouts: HashSet<String> = checkouts_on_disk()?.into_iter().collect();
    let mut records = BTreeMap::new();
    for (name, record) in place_records()? {
        records.insert(name, record);
    }
    let listing = match fs::read_dir(&root) {
        Ok(listing) => Some(listing),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("listing {root}: {e}")),
    };
    let mut entries = Vec::new();
    let mut unopened = 0;
    let mut named = HashSet::new();
    for item in listing.into_iter().flatten() {
        let item = item.map_err(|e| format!("reading an entry of {root}: {e}"))?;
        let name = item.file_name().to_string_lossy().into_owned();
        named.insert(name.clone());
        let path = item.path();
        let is_dir = fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        let walked = walk(&path, &mut Links::default());
        unopened += walked.unopened;
        let total = walked.size();
        let record = records.get(&name);
        let mut note = match record {
            Some(Err(why)) => Some(format!("its record could not be read ({})", plain(why))),
            Some(Ok(r)) if r.sandbox != sandbox => Some(format!(
                "its record names another sandbox, {}",
                plain(&r.sandbox)
            )),
            _ => None,
        };
        let placed_here = matches!(record, Some(Ok(r)) if r.sandbox == sandbox);
        let shape = if !is_dir {
            Shape::NotADirectory
        } else if checkouts.contains(&name) {
            if placed_here {
                Shape::Box
            } else {
                Shape::CheckoutNoRecord
            }
        } else if placed_here {
            Shape::RecordNoCheckout
        } else {
            Shape::NoTree
        };
        let has_tree = matches!(shape, Shape::Box | Shape::CheckoutNoRecord);
        let is_box_dir = is_dir && !name.starts_with('.');
        let rebuild = if is_box_dir {
            rebuildable(&path, has_tree)
        } else {
            Rebuild {
                ignored: Size::ZERO,
                caches: Size::ZERO,
                note: None,
            }
        };
        if let Some(said) = rebuild.note {
            note = Some(match note {
                Some(first) => format!("{first}; {said}"),
                None => said,
            });
        }
        let (cache_kept, skein_cache) = if is_box_dir {
            cache_carried(&path)
        } else {
            (Size::ZERO, Size::ZERO)
        };
        entries.push(Entry {
            name: plain(&name),
            shape,
            carry: total.less(rebuild.ignored.plus(rebuild.caches)),
            ignored: rebuild.ignored,
            caches: rebuild.caches,
            cache_carried: cache_kept,
            skein_cache,
            note,
            on_disk: true,
        });
    }
    for (name, record) in &records {
        if named.contains(name) {
            continue;
        }
        let (shape, note) = match record {
            Ok(r) if r.sandbox == sandbox => (Shape::RecordNoCheckout, None),
            Ok(r) => (
                Shape::RecordNoCheckout,
                Some(format!(
                    "its record names another sandbox, {}",
                    plain(&r.sandbox)
                )),
            ),
            Err(why) => (
                Shape::RecordNoCheckout,
                Some(format!("its record could not be read ({})", plain(why))),
            ),
        };
        entries.push(Entry {
            name: plain(name),
            shape,
            carry: Size::ZERO,
            ignored: Size::ZERO,
            caches: Size::ZERO,
            cache_carried: Size::ZERO,
            skein_cache: Size::ZERO,
            note,
            on_disk: false,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((entries, unopened))
}

fn section(out: &mut String, title: &str, entries: &[Entry], shape: Shape) {
    let members: Vec<&Entry> = entries.iter().filter(|e| e.shape == shape).collect();
    out.push_str(&format!("\n{title} ({})\n", members.len()));
    if members.is_empty() {
        out.push_str("  none\n");
    }
    for entry in members {
        let has_parts =
            matches!(shape, Shape::Box | Shape::CheckoutNoRecord) || entry.caches != Size::ZERO;
        let sizes = match (entry.on_disk, has_parts) {
            (false, _) => "nothing on disk".to_string(),
            (true, true) => format!(
                "carry {}, {}",
                entry.carry.show(),
                rebuildable_text(entry.ignored, entry.caches)
            ),
            (true, false) => format!("carry {}", entry.carry.show()),
        };
        out.push_str(&format!("  {}: {sizes}\n", entry.name));
        if let Some(note) = &entry.note {
            out.push_str(&format!("    {note}\n"));
        }
    }
}

fn rebuildable_text(ignored: Size, caches: Size) -> String {
    format!(
        "rebuildable {} (ignored {}, tool caches {})",
        ignored.plus(caches).show(),
        ignored.show(),
        caches.show()
    )
}

fn holders_text(holders: &[String]) -> String {
    match holders.is_empty() {
        true => String::new(),
        false => format!(", used by {}", holders.join(", ")),
    }
}

fn render(
    root: &str,
    entries: &[Entry],
    unopened: u64,
    docker: &Result<Docker, String>,
    free: Option<u64>,
) -> String {
    let mut out = format!(
        "What moving this whole skein would involve\n\n{root} holds {} entries. Known only from a record: {}.\n",
        entries.iter().filter(|e| e.on_disk).count(),
        entries.iter().filter(|e| !e.on_disk).count(),
    );
    section(
        &mut out,
        "Boxes with a checkout and a record",
        entries,
        Shape::Box,
    );
    section(
        &mut out,
        "Checkouts with no record: real work that nothing lists",
        entries,
        Shape::CheckoutNoRecord,
    );
    section(
        &mut out,
        "Records with no checkout: a start would clone a new box, so never start these",
        entries,
        Shape::RecordNoCheckout,
    );
    section(
        &mut out,
        "Directories with no tree: `skein save` does not carry these",
        entries,
        Shape::NoTree,
    );
    section(
        &mut out,
        "Not directories: listed so nothing is skipped",
        entries,
        Shape::NotADirectory,
    );

    let mut carry = Size::ZERO;
    let mut ignored = Size::ZERO;
    let mut caches = Size::ZERO;
    let mut cache_kept = Size::ZERO;
    let mut skein_cache = Size::ZERO;
    for entry in entries {
        carry = carry.plus(entry.carry);
        ignored = ignored.plus(entry.ignored);
        caches = caches.plus(entry.caches);
        cache_kept = cache_kept.plus(entry.cache_carried);
        skein_cache = skein_cache.plus(entry.skein_cache);
    }
    let rebuild = ignored.plus(caches);
    out.push_str(&format!(
        "\nBox directories: {} to carry, {}.\n\
         Rebuildable is what a box's own git ignores, plus the named tool caches. Ignored files can \
         include hand-made ones such as .env.\n",
        carry.show(),
        rebuildable_text(ignored, caches)
    ));
    if cache_kept != Size::ZERO {
        out.push_str(&format!(
            "Carried from home/.cache: {}, including skein's own session files ({}). Only the \
             named tool caches there count as rebuildable.\n",
            cache_kept.show(),
            skein_cache.show()
        ));
    }

    out.push_str("\nDocker\n");
    let docker_carry = match docker {
        Err(why) => {
            out.push_str(&format!(
                "  Docker did not answer ({}). What it holds is unknown, not empty.\n",
                plain(why)
            ));
            Size::unknown()
        }
        Ok(found) => {
            let mut sum = Size::ZERO;
            if found.named.is_empty() && found.anonymous.is_empty() && found.images.is_empty() {
                out.push_str("  Docker answered: no volumes and no locally built images.\n");
            }
            for (title, list) in [
                ("Named volume", &found.named),
                ("ANONYMOUS volume", &found.anonymous),
            ] {
                for (name, size, holders) in list {
                    sum = sum.plus(*size);
                    out.push_str(&format!(
                        "  {title} {name}: {}{}\n",
                        size.show(),
                        holders_text(holders)
                    ));
                }
            }
            for (name, size) in &found.images {
                sum = sum.plus(*size);
                out.push_str(&format!(
                    "  Image with no registry digest {name}: {}\n",
                    size.show()
                ));
            }
            if found.untagged > 0 {
                out.push_str(&format!(
                    "  {} untagged image layer(s) are not counted.\n",
                    found.untagged
                ));
            }
            sum
        }
    };

    let thin = carry.plus(docker_carry);
    let full = thin.plus(rebuild);
    out.push_str(&format!(
        "\nDisk\n  Free: {}. A move must leave {} free.\n  \
         Thin copy (rebuildable left behind): needs {} plus the floor, {}.\n  \
         Full copy: needs {} plus the floor, {}.\n",
        free.map_or("unknown".to_string(), human),
        human(FLOOR_KB),
        thin.show(),
        verdict(thin, free),
        full.show(),
        verdict(full, free),
    ));

    out.push_str(
        "\nNever moves\n  Claude logins, GitHub and git tokens, service API tokens, private keys, \
         and skein's own secrets.\n",
    );
    if unopened > 0 {
        out.push_str(&format!(
            "  {unopened} credential location(s) were left unopened and are not in these sizes.\n"
        ));
    }
    out.push_str("\nNothing was changed.");
    out
}

/// The report `skein migrate plan` prints.
pub fn migrate_plan() -> Result<String, String> {
    let sandbox = crate::config::load_config()
        .fleet_sandbox
        .trim()
        .to_string();
    let (entries, unopened) = survey(&sandbox)
        .map_err(|why| format!("could not read the fleet: {why}. Nothing was changed."))?;
    let free = available_kb(&skein_home());
    Ok(render(
        &fleet_root(),
        &entries,
        unopened,
        &ask_docker(),
        free,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_size_is_never_read_as_a_fit() {
        let free = Some(FLOOR_KB + 100);
        assert_eq!(verdict(Size::known(100), free), "fits");
        assert_eq!(verdict(Size::known(101), free), "does not fit");
        // Fails if `verdict` treats an open-ended size as its lower bound.
        assert_eq!(
            verdict(Size { low: 1, high: None }, free),
            "cannot say, because some sizes are unknown"
        );
        // Known already to be too big, whatever the unknown part turns out to be.
        assert_eq!(
            verdict(
                Size {
                    low: 101,
                    high: None
                },
                free
            ),
            "does not fit"
        );
        assert_eq!(
            verdict(Size::known(1), None),
            "cannot say, because the free space could not be read"
        );
    }

    #[test]
    fn a_range_too_narrow_to_display_reads_as_about() {
        let gb = 1024 * 1024;
        // Fails if the range prints as `between 84.0 GB and 84.0 GB`.
        assert_eq!(
            Size {
                low: 84 * gb,
                high: Some(84 * gb + 1024)
            }
            .show(),
            "about 84.0 GB"
        );
        assert_eq!(
            Size {
                low: 84 * gb,
                high: Some(85 * gb)
            }
            .show(),
            "unknown (between 84.0 GB and 85.0 GB)"
        );
    }

    #[test]
    fn taking_an_unknown_part_out_leaves_a_range() {
        let total = Size::known(100);
        assert_eq!(total.less(Size::known(30)), Size::known(70));
        // Fails if an unbounded rebuildable size is subtracted as zero.
        assert_eq!(
            total.less(Size {
                low: 30,
                high: None
            }),
            Size {
                low: 0,
                high: Some(70)
            }
        );
    }

    #[test]
    fn docker_sizes_are_decimal_and_junk_is_not_zero() {
        assert_eq!(docker_kb("0B"), Some(0));
        assert_eq!(docker_kb("2kB"), Some(2));
        assert_eq!(docker_kb("1.5GB"), Some(1_464_844));
        assert_eq!(docker_kb("N/A"), None);
    }
}
