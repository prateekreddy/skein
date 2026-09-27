//! Minting: the App's JWT, installation tokens for one repository or for reading, and the
//! refresh that places every box's token files before the hour is out.

use super::*;

// ───────────────────────────── minting ─────────────────────────────

/// base64url without padding, which is the only encoding a JWT accepts.
///
/// Hand-rolled rather than pulled in: skein has no base64 dependency, and this is the whole of what
/// would be used from one. Twenty lines against a crate in the supply chain of a tool that holds a
/// signing key is the right trade.
pub fn b64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let take = chunk.len() + 1;
        for i in 0..take {
            out.push(A[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out
}

/// The signed-input half of a GitHub App JWT: `base64url(header).base64url(payload)`.
///
/// `iat` is backdated a minute because GitHub rejects a token issued in its future, and a Mac whose
/// clock drifts forward by seconds is ordinary. `exp` is well inside the ten-minute maximum.
pub fn jwt_claim(app_id: &str, now: i64) -> String {
    let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    // Built by serde, not by `format!`. `app_credentials` already refuses a non-numeric App id, and
    // that guard is correct — but it is a guard at a DISTANCE: `jwt_claim` is `pub`, so a second
    // caller would not inherit it, and the claim would be rewritten around a quote rather than
    // merely carrying a wrong id. Escaping the value where it is serialised makes the function safe
    // on its own terms, and leaves the id check doing what it is actually good at: saying which
    // setting is wrong instead of letting GitHub answer 401.
    // A struct rather than `json!`, because serde emits struct fields in DECLARATION order while a
    // `json!` map sorts them — and sorting would silently change the bytes of every JWT skein has
    // ever minted. The claim is the same three fields in the same order the `format!` produced.
    #[derive(serde::Serialize)]
    struct Claim<'a> {
        iat: i64,
        exp: i64,
        iss: &'a str,
    }
    let payload = b64url(
        serde_json::to_string(&Claim {
            iat: now - 60,
            exp: now + 540,
            iss: app_id,
        })
        .unwrap_or_default()
        .as_bytes(),
    );
    format!("{header}.{payload}")
}

/// Sign a JWT claim with the App's private key, via `openssl`.
///
/// Shelling out rather than adding an RSA crate, for the same reason as [`b64url`]: this is one
/// `dgst` invocation, and openssl is on every machine skein runs on. The key is read by openssl
/// directly and never passes through skein's memory or a command line.
fn sign_jwt(claim: &str, key_path: &str) -> Result<Secret, String> {
    use std::io::Write;
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign", key_path, "-binary"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("openssl: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("openssl took no stdin")?
        .write_all(claim.as_bytes())
        .map_err(|e| format!("openssl: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("openssl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "could not sign the App JWT: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(Secret::new(format!("{claim}.{}", b64url(&out.stdout))))
}

/// Can this fleet produce a write token at all — by App, or by a stored PAT?
///
/// What [`box_is_scoped`] gates on. Scoping with no way to issue one would not narrow a box's reach;
/// it would take pushing away from every box at once.
///
/// A credential with a problem does not count, and neither does one with no token behind it.
/// Half-configured is the dangerous state: either would report the fleet as ready to scope, and
/// every box would come up unable to push.
pub fn can_issue_write_tokens() -> bool {
    app_credentials().is_ok()
        || write_credentials()
            .iter()
            .any(|c| c.problem().is_none() && credential_has_token(&c.id))
}

/// The App this fleet mints write tokens with, or why it cannot.
///
/// The id is in `config.json` and the key is a path, because that file is written 0644 and
/// round-trips through the browser on every settings save — a private key has no business in it.
/// The key itself sits beside it at 0600 and is only ever read by openssl.
pub fn app_credentials() -> Result<(String, String), String> {
    let config = crate::config::load_config();
    let id = config.github_app_id.trim().to_string();
    let key = match config.github_app_key.trim() {
        "" => crate::config::skein_home()
            .join("github-app.pem")
            .to_string_lossy()
            .into_owned(),
        p => crate::util::expand_tilde(p),
    };
    if id.is_empty() {
        return Err("no GitHub App configured: Settings → GitHub & keys → GitHub App ID".into());
    }
    // An App id is a number, and [`jwt_claim`] interpolates it straight into a JSON claim. Checked
    // rather than trusted: `config.json` is an ordinary file that can be hand-edited, and an `id`
    // holding a quote would rewrite the claim around it rather than merely being a wrong id. The
    // resulting JWT is signed, so GitHub would refuse it either way — this turns an obscure refusal
    // into a message that names the actual problem, and closes the injection on its own terms.
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "the GitHub App ID must be the numeric id, not {id:?} — Settings → GitHub & keys → GitHub App ID"
        ));
    }
    if !std::path::Path::new(&key).exists() {
        return Err(format!("the GitHub App key is not at {key}"));
    }
    Ok((id, key))
}

/// A GitHub App installation token scoped to exactly one repository.
///
/// Two calls: the installation that covers the repo, then a token restricted to it. The restriction
/// is the point — an installation token defaults to *every* repository the App is installed on, which
/// would rebuild the blast radius this module exists to remove.
pub fn mint_token(slug: &str) -> Result<Secret, String> {
    if !slug_is_nameable(slug) {
        return Err(format!("{slug:?} is not a repository"));
    }
    // A PAT its owner stored for this repository wins over the App, and the precedence is the point
    // rather than an optimisation: configuring one is a deliberate act that says "reach this repo
    // this way", usually by someone who did not want an App installed across their account at all.
    // Deferring to the App would quietly override that choice with the thing it was made to avoid.
    if let Some((_, token)) = credential_for(slug) {
        return Ok(token);
    }
    let (app_id, key_path) = app_credentials().map_err(|e| {
        format!("{e}, and no stored token covers {slug} — add one under Settings → GitHub & keys")
    })?;
    let jwt = sign_jwt(
        &jwt_claim(&app_id, chrono::Utc::now().timestamp()),
        &key_path,
    )?;

    let installation = crate::github::get_json(
        &format!("{}/installation", crate::github::repo_path(slug)),
        &jwt,
    )
    .map_err(|e| format!("the App is not installed on {slug}: {e}"))?;
    let id = installation
        .get("id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("no installation id for {slug}"))?;
    let name = slug.split_once('/').map(|(_, n)| n).unwrap_or(slug);

    // `issues: write` alongside `pull_requests: write`, because a comment on a PR is posted through
    // the *issues* endpoint — `POST /repos/{o}/{r}/issues/{n}/comments`. Without it `gh pr comment`
    // fails on a token that can already open the PR it cannot talk about, which reads as a bug.
    // Still nothing else: no `administration`, no `members`, no `workflows`.
    let body = serde_json::json!({
        "repositories": [name],
        "permissions": {
            "contents": "write",
            "pull_requests": "write",
            "issues": "write",
        },
    });
    let token = crate::github::send_json(
        "POST",
        &format!("/app/installations/{id}/access_tokens"),
        &jwt,
        &body,
    )?;
    token
        .get("token")
        .and_then(|v| v.as_str())
        .map(Secret::new)
        .ok_or_else(|| format!("GitHub returned no token for {slug}"))
}

/// Take every token out of a box's token directory.
///
/// The files, not the directory: the box's launcher creates it either way, and removing it under a
/// running box would leave the helper reading through a path that no longer exists.
fn discard_tokens(dir: &std::path::Path) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut failed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let outcome = match path.is_dir() {
            true => std::fs::remove_dir_all(&path), // `read/`, one token per installation owner
            false => std::fs::remove_file(&path),
        };
        if let Err(e) = outcome {
            failed.push(format!("{}: {e}", path.display()));
        }
    }
    match failed.is_empty() {
        true => Ok(()),
        false => Err(format!("could not withdraw {}", failed.join("; "))),
    }
}

/// Put the tokens a box may hold into its state directory, and take away the ones it may not.
///
/// Called on the server's tick, well inside the hour an installation token lives. Both halves matter
/// and the second more than the first: minting is how a grant starts working, but **removing** is
/// how a revoked or expired one stops. A refresher that only added would leave the last token it
/// wrote valid for up to an hour after the grant behind it was withdrawn.
///
/// Errors are per-repository and collected rather than propagated. One repo the App is not installed
/// on must not stop a box's own token being placed — that failure mode would take a box from
/// "cannot push to one repo" to "cannot push at all", which is the same outage the switch exists to
/// avoid causing.
pub fn refresh_tokens(box_name: &str) -> Vec<String> {
    let mut problems = Vec::new();
    // What changed about what this box can reach, for the host audit log (§9.5 R6). **Changes
    // only**: this runs on a cadence and rewrites every token as it rotates, so reporting each
    // write would be a line a minute per box and a log nobody reads. A credential arriving and a
    // credential being taken away are the two things worth a permanent record.
    let mut granted: Vec<String> = Vec::new();
    let mut withdrawn: Vec<String> = Vec::new();
    let dir = std::path::Path::new(&crate::fleet::box_state(box_name)).join("git-tokens");

    // **Not through a link** (§9.5 R8). `create_dir_all` follows a symlink at this path, and so
    // does the one inside `crate::secret::write` — so a `git-tokens` that is a link to somewhere
    // else is a directory the host creates through and places credentials in. The write itself is
    // already safe: `crate::secret::write` renames into place, and `rename` replaces a link rather
    // than following it. The directory was the half that was not.
    //
    // An ordinary box cannot make one — 4a binds its state read-only in its own namespace — which
    // is exactly why finding one means something is wrong rather than something is missing, and why
    // this refuses and names the path instead of repairing it. A privileged box may see every box's
    // files, deliberately; the resize archive and anything that ever wrote outside the cover are
    // the other ways.
    //
    // **Before the scoped check, not after.** The unscoped path does not write — it DELETES, every
    // file in the directory — and through a link that is skein emptying a directory somebody else
    // chose. The dangerous half of this site is the half that looks like cleanup.
    if let Ok(how) = std::fs::symlink_metadata(&dir) {
        if how.file_type().is_symlink() {
            return vec![format!(
                "{}: this box's token directory is a symbolic link, so placing a credential in it \
                 would write somewhere skein did not choose. Nothing was written. Remove the link \
                 and let {box_name} start again.",
                dir.display()
            )];
        }
    }

    // A box that is no longer scoped keeps nothing. This ran *before* the pruning below and returned,
    // so un-scoping a box left every token it had been given sitting in its state directory. For an
    // App token that self-heals within the hour; a stored PAT is returned verbatim by [`mint_token`]
    // and expires when its owner said it would, which may be next year. "Stop scoping this box" has
    // to mean the credentials go, not that they stop being refreshed.
    if !box_is_scoped(box_name) {
        // Everything goes, which is the largest withdrawal there is — so it is reported, and only
        // when there was something to take: an unscoped box is refreshed on every pass and has no
        // tokens on all but the first.
        let had = std::fs::read_dir(&dir)
            .map(|e| e.flatten().count())
            .unwrap_or(0);
        if let Err(e) = discard_tokens(&dir) {
            problems.push(e);
        } else if had > 0 {
            report(box_name, &[], &["every repository".to_string()]);
        }
        return problems;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return vec![format!("{}: {e}", dir.display())];
    }

    // The box's own repo, plus every repo its owner has granted and not yet had expire.
    let now = chrono::Utc::now();
    let mut want: Vec<String> = Vec::new();
    let own = box_repo_slug(box_name);
    if !own.is_empty() {
        want.push(own);
    }
    for g in live_grants_for(box_name, now) {
        if !want.iter().any(|w| same_repo(w, &g.repo)) {
            want.push(g.repo);
        }
    }

    // Anything with a token file that is no longer wanted loses it now. Done before minting so a
    // revoke takes effect even if GitHub is unreachable this tick.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_name() == "read" {
                continue; // pruned against its own list below
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let slug = name.replace("%2F", "/");
            if !want.iter().any(|w| same_repo(w, &slug))
                && std::fs::remove_file(entry.path()).is_ok()
            {
                withdrawn.push(slug.clone());
            }
        }
    }

    // 0600 is not a boundary *between boxes* — they share a uid — and this does not pretend
    // otherwise. It keeps the token out of anything that walks the tree without meaning to, and out
    // of the reach of anything on the host running as another user. The ordering that makes that
    // true is `crate::secret::write`'s, which is the only writer of a credential file in the crate.
    let write = |path: &std::path::Path, token: &Secret| -> Result<(), String> {
        let parent = path.parent().unwrap_or(&dir).to_path_buf();
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        crate::secret::write(path, token)
    };

    for slug in &want {
        let path = std::path::PathBuf::from(token_file(box_name, slug));
        // Whether this box already had a credential for this repo. A token is rewritten on every
        // refresh — App tokens last an hour — so writing one is not an event. Being given one for a
        // repo it did not have is.
        let had = path.exists();
        match mint_token(slug) {
            Ok(token) => match write(&path, &token) {
                Err(e) => problems.push(format!("{slug}: {e}")),
                Ok(()) if !had => granted.push(slug.clone()),
                Ok(()) => {}
            },
            // The old token goes when a new one cannot be had, and this is the whole of revocation
            // for a stored PAT. Forgetting a credential leaves the repo still *wanted* — it is the
            // box's own — so the prune above does not touch it, and leaving the file because the
            // mint failed meant "Forget" reported success while every box kept pushing with the
            // token its owner had just withdrawn. A stored PAT does not expire on its own, so
            // nothing else would ever have taken it away.
            //
            // Failing closed is the right direction here: the cost is a box that cannot push until
            // its credential is fixed, and it can ask. The cost the other way is a live credential
            // its owner believes is gone.
            Err(e) => {
                problems.push(format!("{slug}: {e}"));
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        withdrawn.push(slug.clone());
                        problems.push(format!(
                            "{slug}: the token this box was holding has been withdrawn"
                        ))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        problems.push(format!("{slug}: could not withdraw the old token: {e}"))
                    }
                }
            }
        }
    }

    // Reads: one token per installation, keyed by the owner it belongs to, plus the optional PAT.
    //
    // Failing to mint a read token is reported and never fatal. Reads degrade to what a box can
    // get with no credential of its own, which still covers every public repository — a box with
    // no read token is working with less, not broken, and must not lose the write token it already
    // has over it. (Degrade, not fail closed: see the module note on SKEIN-548 for why "no read
    // token" is not the same as "cannot read".)
    let read_dir = dir.join("read");
    let mut want_read: Vec<String> = Vec::new();
    if app_credentials().is_ok() {
        match installations() {
            Ok(found) => {
                for (id, owner) in found {
                    // The owner becomes a filename, so a login that could climb out of the directory
                    // is skipped rather than trusted because GitHub is unlikely to send one.
                    if owner.is_empty() || owner.contains('/') || owner.contains("..") {
                        continue;
                    }
                    match mint_read_token(id) {
                        Ok(token) => match write(&read_dir.join(&owner), &token) {
                            Ok(()) => want_read.push(owner),
                            Err(e) => problems.push(format!("read {owner}: {e}")),
                        },
                        Err(e) => problems.push(format!("read {owner}: {e}")),
                    }
                }
            }
            Err(e) => problems.push(format!("listing installations: {e}")),
        }
    }
    if let Some(pat) = read_pat() {
        match write(&read_dir.join("_any"), &pat) {
            Ok(()) => want_read.push("_any".into()),
            Err(e) => problems.push(format!("read token: {e}")),
        }
    }
    if let Ok(entries) = std::fs::read_dir(&read_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !want_read.contains(&name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    report(box_name, &granted, &withdrawn);
    problems
}

/// What changed about what a box can push to, into the log skein does not own (§9.5 R6).
///
/// **Silent when nothing changed**, which is most refreshes: a rotation is not an event, and a log
/// that recorded one per box per hour would be a log nobody reads at the moment they need to.
///
/// Reported after the files moved, so the entry describes what is on disk rather than what was
/// intended. Two entries rather than one when both happened, because "granted" and "withdrawn" are
/// answers to different questions and somebody grepping for one should not have to parse the other.
fn report(box_name: &str, granted: &[String], withdrawn: &[String]) {
    for (what, which) in [("granted", granted), ("withdrew", withdrawn)] {
        if which.is_empty() {
            continue;
        }
        crate::warden_client::reported(
            &format!("git-tokens-{box_name}"),
            &format!("{what} a box push credentials"),
            &format!("{box_name}: {}", which.join(", ")),
        );
    }
}

/// Every account or org this App is installed on, as `(installation id, owner login)`.
///
/// One installation per account, so an App on your personal account and on an org is two of them —
/// and an installation token belongs to exactly one. That is why reads are keyed by owner rather
/// than held as a single token: there is no such thing as one token spanning both.
pub fn installations() -> Result<Vec<(i64, String)>, String> {
    let (app_id, key_path) = app_credentials()?;
    let jwt = sign_jwt(
        &jwt_claim(&app_id, chrono::Utc::now().timestamp()),
        &key_path,
    )?;
    let list = crate::github::get_json("/app/installations", &jwt)?;
    Ok(list
        .as_array()
        .map(|v| v.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|i| {
            let id = i.get("id")?.as_i64()?;
            let login = i.get("account")?.get("login")?.as_str()?.to_string();
            Some((id, login))
        })
        .collect())
}

/// A read-only token covering everything one installation reaches.
///
/// The difference from [`mint_token`] is a single field: no `repositories`, so the token is not
/// restricted to one repo. That is the whole of "read any repo you installed the App on" — the
/// installation list *is* the control, maintained in one place and live, so adding a repo there
/// makes it readable with nothing to re-mint and no second credential to keep in step.
///
/// Read-only by construction, not by convention: `contents: read` is the strongest thing in it.
pub fn mint_read_token(installation: i64) -> Result<Secret, String> {
    let (app_id, key_path) = app_credentials()?;
    let jwt = sign_jwt(
        &jwt_claim(&app_id, chrono::Utc::now().timestamp()),
        &key_path,
    )?;
    let body = serde_json::json!({
        "permissions": { "contents": "read", "metadata": "read" },
    });
    let token = crate::github::send_json(
        "POST",
        &format!("/app/installations/{installation}/access_tokens"),
        &jwt,
        &body,
    )?;
    token
        .get("token")
        .and_then(|v| v.as_str())
        .map(Secret::new)
        .ok_or_else(|| format!("GitHub returned no read token for installation {installation}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitgate::testkit::*;

    /// A token directory that is a symbolic link stops the placement, rather than redirecting it.
    ///
    /// The rule is §9.5 R8: no privileged actor follows a path a box can influence. The host mints
    /// a write credential and places it at `<box state>/git-tokens/<repo>`, and `create_dir_all`
    /// follows a link at that path — so a `git-tokens` pointing somewhere else is a directory the
    /// host would create through and drop a live token into.
    ///
    /// **The refusal is the fix, not a repair.** An ordinary box cannot make this link — 4a binds
    /// its own state read-only inside its namespace — so finding one means something is wrong, and
    /// the honest response to "something is wrong here" is to stop and say where, not to delete
    /// somebody's link and carry on.
    #[test]
    fn a_token_directory_that_is_a_link_is_refused_rather_than_followed() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        // Somewhere the token must not land: a directory outside the box entirely.
        let elsewhere = home.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let state = std::path::PathBuf::from(crate::fleet::box_state("web-main"));
        std::fs::create_dir_all(&state).unwrap();
        std::os::unix::fs::symlink(&elsewhere, state.join("git-tokens")).unwrap();

        // Something the link points at, so the *deletion* half is visible. This box is not scoped,
        // which is the path that empties the directory rather than filling it — and emptying one
        // through a link is skein deleting files somebody else chose.
        std::fs::write(elsewhere.join("not-skeins"), b"someone else's file").unwrap();

        let problems = refresh_tokens("web-main");
        std::env::remove_var("SKEIN_HOME");

        assert!(
            problems.iter().any(|p| p.contains("symbolic link")),
            "a linked token directory was not refused: {problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("git-tokens")),
            "the refusal does not name the path somebody has to go and look at: {problems:?}"
        );
        assert!(
            elsewhere.join("not-skeins").exists(),
            "skein deleted through the link — the unscoped path empties the directory, and that is \
             the half of this site that looks like cleanup"
        );
        assert_eq!(
            std::fs::read_dir(&elsewhere).unwrap().count(),
            1,
            "the host wrote through the link, into a directory it did not choose"
        );
    }

    /// An App id is interpolated straight into a signed JSON claim, so it is checked like input.
    #[test]
    fn an_app_id_that_is_not_a_number_is_refused_before_it_reaches_the_claim() {
        let (_lock, _home, _env) = fresh_home();
        let mut cfg = crate::config::load_config();
        cfg.github_app_id = "12\",\"iss\":\"999".into();
        crate::config::save_config(&cfg).unwrap();
        let why = app_credentials().unwrap_err();
        assert!(
            why.contains("numeric"),
            "the id must be refused by name, not left to produce a puzzling 401: {why}"
        );
    }

    #[test]
    fn base64url_matches_the_encoding_a_jwt_actually_accepts() {
        // Known vectors, and specifically the padding cases: a JWT rejects `=`, and the 1- and
        // 2-byte tails are where a hand-rolled encoder gets it wrong.
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(b64url(b"foob"), "Zm9vYg");
        assert_eq!(b64url(b"fooba"), "Zm9vYmE");
        assert_eq!(b64url(b"foobar"), "Zm9vYmFy");
        // The two characters that differ from plain base64, which is the whole reason for -_ .
        assert_eq!(b64url(&[251, 255]), "-_8");
    }

    #[test]
    fn the_jwt_is_backdated_so_a_drifting_host_clock_does_not_mint_a_future_token() {
        let claim = jwt_claim("12345", 1_000_000);
        let payload = claim.split('.').nth(1).unwrap();
        // Decode enough to assert the numbers, without pulling in a decoder: the payload is short
        // and its shape is fixed, so re-encoding the expectation is the cheapest check.
        assert_eq!(
            payload,
            b64url(br#"{"iat":999940,"exp":1000540,"iss":"12345"}"#)
        );
    }
}
