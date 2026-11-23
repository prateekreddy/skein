//! `skein migrate seal` and `unseal`: a finished backup, encrypted under a passphrase the owner holds.
//!
//! A backup holds conversations and `.env` files, so it should not leave the machine readable. The
//! encryption is a separate step over a finished file because `send` resumes per unit and `receive`
//! seeks, and neither survives a stream cipher threaded through it. The sealed file is a standard
//! age file in passphrase mode, so the `age` command line can open it too.
//!
//! Output reaches its name only after it verified. It is written to a private temp beside the
//! destination and linked into place, which cannot replace a file that appeared in the meantime.
//! An unseal that is interrupted would otherwise leave readable plaintext, so the temp is removed
//! on SIGINT, SIGTERM and SIGHUP as well as on every error. SIGKILL cannot be caught: what it leaves
//! is a 0600 file named `.<destination>.partial.<pid>.<n>`. The writer holds a lock on it for its
//! whole life and the kernel drops that lock on any death, so the next seal or unseal to the same
//! destination can tell a dead run's file from a live one and removes only the dead.

use super::available_kb;
use super::bundle::{BundleReader, Closing, Manifest, Stop, UnitHead, FORMAT, KIND_END};
use super::bundle::{KIND_MANIFEST, KIND_UNIT, MAGIC};
use crate::secret::create_private;
use crate::util::Sha256;
use age::secrecy::{ExposeSecret, SecretString};
use age::{scrypt, DecryptError, Decryptor, Encryptor};
use fs2::FileExt;
use std::cell::UnsafeCell;
use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering};
use std::time::SystemTime;

/// What every age file opens with.
const AGE_MAGIC: &[u8] = b"age-encryption.org/v1\n";
/// scrypt's cost, fixed rather than calibrated. age calibrates to the sealing machine and by
/// default refuses anything 16 times that, so a backup sealed on a fast Mac could be refused on the
/// slower machine it is being restored to. 2^18 takes about 256 MiB.
const WORK_FACTOR: u8 = 18;
/// The most a sealed file may ask for: 2^20 is about 1 GiB, which a forged header cannot exceed.
const MAX_WORK_FACTOR: u8 = 20;
/// A passphrase longer than this is a file named by mistake.
const MAX_PASSPHRASE: usize = 1024;
const BUFFER: usize = 1 << 20;

const SEAL_USAGE: &str = "usage: skein migrate seal <bundle> <sealed-file> [--passphrase-fd <n> | \
                          --passphrase-file <path>]   (to move the volume into a directory named \
                          seal, write ./seal)";
const UNSEAL_USAGE: &str = "usage: skein migrate unseal <sealed-file> <bundle> [--passphrase-fd \
                            <n> | --passphrase-file <path>]   (to move the volume into a \
                            directory named unseal, write ./unseal)";

/// Where the passphrase comes from. There is deliberately no argv or environment form.
#[derive(Debug, Clone, PartialEq)]
pub enum PassphraseFrom {
    Prompt,
    Fd(i32),
    File(PathBuf),
}

/// The two paths and the passphrase source, from what followed the verb.
fn parse_args<'a>(
    args: &'a [String],
    usage: &str,
) -> Result<(&'a str, &'a str, PassphraseFrom), String> {
    let mut paths = Vec::new();
    let mut from = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let next = match arg.as_str() {
            "--passphrase-fd" => {
                let fd = rest.next().and_then(|n| n.parse::<i32>().ok());
                PassphraseFrom::Fd(fd.filter(|n| *n >= 0).ok_or(usage)?)
            }
            "--passphrase-file" => PassphraseFrom::File(rest.next().ok_or(usage)?.into()),
            // The usage line, never the argument: this may be a passphrase typed in the wrong place.
            flag if flag.starts_with("--") => return Err(usage.into()),
            path => {
                paths.push(path);
                continue;
            }
        };
        if from.replace(next).is_some() {
            return Err(usage.into());
        }
    }
    match paths[..] {
        [from_path, to] => Ok((from_path, to, from.unwrap_or(PassphraseFrom::Prompt))),
        _ => Err(usage.into()),
    }
}

/// `skein migrate seal <bundle> <sealed-file> [...]`.
pub fn migrate_seal_command(args: &[String]) -> Result<String, String> {
    let (bundle, sealed, from) = parse_args(args, SEAL_USAGE)?;
    migrate_seal(Path::new(bundle), Path::new(sealed), &from)
}

/// `skein migrate unseal <sealed-file> <bundle> [...]`.
pub fn migrate_unseal_command(args: &[String]) -> Result<String, String> {
    let (sealed, bundle, from) = parse_args(args, UNSEAL_USAGE)?;
    migrate_unseal(Path::new(sealed), Path::new(bundle), &from)
}

// ---------------------------------------------------------------------------------------------
// What an interrupted run must put back
// ---------------------------------------------------------------------------------------------

struct SavedTty(UnsafeCell<MaybeUninit<libc::termios>>);
// SAFETY: written once before `TTY_FD` is published with Release, read only after Acquire.
unsafe impl Sync for SavedTty {}

static SAVED_TTY: SavedTty = SavedTty(UnsafeCell::new(MaybeUninit::uninit()));
static TTY_FD: AtomicI32 = AtomicI32::new(-1);
static TEMP_PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

/// Only async-signal-safe calls: `tcsetattr`, `unlink`, `signal` and `raise`.
extern "C" fn on_signal(signal: libc::c_int) {
    // SAFETY: the fd and termios were published before the handler could read them, and the path
    // is a leaked NUL-terminated string.
    unsafe {
        let fd = TTY_FD.load(Ordering::Acquire);
        if fd >= 0 {
            libc::tcsetattr(fd, libc::TCSANOW, (*SAVED_TTY.0.get()).as_ptr());
        }
        let path = TEMP_PATH.load(Ordering::Acquire);
        if !path.is_null() {
            libc::unlink(path);
        }
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

fn catch_signals() {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler above is async-signal-safe.
        unsafe { libc::signal(signal, handler) };
    }
}

/// The private file output grows in, removed unless it was placed.
struct Temp {
    path: PathBuf,
    /// Keeps the lock when the writer's handle is closed before the file is placed.
    _held: Option<File>,
}

/// The name of a temp for `destination`: `.<name>.partial.<pid>.<n>`.
fn is_temp_of(destination: &Path, found: &OsStr) -> bool {
    let name = destination
        .file_name()
        .unwrap_or(OsStr::new("backup"))
        .to_string_lossy();
    let prefix = format!(".{name}.partial.");
    let found = found.to_string_lossy();
    let Some(rest) = found.strip_prefix(&prefix) else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    rest.split_once('.')
        .is_some_and(|(pid, n)| digits(pid) && digits(n))
}

/// Remove what a killed run left for `destination`. A file whose lock cannot be taken belongs to a
/// run still alive, and a filesystem with no locks leaves everything in place.
fn remove_dead_temps(destination: &Path) {
    let Ok(entries) = fs::read_dir(parent_of(destination)) else {
        return;
    };
    // SAFETY: `geteuid` takes nothing and cannot fail.
    let me = unsafe { libc::geteuid() };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !is_temp_of(destination, &entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let plain = fs::symlink_metadata(&path)
            .is_ok_and(|meta| meta.file_type().is_file() && meta.uid() == me);
        if !plain {
            continue;
        }
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        else {
            continue;
        };
        if file.try_lock_exclusive().is_ok() && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        eprintln!("skein: removed {removed} unfinished file(s) left by an interrupted run");
    }
}

/// Lock a new temp. `false` means another run's sweep reached it first, so it must not be used.
fn hold(path: &Path, file: &File) -> Result<bool, io::Error> {
    if let Err(e) = file.try_lock_exclusive() {
        return if e.kind() == io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(e)
        };
    }
    let mine = file.metadata()?;
    Ok(fs::symlink_metadata(path).is_ok_and(|at| at.dev() == mine.dev() && at.ino() == mine.ino()))
}

impl Temp {
    fn beside(destination: &Path) -> Result<(Temp, File), String> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let name = destination
            .file_name()
            .unwrap_or(OsStr::new("backup"))
            .to_string_lossy();
        for _ in 0..8 {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let path =
                parent_of(destination).join(format!(".{name}.partial.{}.{n}", std::process::id()));
            let c_path = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| format!("{} is not a usable path", path.display()))?;
            // Armed before the file exists, so no moment has a file the handler would not remove.
            let old = TEMP_PATH.swap(c_path.into_raw(), Ordering::AcqRel);
            if !old.is_null() {
                // SAFETY: every value stored here came from `CString::into_raw`.
                drop(unsafe { CString::from_raw(old) });
            }
            catch_signals();
            let file = match create_private(&path) {
                Ok(file) => file,
                Err(e) => {
                    disarm();
                    return Err(format!("creating {}: {e}", path.display()));
                }
            };
            let held = match hold(&path, &file) {
                Ok(true) => file.try_clone().ok(),
                Ok(false) => {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                // No locks here. Nothing can sweep this file either, so it is safe to write.
                Err(_) => None,
            };
            return Ok((Temp { path, _held: held }, file));
        }
        disarm();
        Err(format!(
            "could not reserve a private file beside {}, so nothing was written.",
            destination.display()
        ))
    }

    /// Give it the destination's name, refusing if that name has been taken since the first check.
    fn place(self, destination: &Path) -> Result<(), String> {
        match fs::hard_link(&self.path, destination) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(exists_text(destination));
            }
            // exFAT and some network mounts have no hard links, and a backup drive is often one.
            Err(_) if fs::symlink_metadata(destination).is_ok() => {
                return Err(exists_text(destination));
            }
            Err(_) => {
                fs::rename(&self.path, destination)
                    .map_err(|e| format!("placing {}: {e}", destination.display()))?;
                if let Ok(dir) = File::open(parent_of(destination)) {
                    let _ = dir.sync_all();
                }
                return Ok(());
            }
        }
        if let Ok(dir) = File::open(parent_of(destination)) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        disarm();
    }
}

fn disarm() {
    let old = TEMP_PATH.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if !old.is_null() {
        // SAFETY: every value stored here came from `CString::into_raw`.
        drop(unsafe { CString::from_raw(old) });
    }
}

// ---------------------------------------------------------------------------------------------
// The passphrase
// ---------------------------------------------------------------------------------------------

/// One line, read a byte at a time so nothing past it is consumed from a shared descriptor.
fn read_line(input: &mut impl Read) -> Result<SecretString, String> {
    let mut line = Vec::with_capacity(MAX_PASSPHRASE + 1);
    let mut byte = [0u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if line.len() == MAX_PASSPHRASE => {
                return Err(format!(
                    "the passphrase is longer than {MAX_PASSPHRASE} bytes, so nothing was written."
                ))
            }
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("reading the passphrase: {e}")),
        }
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    if line.is_empty() {
        return Err("the passphrase is empty, so nothing was written.".into());
    }
    let text = String::from_utf8(line)
        .map_err(|_| "the passphrase is not UTF-8, so nothing was written.".to_string())?;
    Ok(SecretString::from(text))
}

fn ask_once(tty: &mut File, prompt: &str) -> Result<SecretString, String> {
    let fd = tty.as_raw_fd();
    let no_terminal = || {
        "no terminal to ask for a passphrase on, so nothing was written. Pass --passphrase-fd or \
         --passphrase-file."
            .to_string()
    };
    let mut saved = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `fd` is open and `saved` is a valid out-pointer.
    if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
        return Err(no_terminal());
    }
    // SAFETY: tcgetattr succeeded, so it is initialised.
    let saved = unsafe { saved.assume_init() };
    // SAFETY: no handler reads this until `TTY_FD` is published on the next line.
    unsafe { (*SAVED_TTY.0.get()).write(saved) };
    TTY_FD.store(fd, Ordering::Release);
    catch_signals();
    let mut quiet = saved;
    quiet.c_lflag &= !libc::ECHO;
    quiet.c_lflag |= libc::ECHONL;
    // SAFETY: `fd` is open and `quiet` is a valid termios.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } != 0 {
        TTY_FD.store(-1, Ordering::Release);
        return Err(no_terminal());
    }
    // The prompt follows the switch, so anything typed after seeing it cannot echo.
    let answer = tty
        .write_all(prompt.as_bytes())
        .and_then(|()| tty.flush())
        .map_err(|e| format!("writing the prompt: {e}"))
        .and_then(|()| read_line(tty));
    // SAFETY: `fd` is open and `saved` is what tcgetattr returned.
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
    TTY_FD.store(-1, Ordering::Release);
    answer
}

/// Refuse a passphrase file kept where the sealed file is, which is the one place it protects
/// nothing.
fn refuse_beside(passphrase: &Path, files: &[&Path]) -> Result<(), String> {
    let dir_of = |path: &Path| fs::canonicalize(parent_of(path)).ok();
    let here = dir_of(passphrase);
    if here.is_some() && files.iter().any(|file| dir_of(file) == here) {
        return Err(format!(
            "{} is in the same directory as the backup, where it would open it for anyone \
             holding the two. Keep it somewhere else. Nothing was written.",
            passphrase.display()
        ));
    }
    Ok(())
}

fn passphrase(from: &PassphraseFrom, twice: bool, files: &[&Path]) -> Result<SecretString, String> {
    match from {
        PassphraseFrom::Prompt => {
            let mut tty = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .map_err(|_| {
                    "no terminal to ask for a passphrase on, so nothing was written. Pass \
                     --passphrase-fd or --passphrase-file."
                        .to_string()
                })?;
            let first = ask_once(&mut tty, "Passphrase: ")?;
            if twice {
                let second = ask_once(&mut tty, "Passphrase again: ")?;
                if first.expose_secret() != second.expose_secret() {
                    return Err("the two passphrases differ, so nothing was written.".into());
                }
            }
            Ok(first)
        }
        PassphraseFrom::Fd(fd) => {
            // SAFETY: F_GETFD only asks whether the number is open.
            if unsafe { libc::fcntl(*fd, libc::F_GETFD) } < 0 {
                return Err(format!(
                    "file descriptor {fd} is not open, so nothing was written."
                ));
            }
            // SAFETY: the descriptor is open, was handed to this process for this read, and
            // nothing else here uses it.
            let mut file = unsafe { File::from_raw_fd(*fd) };
            read_line(&mut file)
        }
        PassphraseFrom::File(path) => {
            refuse_beside(path, files)?;
            let mut file =
                File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
            read_line(&mut file)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The files
// ---------------------------------------------------------------------------------------------

fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn exists_text(path: &Path) -> String {
    format!(
        "{} already exists and is never overwritten, so nothing was written. Name a new file.",
        path.display()
    )
}

fn check_destination(path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(exists_text(path));
    }
    let parent = parent_of(path);
    if !parent.is_dir() {
        return Err(format!(
            "{} is not a directory, so nothing was written.",
            parent.display()
        ));
    }
    Ok(())
}

fn check_room(parent: &Path, bytes: u64) -> Result<(), String> {
    // A chunk of 64 KiB gains a 16-byte tag, and the header is far under the extra MiB.
    let need_kb = (bytes + bytes / 4096 + (1 << 20)).div_ceil(1024);
    match available_kb(parent) {
        Some(free) if free < need_kb => Err(format!(
            "this needs about {need_kb} KB and {} has {free} KB free, so nothing was written.",
            parent.display()
        )),
        Some(_) => Ok(()),
        None => Err(format!(
            "the free space at {} could not be read, so nothing was written.",
            parent.display()
        )),
    }
}

/// Does this file open as an age file? `receive` asks, to name the step that was missed.
pub(super) fn is_sealed(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut head = Vec::with_capacity(AGE_MAGIC.len());
    file.take(AGE_MAGIC.len() as u64)
        .read_to_end(&mut head)
        .is_ok()
        && head == AGE_MAGIC
}

/// Every frame of a backup is present and in order, read without hashing the data. A file `send`
/// has not finished fails here, and sealing one would invite deleting the only whole copy.
fn check_whole(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let mut reader = BundleReader::open(BufReader::with_capacity(64 * 1024, file))?;
    let (payload, _) = reader.frame(KIND_MANIFEST, "the manifest")?;
    let manifest: Manifest = serde_json::from_slice(&payload)
        .map_err(|_| Stop::Bad("the manifest cannot be read".into()))?;
    if manifest.format != FORMAT {
        return Err(Stop::Bad(format!(
            "its manifest says format {} and this skein reads format {FORMAT}",
            manifest.format
        ))
        .into());
    }
    let mut trailers = Vec::with_capacity(manifest.units.len());
    for (index, line) in manifest.units.iter().enumerate() {
        let what = format!("unit {}", index + 1);
        let (payload, _) = reader.frame(KIND_UNIT, &what)?;
        let head: UnitHead = serde_json::from_slice(&payload)
            .map_err(|_| Stop::Bad(format!("{what} cannot be read")))?;
        if head.index != index as u64 || head.name != line.name {
            return Err(Stop::Bad(format!("{what} is out of place in this file")).into());
        }
        trailers.push(reader.skip_unit(&what)?);
    }
    let (payload, _) = reader.frame(KIND_END, "the closing manifest")?;
    let closing: Closing = serde_json::from_slice(&payload)
        .map_err(|_| Stop::Bad("the closing manifest cannot be read".into()))?;
    let agrees = closing.units.len() == trailers.len()
        && closing
            .units
            .iter()
            .zip(&trailers)
            .all(|(unit, trailer)| unit.sha256 == trailer.sha256);
    if agrees {
        Ok(())
    } else {
        Err(Stop::Bad("the closing manifest disagrees with the units in this file".into()).into())
    }
}

impl From<Stop> for String {
    fn from(stop: Stop) -> String {
        match stop {
            Stop::Cut(why) | Stop::Bad(why) => why,
        }
    }
}

/// Which side of a copy failed. A failed read of a sealed file is damage, and a failed write is
/// this machine's disk, and telling the owner their backup is damaged for the second is false.
enum Pumped {
    Read(io::Error),
    Write(io::Error),
}

/// Copy everything, returning the SHA-256 of what passed through.
fn pump(input: &mut impl Read, output: &mut impl Write) -> Result<[u8; 32], Pumped> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; BUFFER];
    loop {
        let got = match input.read(&mut buffer) {
            Ok(got) => got,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Pumped::Read(e)),
        };
        if got == 0 {
            return Ok(hasher.finish());
        }
        hasher.update(&buffer[..got]);
        output.write_all(&buffer[..got]).map_err(Pumped::Write)?;
    }
}

fn identity(passphrase: &SecretString) -> scrypt::Identity {
    let mut identity = scrypt::Identity::new(passphrase.clone());
    identity.set_max_work_factor(MAX_WORK_FACTOR);
    identity
}

fn refused_open(error: DecryptError) -> String {
    match error {
        DecryptError::ExcessiveWork { .. } => {
            "refused: this file asks for more key-derivation work than skein accepts, so nothing \
             was written."
                .into()
        }
        DecryptError::InvalidHeader | DecryptError::UnknownFormat => {
            "refused: its header is damaged, so nothing was written.".into()
        }
        // age cannot tell a wrong passphrase from a damaged key line, so neither does this.
        _ => "refused: wrong passphrase, or the file's header is damaged. Nothing was written."
            .into(),
    }
}

fn open_sealed<R: Read>(input: R, phrase: &SecretString) -> Result<impl Read, String> {
    let decryptor = Decryptor::new(input).map_err(refused_open)?;
    if !decryptor.is_scrypt() {
        return Err(
            "refused: this age file is not sealed with a passphrase, so skein cannot \
                    open it. Nothing was written."
                .into(),
        );
    }
    let identity = identity(phrase);
    decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(refused_open)
}

fn fingerprint(path: &Path) -> Result<(u64, Option<SystemTime>), String> {
    let meta = fs::metadata(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok((meta.len(), meta.modified().ok()))
}

/// Encrypt the finished backup at `bundle` to `sealed`, leaving the backup where it is.
pub fn migrate_seal(bundle: &Path, sealed: &Path, from: &PassphraseFrom) -> Result<String, String> {
    remove_dead_temps(sealed);
    check_destination(sealed)?;
    if is_sealed(bundle) {
        return Err(format!(
            "{} is already sealed, so nothing was written.",
            bundle.display()
        ));
    }
    let before = fingerprint(bundle)?;
    check_whole(bundle).map_err(|why| {
        format!(
            "{} is not a whole skein backup: {why}. Nothing was written.",
            bundle.display()
        )
    })?;
    check_room(parent_of(sealed), before.0)?;
    let phrase = passphrase(from, true, &[bundle, sealed])?;

    let mut source =
        File::open(bundle).map_err(|e| format!("opening {}: {e}", bundle.display()))?;
    let (temp, file) = Temp::beside(sealed)?;
    let writing = |e: io::Error| format!("writing {}: {e}", temp.path.display());
    let mut recipient = scrypt::Recipient::new(phrase.clone());
    recipient.set_work_factor(WORK_FACTOR);
    let encryptor = Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
        .map_err(|e| format!("preparing the encryption: {e}"))?;
    let mut stream = encryptor
        .wrap_output(BufWriter::with_capacity(BUFFER, file))
        .map_err(writing)?;
    let plain_hash = pump(&mut source, &mut stream).map_err(|failed| match failed {
        Pumped::Read(e) => format!("reading {}: {e}. Nothing was written.", bundle.display()),
        Pumped::Write(e) => writing(e),
    })?;
    let file = stream
        .finish()
        .map_err(writing)?
        .into_inner()
        .map_err(|e| writing(e.into_error()))?;
    file.sync_all().map_err(writing)?;
    drop(file);

    if fingerprint(bundle)? != before {
        return Err(format!(
            "{} changed while it was being sealed, so nothing was written.",
            bundle.display()
        ));
    }
    // The owner is told they may delete the plain file, so what was written is read back first.
    let check = File::open(&temp.path).map_err(writing)?;
    let mut opened = open_sealed(BufReader::with_capacity(BUFFER, check), &phrase)?;
    let sealed_hash = pump(&mut opened, &mut io::sink()).map_err(|failed| {
        let (Pumped::Read(e) | Pumped::Write(e)) = failed;
        format!("the sealed copy did not read back: {e}. Nothing was written.")
    })?;
    if sealed_hash != plain_hash {
        return Err("the sealed copy did not match the backup, so nothing was written.".into());
    }
    temp.place(sealed)?;
    Ok(format!(
        "Sealed {} to {}\nIt was read back with this passphrase and matches byte for byte. The \
         passphrase was not stored anywhere, and without it this file cannot be opened.\n\nThe \
         plain backup is still at {} and is NOT encrypted. Remove it once the sealed file is \
         safely stored.\nTo restore: skein migrate unseal <sealed-file> <bundle>, then skein \
         migrate receive <bundle> --into <empty-dir>.",
        bundle.display(),
        sealed.display(),
        bundle.display(),
    ))
}

/// Decrypt `sealed` to a new backup file at `bundle`.
pub fn migrate_unseal(
    sealed: &Path,
    bundle: &Path,
    from: &PassphraseFrom,
) -> Result<String, String> {
    // First, so a run refused below still clears the plaintext a killed one left.
    remove_dead_temps(bundle);
    check_destination(bundle)?;
    let mut source =
        File::open(sealed).map_err(|e| format!("opening {}: {e}", sealed.display()))?;
    let size = source.metadata().map_or(0, |m| m.len());
    let mut head = Vec::with_capacity(AGE_MAGIC.len());
    (&mut source)
        .take(AGE_MAGIC.len() as u64)
        .read_to_end(&mut head)
        .map_err(|e| format!("reading {}: {e}", sealed.display()))?;
    if head != AGE_MAGIC {
        let why = if head.starts_with(MAGIC) {
            "it is a plain backup, which `skein migrate receive` takes as it is"
        } else {
            "it is not a sealed skein backup"
        };
        return Err(format!("{}: {why}. Nothing was written.", sealed.display()));
    }
    check_room(parent_of(bundle), size)?;
    let phrase = passphrase(from, false, &[sealed, bundle])?;

    let input = BufReader::with_capacity(BUFFER, io::Cursor::new(head).chain(source));
    // Key derivation happens here, so a wrong passphrase is refused before any file exists.
    let mut opened = open_sealed(input, &phrase)?;
    let (temp, file) = Temp::beside(bundle)?;
    let writing = |e: io::Error| format!("writing {}: {e}", temp.path.display());
    let mut output = BufWriter::with_capacity(BUFFER, file);
    pump(&mut opened, &mut output).map_err(|failed| match failed {
        Pumped::Read(e) => format!(
            "refused: {} is damaged or cut short ({e}). Nothing was written.",
            sealed.display()
        ),
        Pumped::Write(e) => writing(e),
    })?;
    let file = output.into_inner().map_err(|e| writing(e.into_error()))?;
    file.sync_all().map_err(writing)?;
    drop(file);
    check_whole(&temp.path).map_err(|why| {
        format!(
            "{} decrypted, but what it holds is not a whole skein backup: {why}. Nothing was \
             written.",
            sealed.display()
        )
    })?;
    temp.place(bundle)?;
    Ok(format!(
        "Unsealed {} to {}\nIts contents authenticated against the passphrase. This file is NOT \
         encrypted, so remove it once it is restored.\nTo restore: skein migrate receive <bundle> \
         --into <empty-dir>.",
        sealed.display(),
        bundle.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    /// Fails if a `--passphrase <value>` form is ever accepted, or the refusal repeats the value.
    #[test]
    fn a_passphrase_on_the_command_line_is_refused_without_repeating_it() {
        let args = strings(&["a", "b", "--passphrase", "skein-test-typed-here"]);
        let refused = parse_args(&args, SEAL_USAGE).unwrap_err();
        assert_eq!(refused, SEAL_USAGE);
        assert!(!refused.contains("skein-test-typed-here"));
    }

    /// Fails if a second source silently replaces the first.
    #[test]
    fn two_passphrase_sources_are_refused() {
        let args = strings(&["a", "b", "--passphrase-fd", "3", "--passphrase-file", "p"]);
        assert!(parse_args(&args, SEAL_USAGE).is_err());
    }

    /// Fails if a trailing newline became part of the key, so `echo` and a prompt would disagree.
    #[test]
    fn one_line_is_read_and_its_newline_is_not_part_of_it() {
        let mut input: &[u8] = b"skein-test-line\r\nnext";
        let line = read_line(&mut input).unwrap();
        assert_eq!(line.expose_secret(), "skein-test-line");
        assert_eq!(input, b"next", "read past the line");
    }
}
