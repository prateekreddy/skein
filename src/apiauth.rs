//! Who is allowed to drive the cockpit's API.
//!
//! Until this existed, nobody was asked. The server bound loopback and the README named the
//! network as the auth boundary — which is a sound argument right up until you notice who else is
//! on the loopback side of it. **Measured, from inside a box in this fleet:**
//!
//! ```text
//! curl http://host.docker.internal:7878/api/boxes        → 200
//! curl http://host.docker.internal:7878/api/fleet/git-grants → 200
//! ```
//!
//! A box is not a neighbour on a trusted network. It is the party [`crate::gitgate`] exists to
//! constrain, and it had unauthenticated write access to the API that governs the constraint. The
//! whole of gitgate could be undone through it without touching a token:
//!
//! 1. `box-session.sh --request-write <self> <any repo> "…"` files a request.
//! 2. `POST /api/fleet/git-grants/<id> {"approve":true,"hours":0}` approves it, permanently.
//! 3. The host mints a write token for that repository and places it in the box.
//!
//! Or more directly: `POST /api/boxes/<name>/git-scope {"scope":"fleet"}` un-scopes a box, and the
//! settings route clears `scope_git_to_repo` for the whole fleet. Both take effect at the next box
//! start with nobody asked.
//!
//! **The shape of the fix, and why this one.** The secret lives at `~/.skein/api-token`, 0600. That
//! path is the point: a box has `~/.skein/repos` and `~/.skein/boxes` bind-mounted into it, and
//! *not* `~/.skein` itself — checked, not assumed — so the file is out of reach of exactly the
//! party this is about, while staying trivially available to the host that wrote it.
//!
//! Rejected: filtering by peer address. `host.docker.internal` resolves to a link-local address and
//! the connection still arrives, so what the server sees depends on the container runtime's
//! plumbing rather than on anything skein controls. A rule that has to be re-derived per Docker
//! release is not a boundary. A secret is true regardless of how the packet got here.
//!
//! **Why a cookie.** The browser sends it on every same-origin request automatically — every
//! `fetch` in the page and the terminal WebSocket alike — so authenticating the cockpit changed no
//! calling code at all. A `Bearer` header is accepted too, for `curl` and the CLI.

use axum::http::{HeaderMap, StatusCode};

/// The cookie the browser carries. `HttpOnly`, so page script cannot read it back out and hand it
/// somewhere; `SameSite=Strict`, which closes the cross-site POSTs the API was open to — the JSON
/// routes were protected only by axum's content-type check and preflight, and the bodyless ones
/// (`/api/fleet/git-probe`, `/api/repos/:id/pull`) were not protected at all.
pub const COOKIE: &str = "skein_api";

fn token_path() -> std::path::PathBuf {
    crate::config::skein_home().join("api-token")
}

/// Is API auth switched off?
///
/// For a fleet whose owner has some other boundary and does not want this one. Deliberately an env
/// var and not a setting: a setting for it would be reachable through the very API it disarms.
pub fn disabled() -> bool {
    matches!(
        std::env::var("SKEIN_NO_API_AUTH").ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// The fleet's API token, minting one on first use.
///
/// Generated from the OS's randomness via `/dev/urandom` rather than a crate: skein has no rand
/// dependency, and this is the whole of what would be used from one. 32 bytes, hex — 256 bits, so
/// guessing is not a threat model anyone has to think about again.
pub fn token() -> Result<String, String> {
    let path = token_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let mut bytes = [0u8; 32];
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|e| format!("no randomness available for the API token: {e}"))?;
    }
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let home = crate::config::skein_home();
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    // 0600 before the rename, never after: a chmod that follows leaves a window in which the token
    // that authenticates every mutating route is world-readable.
    let tmp = home.join(format!(".api-token.{}", std::process::id()));
    std::fs::write(&tmp, &token).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(token)
}

/// Constant-time comparison, so a wrong token cannot be narrowed down by how long it took to say so.
///
/// The length is compared first and in the clear, which leaks only how long the secret is — a fixed
/// 64 characters, and already public knowledge from this file.
fn same(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Pull our cookie out of a `Cookie:` header.
///
/// Hand-parsed rather than pulling in a cookie crate for one value. Matches on the whole name so
/// `not_skein_api=…` cannot be read as ours.
fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name.trim() == COOKIE).then(|| value.trim().to_string())
    })
}

/// Does this request carry the fleet's token — as a cookie, or as `Authorization: Bearer`?
pub fn authorised(headers: &HeaderMap) -> bool {
    if disabled() {
        return true;
    }
    let Ok(want) = token() else {
        // No token could be read *or* minted. Refusing is the only safe answer: the alternative is
        // that an unreadable `~/.skein` silently reopens every route this exists to close.
        return false;
    };
    if let Some(got) = cookie_token(headers) {
        if same(&got, &want) {
            return true;
        }
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|got| same(got.trim(), &want))
}

/// The refusal, phrased so the person reading it in a terminal knows what to do next.
pub fn refusal() -> (StatusCode, String) {
    (
        StatusCode::UNAUTHORIZED,
        format!(
            "this API needs the fleet's token. Open the cockpit at the URL skein printed \
             (it carries ?t=…), or send `Authorization: Bearer $(cat {})`.\n",
            token_path().display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(axum::http::HeaderName, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(k.clone(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn a_cookie_is_read_by_its_whole_name() {
        let h = headers(&[(
            axum::http::header::COOKIE,
            "other=1; skein_api=abc; trailing=2",
        )]);
        assert_eq!(cookie_token(&h).as_deref(), Some("abc"));

        // A cookie whose name merely *ends* with ours is not ours. Without a whole-name match this
        // is the shape that lets an unrelated cookie authenticate.
        let h = headers(&[(axum::http::header::COOKIE, "not_skein_api=abc")]);
        assert_eq!(cookie_token(&h), None);
    }

    #[test]
    fn no_cookie_header_is_not_an_error() {
        assert_eq!(cookie_token(&HeaderMap::new()), None);
    }

    #[test]
    fn comparison_rejects_near_misses_and_length_games() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
        assert!(!same("", "a"));
        assert!(same("", ""));
    }

    /// The failure that matters most: not "a wrong token is refused" but "an absent one is". A
    /// missing cookie and a missing header must never fall through to allowed.
    #[test]
    fn nothing_at_all_is_refused() {
        // Point $SKEIN_HOME somewhere writable so `token()` mints rather than failing for its own
        // reasons, and make sure the auth switch is not what is doing the work.
        let dir = std::env::temp_dir().join(format!("skein-apiauth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: single-threaded test process for this variable; the suite sets it the same way.
        unsafe {
            std::env::set_var("SKEIN_HOME", &dir);
            std::env::remove_var("SKEIN_NO_API_AUTH");
        }
        assert!(!authorised(&HeaderMap::new()));

        let good = token().unwrap();
        assert_eq!(good.len(), 64, "256 bits, hex");
        assert!(authorised(&headers(&[(
            axum::http::header::AUTHORIZATION,
            &format!("Bearer {good}"),
        )])));
        assert!(authorised(&headers(&[(
            axum::http::header::COOKIE,
            &format!("{COOKIE}={good}"),
        )])));
        assert!(!authorised(&headers(&[(
            axum::http::header::COOKIE,
            &format!("{COOKIE}={}", "0".repeat(64)),
        )])));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
