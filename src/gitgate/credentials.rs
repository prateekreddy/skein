//! The per-repository write credentials an owner stores and the optional read PAT: storing,
//! removing, choosing one for a repo, and asking GitHub what a token may do.

use super::*;

// ───────────────────────────── stored fine-grained PATs ─────────────────────────────

/// A fine-grained PAT its owner minted by hand, for **one** repository.
///
/// The alternative to the App, for someone who would rather not install one across their account at
/// all: a token they created themselves, scoped in GitHub's own UI to exactly the repository they
/// chose. skein never sees anything wider, and cannot — the token *is* the scope.
///
/// **Exactly one repository, and that is the whole security argument.** A token covering three repos
/// would hand all three to whichever box receives it: the credential helper offers it only when git
/// asks about one of them, but the helper runs *inside* the box as the same uid as the agent, so
/// anything it can read the agent can read. A helper routes; it cannot contain. The only way a
/// broad token stays broad-but-safe is never entering the box at all, which is a host-side proxy and
/// a different design. Until then, one repo per token means the credential a box holds is already
/// exactly as narrow as its rights — nothing is trusted to stay in its lane.
///
/// The repository name here is a **claim**, not the enforcement. GitHub enforces what the token can
/// reach; this is how skein knows which repo to hand it to. Getting it wrong costs a token that does
/// not work, never one that works too well.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteCredential {
    #[serde(default)]
    pub id: String,
    /// What its owner calls it, for the cockpit. Never used as a path.
    #[serde(default)]
    pub label: String,
    /// `owner/name`. A list rather than a string because this once held several, and a stored file
    /// written then must still parse — as something [`WriteCredential::problem`] refuses, not as
    /// something that silently keeps working.
    #[serde(default)]
    pub repos: Vec<String>,
}

impl WriteCredential {
    /// Why this credential must not be handed to a box, or `None` if it may be.
    ///
    /// Checked when read and not only when written, because `github-pats.json` is an ordinary file
    /// on the host: it can be hand-edited, and a credential naming three repos would otherwise be
    /// refused at the form and accepted by the code that actually places tokens.
    pub fn problem(&self) -> Option<String> {
        if !valid_credential_id(&self.id) {
            return Some(format!("{:?} is not a credential id", self.id));
        }
        match self.repos.len() {
            1 => {}
            0 => return Some("names no repository".into()),
            n => {
                return Some(format!(
                    "names {n} repositories; a stored token must cover exactly one, or every box \
                     that gets it can write all {n}"
                ))
            }
        }
        match slug_is_nameable(&self.repos[0]) {
            true => None,
            false => Some(format!("{:?} is not a repository", self.repos[0])),
        }
    }

    /// The single repository this token covers, or empty if it is not usable.
    pub fn repo(&self) -> &str {
        match self.problem() {
            None => &self.repos[0],
            Some(_) => "",
        }
    }
}

fn credentials_path() -> std::path::PathBuf {
    crate::config::skein_home().join("github-pats.json")
}

/// The token file for one credential — 0600, and never in the JSON above.
///
/// Same split, and the same reason, as [`crate::tracking::connection_token_path`]: `github-pats.json` is read
/// by the settings screen, so a token in it would be handed to every browser tab that opens Settings.
/// The cockpit only ever learns *whether* one is set.
fn credential_token_path(id: &str) -> std::path::PathBuf {
    crate::config::skein_home().join("github-pats").join(id)
}

/// An id becomes a filename, so it is checked like one. A token written to a path a caller chose is
/// a path traversal wearing a config field's clothes.
pub fn valid_credential_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Every stored credential, without their tokens.
///
/// Unusable ones are returned too, so the cockpit can say *why* a repo has no token rather than
/// showing a list that silently omits the entry someone is looking at.
///
/// An unreadable file reads as no credentials — the same fail-closed direction as [`grants`], and
/// said out loud once for the same reason. The writers do not use this: they read under the lock
/// through [`crate::util::update_json`], which refuses rather than replacing a list it could not
/// read (SKEIN-359).
pub fn write_credentials() -> Vec<WriteCredential> {
    match crate::util::read_json_or_why::<Vec<WriteCredential>>(&credentials_path()) {
        Ok(found) => found.unwrap_or_default(),
        Err(why) => {
            static TOLD: std::sync::Once = std::sync::Once::new();
            TOLD.call_once(|| {
                eprintln!(
                    "skein: cannot read your stored GitHub tokens ({why}) — Settings will show \
                     none, no box will be given one, and skein will refuse to write over that \
                     file. The tokens themselves are in `github-pats/` and are untouched; fix or \
                     move `github-pats.json`."
                );
            });
            Vec::new()
        }
    }
}

/// Is a token stored for this credential?
pub fn credential_has_token(id: &str) -> bool {
    valid_credential_id(id) && credential_token_path(id).exists()
}

/// The credential for `slug`, if one is stored and usable.
///
/// First match wins, in the order its owner arranged them. A repo named twice is a preference, not
/// a conflict — both tokens write the same one repo, so either answer is correct.
pub fn credential_for(slug: &str) -> Option<(WriteCredential, Secret)> {
    write_credentials().into_iter().find_map(|c| {
        // A credential with a problem is skipped rather than used. This is the check that actually
        // holds: the form refuses a multi-repo entry, but the file behind it can be hand-edited,
        // and this is the last point before a token is placed inside a box.
        if c.problem().is_some() || !same_repo(c.repo(), slug) {
            return None;
        }
        let token = crate::secret::read(&credential_token_path(&c.id))
            .ok()
            .flatten()?;
        Some((c, token))
    })
}

/// Any user PAT this fleet already holds, for the host's one GitHub call that names **no**
/// repository — "who am I" (`prq::viewer`). A call about a repository asks [`credential_for_repo`],
/// which never answers with a token filed under some other repository (SKEIN-953).
///
/// The principle: **one credential the user chose, doing every job it is capable of.** A per-repo
/// write token is a PAT belonging to a person — it can say who that person is, and it can read the
/// repository it writes to. Asking someone who has already stored one to *also* authenticate `gh`
/// is asking for a second credential to do a job the first one covers, and on Linux that second one
/// lives in the login keyring, so it asks for a password as well.
///
/// A [`read_pat`] is preferred over these by the caller, because a read credential may safely be
/// broad and a write one may not. This is the fallback for a fleet that has only ever been given
/// write tokens — the ordinary PAT path.
///
/// Not an App: an installation token authenticates an *installation*, not a person, so it cannot
/// answer "whose review is this waiting on". That limit is the App's, not skein's, and the review
/// queue says so rather than silently listing nothing.
pub fn any_user_pat() -> Option<Secret> {
    write_credentials().into_iter().find_map(|c| {
        if c.problem().is_some() {
            return None;
        }
        crate::secret::read(&credential_token_path(&c.id))
            .ok()
            .flatten()
    })
}

/// Store or replace a credential's description. Its token is set separately.
///
/// **Read-modify-write over the whole list, so it is done under the list's own lock and refuses on
/// a file it could not read** ([`crate::util::update_json`], SKEIN-359). Both halves were missing.
/// There was no lock at all, so two credentials stored from two cockpit tabs was last-write-wins
/// and one of them simply never happened; and the read answered "no credentials" for a file that
/// was merely unparseable, so storing one credential over a corrupt `github-pats.json` deleted the
/// description of every other one. That is not a cosmetic loss: the tokens live in `github-pats/`
/// keyed by id, and an entry that is gone from this file is a token skein can no longer match to a
/// repository — a live secret on disk that nothing will ever use again or name to the person who
/// put it there.
pub fn set_write_credential(id: &str, label: &str, repos: &[String]) -> Result<(), String> {
    if !valid_credential_id(id) {
        return Err(format!(
            "{id:?} is not a credential id (lowercase letters, digits and dashes)"
        ));
    }
    let next = WriteCredential {
        id: id.to_string(),
        label: label.trim().to_string(),
        repos: repos.to_vec(),
    };
    if let Some(why) = next.problem() {
        return Err(format!("this token {why}"));
    }
    crate::util::update_json(&credentials_path(), |all: &mut Vec<WriteCredential>| {
        all.retain(|c| c.id != id);
        all.push(next);
        Ok(())
    })
}

/// Store (or, with an empty value, forget) a credential's token.
///
/// `token` arrives as a `&str` because that is the shape it arrives in — typed into Settings and
/// carried in a request body — and becomes a [`Secret`] at the last moment before it reaches disk.
pub fn set_credential_token(id: &str, token: &str) -> Result<(), String> {
    if !valid_credential_id(id) {
        return Err(format!("not a credential id: {id:?}"));
    }
    let path = credential_token_path(id);
    let token = token.trim();
    if token.is_empty() {
        return crate::secret::forget(&path);
    }
    let dir = crate::config::skein_home().join("github-pats");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    crate::secret::write(&path, &Secret::new(token))
}

/// Forget a credential entirely — its description and its token.
///
/// Under the lock and refusing on an unreadable list, exactly as [`set_write_credential`] does:
/// forgetting one credential must not be how the other four are forgotten.
///
/// **The token goes first, and the order is deliberate.** If the list cannot be written — a refusal
/// here, or a full disk — the entry stays behind with no token, which the cockpit already shows as
/// "no token stored" and a person can act on. The other order would leave the opposite: a live
/// secret in `github-pats/` that no entry names, so nothing will use it again and nobody will be
/// told it is there.
pub fn remove_write_credential(id: &str) -> Result<(), String> {
    if !valid_credential_id(id) {
        return Err(format!("not a credential id: {id:?}"));
    }
    let _ = set_credential_token(id, "");
    crate::util::update_json(&credentials_path(), |all: &mut Vec<WriteCredential>| {
        all.retain(|c| c.id != id);
        Ok(())
    })
}

/// One repository's answer to "could a box actually push here?"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub repo: String,
    pub ok: bool,
    /// Where the credential came from — `app` or a stored token's id — or why there is none.
    pub detail: String,
}

/// Actually mint a token for every managed repo, and say what happened.
///
/// [`scope_status`] is deliberately offline, which means it can only report that a credential is
/// *configured*: an App ID that GitHub rejects, a key belonging to a different App, or an App
/// installed on none of these repositories all read as ready. That gap is the whole reason a user
/// cannot tell a working setup from a broken one, and it cannot be closed without spending a round
/// trip — so this is the explicit act that spends it, rather than a background check that would
/// make a slow morning look like a broken fleet.
///
/// The tokens minted here are thrown away. Nothing is placed in a box; this only asks GitHub
/// whether it *would* issue one.
///
/// **A stored PAT is checked against GitHub too, and that is not a detail.** [`mint_token`] returns a
/// stored token verbatim without a round trip — correctly, since minting it is not skein's job — so
/// asking it alone answered "is a token on disk", not "does this token work". An expired or revoked
/// PAT reported **ok**. That is exactly the wrong way round: the App path renews itself hourly and
/// cannot quietly rot, while a PAT carries an expiry its owner chose months ago and fails silently.
/// The credential most in need of checking was the one the check could not see.
pub fn probe_credentials() -> Vec<ProbeResult> {
    let mut out = Vec::new();
    for repo in crate::repos::load_repos() {
        let Some(slug) = repo_slug(&repo) else {
            out.push(ProbeResult {
                repo: repo.id,
                ok: true,
                detail: "no GitHub remote — nothing to scope, and nowhere to push".into(),
            });
            continue;
        };
        let stored = credential_for(&slug);
        let source = match &stored {
            Some((c, _)) => {
                let named = match c.label.trim().is_empty() {
                    true => c.id.clone(),
                    false => c.label.clone(),
                };
                format!("stored token “{named}”")
            }
            None => "the GitHub App".into(),
        };
        out.push(match mint_token(&slug) {
            Ok(token) => match &stored {
                // Minted by the App: GitHub answered a moment ago, and the token is good for an
                // hour. Nothing further to ask.
                None => ProbeResult {
                    repo: slug,
                    ok: true,
                    detail: format!("a write token was issued by {source}"),
                },
                Some(_) => match check_token(&token, &slug) {
                    Ok(true) => ProbeResult {
                        repo: slug,
                        ok: true,
                        detail: format!("{source} works, and GitHub says it may push"),
                    },
                    // Reachable and refused. Named separately from "cannot reach GitHub" because
                    // one is a credential to replace and the other is a network to wait out.
                    Ok(false) => ProbeResult {
                        repo: slug.clone(),
                        ok: false,
                        detail: format!(
                            "{source} cannot write {slug} — expired, revoked, or scoped to another \
                             repository. Store a new one under Settings → GitHub & keys"
                        ),
                    },
                    Err(e) => ProbeResult {
                        repo: slug,
                        ok: false,
                        detail: format!("{source} could not be checked: {e}"),
                    },
                },
            },
            Err(e) => ProbeResult {
                repo: slug,
                ok: false,
                detail: e,
            },
        });
    }
    out
}

/// Does `token` actually carry push rights for `slug` right now?
///
/// `GET /repos/{slug}` returns a `permissions` object for an authenticated caller, so one request
/// answers both halves — the token is still valid, *and* it reaches this repository with write. A
/// 401/404 is the answer for an expired token and for one scoped somewhere else alike, which is why
/// the message above names both rather than guessing between them.
///
/// The token goes in a `--config` document, never argv: a command line is readable by every process
/// on the host, and this is a live push credential. That is [`crate::github`]'s doing rather than
/// this module's — see the note on the module about why there is only one client left.
fn check_token(token: &Secret, slug: &str) -> Result<bool, String> {
    match crate::github::get_json(&crate::github::repo_path(slug), token) {
        Ok(v) => Ok(v
            .get("permissions")
            .and_then(|p| p.get("push"))
            .and_then(|p| p.as_bool())
            .unwrap_or(false)),
        // GitHub answering "Bad credentials" is a *successful* check with a negative answer, not a
        // failure to check. Told apart here so an expired PAT reads as a credential to replace
        // rather than as a network problem to retry.
        Err(e) if e.contains("Bad credentials") || e.contains("Not Found") => Ok(false),
        Err(e) => Err(e),
    }
}

/// An optional read-only PAT covering everything its owner chose.
///
/// **Optional, and deliberately so.** Configuring skein should ask for *one* kind of credential, not
/// two: with an App, reads already come from the installation, and on the PAT path a per-repo write
/// token plus the public repos that need no token covers the ordinary case. This exists for someone who
/// specifically wants cross-repo reads of private repos without running an App — never as a step the
/// setup asks for.
pub fn read_pat() -> Option<Secret> {
    crate::secret::read(&crate::config::skein_home().join("github-read-token"))
        .ok()
        .flatten()
}

/// Store (or, empty, forget) the optional read-only PAT.
pub fn set_read_pat(token: &str) -> Result<(), String> {
    let home = crate::config::skein_home();
    let path = home.join("github-read-token");
    let token = token.trim();
    if token.is_empty() {
        return crate::secret::forget(&path);
    }
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    crate::secret::write(&path, &Secret::new(token))
}

// ───────────────────────────── which of your credentials reaches a repository ─────────────────────────────

/// Where a GitHub token skein acts with came from.
///
/// The order is the point: skein offers several ways to give it GitHub access, and the review queue
/// used to require a fourth — `gh auth login` — because it was built out of the `gh` CLI and `gh`
/// only knows its own store. One credential the user chose should do every job it is capable of.
///
/// **Which order depends on the question** (the owner's decision of 2026-09-27, SKEIN-953). A call
/// about one repository asks [`credential_for_repo`], which puts that repository's own stored token
/// first; the one call that is about nobody's repository — "who am I", `prq::viewer` — asks
/// `prq`'s host credential, which has no repository to prefer.
///
/// Here rather than in `prq` because both of skein's GitHub paths need it: `prq`'s API calls and
/// `repos`' fleet-side git (`repos::fleet_git`), and `repos` may reach `gitgate` but not `prq`
/// (`docs/modules.toml`). `prq` re-exports it, so its callers spell it as they always have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhToken {
    /// `$GH_TOKEN` / `$GITHUB_TOKEN`.
    Environment,
    /// The read token stored in Settings. A user's own PAT, broad by design and **for reads only**:
    /// [`credential_for_repo`] never hands it to a write (SKEIN-1176), because a contents:read token
    /// fails every post, merge and label it is given.
    ReadToken,
    /// A per-repo token the owner stored. For a call about its own repository it is the first
    /// choice; for "who am I", which names no repository, any one of them can say who its owner is.
    WritePat,
    /// The host's own `gh` login, asked for last.
    ///
    /// It was missing, and its absence contradicted this list's own reason for existing: skein used
    /// to read this login itself, to put the account token in front of every box, so a fleet whose
    /// boxes pushed as you necessarily had a credential here that could say who you are. Reported
    /// from a live fleet: `skein doctor` showing `gh secret seeded` and `boxes push with this
    /// account's gh token` three lines above `github token none`, with every pull request queue
    /// answering 502.
    ///
    /// That seeding is gone with the machine-global store (architecture §13a), so this arm no longer
    /// has a fleet-wide caller keeping it warm — which makes it more important rather than less, as
    /// the queue's last resort on a machine where somebody has run `gh auth login`.
    GhCli,
    /// Nothing. The queue says so instead of reporting an empty queue, which is the one failure it
    /// must never look like.
    None,
}

impl GhToken {
    pub fn label(self) -> &'static str {
        match self {
            GhToken::Environment => "$GH_TOKEN",
            GhToken::ReadToken => "the read token in Settings",
            GhToken::WritePat => "a repository write token you stored",
            GhToken::GhCli => "the host's `gh` login",
            GhToken::None => "no token at all",
        }
    }

    /// A stable word for the source, for the cockpit to choose its own sentence by — the page names
    /// where to renew a credential, and that differs by source (SKEIN-1179).
    pub fn key(self) -> &'static str {
        match self {
            GhToken::Environment => "env",
            GhToken::ReadToken => "read",
            GhToken::WritePat => "repo",
            GhToken::GhCli => "gh",
            GhToken::None => "none",
        }
    }
}

/// What a repository-scoped call is going to do with the token it asks for.
///
/// Two values because there are two kinds of credential a person gives skein: tokens that may act,
/// and the read token, which may only look (SKEIN-1176). A verdict, a merge, a label, a resolved
/// thread, a deleted branch, and a review session that posts its own findings are all `Write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Read,
    Write,
}

/// `$GH_TOKEN`, then `$GITHUB_TOKEN`, when either says anything. Read on every call.
pub fn environment_token() -> Option<Secret> {
    ["GH_TOKEN", "GITHUB_TOKEN"].iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| Secret::new(value.trim()))
    })
}

/// The host's `gh` login, once asked: `None` until then, `Some(None)` when `gh` had nothing.
static GH_CLI: std::sync::Mutex<Option<Option<Secret>>> = std::sync::Mutex::new(None);

/// The host's `gh` login — **the one credential source that is remembered**, found or not.
///
/// Every other source is an environment read or a small file read, which is nothing beside the
/// network call every caller is about to make, so they are read again on every call and a token
/// replaced or forgotten in Settings is the one the next call uses (SKEIN-1177). This one is a
/// subprocess with a 15-second ceiling that can unlock a system keyring on a modern Linux, and
/// re-running it per GitHub request — several per queue refresh, per repo, every three minutes from
/// the badge poll — would turn a host with no credential from slow to unusable. So `gh` is asked
/// once per process, and a host given a `gh` login after startup needs a restart. The routes the
/// cockpit sends a person to — Settings, which writes the files [`credential_for`] and [`read_pat`]
/// read — do not.
pub fn gh_login() -> Option<Secret> {
    let mut asked = match GH_CLI.lock() {
        Ok(asked) => asked,
        Err(poisoned) => poisoned.into_inner(),
    };
    if asked.is_none() {
        *asked = Some(crate::repos::gh_cli_token().map(|t| Secret::new(&t)));
    }
    // A fresh `Secret` per caller rather than the remembered one, because [`Secret`] has no `Clone`
    // on purpose: every copy is another buffer to scrub, so a copy is made where somebody can see
    // it being made.
    asked
        .as_ref()
        .and_then(|found| found.as_ref())
        .map(|held| Secret::new(held.expose()))
}

/// Forget what `gh` said, so the next [`gh_login`] asks it again. For tests, which put a different
/// stub `gh` on `PATH` each.
pub fn forget_gh_login() {
    if let Ok(mut asked) = GH_CLI.lock() {
        *asked = None;
    }
}

/// **The one resolver for a GitHub credential about one repository** (SKEIN-953, the owner's
/// decision of 2026-09-27): which of your credentials reaches `slug`, and where it came from.
///
/// 1. The token you stored for `slug` ([`credential_for`]) — the same one its boxes push with.
///    Storing one is a deliberate act that says "reach this repo this way", which is why
///    [`mint_token`] gives it the same precedence over the App.
/// 2. `$GH_TOKEN` / `$GITHUB_TOKEN`.
/// 3. The read token — **for [`Need::Read`] only** (SKEIN-1176).
/// 4. The host's `gh` login.
/// 5. Nothing.
///
/// Every GitHub API call skein makes about a repository asks this (`prq::token_for`), and so does
/// the fleet-side git that clones and fetches mirrors (`repos::fleet_git`) — one resolver, so the
/// queue, the verdict, the merge and the mirror of one repository cannot disagree about which
/// token reaches it.
///
/// There is no "any stored token" step. That was the host token's, and it handed repository A's
/// token to every call about repository B — a fine-grained PAT outside its scope answers 401, so
/// the call failed while a token that covered B sat one line further down the file (SKEIN-953).
///
/// Read fresh on every call, stored files included (SKEIN-1177); only `gh` is asked once. Nothing
/// is kept per repository in a process-wide slot, which was SKEIN-953's own constraint.
pub fn credential_for_repo(slug: &str, need: Need) -> (GhToken, Option<Secret>) {
    if let Some((_, token)) = credential_for(slug) {
        return (GhToken::WritePat, Some(token));
    }
    if let Some(token) = environment_token() {
        return (GhToken::Environment, Some(token));
    }
    if need == Need::Read {
        if let Some(pat) = read_pat() {
            return (GhToken::ReadToken, Some(pat));
        }
    }
    match gh_login() {
        Some(token) => (GhToken::GhCli, Some(token)),
        None => (GhToken::None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitgate::testkit::*;

    /// Register `slug` as a box's own repo and place a token for it, as a live fleet would.
    fn box_holding(name: &str, slug: &str) -> std::path::PathBuf {
        crate::repos::save_repos(&[crate::repos::Repo {
            read_prs: false,
            id: name.into(),
            source: format!("https://github.com/{slug}.git"),
            store: String::new(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }])
        .unwrap();
        let problems = refresh_tokens(name);
        let path = std::path::PathBuf::from(token_file(name, slug));
        assert!(
            path.exists(),
            "the fixture never placed a token ({problems:?})"
        );
        path
    }

    /// The revocation gap. Forgetting a stored PAT left every box still holding it.
    ///
    /// The repo is the box's *own*, so it stays "wanted" and the prune never touches it — and the
    /// mint, having nothing left to mint from, used to fail and leave the previous file exactly
    /// where it was. A stored PAT does not expire on its own, so nothing would ever have removed it:
    /// "Forget" reported success and revoked nothing.
    #[test]
    fn forgetting_a_stored_token_takes_it_away_from_the_boxes_holding_it() {
        let (_lock, _home, _env) = fresh_home();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        set_write_credential("mine", "one repo", &["a/one".into()]).unwrap();
        set_credential_token("mine", "skein-test-pat-XYZ").unwrap();
        // A second, unrelated credential, so the fleet can still issue *something* after the first
        // is forgotten. Without it `can_issue_write_tokens` goes false, the box stops being scoped,
        // and the token is taken by the un-scoping path instead — which is a different fix, tested
        // below. This one has to fail to mint while the box is still very much scoped.
        set_write_credential("other", "elsewhere", &["b/two".into()]).unwrap();
        set_credential_token("other", "skein-test-pat-OTHER").unwrap();

        let path = box_holding("worker", "a/one");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "skein-test-pat-XYZ"
        );

        remove_write_credential("mine").unwrap();
        assert!(box_is_scoped("worker"), "the box must still be scoped here");
        let problems = refresh_tokens("worker");

        assert!(
            !path.exists(),
            "the box is still holding a credential its owner withdrew"
        );
        assert!(
            problems.iter().any(|p| p.contains("withdrawn")),
            "a withdrawal has to be reported, not done silently: {problems:?}"
        );
    }

    /// Un-scoping a box takes its tokens, rather than merely stopping their refresh.
    ///
    /// This returned before the pruning, so every token a box had been given stayed in its state
    /// directory. An App token would expire within the hour; a stored PAT is handed over verbatim
    /// and expires whenever its owner said — possibly never.
    #[test]
    fn un_scoping_a_box_withdraws_what_it_was_already_holding() {
        let (_lock, _home, _env) = fresh_home();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        set_write_credential("mine", "one repo", &["a/one".into()]).unwrap();
        set_credential_token("mine", "skein-test-pat-XYZ").unwrap();
        let path = box_holding("worker", "a/one");

        set_box_scope("worker", Some("fleet")).unwrap();
        let problems = refresh_tokens("worker");

        assert!(problems.is_empty(), "{problems:?}");
        assert!(
            !path.exists(),
            "'stop scoping this box' has to mean the credentials go"
        );
    }

    /// A token is never world-readable, not even for the instant between write and chmod.
    ///
    /// `write_atomic` creates its temp with the umask and would be chmodded afterwards, which is the
    /// window this closes. Not a boundary between boxes — they share a uid — but it is the rule the
    /// rest of this module already follows, and the tokens handed to boxes were the ones not
    /// following it.
    #[cfg(unix)]
    #[test]
    fn a_placed_token_is_never_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let (_lock, home, _env) = fresh_home();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        set_write_credential("mine", "one repo", &["a/one".into()]).unwrap();
        set_credential_token("mine", "skein-test-pat-XYZ").unwrap();
        let path = box_holding("worker", "a/one");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a placed token was {mode:o}");

        // And the same rule for the credential store the host keeps.
        let stored = crate::config::skein_home().join("github-pats").join("mine");
        let mode = std::fs::metadata(&stored).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the stored token was {mode:o}");
        drop(home);
    }

    #[test]
    fn a_stored_token_is_preferred_over_the_app_for_the_repos_it_covers() {
        // The precedence is the feature, not an optimisation. Someone stores a PAT precisely because
        // they did not want an App reaching across their account — deferring to the App would
        // override that choice with the exact thing it was made to avoid.
        let (_lock, _home, _env) = fresh_home();
        set_write_credential("mine", "my one repo", &["a/one".into()]).unwrap();
        set_credential_token("mine", "skein-test-pat-XYZ").unwrap();

        let (found, token) = credential_for("a/one").expect("a stored token covers a/one");
        assert_eq!(token.expose(), "skein-test-pat-XYZ");
        assert_eq!(found.label, "my one repo");
        assert_eq!(
            mint_token("a/one").unwrap().expose(),
            "skein-test-pat-XYZ",
            "the stored token is what a box is given"
        );

        // And a repo it does not cover falls through — to the App, or to a message naming both ways
        // of fixing it rather than only the App.
        assert!(credential_for("b/other").is_none());
        let why = mint_token("b/other").unwrap_err();
        assert!(why.contains("b/other"), "{why}");
        assert!(
            why.contains("GitHub & keys"),
            "the error must say where to add a token: {why}"
        );
    }

    #[test]
    fn a_token_covering_more_than_one_repo_is_refused_outright() {
        // The security argument for the whole feature. A token covering three repos hands all three
        // to whichever box receives it — the helper offers it for one, but the helper runs inside
        // the box as the agent's own uid, so anything it can read the agent can read. A helper
        // routes; it cannot contain.
        let (_lock, _home, _env) = fresh_home();
        let why = set_write_credential("three", "", &["a/one".into(), "a/two".into()]).unwrap_err();
        assert!(why.contains("exactly one"), "{why}");
        assert!(
            write_credentials().is_empty(),
            "it must not have been stored"
        );

        assert!(
            set_write_credential("none", "", &[]).is_err(),
            "a credential naming no repository is not a credential"
        );
    }

    #[test]
    fn a_multi_repo_credential_hand_edited_into_the_file_is_still_never_used() {
        // The check that actually holds. `github-pats.json` is an ordinary host file: refusing this
        // only at the form would leave the code that places tokens accepting what the form rejects.
        let (_lock, home, _env) = fresh_home();
        std::fs::write(
            home.join("github-pats.json"),
            r#"[{"id":"wide","label":"","repos":["a/one","a/two"]}]"#,
        )
        .unwrap();
        set_credential_token("wide", "skein-test-t").unwrap();

        assert!(
            credential_for("a/one").is_none(),
            "a hand-edited multi-repo token was handed out anyway"
        );
        assert!(credential_for("a/two").is_none());
        assert!(
            !can_issue_write_tokens(),
            "and it must not count as a way to issue tokens, or boxes scope with nothing to push with"
        );
        // Still listed, so the cockpit can say why rather than silently omitting it.
        let listed = write_credentials();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].problem().is_some());
    }

    #[test]
    fn a_credential_without_its_token_cannot_scope_a_fleet() {
        // Half-configured is the dangerous state: a description with no token would report the fleet
        // as ready to scope, and every box would come up unable to push.
        let (_lock, _home, _env) = fresh_home();
        set_write_credential("half", "", &["a/one".into()]).unwrap();
        assert!(
            !can_issue_write_tokens(),
            "a credential with no token is not a way to issue one"
        );
        set_credential_token("half", "skein-test-t").unwrap();
        assert!(can_issue_write_tokens());
        assert!(
            box_is_scoped("any-box"),
            "a stored token is enough to scope on, with no App at all"
        );
    }

    #[test]
    fn a_credential_id_cannot_write_its_token_outside_the_token_directory() {
        let (_lock, _home, _env) = fresh_home();
        for bad in ["../../evil", "has/slash", "Upper", "-lead", ""] {
            assert!(!valid_credential_id(bad), "{bad:?} was accepted as an id");
            assert!(
                set_credential_token(bad, "skein-test-t").is_err(),
                "{bad:?}"
            );
            assert!(set_write_credential(bad, "", &[]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_credential_cannot_claim_a_repository_that_is_not_one() {
        let (_lock, _home, _env) = fresh_home();
        assert!(set_write_credential("x", "", &["../../etc/shadow".into()]).is_err());
        assert!(set_write_credential("x", "", &["only-one-part".into()]).is_err());
    }

    #[test]
    fn forgetting_a_credential_takes_its_token_with_it() {
        // A token nothing points at is one nobody rotates, and it would still work.
        let (_lock, _home, _env) = fresh_home();
        set_write_credential("gone", "", &["a/one".into()]).unwrap();
        set_credential_token("gone", "skein-test-t").unwrap();
        remove_write_credential("gone").unwrap();
        assert!(write_credentials().is_empty());
        assert!(!credential_has_token("gone"), "the token file outlived it");
        assert!(credential_for("a/one").is_none());
    }

    /// **A credential list skein cannot read is never written over, and there is a lock now.**
    ///
    /// SKEIN-359, and the audit's original citation. `github-pats.json` says which stored token
    /// covers which repository; the tokens themselves sit in `github-pats/` keyed by the ids in it.
    /// The list was read with `read_to_string(..).ok()`, so a file that would not parse read as *no
    /// credentials*, and storing one credential then wrote that single entry over every other one.
    /// What is left behind is worse than an empty list: a live PAT on disk that no entry names, so
    /// nothing will ever use it again and nobody will be told it is there.
    ///
    /// The corruption is not invented: a zero-length file is what a crash between `write_atomic`'s
    /// write and its rename leaves on ext4, and zero bytes are unparseable JSON.
    ///
    /// Asserted on the bytes on disk, because the error is the nice half.
    #[test]
    fn a_credential_list_skein_cannot_read_is_never_written_over() {
        let (_lock, home, _env) = fresh_home();
        set_write_credential("alpha", "one", &["a/one".into()]).unwrap();
        set_write_credential("beta", "two", &["b/two".into()]).unwrap();
        set_credential_token("alpha", "skein-test-alpha").unwrap();
        let path = home.join("github-pats.json");

        for corrupt in [&b""[..], &b"[{\"id\":\"alpha\""[..]] {
            std::fs::write(&path, corrupt).unwrap();

            let why = set_write_credential("gamma", "three", &["c/three".into()])
                .expect_err("storing a credential over an unreadable list reported success");
            assert!(
                why.contains("cannot read") && why.contains("github-pats.json"),
                "the refusal has to name the file and say it could not be read: {why}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "the unreadable credential list was replaced by a store"
            );

            assert!(
                remove_write_credential("beta").is_err(),
                "forgetting one credential is not how the others are forgotten"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "the unreadable credential list was replaced by a removal"
            );
        }

        // And it recovers the moment the file parses again — the entries were never destroyed.
        std::fs::write(
            &path,
            b"[{\"id\":\"alpha\",\"label\":\"one\",\"repos\":[\"a/one\"]}]",
        )
        .unwrap();
        set_write_credential("gamma", "three", &["c/three".into()]).unwrap();
        let ids: Vec<String> = write_credentials().into_iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["alpha".to_string(), "gamma".to_string()]);
    }

    /// A GitHub that answers every request the same way, for as long as the test wants it.
    ///
    /// Repeated rather than one-shot on purpose: [`crate::github::get_json`] asks a dead connection
    /// again once, and a stub that served a single answer would make a retry look like a hang.
    fn stub_github(status: u16, body: &'static str) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: \
                         {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// **"Your token cannot push here" and "GitHub refused to tell me" are told apart by the HTTP
    /// status**, which is the whole reason this module lost its own curl client.
    ///
    /// The one it had could not see a status at all: it parsed the body and called the answer an
    /// error only when it carried a `message` and neither a `token` nor an `id`. A 403 that carries
    /// an `id` — GitHub's refusals routinely do — therefore came back as SUCCESS, and
    /// [`check_token`] read a body with no `permissions` in it as a token that has lost its push
    /// rights. That answer is acted on: `refresh_tokens` drops a credential a box is using, and the
    /// health report tells its owner their PAT expired, on the strength of a refusal skein never
    /// read.
    ///
    /// Both directions are pinned here, because a status that is read but read wrongly is the same
    /// defect: a 401 saying "Bad credentials" IS a checked answer of no, and must stay `Ok(false)`
    /// rather than becoming a network complaint nobody can act on.
    ///
    /// The concrete change that breaks the first half: dropping `-w "\n%{http_code}"` from
    /// [`crate::github`]'s curl arguments, or deciding 2xx-ness from the body again.
    #[test]
    fn a_refusal_github_puts_a_status_on_is_not_read_as_a_token_without_push() {
        let _g = crate::testutil::env_lock();
        let _hold = crate::github::HoldClear::new();

        let refused = stub_github(
            403,
            r#"{"message":"Must have admin rights to Repository.","id":9}"#,
        );
        std::env::set_var("SKEIN_GITHUB_API", &refused);
        let answered = check_token(&Secret::new("skein-test-write-token"), "acme/thing");
        std::env::remove_var("SKEIN_GITHUB_API");
        let why = answered.expect_err(
            "a 403 was reported as a live token that cannot push — the answer that discards a \
             working credential",
        );
        assert!(
            why.contains("403") && why.contains("Must have admin rights"),
            "a refusal has to arrive as GitHub's own sentence, or nobody can act on it: {why}"
        );

        let expired = stub_github(401, r#"{"message":"Bad credentials"}"#);
        std::env::set_var("SKEIN_GITHUB_API", &expired);
        let answered = check_token(&Secret::new("skein-test-write-token"), "acme/thing");
        std::env::remove_var("SKEIN_GITHUB_API");
        assert_eq!(
            answered,
            Ok(false),
            "an expired credential is a checked answer of no, not a failure to check"
        );

        let allowed = stub_github(200, r#"{"permissions":{"push":true}}"#);
        std::env::set_var("SKEIN_GITHUB_API", &allowed);
        let answered = check_token(&Secret::new("skein-test-write-token"), "acme/thing");
        std::env::remove_var("SKEIN_GITHUB_API");
        assert_eq!(answered, Ok(true), "a token that can push must read as one");
    }
}
