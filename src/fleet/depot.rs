//! The fleet depot: one read-only copy of each version of an artifact, for every box, filled by
//! skein and by no box (SKEIN-1372, `docs/design/box-environment.md` §6).
//!
//! SKEIN-1341 built this shape once for Playwright browsers (`fleet::browsers`). This is the same
//! mechanism with the browser taken out, so that each further kind of artifact is a [`Kind`]
//! declaration and a fetch, and nothing else:
//!
//!   * **The kind's directory**, [`kind_dir`]: `<fleet root>/.skein/depot/<kind>/<entry>`, one
//!     directory per version. The launcher binds what a box uses read-only, and a covered box sees
//!     all of `.skein` read-only besides.
//!   * **The fill**, [`fill_with`]: the kind's fetch runs in a throwaway bwrap namespace
//!     ([`namespace_script`]) that sees `/usr` and `/etc` read-only, the network, one staging
//!     directory, and nothing of the fleet: no fleet root, no `/run`, no sandbox home, a cleared
//!     environment, its own PID namespace, ended with its parent.
//!   * **The check**, [`verify`]: once that namespace has exited, the staged tree is walked without
//!     following anything, and refused whole on the first thing that is not a plain file, a plain
//!     directory, or (for a kind that declares them) a relative link that stays inside its entry.
//!   * **The publish**, [`publish`]: files made read-only, then one `renameat2(RENAME_NOREPLACE)`
//!     per entry, so a box sees an entry whole or not at all and a published entry never changes.
//!   * **The clean**, [`remove_unused`]: removing entries is offered and confirmed, never automatic,
//!     and every name is checked again, at the moment of removal, against what is in use.
//!
//! Which entry a box uses, and when that changes, is not this module's business: that is the
//! kind's (`fleet::versions` for agent CLIs and Rust toolchains). This module only guarantees that
//! what it publishes is what was checked, and that it never changes afterwards.

use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// What a kind may hold as a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Links {
    /// No link anywhere: a browser download has none (measured, SKEIN-1341), so one is a fill
    /// that did something a download does not.
    Refused,
    /// A link whose target is relative and stays inside its own entry, read without following it.
    /// An npm prefix needs these: `bin/claude` is `../lib/node_modules/@anthropic-ai/claude-code/
    /// bin/claude.exe`, and that is how npm says which file the command is.
    Internal,
}

/// One kind of artifact the depot holds: a declaration, not machinery.
#[derive(Debug, Clone, Copy)]
pub struct Kind {
    /// The directory under `.skein/depot`, and the word the cockpit uses.
    pub name: &'static str,
    /// Whether a top-level name is the shape of one of this kind's entries. It becomes a path in
    /// the depot and in every box, so it is checked as strictly as a path component must be.
    pub entry_is_nameable: fn(&str) -> bool,
    pub links: Links,
    /// The most one fill may add.
    pub fill_cap: u64,
    /// The kind's share of the disk. A fill starts only while there is room for `fill_cap` under it.
    pub budget: u64,
    /// How long one fill may take before it is stopped.
    pub deadline: Duration,
}

/// Where the depot is, inside the sandbox and, read-only, inside every box.
pub fn depot_dir() -> String {
    format!("{}/.skein/depot", fleet_root())
}

/// One kind's directory: its entries, and its staging.
pub fn kind_dir(kind: &Kind) -> PathBuf {
    Path::new(&depot_dir()).join(kind.name)
}

/// Where a fill stages, **inside the kind's directory**, so the publish is a rename within one
/// filesystem and the read-only bind of the depot covers staging too. Hidden, so it is never an
/// entry.
fn staging_root(kind: &Kind) -> PathBuf {
    kind_dir(kind).join(".staging")
}

/// What a fill runs, and what it is given.
#[derive(Debug, Clone, Default)]
pub struct Fill {
    /// The command, run with `/stage` as the only writable path.
    pub fetch: Vec<String>,
    /// Variables set for it, beyond `PATH` and `HOME` (`/stage/home`).
    pub env: Vec<(String, String)>,
    /// Paths bound read-only into the namespace, `(source in the sandbox, path inside)`. For a
    /// fetch that needs a tool the sandbox has and `/usr` does not, such as rustup.
    pub read_only: Vec<(String, String)>,
    /// Where under `/stage` the fetch leaves the entries, one directory per entry.
    pub out: String,
}

/// The script that runs a fill's fetch in a namespace of its own, which sees nothing of the fleet
/// and writes nothing but `stage`.
///
/// The same namespace `fleet::browsers` fills in (SKEIN-1341), with the fetch, its variables and
/// its read-only extras as arguments. **What is in it**: `/usr`, `/etc` and the merged-usr links
/// read-only, a fresh `/proc`, `/dev` and `/tmp`, the network, and `stage` read-write at `/stage`.
/// **What is not**: the fleet root, so no box and no `.skein`; `/run`, so no `docker.sock`; the
/// sandbox's home, so no login, token or npm config. The environment is cleared, then given the
/// proxy and certificate names the sandbox's network needs. `--unshare-pid` and
/// `--die-with-parent`, so nothing it starts outlives it and the check that follows races no writer.
pub fn namespace_script(stage: &str, fill: &Fill, deadline: Duration) -> String {
    let fetch = fill
        .fetch
        .iter()
        .map(|w| sh_quote(w))
        .collect::<Vec<_>>()
        .join(" ");
    let mut extra = String::new();
    for (k, v) in &fill.env {
        extra.push_str(&format!(" --setenv {} {}", sh_quote(k), sh_quote(v)));
    }
    for (from, to) in &fill.read_only {
        extra.push_str(&format!(" --ro-bind {} {}", sh_quote(from), sh_quote(to)));
    }
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
mkdir -p "$stage/home" "$stage/{out}" || exit 1
timeout -k 10 {deadline} bwrap --unshare-all --share-net --unshare-pid --die-with-parent \
  --new-session --clearenv \
  --ro-bind /usr /usr --ro-bind-try /bin /bin --ro-bind-try /lib /lib \
  --ro-bind-try /lib64 /lib64 --ro-bind-try /sbin /sbin --ro-bind /etc /etc \
  --proc /proc --dev /dev --tmpfs /tmp \
  --bind "$stage" /stage --chdir /stage/home \
  --setenv PATH /usr/local/bin:/usr/bin:/bin --setenv HOME /stage/home{extra} \
  "$@" -- {fetch} 2>&1"#,
        stage = sh_quote(stage),
        out = fill.out,
        deadline = deadline.as_secs(),
    )
}

/// What a fill staged, once it has passed [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// The entries, by name, sorted.
    pub entries: Vec<String>,
    /// Allocated bytes, as every other disk figure in skein is counted.
    pub bytes: u64,
}

/// Is `target`, a link found at `link` (relative to its entry), a relative path that stays inside
/// that entry? Decided on the text alone, never by following it: `..` may not climb above the
/// entry's top at any point.
fn link_stays_inside(link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    // Where the link sits, as components below the entry's top; the link's own name is not one.
    let mut depth: i64 = link.components().count() as i64 - 1;
    for part in target.components() {
        match part {
            std::path::Component::ParentDir => depth -= 1,
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            _ => return false,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

/// Check what a fill staged in `out`, **without following anything**, and refuse it whole on the
/// first thing that is not part of a plain entry of `kind`.
///
/// Refused: a link, unless the kind declares [`Links::Internal`] and the link is relative and stays
/// inside its entry (an entry's top is never a link); anything that is not a regular file, a
/// directory or such a link (a device, a pipe, a socket); a file with another hard link (a name
/// for an inode outside the tree); a setuid or setgid bit; anything on another filesystem; a
/// top-level name that is not the shape of an entry; and more than the kind's fill cap.
pub fn verify(out: &Path, kind: &Kind) -> Result<Staged, String> {
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
        if !(kind.entry_is_nameable)(&name) {
            return Err(format!(
                "the fetch staged {name:?}, which is not the name of a {} entry",
                kind.name
            ));
        }
        let entry = out.join(&name);
        if !std::fs::symlink_metadata(&entry).is_ok_and(|m| m.is_dir()) {
            return Err(format!("{name} is not a directory"));
        }
        let mut stack = vec![entry.clone()];
        while let Some(path) = stack.pop() {
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let shown = path
                .strip_prefix(out)
                .unwrap_or(&path)
                .display()
                .to_string();
            let kind_of = meta.file_type();
            if meta.dev() != dev {
                return Err(format!("{shown} is on another filesystem"));
            }
            if kind_of.is_symlink() {
                let target = std::fs::read_link(&path)
                    .map_err(|e| format!("cannot read the link {shown}: {e}"))?;
                let inside = path.strip_prefix(&entry).unwrap_or(&path);
                if kind.links == Links::Internal && link_stays_inside(inside, &target) {
                    continue;
                }
                return Err(format!("{shown} is a link"));
            }
            if meta.mode() & 0o6000 != 0 {
                return Err(format!("{shown} is setuid or setgid"));
            }
            bytes += meta.blocks() * 512;
            if kind_of.is_dir() {
                for child in
                    std::fs::read_dir(&path).map_err(|e| format!("cannot read {shown}: {e}"))?
                {
                    let child = child.map_err(|e| format!("cannot read {shown}: {e}"))?;
                    stack.push(child.path());
                }
            } else if kind_of.is_file() {
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
        return Err(format!("the fetch staged no {} entry", kind.name));
    }
    if bytes > kind.fill_cap {
        return Err(format!(
            "the fetch staged {} MB, more than the {} MB one fill may add",
            bytes >> 20,
            kind.fill_cap >> 20
        ));
    }
    entries.sort();
    Ok(Staged { entries, bytes })
}

/// Take write permission off every file under `dir`, following nothing, and group and other write
/// off every directory. Execute bits stay. Directories keep their owner's write bit, for the reason
/// `fleet::browsers` gives: what makes the depot read-only in a box is the bind, and a directory
/// without it would make a plain `rm -rf` of the fleet root fail.
pub(crate) fn make_read_only(dir: &Path) -> Result<(), String> {
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
pub(crate) fn make_writable(dir: &Path) {
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
pub(crate) fn remove_tree(dir: &Path) -> Result<(), String> {
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
pub(crate) fn rename_no_replace(from: &Path, to: &Path) -> Result<(), String> {
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

/// Move what [`verify`] passed into `dir`, one entry at a time: read-only first, then one atomic
/// rename. An entry already there is left as it is and the staged copy dropped. Returns the
/// entries that are new.
pub fn publish(out: &Path, dir: &Path, staged: &Staged) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut new = Vec::new();
    for name in &staged.entries {
        let to = dir.join(name);
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

/// The kind's published entries, by name, sorted. A directory that is not the shape of an entry is
/// not one, and neither is anything that is not a directory.
pub fn published(kind: &Kind) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(kind_dir(kind))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| (kind.entry_is_nameable)(n))
        .collect();
    names.sort();
    names
}

/// Is `name` a published entry of `kind`: the right shape, and a real directory, not a link.
pub fn is_published(kind: &Kind, name: &str) -> bool {
    (kind.entry_is_nameable)(name)
        && std::fs::symlink_metadata(kind_dir(kind).join(name)).is_ok_and(|m| m.is_dir())
}

/// One entry's size, by the walk every other disk figure uses.
pub fn entry_bytes(kind: &Kind, name: &str) -> u64 {
    super::disk::tree_bytes(&kind_dir(kind).join(name), None)
}

/// The kind's size.
pub fn kind_bytes(kind: &Kind) -> u64 {
    published(kind).iter().map(|n| entry_bytes(kind, n)).sum()
}

/// Why a fill of `kind` cannot start now, or `None` when there is room for one.
pub fn no_room(kind: &Kind, used: u64) -> Option<String> {
    (used + kind.fill_cap > kind.budget).then(|| {
        format!(
            "The fleet's {} store is full: {} MB of {} MB. New versions wait. Remove versions no \
             box uses to make room.",
            kind.name,
            used >> 20,
            kind.budget >> 20
        )
    })
}

/// One fill at a time in this process, whatever the kind: a fill is minutes of network and disk,
/// and two at once only compete for both.
static FILLING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `fill` for `kind` and publish what it staged. Returns the staged entries, and which of them
/// are new; an entry the depot already has is kept and the staged copy dropped.
pub fn fill_with(sandbox: &str, kind: &Kind, fill: &Fill) -> Result<(Staged, Vec<String>), String> {
    let _one = FILLING.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(full) = no_room(kind, kind_bytes(kind)) {
        return Err(full);
    }
    let stage = staging_root(kind).join(format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        std::process::id()
    ));
    std::fs::create_dir_all(&stage).map_err(|e| format!("cannot stage a fill: {e}"))?;
    let _ = std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700));
    let outcome = fill_staged(sandbox, kind, fill, &stage);
    let _ = remove_tree(&stage);
    outcome
}

fn fill_staged(
    sandbox: &str,
    kind: &Kind,
    fill: &Fill,
    stage: &Path,
) -> Result<(Staged, Vec<String>), String> {
    let ran = crate::place::own_sandbox(sandbox).attempt(
        &namespace_script(&stage.display().to_string(), fill, kind.deadline),
        &[],
        kind.deadline + Duration::from_secs(60),
    )?;
    if ran.code != 0 {
        let said = String::from_utf8_lossy(&ran.out);
        let tail: Vec<&str> = said.lines().rev().take(15).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(format!("{}\n{}", tail.join("\n"), ran.err)
            .trim()
            .to_string());
    }
    // The namespace has exited and its PID namespace with it, so nothing is writing any more.
    let out = stage.join(&fill.out);
    let staged = verify(&out, kind).map_err(|why| format!("refused: {why}"))?;
    let new = publish(&out, &kind_dir(kind), &staged)?;
    Ok((staged, new))
}

/// Leftovers of a fill this process did not finish. Only while holding the fill lock, so never
/// under a fill in progress.
pub fn sweep_staging(kind: &Kind) {
    let Ok(_one) = FILLING.try_lock() else {
        return;
    };
    for e in std::fs::read_dir(staging_root(kind))
        .into_iter()
        .flatten()
        .flatten()
    {
        let _ = remove_tree(&e.path());
    }
}

/// What [`remove_unused`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Cleaned {
    pub removed: Vec<String>,
    pub freed: u64,
}

/// Remove the entries of `kind` in `names`, each only if `kept` does not hold it **at the moment of
/// removal**.
///
/// `kept` is the kind's own answer to "what must stay": every entry a box uses, and whatever the
/// kind keeps besides (the current version, the one for rollback). `Err` from it means it cannot
/// say, and then nothing is removed at all. Every name must still be removable, or nothing is: the
/// person confirmed a list, and a list that changed is not the one they confirmed. Each entry is
/// renamed out of the kind's directory first, so a box starting now finds it whole or gone.
pub fn remove_unused(
    kind: &Kind,
    names: &[String],
    kept: impl FnOnce() -> Result<std::collections::BTreeSet<String>, String>,
) -> Result<Cleaned, String> {
    if names.is_empty() {
        return Err("nothing was chosen to remove".into());
    }
    let _one = FILLING.lock().unwrap_or_else(|p| p.into_inner());
    let kept = kept()?;
    let have = published(kind);
    let refused: Vec<&String> = names
        .iter()
        .filter(|n| !have.contains(n) || kept.contains(*n))
        .collect();
    if !refused.is_empty() {
        return Err(format!(
            "skein removed nothing: {} {} in use, or no longer there. Open the view again to see \
             what can be removed now.",
            refused
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if refused.len() == 1 { "is" } else { "are" }
        ));
    }
    let trash = staging_root(kind).join(format!("clean-{}", std::process::id()));
    std::fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    let mut done = Cleaned::default();
    for name in names {
        let from = kind_dir(kind).join(name);
        let bytes = super::disk::tree_bytes(&from, None);
        make_writable(&from);
        std::fs::rename(&from, trash.join(name))
            .map_err(|e| format!("cannot remove {name}: {e}"))?;
        done.removed.push(name.clone());
        done.freed += bytes;
    }
    remove_tree(&trash)?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple(n: &str) -> bool {
        !n.is_empty()
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || "@.-".contains(c))
    }

    const NO_LINKS: Kind = Kind {
        name: "test-plain",
        entry_is_nameable: simple,
        links: Links::Refused,
        fill_cap: 1 << 30,
        budget: 4 << 30,
        deadline: Duration::from_secs(60),
    };
    const LINKS: Kind = Kind {
        links: Links::Internal,
        name: "test-links",
        ..NO_LINKS
    };

    fn prefix(out: &Path, name: &str) {
        let pkg = out.join(name).join("lib/node_modules/@a/cli/bin");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("cli.exe"), "#!/bin/sh\necho 1.0.0\n").unwrap();
        std::fs::create_dir_all(out.join(name).join("bin")).unwrap();
        std::os::unix::fs::symlink(
            "../lib/node_modules/@a/cli/bin/cli.exe",
            out.join(name).join("bin/cli"),
        )
        .unwrap();
    }

    /// **A link inside an entry is accepted only by a kind that declares internal links, and only
    /// when it is relative and does not climb out of its entry.**
    ///
    /// What would make it fail: `link_stays_inside` ignoring `..` (the escape passes), accepting an
    /// absolute target (`/etc` passes), or counting the link's own name as a level (a link at the
    /// entry's top with `../x` passes); `verify` accepting links for [`Links::Refused`].
    #[test]
    fn only_a_relative_link_that_stays_in_its_entry_is_published() {
        let dir = crate::testutil::tempdir();
        let good = dir.join("good");
        prefix(&good, "cli@1.0.0");
        assert_eq!(verify(&good, &LINKS).unwrap().entries, ["cli@1.0.0"]);
        assert!(verify(&good, &NO_LINKS)
            .expect_err("a kind with no links")
            .contains("is a link"));

        for (case, at, target) in [
            ("absolute", "bin/x", "/etc"),
            ("climbs out", "bin/x", "../../other/thing"),
            ("from the top", "x", "../cli@1.0.0"),
        ] {
            let out = dir.join(case.replace(' ', "-"));
            prefix(&out, "cli@1.0.0");
            std::os::unix::fs::symlink(target, out.join("cli@1.0.0").join(at)).unwrap();
            let why = verify(&out, &LINKS).expect_err(case);
            assert!(why.contains("is a link"), "{case}: {why}");
        }
        assert!(link_stays_inside(
            Path::new("bin/cli"),
            Path::new("../lib/node_modules/@a/cli/bin/cli.exe")
        ));
        make_writable(&dir);
    }
}
