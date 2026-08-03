//! Browsing a box's workspace from the cockpit.
//!
//! Reads **the box's own tree** over `sbx exec`, with the path resolved and escape-guarded inside
//! the box rather than here — a symlink pointing out is refused where it can actually be followed.
//! The host clone is a labelled fallback for a box that cannot be asked, never a silent substitute:
//! for a clone-mode box the two are different checkouts on different branches.

use crate::answer::Answer;
use crate::util::*;
use crate::{box_liveness, lookup_dir, sbx_guest_output, valid_name, Liveness};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One entry in a workspace directory listing.
#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// A workspace directory listing. `path` is the normalized workspace-relative dir ("" = root).
#[derive(Debug, Serialize)]
pub struct FileListing {
    pub path: String,
    pub entries: Vec<FileEntry>,
}

/// Cap on file bytes served to the cockpit — larger than any doc/source file a human reads,
/// small enough that a stray binary can't balloon a response. The UI shows a truncation notice.
pub const FILE_READ_CAP: usize = 2 * 1024 * 1024;

/// Resolve `rel` safely inside a box's workspace. Rejects absolute paths and `..` components up
/// front, then canonicalizes and re-checks containment so a symlink inside the tree can't escape
/// it either. Returns (workspace_root, resolved_target).
pub(crate) fn resolve_in_workspace(name: &str, rel: &str) -> Result<(PathBuf, PathBuf), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let dir = lookup_dir(name).ok_or_else(|| format!("no workspace known for {name}"))?;
    let root = PathBuf::from(expand_tilde(&dir))
        .canonicalize()
        .map_err(|e| format!("workspace unavailable: {e}"))?;
    if rel.starts_with('/') || rel.split('/').any(|c| c == "..") {
        return Err("invalid path".into());
    }
    let target = root
        .join(rel)
        .canonicalize()
        .map_err(|_| format!("not found: {rel}"))?;
    if !target.starts_with(&root) {
        return Err("path escapes the workspace".into());
    }
    Ok((root, target))
}

// Reading the box itself, not the host clone. A clone-mode box works on its OWN copy of the repo:
// the host-side `dir` is a different checkout, usually on a different branch, and for a repo whose
// host clone never got a working tree it is empty — which is exactly how "the Files tab shows
// nothing" happened while the agent had a full tree three feet away. So: read the box when the box
// is up, fall back to the host clone when it isn't, and always say which one you got.

/// The guest half of a file operation: resolve `rel` against the box's repo root, refuse anything
/// that escapes it (`realpath` first, then a prefix check — a symlink out is the case that matters),
/// and emit a `SKEIN_FS` status line the host parses.
pub(crate) fn guest_fs_preamble(rel: &str) -> String {
    format!(
        "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; \
         target=\"$(realpath -m \"$root/{}\" 2>/dev/null)\"; \
         case \"$target\" in \"$root\"|\"$root\"/*) ;; *) echo 'SKEIN_FS ESCAPE'; exit 0 ;; esac; ",
        sh_quote(rel).trim_matches('\'')
    )
}

/// One `SKEIN_FS <word> …` status line, then the payload.
pub(crate) fn split_guest_fs(raw: &str) -> Result<(String, String), String> {
    let (head, body) = raw.split_once('\n').unwrap_or((raw.trim_end(), ""));
    let rest = head
        .trim()
        .strip_prefix("SKEIN_FS ")
        .ok_or("the box did not answer with a listing")?;
    match rest.split_once(' ').unwrap_or((rest, "")) {
        ("OK", detail) => Ok((detail.to_string(), body.to_string())),
        ("ESCAPE", _) => Err("path escapes the workspace".into()),
        ("NOTDIR", _) => Err("not a directory".into()),
        ("NOTFILE", _) => Err("not a file".into()),
        (other, _) => Err(format!("the box could not read that ({other})")),
    }
}

/// Parse `find -printf '%y\t%s\t%f\n'` output into entries. `%y` is the type of the entry itself and
/// `%Y` the type after following a symlink; we ask for both so a linked directory reads as one while
/// a link pointing nowhere still appears instead of vanishing.
pub(crate) fn parse_guest_listing(body: &str) -> Vec<FileEntry> {
    body.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            let own = parts.next()?;
            let followed = parts.next()?;
            let size: u64 = parts.next()?.parse().unwrap_or(0);
            let name = parts.next()?.to_string();
            (name != ".git" && !name.is_empty()).then_some(FileEntry {
                name,
                dir: followed == "d" || (followed.is_empty() && own == "d"),
                size,
            })
        })
        .collect()
}

pub(crate) fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// List a directory inside the box itself. `Err` when the box can't be asked — the caller then
/// falls back to the host clone rather than showing nothing.
pub(crate) fn list_files_in_box(name: &str, rel: &str) -> Result<FileListing, String> {
    let script = format!(
        "{}[ -d \"$target\" ] || {{ echo 'SKEIN_FS NOTDIR'; exit 0; }}; \
         printf 'SKEIN_FS OK\\n'; \
         find \"$target\" -maxdepth 1 -mindepth 1 -printf '%y\\t%Y\\t%s\\t%f\\n' 2>/dev/null",
        guest_fs_preamble(rel)
    );
    let raw = sbx_guest_output(name, &script, Duration::from_secs(20))?;
    let (_, body) = split_guest_fs(&raw)?;
    let mut entries = parse_guest_listing(&body);
    sort_entries(&mut entries);
    Ok(FileListing {
        path: rel.trim_matches('/').to_string(),
        entries,
    })
}

/// Like [`sbx_guest_output`] but keeps stdout as BYTES. Images and PDFs come through here; a lossy
/// UTF-8 conversion would silently corrupt every one of them.
pub(crate) fn sbx_guest_bytes(
    name: &str,
    shell: &str,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new("sbx");
    command.args(["exec", name, "bash", "-lc", shell]);
    let out = bounded_output(&mut command, "sbx exec", timeout)?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("sbx exec exited {}", out.status)
        } else {
            detail
        });
    }
    Ok(out.stdout)
}

/// Read a file inside the box. The status line comes first as text, the file's raw bytes after it.
pub(crate) fn read_file_in_box(name: &str, rel: &str) -> Result<(Vec<u8>, bool), String> {
    let script = format!(
        "{}[ -f \"$target\" ] || {{ echo 'SKEIN_FS NOTFILE'; exit 0; }}; \
         printf 'SKEIN_FS OK %s\\n' \"$(wc -c <\"$target\")\"; \
         head -c {} \"$target\"",
        guest_fs_preamble(rel),
        FILE_READ_CAP
    );
    let raw = sbx_guest_bytes(name, &script, Duration::from_secs(30))?;
    let split = raw.iter().position(|b| *b == b'\n').unwrap_or(raw.len());
    let header = String::from_utf8_lossy(&raw[..split]).into_owned();
    let (size, _) = split_guest_fs(&header)?;
    let bytes = raw.get(split + 1..).unwrap_or(&[]).to_vec();
    let truncated = size.trim().parse::<usize>().unwrap_or(0) > FILE_READ_CAP;
    Ok((bytes, truncated))
}

/// List a directory inside a box's workspace — dirs first, then files, both case-insensitively
/// alphabetical. `.git` is omitted (never what a doc-reading dev wants and enormous); other
/// dotfiles show, because .env/.github/.claude are exactly the things people check.
pub(crate) fn list_host_files(name: &str, rel: &str) -> Result<FileListing, String> {
    let (root, dir) = resolve_in_workspace(name, rel)?;
    if !dir.is_dir() {
        return Err("not a directory".into());
    }
    let mut entries: Vec<FileEntry> = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != ".git")
        .filter_map(|e| {
            // follow symlinks for the type: a linked directory (skein's own `.claude` store link is
            // one) must read as a directory, not as a few-byte "file". A broken link falls back to
            // the link's own metadata so it still appears rather than vanishing.
            let md = fs::metadata(e.path()).or_else(|_| e.metadata()).ok()?;
            Some(FileEntry {
                name: e.file_name().to_string_lossy().into_owned(),
                dir: md.is_dir(),
                size: md.len(),
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let path = dir
        .strip_prefix(&root)
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .into_owned();
    Ok(FileListing { path, entries })
}

/// Read a file from the box's HOST-side clone, capped at FILE_READ_CAP. Returns (bytes, truncated).
pub(crate) fn read_host_file(name: &str, rel: &str) -> Result<(Vec<u8>, bool), String> {
    use std::io::Read as _;
    let (_, file) = resolve_in_workspace(name, rel)?;
    if !file.is_file() {
        return Err("not a file".into());
    }
    let len = fs::metadata(&file).map_err(|e| e.to_string())?.len();
    let truncated = len as usize > FILE_READ_CAP;
    let mut buf = Vec::with_capacity(len.min(FILE_READ_CAP as u64) as usize);
    fs::File::open(&file)
        .map_err(|e| e.to_string())?
        .take(FILE_READ_CAP as u64)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok((buf, truncated))
}

/// List a directory for the cockpit: the box's own tree when the box is up, the host clone when it
/// isn't. Falling back is not a silent substitution — the listing says which tree answered, because
/// for a clone-mode box those are different branches and one of them may have no working tree at all.
pub fn list_box_files(name: &str, rel: &str) -> Result<Answer<FileListing>, String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    if box_liveness(name) == Some(Liveness::Running) {
        match list_files_in_box(name, rel) {
            Ok(listing) => return Ok(annotate(Answer::from_box(listing))),
            // A refusal by the box (escape, not-a-directory) is an answer; only an inability to ask
            // it falls through to the host clone.
            Err(e) if e.contains("escapes") || e.contains("not a directory") => return Err(e),
            Err(_) => {}
        }
    }
    let listing = list_host_files(name, rel)?;
    let why = if box_liveness(name) == Some(Liveness::Running) {
        "read from the host clone — the box could not be asked"
    } else {
        "read from the host clone — this box isn't running"
    };
    Ok(annotate(Answer::from_host(listing, why)))
}

/// An empty directory and a checkout that was never populated look identical, and the second is the
/// one that makes a dev say "files don't work". Only the root can tell them apart: a repo root with
/// nothing but `.git` is a clone with no working tree.
pub(crate) fn annotate(mut answer: Answer<FileListing>) -> Answer<FileListing> {
    if !answer.value.entries.is_empty() || !answer.value.path.is_empty() {
        return answer;
    }
    answer.note = if answer.note.is_empty() {
        "this workspace has no files in it".into()
    } else {
        format!("{} — and it has no files in it", answer.note)
    };
    answer
}

/// Read a file for the cockpit, from the box when it's up and the host clone when it isn't.
pub fn read_box_file(name: &str, rel: &str) -> Result<(Vec<u8>, bool), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    if box_liveness(name) == Some(Liveness::Running) {
        match read_file_in_box(name, rel) {
            Ok(v) => return Ok(v),
            Err(e) if e.contains("escapes") || e.contains("not a file") => return Err(e),
            Err(_) => {}
        }
    }
    read_host_file(name, rel)
}
