//! Browsing a box's workspace from the cockpit.
//!
//! Reads **the box's own tree** over `sbx exec`, with the path resolved and escape-guarded inside
//! the box rather than here — a symlink pointing out is refused where it can actually be followed.
//! The host clone is a labelled fallback for a box that cannot be asked, never a silent substitute:
//! for a clone-mode box the two are different checkouts on different branches.

use crate::answer::Answer;
use crate::place::place_of;
use crate::sandbox::sbx_guest_output;
use crate::util::*;
use crate::{box_liveness, lookup_dir, valid_name, Liveness};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
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
    place_of(name)
        .ok_or("invalid box name")?
        .bytes(shell, timeout)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answer::Source;
    #[allow(unused_imports)]
    use crate::testutil::*;
    #[allow(unused_imports)]
    use std::{env, fs};

    #[test]
    fn a_listing_from_the_box_reads_types_the_way_the_box_sees_them() {
        // `find -printf '%y\t%Y\t%s\t%f\n'`: %y is the entry's own type, %Y the type after following
        // a symlink. A linked directory must read as a directory; a link pointing nowhere (%Y = N)
        // must still appear, because a file you can see is debuggable and one that vanished is not.
        let body = "d\td\t4096\tdocs\nf\tf\t120\tREADME.md\nl\td\t12\tlinked\nl\tN\t9\tbroken\nd\td\t4096\t.git\n";
        let mut entries = parse_guest_listing(body);
        sort_entries(&mut entries);
        let seen: Vec<(&str, bool)> = entries.iter().map(|e| (e.name.as_str(), e.dir)).collect();
        assert_eq!(
            seen,
            vec![
                ("docs", true),
                ("linked", true),
                ("broken", false),
                ("README.md", false)
            ],
            "dirs first (a symlinked dir among them), then files; .git omitted"
        );
        assert_eq!(entries[3].size, 120);
        // a line the box couldn't format is skipped rather than becoming a nameless row
        assert!(parse_guest_listing("garbage\n\n").is_empty());
    }

    #[test]
    fn the_box_answering_no_is_different_from_the_box_not_answering() {
        // OK carries its detail; a refusal is an ANSWER and must not fall through to the host clone
        // (that is how you end up reading a different branch's files and never being told).
        assert_eq!(
            split_guest_fs("SKEIN_FS OK 42\nbody").unwrap(),
            ("42".into(), "body".into())
        );
        assert_eq!(
            split_guest_fs("SKEIN_FS OK\nrows").unwrap(),
            ("".into(), "rows".into())
        );
        assert!(split_guest_fs("SKEIN_FS ESCAPE\n")
            .unwrap_err()
            .contains("escapes"));
        assert!(split_guest_fs("SKEIN_FS NOTDIR\n")
            .unwrap_err()
            .contains("not a directory"));
        assert!(split_guest_fs("bash: sbx: command not found").is_err());
        // the preamble refuses traversal before it resolves anything
        let pre = guest_fs_preamble("../../etc");
        assert!(pre.contains("realpath -m") && pre.contains("SKEIN_FS ESCAPE"));
    }

    #[test]
    fn an_empty_root_says_whether_the_checkout_is_the_problem() {
        // The bug this whole path exists for: a host clone holding nothing but `.git` listed as
        // "empty", so the Files tab looked broken while the box had a full tree.
        let empty = |path: &str| FileListing {
            path: path.into(),
            entries: vec![],
        };
        let bare = annotate(Answer::from_host(empty(""), ""));
        assert_eq!(bare.note, "this workspace has no files in it");
        assert_eq!(bare.source, Source::Host, "the fallback still says so");
        // an empty SUBdirectory is just an empty directory — no alarming note
        let sub = annotate(Answer::from_box(empty("docs")));
        assert!(sub.note.is_empty());
        // and a fallback keeps its own explanation, with the emptiness appended
        let fell_back = annotate(Answer::from_host(
            empty(""),
            "read from the host clone — this box isn't running",
        ));
        assert!(fell_back.note.contains("isn't running") && fell_back.note.contains("no files"));
    }

    #[test]
    fn file_api_lists_reads_and_guards_the_workspace() {
        let _g = env_lock();
        let dir_tmp = tempdir();
        let dir = dir_tmp.join("ws");
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap(); // must be hidden from listings
        fs::write(dir.join("README.md"), "# hi").unwrap();
        fs::write(dir.join("docs").join("a.txt"), "aaa").unwrap();
        // a symlink pointing OUTSIDE the workspace must not be traversable
        let _ = std::os::unix::fs::symlink("/etc", dir.join("esc"));
        // one pointing INSIDE it is an ordinary directory, and must list as one
        let _ = std::os::unix::fs::symlink(dir.join("docs"), dir.join("linked"));
        let reg = dir.parent().unwrap().join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"bx":{{"branch":"b","dir":"{}","lastSeen":"2026-01-01T00:00:00Z","status":""}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::set_var("SKEIN_LS_CMD", "false"); // no sbx here — registry is the lookup path

        let l = list_box_files("bx", "").unwrap();
        assert_eq!(
            l.source,
            Source::Host,
            "no sbx here, so this is the host clone and has to say so"
        );
        let names: Vec<&str> = l.value.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(!names.contains(&".git"), ".git must be omitted");
        assert_eq!(names[0], "docs", "dirs sort first");
        assert!(names.contains(&"README.md"));
        let (bytes, truncated) = read_box_file("bx", "README.md").unwrap();
        assert!(!truncated);
        assert_eq!(bytes, b"# hi");
        assert_eq!(list_box_files("bx", "docs").unwrap().value.entries.len(), 1);
        // a symlinked directory reads as a directory (type follows the link), and opens
        assert!(l.value.entries.iter().any(|e| e.name == "linked" && e.dir));
        assert_eq!(
            list_box_files("bx", "linked").unwrap().value.entries.len(),
            1
        );
        // traversal / absolute / symlink-escape / bad-name are all rejected
        assert!(read_box_file("bx", "../sandboxes.json").is_err());
        assert!(read_box_file("bx", "/etc/passwd").is_err());
        assert!(list_box_files("bx", "esc").is_err());
        assert!(read_box_file("../bx", "README.md").is_err());

        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_LS_CMD");
    }
}
