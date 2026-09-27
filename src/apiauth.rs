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

/// Is the auth-off switch set in this process's environment?
///
/// The reading on its own, which is not the same answer as [`disabled`] everywhere skein runs.
/// `skein doctor` reports on the switch rather than acting on it, so it wants this one.
pub fn switch_set() -> bool {
    matches!(
        std::env::var("SKEIN_NO_API_AUTH").ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Is this process running under the fleet's doorway — the cockpit, or something it started?
///
/// **Not `SKEIN_IN_FLEET`, which is the obvious answer and a dead one.** `deployment::in_fleet()`
/// was removed at SKEIN-576 — skein stands inside the sandbox it manages now, so there is no second
/// deployment to tell apart — and the variable went with it at SKEIN-643. Nothing in this tree sets
/// it any longer: `grep -rn SKEIN_IN_FLEET . --exclude-dir=.git` is 11 lines and every one of them
/// is a comment, with `fleet::start_server` passing the doorway `SKEIN_HOME` and nothing else. A
/// fleet created before that commit still carries it, which is exactly the trap: gating on it would
/// look right on such a machine and refuse nothing at all on a fleet created today.
///
/// [`crate::doorway::INHERITED_ONLY`] is the live one. `src/server-doorway.py`'s `spawn` sets it on
/// the server's environment at the exec, beside the listening descriptor, and nothing else in the
/// tree writes it.
///
/// **What it marks: the cockpit, and nothing a box starts.** It used to reach further, because a box
/// session inherited the cockpit's whole environment: `env | grep SKEIN_LISTEN` in a box answered
/// `SKEIN_LISTEN_INHERITED_ONLY=1`, so a `skein-server` started *inside a box* with the switch set
/// was refused too — by a leak rather than by a decision. A box session now inherits only the
/// launcher's allow-list (`inherited_env` in `src/box-session.sh`, SKEIN-972), and this variable is
/// not on it. A box started before that still carries it until it restarts, which is why
/// `tests/ui/harness/server.mjs` still strips it. The in-box half of the refusal is kept, on purpose
/// now, by [`inside_a_box`] (SKEIN-1086).
///
/// **Why a box is refused as well as the cockpit.** The switch's justification is "an owner who has
/// some other boundary", and §9.4's answer to why there is none in-fleet is the shared network
/// namespace — which a box is on. A server a box starts with auth off is reachable from every other
/// box exactly as the cockpit is. What lies outside both is a machine that is not a skein fleet, and
/// there the switch works exactly as it is documented to.
///
/// **A box cannot forge it in either direction, which is the property that matters.** A box setting
/// this in its own shell changes its own processes and nothing about the cockpit: an environment is
/// written only by whoever execs the process, the cockpit is exec'd by the doorway in the sandbox,
/// and no box starts it. Nothing here is read from a file, a setting or a request, so there is
/// nothing a box can create to make the cockpit refuse to start — which would hand it a way to stop
/// the fleet — and nothing it can delete to make the cockpit accept the switch. A signal derived
/// from the fleet root or from a marker file would fail on precisely that point, since
/// `~/.skein/boxes` is bind-mounted into every box. This variable already decides whether skein may
/// be the cockpit at all (§9.4), so it carries no trust it was not carrying already.
fn under_the_fleets_doorway() -> bool {
    crate::doorway::inherited_only()
}

/// The variable the box launcher exports into every box's session, to say it is one.
pub const IN_BOX: &str = "SKEIN_IN_BOX";

/// Is this process inside a box — started from a box's session, which `src/box-session.sh` marks
/// with [`IN_BOX`] on purpose (SKEIN-1086)?
///
/// **The deliberate replacement for a leak.** Until SKEIN-972 a server a box started carried the
/// doorway's [`crate::doorway::INHERITED_ONLY`] by inheritance, and [`under_the_fleets_doorway`]
/// refused the switch for it by accident. That variable means "exec'd by the doorway, holding its
/// socket", which a box's server is not, and it also made such a server refuse to bind a port of its
/// own. The question the refusal actually asks is "is this on the fleet's network namespace", and a
/// box answers yes for the reason given under [`under_the_fleets_doorway`]. So the launcher says so
/// in a variable of its own, and this reads it.
///
/// **What it can and cannot be made to say.** It cannot reach the cockpit: the doorway execs the
/// cockpit, not a box, so setting it in a box changes that box's processes only. A box CAN unset it
/// in its own shell and then start a server with the switch on. That is not a boundary this check
/// could hold: a box that means to serve something unauthenticated on the shared namespace can do
/// so with any program at all. What the check stops is the accident — an agent or a person running
/// the documented switch inside a box and exposing the API to every other box without knowing it.
fn inside_a_box() -> bool {
    std::env::var(IN_BOX).is_ok_and(|value| value == "1")
}

/// Where the switch is refused: under the fleet's doorway, or inside a box.
fn refused_here() -> bool {
    under_the_fleets_doorway() || inside_a_box()
}

/// Is API auth switched off?
///
/// For a fleet whose owner has some other boundary and does not want this one. Deliberately an env
/// var and not a setting: a setting for it would be reachable through the very API it disarms.
///
/// **Under the fleet's doorway, or inside a box, the answer is no, whatever the variable says**
/// (SKEIN-962 for the cockpit, SKEIN-1086 for a box; and
/// architecture §9.4, which has said "in-fleet there is no such boundary, so the switch is refused"
/// since it was written). The switch is for an owner who has some other boundary; here there is none to
/// have — every box shares one network namespace with the cockpit's port, and the token is the
/// whole of what stands between a box and `/api/fleet/git-grants`. So there the variable stops
/// meaning "off" and starts meaning "refuse to serve": [`off_switch_refused`] is what the cockpit
/// branches on, and this staying `false` is the belt beside that brace — anything that reached a
/// route despite the refusal is still asked for the token rather than waved through.
pub fn disabled() -> bool {
    switch_set() && !refused_here()
}

/// Is the auth-off switch set where it is refused?
///
/// The cockpit serves [`off_switch_refusal`] and nothing else when this is true, and `skein doctor`
/// reports the same thing. [`under_the_fleets_doorway`] and [`inside_a_box`] are what "where"
/// means; the first is why a box cannot choose the cockpit's answer, the second what a box's own
/// server is told.
pub fn off_switch_refused() -> bool {
    switch_set() && refused_here()
}

/// What the cockpit says, and then serves, instead of starting with the switch on.
///
/// What happened, why it is refused here rather than everywhere, and what to do — in that order,
/// because the second sentence is the one that stops this reading as skein being broken.
pub fn off_switch_refusal() -> &'static str {
    "skein-server: $SKEIN_NO_API_AUTH is set and this server is running under the fleet's doorway \
     ($SKEIN_LISTEN_INHERITED_ONLY=1) or inside a box ($SKEIN_IN_BOX=1), where the switch is refused \
     (architecture §9.4).\n\
     The switch exists for an owner who has some other boundary. Inside the sandbox there is no \
     other boundary to have: every box shares one network namespace with this port, so the fleet's \
     API token is the only thing standing between a box and /api/fleet/git-grants.\n\
     Nothing but this message is served, on any path. Unset $SKEIN_NO_API_AUTH wherever this server \
     is started and restart it, then open the cockpit from the host with:\n    \
     open \"http://127.0.0.1:7878/?t=$(cat ~/.skein/api-token)\"\n"
}

/// **The one way to open the cockpit with its token**, from the host (SKEIN-1185).
///
/// Every place that tells a person how to get in used to say "open the URL skein printed", and on
/// the documented install nothing prints one anywhere a person is looking: `skein-server` runs
/// under the fleet's doorway inside the sandbox, and `bootstrap.sh` never printed it. The cockpit's
/// own 401 page (`sayUnauthorised` in `src/web/app/board.js`) already had the line that works from
/// the host, so that line is the one every other place gives — this refusal, the off-switch
/// refusal above, and the end of `bootstrap.sh`.
/// `the_way_in_is_one_line_everywhere_it_is_given` holds them to it.
pub const OPEN_FROM_THE_HOST: &str = "open \"http://127.0.0.1:7878/?t=$(cat ~/.skein/api-token)\"";

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
/// `a_printed_cockpit_url_carries_a_token_that_opens_the_api` in `tests/server/requests.rs` is the assertion
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

/// Does this request carry the fleet's token — as a cookie, or as `Authorization: Bearer`?
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
            "this API needs the fleet's token. From the host, open the cockpit with \
             `{OPEN_FROM_THE_HOST}`, or send `Authorization: Bearer $(cat {})`.\n",
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
        let want = crate::secret::Secret::new("skein-test-abc");
        assert!(want.same("skein-test-abc"));
        assert!(!want.same("skein-test-abd"));
        assert!(!want.same("skein-test-abcd"));
        assert!(!crate::secret::Secret::new("").same("a"));
        assert!(crate::secret::Secret::new("").same(""));
    }

    /// **Every place that tells a person how to open the cockpit gives the same line, and it is
    /// the one that works from the host** (SKEIN-1185).
    ///
    /// **The concrete change that makes it fail, named before it was written:** putting "the URL
    /// skein printed" back into [`refusal`], or reverting the end of `bootstrap.sh` to "open what
    /// the cockpit prints for its token", fails `points at output nobody sees`. Both planted.
    #[test]
    fn the_way_in_is_one_line_everywhere_it_is_given() {
        let page = include_str!("web/index.html");
        assert!(
            page.contains(OPEN_FROM_THE_HOST),
            "the cockpit's own 401 page no longer gives `{OPEN_FROM_THE_HOST}`, and it is the \
             source every other place copies"
        );
        let (_, said) = refusal();
        assert!(
            said.contains(OPEN_FROM_THE_HOST),
            "the API's 401 gives the line: {said}"
        );
        assert!(
            off_switch_refusal().contains(OPEN_FROM_THE_HOST),
            "the off-switch refusal gives the line: {}",
            off_switch_refusal()
        );
        // The install's last words, where the port is the one it just started on. Asserted on the
        // shape that survives the shell — `\$(cat …)` in the heredoc prints as `$(cat …)`.
        let install = include_str!("../bootstrap.sh");
        assert!(
            install.contains("open \"http://127.0.0.1:$port/?t=\\$(cat ~/.skein/api-token)\""),
            "the install ends with the line"
        );
        for said in [page, said.as_str(), off_switch_refusal(), install] {
            for gone in [
                "URL skein printed",
                "URL skein prints",
                "what the cockpit prints",
            ] {
                assert!(
                    !said.contains(gone),
                    "\"{gone}\" points at output nobody sees on the documented install"
                );
            }
        }
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
        let dir = crate::testutil::tempdir();
        // Pinned rather than set: the home this points at is deleted at the end of the test, and
        // a variable naming a deleted directory is worse for the next test than one naming nothing.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &dir).unset("SKEIN_NO_API_AUTH");
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
    }

    /// `skein doctor` prints the cockpit URL, so it reads the token — and reading must not create it.
    /// A fleet whose credential was minted by running a diagnostic has one nobody chose to make.
    #[test]
    fn looking_at_the_token_does_not_create_one() {
        let _lock = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &dir);

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
    }

    /// **The switch is refused inside a box, and honoured where neither marker is set**
    /// (SKEIN-1086).
    ///
    /// What would make each half fail: dropping [`inside_a_box`] from [`refused_here`] fails the
    /// first (a box's own server takes the switch and serves the API unauthenticated to every other
    /// box); making [`refused_here`] answer `true` unconditionally fails the second (the documented
    /// switch stops working for an owner outside any fleet). The doorway half of the same rule is
    /// the spawned test in `tests/server/door.rs`.
    #[test]
    fn the_switch_is_refused_inside_a_box_and_honoured_where_neither_marker_is_set() {
        let _env = crate::testutil::env_lock();
        std::env::set_var("SKEIN_NO_API_AUTH", "1");
        std::env::remove_var(crate::doorway::INHERITED_ONLY);

        std::env::set_var(IN_BOX, "1");
        assert!(
            off_switch_refused() && !disabled(),
            "a server started inside a box honoured $SKEIN_NO_API_AUTH, so it serves the fleet's \
             API unauthenticated to every box on the shared network namespace"
        );

        std::env::remove_var(IN_BOX);
        assert!(
            !off_switch_refused() && disabled(),
            "with neither the doorway's variable nor the box marker set, the documented off-switch \
             was refused anyway"
        );
    }
}
