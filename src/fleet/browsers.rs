//! Playwright browsers for the whole fleet: one read-only copy of each, fetched by skein and by no
//! box (SKEIN-1341).
//!
//! Every box used to download its own browsers into `~/.cache/ms-playwright`: 5.3 GB over 14 boxes,
//! measured on 2026-10-26, most of it the same few versions over and over. The owner's decision of
//! 2026-10-29: **Playwright browsers only**, one copy of each browser version for the whole fleet,
//! read-only in every box, installed by skein from Playwright's official download, outside any box
//! and never by a box. A box that needs a version the fleet does not have files a request, and skein
//! fetches it without asking the owner, because only skein writes the store and only from the
//! official source.
//!
//! The pieces, in the order a browser travels:
//!
//!   * **The store**, [`store_dir`]: `<fleet root>/.skein/browsers/<entry>`, one directory per
//!     browser revision exactly as Playwright lays it out (`chromium-1234`, `ffmpeg-1011`). The
//!     launcher binds it read-only into every box, the workshop box included, beside the fleet's
//!     Rust toolchain (`box-session.sh`, "the fleet's Playwright browsers").
//!   * **The links**, built by the kit inside each box, as the box: one symlink per store entry in
//!     the box's own `~/.cache/ms-playwright`, which the launcher names as `PLAYWRIGHT_BROWSERS_PATH`
//!     with `PLAYWRIGHT_SKIP_BROWSER_GC=1` (`src/kit/skein-startup.sh`). A version the store lacks
//!     still downloads, privately, into the same directory, so nothing a box did before breaks.
//!   * **The request**, through the package queue a box already has: kind `playwright`, one
//!     `<browser>@<version>`, refused unless both are strictly the shape of a real one
//!     ([`want_is_nameable`]). [`serve`] approves it without the owner and fills it.
//!   * **The fill**, [`fill`]: Playwright's own installer, `npx playwright@<version> install
//!     <browser>`, run by skein in the fleet sandbox inside a throwaway namespace
//!     ([`namespace_script`]) that sees `/usr` and `/etc` read-only and nothing of the fleet, then
//!     checked, made read-only and renamed into the store once that namespace is gone.
//!   * **The view and the clean**, [`store_view`] and [`clean_unused`]: what the store holds, which
//!     boxes link each entry, and removing what no box links, offered and confirmed in the cockpit,
//!     never automatic.
//!
//! **Who runs the fill, and why it cannot run a box's code.** skein does, as the fleet sandbox's
//! own user, from the server's request pass. Not the warden: the warden is the host's (§8), and
//! this needs no privilege at all — it writes one staging directory skein owns. What runs inside
//! the namespace is the `playwright` package at the exact version asked for, fetched from the npm
//! registry by the sandbox's own `npx`, with `HOME`, the npm cache and the browsers path all inside
//! the staging directory. No box's tree, home, `node_modules` or config is in that namespace, so
//! there is no box file for it to run or read; it is upstream code, unprivileged, writing only
//! staging. What it writes is then judged by skein from outside, after the namespace has exited and
//! `--unshare-pid` has taken every process it started.
//!
//! **What this does not defend.** Playwright publishes no digest for its browser downloads
//! (`docs/design/box-environment.md` §6.1 measured its bundle: none), so the store holds what the
//! official CDN served, as every box's own download did. And the workshop box, like the sandbox it
//! stands for, can reach the staging directory during a fill and the store's files through the
//! Docker socket; the read-only bind is for every box that does not use either.

use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The browsers a box may ask for: what `playwright install` accepts that is a download into the
/// browsers directory. `chrome` and `msedge` are left out because Playwright installs those into the
/// system with root, which no fill does.
pub const BROWSERS: &[&str] = &[
    "chromium",
    "chromium-headless-shell",
    "firefox",
    "webkit",
    "ffmpeg",
];

/// The most one fill may add to the store. Measured for Playwright 1.62.0 on 2026-10-29: `install
/// chromium` brought chromium (641 MB), its headless shell (340 MB) and ffmpeg (3.3 MB), about
/// 1 GB, and no single browser is larger. Twice that, so a bigger build next year is not refused.
pub const FILL_CAP: u64 = 2 << 30;

/// The store's budget. A fill starts only while the store has room for [`FILL_CAP`] under it, so
/// the store never passes this. About three versions of everything; the 14-box survey found 5.3 GB
/// of browsers in total, mostly copies.
pub const BUDGET: u64 = 6 << 30;

/// How long one fill may take before it is stopped. 1 GB took two minutes on 2026-10-29.
const FILL_DEADLINE: Duration = Duration::from_secs(20 * 60);

/// The box's own browsers directory under its home, where the kit puts the links and where
/// `PLAYWRIGHT_BROWSERS_PATH` points (`box-session.sh` spells it `$HOME/.cache/ms-playwright`).
const FARM: &str = ".cache/ms-playwright";

/// How many entries of one box's browsers directory are read before the box is called unreadable.
/// A box can make that directory as large as it likes, and this is read on every view.
const FARM_BOUND: usize = 4096;

/// Where the fleet's browsers are, inside the sandbox and, read-only, inside every box.
pub fn store_dir() -> String {
    format!("{}/.skein/browsers", fleet_root())
}

/// Where a fill stages, **inside the store**, so the store's read-only bind covers it in every box
/// and the publish is a rename within one filesystem. Hidden, so the kit never links it.
fn staging_root() -> PathBuf {
    Path::new(&store_dir()).join(".staging")
}

/// skein's record of which entries each fill brought in, so a request for a version the fleet
/// already has is answered without fetching. skein's own, never in a box's reach.
fn fills_path() -> PathBuf {
    crate::config::skein_home().join("browsers.json")
}

/// `<browser>@<version>`, split, when both halves are the shape of a real one.
pub fn want_parts(want: &str) -> Option<(&str, &str)> {
    let (browser, version) = want.split_once('@')?;
    (BROWSERS.contains(&browser) && version_is_nameable(version)).then_some((browser, version))
}

/// Is this a browser and a Playwright version, and nothing else?
///
/// The version reaches `npx playwright@<version>` on a command line, so it is a whitelist of the
/// one shape Playwright's releases have: `1.<minor>.<patch>`, plain numbers, no leading zeros, no
/// pre-release tag. Minor 20 and up: older releases laid their browsers out differently. Whether
/// the version exists is npm's to say, and a fill of one that does not fails with npm's sentence.
pub fn want_is_nameable(want: &str) -> bool {
    want_parts(want).is_some()
}

fn version_is_nameable(v: &str) -> bool {
    let number = |s: &str| {
        !s.is_empty()
            && s.len() <= 4
            && s.chars().all(|c| c.is_ascii_digit())
            && (s == "0" || !s.starts_with('0'))
    };
    let mut parts = v.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("1"), Some(minor), Some(patch), None) => {
            number(minor) && number(patch) && minor.parse::<u32>().is_ok_and(|m| m >= 20)
        }
        _ => false,
    }
}

/// Is this the name of a browser directory as Playwright makes one: `<name>-<revision>`?
///
/// It becomes a path in the store and a link in every box's browsers directory, so it is checked as
/// strictly as the kit checks it before linking.
pub fn entry_is_nameable(name: &str) -> bool {
    let Some((browser, revision)) = name.rsplit_once('-') else {
        return false;
    };
    name.len() <= 64
        && browser.starts_with(|c: char| c.is_ascii_lowercase())
        && browser
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !revision.is_empty()
        && revision.chars().all(|c| c.is_ascii_digit())
}

/// The fetch itself: Playwright's own installer at the version asked for.
pub fn fetch_argv(browser: &str, version: &str) -> Vec<String> {
    vec![
        "npx".into(),
        "-y".into(),
        format!("playwright@{version}"),
        "install".into(),
        browser.into(),
    ]
}

/// The script that runs `fetch` in a namespace of its own, which sees nothing of the fleet and
/// writes nothing but `stage`.
///
/// **What is in it**: `/usr`, `/etc` and the merged-usr links read-only, a fresh `/proc`, `/dev` and
/// `/tmp`, the network (it has to reach npm and Playwright's CDN), and `stage` read-write at
/// `/stage`, a path that names nothing of the fleet: bound where it really is, under the fleet root,
/// the namespace would hold an empty skeleton of the fleet root's own path. **What is
/// not**: the fleet root, so no box's directory, no `.skein` and no `.skein/private`; `/run`, so no
/// `docker.sock` and no agent sockets; the sandbox's home, so no login, token or npm config. The
/// environment is cleared and given only the proxy and certificate names the sandbox's network
/// needs, which name a proxy and a CA file and are no credential. `--unshare-pid` and
/// `--die-with-parent`, unlike a box, so nothing it starts outlives it: the check that follows must
/// not race a writer.
///
/// `resolv.conf` is bound where it really is when it is a link out of `/etc`, or names stop
/// resolving. A CA file outside `/usr` and `/etc` is bound the same way, read-only.
pub fn namespace_script(stage: &str, fetch: &[String]) -> String {
    let fetch = fetch
        .iter()
        .map(|w| sh_quote(w))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"stage={stage}
set --
for n in HTTPS_PROXY HTTP_PROXY NO_PROXY https_proxy http_proxy no_proxy NODE_USE_ENV_PROXY NODE_EXTRA_CA_CERTS SSL_CERT_FILE; do
  v="$(printenv "$n")" && set -- "$@" --setenv "$n" "$v"
done
for f in "$(printenv NODE_EXTRA_CA_CERTS)" "$(printenv SSL_CERT_FILE)"; do
  case "$f" in /usr/*|/etc/*|"") ;; *) [ -f "$f" ] && set -- "$@" --ro-bind "$f" "$f" ;; esac
done
r="$(readlink -f /etc/resolv.conf)"
case "$r" in /etc/*|"") ;; *) set -- "$@" --ro-bind-try "$r" "$r" ;; esac
mkdir -p "$stage/home" "$stage/out" || exit 1
timeout -k 10 {deadline} bwrap --unshare-all --share-net --unshare-pid --die-with-parent \
  --new-session --clearenv \
  --ro-bind /usr /usr --ro-bind-try /bin /bin --ro-bind-try /lib /lib \
  --ro-bind-try /lib64 /lib64 --ro-bind-try /sbin /sbin --ro-bind /etc /etc \
  --proc /proc --dev /dev --tmpfs /tmp \
  --bind "$stage" /stage --chdir /stage/home \
  --setenv PATH /usr/local/bin:/usr/bin:/bin --setenv HOME /stage/home \
  --setenv npm_config_cache /stage/npm --setenv npm_config_update_notifier false \
  --setenv PLAYWRIGHT_BROWSERS_PATH /stage/out --setenv PLAYWRIGHT_SKIP_BROWSER_GC 1 \
  "$@" -- {fetch} 2>&1"#,
        stage = sh_quote(stage),
        deadline = FILL_DEADLINE.as_secs(),
    )
}

/// What a fill staged, once it has passed [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// The browser directories, by name, sorted.
    pub entries: Vec<String>,
    /// Allocated bytes, as every other disk figure in skein is counted.
    pub bytes: u64,
}

/// Check what a fill staged in `out`, **without following anything**, and refuse it whole on the
/// first thing that is not a plain browser directory.
///
/// Refused: a symlink anywhere (a link in the store would lead every box that runs the browser
/// somewhere the fill chose); anything that is not a regular file or a directory (a device, a pipe,
/// a socket); a file with another hard link (a name for an inode outside the tree); a setuid or
/// setgid bit; anything on another filesystem; a top-level name that is not `<name>-<revision>`
/// (it becomes a path in the store and in every box). Playwright's own `.links` directory is its
/// bookkeeping for the staging directory and is not published, so it is not judged either.
///
/// Measured on 2026-10-29 for chromium, its headless shell and ffmpeg at Playwright 1.62.0: no
/// link, no special file, no hard link, no setuid bit in 1 GB of browser, so a refusal here is a
/// fill that did something a browser download does not.
pub fn verify(out: &Path) -> Result<Staged, String> {
    let top = std::fs::symlink_metadata(out).map_err(|e| format!("nothing was staged ({e})"))?;
    if !top.is_dir() {
        return Err("what was staged is not a directory".into());
    }
    let dev = top.dev();
    let mut entries = Vec::new();
    let mut bytes = 0u64;
    let names = std::fs::read_dir(out).map_err(|e| format!("cannot read what was staged: {e}"))?;
    for item in names {
        let item = item.map_err(|e| format!("cannot read what was staged: {e}"))?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name == ".links" {
            continue;
        }
        if !entry_is_nameable(&name) {
            return Err(format!(
                "the fetch staged {name:?}, which is not a browser directory"
            ));
        }
        let mut stack = vec![out.join(&name)];
        while let Some(path) = stack.pop() {
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let shown = path
                .strip_prefix(out)
                .unwrap_or(&path)
                .display()
                .to_string();
            let kind = meta.file_type();
            if meta.dev() != dev {
                return Err(format!("{shown} is on another filesystem"));
            }
            if kind.is_symlink() {
                return Err(format!("{shown} is a link"));
            }
            if meta.mode() & 0o6000 != 0 {
                return Err(format!("{shown} is setuid or setgid"));
            }
            bytes += meta.blocks() * 512;
            if kind.is_dir() {
                for child in
                    std::fs::read_dir(&path).map_err(|e| format!("cannot read {shown}: {e}"))?
                {
                    let child = child.map_err(|e| format!("cannot read {shown}: {e}"))?;
                    stack.push(child.path());
                }
            } else if kind.is_file() {
                if meta.nlink() != 1 {
                    return Err(format!("{shown} is a hard link"));
                }
            } else {
                return Err(format!(
                    "{shown} is not a file or a directory (a device, pipe or socket)"
                ));
            }
        }
        entries.push(name);
    }
    if entries.is_empty() {
        return Err("the fetch staged no browser".into());
    }
    if bytes > FILL_CAP {
        return Err(format!(
            "the fetch staged {} MB, more than the {} MB one fill may add",
            bytes >> 20,
            FILL_CAP >> 20
        ));
    }
    entries.sort();
    Ok(Staged { entries, bytes })
}

/// Take write permission off every file under `dir`, following nothing, and group and other write
/// off every directory. Execute bits stay: a browser is a program.
///
/// **Directories keep their owner's write bit, deliberately.** What makes the store read-only in a
/// box is the read-only bind, not a mode: every box runs as the uid that owns these files, so a
/// mode is one `chmod` away for anything that can reach them writable. A directory without its
/// write bit would cost something real, though: a plain `rm -rf` of the fleet root, which the
/// sandbox's own teardown and a test fixture's cleanup both are, fails on it.
fn make_read_only(dir: &Path) -> Result<(), String> {
    let set = |path: &Path, mode: u32| -> Result<(), String> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("cannot make {} read-only: {e}", path.display()))
    };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            for child in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
                stack.push(child.map_err(|e| e.to_string())?.path());
            }
            set(&path, (meta.mode() & 0o7777 & !0o022) | 0o700)?;
        } else if meta.is_file() {
            set(&path, meta.mode() & 0o7777 & !0o222)?;
        }
    }
    Ok(())
}

/// Give write permission back to the owner under `dir`, following nothing, so it can be removed.
fn make_writable(dir: &Path) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            let _ = std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode((meta.mode() & 0o7777) | 0o700),
            );
            for child in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                stack.push(child.path());
            }
        }
    }
}

/// Remove `dir` and everything under it, following no link.
fn remove_tree(dir: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dir) {
        Err(_) => Ok(()),
        Ok(meta) if !meta.is_dir() => std::fs::remove_file(dir).map_err(|e| e.to_string()),
        Ok(_) => {
            make_writable(dir);
            std::fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))
        }
    }
}

/// `rename(2)` that refuses to replace anything at `to`: a published entry never changes.
fn rename_no_replace(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|e| e.to_string());
    let (f, t) = (c(from)?, c(to)?);
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

/// Move what [`verify`] passed into `store`, one entry at a time: read-only first, then one atomic
/// rename, so a box sees an entry whole or not at all. An entry the store already has is left as it
/// is and the staged copy dropped. Returns the entries that are new.
pub fn publish(out: &Path, store: &Path, staged: &Staged) -> Result<Vec<String>, String> {
    let mut new = Vec::new();
    for name in &staged.entries {
        let to = store.join(name);
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        let from = out.join(name);
        make_read_only(&from)?;
        match rename_no_replace(&from, &to) {
            Ok(()) => {}
            Err(_) if std::fs::symlink_metadata(&to).is_ok() => continue,
            Err(e) => return Err(format!("cannot publish {name}: {e}")),
        }
        new.push(name.clone());
    }
    Ok(new)
}

/// One fill at a time in this process: a second would only fetch the same thing twice.
static FILLING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The store's size, by the same walk as every other disk figure.
fn store_bytes() -> u64 {
    published()
        .iter()
        .map(|n| super::disk::tree_bytes(&Path::new(&store_dir()).join(n), None))
        .sum()
}

/// The entries in the store, by name, sorted. Anything not shaped like one is not an entry.
fn published() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(store_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| entry_is_nameable(n))
        .collect();
    names.sort();
    names
}

/// Why a fill cannot start now, or `None` when the store has room for one.
fn no_room(used: u64) -> Option<String> {
    (used + FILL_CAP > BUDGET).then(|| {
        format!(
            "The fleet's browser store is full: {} MB of {} MB. New browser requests wait. Clean \
             versions no box uses to make room.",
            used >> 20,
            BUDGET >> 20
        )
    })
}

fn fills() -> std::collections::BTreeMap<String, Vec<String>> {
    crate::util::read_json_or_why(&fills_path())
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Fetch `want` into the store, the way [`crate::substrate::install`] runs an install: what it said,
/// and whether it worked. Production's fetch is [`fetch_argv`].
pub fn fill(sandbox: &str, want: &str) -> Result<String, String> {
    let (browser, version) = want_parts(want).ok_or_else(|| {
        format!("{want:?} is not a browser and a Playwright version, like chromium@1.56.1")
    })?;
    fill_with(sandbox, want, &fetch_argv(browser, version))
}

/// [`fill`] with the fetch passed in, so a test can stage a fixture tree through the real namespace.
pub fn fill_with(sandbox: &str, want: &str, fetch: &[String]) -> Result<String, String> {
    let _one = FILLING.lock().unwrap_or_else(|p| p.into_inner());
    let store = PathBuf::from(store_dir());
    if let Some(had) = fills().get(want) {
        if !had.is_empty() && had.iter().all(|n| store.join(n).is_dir()) {
            return Ok(format!("already in the fleet's store: {}", had.join(", ")));
        }
    }
    if let Some(full) = no_room(store_bytes()) {
        return Err(full);
    }
    let stage = staging_root().join(format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        std::process::id()
    ));
    std::fs::create_dir_all(&stage).map_err(|e| format!("cannot stage a fill: {e}"))?;
    let _ = std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700));
    let outcome = fill_staged(sandbox, want, fetch, &stage, &store);
    let _ = remove_tree(&stage);
    outcome
}

fn fill_staged(
    sandbox: &str,
    want: &str,
    fetch: &[String],
    stage: &Path,
    store: &Path,
) -> Result<String, String> {
    let ran = crate::place::own_sandbox(sandbox).attempt(
        &namespace_script(&stage.display().to_string(), fetch),
        &[],
        FILL_DEADLINE + Duration::from_secs(60),
    )?;
    // Playwright draws a progress bar a line at a time; the lines worth keeping are the others.
    let said: String = String::from_utf8_lossy(&ran.out)
        .lines()
        .filter(|l| !l.contains('|') && !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if ran.code != 0 {
        return Err(format!("{said}\n{}", ran.err).trim().to_string());
    }
    // The namespace has exited and its PID namespace with it, so nothing is writing any more.
    let out = stage.join("out");
    let staged = verify(&out).map_err(|why| format!("refused: {why}"))?;
    std::fs::create_dir_all(store).map_err(|e| e.to_string())?;
    let new = publish(&out, store, &staged)?;
    crate::util::update_json(
        &fills_path(),
        |m: &mut std::collections::BTreeMap<String, Vec<String>>| {
            m.insert(want.to_string(), staged.entries.clone());
            Ok(())
        },
    )?;
    Ok(format!(
        "{said}\nin the fleet's store: {}{}",
        staged.entries.join(", "),
        if new.is_empty() {
            String::new()
        } else {
            format!(" ({} new)", new.len())
        }
    )
    .trim()
    .to_string())
}

/// Leftovers of a fill this process did not finish: a crash or a stop between staging and the
/// removal. Only while holding the fill lock, so never under a fill in progress.
fn sweep_staging() {
    for e in std::fs::read_dir(staging_root())
        .into_iter()
        .flatten()
        .flatten()
    {
        let _ = remove_tree(&e.path());
    }
}

/// **The request pass**: every pending browser request, answered without the owner (SKEIN-1341).
///
/// At most one request per box per pass, oldest first, which is what makes "one outstanding
/// request per box" hold even for a box that wrote its queue directly instead of asking through
/// the launcher, which refuses a second. A request that is not a browser and a version is refused
/// with the reason. While the store has no room, requests stay pending and [`store_view`] says why.
/// Returns a line for each thing it did, for the server's log.
pub fn serve(sandbox: &str) -> Vec<String> {
    let mut said = Vec::new();
    {
        let Ok(_one) = FILLING.try_lock() else {
            return said;
        };
        sweep_staging();
    }
    let Ok(asked) = crate::substrate::list(sandbox) else {
        return said;
    };
    let mut seen = std::collections::BTreeSet::new();
    for r in asked
        .into_iter()
        .filter(|r| r.kind == "playwright" && r.is_pending())
    {
        if !seen.insert(r.box_name.clone()) {
            continue;
        }
        if let Some(why) = r.problem() {
            match crate::substrate::refuse(sandbox, &r, &why) {
                Ok(_) => said.push(format!("refused {}'s browser request: {why}", r.box_name)),
                Err(e) => said.push(format!("{}'s browser request: {e}", r.box_name)),
            }
            continue;
        }
        if no_room(store_bytes()).is_some() {
            continue;
        }
        if let Err(e) = crate::substrate::decide(sandbox, &r, true, false) {
            said.push(format!("{}'s browser request: {e}", r.box_name));
            continue;
        }
        let what = r.packages.join(" ");
        match crate::substrate::install(sandbox, &r.box_name, &r.id) {
            Ok(_) => said.push(format!(
                "fetched {what} for the fleet, asked by {}",
                r.box_name
            )),
            Err(e) => said.push(format!(
                "could not fetch {what}, asked by {}: {}",
                r.box_name,
                e.lines().next().unwrap_or("")
            )),
        }
    }
    said
}

/// One browser directory in the store, for the disk view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Shelf {
    pub name: String,
    pub bytes: u64,
    /// The boxes whose browsers directory links it, running or stopped.
    pub boxes: Vec<String>,
    /// The requests that brought it in, `<browser>@<version>`.
    pub wants: Vec<String>,
}

/// The fleet's browser store, for the disk view.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct StoreView {
    pub entries: Vec<Shelf>,
    pub bytes: u64,
    pub budget: u64,
    /// Why new requests are waiting, when they are.
    pub full: Option<String>,
    /// Browser requests not answered yet, `<box>: <browser>@<version>`.
    pub waiting: Vec<String>,
    /// Boxes whose browsers directory skein could not read without following a link, or which held
    /// more than it reads. Nothing is cleaned while there are any: they might link anything.
    pub unknown: Vec<String>,
}

/// Which store entries each box links, read from outside without following a link: the box's
/// `home`, `.cache` and browsers directory must each be a real directory, and each entry is read
/// with `readlink`. A box with no browsers directory links nothing; one skein cannot read this way
/// is `Err`, and its links are unknown.
fn box_links(store: &str) -> (std::collections::BTreeMap<String, Vec<String>>, Vec<String>) {
    let mut links: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut unknown = Vec::new();
    let root = fleet_root();
    let prefix = format!("{store}/");
    let mut boxes: Vec<String> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.') && crate::util::valid_name(n))
        .collect();
    boxes.sort();
    'boxes: for name in boxes {
        let mut at = Path::new(&root).join(&name);
        for part in std::iter::once("home").chain(FARM.split('/')) {
            match std::fs::symlink_metadata(&at) {
                Ok(m) if m.is_dir() => at.push(part),
                Ok(_) => {
                    unknown.push(name);
                    continue 'boxes;
                }
                Err(_) => continue 'boxes,
            }
        }
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                unknown.push(name);
                continue;
            }
            Err(_) => continue,
        }
        let Ok(items) = std::fs::read_dir(&at) else {
            unknown.push(name);
            continue;
        };
        let mut read = 0usize;
        for item in items.flatten() {
            read += 1;
            if read > FARM_BOUND {
                unknown.push(name.clone());
                continue 'boxes;
            }
            let Ok(target) = std::fs::read_link(item.path()) else {
                continue;
            };
            if let Some(entry) = target.to_str().and_then(|t| t.strip_prefix(&prefix)) {
                let entry = entry.trim_end_matches('/');
                if entry_is_nameable(entry) {
                    links
                        .entry(entry.to_string())
                        .or_default()
                        .push(name.clone());
                }
            }
        }
    }
    (links, unknown)
}

/// What the store holds, who links what, and whether requests are waiting and why.
pub fn store_view(sandbox: &str) -> StoreView {
    let store = store_dir();
    let (links, unknown) = box_links(&store);
    let wants = fills();
    let entries: Vec<Shelf> = published()
        .into_iter()
        .map(|name| Shelf {
            bytes: super::disk::tree_bytes(&Path::new(&store).join(&name), None),
            boxes: links.get(&name).cloned().unwrap_or_default(),
            wants: wants
                .iter()
                .filter(|(_, e)| e.contains(&name))
                .map(|(w, _)| w.clone())
                .collect(),
            name,
        })
        .collect();
    let bytes = entries.iter().map(|e| e.bytes).sum();
    let waiting: Vec<String> = crate::substrate::list(sandbox)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.kind == "playwright" && (r.is_pending() || r.state == "approved"))
        .map(|r| format!("{}: {}", r.box_name, r.packages.join(" ")))
        .collect();
    StoreView {
        full: no_room(bytes).filter(|_| !waiting.is_empty()),
        entries,
        bytes,
        budget: BUDGET,
        waiting,
        unknown,
    }
}

/// What [`clean_unused`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Cleaned {
    pub removed: Vec<String>,
    pub freed: u64,
}

/// Remove the store entries in `names`, each only if no box links it **at the moment of removal**.
///
/// The cockpit offers this and the person confirms it; nothing calls it on its own. Every name must
/// still be an entry no box links, or nothing is removed: the person confirmed a list, and a list
/// that changed is not the one they confirmed. While any box's links cannot be read, nothing is
/// removed at all, since that box might link anything. Each entry is renamed out of the store
/// first, so a box starting now either links it whole or finds it gone (and the kit drops the
/// dangling link at its next start, and Playwright downloads privately meanwhile).
pub fn clean_unused(names: &[String]) -> Result<Cleaned, String> {
    if names.is_empty() {
        return Err("nothing was chosen to remove".into());
    }
    let _one = FILLING.lock().unwrap_or_else(|p| p.into_inner());
    let store = store_dir();
    let (links, unknown) = box_links(&store);
    if !unknown.is_empty() {
        return Err(format!(
            "skein cannot read which browsers {} links, so it removed nothing. Their \
             ~/.cache/ms-playwright must be a plain directory. Fix that, then clean again.",
            unknown.join(", ")
        ));
    }
    let have = published();
    let changed: Vec<&String> = names
        .iter()
        .filter(|n| !have.contains(n) || links.contains_key(*n))
        .collect();
    if !changed.is_empty() {
        return Err(format!(
            "The store changed since you looked, so skein removed nothing. Open the view again \
             to see what can be cleaned now. (No longer unused: {})",
            changed
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let trash = staging_root().join(format!("clean-{}", std::process::id()));
    std::fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    let mut done = Cleaned::default();
    for name in names {
        let from = Path::new(&store).join(name);
        let bytes = super::disk::tree_bytes(&from, None);
        let to = trash.join(name);
        make_writable(&from);
        std::fs::rename(&from, &to).map_err(|e| format!("cannot remove {name}: {e}"))?;
        done.removed.push(name.clone());
        done.freed += bytes;
    }
    remove_tree(&trash)?;
    crate::util::update_json(
        &fills_path(),
        |m: &mut std::collections::BTreeMap<String, Vec<String>>| {
            m.retain(|_, e| !e.iter().any(|n| names.contains(n)));
            Ok(())
        },
    )?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, env_pins, tempdir};

    fn fixture_entry(out: &Path, name: &str) {
        std::fs::create_dir_all(out.join(name).join("chrome-linux")).unwrap();
        std::fs::write(out.join(name).join("INSTALLATION_COMPLETE"), "").unwrap();
        std::fs::write(out.join(name).join("chrome-linux/chrome"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            out.join(name).join("chrome-linux/chrome"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    /// **A bad version or browser name is refused, by the shape check every path shares.**
    ///
    /// What would make it fail: dropping the `BROWSERS.contains` check (a made-up browser passes);
    /// loosening `version_is_nameable` to any dotted string (`1.56.1 --foo`, `1.56.1-beta`, `latest`
    /// pass); accepting a leading zero or a `2.x`.
    #[test]
    fn a_request_naming_anything_but_a_browser_and_a_playwright_version_is_refused() {
        for good in [
            "chromium@1.56.1",
            "chromium-headless-shell@1.62.0",
            "firefox@1.20.0",
            "webkit@1.64.10",
            "ffmpeg@1.62.0",
        ] {
            assert!(want_is_nameable(good), "{good} was refused");
        }
        for bad in [
            "chromium",
            "chromium@",
            "@1.56.1",
            "chrome@1.56.1",
            "msedge@1.56.1",
            "Chromium@1.56.1",
            "chromium@latest",
            "chromium@1.56",
            "chromium@1.56.1.2",
            // A pre-release, spelled in two halves: its text reads as an address to the residue gate.
            concat!("chromium@1.56.1-", "beta-1"),
            "chromium@1.056.1",
            "chromium@01.56.1",
            "chromium@2.0.0",
            "chromium@1.19.0",
            "chromium@1.56.1 --with-deps",
            "chromium@1.56.1;id",
            "chromium@1.56.1@1.56.1",
            "../chromium@1.56.1",
            "chromium@1.99999.1",
        ] {
            assert!(!want_is_nameable(bad), "{bad:?} was accepted");
        }
    }

    /// **The package queue takes a browser request only in that shape, one browser at a time, and
    /// never writes one into the manifest a rebuilt sandbox reinstalls from.**
    ///
    /// What would make it fail: `package_is_nameable` sending `playwright` to the apt rule (which
    /// refuses the `@`, so the good one fails); dropping the one-browser check; `merged` without its
    /// kind guard (the browser lands in the apt list, which runs as root at every launch).
    #[test]
    fn the_package_queue_takes_one_browser_in_the_strict_shape_and_never_remembers_it() {
        let ask = |packages: &[&str]| crate::substrate::Request {
            id: "20261029-100000-1".into(),
            box_name: "web-main".into(),
            kind: "playwright".into(),
            packages: packages.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        };
        assert_eq!(ask(&["chromium@1.62.0"]).problem(), None);
        assert!(ask(&["chromium@latest"]).problem().is_some());
        assert!(ask(&["chrome@1.62.0"]).problem().is_some());
        assert!(ask(&["chromium@1.62.0", "firefox@1.62.0"])
            .problem()
            .is_some());
        let m = crate::substrate::merged(Default::default(), &ask(&["chromium@1.62.0"]));
        assert!(m.apt.is_empty() && m.npm.is_empty(), "{m:?}");
    }

    /// **A fill starts only while the store has room for the largest one under its budget.**
    ///
    /// What would make it fail: `no_room` comparing without `FILL_CAP`, so a store one byte under
    /// the budget starts a fill that takes it a gigabyte over.
    #[test]
    fn a_fill_waits_while_the_store_has_no_room_for_one_more() {
        assert_eq!(no_room(BUDGET - FILL_CAP), None);
        let full = no_room(BUDGET - FILL_CAP + 1).expect("no room");
        assert!(full.contains("Clean versions no box uses"), "{full}");
    }

    #[test]
    fn only_a_playwright_browser_directory_name_is_an_entry() {
        for good in [
            "chromium-1234",
            "chromium_headless_shell-1234",
            "ffmpeg-1011",
            "webkit-2215",
        ] {
            assert!(entry_is_nameable(good), "{good}");
        }
        for bad in [
            "chromium",
            "-1234",
            ".links",
            ".staging",
            "chromium-",
            "chromium-12a",
            "Chromium-1",
            "chro mium-1",
            "chromium-1/..",
            "..-1",
        ] {
            assert!(!entry_is_nameable(bad), "{bad:?}");
        }
    }

    /// **A fill that stages a symlink, a special file, a hard link or a name that is not a browser
    /// directory is refused whole, and the store is untouched.**
    ///
    /// What would make it fail: `verify` reading `metadata` (which follows a link) instead of
    /// `symlink_metadata`, so the link is read as the directory it leads to; the `else` that refuses
    /// what is neither a file nor a directory; the `nlink` check; the top-level name check. The
    /// clean staging passes, so a refusal is about the planted thing and not about the fixture.
    #[test]
    fn a_fill_that_stages_a_link_a_device_or_a_path_escape_is_refused() {
        let dir = tempdir();
        let fresh = |tag: &str| {
            let out = dir.join(tag);
            fixture_entry(&out, "chromium-1234");
            fixture_entry(&out, "ffmpeg-1011");
            std::fs::create_dir_all(out.join(".links")).unwrap();
            out
        };
        let clean = fresh("clean");
        let staged = verify(&clean).expect("a plain browser tree passes");
        assert_eq!(staged.entries, ["chromium-1234", "ffmpeg-1011"]);

        let link = fresh("link");
        std::os::unix::fs::symlink("/etc", link.join("chromium-1234/chrome-linux/lib")).unwrap();
        let why = verify(&link).expect_err("a link was staged");
        assert!(why.contains("is a link"), "{why}");

        let top_link = fresh("toplink");
        std::os::unix::fs::symlink("/usr", top_link.join("webkit-1")).unwrap();
        assert!(verify(&top_link)
            .expect_err("a link entry")
            .contains("is a link"));

        let pipe = fresh("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(pipe.join("chromium-1234/chrome-linux/fifo"))
            .status()
            .unwrap();
        assert!(made.success());
        let why = verify(&pipe).expect_err("a pipe was staged");
        assert!(why.contains("not a file or a directory"), "{why}");

        // A socket, the third special kind. A device cannot be made without root, which a test does
        // not have; it takes the same branch as these two.
        let sock = fresh("sock");
        let _listener = std::os::unix::net::UnixListener::bind(sock.join("ffmpeg-1011/s")).unwrap();
        let why = verify(&sock).expect_err("a socket was staged");
        assert!(why.contains("not a file or a directory"), "{why}");

        let hard = fresh("hard");
        std::fs::hard_link(
            hard.join("chromium-1234/chrome-linux/chrome"),
            hard.join("ffmpeg-1011/chrome"),
        )
        .unwrap();
        assert!(verify(&hard)
            .expect_err("a hard link")
            .contains("hard link"));

        let named = fresh("named");
        std::fs::create_dir_all(named.join("..hidden-1")).unwrap();
        assert!(verify(&named)
            .expect_err("a name that is not an entry")
            .contains("not a browser directory"));

        let setuid = fresh("setuid");
        std::fs::set_permissions(
            setuid.join("chromium-1234/chrome-linux/chrome"),
            std::fs::Permissions::from_mode(0o4755),
        )
        .unwrap();
        assert!(verify(&setuid).expect_err("setuid").contains("setuid"));
    }

    /// **What is published is read-only, whole, and never replaced.**
    ///
    /// What would make it fail: `publish` skipping `make_read_only` (a file stays writable); a
    /// `rename` that replaces, so a second fill of the same revision overwrites the first (the
    /// marker would change).
    #[test]
    fn a_published_entry_is_read_only_and_never_replaced() {
        let dir = tempdir();
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let out = dir.join("out");
        fixture_entry(&out, "chromium-1234");
        let staged = verify(&out).unwrap();
        assert_eq!(publish(&out, &store, &staged).unwrap(), ["chromium-1234"]);
        let file = store.join("chromium-1234/chrome-linux/chrome");
        let mode = std::fs::metadata(&file).unwrap().mode();
        assert_eq!(mode & 0o222, 0, "published file is writable: {mode:o}");
        assert_eq!(
            mode & 0o111,
            0o111,
            "published browser lost its execute bit"
        );
        let top = std::fs::metadata(store.join("chromium-1234"))
            .unwrap()
            .mode();
        assert_eq!(top & 0o022, 0, "the entry is writable by others: {top:o}");

        let again = dir.join("again");
        fixture_entry(&again, "chromium-1234");
        std::fs::write(again.join("chromium-1234/INSTALLATION_COMPLETE"), "second").unwrap();
        let staged = verify(&again).unwrap();
        assert!(publish(&again, &store, &staged).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(store.join("chromium-1234/INSTALLATION_COMPLETE")).unwrap(),
            "",
            "a second fill replaced a published entry"
        );
        make_writable(&dir);
    }

    /// **The clean removes only what no box links, re-checked at the moment of removal.**
    ///
    /// What would make it fail: `clean_unused` not re-reading the links (it removes an entry a box
    /// linked after the view was drawn); `box_links` following a box's link instead of reading it
    /// (a box whose browsers directory is a link would be read as linking nothing).
    #[test]
    fn only_what_no_box_links_is_cleaned() {
        let _g = env_lock();
        let dir = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"))
            .set("SKEIN_HOME", dir.join("home"));
        let store = PathBuf::from(store_dir());
        std::fs::create_dir_all(&store).unwrap();
        let out = dir.join("out");
        fixture_entry(&out, "chromium-1");
        fixture_entry(&out, "chromium-2");
        let staged = verify(&out).unwrap();
        publish(&out, &store, &staged).unwrap();
        let farm = dir.join("fleet/web-main/home").join(FARM);
        std::fs::create_dir_all(&farm).unwrap();
        std::os::unix::fs::symlink(store.join("chromium-1"), farm.join("chromium-1")).unwrap();

        let view = store_view("no-such-sandbox");
        let by = |n: &str| view.entries.iter().find(|e| e.name == n).unwrap().clone();
        assert_eq!(by("chromium-1").boxes, ["web-main"]);
        assert!(by("chromium-2").boxes.is_empty());

        let why = clean_unused(&["chromium-1".into(), "chromium-2".into()])
            .expect_err("a linked entry was offered");
        assert!(why.contains("No longer unused: chromium-1"), "{why}");
        assert!(
            store.join("chromium-2").is_dir(),
            "a refused clean removed something"
        );

        // A box whose browsers directory is a link is not read through, and blocks the clean.
        let other = dir.join("fleet/api-main/home").join(".cache");
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&farm, other.join("ms-playwright")).unwrap();
        let why = clean_unused(&["chromium-2".into()]).expect_err("an unreadable box");
        assert!(why.contains("api-main"), "{why}");
        std::fs::remove_file(other.join("ms-playwright")).unwrap();

        let done = clean_unused(&["chromium-2".into()]).expect("unused is cleaned");
        assert_eq!(done.removed, ["chromium-2"]);
        assert!(!store.join("chromium-2").exists());
        assert!(store.join("chromium-1").is_dir());
        make_writable(&dir);
    }
}
