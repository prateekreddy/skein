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

/// The token already on disk, or `None` — never minting one.
///
/// For anything that wants to *report* on auth rather than perform it: `skein doctor` prints the URL
/// that opens the cockpit, and a diagnostic that created the fleet's credential as a side effect of
/// being run would be a surprising thing for a command whose whole job is to look.
pub fn stored() -> Option<String> {
    crate::secret::read(&token_path())
        .ok()
        .flatten()
        .map(|s| s.expose().to_string())
}

/// The fleet's API token as a [`crate::secret::Secret`], minting one on first use.
///
/// 32 bytes from the kernel, hex — 256 bits, so guessing is not a threat model anyone has to think
/// about again. Nothing here ever holds it as a `String`: the comparisons below go through
/// [`crate::secret::Secret::same`], and the credential becomes bare characters only where a caller
/// says [`crate::secret::Secret::expose`] and can be seen doing it.
fn minted() -> Result<crate::secret::Secret, String> {
    let path = token_path();
    // An unreadable file falls through to the mint, exactly as it did before this went through
    // `secret::read`. Propagating that error instead would be an improvement — `authorised` already
    // refuses everything when the token cannot be read, and a re-mint silently invalidates every
    // open cockpit session — but it is a change to what a person sees, so it is a decision rather
    // than a refactor.
    if let Ok(Some(existing)) = crate::secret::read(&path) {
        return Ok(existing);
    }
    let home = crate::config::skein_home();
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    crate::secret::mint(&path, 32)
}

/// The fleet's API token, minting one on first use.
///
/// **A [`crate::secret::Secret`], even though its two callers immediately print it.** They print it
/// into the `?t=` URL a browser needs, which is the credential's delivery channel and not a leak —
/// so each says `expose()`, one word, where the decision is visible. What the type buys is the
/// *other* caller, the one nobody has written yet: under a `String`, a `{t}` in a log line or a
/// `{:?}` of a struct holding it is a leak that looks like ordinary code, and under a `Secret` it
/// is `<secret>`.
///
/// The failure that made this worth saying out loud, from `secrets` Rule 2's own notes: converting
/// this without converting the two printers compiles clean and ships a cockpit URL that cannot open
/// the cockpit, because `{t}` becomes `<secret>` silently.
/// `a_printed_cockpit_url_carries_a_token_that_opens_the_api` in `tests/server.rs` is the assertion
/// that would have caught it, and it compares the printed value with the bytes on disk rather than
/// with a shape.
pub fn token() -> Result<crate::secret::Secret, String> {
    minted()
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
/// Is `offered` the fleet's token? Constant-time, for callers outside [`authorised`].
///
/// The `?t=` exchange in `skein-server` compared with `==` — the only plain-string comparison of
/// this secret in the crate, and the one that turns a URL into a session cookie. [`same`] exists
/// precisely so that comparison is not written by hand twice, and this is how a caller that is not
/// looking at headers reaches it.
pub fn matches(offered: &str) -> bool {
    if disabled() {
        return true;
    }
    minted().is_ok_and(|want| want.same(offered.trim()))
}

pub fn authorised(headers: &HeaderMap) -> bool {
    if disabled() {
        return true;
    }
    let Ok(want) = minted() else {
        // No token could be read *or* minted. Refusing is the only safe answer: the alternative is
        // that an unreadable `~/.skein` silently reopens every route this exists to close.
        return false;
    };
    if let Some(got) = cookie_token(headers) {
        if want.same(&got) {
            return true;
        }
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|got| want.same(got.trim()))
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

    /// The comparison this module authenticates with is `Secret::same`, and it is checked here as
    /// well as in `secret` because *which* comparison is used is a fact about this file: the `==`
    /// that used to sit in the `?t=` exchange is the reason [`matches`] exists at all.
    #[test]
    fn comparison_rejects_near_misses_and_length_games() {
        let want = crate::secret::Secret::new("abc");
        assert!(want.same("abc"));
        assert!(!want.same("abd"));
        assert!(!want.same("abcd"));
        assert!(!crate::secret::Secret::new("").same("a"));
        assert!(crate::secret::Secret::new("").same(""));
    }

    /// The failure that matters most: not "a wrong token is refused" but "an absent one is". A
    /// missing cookie and a missing header must never fall through to allowed.
    #[test]
    fn nothing_at_all_is_refused() {
        // Point $SKEIN_HOME somewhere writable so `token()` mints rather than failing for its own
        // reasons, and make sure the auth switch is not what is doing the work.
        //
        // The lock is not optional. cargo runs these as threads in one process, so $SKEIN_HOME is
        // shared: without it this test moved the home out from under any other test mid-run — and
        // then deleted the directory. It passed alone and took a neighbour down whenever a second
        // $SKEIN_HOME test existed in this module, which is the shape of a flake nobody can place.
        let _lock = crate::testutil::env_lock();
        let dir = std::env::temp_dir().join(format!("skein-apiauth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: single-threaded test process for this variable; the suite sets it the same way.
        unsafe {
            std::env::set_var("SKEIN_HOME", &dir);
            std::env::remove_var("SKEIN_NO_API_AUTH");
        }
        assert!(!authorised(&HeaderMap::new()));

        let good = token().unwrap();
        let good = good.expose();
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

    /// `skein doctor` prints the cockpit URL, so it reads the token — and reading must not create it.
    /// A fleet whose credential was minted by running a diagnostic has one nobody chose to make.
    #[test]
    fn looking_at_the_token_does_not_create_one() {
        let _lock = crate::testutil::env_lock();
        let dir = std::env::temp_dir().join(format!("skein-apiauth-peek-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: guarded by the crate-wide env lock, as every $SKEIN_HOME test is.
        unsafe { std::env::set_var("SKEIN_HOME", &dir) };

        assert_eq!(stored(), None, "nothing on disk is nothing to report");
        assert!(
            !dir.join("api-token").exists(),
            "looking must not have written one"
        );

        let minted = token().unwrap();
        assert_eq!(
            stored().as_deref(),
            Some(minted.expose()),
            "and once one exists, it is what doctor prints"
        );

        // Owner-only, asserted here because it was asserted nowhere. `volume.rs` checks that a
        // 0600 `api-token` survives a volume move, but it writes that fixture itself — so the mode
        // the MINTER produces had no test at all, on the credential that authenticates every
        // mutating route.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("api-token"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the fleet's API token was {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
