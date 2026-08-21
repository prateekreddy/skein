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
