//! The backup file: its frames, its tar entries, and an extractor that trusts nothing it reads.
//!
//! ```text
//! file    := "SKEINBDL" format:u32be  M  unit*  E
//! unit    := U  D*  T
//! frame   := kind:u8  length:u64be  payload  [sha256(kind, length, payload), except on D]
//! ```
//!
//! `M` is the opening manifest, `U` names a unit, each `D` holds up to 1 MiB of the unit's tar, `T`
//! gives its entry count, byte count and SHA-256, and `E` is the closing manifest.
//!
//! The writer never seeks. A hash is known only after its bytes have gone, so `M` holds what a scan
//! measured beforehand, each `T` follows its own unit, and `E` repeats every unit with its hash.
//! A file cut anywhere is therefore valid up to its last `T`.

use crate::util::Sha256;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

pub(super) const MAGIC: &[u8; 8] = b"SKEINBDL";
pub(super) const FORMAT: u32 = 1;
pub(super) const KIND_MANIFEST: u8 = b'M';
pub(super) const KIND_UNIT: u8 = b'U';
const KIND_DATA: u8 = b'D';
pub(super) const KIND_TRAILER: u8 = b'T';
pub(super) const KIND_END: u8 = b'E';
const CHUNK: usize = 1 << 20;
const MAX_META: u64 = 64 << 20;
const MAX_PAX: u64 = 1 << 20;
const BLOCK: usize = 512;
/// The largest value an 11-digit octal tar field holds.
const OCTAL_11: u64 = 0o77777777777;
/// The mode bits a restore applies. Set-uid and set-gid are never restored.
const MODE_MASK: u32 = 0o1777;

/// Why reading stopped: the file ended early, or what it holds cannot be trusted.
#[derive(Debug)]
pub(super) enum Stop {
    Cut(String),
    Bad(String),
}

impl Stop {
    fn from_io(error: io::Error, what: &str) -> Stop {
        match error.kind() {
            io::ErrorKind::UnexpectedEof => Stop::Cut(format!("the file ends inside {what}")),
            io::ErrorKind::InvalidData => Stop::Bad(error.to_string()),
            _ => Stop::Bad(format!("reading {what}: {error}")),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(super) struct UnitLine {
    pub name: String,
    pub files: u64,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub(super) struct DockerNote {
    pub included: bool,
    pub answered: bool,
    pub why_unknown: String,
    pub volumes: Vec<String>,
    pub images: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct Manifest {
    pub format: u32,
    pub volume_schema: u32,
    pub skein_commit: String,
    pub arch: String,
    pub os: String,
    pub created: String,
    pub encrypted: bool,
    /// Counts and bytes as scanned before the copy; `sha256` is empty here and final in `Closing`.
    pub units: Vec<UnitLine>,
    pub docker: DockerNote,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub(super) struct LeftOut {
    pub credentials: Vec<String>,
    pub tool_caches: Vec<String>,
    pub build_output: Vec<String>,
    pub not_on_allow_list: Vec<String>,
    pub unreadable: Vec<String>,
    pub other_filesystem: Vec<String>,
    pub sockets_and_devices: u64,
    pub changed_while_reading: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct Closing {
    pub units: Vec<UnitLine>,
    pub left_out: LeftOut,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct UnitHead {
    pub index: u64,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct Trailer {
    pub files: u64,
    pub bytes: u64,
    pub sha256: String,
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn frame_digest(kind: u8, payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(&[kind]);
    hasher.update(&(payload.len() as u64).to_be_bytes());
    hasher.update(payload);
    hasher.finish()
}

/// Write a metadata frame and return its digest.
pub(super) fn write_frame(out: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<[u8; 32]> {
    let digest = frame_digest(kind, payload);
    out.write_all(&[kind])?;
    out.write_all(&(payload.len() as u64).to_be_bytes())?;
    out.write_all(payload)?;
    out.write_all(&digest)?;
    Ok(digest)
}

/// A unit's tar stream on its way out as `D` frames, hashed and counted.
pub(super) struct UnitWriter<'a, W: Write> {
    out: &'a mut W,
    buffer: Vec<u8>,
    hasher: Sha256,
    bytes: u64,
}

impl<'a, W: Write> UnitWriter<'a, W> {
    pub(super) fn new(out: &'a mut W) -> Self {
        UnitWriter {
            out,
            buffer: Vec::with_capacity(CHUNK),
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    fn drain(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.out.write_all(&[KIND_DATA])?;
        self.out
            .write_all(&(self.buffer.len() as u64).to_be_bytes())?;
        self.out.write_all(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }

    /// End the tar, flush it, and write the unit's trailer.
    pub(super) fn finish(mut self, files: u64) -> io::Result<Trailer> {
        self.write_all(&[0u8; BLOCK * 2])?;
        self.drain()?;
        let trailer = Trailer {
            files,
            bytes: self.bytes,
            sha256: hex(&self.hasher.finish()),
        };
        let payload = serde_json::to_vec(&trailer)?;
        write_frame(self.out, KIND_TRAILER, &payload)?;
        Ok(trailer)
    }
}

impl<W: Write> Write for UnitWriter<'_, W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let take = data.len().min(CHUNK - self.buffer.len());
        self.buffer.extend_from_slice(&data[..take]);
        self.hasher.update(&data[..take]);
        self.bytes += take as u64;
        if self.buffer.len() == CHUNK {
            self.drain()?;
        }
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What one tar entry is.
pub(super) enum EntryKind<'a> {
    Dir,
    File(u64),
    Symlink(&'a [u8]),
    HardLink(&'a [u8]),
}

/// The tar bytes an entry takes, for measuring before the copy.
pub(super) fn entry_cost(path_len: usize, link_len: usize, size: u64) -> u64 {
    let pax = if path_len > 100 || link_len > 100 || size > OCTAL_11 {
        (BLOCK as u64) + ((path_len + link_len + 64) as u64).next_multiple_of(BLOCK as u64)
    } else {
        0
    };
    pax + BLOCK as u64 + size.next_multiple_of(BLOCK as u64)
}

fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
    let tail = key.len() + value.len() + 3;
    let mut length = tail + tail.to_string().len();
    if (tail + length.to_string().len()) != length {
        length = tail + length.to_string().len();
    }
    let mut record = format!("{length} {key}=").into_bytes();
    record.extend_from_slice(value);
    record.push(b'\n');
    record
}

fn put_octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(&text.as_bytes()[text.len() - digits..]);
    field[digits] = 0;
}

fn header(path: &[u8], link: &[u8], size: u64, mode: u32, mtime: u64, flag: u8) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];
    let name = path.len().min(100);
    block[..name].copy_from_slice(&path[..name]);
    put_octal(&mut block[100..108], u64::from(mode & 0o7777));
    put_octal(&mut block[108..116], 0);
    put_octal(&mut block[116..124], 0);
    put_octal(&mut block[124..136], size.min(OCTAL_11));
    put_octal(&mut block[136..148], mtime.min(OCTAL_11));
    block[148..156].fill(b' ');
    block[156] = flag;
    let linked = link.len().min(100);
    block[157..157 + linked].copy_from_slice(&link[..linked]);
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    let sum: u32 = block.iter().map(|b| u32::from(*b)).sum();
    let text = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(text.as_bytes());
    block
}

/// Write one entry's header, with a PAX record for whatever does not fit in it.
pub(super) fn write_entry_header(
    out: &mut impl Write,
    path: &[u8],
    kind: &EntryKind,
    mode: u32,
    mtime: u64,
) -> io::Result<()> {
    let (flag, size, link): (u8, u64, &[u8]) = match kind {
        EntryKind::Dir => (b'5', 0, b""),
        EntryKind::File(size) => (b'0', *size, b""),
        EntryKind::Symlink(target) => (b'2', 0, target),
        EntryKind::HardLink(target) => (b'1', 0, target),
    };
    let mut pax = Vec::new();
    if path.len() > 100 {
        pax.extend(pax_record("path", path));
    }
    if link.len() > 100 {
        pax.extend(pax_record("linkpath", link));
    }
    if size > OCTAL_11 {
        pax.extend(pax_record("size", size.to_string().as_bytes()));
    }
    if !pax.is_empty() {
        out.write_all(&header(
            b"pax-header",
            b"",
            pax.len() as u64,
            0o600,
            0,
            b'x',
        ))?;
        out.write_all(&pax)?;
        pad(out, pax.len() as u64)?;
    }
    out.write_all(&header(path, link, size, mode, mtime, flag))
}

pub(super) fn pad(out: &mut impl Write, size: u64) -> io::Result<()> {
    let rest = (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
    out.write_all(&[0u8; BLOCK][..rest as usize])
}

/// Copy exactly `size` bytes of `file`. Returns whether the file was not that size any more; a
/// short file is padded with zeros so the tar stays well formed.
pub(super) fn write_body(out: &mut impl Write, file: &mut fs::File, size: u64) -> io::Result<bool> {
    let mut left = size;
    let mut buffer = vec![0u8; 256 * 1024];
    while left > 0 {
        let want = buffer
            .len()
            .min(usize::try_from(left).unwrap_or(usize::MAX));
        let got = file.read(&mut buffer[..want])?;
        if got == 0 {
            break;
        }
        out.write_all(&buffer[..got])?;
        left -= got as u64;
    }
    let shrank = left > 0;
    while left > 0 {
        let zeros = buffer
            .len()
            .min(usize::try_from(left).unwrap_or(usize::MAX));
        buffer[..zeros].fill(0);
        out.write_all(&buffer[..zeros])?;
        left -= zeros as u64;
    }
    pad(out, size)?;
    let grew = file.read(&mut [0u8; 1])? > 0;
    Ok(shrank || grew)
}

/// Reads frames from a bundle.
pub(super) struct BundleReader<R> {
    input: R,
}

impl<R: Read + Seek> BundleReader<R> {
    /// Check the magic and refuse a format this binary does not know.
    pub(super) fn open(mut input: R) -> Result<Self, Stop> {
        let mut head = [0u8; 12];
        input
            .read_exact(&mut head)
            .map_err(|e| Stop::from_io(e, "the file header"))?;
        if &head[..8] != MAGIC {
            return Err(Stop::Bad("this is not a skein backup file".into()));
        }
        let format = u32::from_be_bytes([head[8], head[9], head[10], head[11]]);
        if format > FORMAT {
            return Err(Stop::Bad(format!(
                "this backup is format {format} and this skein reads up to format {FORMAT}. \
                 Upgrade skein to restore it."
            )));
        }
        if format == 0 {
            return Err(Stop::Bad(
                "this backup names format 0, which never existed".into(),
            ));
        }
        Ok(BundleReader { input })
    }

    fn head(&mut self, what: &str) -> Result<(u8, u64), Stop> {
        let mut head = [0u8; 9];
        self.input
            .read_exact(&mut head)
            .map_err(|e| Stop::from_io(e, what))?;
        let mut length = [0u8; 8];
        length.copy_from_slice(&head[1..]);
        Ok((head[0], u64::from_be_bytes(length)))
    }

    fn body(&mut self, kind: u8, length: u64, what: &str) -> Result<(Vec<u8>, [u8; 32]), Stop> {
        if length > MAX_META {
            return Err(Stop::Bad(format!("{what} claims an impossible length")));
        }
        let mut payload = vec![0u8; length as usize];
        self.input
            .read_exact(&mut payload)
            .map_err(|e| Stop::from_io(e, what))?;
        let mut digest = [0u8; 32];
        self.input
            .read_exact(&mut digest)
            .map_err(|e| Stop::from_io(e, what))?;
        if frame_digest(kind, &payload) != digest {
            return Err(Stop::Bad(format!(
                "{what} is damaged: its checksum is wrong"
            )));
        }
        Ok((payload, digest))
    }

    /// The next frame, which must be of `kind`.
    pub(super) fn frame(&mut self, kind: u8, what: &str) -> Result<(Vec<u8>, [u8; 32]), Stop> {
        let (found, length) = self.head(what)?;
        if found != kind {
            return Err(Stop::Bad(format!("{what} is missing where it should be")));
        }
        self.body(kind, length, what)
    }

    /// Step over a unit's data without reading it, returning its trailer.
    pub(super) fn skip_unit(&mut self, what: &str) -> Result<Trailer, Stop> {
        loop {
            let (kind, length) = self.head(what)?;
            match kind {
                KIND_DATA => {
                    if length > CHUNK as u64 {
                        return Err(Stop::Bad(format!("{what} holds an oversized chunk")));
                    }
                    let offset = i64::try_from(length)
                        .map_err(|_| Stop::Bad(format!("{what} holds an oversized chunk")))?;
                    let at = self
                        .input
                        .stream_position()
                        .map_err(|e| Stop::from_io(e, what))?;
                    let end = self
                        .input
                        .seek(SeekFrom::End(0))
                        .map_err(|e| Stop::from_io(e, what))?;
                    if at + length > end {
                        return Err(Stop::Cut(format!("the file ends inside {what}")));
                    }
                    self.input
                        .seek(SeekFrom::Start(at))
                        .map_err(|e| Stop::from_io(e, what))?;
                    self.input
                        .seek(SeekFrom::Current(offset))
                        .map_err(|e| Stop::from_io(e, what))?;
                }
                KIND_TRAILER => {
                    let (payload, _) = self.body(KIND_TRAILER, length, what)?;
                    return parse_trailer(&payload, what);
                }
                _ => return Err(Stop::Bad(format!("{what} is damaged: unknown frame"))),
            }
        }
    }

    /// A reader over the current unit's tar stream.
    pub(super) fn unit<'a>(&'a mut self, what: &'a str) -> UnitData<'a, R> {
        UnitData {
            reader: self,
            what,
            left: 0,
            hasher: Some(Sha256::new()),
            bytes: 0,
            trailer: None,
        }
    }
}

fn parse_trailer(payload: &[u8], what: &str) -> Result<Trailer, Stop> {
    serde_json::from_slice(payload)
        .map_err(|_| Stop::Bad(format!("{what} has an unreadable trailer")))
}

/// The tar bytes of one unit, hashed as they are read. Ends at the unit's trailer.
pub(super) struct UnitData<'a, R> {
    reader: &'a mut BundleReader<R>,
    what: &'a str,
    left: u64,
    hasher: Option<Sha256>,
    bytes: u64,
    trailer: Option<Trailer>,
}

impl<R: Read + Seek> UnitData<'_, R> {
    /// Read to the trailer and check the unit's hash and sizes against it.
    pub(super) fn verify(mut self, entries: u64) -> Result<Trailer, Stop> {
        let mut rest = [0u8; 8192];
        loop {
            let got = self
                .read(&mut rest)
                .map_err(|e| Stop::from_io(e, self.what))?;
            if got == 0 {
                break;
            }
            if rest[..got].iter().any(|b| *b != 0) {
                return Err(Stop::Bad(format!(
                    "{} is damaged: data follows the end of its archive",
                    self.what
                )));
            }
        }
        let trailer = self
            .trailer
            .take()
            .ok_or_else(|| Stop::Bad(format!("{} has no trailer", self.what)))?;
        let hash = hex(&self.hasher.take().expect("hashed once").finish());
        if hash != trailer.sha256 || self.bytes != trailer.bytes {
            return Err(Stop::Bad(format!(
                "{} is damaged: its SHA-256 does not match",
                self.what
            )));
        }
        if entries != trailer.files {
            return Err(Stop::Bad(format!(
                "{} is damaged: it holds {entries} entries and says {}",
                self.what, trailer.files
            )));
        }
        Ok(trailer)
    }
}

impl<R: Read + Seek> Read for UnitData<'_, R> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if self.trailer.is_some() || into.is_empty() {
            return Ok(0);
        }
        while self.left == 0 {
            let mut head = [0u8; 9];
            self.reader.input.read_exact(&mut head)?;
            let mut length = [0u8; 8];
            length.copy_from_slice(&head[1..]);
            let length = u64::from_be_bytes(length);
            match head[0] {
                KIND_DATA if length > 0 && length <= CHUNK as u64 => self.left = length,
                KIND_TRAILER => {
                    let (payload, _) = self.reader.body(KIND_TRAILER, length, self.what).map_err(
                        |stop| match stop {
                            Stop::Cut(why) => io::Error::new(io::ErrorKind::UnexpectedEof, why),
                            Stop::Bad(why) => io::Error::new(io::ErrorKind::InvalidData, why),
                        },
                    )?;
                    let trailer = parse_trailer(&payload, self.what).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "unreadable unit trailer")
                    })?;
                    self.trailer = Some(trailer);
                    return Ok(0);
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} is damaged: unknown frame", self.what),
                    ))
                }
            }
        }
        let want = into
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let got = self.reader.input.read(&mut into[..want])?;
        if got == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= got as u64;
        self.bytes += got as u64;
        if let Some(hasher) = &mut self.hasher {
            hasher.update(&into[..got]);
        }
        Ok(got)
    }
}

/// A directory's metadata, applied once everything inside it exists.
pub(super) struct DirMeta {
    pub path: PathBuf,
    pub mode: u32,
    pub mtime: u64,
}

fn field_end(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    &field[..end]
}

fn octal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field_end(field)).ok()?.trim();
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

fn checksum_holds(block: &[u8; BLOCK]) -> bool {
    let Some(stored) = octal(&block[148..156]) else {
        return false;
    };
    let sum: u64 = block
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                u64::from(b' ')
            } else {
                u64::from(*b)
            }
        })
        .sum();
    stored == sum
}

/// `raw` as a path this restore may write: relative, plain names only, under `fleet` or `home`.
pub(super) fn safe_relative(raw: &[u8]) -> Result<PathBuf, String> {
    let shown = String::from_utf8_lossy(raw);
    let refuse = |why: &str| Err(format!("refused the entry {shown:?}: {why}"));
    let trimmed = raw.strip_suffix(b"/").unwrap_or(raw);
    if trimmed.is_empty() || trimmed.contains(&0) {
        return refuse("it has no usable name");
    }
    let path = Path::new(OsStr::from_bytes(trimmed));
    if path.is_absolute() {
        return refuse("it is an absolute path");
    }
    if trimmed
        .split(|b| *b == b'/')
        .any(|part| part.is_empty() || part == b"." || part == b"..")
    {
        return refuse("it steps outside its place");
    }
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return refuse("it steps outside its place");
    }
    let first = path.components().next();
    if !matches!(first, Some(Component::Normal(top)) if top == "fleet" || top == "home") {
        return refuse("it is not under fleet/ or home/");
    }
    Ok(path.to_path_buf())
}

/// Make every parent of `rel` under `root`, refusing a link or a file on the way.
fn real_parents(root: &Path, rel: &Path) -> Result<(), String> {
    let mut at = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "refused the entry {}: a parent of it is a link or a file",
                    rel.display()
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&at).map_err(|e| format!("creating {}: {e}", at.display()))?;
            }
            Err(e) => return Err(format!("reading {}: {e}", at.display())),
        }
    }
    Ok(())
}

/// Set a path's modification time without following a link.
pub(super) fn set_mtime(path: &Path, mtime: u64) -> io::Result<()> {
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let stamp = libc::timespec {
        tv_sec: mtime as libc::time_t,
        tv_nsec: 0,
    };
    let times = [stamp, stamp];
    // SAFETY: `name` is NUL-terminated and `times` holds the two entries utimensat reads.
    let done = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            name.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if done == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn read_block(data: &mut impl Read, block: &mut [u8; BLOCK], what: &str) -> Result<bool, Stop> {
    let mut filled = 0;
    while filled < BLOCK {
        let got = data
            .read(&mut block[filled..])
            .map_err(|e| Stop::from_io(e, what))?;
        if got == 0 {
            return if filled == 0 {
                Ok(false)
            } else {
                Err(Stop::Bad(format!("{what} is damaged: a torn tar block")))
            };
        }
        filled += got;
    }
    Ok(true)
}

fn skip_padding(data: &mut impl Read, size: u64, what: &str) -> Result<(), Stop> {
    let rest = ((BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64) as usize;
    let mut pad = [0u8; BLOCK];
    data.read_exact(&mut pad[..rest])
        .map_err(|e| Stop::from_io(e, what))
}

/// The `path`, `linkpath` and `size` a PAX header sets for the next entry.
#[derive(Default)]
struct Pax {
    path: Option<Vec<u8>>,
    link: Option<Vec<u8>>,
    size: Option<u64>,
}

fn parse_pax(mut body: &[u8], what: &str) -> Result<Pax, Stop> {
    let bad = || Stop::Bad(format!("{what} is damaged: a malformed PAX header"));
    let mut pax = Pax::default();
    while !body.is_empty() {
        let space = body.iter().position(|b| *b == b' ').ok_or_else(bad)?;
        let length: usize = std::str::from_utf8(&body[..space])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(bad)?;
        if length <= space + 1 || length > body.len() || body[length - 1] != b'\n' {
            return Err(bad());
        }
        let record = &body[space + 1..length - 1];
        let equals = record.iter().position(|b| *b == b'=').ok_or_else(bad)?;
        let (key, value) = (&record[..equals], &record[equals + 1..]);
        match key {
            b"path" => pax.path = Some(value.to_vec()),
            b"linkpath" => pax.link = Some(value.to_vec()),
            b"size" => {
                let text = std::str::from_utf8(value).map_err(|_| bad())?;
                pax.size = Some(text.parse().map_err(|_| bad())?);
            }
            _ => {}
        }
        body = &body[length..];
    }
    Ok(pax)
}

/// Extract one unit's tar into `stage`. Returns the entry count and the directories whose mode and
/// time are applied after publishing. Nothing is created through a link.
pub(super) fn extract(
    data: &mut impl Read,
    stage: &Path,
    what: &str,
) -> Result<(u64, Vec<DirMeta>), Stop> {
    let mut entries = 0u64;
    let mut dirs = Vec::new();
    let mut pending = Pax::default();
    let mut block = [0u8; BLOCK];
    loop {
        if !read_block(data, &mut block, what)? {
            return Err(Stop::Bad(format!(
                "{what} is damaged: its archive has no end"
            )));
        }
        if block.iter().all(|b| *b == 0) {
            return Ok((entries, dirs));
        }
        if !checksum_holds(&block) {
            return Err(Stop::Bad(format!(
                "{what} is damaged: a tar header checksum is wrong"
            )));
        }
        let bad = || Stop::Bad(format!("{what} is damaged: an unreadable tar header"));
        let flag = block[156];
        let header_size = octal(&block[124..136]).ok_or_else(bad)?;
        if flag == b'x' {
            if header_size > MAX_PAX {
                return Err(Stop::Bad(format!("{what} holds an oversized PAX header")));
            }
            let mut body = vec![0u8; header_size as usize];
            data.read_exact(&mut body)
                .map_err(|e| Stop::from_io(e, what))?;
            skip_padding(data, header_size, what)?;
            pending = parse_pax(&body, what)?;
            continue;
        }
        let taken = std::mem::take(&mut pending);
        let raw_path = taken
            .path
            .unwrap_or_else(|| field_end(&block[..100]).to_vec());
        let link = taken
            .link
            .unwrap_or_else(|| field_end(&block[157..257]).to_vec());
        let size = taken.size.unwrap_or(header_size);
        let mode = octal(&block[100..108]).ok_or_else(bad)? as u32 & MODE_MASK;
        let mtime = octal(&block[136..148]).ok_or_else(bad)?;
        let rel = safe_relative(&raw_path).map_err(Stop::Bad)?;
        real_parents(stage, &rel).map_err(Stop::Bad)?;
        let target = stage.join(&rel);
        let fail = |e: io::Error| Stop::Bad(format!("restoring {}: {e}", rel.display()));
        match flag {
            b'5' => {
                match fs::symlink_metadata(&target) {
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => {
                        return Err(Stop::Bad(format!(
                            "refused the entry {}: it replaces a link or a file",
                            rel.display()
                        )))
                    }
                    Err(_) => fs::create_dir(&target).map_err(fail)?,
                }
                dirs.push(DirMeta {
                    path: rel.clone(),
                    mode,
                    mtime,
                });
            }
            b'0' | 0 => {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&target)
                    .map_err(fail)?;
                let copied = io::copy(&mut data.by_ref().take(size), &mut file)
                    .map_err(|e| Stop::from_io(e, what))?;
                if copied != size {
                    return Err(Stop::Bad(format!("{what} is damaged: a file is cut short")));
                }
                skip_padding(data, size, what)?;
                file.set_permissions(fs::Permissions::from_mode(mode))
                    .map_err(fail)?;
                drop(file);
                set_mtime(&target, mtime).map_err(fail)?;
            }
            b'2' => {
                if link.is_empty() || link.contains(&0) {
                    return Err(Stop::Bad(format!(
                        "refused the entry {}: an empty link",
                        rel.display()
                    )));
                }
                symlink(std::ffi::OsString::from_vec(link), &target).map_err(fail)?;
                set_mtime(&target, mtime).map_err(fail)?;
            }
            b'1' => {
                let source = safe_relative(&link).map_err(Stop::Bad)?;
                real_parents(stage, &source).map_err(Stop::Bad)?;
                let from = stage.join(&source);
                if !fs::symlink_metadata(&from).is_ok_and(|m| m.is_file()) {
                    return Err(Stop::Bad(format!(
                        "refused the entry {}: a hard link to something that is not a file here",
                        rel.display()
                    )));
                }
                fs::hard_link(&from, &target).map_err(fail)?;
            }
            other => {
                return Err(Stop::Bad(format!(
                    "refused the entry {}: type {:?} is never restored",
                    rel.display(),
                    char::from(other)
                )))
            }
        }
        entries += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn sha256sum(data: &[u8]) -> String {
        let mut child = Command::new("sha256sum")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("sha256sum is on this machine");
        child.stdin.take().unwrap().write_all(data).unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8(out.stdout).unwrap()[..64].to_string()
    }

    /// Fails if the streaming hash depends on where its input was split.
    #[test]
    fn the_stream_hash_agrees_with_sha256sum_however_it_is_split() {
        for length in [0, 1, 55, 56, 57, 63, 64, 65, 119, 120, 128, 200] {
            let data: Vec<u8> = (0..length).map(|i| (i * 7 % 251) as u8).collect();
            let expected = sha256sum(&data);
            for split in 0..=length {
                let mut hasher = Sha256::new();
                hasher.update(&data[..split]);
                hasher.update(&data[split..]);
                assert_eq!(hex(&hasher.finish()), expected, "{length} split at {split}");
            }
        }
    }

    /// Fails if a PAX record's length stops counting its own digits, which GNU tar rejects.
    #[test]
    fn a_pax_record_states_its_own_length() {
        for size in [1, 90, 94, 95, 96, 97, 98, 99, 100, 990, 995, 996, 1000] {
            let record = pax_record("path", &vec![b'x'; size]);
            let (stated, _) = record.split_at(record.iter().position(|b| *b == b' ').unwrap());
            let stated: usize = std::str::from_utf8(stated).unwrap().parse().unwrap();
            assert_eq!(stated, record.len(), "value of {size} bytes");
        }
    }
}
