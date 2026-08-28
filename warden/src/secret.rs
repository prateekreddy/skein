//! What the warden checks instead of an address.
//!
//! # Why an address stops being an answer
//!
//! Today the warden binds loopback and that IS the boundary: a box reaches the host through the
//! gateway address, so a loopback listener answers host processes and nothing inside the sandbox.
//! `serve.rs` says so, and says the rest of this paragraph: **step 4c is precisely when that has to
//! change.** Once skein is a process *inside* the sandbox it reaches the warden the way a box would,
//! and architecture §9.4 states the consequence — "indistinguishable from skein by address or uid".
//!
//! So the bind widens at 4c and something else has to answer "who is asking". This is it, and it is
//! only possible because 4a landed: `src/box-session.sh` covers the state root and binds back what a
//! box is entitled to, so **a file in a place no box's mount view reaches** is a thing skein can have
//! and a box cannot. The secret is not clever; the cover is what makes a plain file sufficient.
//!
//! # What it proves, and the two things it does not
//!
//! Possession of a file. That is all, and the two limits are worth stating where somebody will look
//! for them:
//!
//! * **It is not an approval.** §8.1's rule stands untouched — a doer runs because a person at the
//!   host said so, and holding the secret does not make anything happen. What it adds is that the
//!   two *reporting* endpoints stop being world-readable once the bind widens, and that a box cannot
//!   spend §8.5's doorway to keep skein from ever reaching the person.
//! * **It is not a replacement for the narrow bind.** Both, and the bind widens only at 4c.
//!
//! # It fails closed, and says which failure it is
//!
//! A warden that cannot read its own copy refuses **everything**, and says that rather than saying
//! the caller was wrong. The alternative — treat "no secret" as "no checking" — is the failure that
//! is discovered on the day it matters, and it would arrive by a file being deleted rather than by
//! anybody deciding anything.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The header skein presents it in. Lower-case, because `wire` folds header names.
pub const HEADER: &str = "x-skein-warden";

/// The warden's copy of the shared secret.
pub struct Secret {
    known: Option<String>,
    where_: PathBuf,
}

impl Secret {
    /// Read the secret from the warden's home, minting one if there is none.
    ///
    /// **The warden mints, and skein only reads.** One minter, because two would each write a
    /// different value and the mismatch would look exactly like an intruder — the loudest possible
    /// failure for the most boring possible cause. The warden owns the directory, so the warden owns
    /// the file.
    pub fn kept_in(home: &Path) -> Secret {
        let where_ = home.join("secret");
        if let Some(known) = read(&where_) {
            return Secret {
                known: Some(known),
                where_,
            };
        }
        let minted = mint();
        let written = std::fs::create_dir_all(home)
            .and_then(|_| write_private(&where_, &minted))
            .is_ok();
        Secret {
            known: written.then_some(minted),
            where_,
        }
    }

    /// A warden with no secret cannot check one, and says so instead of letting everybody through.
    pub fn missing(&self) -> bool {
        self.known.is_none()
    }

    pub fn where_(&self) -> &Path {
        &self.where_
    }

    /// Does the caller hold it?
    ///
    /// Compared in constant time — every byte of both, always. A comparison that returns early on
    /// the first difference tells a caller how much of its guess was right, and a secret guessable
    /// one byte at a time is not one. `false` whenever there is nothing to compare against, so the
    /// missing case cannot be reached through here by accident.
    pub fn matches(&self, offered: &str) -> bool {
        let Some(known) = &self.known else {
            return false;
        };
        same(known.as_bytes(), offered.trim().as_bytes())
    }
}

/// Adopt a secret left behind at the pre-derivation default, `~/.skein/warden/secret`.
///
/// Before [`crate::home`] derived the warden's home from the volume root it was fixed there, so a
/// warden upgraded on a volume that lives elsewhere would find its new home empty and mint —
/// leaving the old secret lying at a path no cover is derived over, which is the exact exposure
/// the derivation exists to close.
///
/// **Moved, not re-minted, and the reason is how each failure reads.** Nothing durable is keyed by
/// the bytes — skein reads the secret per request (`src/warden_client.rs`), the warden reads it at
/// start, the outcome store keys on operation ids — so a re-mint would heal itself at the next
/// restart of each end. But a re-mint leaves the old, valid-looking secret behind uncovered, and
/// during the upgrade window a client still resolving the old path would present the *old* bytes
/// and be refused as a mismatch — which looks exactly like an intruder ([`Secret::kept_in`]'s
/// note), the loudest possible failure for the most boring possible cause. After a move that
/// client presents nothing and is told the file's name, which is actionable.
///
/// Renamed by the warden and only the warden: one owner of the directory, one mover — the same
/// rule as one minter. skein's read side keeps a read-only courtesy for the window before this has
/// run, and never writes. And never under `$SKEIN_WARDEN_HOME`: the override is a test and
/// development knob, and adopting there would move the host's real pairing into a scratch
/// directory.
pub fn adopt_left_behind(home: &Path) {
    // Only for a *derived* home. Under `$SKEIN_WARDEN_HOME` — tests and development — adopting
    // would MOVE the host's real pairing out of `~/.skein/warden` into a scratch directory, which
    // is a test run deleting a production secret. An overridden warden mints its own.
    if std::env::var_os("SKEIN_WARDEN_HOME").is_some_and(|s| !s.is_empty()) {
        return;
    }
    if read(&home.join("secret")).is_some() {
        return;
    }
    let old = match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h).join(".skein/warden/secret"),
        None => return,
    };
    let new = home.join("secret");
    if old == new || read(&old).is_none() {
        return;
    }
    if std::fs::create_dir_all(home).is_err() {
        return; // `kept_in` will fail to mint at the same home, and reports that loudly.
    }
    // `rename` first; a volume repointed onto another disk — the common reason for a non-default
    // `$SKEIN_HOME` — makes it EXDEV, so fall back to a 0600 copy and remove the original: the
    // point is that nothing secret-shaped stays at the uncovered path.
    let moved = std::fs::rename(&old, &new).or_else(|_| match read(&old) {
        Some(bytes) => write_private(&new, &bytes).and_then(|_| std::fs::remove_file(&old)),
        None => Err(std::io::Error::other("became unreadable mid-move")),
    });
    match moved {
        Ok(()) => eprintln!(
            "skein-warden: moved the secret from {} to {} — the home is derived from the volume \
             root now, and the existing pairing travels with it",
            old.display(),
            new.display()
        ),
        Err(e) => eprintln!(
            "skein-warden: could not move the secret from {} to {} ({e}) — a fresh one will be \
             minted there, and the file left behind should be deleted: it is outside the volume's \
             cover",
            old.display(),
            new.display()
        ),
    }
}

/// Byte-for-byte, in time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    // Length is compared as data too. Returning early on a length mismatch would leak the length,
    // which for a fixed-width secret is nothing — and this is not the place to depend on that
    // staying true.
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

/// 32 hex characters from the kernel.
///
/// `/dev/urandom` rather than a crate: this is the whole of what a random-number dependency would be
/// used for, and the warden's dependency list is an argument it makes about itself (§8).
///
/// An unreadable `/dev/urandom` yields an empty string, which `kept_in` writes nowhere and reports as
/// missing — a warden that refuses everything, rather than one holding a predictable secret.
fn mint() -> String {
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        return String::new();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// `0600` from the moment it exists — created with the mode rather than chmod'd after, so there is
/// no instant in which it is readable by anybody else.
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "warden-secret-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn it_is_minted_once_and_read_back_after() {
        let home = scratch("mint");
        let first = Secret::kept_in(&home);
        assert!(!first.missing());
        let again = Secret::kept_in(&home);
        assert!(
            again.matches(&std::fs::read_to_string(home.join("secret")).unwrap()),
            "a restart minted a new secret, so every client holding the old one is now an intruder"
        );
    }

    #[test]
    fn it_is_readable_by_nobody_else() {
        use std::os::unix::fs::PermissionsExt;
        let home = scratch("mode");
        let _ = Secret::kept_in(&home);
        let mode = std::fs::metadata(home.join("secret"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the secret is readable beyond its owner, which is the whole of what it is for"
        );
    }

    #[test]
    fn a_warden_that_cannot_read_its_own_copy_matches_nothing() {
        // The failure that must not turn into "no checking": an unwritable home means no secret,
        // and a secret nobody has cannot be presented by anybody — including the right caller.
        let home = scratch("none").join("not-a-directory/deeper");
        std::fs::write(home.parent().unwrap(), "a file where a directory would go").unwrap();
        let secret = Secret::kept_in(&home);
        assert!(secret.missing());
        assert!(!secret.matches(""));
        assert!(!secret.matches("anything at all"));
    }

    /// A secret at the old fixed default is moved to the derived home, and wins there after.
    ///
    /// Moved rather than re-minted — the pairing survives the path change — and moved *away*: the
    /// old copy must not stay at a path no cover is derived over. `$HOME` is faked to a scratch
    /// directory under the crate's env lock, because the old default hangs off it.
    #[test]
    fn a_secret_left_at_the_old_default_is_adopted_not_reminted() {
        let _g = crate::env_lock();
        let was_home = std::env::var_os("HOME");
        let was_override = std::env::var_os("SKEIN_WARDEN_HOME");
        let fake_host = scratch("adopt-host");
        std::env::set_var("HOME", &fake_host);
        std::env::remove_var("SKEIN_WARDEN_HOME");

        let old = fake_host.join(".skein/warden");
        std::fs::create_dir_all(&old).unwrap();
        write_private(&old.join("secret"), "the-existing-pairing").unwrap();

        let home = scratch("adopt-new").join("warden");

        // Under the explicit override nothing is adopted: a test or dev warden pulling the host's
        // real pairing out of `~/.skein/warden` would be a scratch run deleting a real secret.
        std::env::set_var("SKEIN_WARDEN_HOME", &home);
        adopt_left_behind(&home);
        assert!(
            old.join("secret").exists() && !home.join("secret").exists(),
            "an overridden home adopted the host's pairing"
        );
        std::env::remove_var("SKEIN_WARDEN_HOME");

        adopt_left_behind(&home);

        assert!(
            !old.join("secret").exists(),
            "the old copy stayed at the uncovered path"
        );
        assert_eq!(
            read(&home.join("secret")).as_deref(),
            Some("the-existing-pairing"),
            "the pairing did not travel — a re-mint would read as an intruder to a client \
             still holding the old bytes"
        );
        // And the warden that then starts holds those bytes, not fresh ones.
        let secret = Secret::kept_in(&home);
        assert!(secret.matches("the-existing-pairing"));

        // Idempotent, and never a clobber: with a secret already at the derived home, a file
        // appearing at the old path again is somebody else's and stays put.
        write_private(&old.join("secret"), "somebody-elses").unwrap();
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        adopt_left_behind(&home);
        assert_eq!(
            read(&home.join("secret")).as_deref(),
            Some("the-existing-pairing"),
            "an adoption ran over a home that already had its secret"
        );
        assert!(old.join("secret").exists());

        match was_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match was_override {
            Some(v) => std::env::set_var("SKEIN_WARDEN_HOME", v),
            None => std::env::remove_var("SKEIN_WARDEN_HOME"),
        }
    }

    #[test]
    fn the_comparison_does_not_stop_at_the_first_difference() {
        let home = scratch("cmp");
        let secret = Secret::kept_in(&home);
        let known = std::fs::read_to_string(home.join("secret")).unwrap();
        assert!(secret.matches(&known));
        assert!(
            secret.matches(&format!("  {known}\n")),
            "surrounding space is not a mismatch"
        );
        assert!(
            !secret.matches(&known[..known.len() - 1]),
            "a prefix is not the secret"
        );
        assert!(!secret.matches(&format!("{known}x")));
        assert!(!secret.matches(""));
        // The property itself, asserted where it can be: every byte is looked at, so a guess that
        // shares a long prefix is no closer than one that shares none.
        let mut wrong = known.clone().into_bytes();
        wrong[0] ^= 1;
        assert!(!secret.matches(&String::from_utf8(wrong).unwrap()));
        let mut late = known.clone().into_bytes();
        *late.last_mut().unwrap() ^= 1;
        assert!(!secret.matches(&String::from_utf8(late).unwrap()));
    }
}
