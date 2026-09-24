//! Whose GitHub login this runs as, and which repository the queue is about.
//!
//! Both questions have the same shape and the same trap: an answer GitHub gave is worth
//! remembering, and a refusal is not. [`what_github_said`] is that rule as a function, and
//! [`renamed_to`] and [`trunk_of`] are the two lookups written through it.

use super::*;

/// Where the token the host talks to GitHub with came from, in the order it is looked for.
///
/// The order is the point: skein offers three ways to give it GitHub access, and the review queue
/// used to require a fourth — `gh auth login` — because it was built out of the `gh` CLI and `gh`
/// only knows its own store. One credential the user chose should do every job it is capable of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhToken {
    /// `$GH_TOKEN` / `$GITHUB_TOKEN`.
    Environment,
    /// The read token stored in Settings. A user's own PAT, which is what a review queue needs: it
    /// answers "who are you" and can see the repositories its owner can.
    ReadToken,
    /// A per-repo write token, used because it is also a user's PAT and no read token was stored.
    /// Narrower than a read token — what it cannot see is reported as a blind spot rather than
    /// quietly missing from the queue.
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
}

/// The token and where it came from. **A credential that was found is resolved once per process;
/// the absence of one is not remembered.**
///
/// The asymmetry is the whole of it, and it was learned the way these things are. The memo used to
/// hold `(GhToken::None, None)` too, and nothing in production ever cleared it — every call site of
/// [`forget_host_token`] below is in a test, and there has never been one anywhere else. So a
/// server started before its user had a
/// token stayed that way: install, open the cockpit, read *"no GitHub token … add a read token in
/// Settings → GitHub & keys"*, add one, and the queue goes on saying the same sentence until
/// somebody thinks to restart the server. The first-run path, ending in a dead end that reads as
/// the feature being broken.
///
/// Re-looking costs two environment reads and two small file reads, which is nothing beside the
/// network call every caller is about to make. The one source that is *not* free is the `gh` CLI,
/// and [`look_for_a_credential`] says what happens to that one.
///
/// An App is deliberately absent from this list. An installation token authenticates an
/// installation, not a person, so it cannot answer "whose review is this waiting on" — the queue's
/// whole question. That limit is the App's, and saying so beats falling back to something that
/// half-works.
fn host_credential() -> (GhToken, Option<crate::secret::Secret>) {
    let mut slot = match GH_TOKEN.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    if slot.is_none() {
        *slot = look_for_a_credential();
    }
    // A fresh `Secret` per caller rather than the cached one, because [`crate::secret::Secret`] has
    // no `Clone` on purpose: every copy is another buffer to scrub, so a copy is made where somebody
    // can see it being made. The cached one stays here and is scrubbed when the process ends.
    match slot.as_ref() {
        Some((source, held)) => (*source, Some(crate::secret::Secret::new(held.expose()))),
        None => (GhToken::None, None),
    }
}

/// Every place a credential can come from, in the order they win, and `None` if there is none.
///
/// Separate from the memo above so that the memo can hold what this **found** and nothing else.
fn look_for_a_credential() -> Option<(GhToken, crate::secret::Secret)> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Some((
                    GhToken::Environment,
                    crate::secret::Secret::new(value.trim()),
                ));
            }
        }
    }
    if let Some(pat) = crate::gitgate::read_pat() {
        return Some((GhToken::ReadToken, pat));
    }
    // Any write PAT they stored. It belongs to a person, so it can say who that person is —
    // which is the whole of what this needs.
    if let Some(pat) = crate::gitgate::any_user_pat() {
        return Some((GhToken::WritePat, pat));
    }
    // Last, and last for a reason rather than by accident: `gh` keeps its token in the system
    // keyring on a modern Linux, so asking can unlock one — which is why skein's own startup
    // stopped asking once the fleet secret was seeded. Every source above costs nothing, so
    // this is reached only by a host that would otherwise have no credential at all.
    //
    // **The one source whose absence is written down**, because it is the one that costs
    // something: a subprocess, a 15-second ceiling, and possibly a keyring prompt. Re-running it
    // on every `host_token()` — seventeen call sites outside the tests, several of them per queue
    // refresh, per repo, every three minutes from the badge poll — would turn a host with no
    // credential from slow to unusable. So `gh` is asked once, and a host that is given a `gh`
    // login after startup still needs a restart. The route the cockpit actually sends a person to
    // — Settings → GitHub & keys, which writes a file this reads two lines up — does not, and that
    // is the one the first-run dead end was on.
    let mut asked = match GH_CLI_ASKED.lock() {
        Ok(asked) => asked,
        Err(poisoned) => poisoned.into_inner(),
    };
    if !*asked {
        *asked = true;
        if let Some(token) = crate::repos::gh_cli_token() {
            return Some((GhToken::GhCli, crate::secret::Secret::new(&token)));
        }
    }
    None
}

/// Which credential the host's GitHub calls are running on, for the places that report it.
pub fn host_token_source() -> GhToken {
    host_credential().0
}

/// The token itself, or the sentence to show instead of an empty queue.
///
/// Public because the workflow tick acts as you — a label, a merge, a deleted branch are all things
/// GitHub attributes to whoever's credential asked. There is deliberately no second, quieter
/// credential for automation: everything skein does on its own is done as you, and shows up in the
/// repository's history under your name where you can see it.
pub fn host_token() -> Result<crate::secret::Secret, String> {
    host_credential().1.ok_or_else(|| {
        "no GitHub token: the review queue reads pull requests as you, and nothing here names a \
         user. Any of these does it — `gh auth login` on the host, exporting GH_TOKEN, or a read \
         token in Settings → GitHub & keys. A GitHub App cannot: an installation token is not a \
         person."
            .to_string()
    })
}

/// **A per-process memo here holds only what GitHub actually said.**
///
/// The rule, and the reason it is a function rather than a comment. Two lookups below are
/// remembered for the life of the process because their answers change about once a year and the
/// queue is polled from a board: [`renamed_to`] and [`trunk_of`]. Both used to swallow a failure
/// into a sentinel — `None` for "not renamed", `""` for "trunk unknown" — and then cache the
/// sentinel exactly as they would cache an answer.
///
/// That is worse than it sounds, because of WHEN it happens. `crate::github::call` refuses every
/// request while the rate-limit hold is engaged, without asking GitHub at all, and the hold is
/// engaged by the GraphQL search that runs *before* both of these inside [`queue_within`]. So one
/// rate-limited refresh does not fail these lookups: it fills them, permanently, with answers
/// nobody was given. The limit lifts, the queue comes back, and skein goes on believing a thing it
/// was never told until somebody restarts it. Nothing logs, nothing shows a blind spot, and there
/// is nothing for a person to clear because nothing says it is there. Found three times — the
/// merge train dead until a restart (SKEIN-238), a renamed repo reading as an empty queue
/// (SKEIN-281), and once avoided on purpose in `crate::prwork::facts_of`, where an unknown trunk
/// is `None` rather than "not the trunk" so that blindness cannot become a stop.
///
/// So: `ask` returns `Ok` only when GitHub answered. An `Err` — a refusal, a hold, a missing
/// token, a body that did not carry the field — is returned to the caller and **not written down**,
/// so the next refresh asks again. During a hold that retry costs nothing: it is refused before it
/// is spent, in exactly the condition that produces it.
///
/// Anything else remembered per process from a GitHub answer belongs here too. Reading a local
/// credential does not ([`host_credential`]) — no rate limit can manufacture "no token at all" —
/// but it keeps the same rule for the same reason: only a credential that was **found** is written
/// down, because a miss there is a person who has not finished setting skein up yet.
fn what_github_said<T: Clone>(
    memo: &std::sync::Mutex<std::collections::BTreeMap<String, T>>,
    slug: &str,
    ask: impl FnOnce() -> Result<T, String>,
) -> Option<T> {
    // Poison-tolerant like the hold in `crate::github`: the value is a cached answer, and there is
    // no invariant a panicking caller could have left half-written.
    let mut seen = match memo.lock() {
        Ok(seen) => seen,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(known) = seen.get(slug) {
        return Some(known.clone());
    }
    let answer = ask().ok()?;
    seen.insert(slug.to_string(), answer.clone());
    Some(answer)
}

/// The repository's current name when it differs from the one skein holds, else `None`.
///
/// Remembered per process like the token beside it: this is a REST round trip and the queue is
/// polled from the board, so asking per refresh would spend a call on an answer that changes about
/// once a year. Only through [`remembered`], so the thing written down is always a name GitHub
/// gave — **"GitHub says it is still called this" is an answer and is cached; "GitHub would not
/// tell me" is not.** The two used to be the same `None`, and on the one repo that had the queue
/// switched on for it — renamed, with the old slug still in the registry — a single refused
/// lookup meant the search kept asking `repo:<the old name>` and the queue read empty until a
/// restart. GitHub's *search* does not follow a rename the way its REST redirect does, which is
/// what `crate::github::canonical_repo` exists to read.
pub(super) fn renamed_to(slug: &str) -> Option<String> {
    what_github_said(&RENAMES, slug, || {
        let token = host_token()?;
        let now = crate::github::canonical_repo(slug, &token)?;
        // The name GitHub gave, and `None` when that is the name skein already holds. This `None`
        // is an ANSWER — it is inside the `Ok`, so it is remembered.
        Ok(Some(now).filter(|now| !now.eq_ignore_ascii_case(slug)))
    })
    .flatten()
}

/// Write a rename down — but only under a name skein can act on.
///
/// [`renamed_to`] hands back whatever `full_name` the API answered with, and until this existed
/// that value went straight into `repos.json`. [`crate::repos::follow_rename`]'s own guard is "two
/// non-empty `/`-separated parts", which `acme/space one` and `-flag/x` both pass. The write is a
/// substring replacement into `Repo::source`, so what lands is a URL no reader in this crate can
/// parse: [`crate::gitgate::repo_slug`] answers `None` from then on, and every queue over that
/// repository reports *"this repo has no GitHub remote"* — naming nothing about the rename that
/// caused it. It is on disk, so a restart does not clear it, and `repoint_mirror` has put the same
/// unusable URL on the mirror's `origin` beside it.
///
/// **The rule is SKEIN-641's, unchanged**: [`crate::gitgate::slug_from_path`] round-tripped, the
/// producer's own rather than a second alphabet beside it. Round-tripped rather than merely
/// accepted, because the invariant a write needs is that what goes in comes back out —
/// `acme/thing.git` is nameable and reads back as `acme/thing`.
///
/// It lives here and not in `repos` because `repos` does not depend on `gitgate` and should not
/// learn GitHub's naming rules in order to do a write. `prq` already reaches both, and both callers
/// of [`renamed_to`] are in it, so this is the choke point where the untrusted answer becomes a
/// stored fact.
///
/// **A refused name is `Ok(false)` and is deliberately not reported from here.** The caller goes on
/// to search under that same name, where `prq::search::repo_qualifier` refuses it with a sentence
/// naming the name and saying why — so a second sentence at the write would put two blind spots on
/// screen for one fact. That is SKEIN-258's damage, which this module has already been bitten by:
/// five lines reading as five broken things when they were one failure. The queue stays loud about
/// it; only the registry stays clean. The `bool` is what a caller would read the day that reporting
/// is wanted, so the distinction is in the type rather than lost in an `Ok(())`.
pub(super) fn record_rename(id: &str, was: &str, now: &str) -> Result<bool, String> {
    if crate::gitgate::slug_from_path(now).as_deref() != Some(now) {
        return Ok(false);
    }
    crate::repos::follow_rename(id, was, now).map(|()| true)
}

static RENAMES: std::sync::Mutex<std::collections::BTreeMap<String, Option<String>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Forget the resolved names, for tests and for a caller that has just been told one changed.
pub fn forget_renames() {
    if let Ok(mut seen) = RENAMES.lock() {
        seen.clear();
    }
}

/// The repository's default branch — `main`, `master`, whatever the repo says — for
/// [`Queue::trunk`].
///
/// Remembered per process for [`renamed_to`]'s reason: this is a REST round trip whose answer
/// changes about never, asked from a poll. Through [`what_github_said`], so only a branch GitHub named
/// is written down — a refusal is not, and the next refresh asks again (SKEIN-238: cached, one
/// rate-limited refresh made `base_is_trunk` false on every pull request in the repo, the merge
/// train's `base:trunk` claimed nothing, and the train was dead until a restart).
///
/// `""` for a repo whose trunk skein has not been told, which is [`Queue::trunk`]'s contract for
/// "not known" — and the empty string is never what gets remembered, because a body with no
/// `default_branch` is an `Err` here rather than an answer.
///
/// **`pub` for the merge a person presses** (SKEIN-338). [`crate::prwork::merge_by_hand`] needs the
/// trunk and must not refresh the queue to get it, and this is already the memoised answer the
/// queue itself uses — so both roads to a merge read the repository's default branch from the same
/// place and cannot disagree about what the trunk is.
pub fn trunk_of(slug: &str) -> String {
    what_github_said(&TRUNKS, slug, || {
        let token = host_token()?;
        let repo = crate::github::get_json(&crate::github::repo_path(slug), &token)?;
        repo.get("default_branch")
            .and_then(|b| b.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("GitHub did not say what {slug}'s default branch is"))
    })
    .unwrap_or_default()
}

static TRUNKS: std::sync::Mutex<std::collections::BTreeMap<String, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Forget the resolved trunks — for tests, like [`forget_renames`] above.
pub fn forget_trunks() {
    if let Ok(mut seen) = TRUNKS.lock() {
        seen.clear();
    }
}

/// The one resolution, remembered. A `Mutex<Option<_>>` rather than a `OnceLock` so a test can
/// forget it; the outer `Option` is "**have we found one**" — never "have we looked yet", which is
/// the distinction [`host_credential`] exists to keep.
static GH_TOKEN: std::sync::Mutex<Option<(GhToken, crate::secret::Secret)>> =
    std::sync::Mutex::new(None);

/// Whether the `gh` CLI has already been asked and had nothing — see [`look_for_a_credential`].
static GH_CLI_ASKED: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// Forget it, so the next call resolves again.
///
/// Public because `tests/review_queue.rs` is a separate crate and points skein at a different stub
/// per test: a token resolved once for the process would be the first test's, in every test.
///
/// Both memos, and the second one matters more than it looks: a test that leaves `GH_CLI_ASKED`
/// set makes the next test's stub `gh` on `PATH` unreachable, and a test whose credential is never
/// asked for passes for the wrong reason.
pub fn forget_host_token() {
    if let Ok(mut slot) = GH_TOKEN.lock() {
        *slot = None;
    }
    if let Ok(mut asked) = GH_CLI_ASKED.lock() {
        *asked = false;
    }
}

/// The GitHub repository a managed repo maps to, as `owner/name`.
///
/// One resolver, in [`crate::gitgate`], because the queue and the write token must agree on what repo
/// this is. They did not: this module fell back to the clone's `origin` while the token path read
/// only `repo.source`, so an adopted-in-place repo had a review queue *and* no way to push to the
/// repository that queue was listing.
pub fn repo_slug(repo: &Repo) -> Option<String> {
    crate::gitgate::repo_slug(repo)
}

/// The repository a WRITE should address, derived without refreshing the queue. (SKEIN-272)
///
/// The post path used to take its slug from `queue(repo, false)`, which is a full refresh past its
/// sixty-second cache: the viewer lookup, the rename check, and the five membership searches in one
/// request. So pressing "post comments" more than a minute after the last refresh inherited every
/// way a GitHub *read* can fail, and the `?` on that line turned "skein could not re-read your
/// queue" into "your review was not posted" — reported in the refresh's own words, which name a
/// repository and five membership searches nobody asked about. Reported live: the reviewer posted
/// by hand instead.
///
/// So the slug is derived from what a write actually needs. The remote is read from the checkout,
/// which is local and cannot fail over the network. The rename is followed because a POST to a
/// stale name is not redirected the way a GET is — but [`renamed_to`] is memoised per process and
/// answers `None` when GitHub cannot be asked, so a read that fails costs the stored name and never
/// the post. The only error left is the one that is genuinely about posting: there is nowhere to
/// post to.
pub fn slug_for_write(repo: &Repo) -> Result<String, String> {
    let stored = repo_slug(repo)
        .ok_or("this repo has no GitHub remote, so there is no pull request to post to")?;
    match renamed_to(&stored) {
        Some(now) => {
            // Best-effort, exactly as in [`queue_within`]: a rename skein cannot write down is one
            // it looks up again next time, which is not a reason to lose a review.
            let _ = record_rename(&repo.id, &stored, &now);
            Ok(now)
        }
        None => Ok(stored),
    }
}

// ───────────────────────────── viewer identity ─────────────────────────────

/// Your GitHub login, and the teams you belong to **when GitHub would say** — `None` when it
/// would not.
///
/// Teams are best-effort: `user/teams` needs `read:org`, which a perfectly good login may lack.
/// The two outcomes used to be the same empty list, and they are different facts (SKEIN-262):
/// *"you are in no teams"* means the team rules that exist have all been asked, and *"GitHub would
/// not tell me"* means a whole class of membership was never asked about at all. Only the second
/// makes the queue's open list incomplete, and only the second earns the `read:org` sentence — a
/// solo account was getting a permanent instruction to fix something that was not broken.
///
/// This is [`what_github_said`]'s rule in the shape a `Result` inside a `Result` would give: an
/// empty list is an ANSWER and is used as one; a refusal is not turned into one.
pub fn viewer() -> Result<(String, Option<Vec<String>>), String> {
    let token = host_token()?;
    let user = crate::github::get_json("/user", &token).map_err(|e| {
        // Which credential this ran on, and every way to change it. The queue is about *your* pull
        // requests, so it needs a token that names a user — and the answer used to be "run
        // `gh auth login`", as if that were the only one.
        format!(
            "GitHub could not identify you from {}: {e}. The review queue needs a token that names \
             a user — export GH_TOKEN, or add a read token in Settings → GitHub & keys (a PAT of \
             your own, which is what a fleet on the PAT path already has).",
            host_token_source().label()
        )
    })?;
    let login = user
        .get("login")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if login.is_empty() {
        return Err("GitHub returned no login for this token".into());
    }
    // Best-effort, and the failure is returned AS a failure. A body that is not an array is the
    // same non-answer as a 403: neither is GitHub telling us the list is empty.
    let teams = crate::github::get_json("/user/teams?per_page=100", &token)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|teams| {
            teams
                .iter()
                .filter_map(|t| {
                    let org = t.get("organization")?.get("login")?.as_str()?;
                    let slug = t.get("slug")?.as_str()?;
                    Some(format!("{org}/{slug}"))
                })
                .collect()
        });
    Ok((login, teams))
}

// ---------- how long each credential has left (SKEIN-928) ----------
//
// The fleet's GitHub credential has an expiry date and nothing in skein could see it. When it
// passes, every API call and every `git push` from every box fails at once, and it does not look
// like an expiry — it looks like an auth bug, which is a day spent before anybody thinks to check a
// date. Only the owner can renew it: it is regenerated on github.com and re-set from the host. So
// skein's whole job here is to see it coming early enough to be acted on, and to say what to do
// rather than only what is wrong.
//
// **The resolution lives here because the credential does.** `host_credential` above already knows
// which of four sources skein's GitHub calls are running on; asking that question a second time in
// `health` would be two answers to one question, which is the shape SKEIN-547 is about.

/// What GitHub says about how long one credential has left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Life {
    /// GitHub answered and named a date: the date as GitHub spelled it, and whole days from now to
    /// it — **negative once it is past**, which is a state a reader has to be able to say out loud.
    Expires { when: String, days: i64 },
    /// GitHub answered and named no expiry at all. A personal access token with no expiry date is
    /// a real and supported thing, and it is the one answer here that needs nothing from anybody.
    Endless,
    /// GitHub did not answer the question, and this is why.
    ///
    /// **Never folded into [`Life::Endless`]**, and that distinction is what the whole check turns
    /// on. Measured from a box on 2026-09-20 with `Authorization: token skein-test-garbage`:
    /// `HTTP/2 401`, and **no** `github-authentication-token-expiration` header at all. So "the
    /// header was not there" is what a dead credential looks like as well as what an endless one
    /// looks like, and a reader that takes the absence for good news reports the worst state in the
    /// fleet as the healthiest.
    Unanswered(String),
}

/// One credential skein holds, where it came from, and what GitHub said about it.
///
/// The recipe for replacing it is deliberately NOT here: it differs by source, it names the fleet
/// sandbox, and it is a sentence shown to a person — all of which belong to the reporting layer,
/// the way [`crate::health`] already owns the wording for `gitgate`'s scope states rather than
/// `gitgate` owning it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialLife {
    pub source: GhToken,
    /// What to call it in a sentence — `$GH_TOKEN`, or the label and repository its owner gave it.
    pub label: String,
    pub life: Life,
}

/// Whole days from `now` to the date GitHub put in `github-authentication-token-expiration`.
///
/// GitHub's spelling is `2026-10-15 13:19:49 UTC` (measured on this fleet 2026-09-15, SKEIN-928).
/// `None` for anything this cannot read, which the caller turns into [`Life::Unanswered`] rather
/// than into a pass — an expiry skein failed to parse is not an expiry skein has cleared.
///
/// **Floor division, not truncation.** A whole-day count that rounds toward zero answers `0` for
/// a credential that died four hours ago and `0` for one that dies in four hours; `div_euclid`
/// gives `-1` and `0`, and only one of those two sentences is true.
pub fn days_until(when: &str, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let text = when.trim();
    let text = text.strip_suffix("UTC").unwrap_or(text).trim();
    let at = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok()?;
    Some((at.and_utc() - now).num_seconds().div_euclid(86_400))
}

/// Ask GitHub about one credential. Separate from [`credential_lives`] so that the mapping from an
/// answer to a [`Life`] is written once rather than once per source.
fn life_of(token: &crate::secret::Secret, now: chrono::DateTime<chrono::Utc>) -> Life {
    match crate::github::token_expiry(token) {
        Err(why) => Life::Unanswered(why),
        Ok(None) => Life::Endless,
        Ok(Some(when)) => match days_until(&when, now) {
            Some(days) => Life::Expires { when, days },
            None => Life::Unanswered(format!(
                "GitHub named an expiry skein could not read: {when:?}"
            )),
        },
    }
}

/// Every GitHub credential skein holds, asked how long it has left.
///
/// **One network call per credential, so nothing polled may call this directly** — see
/// [`crate::health::token_expiry_health`], which is where the gate is.
///
/// The host credential comes first because it is the one skein's own calls run on, and it is
/// skipped when it resolved to a stored write PAT: that token is listed below under its own name,
/// and one credential reported twice under two names reads as two problems.
pub fn credential_lives(now: chrono::DateTime<chrono::Utc>) -> Vec<CredentialLife> {
    let mut out = Vec::new();
    let (source, held) = host_credential();
    if let Some(token) = held {
        if source != GhToken::WritePat {
            out.push(CredentialLife {
                source,
                label: source.label().to_string(),
                life: life_of(&token, now),
            });
        }
    }
    // The stored per-repo tokens — `~/.skein/github-pats/<id>`, described by `github-pats.json`.
    // Reached through `credential_for` because that is the accessor `gitgate` publishes and the
    // same one that decides which token a box is handed; deduplicated by the id it answers with, so
    // one token filed against two repositories is one row rather than two.
    let mut seen = std::collections::BTreeSet::new();
    for stored in crate::gitgate::write_credentials() {
        if stored.problem().is_some() {
            continue;
        }
        let Some((found, token)) = crate::gitgate::credential_for(stored.repo()) else {
            continue;
        };
        if !seen.insert(found.id.clone()) {
            continue;
        }
        let repo = found.repo().to_string();
        let label = match found.label.trim().is_empty() {
            true => repo,
            false => format!("{} ({repo})", found.label.trim()),
        };
        out.push(CredentialLife {
            source: GhToken::WritePat,
            label,
            life: life_of(&token, now),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prq::fixtures::{batched_repo, recording_github, routing_github, wired};

    /// **GitHub's expiry date, counted the way a person would count it** (SKEIN-928).
    ///
    /// The header's spelling is `2026-10-15 13:19:49 UTC`, measured on this fleet on 2026-09-15.
    /// Two things have to hold for the line built on it to be true: the format parses at all, and a
    /// date that is already past comes back negative.
    ///
    /// Counterfactuals, named before the assertions: swapping `div_euclid(86_400)` for a
    /// whole-day count that rounds toward zero makes `four hours past its date must not read as
    /// still alive` fail, because it answers `0` for both sides of the date. Dropping the
    /// `strip_suffix("UTC")` makes `parses GitHub's own spelling` fail, since `%S` does not eat a
    /// trailing zone name. Returning `Some(0)` instead of `None` for an unparseable value would
    /// make `an expiry skein cannot read is not an expiry skein has cleared` fail — and that one is
    /// the dangerous direction, since a `Some` is what the caller turns into a countdown.
    #[test]
    fn an_expiry_date_is_counted_forwards_and_backwards() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-20T13:19:49Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            days_until("2026-10-15 13:19:49 UTC", now),
            Some(25),
            "parses GitHub's own spelling, and counts whole days to it"
        );
        assert_eq!(
            days_until("2026-09-20 09:19:49 UTC", now),
            Some(-1),
            "four hours past its date must not read as still alive"
        );
        assert_eq!(
            days_until("2026-09-21 09:19:49 UTC", now),
            Some(0),
            "under a day left is zero days left, not one"
        );
        assert_eq!(
            days_until("whenever", now),
            None,
            "an expiry skein cannot read is not an expiry skein has cleared"
        );
    }

    /// **The GitHub credential is a `Secret` from the cache outwards, and prints as one.**
    ///
    /// `host_credential` memoises the resolved token for the life of the process, and it used to
    /// memoise a `String` — so the credential sat in a static, in the clear, and any `{:?}` of what
    /// `host_token` returned put it on somebody's terminal or in a log. Under
    /// [`crate::secret::Secret`] that same `{:?}` is `<secret>`, and the bytes are scrubbed when the
    /// process ends.
    ///
    /// The assertion fails the moment `host_token` goes back to returning a `String`: the formatted
    /// value becomes the token. And the second half is what stops that being a test of nothing —
    /// the credential is still *reachable*, so this is about how it prints, not about it being gone.
    #[test]
    fn the_github_credential_is_a_secret_and_prints_as_one() {
        let _env = crate::testutil::env_lock();
        forget_host_token();
        let mut env = crate::testutil::env_pins();
        env.set("GH_TOKEN", "skein-test-host-token");

        let held = host_token().expect("the environment names a credential");
        assert_eq!(
            format!("{held:?}"),
            "<secret>",
            "the GitHub token prints itself, so every `{{:?}}` on the path to GitHub is a leak"
        );
        assert_eq!(
            held.expose(),
            "skein-test-host-token",
            "the credential did not survive the cache, so the line above proves only that it is gone"
        );

        forget_host_token();
    }

    /// A tiny GitHub that records what it was handed. Returns `(base_url, seen)`.
    ///
    /// A real socket rather than a stubbed function, because the thing worth testing after this port
    /// is the wire: which credential reached the API, in which header. A stub would agree with
    /// whatever the client did.
    fn fake_github(body: &'static str) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                // Headers only: every request here either has no body or one this does not read,
                // and the connection is closed immediately after answering.
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(rest) = line.to_ascii_lowercase().strip_prefix("authorization:") {
                        recorder.lock().unwrap().push(rest.trim().to_string());
                    }
                    line.clear();
                }
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// A repository that was renamed fills its queue, and skein records the new name.
    ///
    /// The bug this is about produced no error anywhere. `acme/gadget-demo` became
    /// `acme/thing`; GitHub's REST redirects, so diffs and merges kept working, while its
    /// SEARCH matches a stale name against nothing and answers 200 with zero results. The queue
    /// collected nothing, recorded no blind spot, and rendered empty — with twenty-three pull
    /// requests waiting on a review behind it.
    #[test]
    fn a_renamed_repository_fills_its_queue_and_the_new_name_is_written_down() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        // Bound after `home`, so the pins go back before the directory they name is
        // removed — and from `Drop`, so they go back on the failing path too.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path)
            .set("GH_TOKEN", "skein-test-gho")
            .unset("GITHUB_TOKEN");
        let (base, asked) = routing_github();
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();

        // Built from JSON like the other fixtures here: `Repo` gains fields regularly and a
        // struct literal is the thing that stops compiling for a reason unrelated to this test.
        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/old-name.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        let answered = queue(&repo, true).expect("the queue answered");
        assert!(
            !answered.prs.is_empty(),
            "the queue is empty on a repository that was renamed — which is the failure it must \
             never be able to show by accident. Asked: {:?}",
            asked.lock().unwrap()
        );

        // And the new name is recorded, so everything else keyed on the slug follows it — the write
        // credential a box pushes with, the mirror's origin, the next queue refresh.
        let after = crate::repos::load_repos();
        let stored = &after.iter().find(|r| r.id == "demo").unwrap().source;
        assert!(
            stored.contains("acme/new-name"),
            "the rename was used and not written down, so every restart pays for it again: {stored}"
        );
        // The URL's shape survives — skein does not own it, and rebuilding one would change a
        // repo's transport along with its name.
        assert!(
            stored.starts_with("https://") && stored.ends_with(".git"),
            "the URL was rebuilt rather than edited: {stored}"
        );
        // The id is untouched. Box names, box roots and placement records are built from it.
        assert_eq!(after.iter().find(|r| r.id == "demo").unwrap().id, "demo");

        forget_host_token();
        forget_renames();
    }

    /// A GitHub that can be taken away and given back, answering a rename either way.
    ///
    /// Beside [`routing_github`] rather than folded into it: that one exists to prove a rename is
    /// followed at all, and this one exists to prove that FAILING to ask about one is not an
    /// answer. It also counts what was asked, because the other half of the rule — an answer, even
    /// "not renamed", is still remembered — is a claim about how often GitHub is paid.
    fn flaky_rename_github(
        down: std::sync::Arc<std::sync::Mutex<bool>>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = asked.clone();
        let mine = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut body).ok();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                recorder.lock().unwrap().push(path.clone());
                let out = *down.lock().unwrap();
                let (status, answer) = match path.as_str() {
                    // The one call that is taken away. 504 rather than a 403, because that is what
                    // a live fleet actually got the day this was written — and because the
                    // rule is about caching a failure, not about which failure it was.
                    "/repos/acme/old-name" if out => {
                        (504, r#"{"message":"Gateway Timeout"}"#.to_string())
                    }
                    "/user" => (200, r#"{"login":"me"}"#.to_string()),
                    p if p.starts_with("/user/teams") => (200, "[]".to_string()),
                    "/repos/acme/old-name" => (
                        301,
                        format!(
                            r#"{{"message":"Moved Permanently","url":"{mine}/repositories/42"}}"#
                        ),
                    ),
                    "/repositories/42" => (200, r#"{"full_name":"acme/new-name"}"#.to_string()),
                    // `default_branch` included, or `trunk_of` would rightly keep asking and the
                    // paid-once assertion below would be counting its retries.
                    "/repos/acme/new-name" => (
                        200,
                        r#"{"full_name":"acme/new-name","default_branch":"main"}"#.to_string(),
                    ),
                    "/graphql" => {
                        let hit =
                            body.contains("acme/new-name") && body.contains("review-requested");
                        (
                            200,
                            match hit {
                                true => r#"{"data":{"q0":{"nodes":[{"number":7,"title":"a pull request","url":"u","isDraft":false,"author":{"login":"someone"},"headRefOid":"abc","updatedAt":"2026-08-01T00:00:00Z","latestReviews":{"nodes":[]},"reviewRequests":{"nodes":[]}}]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string(),
                                false => r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string(),
                            },
                        )
                    }
                    _ => (200, "{}".to_string()),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (base, asked)
    }

    /// A rename lookup that failed is asked again — and one that answered is not.
    ///
    /// Both halves, in one test, because either alone is a bug. This is [`what_github_said`]'s
    /// rule asserted on the memo that had it wrong (SKEIN-281): `renamed_to` swallowed a refusal
    /// into the same `None` it uses for "GitHub says this repo is still called that", and cached
    /// it for the life of the process. The failure is not hypothetical — the one repository the
    /// owner has the review queue switched on for was renamed, its registry entry still carried
    /// the old slug, and GitHub 504'd on it the day this was written. GitHub's SEARCH does not
    /// follow a rename the way its REST redirect does, so the queue reads 200 with zero results
    /// and renders empty, with no error and no blind spot — until somebody restarts skein.
    ///
    /// The second half is what stops the fix being "cache nothing": `Ok(None)` — a repository
    /// GitHub says was NOT renamed — is an answer, and must still be paid for only once, or every
    /// poll on every fleet buys a call per repo to be told nothing changed.
    #[test]
    fn a_rename_lookup_that_failed_is_asked_again_and_one_that_answered_is_not() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        // Bound after `home`, so the pins go back before the directory they name is
        // removed — and from `Drop`, so they go back on the failing path too.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path)
            .set("GH_TOKEN", "skein-test-gho")
            .unset("GITHUB_TOKEN");
        let down = std::sync::Arc::new(std::sync::Mutex::new(true));
        let (base, asked) = flaky_rename_github(down.clone());
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();
        forget_trunks();

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/old-name.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        // The refresh that lands while the lookup is refused. An empty queue here is honest:
        // skein was not told the new name, so it searched the one it holds.
        let during = queue(&repo, true).expect("a blind queue still answers");
        assert!(
            during.prs.is_empty(),
            "the fixture did not reproduce the outage: {:?}",
            asked.lock().unwrap()
        );

        // GitHub comes back.
        *down.lock().unwrap() = false;

        let after = queue(&repo, true).expect("a healthy GitHub answers");
        assert!(
            !after.prs.is_empty(),
            "one refused lookup was remembered as \"this repo was not renamed\", so the search \
             keeps asking a name GitHub matches against nothing and the queue reads empty until \
             the process restarts. Asked: {:?}",
            asked.lock().unwrap()
        );
        // And the recovered name is written down, exactly as it is on the path that never failed.
        let stored = crate::repos::load_repos()
            .into_iter()
            .find(|r| r.id == "demo")
            .map(|r| r.source)
            .unwrap_or_default();
        assert!(
            stored.contains("acme/new-name"),
            "the rename was recovered and not written down: {stored}"
        );

        // The other half. `acme/new-name` answers "still called that" — an answer, and remembered:
        // a second refresh must not pay for it again.
        let repo = crate::repos::load_repos().remove(0);
        let _settled = queue(&repo, true).expect("the queue answered under the new name");
        let asks_of_new = || {
            asked
                .lock()
                .unwrap()
                .iter()
                .filter(|path| path.as_str() == "/repos/acme/new-name")
                .count()
        };
        let paid = asks_of_new();
        assert!(paid >= 1, "nothing ever asked GitHub about the new name");
        let _again = queue(&repo, true).expect("and again");
        assert_eq!(
            asks_of_new(),
            paid,
            "\"not renamed\" stopped being remembered, so every poll now buys a call per repo to \
             be told nothing changed"
        );

        forget_host_token();
        forget_renames();
        forget_trunks();
    }

    /// A rename to a name skein cannot act on does not become the name skein holds.
    ///
    /// SKEIN-641 closed the read: `search::repo_qualifier` will not build a `repo:` qualifier out
    /// of a slug `gitgate::slug_from_path` would not hand back unchanged, so a space can no longer
    /// rescope the queue's search. This is the write, which was still open. `renamed_to` adopts
    /// whatever `full_name` the REST answer carried, and `repos::follow_rename` only asked for two
    /// non-empty `/`-separated parts — so `acme/space one` was written into `Repo::source` by
    /// substring replacement, the mirror's `origin` was repointed at the same unusable URL, and
    /// from then on `gitgate::repo_slug` answered `None`. Every queue over the repository then read
    /// *"this repo has no GitHub remote"*, which names nothing about the rename, and it is on disk,
    /// so restarting does not clear it.
    ///
    /// Both halves are asserted, and the second is why this is not a one-line assertion. A guard
    /// that refused every rename would satisfy the registry assertion perfectly while emptying the
    /// queue of every renamed repository in the fleet, so the reader must still be told — and the
    /// telling comes from the search's refusal, naming the name GitHub gave.
    ///
    /// The concrete change that makes this fail is deleting the `slug_from_path` round-trip from
    /// [`record_rename`]: the unusable name is written and `repo_slug` stops resolving. The
    /// opposite mistake — a guard that refuses everything — reddens
    /// [`a_rename_lookup_that_failed_is_asked_again_and_one_that_answered_is_not`] above, which
    /// asserts that `acme/new-name` IS written down.
    #[test]
    fn a_rename_to_an_unusable_name_is_not_written_into_the_registry() {
        let _g = crate::testutil::env_lock();
        let _h = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        // Bound after `home`, so the pins go back before the directory they name is
        // removed — and from `Drop`, so they go back on the failing path too.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path)
            .set("GH_TOKEN", "skein-test-token")
            .unset("GITHUB_TOKEN");
        let base = renames_to_an_unusable_name_github();
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();
        forget_trunks();

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/old-name.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        let seen = queue(&repo, true).expect("a queue that cannot search still answers");

        // The registry still names a repository skein can act on.
        let after = crate::repos::load_repos().remove(0);
        assert_eq!(
            after.source, "https://github.com/acme/old-name.git",
            "GitHub's unusable answer was written into the registry"
        );
        assert_eq!(
            crate::gitgate::repo_slug(&after).as_deref(),
            Some("acme/old-name"),
            "the registry holds a name its own reader refuses, so every queue over this repo now \
             reports that it has no GitHub remote — and it is on disk, so a restart will not clear \
             it: {}",
            after.source
        );

        // And the reader is not left in the dark for the sake of a clean registry: the search's
        // own refusal names the name GitHub gave.
        assert!(
            seen.blind_spots
                .iter()
                .any(|b| b.contains("acme/space one")),
            "nothing on screen named the rename that stopped the queue: {:?}",
            seen.blind_spots
        );

        forget_host_token();
        forget_renames();
        forget_trunks();
    }

    /// A GitHub that renames a repository to a name skein cannot act on.
    ///
    /// `acme/space one` rather than an invented shape: `full_name` is a JSON string, nothing
    /// between it and `repos.json` inspected it, and it is the same value SKEIN-641 measured the
    /// search against — so the read guard and the write guard are proven on one name.
    fn renames_to_an_unusable_name_github() -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let mine = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    line.clear();
                }
                let (status, answer) = match path.as_str() {
                    "/user" => (200, r#"{"login":"me"}"#.to_string()),
                    p if p.starts_with("/user/teams") => (200, "[]".to_string()),
                    // The rename, read the way `canonical_repo` reads one: a 301 carrying the
                    // numeric id, which is the identifier that does not move.
                    "/repos/acme/old-name" => (
                        301,
                        format!(
                            r#"{{"message":"Moved Permanently","url":"{mine}/repositories/42"}}"#
                        ),
                    ),
                    "/repositories/42" => (200, r#"{"full_name":"acme/space one"}"#.to_string()),
                    _ => (200, "{}".to_string()),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        base
    }

    /// A GitHub whose repository answer carries a default branch, recording every path asked.
    ///
    /// Beside [`routing_github`] rather than folded into it: that one exists to tell a rename from
    /// an empty repository, and this one exists to count how often `/repos/<slug>` is paid for.
    fn trunk_github() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut length = 0usize;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                if length > 0 {
                    let mut body = vec![0u8; length];
                    reader.read_exact(&mut body).ok();
                }
                recorder.lock().unwrap().push(path.clone());
                let answer = match path.as_str() {
                    "/user" => r#"{"login":"me"}"#.to_string(),
                    p if p.starts_with("/user/teams") => "[]".to_string(),
                    "/repos/acme/trunky" => {
                        r#"{"full_name":"acme/trunky","default_branch":"main"}"#.to_string()
                    }
                    // The batched wire: every alias answers, or the miss reads as a blind spot.
                    "/graphql" => {
                        r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                            .to_string()
                    }
                    _ => "{}".to_string(),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), asked)
    }

    /// A refresh fills the repo's trunk, and pays for the lookup once per process.
    #[test]
    fn a_refresh_learns_the_trunk_once_and_remembers_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        // Bound after `home`, so the pins go back before the directory they name is
        // removed — and from `Drop`, so they go back on the failing path too.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path)
            .set("GH_TOKEN", "skein-test-gho")
            .unset("GITHUB_TOKEN");
        let (base, asked) = trunk_github();
        env.set("SKEIN_GITHUB_API", &base);
        forget_host_token();
        forget_renames();
        forget_trunks();

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "trunky",
            "source": "https://github.com/acme/trunky.git",
            "work": "",
            "store": "",
            "review_queue": true,
        }))
        .unwrap();
        crate::repos::save_repos(std::slice::from_ref(&repo)).unwrap();

        let first = queue(&repo, true).expect("the queue answered");
        assert_eq!(
            first.trunk, "main",
            "the refresh did not learn the repository's default branch"
        );
        let repo_asks = || {
            asked
                .lock()
                .unwrap()
                .iter()
                .filter(|path| path.as_str() == "/repos/acme/trunky")
                .count()
        };
        let after_first = repo_asks();
        assert!(after_first >= 1, "nothing ever asked GitHub for the repo");

        let second = queue(&repo, true).expect("the queue answered again");
        assert_eq!(second.trunk, "main", "the remembered trunk was dropped");
        assert_eq!(
            repo_asks(),
            after_first,
            "a second refresh paid for the trunk lookup again instead of remembering it"
        );

        forget_host_token();
        forget_renames();
        forget_trunks();
    }

    /// The host uses the credential you already gave it, and never asks for another.
    ///
    /// This replaces a test about `gh`'s keyring, because the keyring is no longer reachable from
    /// here: the queue talks to the API with a token skein holds. What survives is the property
    /// that mattered — one credential the user chose, doing every job it is capable of — and it is
    /// now asserted on the wire rather than on a subprocess's environment.
    ///
    /// **The host's own `gh` login counts as a credential the user already gave skein.** It did
    /// not, and the contradiction was visible in one `skein doctor`: `gh secret seeded` and
    /// `boxes push with this account's gh token` three lines above `github token none`, with every
    /// review queue answering 502. Skein was reading that login to put a credential in front of
    /// every box and refusing to read it to answer "who are you".
    ///
    /// Driven with a stub `gh` on PATH, because the property is that the CLI is ASKED — a test that
    /// injected the token would pass on a version that never ran anything.
    #[test]
    fn the_hosts_gh_login_is_the_last_credential_tried_and_it_is_tried() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        // Bound after the directory, so the pins go back before it is removed — and from `Drop`, so
        // `$PATH` goes back on the path where an assertion unwinds past the line that put it back.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home)
            .unset("GH_TOKEN")
            .unset("GITHUB_TOKEN");
        let (base, seen) = fake_github(r#"{"login":"me"}"#);
        env.set("SKEIN_GITHUB_API", &base);

        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("gh"),
            "#!/usr/bin/env bash\n[ \"$1 $2\" = \"auth token\" ] || exit 1\necho gho_from_the_cli\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let path = std::env::var("PATH").unwrap_or_default();
        env.set("PATH", format!("{}:{path}", bin.display()));

        // Nothing stored anywhere: the state a fleet is in when it has only ever been set up with
        // `gh auth login`, which is the commonest way there is.
        forget_host_token();
        assert_eq!(viewer().unwrap().0, "me");
        assert_eq!(
            host_token_source(),
            GhToken::GhCli,
            "the host has a `gh` login and the queue still reports no credential"
        );
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|h| h == "bearer gho_from_the_cli"),
            "the CLI's token never reached GitHub: {:?}",
            seen.lock().unwrap()
        );

        // And it is LAST. Asking `gh` can unlock a system keyring, so anything already stored has
        // to win — otherwise every board poll pays for a credential skein was already holding.
        crate::gitgate::set_read_pat("skein-test-pat-read").unwrap();
        forget_host_token();
        seen.lock().unwrap().clear();
        assert_eq!(viewer().unwrap().0, "me");
        assert_eq!(
            host_token_source(),
            GhToken::ReadToken,
            "the `gh` CLI was asked while a stored token was sitting right there"
        );

        crate::gitgate::set_read_pat("").unwrap();
        forget_host_token();
    }

    /// **A token added in Settings works without restarting the server** — the first-run path.
    ///
    /// Install, open the cockpit, and the queue says *"no GitHub token … add a read token in
    /// Settings → GitHub & keys"*. Do that. The memo in [`host_credential`] used to hold the
    /// failure as firmly as it holds a success, and nothing in production has ever called
    /// [`forget_host_token`] — every call site of it is in a test — so the queue went on printing
    /// that same sentence until somebody restarted skein. An onboarding dead end that reads as
    /// the feature being broken.
    ///
    /// Driven with a `gh` on `PATH` that has no login, because that is the host state this is
    /// about and because a real `gh` on the machine running the tests would otherwise answer.
    ///
    /// **What would make this fail:** putting the miss back in the memo — `slot.get_or_insert_with`
    /// returning `(GhToken::None, None)`. The second `host_token_source` then still says `None`
    /// with the token sitting in `$SKEIN_HOME/github-read-token`. And the second half fails the
    /// other way: dropping `GH_CLI_ASKED` makes the log three lines instead of one, which is a
    /// subprocess and a possible keyring prompt per GitHub call on a host with no credential.
    #[test]
    fn a_token_stored_after_the_first_look_is_found_without_a_restart() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        // Bound after the directory, so the pins go back before it is removed — and from `Drop`, so
        // `$PATH` goes back on the path where an assertion unwinds past the line that put it back.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home)
            .unset("GH_TOKEN")
            .unset("GITHUB_TOKEN");

        // A `gh` that is installed and logged out, which is what most hosts look like, recording
        // every time it is asked.
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = home.join("gh-asked");
        std::fs::write(
            bin.join("gh"),
            format!(
                "#!/usr/bin/env bash\necho asked >> {}\nexit 1\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let path = std::env::var("PATH").unwrap_or_default();
        env.set("PATH", format!("{}:{path}", bin.display()));

        forget_host_token();
        assert_eq!(
            host_token_source(),
            GhToken::None,
            "the fixture host already has a credential, so the rest of this proves nothing"
        );
        assert!(
            host_token().is_err(),
            "a token was resolved out of a host that has none"
        );

        // Asked once, however many times the queue asks for a credential: `gh auth token` is a
        // subprocess with a 15-second ceiling that can unlock a system keyring.
        let _ = host_token_source();
        let _ = host_token();
        assert_eq!(
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "`gh` is being re-asked on every call, which is a subprocess per GitHub request"
        );

        // The person now does what the cockpit told them to do. Nothing restarts, and nothing
        // in the server calls `forget_host_token` — that is the point.
        crate::gitgate::set_read_pat("skein-test-pat-read").unwrap();

        assert_eq!(
            host_token_source(),
            GhToken::ReadToken,
            "the token was added in Settings and the queue still says there is none"
        );
        assert_eq!(
            host_token().expect("no token after storing one").expose(),
            "skein-test-pat-read",
            "the source moved but the credential handed to GitHub did not"
        );

        crate::gitgate::set_read_pat("").unwrap();
        forget_host_token();
    }

    #[test]
    fn the_host_reads_github_with_the_credential_you_already_gave_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
        let (base, seen) = fake_github(r#"{"login":"me"}"#);
        env.set("SKEIN_GITHUB_API", &base);

        // A read token: the credential someone stores when they want cross-repo reads without an App.
        crate::gitgate::set_read_pat("skein-test-pat-read").unwrap();
        forget_host_token();
        assert_eq!(viewer().unwrap().0, "me");
        assert_eq!(host_token_source(), GhToken::ReadToken);
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|h| h == "bearer skein-test-pat-read"),
            "the token the user chose never reached GitHub: {:?}",
            seen.lock().unwrap()
        );

        // And with only a per-repo write token stored: it belongs to a person too, so it can say
        // who that person is. Nothing else is asked for.
        crate::gitgate::set_read_pat("").unwrap();
        crate::gitgate::set_write_credential("mine", "mine", &["me/repo".into()]).unwrap();
        crate::gitgate::set_credential_token("mine", "skein-test-pat-write").unwrap();
        forget_host_token();
        seen.lock().unwrap().clear();
        assert_eq!(viewer().unwrap().0, "me");
        assert_eq!(host_token_source(), GhToken::WritePat);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|h| h == "bearer skein-test-pat-write"));

        // The environment wins over both, for headless and CI.
        std::env::set_var("GH_TOKEN", "skein-test-gho-exported");
        forget_host_token();
        seen.lock().unwrap().clear();
        let _ = viewer();
        assert_eq!(host_token_source(), GhToken::Environment);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|h| h == "bearer skein-test-gho-exported"));

        // With nothing at all, the queue says what is missing — and says which credential cannot
        // cover it, because an App is the one path that genuinely cannot.
        std::env::remove_var("GH_TOKEN");
        let bare = crate::testutil::tempdir();
        env.set("SKEIN_HOME", bare.as_ref() as &std::path::Path);
        forget_host_token();
        let why = viewer().expect_err("no token, no queue");
        assert!(
            why.contains("GH_TOKEN") && why.contains("read token"),
            "{why}"
        );
        assert!(
            why.contains("App"),
            "the one path that cannot do this: {why}"
        );

        forget_host_token();
    }

    /// **A post must not inherit a read's failures** (SKEIN-272). The slug a write addresses used
    /// to come out of `queue(repo, false)` — a full refresh past its sixty-second cache, viewer
    /// lookup and five membership searches included — so a GitHub that would not answer a *read*
    /// made a *write* impossible, and said so in the refresh's own words. Reported live: a
    /// reviewer pressed "post comments" and was told five membership searches were missing.
    ///
    /// The remote is in the checkout. Nothing here needs GitHub to be up.
    #[test]
    fn the_repository_a_post_addresses_is_derived_without_a_refresh() {
        let (base, seen) = recording_github(None, None);
        let _env = wired(&base);
        forget_renames();

        let slug = slug_for_write(&batched_repo("acme/thing"))
            .expect("a GitHub that will not answer must not make a post impossible");

        assert_eq!(slug, "acme/thing");
        let asked = seen.lock().unwrap().clone();
        assert!(
            asked.iter().all(|r| !r.contains("/graphql")),
            "the post asked for a queue refresh: {asked:#?}"
        );
        assert!(
            asked.iter().all(|r| !r.contains("/user/teams")),
            "the post asked who the viewer's teams are: {asked:#?}"
        );
        forget_renames();
    }

    /// The one GitHub fact a write does need: a renamed repository. A POST is not redirected the
    /// way a GET is, so the canonical name is followed — memoised, and `None` when it cannot be
    /// asked, which is what makes the test above possible.
    #[test]
    fn a_post_addresses_the_repository_under_the_name_it_has_now() {
        let (base, _seen) = recording_github(Some(r#"{"full_name":"acme/renamed"}"#), None);
        let _env = wired(&base);
        forget_renames();

        let slug = slug_for_write(&batched_repo("acme/thing")).expect("the rename resolved");

        assert_eq!(
            slug, "acme/renamed",
            "a review would have been posted to a name the repository no longer has"
        );
        forget_renames();
    }
}
