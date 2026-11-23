//! A box's git write access to one repository: what the owner granted, whether the box should
//! hold a token for it, what is actually placed, and the one status derived from those three.
//!
//! "Granted" is never stored. A request's answer and a grant's date say what was decided, and
//! neither changes when a token fails to arrive, expires or is removed, so [`derive`] says
//! "granted" only for a token that is in the box and unexpired.
//!
//! Observing a token reads its file's metadata and never opens it ([`observe_token`]).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How long GitHub keeps an App installation token valid. A stored token has no lifetime skein
/// knows, so only an App token can be read as expired from its file's age.
pub const APP_TOKEN_LIFE: Duration = Duration::hours(1);

/// One grant as `git-grants.json` holds it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    #[serde(default, rename = "box")]
    pub box_name: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub granted: String,
    /// RFC3339, or empty for "until the box is destroyed".
    ///
    /// Empty rather than a far-future date so the cockpit can say *which* of the two a grant is,
    /// and so a permanent grant is a deliberate-looking record instead of a timestamp in 2099.
    #[serde(default)]
    pub expires: String,
}

impl Grant {
    /// Is this grant still good at `now`?
    ///
    /// An unparseable expiry counts as **expired**, not as permanent. The alternative fails open:
    /// a corrupted date would silently upgrade a 24-hour grant into a forever one, and nothing
    /// would ever report it.
    pub fn is_live(&self, now: DateTime<Utc>) -> bool {
        if self.expires.trim().is_empty() {
            return true;
        }
        DateTime::parse_from_rfc3339(self.expires.trim())
            .map(|e| e > now)
            .unwrap_or(false)
    }
}

/// Every grant in the file at `path`: empty when there is no file, an error when it cannot be read.
pub fn read_grants(path: &Path) -> Result<Vec<Grant>, String> {
    crate::util::read_json_or_why::<Vec<Grant>>(path).map(Option::unwrap_or_default)
}

/// Change the grants file under its lock. Refuses a file it cannot read rather than replacing it.
pub fn update_grants<R>(
    path: &Path,
    change: impl FnOnce(&mut Vec<Grant>) -> Result<R, String>,
) -> Result<R, String> {
    crate::util::update_json(path, change)
}

/// A box's token directory, inside its state directory.
pub fn tokens_dir(box_state: &Path) -> PathBuf {
    box_state.join("git-tokens")
}

/// A repository's token file in a box's token directory.
pub fn token_path(tokens: &Path, slug: &str) -> PathBuf {
    tokens.join(slug.replace('/', "%2F"))
}

/// Where the host says why a box holds no token for a repository: `why/` in the same directory.
pub fn reason_path(tokens: &Path, slug: &str) -> PathBuf {
    tokens.join("why").join(slug.replace('/', "%2F"))
}

/// What the owner decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Declared {
    /// The grants file exists and skein cannot read it, so it does not know.
    Unreadable,
    /// No grant on record: it was withdrawn, or never made.
    Absent,
    Granted {
        live: bool,
    },
}

/// What is in the box's token file, from its metadata alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Token {
    Missing,
    /// Something is there, but not a private, non-empty file of skein's.
    NotAsPlaced,
    /// `expires` is `None` for a token whose lifetime skein does not know.
    Placed {
        expires: Option<DateTime<Utc>>,
    },
}

/// What skein found.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    pub token: Token,
    /// The refresher's reason, or the setup check's, for there being no token to place.
    pub why_none: Option<String>,
    /// GitHub turned down the stored token this repository would be given.
    pub turned_down: bool,
}

/// One box's access to one repository.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub declared: Declared,
    /// Whether this box takes per-repository tokens at all. One on the fleet-wide credential does
    /// not, so a grant it holds has no effect.
    pub wanted: bool,
    pub observed: Observed,
}

/// What people are shown for a [`Record`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shown {
    /// A stable word for code and tests.
    pub status: &'static str,
    /// The short word on the row.
    pub label: &'static str,
    /// What is wrong and what to do next. Empty when nothing is.
    pub says: String,
    /// There is a grant in force to take back.
    pub can_revoke: bool,
}

/// Read a token file's metadata. **Never opens it.**
///
/// A link, a directory, an empty file, one another user owns or one others can read is not what
/// `secret::write` leaves, so it does not count as placed.
pub fn observe_token(path: &Path, lifetime: Option<Duration>) -> Token {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Token::Missing,
        Err(_) => return Token::NotAsPlaced,
    };
    // SAFETY: `geteuid` takes nothing, touches no memory and cannot fail.
    let me = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.len() == 0 || meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Token::NotAsPlaced;
    }
    let expires = match lifetime {
        None => None,
        Some(life) => match meta.modified() {
            Ok(at) => Some(DateTime::<Utc>::from(at) + life),
            Err(_) => return Token::NotAsPlaced,
        },
    };
    Token::Placed { expires }
}

/// The one status for a box's access to `repo`.
pub fn derive(repo: &str, record: &Record, now: DateTime<Utc>) -> Shown {
    let shown = |status, label, says: String, can_revoke| Shown {
        status,
        label,
        says,
        can_revoke,
    };
    // A token can outlast its grant by up to one refresh, and the owner should know it has.
    let leftover = match record.observed.token {
        Token::Missing => "",
        _ => " Its token is removed at the next refresh, within 20 minutes.",
    };
    match record.declared {
        Declared::Unreadable => {
            return shown(
                "unknown",
                "unknown",
                "skein cannot read its list of grants, so it cannot say whether this box has \
                 access. Fix or move git-grants.json in skein's home folder."
                    .into(),
                false,
            )
        }
        Declared::Absent => {
            return shown(
                "withdrawn",
                "withdrawn",
                format!(
                    "This access was withdrawn. The box can ask again if it still needs \
                     {repo}.{leftover}"
                ),
                false,
            )
        }
        Declared::Granted { live: false } => {
            return shown(
                "expired",
                "expired",
                format!(
                    "This access has expired. The box can ask again if it still needs \
                     {repo}.{leftover}"
                ),
                false,
            )
        }
        Declared::Granted { live: true } => {}
    }
    if !record.wanted {
        return shown(
            "not_in_use",
            "not in use",
            "This box uses the fleet-wide GitHub access, so this grant has no effect. To use it, \
             set the box's GitHub scope to its repository and restart the box."
                .into(),
            true,
        );
    }
    let store_step =
        format!("Store a token for {repo} under Settings → GitHub & keys → Write tokens");
    if record.observed.turned_down {
        return shown(
            "token_refused",
            "token refused",
            format!("GitHub turned down the stored token for {repo}. {store_step}."),
            true,
        );
    }
    let token_live = match record.observed.token {
        Token::Placed { expires } => expires.is_none_or(|at| at > now),
        _ => false,
    };
    if token_live {
        return shown("granted", "granted", String::new(), true);
    }
    if let Some(why) = &record.observed.why_none {
        return shown(
            "no_token",
            "no token",
            format!("Granted, but no token can be made for {repo}: {why}. {store_step}."),
            true,
        );
    }
    match record.observed.token {
        Token::Placed { .. } => shown(
            "token_expired",
            "token expired",
            format!(
                "The token in this box has expired, so it cannot push to {repo}. skein replaces \
                 it within 20 minutes. If this stays, check Settings → GitHub & keys."
            ),
            true,
        ),
        Token::NotAsPlaced => shown(
            "token_damaged",
            "token problem",
            format!(
                "The token file for {repo} in this box is not the one skein placed. skein \
                 replaces it within 20 minutes. If this stays, restart the box."
            ),
            true,
        ),
        Token::Missing => shown(
            "waiting",
            "waiting for token",
            format!(
                "Granted, but the token for {repo} is not in the box yet. skein places it within \
                 20 minutes. If this stays, check Settings → GitHub & keys."
            ),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn record(declared: Declared, wanted: bool, token: Token) -> Record {
        Record {
            declared,
            wanted,
            observed: Observed {
                token,
                why_none: None,
                turned_down: false,
            },
        }
    }

    const LIVE: Declared = Declared::Granted { live: true };

    fn status(r: &Record) -> &'static str {
        derive("acme/thing", r, now()).status
    }

    #[test]
    fn granted_needs_a_token_that_is_placed_and_unexpired() {
        // Fails if `derive` answers "granted" from the grant alone.
        assert_eq!(status(&record(LIVE, true, Token::Missing)), "waiting");
        // Fails if the expiry comparison is dropped or inverted.
        let past = Token::Placed {
            expires: Some(now() - Duration::minutes(1)),
        };
        assert_eq!(status(&record(LIVE, true, past)), "token_expired");
        // Fails if an unexpired token stops counting.
        let future = Token::Placed {
            expires: Some(now() + Duration::minutes(1)),
        };
        assert_eq!(status(&record(LIVE, true, future)), "granted");
        // Fails if a token of unknown lifetime (a stored PAT) is read as expired.
        let stored = Token::Placed { expires: None };
        assert_eq!(status(&record(LIVE, true, stored)), "granted");
        // Fails if a damaged file counts as placed.
        assert_eq!(
            status(&record(LIVE, true, Token::NotAsPlaced)),
            "token_damaged"
        );
    }

    #[test]
    fn a_placed_token_does_not_outrank_what_the_owner_decided() {
        let placed = Token::Placed { expires: None };
        // Fails if the token is checked before a withdrawn grant.
        let gone = derive("acme/thing", &record(Declared::Absent, true, placed), now());
        assert_eq!(gone.status, "withdrawn");
        assert!(gone.says.contains("next refresh"), "{}", gone.says);
        assert!(!gone.can_revoke);
        // Fails if the token is checked before an expired grant.
        let over = Declared::Granted { live: false };
        assert_eq!(status(&record(over, true, placed)), "expired");
        // Fails if an unreadable grants file reads as withdrawn, which it may not be.
        assert_eq!(
            status(&record(Declared::Unreadable, true, placed)),
            "unknown"
        );
        // Fails if a box on the fleet-wide credential is told its grant is working.
        assert_eq!(status(&record(LIVE, false, placed)), "not_in_use");
    }

    #[test]
    fn a_token_github_turned_down_is_not_granted_and_a_reason_is_said() {
        let placed = Token::Placed { expires: None };
        let mut r = record(LIVE, true, placed);
        r.observed.turned_down = true;
        // Fails if `turned_down` is ignored.
        assert_eq!(status(&r), "token_refused");
        let mut r = record(LIVE, true, Token::Missing);
        r.observed.why_none = Some("the App is not installed".into());
        // Fails if the refresher's reason is dropped.
        let shown = derive("acme/thing", &r, now());
        assert_eq!(shown.status, "no_token");
        assert!(
            shown.says.contains("the App is not installed"),
            "{}",
            shown.says
        );
    }

    #[test]
    fn observing_reads_metadata_and_refuses_what_skein_did_not_place() {
        let dir = crate::testutil::tempdir();
        let dir: &Path = dir.as_ref();
        let path = dir.join("token");
        // Fails if a missing file is anything but missing.
        assert_eq!(observe_token(&path, None), Token::Missing);

        std::fs::write(&path, "fixture-value").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        // Fails if a private, non-empty file of ours is refused.
        assert_eq!(observe_token(&path, None), Token::Placed { expires: None });
        // Fails if the expiry is not the file's age plus the lifetime.
        let Token::Placed { expires: Some(at) } = observe_token(&path, Some(APP_TOKEN_LIFE)) else {
            panic!("a placed token with a lifetime has an expiry");
        };
        assert!(at > Utc::now() + Duration::minutes(59), "{at}");

        // Even unreadable it still counts, because only metadata is consulted. Fails if observing
        // ever opens the file. Root ignores the mode, so this proves nothing there.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(observe_token(&path, None), Token::Placed { expires: None });

        // Fails if the group/other mode check goes.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(observe_token(&path, None), Token::NotAsPlaced);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        // Fails if a link is followed.
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(observe_token(&link, None), Token::NotAsPlaced);

        // Fails if an empty file counts.
        let empty = dir.join("empty");
        std::fs::write(&empty, "").unwrap();
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(observe_token(&empty, None), Token::NotAsPlaced);
    }
}
