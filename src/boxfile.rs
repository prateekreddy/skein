//! Reading what a box wrote, without letting the box choose what is read.
//!
//! skein-server runs as the same user as every box and reads files boxes write: the shared store's
//! `status/`, `sessions/`, `tasks/`, `diffs/` and `journals/`, and each box's `claude-projects/`,
//! which the box has bound read-write. A plain `open` there follows whatever the box put at the
//! path. A link to a credential file hands its contents to whoever can read the result, a FIFO
//! parks the reading thread until somebody writes to it, and a link to `/dev/zero` reads until
//! the process runs out of memory.
//!
//! So everything here opens with `O_NOFOLLOW | O_NONBLOCK`, checks the open file (not the path)
//! is a regular file, and reads no more than its caller's cap. `O_NONBLOCK` is what makes a FIFO
//! open return at once rather than wait for a writer; on a regular file it changes nothing.
//! `O_NOFOLLOW` covers only the last component, so a directory a box can replace is opened once as
//! a [`Dir`], and the files under it are reached through that open directory rather than by name
//! again.
//!
//! The flags come from `libc`, not from hand-written numbers: `O_NOFOLLOW` differs between
//! architectures (see `secret::create_private`).

use std::ffi::OsStr;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// The cap for a hook's JSON signal (`status/`, `sessions/`, `tasks/`, `boot/`). They are a few
/// hundred bytes; a megabyte is room for any real one and nothing a poll should ever hold.
pub const SIGNAL_CAP: u64 = 1 << 20;

/// Open `path` for reading if, and only if, it is a regular file and not a symbolic link.
///
/// The check is on the open descriptor, so what was checked is what will be read.
pub fn open(path: &Path) -> io::Result<(File, Metadata)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok((file, meta))
}

/// The bytes of the regular file at `path`, refused when there are more than `cap` of them.
pub fn read(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let (file, meta) = open(path)?;
    let too_big = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is larger than {cap} bytes", path.display()),
        )
    };
    if meta.len() > cap {
        return Err(too_big());
    }
    // Bounded again while reading, because a file the box is still appending to can grow past the
    // length it had a moment ago.
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(too_big());
    }
    Ok(bytes)
}

/// [`read`], as UTF-8 text. Invalid UTF-8 is an error, as it is for `fs::read_to_string`.
pub fn read_text(path: &Path, cap: u64) -> io::Result<String> {
    String::from_utf8(read(path, cap)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A directory a box can write, held open so that it cannot be swapped for a link while it is used.
///
/// Opened with `O_NOFOLLOW`, so a link at the path is refused. Names under it are resolved through
/// `/proc/self/fd/<fd>/<name>`, which the kernel looks up in the directory this descriptor holds,
/// whatever is at the original path by then.
pub struct Dir {
    file: File,
}

impl Dir {
    /// Open the directory at `path`, refusing a symbolic link or anything that is not a directory.
    pub fn open(path: &Path) -> io::Result<Dir> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)?;
        Ok(Dir { file })
    }

    /// The path that reaches `name` inside this open directory, or the directory itself when
    /// `name` is empty. Pass it to [`open`] or [`read`] for a file, or to [`Dir::open`] for a
    /// directory below this one.
    pub fn path(&self, name: impl AsRef<OsStr>) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd())).join(name.as_ref())
    }
}

/// The cap for one file in a box's drop-box (`substrate`, `gitgate`, `asks`, `jobs`): a request
/// is one small JSON object, and the largest a real one gets is an ask at the 2048-byte cap
/// `asks::MAX_BYTES` sets on its text. 64 KiB is room for any of them and nothing a poll should
/// hold.
pub const DROP_BOX_CAP: u64 = 64 << 10;

/// One file read out of a drop-box queue.
#[derive(Debug, Clone, PartialEq)]
pub struct Dropped {
    /// The directory it was found in, which is the box it is from. Empty for a file directly under
    /// the queue root, from before the queues were split per box.
    pub box_name: String,
    /// The file's own name, `<id>.json`.
    pub name: String,
    /// What it holds, read by [`read_text`].
    pub text: String,
}

/// Every `*.json` file in the drop-box queue at `root`: `<root>/<box>/*.json`, and `<root>/*.json`
/// as well when `flat`. A file that is a link, a FIFO, a device, over `cap`, or gone by the time it
/// is opened is left out. So is a name starting with a dot, the way a shell glob leaves out the
/// writer's `.tmp.XXXXXX`.
///
/// **This is the listing and the read in one**, because the box writes these directories and
/// skein-server can read what no box may. A listing that checks a path and a read that opens it
/// again let the box swap the file for a link in between, which is what the queues' old
/// `[ -f "$f" ]` followed by `jq "$f"` did (SKEIN-1406). Here a name is only ever opened once,
/// with [`open`]'s flags, and what was opened is what is read. Each box's directory is held as a
/// [`Dir`], so its files are reached through the directory that was opened.
pub fn drop_box(root: &Path, flat: bool, cap: u64) -> Vec<Dropped> {
    let Ok(queue) = Dir::open(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in box_dirs(&queue) {
        // Judged a directory by its own entry above, and opened here with `Dir::open`, which
        // refuses a link as well, so an entry swapped for one in between is not walked into.
        if let Ok(held) = Dir::open(&queue.path(&name)) {
            for file in names(&held) {
                if let Some(text) = read_named(&held, &file, cap) {
                    out.push(Dropped {
                        box_name: name.clone(),
                        name: file,
                        text,
                    });
                }
            }
        }
    }
    if flat {
        for file in names(&queue) {
            if let Some(text) = read_named(&queue, &file, cap) {
                out.push(Dropped {
                    box_name: String::new(),
                    name: file,
                    text,
                });
            }
        }
    }
    out
}

/// [`drop_box`], as the JSON objects its files hold, each stamped `"box"` with the directory it
/// was found in, overwriting whatever the file said. This is the shape the three queues' `jq`
/// listings printed, so their parsers take it unchanged.
///
/// A file may hold more than one object, as `jq`'s `inputs` read it, and anything that is not an
/// object is dropped. A file that does not parse is dropped on its own. Under `jq` it ended the
/// whole listing, and since `jq` reads its files as one stream, a box's half-written object could
/// swallow the start of the next box's file.
pub fn drop_box_objects(root: &Path, flat: bool) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for file in drop_box(root, flat, DROP_BOX_CAP) {
        let values: Result<Vec<serde_json::Value>, _> =
            serde_json::Deserializer::from_str(&file.text)
                .into_iter::<serde_json::Value>()
                .collect();
        let Ok(values) = values else {
            continue;
        };
        for mut value in values {
            if let Some(object) = value.as_object_mut() {
                object.insert("box".into(), file.box_name.clone().into());
                out.push(value);
            }
        }
    }
    out
}

/// The bytes of the file `name` in the drop-box directory `dir`, for a caller that needs one file
/// rather than the queue: the directory is opened as a [`Dir`] and the file read through it, by
/// [`read_text`].
pub fn read_in(dir: &Path, name: &str, cap: u64) -> io::Result<String> {
    read_text(&Dir::open(dir)?.path(name), cap)
}

/// The names of files in `dir` a listing considers: UTF-8, not starting with a dot. Only names:
/// nothing here is opened, and what each one is is decided by opening it, with [`read_text`].
fn names(dir: &Dir) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir.path("")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    out.sort();
    out
}

/// The names in the queue `dir` whose own entries say they are directories: the boxes'
/// drop-boxes. The type is the entry's `d_type`, which is not followed, so a box entry that is
/// itself a link is skipped here; only the fleet side can make one, and none is a drop-box.
fn box_dirs(dir: &Dir) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir.path("")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    out.sort();
    out
}

/// A `*.json` name in `dir`, read by [`read_text`], or `None`.
fn read_named(dir: &Dir, name: &str, cap: u64) -> Option<String> {
    if !name.ends_with(".json") || name == ".json" {
        return None;
    }
    read_text(&dir.path(name), cap).ok()
}

/// Make a FIFO at `path`, for a test that plants one where a box could.
#[cfg(test)]
pub(crate) fn plant_fifo(path: &Path) {
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo runs");
    assert!(made.success(), "mkfifo {}", path.display());
}

/// Run `read` on a thread and wait at most five seconds for it, so a reader that blocks on the FIFO
/// at `fifo` fails the test rather than hanging the suite. On a timeout the FIFO is opened for
/// writing, which releases the blocked reader, and the test panics with `what`.
#[cfg(test)]
pub(crate) fn within<T: Send + 'static>(
    fifo: &Path,
    what: &str,
    read: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(answer) => answer,
        Err(_) => {
            let _ = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(fifo);
            panic!("{what} waited on a FIFO instead of refusing it");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;
    use std::fs;

    // Fails if `O_NOFOLLOW` is dropped: the link is followed and the target's bytes come back.
    #[test]
    fn a_link_is_refused_even_to_a_file_the_reader_could_read() {
        let dir = tempdir();
        fs::write(dir.join("credential"), "skein-test-not-a-secret").unwrap();
        std::os::unix::fs::symlink(dir.join("credential"), dir.join("a.jsonl")).unwrap();
        assert!(
            read(&dir.join("a.jsonl"), SIGNAL_CAP).is_err(),
            "a link the box planted was followed"
        );
        assert_eq!(
            read_text(&dir.join("credential"), SIGNAL_CAP).unwrap(),
            "skein-test-not-a-secret",
            "the control: the same file read by its own path"
        );
    }

    // Fails if `O_NONBLOCK` is dropped: opening a FIFO with no writer waits for one, so the reader
    // never answers. The wait is on a thread with its own deadline so a regression fails here
    // rather than hanging the suite, and the FIFO is opened for writing afterwards to release it.
    #[test]
    fn a_fifo_is_refused_at_once_rather_than_waited_on() {
        let dir = tempdir();
        let fifo = dir.join("status.json");
        plant_fifo(&fifo);
        let at = fifo.clone();
        let answer = within(&fifo, "reading a FIFO", move || {
            read(&at, SIGNAL_CAP).map(|b| b.len())
        });
        assert!(answer.is_err(), "a FIFO was read as a file: {answer:?}");
    }

    // Fails if the regular-file check on the open descriptor is dropped: `/dev/null` opens fine
    // and reads as an empty file.
    #[test]
    fn a_device_is_refused() {
        assert!(read(Path::new("/dev/null"), SIGNAL_CAP).is_err());
    }

    // Fails if the cap is not applied: the file is one byte over it and would be returned whole.
    #[test]
    fn a_file_over_the_cap_is_refused_and_one_at_it_is_read() {
        let dir = tempdir();
        fs::write(dir.join("at"), vec![b'x'; 64]).unwrap();
        fs::write(dir.join("over"), vec![b'x'; 65]).unwrap();
        assert_eq!(read(&dir.join("at"), 64).unwrap().len(), 64);
        assert!(read(&dir.join("over"), 64).is_err());
    }

    // A directory opened as a `Dir` stays the one that was opened. Fails if `Dir::path` resolves
    // by the original path instead: after the swap that path is a link to `elsewhere`, and the read
    // would return its file. Also fails if `Dir::open` follows a link.
    #[test]
    fn a_directory_swapped_for_a_link_after_it_was_opened_is_not_followed() {
        let dir = tempdir();
        let projects = dir.join("slug");
        let elsewhere = dir.join("elsewhere");
        fs::create_dir_all(&projects).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(projects.join("a.jsonl"), "the box's own").unwrap();
        fs::write(elsewhere.join("a.jsonl"), "somewhere else").unwrap();

        let held = Dir::open(&projects).unwrap();
        fs::rename(&projects, dir.join("moved")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &projects).unwrap();

        assert_eq!(
            read_text(&held.path("a.jsonl"), SIGNAL_CAP).unwrap(),
            "the box's own"
        );
        assert!(
            Dir::open(&projects).is_err(),
            "a link at a directory's path was opened as the directory"
        );
    }

    // ─────────────── the drop-box queues (SKEIN-1406) ───────────────

    /// A queue root with one box's drop-box in it, `web-main`, and a file outside it that holds a
    /// valid record, standing for what skein-server can read and no box may.
    fn queue() -> (crate::testutil::TempDir, PathBuf, PathBuf) {
        let dir = tempdir();
        let root = dir.join("requests");
        fs::create_dir_all(root.join("web-main")).unwrap();
        let secret = dir.join("private.json");
        fs::write(
            &secret,
            r#"{"id":"planted","why":"skein-test-not-a-secret"}"#,
        )
        .unwrap();
        (dir, root, secret)
    }

    fn listed(root: &Path, flat: bool) -> Vec<(String, String)> {
        drop_box(root, flat, DROP_BOX_CAP)
            .into_iter()
            .map(|d| (d.box_name, d.name))
            .collect()
    }

    // Fails if the listing reads its own files any other way than `read`'s: what a box writes is
    // listed, under its directory's name, and the queue root's files only when `flat` asks.
    #[test]
    fn a_drop_box_lists_the_boxs_own_json_files_and_nothing_else() {
        let (_dir, root, _) = queue();
        fs::write(root.join("web-main/a.json"), "{}").unwrap();
        fs::write(root.join("web-main/.tmp.abc123"), "{}").unwrap();
        fs::write(root.join("web-main/.hidden.json"), "{}").unwrap();
        fs::write(root.join("web-main/notes.txt"), "{}").unwrap();
        fs::write(root.join("old.json"), "{}").unwrap();
        let web = |n: &str| ("web-main".to_string(), n.to_string());
        assert_eq!(listed(&root, false), vec![web("a.json")]);
        assert_eq!(
            listed(&root, true),
            vec![web("a.json"), (String::new(), "old.json".into())]
        );
    }

    // Fails if `O_NOFOLLOW` is dropped from `open`: the link is followed and the record it names
    // is listed as the box's own.
    #[test]
    fn a_link_in_a_drop_box_is_never_read() {
        let (_dir, root, secret) = queue();
        std::os::unix::fs::symlink(&secret, root.join("web-main/planted.json")).unwrap();
        // And a box entry that is itself a link, which only the fleet side could make, is not
        // walked into either. Two walls stand here, the entry's own `d_type` in `box_dirs` and
        // `Dir::open`'s `O_NOFOLLOW`, so this fails only with both taken away.
        fs::create_dir_all(root.parent().unwrap().join("elsewhere")).unwrap();
        fs::write(root.parent().unwrap().join("elsewhere/x.json"), "{}").unwrap();
        std::os::unix::fs::symlink(
            root.parent().unwrap().join("elsewhere"),
            root.join("linked"),
        )
        .unwrap();
        assert_eq!(listed(&root, true), Vec::<(String, String)>::new());
        assert!(drop_box_objects(&root, true).is_empty());
    }

    // The race, made deterministic: the listing has taken the name, then the box swaps the file
    // for a link before it is read. Fails if `O_NOFOLLOW` is dropped from `open` — the read
    // returns the link's target — or if the read goes back to the path a check approved.
    #[test]
    fn a_file_swapped_for_a_link_between_listing_and_reading_is_not_read() {
        let (_dir, root, secret) = queue();
        fs::write(root.join("web-main/r.json"), r#"{"id":"r"}"#).unwrap();
        let held = Dir::open(&root.join("web-main")).unwrap();
        let names = names(&held);
        assert_eq!(
            names,
            vec!["r.json".to_string()],
            "the premise: it was listed"
        );
        fs::remove_file(root.join("web-main/r.json")).unwrap();
        std::os::unix::fs::symlink(&secret, root.join("web-main/r.json")).unwrap();
        assert_eq!(read_named(&held, "r.json", DROP_BOX_CAP), None);
        assert!(
            read_in(&root.join("web-main"), "r.json", DROP_BOX_CAP).is_err(),
            "the single-file read the write-backs use followed the link"
        );
    }

    // Fails if `O_NONBLOCK` is dropped from `open`: the listing opens the FIFO and waits for a
    // writer that never comes.
    #[test]
    fn a_fifo_in_a_drop_box_does_not_hang_the_listing() {
        let (_dir, root, _) = queue();
        fs::write(root.join("web-main/a.json"), "{}").unwrap();
        let fifo = root.join("web-main/fifo.json");
        plant_fifo(&fifo);
        let at = root.clone();
        let got = within(&fifo, "listing a drop-box", move || listed(&at, false));
        assert_eq!(got, vec![("web-main".to_string(), "a.json".to_string())]);
    }

    // Fails if the cap is not applied: the file is one byte over it and would be listed.
    #[test]
    fn an_oversized_file_in_a_drop_box_is_skipped() {
        let (_dir, root, _) = queue();
        let body = |n: usize| format!(r#"{{"id":"x","pad":"{}"}}"#, "p".repeat(n));
        let at = body(DROP_BOX_CAP as usize - body(0).len());
        assert_eq!(at.len() as u64, DROP_BOX_CAP);
        fs::write(root.join("web-main/at.json"), &at).unwrap();
        fs::write(root.join("web-main/over.json"), format!("{at} ")).unwrap();
        assert_eq!(
            listed(&root, false),
            vec![("web-main".to_string(), "at.json".to_string())]
        );
    }

    // The shape the three queues' parsers take. Fails if `"box"` is not overwritten from the
    // directory, if a non-object is kept, if a file holding two objects yields one, or if a file
    // that does not parse takes the rest of the queue with it, as it did under `jq`.
    #[test]
    fn drop_box_objects_are_stamped_with_their_directory_and_one_bad_file_costs_only_itself() {
        let (_dir, root, _) = queue();
        fs::create_dir_all(root.join("api")).unwrap();
        fs::write(root.join("web-main/a.json"), r#"{"id":"a","box":"api"}"#).unwrap();
        fs::write(root.join("web-main/arr.json"), "[1,2]").unwrap();
        fs::write(root.join("web-main/two.json"), r#"{"id":"b"} {"id":"c"}"#).unwrap();
        fs::write(root.join("api/half.json"), r#"{"id":"#).unwrap();
        fs::write(root.join("api/d.json"), r#"{"id":"d"}"#).unwrap();
        fs::write(root.join("old.json"), r#"{"id":"e","box":"web-main"}"#).unwrap();
        let got: Vec<(String, String)> = drop_box_objects(&root, true)
            .iter()
            .map(|v| {
                (
                    v["id"].as_str().unwrap().into(),
                    v["box"].as_str().unwrap().into(),
                )
            })
            .collect();
        let pair = |i: &str, b: &str| (i.to_string(), b.to_string());
        assert_eq!(
            got,
            vec![
                pair("d", "api"),
                pair("a", "web-main"),
                pair("b", "web-main"),
                pair("c", "web-main"),
                pair("e", ""),
            ]
        );
    }
}
