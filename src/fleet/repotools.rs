//! A repo's tools area, `<fleet root>/.repo-tools/<repo id>` (SKEIN-1431): how big it is, which of
//! the tool versions installed in it nobody has used for a while, whether a box has one of them open
//! now, and the tools' own pruning of their caches (SKEIN-1432).
//!
//! The owner's rules, on SKEIN-1426:
//!
//!   * **skein lists, and never deletes on its own.** [`repo_tools`] reads; the only removal is the
//!     one a box's agent asks for through `skein_clean`, from the list this writes into the box's
//!     signal file (`resource_signal::clean_list`).
//!   * **Last use is the newest access time of a version's executables**, as the file system keeps
//!     it (`relatime`: updated at most once a day, which is all a count of days needs).
//!   * **A version is removable only while no running box of the repo has a file in it open**
//!     ([`open_tool_files`]).
//!   * **How many days is the owner's setting** (`Config::unused_tool_days`), like the compiler
//!     artefact days, and 0 turns the listing off.
//!   * **The area's size is shown per repo, with no budget.**
//!
//! The pruning is each tool's own command, run in the boxes by [`prune_tool_caches`]: `pnpm store
//! prune` for each box's own pnpm store, `uv cache prune` for the repo's uv cache. cargo needs
//! nothing from skein: since cargo 1.88 it cleans its own download caches of what it has not used
//! for months (`cache.auto-clean-frequency`, one day by default), the box's `CARGO_HOME` is the
//! area's, and nothing skein writes turns that off.

use super::*;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Repo `id`'s tools area on the fleet's disk, where the launcher makes it (`box-session.sh`'s
/// repo-tools block) and every box of the repo sees it at `~/.repo-tools`.
pub fn repo_tools_dir(id: &str) -> PathBuf {
    Path::new(&fleet_root()).join(".repo-tools").join(id)
}

/// Where each installer the launcher points into the area keeps one directory per version, under
/// the area, and how a version's directory name reads to a person.
///
/// The paths are the launcher's exports (`RUSTUP_HOME`, `NVM_DIR`, `FNM_DIR`, `VOLTA_HOME`,
/// `UV_PYTHON_INSTALL_DIR`, `PYENV_ROOT`, `GOPATH` and `MISE_DATA_DIR`, all under the area), with
/// each tool's own layout below them.
const VERSION_DIRS: &[(&str, &str, Naming)] = &[
    ("rustup", "rustup/toolchains", Naming::Rust),
    ("nvm", "nvm/versions/node", Naming::Node),
    ("fnm", "fnm/node-versions", Naming::Node),
    ("volta", "volta/tools/image/node", Naming::Node),
    ("uv", "uv/python", Naming::UvPython),
    ("pyenv", "pyenv/versions", Naming::Python),
    // `go` fetches a toolchain a `go.mod` asks for into the module cache, as a module.
    ("go", "go/pkg/mod/golang.org", Naming::Go),
    // `mise` keeps `installs/<tool>/<version>`: one level more than the rest.
    ("mise", "mise/installs", Naming::Mise),
];

#[derive(Clone, Copy)]
enum Naming {
    Rust,
    Node,
    UvPython,
    Python,
    Go,
    Mise,
}

/// What a person reads for version directory `name`, or `None` when it is not a version.
fn version_name(naming: Naming, name: &str) -> Option<String> {
    match naming {
        // `1.92.0-x86_64-unknown-linux-gnu`, `nightly-2026-01-01-aarch64-…`: the host triple goes.
        Naming::Rust => {
            let parts: Vec<&str> = name.split('-').collect();
            let arch = parts.iter().position(|p| {
                [
                    "x86_64",
                    "aarch64",
                    "i686",
                    "armv7",
                    "arm",
                    "riscv64gc",
                    "powerpc64le",
                    "s390x",
                    "loongarch64",
                ]
                .contains(p)
            });
            let version = parts[..arch.unwrap_or(parts.len())].join("-");
            (!version.is_empty()).then(|| format!("Rust {version}"))
        }
        Naming::Node => Some(format!("Node {}", name.strip_prefix('v').unwrap_or(name))),
        // `cpython-3.12.1-linux-x86_64-gnu`, `pypy-3.10.14-linux-x86_64-gnu`.
        Naming::UvPython => {
            let mut parts = name.splitn(3, '-');
            let implementation = parts.next()?;
            let version = parts.next()?;
            let tool = match implementation {
                "cpython" => "Python",
                "pypy" => "PyPy",
                other => other,
            };
            Some(format!("{tool} {version}"))
        }
        Naming::Python => Some(format!("Python {name}")),
        // `toolchain@v0.0.1-go1.22.0.linux-amd64`; anything else in `golang.org` is a module.
        Naming::Go => {
            let rest = name.strip_prefix("toolchain@")?;
            let version = rest.split_once("-go")?.1;
            let version = match version.rsplit_once('.') {
                Some((v, platform)) if platform.contains('-') => v,
                _ => version,
            };
            Some(format!("Go {version}"))
        }
        // `<tool>/<version>`, joined by the caller.
        Naming::Mise => {
            let (tool, version) = name.split_once('/')?;
            let tool = match tool {
                "node" => "Node",
                "python" => "Python",
                "go" => "Go",
                "rust" => "Rust",
                "ruby" => "Ruby",
                "java" => "Java",
                other => other,
            };
            Some(format!("{tool} {version}"))
        }
    }
}

/// One installed version of a tool in a repo's tools area.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolVersion {
    /// Absolute, as the fleet sees it.
    pub path: String,
    /// Relative to the area: the same in every box of the repo, which sees the area at
    /// `~/.repo-tools`.
    pub rel: String,
    /// `rustup`, `nvm`, `fnm`, `volta`, `uv`, `pyenv`, `go` or `mise`.
    pub installer: &'static str,
    /// `Rust 1.92.0`, `Node 20.11.1`, `Python 3.12.1`.
    pub name: String,
    /// Allocated bytes, by the walk every other disk figure in skein comes from.
    pub bytes: u64,
    /// Whole days since one of its executables was last read.
    pub unused_days: u64,
    /// The line a person reads: `Rust 1.92.0 · unused 41 days · 1.3 GB`.
    pub shown: String,
}

/// Bytes as the version line prints them: `1.3 GB`, or `420 MB` under a gigabyte.
fn size(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 1024.0 {
        format!("{:.1} GB", mib / 1024.0)
    } else {
        format!("{} MB", (mib.round() as u64).max(1))
    }
}

/// `Rust 1.92.0 · unused 41 days · 1.3 GB`.
fn shown(name: &str, unused_days: u64, bytes: u64) -> String {
    let days = match unused_days {
        1 => "1 day".to_string(),
        n => format!("{n} days"),
    };
    format!("{name} · unused {days} · {}", size(bytes))
}

/// Walk version directory `dir` on device `dev`, never through a link: its allocated bytes, and
/// the newest access time of its executables (any execute bit, which shared libraries carry too),
/// or of anything in it when it has none.
fn walk_version(dir: &Path, dev: u64) -> (u64, Option<i64>) {
    let mut bytes = 0u64;
    let mut newest_exec: Option<i64> = None;
    let mut newest_any: Option<i64> = None;
    let mut seen = std::collections::HashSet::new();
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        seen.insert((meta.dev(), meta.ino()));
        bytes += meta.blocks() * 512;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(kids) = std::fs::read_dir(&d) else {
            continue;
        };
        for kid in kids.flatten() {
            let Ok(meta) = kid.metadata() else {
                continue;
            };
            if meta.dev() != dev || !seen.insert((meta.dev(), meta.ino())) {
                continue;
            }
            bytes += meta.blocks() * 512;
            if meta.is_dir() {
                stack.push(kid.path());
            } else if meta.is_file() {
                let at = meta.atime();
                newest_any = Some(newest_any.map_or(at, |n| n.max(at)));
                if meta.mode() & 0o111 != 0 {
                    newest_exec = Some(newest_exec.map_or(at, |n| n.max(at)));
                }
            }
        }
    }
    (bytes, newest_exec.or(newest_any))
}

/// Every tool version installed in `area`, measured, biggest first. `now` is seconds since the
/// epoch. A link where a version would be (the fleet's own Rust toolchain is linked into rustup's
/// `toolchains` as `fleet`) is not the repo's and is skipped, as is anything off the area's device.
pub fn tool_versions_in(area: &Path, now: i64) -> Vec<ToolVersion> {
    let Ok(meta) = std::fs::symlink_metadata(area) else {
        return Vec::new();
    };
    if !meta.is_dir() {
        return Vec::new();
    }
    let dev = meta.dev();
    let real_dir =
        |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && m.dev() == dev);
    let children = |p: &Path| -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(p) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .filter(|n| real_dir(&p.join(n)))
            .collect();
        names.sort();
        names
    };
    let mut out = Vec::new();
    for (installer, under, naming) in VERSION_DIRS {
        let parent = area.join(under);
        // Every component from the area down must be a real directory, or a link could lead the
        // walk, and later a removal, out of the area.
        let mut at = area.to_path_buf();
        if !under.split('/').all(|c| {
            at.push(c);
            real_dir(&at)
        }) {
            continue;
        }
        let mut found: Vec<(String, String)> = Vec::new();
        for name in children(&parent) {
            match naming {
                Naming::Mise => {
                    for version in children(&parent.join(&name)) {
                        found.push((format!("{name}/{version}"), format!("{name}/{version}")));
                    }
                }
                _ => found.push((name.clone(), name)),
            }
        }
        for (rel_name, label) in found {
            let Some(name) = version_name(*naming, &label) else {
                continue;
            };
            let path = parent.join(&rel_name);
            let (bytes, last) = walk_version(&path, dev);
            let unused_days = last.map_or(0, |t| (now - t).max(0) as u64 / 86_400);
            out.push(ToolVersion {
                path: path.display().to_string(),
                rel: format!("{under}/{rel_name}"),
                installer,
                shown: shown(&name, unused_days, bytes),
                name,
                bytes,
                unused_days,
            });
        }
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    out
}

/// A repo's tools area as a person is shown it: its size, and the versions in it unused for the
/// owner's number of days.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepoTools {
    /// The area, `<fleet root>/.repo-tools/<id>`.
    pub path: String,
    /// All of it, caches and tools together. Counted toward the repo and toward no box.
    pub bytes: u64,
    /// The setting the list was made with (`Config::unused_tool_days`); 0 means no list.
    pub unused_after_days: u32,
    /// Versions unused for at least that many days, biggest first.
    pub unused: Vec<ToolVersion>,
}

/// Repo `id`'s tools area, measured, or `None` when it has none: the launcher has not made one
/// yet, or the repo's store is kept elsewhere and its boxes keep their own tools. A link where the
/// area should be is not one.
pub fn repo_tools(id: &str) -> Option<RepoTools> {
    repo_tools_at(
        &repo_tools_dir(id),
        crate::config::load_config().unused_tool_days,
        chrono::Utc::now().timestamp(),
    )
}

/// [`repo_tools`] for the area at `area`, with the setting and the clock passed in.
pub fn repo_tools_at(area: &Path, days: u32, now: i64) -> Option<RepoTools> {
    let meta = std::fs::symlink_metadata(area).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let unused = match days {
        0 => Vec::new(),
        d => tool_versions_in(area, now)
            .into_iter()
            .filter(|v| v.unused_days >= u64::from(d))
            .collect(),
    };
    Some(RepoTools {
        path: area.display().to_string(),
        bytes: tree_bytes(area, Some(meta.dev())),
        unused_after_days: days,
        unused,
    })
}

/// Every file a process on this machine has open in one of `areas`, as paths relative to the area,
/// keyed by the area's path as given, or `None` when `/proc` cannot be listed, so the caller cannot
/// tell and must not offer a removal. Open is what a process runs (`exe`), works in (`cwd`), holds
/// open (`fd`) or has mapped (`maps`, which is where a running toolchain's shared libraries are).
///
/// A box sees its repo's area at `~/.repo-tools`, so a path a box process has open reads
/// `<its home>/.repo-tools/<rel>`, and every repo's boxes spell it the same way. Which repo's area
/// it is, is asked of the process's own root: `/proc/<pid>/root/<its home>/.repo-tools` is one
/// directory with the area, by device and inode, exactly when the box has that area bound there. A
/// process outside every box spells the area by its own path, `<fleet root>/.repo-tools/<id>/<rel>`,
/// and is matched the same way.
pub fn open_tool_files(areas: &[PathBuf]) -> Option<BTreeMap<String, BTreeSet<String>>> {
    open_tool_files_under(Path::new("/proc"), areas)
}

/// [`open_tool_files`] over the process table at `proc_root`.
pub(crate) fn open_tool_files_under(
    proc_root: &Path,
    areas: &[PathBuf],
) -> Option<BTreeMap<String, BTreeSet<String>>> {
    let ids: HashMap<(u64, u64), String> = areas
        .iter()
        .filter_map(|a| {
            let m = std::fs::symlink_metadata(a).ok()?;
            m.is_dir()
                .then(|| ((m.dev(), m.ino()), a.display().to_string()))
        })
        .collect();
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if ids.is_empty() {
        return Some(out);
    }
    let pids = std::fs::read_dir(proc_root).ok()?;
    const MARK: &str = "/.repo-tools/";
    for entry in pids.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            .map(str::to_string)
        else {
            continue;
        };
        let at = proc_root.join(&pid);
        let mut paths: Vec<String> = Vec::new();
        for link in ["exe", "cwd"] {
            if let Ok(p) = std::fs::read_link(at.join(link)) {
                paths.push(p.to_string_lossy().into_owned());
            }
        }
        if let Ok(fds) = std::fs::read_dir(at.join("fd")) {
            for fd in fds.flatten() {
                if let Ok(p) = std::fs::read_link(fd.path()) {
                    paths.push(p.to_string_lossy().into_owned());
                }
            }
        }
        if let Ok(maps) = std::fs::read_to_string(at.join("maps")) {
            for line in maps.lines() {
                // address perms offset dev inode path: the path is what follows the fifth field.
                let mut rest = line;
                for _ in 0..5 {
                    rest = rest.trim_start();
                    rest = rest.find(char::is_whitespace).map_or("", |i| &rest[i..]);
                }
                let path = rest.trim_start();
                if path.starts_with('/') {
                    paths.push(path.to_string());
                }
            }
        }
        let mut asked: HashMap<String, Option<(u64, u64)>> = HashMap::new();
        let mut identify = |dir: String| -> Option<String> {
            let key = asked.entry(dir.clone()).or_insert_with(|| {
                let m =
                    std::fs::metadata(at.join("root").join(dir.trim_start_matches('/'))).ok()?;
                Some((m.dev(), m.ino()))
            });
            key.and_then(|k| ids.get(&k).cloned())
        };
        for raw in paths {
            let p = raw.strip_suffix(" (deleted)").unwrap_or(&raw);
            let Some(i) = p.find(MARK) else {
                continue;
            };
            let (prefix, rest) = (&p[..i], &p[i + MARK.len()..]);
            // A box's view: the area is `<home>/.repo-tools`.
            if let Some(area) = identify(format!("{prefix}/.repo-tools")) {
                out.entry(area).or_default().insert(rest.to_string());
                continue;
            }
            // The fleet's view: the area is `<fleet root>/.repo-tools/<id>`.
            let (id, rel) = rest.split_once('/').unwrap_or((rest, ""));
            if let Some(area) = identify(format!("{prefix}/.repo-tools/{id}")) {
                out.entry(area).or_default().insert(rel.to_string());
            }
        }
    }
    Some(out)
}

/// Whether any of `open`, paths relative to an area, is the version `rel` or inside it.
pub fn version_is_open(open: &BTreeSet<String>, rel: &str) -> bool {
    let inside = format!("{rel}/");
    open.iter().any(|p| p == rel || p.starts_with(&inside))
}

/// How often each running box's tools prune their own caches: once a day, as cargo's own
/// automatic clean runs.
pub const PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// What a box runs to prune its tools' caches, by each tool's own command, through a crossing,
/// which sets every variable the box's session sets (SKEIN-1354): `npm_config_store_dir` is the
/// box's own pnpm store and `UV_CACHE_DIR` the repo's uv cache in its tools area. A tool the box
/// does not have, or a cache it has not made, is skipped. `uv` is run in one box of each repo only,
/// since the cache is the repo's: `$1` is `uv` in that one. uv waits for any other uv using the
/// cache to finish before it prunes, and the deadline the crossing is given bounds that wait.
pub const PRUNE_SCRIPT: &str = r#"set -u
if [ -n "${npm_config_store_dir-}" ] && [ -d "$npm_config_store_dir" ] && command -v pnpm >/dev/null 2>&1; then
  pnpm store prune --store-dir "$npm_config_store_dir" >/dev/null 2>&1 && echo "pnpm pruned" || echo "pnpm failed"
fi
if [ "${1-}" = uv ] && [ -n "${UV_CACHE_DIR-}" ] && [ -d "$UV_CACHE_DIR" ] && command -v uv >/dev/null 2>&1; then
  uv cache prune --cache-dir "$UV_CACHE_DIR" >/dev/null 2>&1 && echo "uv pruned" || echo "uv failed"
fi
"#;

/// Which of `running` boxes prune now, and which of them prunes its repo's uv cache: every box not
/// pruned within [`PRUNE_EVERY`] by `last`, and the first of those in each repo for uv. `repo_of`
/// names a box's repo. A box with no repo has no area and prunes only its pnpm store.
pub fn prune_plan(
    running: &[String],
    last: &HashMap<String, std::time::Instant>,
    now: std::time::Instant,
    repo_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, bool)> {
    let mut uv_taken = BTreeSet::new();
    let mut out = Vec::new();
    for name in running {
        if last
            .get(name)
            .is_some_and(|t| now.saturating_duration_since(*t) < PRUNE_EVERY)
        {
            continue;
        }
        let uv = repo_of(name).is_some_and(|r| uv_taken.insert(r));
        out.push((name.clone(), uv));
    }
    out
}

/// Prune the tools' caches of every running box due one ([`prune_plan`]), and record when.
/// Never deletes anything the tools would not: each runs its own prune.
pub fn prune_tool_caches(last: &mut HashMap<String, std::time::Instant>) {
    let liveness = crate::fleet::fleet_liveness();
    let mut running: Vec<String> = liveness
        .iter()
        .filter(|(_, up)| **up)
        .map(|(n, _)| n.clone())
        .collect();
    running.sort();
    let now = std::time::Instant::now();
    let plan = prune_plan(&running, last, now, &|n| {
        crate::repos::repo_for_box(n).map(|r| r.id)
    });
    for (name, uv) in plan {
        let script = format!(
            "bash -c {} prune {}",
            crate::util::sh_quote(PRUNE_SCRIPT),
            if uv { "uv" } else { "-" }
        );
        let ran = crate::place::located(&name)
            .and_then(|p| p.exec(&script, std::time::Duration::from_secs(600)));
        if let Err(e) = ran {
            eprintln!("skein: pruning {name}'s tool caches: {e}");
        }
        last.insert(name, now);
    }
}

/// The loop skein-server runs: [`prune_tool_caches`] every hour, so a box is pruned within an hour
/// of the server starting and then once a day. Never returns.
pub async fn watch_tool_caches() {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
    let mut last: HashMap<String, std::time::Instant> = HashMap::new();
    loop {
        tick.tick().await;
        let mut held = std::mem::take(&mut last);
        match tokio::task::spawn_blocking(move || {
            prune_tool_caches(&mut held);
            held
        })
        .await
        {
            Ok(done) => last = done,
            Err(e) => eprintln!("skein: tool cache pruning did not run: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    const DAY: i64 = 86_400;
    /// The clock the fixtures are read at.
    const NOW: i64 = 1_800_000_000;

    /// A file of `kb` KiB at `path`, executable when `exec`, last read `days` days before [`NOW`]
    /// and written 400 days before it.
    fn put(path: &Path, kb: usize, exec: bool, days: i64) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; kb * 1024]).unwrap();
        let mode = if exec { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        let at =
            |d: i64| std::time::UNIX_EPOCH + std::time::Duration::from_secs((NOW - d * DAY) as u64);
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_times(
            std::fs::FileTimes::new()
                .set_accessed(at(days))
                .set_modified(at(400)),
        )
        .unwrap();
    }

    /// An area with one version of every installer, each old but one fresh Rust, and the fleet's
    /// toolchain linked in as rustup's `fleet`, as the shim links it.
    fn area(base: &Path) -> PathBuf {
        let area = base.join(".repo-tools/web");
        let rust = area.join("rustup/toolchains");
        put(
            &rust.join("1.80.0-x86_64-unknown-linux-gnu/bin/rustc"),
            64,
            true,
            41,
        );
        // The newest READ of a non-executable is not use: rustdoc's html read yesterday.
        put(
            &rust.join("1.80.0-x86_64-unknown-linux-gnu/share/doc/index.html"),
            8,
            false,
            1,
        );
        put(
            &rust.join("1.92.0-x86_64-unknown-linux-gnu/bin/rustc"),
            64,
            true,
            2,
        );
        let fleet = base.join("fleet-toolchain");
        put(&fleet.join("bin/rustc"), 8, true, 90);
        std::os::unix::fs::symlink(&fleet, rust.join("fleet")).unwrap();
        put(
            &area.join("nvm/versions/node/v20.11.1/bin/node"),
            32,
            true,
            50,
        );
        put(
            &area.join("fnm/node-versions/v18.0.0/installation/bin/node"),
            32,
            true,
            60,
        );
        put(
            &area.join("volta/tools/image/node/22.1.0/bin/node"),
            32,
            true,
            70,
        );
        put(
            &area.join("uv/python/cpython-3.12.1-linux-x86_64-gnu/bin/python3.12"),
            32,
            true,
            80,
        );
        std::fs::create_dir_all(area.join("uv/python/.lock")).unwrap();
        put(
            &area.join("pyenv/versions/3.11.4/bin/python3.11"),
            32,
            true,
            35,
        );
        put(
            &area.join("go/pkg/mod/golang.org/toolchain@v0.0.1-go1.22.0.linux-amd64/bin/go"),
            32,
            true,
            45,
        );
        // A module, not a toolchain, beside it.
        put(
            &area.join("go/pkg/mod/golang.org/x/text@v0.14.0/doc.go"),
            4,
            false,
            300,
        );
        put(
            &area.join("mise/installs/ruby/3.3.0/bin/ruby"),
            32,
            true,
            31,
        );
        put(
            &area.join("cache/cargo/registry/cache/x.crate"),
            128,
            false,
            1,
        );
        area
    }

    /// **A tool version nobody has run for the owner's number of days is listed, with what it is,
    /// how long unused and how big; one run recently is not; the setting's 0 lists nothing; and
    /// the area's whole size is given** (SKEIN-1432).
    ///
    /// Every installer the launcher points into the area is here, named as a person reads it, and
    /// the fleet's own Rust toolchain, a link in rustup's `toolchains`, is never the repo's to list.
    ///
    /// What would make it fail: reading use from any file rather than the executables (the old
    /// Rust's documentation was read yesterday, so it reads unused 1 day and drops off); reading
    /// mtime (every version reads 400 days, and the fresh Rust is listed); following the `fleet`
    /// link (a third Rust appears); a layout path wrong (that installer's line is missing); the
    /// `0` case not short-cut (the list is not empty); or the threshold compared the wrong way.
    #[test]
    fn an_old_version_is_listed_and_a_fresh_one_is_not() {
        let dir = tempdir();
        let area = area(dir.as_ref());
        let listed = repo_tools_at(&area, 30, NOW).expect("the area is a directory");
        let mut names: Vec<String> = listed
            .unused
            .iter()
            .map(|v| format!("{} {} {}", v.installer, v.name, v.unused_days))
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "fnm Node 18.0.0 60",
                "go Go 1.22.0 45",
                "mise Ruby 3.3.0 31",
                "nvm Node 20.11.1 50",
                "pyenv Python 3.11.4 35",
                "rustup Rust 1.80.0 41",
                "uv Python 3.12.1 80",
                "volta Node 22.1.0 70",
            ],
        );
        let rust = listed
            .unused
            .iter()
            .find(|v| v.installer == "rustup")
            .unwrap();
        assert_eq!(
            rust.rel,
            "rustup/toolchains/1.80.0-x86_64-unknown-linux-gnu"
        );
        assert!(
            rust.shown.starts_with("Rust 1.80.0 · unused 41 days · ")
                && rust.shown.ends_with(" MB"),
            "{}",
            rust.shown
        );
        assert!(listed.bytes >= 600 * 1024, "{}", listed.bytes);
        assert_eq!(listed.bytes, tree_bytes(&area, None));
        assert_eq!(listed.unused_after_days, 30);

        let fresh = repo_tools_at(&area, 1, NOW).unwrap();
        assert!(
            fresh.unused.iter().any(|v| v.name == "Rust 1.92.0"),
            "a version unused for two days is listed at one"
        );
        assert!(repo_tools_at(&area, 0, NOW).unwrap().unused.is_empty());
        let base: &Path = dir.as_ref();
        assert_eq!(repo_tools_at(&base.join("none"), 30, NOW), None);
    }

    /// `sleep` holding `path` open on descriptor 3, from where it was started.
    fn holding(path: &Path, cwd: &Path) -> std::process::Child {
        let child = std::process::Command::new("bash")
            .arg("-c")
            .arg("exec 3<\"$1\"; exec sleep 30")
            .arg("holder")
            .arg(path)
            .current_dir(cwd)
            .spawn()
            .unwrap();
        // Until `sleep` holds it: the descriptor is there once bash has exec'd.
        let fd = PathBuf::from(format!("/proc/{}/fd/3", child.id()));
        for _ in 0..200 {
            if std::fs::read_link(&fd).is_ok_and(|p| p == path)
                && std::fs::read_link(format!("/proc/{}/exe", child.id()))
                    .is_ok_and(|e| e.ends_with("sleep"))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        child
    }

    /// **A file a process has open in a version marks that version open, in that repo's area
    /// alone, and a process working inside one marks it too** (SKEIN-1432: a version is removable
    /// only while no box of the repo has a file in it open).
    ///
    /// The process table here spells the area by its own path, as a process outside every box
    /// does; `tests/isolation_bwrap/repocache.rs` holds a file open from inside a real box, which
    /// spells it `~/.repo-tools`.
    ///
    /// What would make it fail: not reading descriptors (nothing is open); not reading `cwd` (the
    /// version worked in is not open); matching by the path after `.repo-tools/` without asking
    /// which area it is (`webapp`'s same version reads open in `web`); or a prefix match without
    /// the separator (`1.80.0-x` open makes `1.80.0` open).
    #[test]
    fn a_file_open_in_a_version_is_seen_in_that_area_only() {
        let dir = tempdir();
        let base: &Path = dir.as_ref();
        let web = area(base);
        let other = base.join(".repo-tools/webapp");
        let tc = "rustup/toolchains/1.80.0-x86_64-unknown-linux-gnu";
        put(&other.join(tc).join("bin/rustc"), 4, true, 41);
        put(
            &other.join("pyenv/versions/3.11.4/bin/python3.11"),
            4,
            true,
            41,
        );
        let mut held = holding(&web.join(tc).join("bin/rustc"), base);
        let mut inside = holding(
            &other.join("pyenv/versions/3.11.4/bin/python3.11"),
            &web.join("nvm/versions/node/v20.11.1/bin"),
        );
        let open = open_tool_files(&[web.clone(), other.clone()]);
        held.kill().ok();
        inside.kill().ok();
        held.wait().ok();
        inside.wait().ok();
        let open = open.expect("this process table can be read");
        let none = BTreeSet::new();
        let in_web = open.get(&web.display().to_string()).unwrap_or(&none);
        let in_other = open.get(&other.display().to_string()).unwrap_or(&none);
        assert!(version_is_open(in_web, tc), "{open:?}");
        assert!(
            version_is_open(in_web, "nvm/versions/node/v20.11.1"),
            "{open:?}"
        );
        assert!(
            !version_is_open(in_web, "pyenv/versions/3.11.4"),
            "{open:?}"
        );
        assert!(
            version_is_open(in_other, "pyenv/versions/3.11.4"),
            "{open:?}"
        );
        assert!(!version_is_open(in_other, tc), "{open:?}");
        assert!(
            !version_is_open(in_web, "rustup/toolchains/1.80"),
            "{open:?}"
        );
    }

    /// **Each running box prunes once a day, and each repo's uv cache is pruned by one box of the
    /// repo at a time** (SKEIN-1432).
    ///
    /// What would make it fail: no record of the last prune (`web-main` prunes again an hour
    /// later); uv given to every box (`web-two` prunes the cache `web-main` is pruning); or uv
    /// given to a box with no repo, which has no area.
    #[test]
    fn each_box_prunes_daily_and_one_box_a_repo_prunes_uv() {
        let now = std::time::Instant::now();
        let running: Vec<String> = ["solo", "web-main", "web-two", "webapp-main"]
            .map(String::from)
            .to_vec();
        let repo_of = |n: &str| -> Option<String> {
            ["webapp", "web"]
                .into_iter()
                .find(|r| n.starts_with(&format!("{r}-")))
                .map(String::from)
        };
        let first = prune_plan(&running, &HashMap::new(), now, &repo_of);
        assert_eq!(
            first,
            vec![
                ("solo".to_string(), false),
                ("web-main".to_string(), true),
                ("web-two".to_string(), false),
                ("webapp-main".to_string(), true),
            ]
        );
        let mut last = HashMap::new();
        last.insert("web-main".to_string(), now);
        let hour = now + std::time::Duration::from_secs(3600);
        let later = prune_plan(&running, &last, hour, &repo_of);
        assert!(!later.iter().any(|(n, _)| n == "web-main"), "{later:?}");
        assert!(later.contains(&("web-two".to_string(), true)), "{later:?}");
        let day = now + PRUNE_EVERY;
        assert!(
            prune_plan(&running, &last, day, &repo_of).contains(&("web-main".to_string(), true))
        );
    }

    /// **The prune runs each tool's own command, on the store and cache the box's session points
    /// it at, and skips a tool or a cache that is not there** (SKEIN-1432).
    ///
    /// Run with stand-ins for `pnpm` and `uv` that record how they were called.
    ///
    /// What would make it fail: the wrong subcommand or store (the recorded line differs); uv run
    /// in a box not chosen for it (`-` records it); or a cache that was never made pruned anyway.
    #[test]
    fn the_prune_script_runs_each_tools_own_prune() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let base: &Path = dir.as_ref();
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = base.join("log");
        for tool in ["pnpm", "uv"] {
            let stub = bin.join(tool);
            std::fs::write(
                &stub,
                format!("#!/bin/sh\necho {tool} \"$@\" >> {}\n", log.display()),
            )
            .unwrap();
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let store = base.join("pnpm-store");
        let uv = base.join("uv");
        let run = |arg: &str, make: bool| {
            let _ = std::fs::remove_file(&log);
            let _ = std::fs::remove_dir(&store);
            let _ = std::fs::remove_dir(&uv);
            if make {
                std::fs::create_dir_all(&store).unwrap();
                std::fs::create_dir_all(&uv).unwrap();
            }
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(PRUNE_SCRIPT)
                .arg("prune")
                .arg(arg)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("npm_config_store_dir", &store)
                .env("UV_CACHE_DIR", &uv)
                .output()
                .unwrap();
            assert!(out.status.success());
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                std::fs::read_to_string(&log).unwrap_or_default(),
            )
        };
        let (said, called) = run("uv", true);
        assert_eq!(said, "pnpm pruned\nuv pruned\n");
        assert_eq!(
            called,
            format!(
                "pnpm store prune --store-dir {}\nuv cache prune --cache-dir {}\n",
                store.display(),
                uv.display()
            )
        );
        let (_, called) = run("-", true);
        assert_eq!(
            called,
            format!("pnpm store prune --store-dir {}\n", store.display())
        );
        let (said, called) = run("uv", false);
        assert_eq!((said.as_str(), called.as_str()), ("", ""));
    }
}
