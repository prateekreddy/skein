//! The per-repository write credentials an owner stores and the optional read PAT: storing,
//! removing, choosing one for a repo, and asking GitHub what a token may do.

use super::*;

// ───────────────────────────── stored fine-grained PATs ─────────────────────────────

/// A fine-grained PAT its owner minted by hand: for **one** repository, unless they chose to share it.
///
/// The alternative to the App, for someone who would rather not install one across their account at
/// all: a token they created themselves, scoped in GitHub's own UI to exactly the repositories they
/// chose. skein never sees anything wider, and cannot — the token *is* the scope.
///
/// **One repository per token, unless the person shares it on purpose — and sharing has a cost that
/// is stated before it is made.** A token that reaches several repositories hands every one of them
/// to whichever box receives it: the credential helper offers it only when git asks about the box's
/// own repo, but the helper runs *inside* the box as the same uid as the agent, so anything it can
/// read the agent can read. A helper routes; it cannot contain. So a box of `owner/a` holding a
/// token shared with `owner/b` can push to `owner/b`, and nothing skein does inside the box changes
/// that.
///
/// Until 2026-09-29 that was the whole rule: exactly one repository, refused otherwise. The owner
/// decided then (SKEIN-1231: "Allow sharing, with a warning") that a person may reuse one token for
/// several repositories, provided they do it deliberately and are told the cost first. What that
/// means in the code:
///
/// * **Sharing is the only way to a multi-repo entry.** It happens when a repo is added and the
///   person picks "use the token another repo has" — the add dialog says, before they confirm, that
///   the boxes of every repo sharing it can push to all of them — and GitHub must say the token can
///   push to the new repository before the share is saved ([`check_token`]). The entry is then
///   marked [`WriteCredential::shared`].
/// * **Anything else still covers exactly one.** [`set_write_credential`] refuses a list of several,
///   and an entry naming several that is not marked shared — a hand edit, or a file from before
///   sharing existed — is listed with its problem and never used.
/// * **Leaving a share takes only that repository's coverage.** Removing a repo, or storing a token
///   for one repo alone ([`store_repo_token`]), drops it from the shared entry; the token file stays
///   for as long as any repository is left on it.
///
/// The repository names here are a **claim**, not the enforcement. GitHub enforces what the token
/// can reach; this is how skein knows which repo to hand it to. Getting it wrong costs a token that
/// does not work, never one that works too well — which is why the share is checked against GitHub
/// rather than taken on trust.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteCredential {
    #[serde(default)]
    pub id: String,
    /// What its owner calls it, for the cockpit. Never used as a path.
    #[serde(default)]
    pub label: String,
    /// `owner/name`, each. One, unless [`WriteCredential::shared`] says the person chose more.
    #[serde(default)]
    pub repos: Vec<String>,
    /// The person shared this token with another repository on purpose, from the add dialog, having
    /// been told what that lets each repo's boxes do. The only thing that lets `repos` hold more
    /// than one. Absent in every file written before sharing existed, which reads as `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
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
        match (self.repos.len(), self.shared) {
            (0, _) => return Some("names no repository".into()),
            (1, _) | (_, true) => {}
            (n, false) => {
                return Some(format!(
                    "names {n} repositories without having been shared from the add dialog; a \
                     stored token covers one repository unless its owner shares it, because every \
                     box that gets it can write all {n}"
                ))
            }
        }
        match self.repos.iter().find(|r| !slug_is_nameable(r)) {
            Some(bad) => Some(format!("{bad:?} is not a repository")),
            None => None,
        }
    }

    /// The first repository this token covers — the only one unless it is shared — or empty if it
    /// is not usable.
    pub fn repo(&self) -> &str {
        match self.problem() {
            None => &self.repos[0],
            Some(_) => "",
        }
    }

    /// May this credential be handed to a box of `slug`? Only when it is usable and names it.
    pub fn covers(&self, slug: &str) -> bool {
        self.problem().is_none() && self.repos.iter().any(|r| same_repo(r, slug))
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
        // holds: the form refuses a multi-repo entry and only a share from the add dialog makes
        // one, but the file behind it can be hand-edited, and this is the last point before a token
        // is placed inside a box.
        if !c.covers(slug) {
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
        shared: false,
    };
    if next.repos.len() > 1 {
        return Err(format!(
            "this token names {} repositories; a token stored here covers exactly one. To use one \
             token for several, add each further repo with \"use the token another repo has\", \
             which says what sharing lets their boxes do before it is saved",
            next.repos.len()
        ));
    }
    if let Some(why) = next.problem() {
        return Err(format!("this token {why}"));
    }
    crate::util::update_json(&credentials_path(), |all: &mut Vec<WriteCredential>| {
        // Writing over a shared entry would take the token away from every other repo on it, with
        // nothing on their cards to say so (SKEIN-1231).
        if let Some(sharing) = all.iter().find(|c| c.id == id && c.repos.len() > 1) {
            return Err(format!(
                "{id} is the token {} share, and replacing it here would take it from all but one \
                 of them. Replace it for all of them from any of their cards, or use a token for \
                 one repo alone from that repo's card",
                sharing.repos.join(", ")
            ));
        }
        all.retain(|c| c.id != id);
        all.push(next);
        Ok(())
    })
}

/// Store `token` as `slug`'s own, covering `slug` alone — the add dialog's "paste a new token", and
/// a repo card's "use a token for this repo alone" (SKEIN-1231). Returns the id it is filed under.
///
/// **If `slug` was sharing a token, it stops**: it is dropped from that entry, which keeps its token
/// file for the repositories still on it. Left there, the shared entry — earlier in the file —
/// would go on answering [`credential_for`] for `slug`, and the token just stored would never be
/// used.
///
/// The id is `slug`'s own ([`repo_credential_id`]), unless a shared entry already has that id — the repo that first shared its token keeps nothing of the
/// share when it leaves — in which case it gets the first free `<id>-<n>`.
///
/// The list is written before the token, the order [`remove_write_credential`] argues for: a
/// failure between the two leaves an entry the cockpit shows as "no token stored", never a live
/// token that no entry names.
pub fn store_repo_token(slug: &str, token: &str) -> Result<String, String> {
    if !slug_is_nameable(slug) {
        return Err(format!("{slug:?} is not a repository"));
    }
    if token.trim().is_empty() {
        return Err("no token was given".into());
    }
    let own = repo_credential_id(slug);
    let mut emptied = Vec::new();
    let id = crate::util::update_json(&credentials_path(), |all: &mut Vec<WriteCredential>| {
        emptied = leave_shares(all, slug);
        let taken = |id: &str| {
            all.iter()
                .any(|c| c.id == id && !(c.repos.len() == 1 && same_repo(&c.repos[0], slug)))
        };
        let id = match taken(&own) {
            false => own.clone(),
            true => (2..)
                .map(|n| {
                    let base: String = own.chars().take(60).collect();
                    format!("{base}-{n}")
                })
                .find(|id| !taken(id))
                .unwrap_or_default(),
        };
        all.retain(|c| c.id != id && !(c.repos.len() == 1 && same_repo(&c.repos[0], slug)));
        all.push(WriteCredential {
            id: id.clone(),
            label: slug.to_string(),
            repos: vec![slug.to_string()],
            shared: false,
        });
        Ok(id)
    })?;
    forget_tokens(&emptied);
    set_credential_token(&id, token)?;
    Ok(id)
}

/// Take `slug` off every shared entry in `all`. An entry left with one repository is no longer
/// shared; one left with none — only a hand-edited file can get there — is removed, and its id is
/// returned so its token file can go too, after the list is written.
fn leave_shares(all: &mut Vec<WriteCredential>, slug: &str) -> Vec<String> {
    for c in all.iter_mut().filter(|c| c.repos.len() > 1) {
        c.repos.retain(|r| !same_repo(r, slug));
        c.shared = c.repos.len() > 1;
    }
    let emptied: Vec<String> = all
        .iter()
        .filter(|c| c.repos.is_empty())
        .map(|c| c.id.clone())
        .collect();
    all.retain(|c| !c.repos.is_empty());
    emptied
}

/// Forget the token files of entries [`leave_shares`] removed. Best effort, and after the list is
/// written, for the reason [`remove_write_credential`] gives.
fn forget_tokens(ids: &[String]) {
    for id in ids {
        let _ = set_credential_token(id, "");
    }
}

/// The id a repository's own token is stored under. It is the rule the cockpit's `credId` applied
/// before the host took over choosing ids (SKEIN-1231), so every id already on disk was made by it,
/// and re-storing a repo's token finds and replaces the entry it wrote. Lowercased; each run of
/// anything but `a-z`, `0-9` and `-` becomes one `-`; dashes trimmed from both ends; at most 64
/// characters.
pub fn repo_credential_id(slug: &str) -> String {
    let mut id = String::new();
    let mut run = false;
    for c in slug.to_lowercase().chars() {
        match c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            true => {
                id.push(c);
                run = false;
            }
            false if !run => {
                id.push('-');
                run = true;
            }
            false => {}
        }
    }
    id.trim_matches('-').chars().take(64).collect()
}

/// The token of stored credential `id`, if GitHub says it can push to `slug` — the check a share
/// must pass before it is saved (SKEIN-1231). The token comes back so the clone that adds the repo
/// can use it, with the words that name it — "the token owner/a and owner/b use"; nothing is
/// written here.
///
/// The credential is named by the repositories it covers, never by any part of its token, in the
/// refusals as on the page.
pub fn shareable_token(id: &str, slug: &str) -> Result<(Secret, String), String> {
    let Some(c) = write_credentials().into_iter().find(|c| c.id == id) else {
        return Err(format!(
            "there is no stored token called {id:?} any more. Paste a token for {slug} instead"
        ));
    };
    let whose = format!("the token {} uses", c.repos.join(" and "));
    if let Some(why) = c.problem() {
        return Err(format!("{whose} cannot be shared: it {why}"));
    }
    let Some(token) = crate::secret::read(&credential_token_path(&c.id))
        .ok()
        .flatten()
    else {
        return Err(format!(
            "{whose} has no token stored any more, so there is nothing to share. Paste a token for \
             {slug} instead"
        ));
    };
    match check_token(&token, slug) {
        Ok(true) => Ok((token, whose)),
        Ok(false) => Err(format!(
            "{whose} cannot push to {slug}: GitHub says it has expired, or it was not granted \
             {slug}. Nothing was saved. Paste a new token for {slug} instead, or add {slug} to \
             that token's repositories on GitHub and try again"
        )),
        Err(e) => Err(format!(
            "could not ask GitHub whether {whose} can push to {slug}, so it was not shared: {e}"
        )),
    }
}

/// Add `slug` to the coverage of stored credential `id` and mark it shared — the save half of a
/// share, run only after [`shareable_token`] has passed and the repo's clone has worked with it. One
/// entry and one token file, not a copy, so replacing the token later reaches every repo on it.
pub fn share_credential(id: &str, slug: &str) -> Result<(), String> {
    if !slug_is_nameable(slug) {
        return Err(format!("{slug:?} is not a repository"));
    }
    crate::util::update_json(&credentials_path(), |all: &mut Vec<WriteCredential>| {
        let Some(c) = all.iter_mut().find(|c| c.id == id) else {
            return Err(format!("there is no stored token called {id:?} any more"));
        };
        if !c.repos.iter().any(|r| same_repo(r, slug)) {
            c.repos.push(slug.to_string());
        }
        c.shared = c.repos.len() > 1;
        Ok(())
    })
}

/// A repository skein no longer manages leaves every token it was sharing — only its own name is
/// taken off; the token stays for the repositories still on it (SKEIN-1231). A token `slug` holds
/// alone is left as it always was: removing a repo does not forget its token.
pub fn release_shared_coverage(slug: &str) -> Result<(), String> {
    let emptied =
        crate::util::update_json(&credentials_path(), |all: &mut Vec<WriteCredential>| {
            Ok(leave_shares(all, slug))
        })?;
    forget_tokens(&emptied);
    Ok(())
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
    fn a_token_pasted_for_more_than_one_repo_is_refused_outright() {
        // What stays forbidden after sharing was allowed (SKEIN-1231, 2026-09-29): a token STORED
        // for several repositories at once. A token covering three repos hands all three to
        // whichever box receives it — the helper offers it for one, but the helper runs inside the
        // box as the agent's own uid, so anything it can read the agent can read. The one way to a
        // multi-repo entry is a share from the add dialog, which states that cost first.
        //
        // What fails it: dropping the `repos.len() > 1` refusal in `set_write_credential`. The entry
        // is then still refused, by `problem()`, but in words that never tell the person sharing
        // exists — the first assertion, on the refusal's wording, is the one that fails.
        let (_lock, _home, _env) = fresh_home();
        let why = set_write_credential("three", "", &["a/one".into(), "a/two".into()]).unwrap_err();
        assert!(
            why.contains("covers exactly one") && why.contains("use the token another repo has"),
            "{why}"
        );
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
        // An entry naming several repositories is used only when it is marked `shared`, which only
        // the add dialog's share writes (SKEIN-1231) — so an unmarked one, a hand edit or a file
        // from before sharing existed, is still refused. What fails it: `problem()` accepting any
        // length again, or ignoring the marker.
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
            any_user_pat().is_none(),
            "nor lent out as the person's own PAT"
        );
        assert!(
            !can_issue_write_tokens(),
            "and it must not count as a way to issue tokens, or boxes scope with nothing to push with"
        );
        // Still listed, so the cockpit can say why rather than silently omitting it.
        let listed = write_credentials();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].problem().is_some());
    }

    /// **A token shared on purpose covers every repository on it** (SKEIN-1231), from the one file
    /// — the owner's "Allow sharing, with a warning". The same entry as the test above, marked.
    /// What fails it: `covers` still reading only the first repository, or `problem()` refusing a
    /// marked entry — `credential_for("a/two")` then answers nothing.
    #[test]
    fn a_token_shared_on_purpose_is_used_for_every_repo_it_covers() {
        let (_lock, home, _env) = fresh_home();
        let id = store_repo_token("a/one", "skein-test-shared").unwrap();
        share_credential(&id, "a/two").unwrap();

        for slug in ["a/one", "A/Two"] {
            let (c, token) = credential_for(slug)
                .unwrap_or_else(|| panic!("the shared token does not reach {slug}"));
            assert_eq!(
                (c.id.as_str(), token.expose()),
                ("a-one", "skein-test-shared"),
                "{slug}"
            );
        }
        assert!(can_issue_write_tokens() && any_user_pat().is_some());
        let listed = write_credentials();
        assert_eq!(
            (
                listed.len(),
                listed[0].shared,
                std::fs::read_dir(home.join("github-pats")).unwrap().count()
            ),
            (1, true, 1),
            "a share must be one entry and one token file, marked as shared — not a copy"
        );
        assert!(set_write_credential("x", "", &["../a".into()]).is_err());
    }

    /// **A repo leaving a share takes its name off and nothing else** (SKEIN-1231): the other repos
    /// keep the token, and the file is still there. What fails it: `leave_shares` dropping the whole
    /// entry (or the removal forgetting its token) — `credential_for("a/one")` then answers nothing.
    #[test]
    fn a_repo_leaving_a_share_keeps_the_token_for_the_others() {
        let (_lock, home, _env) = fresh_home();
        let id = store_repo_token("a/one", "skein-test-shared").unwrap();
        share_credential(&id, "a/two").unwrap();
        share_credential(&id, "a/three").unwrap();

        release_shared_coverage("a/two").unwrap();
        assert!(
            credential_for("a/two").is_none(),
            "a/two left and still has the token"
        );
        for slug in ["a/one", "a/three"] {
            let (_, token) = credential_for(slug)
                .unwrap_or_else(|| panic!("{slug} lost the token when a/two left"));
            assert_eq!(token.expose(), "skein-test-shared");
        }
        release_shared_coverage("a/three").unwrap();
        let (c, _) = credential_for("a/one").expect("the last repo on it lost the token");
        assert!(!c.shared, "one repository left is not a share any more");
        assert!(home.join("github-pats").join(&id).exists());

        // And a token a repo holds alone is left alone when it is removed, as it always was.
        release_shared_coverage("a/one").unwrap();
        assert!(
            credential_for("a/one").is_some(),
            "a token held alone was taken on removal"
        );
    }

    /// **"Use a token for this repo alone" leaves the share, and the others keep theirs**
    /// (SKEIN-1231), including when it is the repo the shared token was first stored for — whose own
    /// id the shared entry still has. What fails it: `store_repo_token` writing its entry under the
    /// shared entry's id (dropping the `taken` check) — the shared entry is then replaced, and
    /// `a/two` loses its token.
    #[test]
    fn a_repo_that_stops_sharing_gets_its_own_token_and_the_others_keep_theirs() {
        let (_lock, _home, _env) = fresh_home();
        let shared = store_repo_token("a/one", "skein-test-shared").unwrap();
        share_credential(&shared, "a/two").unwrap();

        let own = store_repo_token("a/one", "skein-test-one-alone").unwrap();
        assert_ne!(
            own, shared,
            "a/one's own token was filed under the shared entry's id"
        );
        let (c, token) = credential_for("a/one").unwrap();
        assert_eq!(
            (c.id.as_str(), token.expose()),
            (own.as_str(), "skein-test-one-alone")
        );
        let (c, token) = credential_for("a/two").expect("a/two lost the shared token");
        assert_eq!(
            (c.id.as_str(), token.expose()),
            (shared.as_str(), "skein-test-shared")
        );
        assert!(!c.shared);

        // Writing over a shared entry through the card's ordinary route is refused, not obeyed.
        share_credential(&shared, "a/three").unwrap();
        let why = set_write_credential(&shared, "", &["a/two".into()]).unwrap_err();
        assert!(why.contains("a/two, a/three"), "{why}");
        assert!(
            credential_for("a/three").is_some(),
            "the refused write took a/three's token"
        );
    }

    /// **A share is checked against GitHub first** (SKEIN-1231): only a token GitHub says can push
    /// to the new repository comes back to be shared, and a refusal names the token by the repos
    /// that use it, never by its bytes. The GitHub is a local stub. What fails it: `shareable_token`
    /// returning the token without `check_token` — the push-false case then answers `Ok`.
    #[test]
    fn a_token_is_shared_only_when_github_says_it_can_push_to_the_new_repo() {
        let (_lock, _home, mut env) = fresh_home();
        let _hold = crate::github::HoldClear::new();
        let id = store_repo_token("a/one", "skein-test-shared").unwrap();

        env.set(
            "SKEIN_GITHUB_API",
            stub_github(200, r#"{"permissions":{"push":false}}"#),
        );
        let why = shareable_token(&id, "a/two")
            .err()
            .expect("a token that cannot push was offered for sharing");
        assert!(
            why.contains("the token a/one uses cannot push to a/two")
                && why.contains("Nothing was saved"),
            "{why}"
        );
        assert!(
            !why.contains("skein-test-shared"),
            "the refusal carries the token: {why}"
        );

        env.set(
            "SKEIN_GITHUB_API",
            stub_github(200, r#"{"permissions":{"push":true}}"#),
        );
        let (token, whose) =
            shareable_token(&id, "a/two").expect("a token that can push was refused");
        assert_eq!(
            (token.expose(), whose.as_str()),
            ("skein-test-shared", "the token a/one uses")
        );
        assert!(
            credential_for("a/two").is_none(),
            "the check saved the share by itself"
        );
    }

    /// **The id a repo's own token is stored under is the one the cockpit's `credId` gave it**, so
    /// the entries already on disk are found and replaced rather than landed behind, where
    /// `credential_for`'s first-match-wins would keep the old token in use. Each case is what that
    /// function (`repo.toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "")
    /// .slice(0, 64)`) returned for that input, run under node before it was deleted.
    #[test]
    fn a_repos_own_token_is_filed_under_the_id_the_card_uses() {
        for (slug, want) in [
            ("acme/thing", "acme-thing"),
            ("Acme/Thing.JS", "acme-thing-js"),
            ("a.-b/c_d", "a--b-c-d"),
            ("-lead/x", "lead-x"),
        ] {
            assert_eq!(repo_credential_id(slug), want, "{slug}");
        }
        assert_eq!(
            repo_credential_id(&format!("{}/x", "a".repeat(80))).len(),
            64
        );
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
