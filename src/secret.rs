//! One writer for every credential skein keeps, and one type that carries it.
//!
//! # Why a type at all
//!
//! Every credential in this tree used to be a `String`, and a `String` is willing. It prints in a
//! panic, in a `{:?}` of the struct that holds it, in a log line somebody added while debugging
//! something else; it serialises into JSON the moment it is a field of anything that derives
//! `Serialize`; it leaves its bytes in the allocator when it drops. None of those is a bug anybody
//! writes on purpose — each one is a line of code that looks correct and is correct about
//! everything except what the value happens to be.
//!
//! [`Secret`] is the same bytes with those four doors shut: [`std::fmt::Display`] and
//! [`std::fmt::Debug`] both print `<secret>`, there is no `Serialize` impl and there must never be
//! one, and [`Drop`] overwrites the buffer. Reaching the bytes takes [`Secret::expose`], which is
//! spelled that way so it reads as a decision at the call site.
//!
//! # Why one writer
//!
//! Nine places in `src/` wrote a 0600 credential by hand, in four different orders, and the
//! differences were not deliberate: some wrote the temp with `fs::write` and chmod'd it after —
//! which leaves the token readable at the process umask for as long as that takes — while others
//! got it right and said so in a comment three files away from the one that did not. The ordering
//! rule cannot be applied consistently by being written down nine times.
//!
//! So [`write`] is the only place that creates a credential file, and it does three things the
//! hand-rolled versions each did some of:
//!
//! * the temp is **created with mode 0600**, not chmod'd to it, so there is no instant in which it
//!   is readable by anybody else;
//! * the temp is created `O_EXCL`, so a symlink planted at its path is refused rather than
//!   written through — see [`create_private`] for why that, rather than a hand-spelled
//!   `O_NOFOLLOW`, is the flag doing the work;
//! * the bytes reach the disk before the rename, and the rename is what publishes them — so a
//!   crash leaves the whole old file rather than a truncated new one, and the destination is never
//!   observed at the wrong mode because it is never created at the wrong mode.
//!
//! A rename does not follow a symlink at the destination either: it replaces the link. That is
//! what makes the temp-then-rename shape the right one for a file whose directory somebody else
//! may be able to write.
//!
//! # Why this duplicates `warden/src/secret.rs`, and must
//!
//! The patterns here are lifted from `warden/src/secret.rs` — 32 hex characters read from
//! `/dev/urandom` rather than from a crate, a file created *with* its mode, a comparison that
//! looks at every byte. That module has been through review and this one should not diverge from
//! it by accident.
//!
//! It is still a second copy on purpose. The warden is a separate crate with an empty depends-on
//! column (architecture §14, and the warden's own §8): it is the process a compromised skein has
//! to get past, so a shared library between the two would be a shared blast radius, and the
//! warden's short dependency list is an argument it makes about itself. Sharing this code would
//! spend that argument to save sixty lines. **Do not "fix" the duplication** — change both, and
//! say in each that the other exists.
//!
//! # Unix only, and not conditionally
//!
//! There are no `#[cfg(unix)]` guards below. skein is a Linux program — it spells `nsenter`,
//! `bwrap` and `cgroup.kill` — and a guard here would only mean that on a platform the rest of the
//! crate cannot build for, the credential writer would silently be the one without the mode.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// A credential, and the four things it will not do.
///
/// It does not print itself, serialise itself, clone itself, or leave its bytes behind when it
/// drops. Everything else about it is a `String`, reachable through [`Secret::expose`].
///
/// **No `Serialize`, and no `Deserialize` either.** The absence is the feature, and
/// `a_secret_cannot_be_serialised` fails if anybody adds one. A `Deserialize` would be the same
/// hole in the other direction: it is what makes a credential arrive inside a request body that
/// something else then logs.
///
/// **No `Clone`.** Not because a copy is unsafe, but because every copy is another buffer to
/// scrub and this type's whole claim is about buffers. Where a caller genuinely needs two, it
/// says so with [`Secret::new`] and the reader can see it.
pub struct Secret(String);

impl Secret {
    /// Take ownership of a credential that arrived from somewhere else.
    ///
    /// The ingest point, and there is deliberately only one: a token typed into the cockpit, read
    /// out of the environment, or returned by GitHub is a `String` before skein ever sees it, and
    /// pretending otherwise would mean a second constructor per source.
    ///
    /// Trimmed, because every hand-rolled writer this replaced trimmed — a token pasted into a
    /// form arrives with the newline the paste carried, and a credential that differs from itself
    /// by trailing space is a comparison failure nobody can see.
    pub fn new(value: impl AsRef<str>) -> Secret {
        Secret(value.as_ref().trim().to_string())
    }

    /// The bytes, for the one call that has to have them.
    ///
    /// Named `expose` rather than `as_str` so that grepping for where a credential leaves this type
    /// is a question with an answer, and so that the call site reads as the decision it is.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Nothing was there. Distinct from "there is a secret and it is short".
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Is `offered` this secret? Compared in time that does not depend on where they differ.
    ///
    /// A comparison that returns early on the first difference tells the caller how much of its
    /// guess was right, and a secret guessable one byte at a time is not one. The length is
    /// compared as data for the same reason — for a fixed-width token it leaks nothing, and this
    /// is not the place to depend on that staying true.
    ///
    /// `offered` is taken exactly as given; callers that accept a header or a cookie trim it
    /// themselves, because *where* the trimming happens is part of what they are asserting.
    pub fn same(&self, offered: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), offered.as_bytes());
        let mut diff = (a.len() ^ b.len()) as u8;
        for i in 0..a.len().max(b.len()) {
            diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
        }
        diff == 0
    }

    /// Overwrite the buffer in place. What [`Drop`] does, factored out so a test can watch it.
    ///
    /// In place, through the existing allocation — assigning a fresh `String` would drop the old
    /// buffer with the credential still in it, which is the thing this exists to avoid.
    fn scrub(&mut self) {
        // SAFETY: NUL is valid UTF-8, so a buffer filled with zeroes is still a valid `String`.
        unsafe { self.0.as_bytes_mut() }.fill(0);
        // The write has no observable effect by the abstract machine's reckoning, which is exactly
        // the licence a compiler needs to delete it. A fence is not a guarantee — see the note on
        // `dropping_a_secret_scrubs_its_buffer` — but it is the strongest thing available without
        // a dependency.
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        #[cfg(test)]
        SCRUBS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.scrub();
    }
}

/// `<secret>`, in a panic, a log line, a `{}` and a `{:?}` alike.
impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<secret>")
    }
}

/// Deliberately the same as [`std::fmt::Display`], and deliberately not derived.
///
/// A derived `Debug` is how the value ends up in the `{:?}` of the struct that holds it, which is
/// the most common way a credential reaches a log — nobody formats the token, they format the
/// request that carries it.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<secret>")
    }
}

/// How many times [`Secret::scrub`] has run, so a test can prove [`Drop`] reaches it.
#[cfg(test)]
static SCRUBS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Mint a new credential at `path`: `bytes` bytes from the kernel, hex, written 0600.
///
/// `/dev/urandom` rather than a random-number crate, for the reason `warden/src/secret.rs` gives:
/// this is the whole of what such a dependency would be used for. An unreadable `/dev/urandom` is
/// an error rather than a weaker secret — a credential nobody can mint is a failure somebody sees,
/// and a predictable one is not.
///
/// The value is returned as well as written, because every caller wants both: the file is what the
/// next process reads, and the return is what this one uses.
pub fn mint(path: &Path, bytes: usize) -> Result<Secret, String> {
    let mut raw = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut raw))
        .map_err(|e| format!("no randomness available for {}: {e}", path.display()))?;
    let value = Secret(raw.iter().map(|b| format!("{b:02x}")).collect());
    raw.fill(0);
    write(path, &value)?;
    Ok(value)
}

/// Write `value` to `path`, owner-readable from the moment it exists.
///
/// The parent directory is **not** created. Every caller already knows where its own state goes,
/// and some of them want the directory at a mode of their own — `tracking`'s `tokens/` is 0700 —
/// so a directory created quietly here would be one created at the wrong mode there.
pub fn write(path: &Path, value: &Secret) -> Result<(), String> {
    write_bytes(path, value.expose().as_bytes())
}

/// [`write`], for a credential whose bytes are not a text token.
///
/// One caller: the fleet's kept copy of an agent login, which is a JSON document that has to land
/// byte-for-byte as it came out of the sandbox. Passing it through [`Secret`] would trim it, and a
/// credential file that differs from its source by a newline is a difference something downstream
/// will eventually compare.
///
/// Not a hole in the type, and worth saying why: what [`Secret`] protects is the value in memory —
/// printing, serialising, scrubbing — and none of that is what this function does. What it shares
/// with [`write`] is the *file* discipline, which is the part that must not be written twice.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    // TODO(SKEIN-518, secrets Rule 1): refuse a `path` with no `private` component, as a
    // debug-time assertion plus a test. Rule 1 puts every skein-only credential under
    // `$SKEIN_HOME/private/` and `/boxes/.skein/private/`, and the directory is then the cover that
    // makes the mode redundant — an assertion here is what stops the next credential being written
    // outside it.
    //
    // It cannot go in yet, and not for a tidiness reason: **nothing is under `private/` today.**
    // Every call site this function has — `api-token`, `fleet-agent.token`, `github-pats/<id>`,
    // `github-read-token`, `tokens/<id>`, `git-tokens/<repo>`, the fleet's kept login — writes to
    // the path it has always written to, and the assertion would refuse all of them. Moving them is
    // a change to the on-disk layout of a live fleet and needs a migration, which is its own wave.
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory to write into", path.display()))?;
    let tmp = temp_beside(path);
    let flushed = (|| -> std::io::Result<()> {
        let mut file = create_private(&tmp)?;
        file.write_all(bytes)?;
        // Before the rename, not after: a rename that publishes bytes still only in the page cache
        // can survive a crash as a present, zero-length file — and a zero-length credential reads
        // as "never signed in" rather than as damage.
        file.sync_all()
    })();
    if let Err(e) = flushed {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("writing {}: {e}", tmp.display()));
    }
    // A rename REPLACES a symlink at the destination rather than writing through it, which is why
    // the destination needs no `O_NOFOLLOW` of its own.
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("placing {}: {e}", path.display())
    })?;
    let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    Ok(())
}

/// The credential at `path`, or `None`.
///
/// Three answers collapse into `None` on purpose: the file is absent, the file is empty, or the
/// file is whitespace. All three mean the same thing to every caller — there is no credential here
/// — and the writers above never produce the second or the third.
///
/// An **unreadable** file is an `Err` and never `None`. That distinction is the whole reason this
/// returns a `Result` at all: a permission fault on `$SKEIN_HOME` that read as "no token" would
/// quietly re-open every route the token exists to close.
pub fn read(path: &Path) -> Result<Option<Secret>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let value = Secret::new(raw);
            Ok((!value.is_empty()).then_some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

/// Remove the credential at `path`. Already gone is success.
pub fn forget(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("clearing {}: {e}", path.display())),
    }
}

/// A temp in the destination's own directory, so the rename that follows is within one filesystem.
///
/// pid **and** a per-call counter: a pid-only name lets two threads of one process writing into the
/// same directory clobber each other's temp mid-write and rename the wrong bytes into place — the
/// bug `util::write_atomic` already carries a comment about.
fn temp_beside(path: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("secret");
    let dir = path.parent().unwrap_or(Path::new("."));
    dir.join(format!(".{name}.tmp.{}.{n}", std::process::id()))
}

/// A new file that is 0600 **before** it holds anything, and is never somebody else's symlink.
///
/// `mode` is the *create* mode, so the file is owner-only at birth rather than after a chmod a
/// reader can win the race to. `create_new` is `O_CREAT | O_EXCL`, and POSIX says that combination
/// fails when the trailing component is a symbolic link — "regardless of the contents of the
/// symbolic link", which is the whole of what `O_NOFOLLOW` would add here.
///
/// **`O_NOFOLLOW` is deliberately not spelled out beside it**, and the reason is worth keeping: the
/// constant is per-architecture, not per-OS. `0o400000` on x86-64 is `O_LARGEFILE` on aarch64,
/// where `O_NOFOLLOW` is `0o100000` — so a hand-written value taken from the common header is a
/// protection that is silently absent on half the machines skein runs on, and `libc` is a
/// dependency this crate does not otherwise have. `a_symlink_at_the_temp_path_is_not_followed`
/// checks the property that is actually here rather than the flag that would imply it.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "skein-secret-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Every way a value gets printed in this tree, and none of them is the credential.
    ///
    /// Fails if `Display` or `Debug` is ever changed to show the bytes — including by deriving
    /// `Debug`, which is the way it would actually happen.
    #[test]
    fn a_secret_prints_as_a_placeholder_everywhere() {
        let secret = Secret::new("ghp_the_actual_credential");
        let displayed = format!("{secret}");
        let debugged = format!("{secret:?}");
        // A struct that holds one, because a derived `Debug` on the ENCLOSING type is how a
        // credential usually reaches a log: nobody formats the token, they format the request.
        // `dead_code` because neither field is ever read by name — which is exactly the point:
        // what reads them is the derived `Debug`, and that is the path a credential takes into a
        // log.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Request {
            url: &'static str,
            token: Secret,
        }
        let request = Request {
            url: "https://api.github.com",
            token: Secret::new("ghp_the_actual_credential"),
        };
        let enclosing = format!("{request:?}");
        // A panic message, which is the one place a value is printed by code nobody wrote.
        let panicked = std::panic::catch_unwind(|| panic!("{}", Secret::new("ghp_in_a_panic")))
            .expect_err("the panic did not happen, so nothing was formatted");
        let panicked = panicked
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();

        for (what, text) in [
            ("Display", &displayed),
            ("Debug", &debugged),
            ("the enclosing struct's Debug", &enclosing),
            ("a panic message", &panicked),
        ] {
            assert!(
                !text.contains("ghp_"),
                "{what} printed the credential itself: {text}"
            );
            assert!(text.contains("<secret>"), "{what} printed {text}");
        }
        assert!(enclosing.contains("api.github.com"), "{enclosing}");
    }

    /// `Secret` has no `Serialize`, and this is the assertion that a new one would fail.
    ///
    /// Autoref specialisation: `answer()` resolves to the `ViaSerialize` impl when the type has a
    /// `Serialize` and falls back to `ViaFallback` when it does not, so what is asserted is a fact
    /// about the trait impls rather than about any particular value. `String` is the control —
    /// without it this test would also pass if the probe were simply broken.
    ///
    /// Spelled out at each call rather than wrapped in a `serializable::<T>()` helper, and that is
    /// not a style choice: inside a generic function `T: Serialize` is not known, so the helper
    /// would answer `false` for **every** type — a test that passes because it can only pass.
    // `needless_borrow` is wrong here and load-bearing: the double `&` is what makes method
    // resolution reach `ViaSerialize` before `ViaFallback`, so removing it makes the probe answer
    // `false` for every type — including the `String` control, which is how this was found.
    #[allow(clippy::needless_borrow)]
    #[test]
    fn a_secret_cannot_be_serialised() {
        use std::marker::PhantomData;
        struct Probe<T>(PhantomData<T>);
        trait ViaSerialize {
            fn serializable(&self) -> bool {
                true
            }
        }
        impl<T: serde::Serialize> ViaSerialize for &Probe<T> {}
        trait ViaFallback {
            fn serializable(&self) -> bool {
                false
            }
        }
        impl<T> ViaFallback for Probe<T> {}

        assert!(
            (&&Probe::<String>(PhantomData)).serializable(),
            "the probe itself is broken"
        );
        assert!(
            !(&&Probe::<Secret>(PhantomData)).serializable(),
            "`Secret` grew a Serialize impl, so every struct holding one now writes the \
             credential into whatever JSON it is part of"
        );

        // And the concrete consequence, in the shape it would actually take: the enclosing struct
        // serialises, and the credential is not in what it produces because it cannot be a field
        // that serde will look at.
        #[derive(serde::Serialize)]
        #[allow(dead_code)] // never read, and never serialised — the whole assertion below
        struct Stored {
            id: &'static str,
            #[serde(skip)]
            token: Secret,
        }
        let json = serde_json::to_string(&Stored {
            id: "mine",
            token: Secret::new("ghp_the_actual_credential"),
        })
        .unwrap();
        assert_eq!(json, r#"{"id":"mine"}"#);
    }

    /// The file is owner-only from the instant it exists, not from the chmod after.
    ///
    /// Asserted on the create itself, before a byte is written, because that is where the two
    /// implementations differ: `File::create` here reads 0644 under the usual umask and only
    /// becomes 0600 later, which is a window a reader can win.
    #[test]
    fn the_file_is_owner_only_before_it_holds_anything() {
        let dir = scratch("mode");
        let path = dir.join("brand-new");
        let file = create_private(&path).expect("a fresh path is creatable");
        assert_eq!(
            mode_of(&path),
            0o600,
            "the credential file was readable by somebody else for the length of a write"
        );
        drop(file);

        // And the finished article, through the public door.
        let placed = dir.join("api-token");
        write(&placed, &Secret::new("ghp_written")).unwrap();
        assert_eq!(mode_of(&placed), 0o600);
        assert_eq!(
            read(&placed).unwrap().unwrap().expose(),
            "ghp_written",
            "the bytes did not survive the temp and the rename"
        );
        // Nothing left behind: a temp nobody renames is a live credential nobody will clean up.
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(strays.is_empty(), "temp files left behind: {strays:?}");
    }

    /// A symlink at the destination is REPLACED, and what it pointed at is untouched.
    ///
    /// The attack this closes: a directory somebody else can write gets a link from the credential's
    /// name to a file elsewhere, and the next write puts a live token there instead. `fs::write`
    /// straight to the path does exactly that; a rename does not.
    #[test]
    fn a_symlink_at_the_destination_is_replaced_rather_than_followed() {
        let dir = scratch("dst-link");
        let outside = scratch("dst-link-outside").join("somebody-elses-file");
        std::fs::write(&outside, "not a credential").unwrap();
        let path = dir.join("api-token");
        std::os::unix::fs::symlink(&outside, &path).unwrap();

        write(&path, &Secret::new("ghp_written")).unwrap();

        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "not a credential",
            "the write followed the link and put a live credential outside the directory"
        );
        assert!(!path.is_symlink(), "the link survived the write");
        assert_eq!(read(&path).unwrap().unwrap().expose(), "ghp_written");
    }

    /// A symlink at the *temp* path is refused rather than written through.
    ///
    /// The temp name is unpredictable, so this drives [`create_private`] at the path directly —
    /// which is the same call [`write`] makes and the only part of it a planted link could catch.
    ///
    /// The second half is the presence-before-absence rule: the same directory, the same planted
    /// link, opened the way an ordinary create would open it, **does** write through to the
    /// outside file. Without that, a refusal here would prove nothing about whether `create_new`
    /// is what is doing the refusing.
    #[test]
    fn a_symlink_at_the_temp_path_is_not_followed() {
        let dir = scratch("tmp-link");
        let outside_dir = scratch("tmp-link-outside");

        let guarded = outside_dir.join("guarded");
        std::fs::write(&guarded, "not a credential").unwrap();
        let tmp = dir.join(".api-token.tmp.1.0");
        std::os::unix::fs::symlink(&guarded, &tmp).unwrap();
        let refused = create_private(&tmp).expect_err("the planted symlink was opened");
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::AlreadyExists,
            "refused for the wrong reason: {refused:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&guarded).unwrap(),
            "not a credential",
            "the create followed the link and wrote outside the directory"
        );

        let clobbered = outside_dir.join("clobbered");
        std::fs::write(&clobbered, "not a credential").unwrap();
        let naive = dir.join(".api-token.tmp.2.0");
        std::os::unix::fs::symlink(&clobbered, &naive).unwrap();
        std::fs::write(&naive, "a live credential").expect("an ordinary write");
        assert_eq!(
            std::fs::read_to_string(&clobbered).unwrap(),
            "a live credential",
            "an ordinary create did NOT follow the link either, so the assertion above is not \
             about `create_new` and proves nothing"
        );
    }

    /// Absent is `None`; unreadable is an error, and never quietly "no credential".
    #[test]
    fn a_missing_secret_is_none_and_an_unreadable_one_is_an_error() {
        let dir = scratch("read");
        assert!(read(&dir.join("nothing-here")).unwrap().is_none());

        let empty = dir.join("empty");
        std::fs::write(&empty, "   \n").unwrap();
        assert!(
            read(&empty).unwrap().is_none(),
            "whitespace read as a credential, which is a token every comparison will refuse"
        );

        // A directory where a file should be: readable path, unreadable content. This is the case
        // that must not collapse into `None` — the same shape as an unreadable `$SKEIN_HOME`.
        let notafile = dir.join("notafile");
        std::fs::create_dir(&notafile).unwrap();
        assert!(read(&notafile).is_err());

        assert!(forget(&dir.join("nothing-here")).is_ok(), "absent is fine");
        let placed = dir.join("placed");
        write(&placed, &Secret::new("ghp_x")).unwrap();
        forget(&placed).unwrap();
        assert!(!placed.exists());
    }

    /// Minted from the kernel, hex, at the width asked for, and on disk before it is returned.
    #[test]
    fn a_minted_secret_is_random_hex_of_the_width_asked_for() {
        let dir = scratch("mint");
        let a = mint(&dir.join("one"), 32).unwrap();
        let b = mint(&dir.join("two"), 32).unwrap();
        assert_eq!(a.expose().len(), 64, "32 bytes is 64 hex characters");
        assert!(a.expose().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(
            a.expose(),
            b.expose(),
            "two mints agreed, so this is not randomness"
        );
        assert_eq!(
            read(&dir.join("one")).unwrap().unwrap().expose(),
            a.expose(),
            "the value returned is not the value written, so the next process reads a different \
             credential from the one this one is using"
        );
        assert_eq!(mode_of(&dir.join("one")), 0o600);
        assert_eq!(mint(&dir.join("short"), 16).unwrap().expose().len(), 32);
    }

    /// A guess is refused whatever it shares with the secret, and nothing is trimmed on the way in.
    ///
    /// **What this cannot see:** the timing property [`Secret::same`] exists for. Every assertion
    /// below passes just as well against a comparison that returns on the first differing byte —
    /// the results are identical and only the duration differs, and a duration is not something a
    /// test in this language can hold still enough to assert. What is checked here is the part
    /// that CAN go wrong silently: the answers, and where the trimming does and does not happen.
    #[test]
    fn a_guess_is_refused_whatever_it_shares_with_the_secret() {
        let secret = Secret::new("0123456789abcdef");
        assert!(secret.same("0123456789abcdef"));
        assert!(
            !secret.same("0123456789abcde"),
            "a prefix is not the secret"
        );
        assert!(!secret.same("0123456789abcdefg"));
        assert!(!secret.same(""));
        assert!(
            !secret.same(" 0123456789abcdef "),
            "`same` trims nothing; the caller decides what its own input may carry"
        );
        let mut early = secret.expose().to_string().into_bytes();
        early[0] ^= 1;
        assert!(!secret.same(&String::from_utf8(early).unwrap()));
        let mut late = secret.expose().to_string().into_bytes();
        *late.last_mut().unwrap() ^= 1;
        assert!(!secret.same(&String::from_utf8(late).unwrap()));
    }

    /// The buffer is overwritten in place, and `Drop` is what reaches that code.
    ///
    /// Two assertions, because neither alone is the claim. The first drives [`Secret::scrub`] on a
    /// value the test still owns, so the zeroed bytes can be read back through the same allocation
    /// — and checks the pointer did not move, since scrubbing a fresh buffer and dropping the old
    /// one is the mistake that would look identical from outside. The second proves `Drop` calls
    /// it, by counting.
    ///
    /// **What this cannot see.** Whether the zeroes survive the optimiser: the write has no
    /// observable effect and the compiler may delete it, and no test written in the same language
    /// can tell. It also cannot see a copy made before the scrub — a `String` cloned out through
    /// [`Secret::expose`] has its own buffer and its own lifetime. Both are why `expose` is spelled
    /// to be greppable.
    #[test]
    fn dropping_a_secret_scrubs_its_buffer() {
        let mut secret = Secret::new("ghp_the_actual_credential");
        let before = secret.expose().as_ptr();
        let len = secret.expose().len();
        secret.scrub();
        assert_eq!(
            secret.expose().as_ptr(),
            before,
            "the buffer moved, so what was scrubbed is not what held the credential"
        );
        assert_eq!(
            secret.expose().as_bytes(),
            vec![0u8; len],
            "the credential is still in the buffer"
        );

        let counted = SCRUBS.load(std::sync::atomic::Ordering::SeqCst);
        drop(Secret::new("ghp_another"));
        assert!(
            SCRUBS.load(std::sync::atomic::Ordering::SeqCst) > counted,
            "dropping a Secret did not scrub it — the `Drop` impl is gone or no longer calls scrub"
        );
    }

    /// The bytes land exactly as given, newline and all, with the same file discipline.
    #[test]
    fn raw_bytes_are_written_without_being_trimmed() {
        let dir = scratch("bytes");
        let path = dir.join(".credentials.json");
        write_bytes(&path, b"{\"claudeAiOauth\":{}}\n").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"{\"claudeAiOauth\":{}}\n",
            "the login blob was altered on the way to disk"
        );
        assert_eq!(mode_of(&path), 0o600);
    }
}
