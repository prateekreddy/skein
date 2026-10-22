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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tempdir;
    use std::fs;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

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
        let made = Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo {}", fifo.display());

        let (tx, rx) = mpsc::channel();
        let at = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read(&at, SIGNAL_CAP).map(|b| b.len()));
        });
        let answer = rx.recv_timeout(Duration::from_secs(5));
        if answer.is_err() {
            let _ = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo);
        }
        let answer = answer.expect("reading a FIFO hung instead of refusing it");
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
}
