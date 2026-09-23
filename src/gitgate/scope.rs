//! Whether a box is scoped, which repository it writes, what credential it holds, and the
//! scoping state the health line reports.

use super::*;

// ───────────────────────────── the switch ─────────────────────────────

/// Where a box's own answer to "scope my GitHub credential?" is kept.
///
/// [`crate::fleet::box_declared`], which is host-only and not in the sandbox at all — not the box's
/// state directory, which is bound read-write into the box because its conversation lives there.
/// This file decides whether the box gets a token scoped to its own repository or the account-wide
/// one, so a box that could write it could hand itself the account: exactly the outcome `apiauth`
/// exists to prevent, reached with no API call at all.
const SCOPE_FLAG: &str = "git-scope";

/// The box's own declared scope, if it has one — `None` means it follows the fleet default.
///
/// Separate from [`box_is_scoped`] so the *inheritance* can be asserted apart from the answer: the
/// two failure directions are not equal, and "no override" has to be distinguishable from "override
/// says fleet" for a test to show that an abandoned file is not being read as one.
pub fn declared_scope(box_name: &str) -> Option<String> {
    match crate::fleet::declared_read(box_name, SCOPE_FLAG)
        .unwrap_or_default()
        .trim()
    {
        "repo" => Some("repo".into()),
        "fleet" => Some("fleet".into()),
        _ => None,
    }
}

/// Is this box's GitHub credential scoped to its own repository?
///
/// The per-box file wins over the fleet default when it holds one of the two words it may. Anything
/// else — an empty file, a hand-edit, a half-written write — falls back to the default rather than
/// guessing, because the two failure directions are not equal: guessing "fleet" hands a box the
/// account, and guessing "repo" costs it a push it can ask for.
pub fn box_is_scoped(box_name: &str) -> bool {
    // Nothing to issue with is nothing to scope with. With neither an App nor a stored PAT there is
    // no write token for a box's *own* repo either, so scoping here would not narrow a box's reach —
    // it would take pushing away from every box in the fleet at once, which is the one outcome this
    // must never produce. The default may therefore be on from the day it ships: until one of the
    // two exists it changes nothing at all, and the moment one does, boxes are scoped with no
    // second switch to remember.
    if !can_issue_write_tokens() {
        return false;
    }
    match crate::fleet::declared_read(box_name, SCOPE_FLAG)
        .unwrap_or_default()
        .trim()
    {
        "repo" => true,
        "fleet" => false,
        _ => crate::config::load_config().scope_git_to_repo,
    }
}

/// Set (or clear, with `None`) one box's override. Takes effect at the box's **next start**: the
/// credential is placed as the box comes up, and a running box already holds what it was given.
pub fn set_box_scope(box_name: &str, scope: Option<&str>) -> Result<(), String> {
    if !crate::util::valid_name(box_name) {
        return Err(format!("unusable box name {box_name:?}"));
    }
    let Some(scope) = scope else {
        // A missing file is inheritance, so removing it is how a box goes back to following the
        // fleet — not writing the fleet's current answer into it, which would freeze today's default.
        return crate::fleet::declared_clear(box_name, SCOPE_FLAG);
    };
    if scope != "repo" && scope != "fleet" {
        return Err(format!("unknown scope {scope:?}"));
    }
    crate::fleet::declared_write(box_name, SCOPE_FLAG, scope.as_bytes())
}

/// The GitHub repository a managed repo maps to, as `owner/name` — the one answer to that question.
///
/// `repo.source` settles it, because [`crate::repos::add_repo`] refuses anything that is not a
/// remote. The fallback through [`crate::repos::repo_origin_url`] is for entries that predate that
/// refusal: a repo registered from a local path has a filesystem path in `source`, which
/// [`slug_from_url`] rejects on purpose, and its remote survives on its mirror's `origin`.
///
/// That fallback is not a nicety. Being registered by path said nothing about whether a repo had a
/// GitHub remote — skein's own was registered that way and its origin is
/// `git@github.com:owner/name` — so reading only `source` called a perfectly ordinary GitHub repo
/// "not GitHub", and since the launcher unsets the account `GH_TOKEN` and covers the ssh-agent for
/// *every* scoped box, a repo that got no token of its own was left with no way to push at all.
///
/// `None` only for a repo with no GitHub identity anywhere — no URL, no origin — which genuinely has
/// nowhere to push.
pub fn repo_slug(repo: &crate::repos::Repo) -> Option<String> {
    slug_from_url(&repo.source).or_else(|| {
        crate::repos::repo_origin_url(repo)
            .as_deref()
            .and_then(slug_from_url)
    })
}

/// The repository a box may write, as `owner/name` — or empty when skein does not know one.
///
/// Empty is not "everything": [`crate::fleet::session_script`] passes it through to the launcher,
/// which places no own-repo token when it is empty, so an unknown repo is a box with no write
/// credential of its own. That is the right failure for a repo with no GitHub remote — see
/// [`repo_slug`] for why an older entry holding a local path is not the same thing.
pub fn box_repo_slug(box_name: &str) -> String {
    crate::repos::repo_for_box(box_name)
        .and_then(|r| repo_slug(&r))
        .unwrap_or_default()
}

/// What scoping is actually doing, as this module understands it.
///
/// A value rather than a rendered string, because three callers want the same answer in different
/// shapes: the health report as a check, `skein doctor` as a line, and the cockpit as a panel state.
/// Deriving it in each of them meant every caller reaching into `app_credentials`,
/// `write_credentials`, `credential_has_token` and the config to re-infer what this module already
/// knows — and drifting apart the first time a state was added. Adding one now is a variant here
/// and a match arm there.
#[derive(Debug, Clone, PartialEq)]
pub enum ScopeStatus {
    /// Not asked for. A correct state, not a fault.
    Off,
    /// Asked for, and nothing set up to serve it. Every fresh install, since the setting defaults
    /// on *because* it is inert until a credential exists — so this is "not yet", never "broken".
    NotConfigured,
    /// Asked for, something was configured, and it cannot issue a token. The only failure state.
    Unusable { why: String, refused: Vec<String> },
    /// In force. `app` is empty when only stored per-repo tokens are in use.
    Active { app: String, tokens: usize },
}

/// Which credential a box actually receives — the answer to "can boxes push at all".
///
/// Distinct from [`ScopeStatus`], which answers "is scoping in force". The two part company in the
/// state that matters most on a first run: scoping asked for, nothing configured to serve it, so
/// [`box_is_scoped`] fails open and a box keeps the fleet-wide credential — `NotConfigured` there,
/// `Account` or `None` here depending on whether anyone chose to seed one.
#[derive(Debug, Clone, PartialEq)]
pub enum BoxCredential {
    /// Nobody has chosen, so skein places no GitHub credential in a box at all — nothing of its
    /// own to read or push with. A real state since all three paths became opt-in. (What a box can
    /// reach over the network regardless is not this enum's subject — SKEIN-548.)
    None,
    /// This account's `gh` token, seeded fleet-wide: every box, everything it reaches, read and write.
    Account,
    /// Per-box scoped tokens. `app` is empty when only stored per-repo tokens are in use.
    Scoped { app: String, tokens: usize },
}

impl BoxCredential {
    /// One phrase naming the credential, for a checklist row or a doctor line. Empty for `None`, so
    /// a caller can treat "" as "nothing chosen" without matching.
    pub fn label(&self) -> String {
        match self {
            BoxCredential::None => String::new(),
            BoxCredential::Account => "this account's gh token".into(),
            BoxCredential::Scoped { app, tokens } => {
                let mut parts = Vec::new();
                if !app.is_empty() {
                    parts.push(format!("App {app}"));
                }
                match tokens {
                    0 => {}
                    1 => parts.push("1 repository token".into()),
                    n => parts.push(format!("{n} repository tokens")),
                }
                // An App whose id is blank is still an App — `app_credentials()` succeeded, so
                // something must be said rather than an empty string that reads as "nothing chosen".
                match parts.is_empty() {
                    true => "a GitHub App".into(),
                    false => parts.join(" · "),
                }
            }
        }
    }
}

/// What a box gets, without calling GitHub.
///
/// Answered through [`scope_status`] rather than from the config directly, because *every* state in
/// which scoping is not in force — switched off, nothing configured, configured and refused — leaves
/// a box holding the fleet-wide credential. `box_is_scoped` fails open, so there is exactly one
/// question worth asking ("is scoping actually serving this box") and one answer for every way it can
/// be no. Reading `scope_git_to_repo` here as well only looked more careful: `scope_status` returns
/// `Off` for it already, so the extra branch could not change an answer — checked by removing it and
/// watching nothing fail.
pub fn box_credential() -> BoxCredential {
    match scope_status() {
        ScopeStatus::Active { app, tokens } => BoxCredential::Scoped { app, tokens },
        // Not scoping, for whatever reason. The box holds whatever the fleet-wide answer is — which is
        // now a choice, and so can be nothing at all.
        _ => match crate::config::load_config().seed_gh_secret {
            // Seeding is switched on, so the intended answer is the account token. Whether a box
            // actually *has* one is a different question, and in-fleet the answer is no unless it
            // was seeded before the move: both halves of the seeding are the host's, so a skein
            // inside the sandbox cannot put one there.
            //
            // Reported as unseeded rather than as the account token, because this string is what
            // the first-run checklist reads as "boxes can push" — and a fleet told it can push,
            // which cannot, learns otherwise from a 403 inside a box some minutes later. The marker
            // is the evidence and it travels with the volume, so a fleet seeded on the host and
            // then moved in still reads `Account`, correctly.
            true if crate::repos::gh_secret_seeded().is_none() => BoxCredential::None,
            true => BoxCredential::Account,
            false => BoxCredential::None,
        },
    }
}

/// Diagnose scoping without calling GitHub.
///
/// Deliberately offline: this is polled by the health path, and a network round trip per poll would
/// make a slow morning look like a broken fleet. Proving a credential really mints is an explicit
/// act, not a background one.
pub fn scope_status() -> ScopeStatus {
    let config = crate::config::load_config();
    if !config.scope_git_to_repo {
        return ScopeStatus::Off;
    }
    let stored = write_credentials();
    let usable = stored
        .iter()
        .filter(|c| c.problem().is_none() && credential_has_token(&c.id))
        .count();
    let refused: Vec<String> = stored
        .iter()
        .filter_map(|c| c.problem().map(|w| format!("{}: {w}", c.id)))
        .collect();
    let app = app_credentials();

    if app.is_ok() || usable > 0 {
        return ScopeStatus::Active {
            app: match app.is_ok() {
                true => config.github_app_id.trim().to_string(),
                false => String::new(),
            },
            tokens: usable,
        };
    }
    // Nothing usable. Whether that is "not set up" or "broken" turns on whether anyone tried.
    let attempted = !config.github_app_id.trim().is_empty() || !stored.is_empty();
    match attempted {
        false => ScopeStatus::NotConfigured,
        true => ScopeStatus::Unusable {
            why: app.err().unwrap_or_else(|| "no usable credential".into()),
            refused,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitgate::testkit::*;

    /// A git repo at `dir` whose `origin` is `remote` (or none, when empty).
    fn clone_with_origin(dir: &std::path::Path, remote: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        if !remote.is_empty() {
            git(&["remote", "add", "origin", remote]);
        }
    }

    /// A `Repo` through serde, so the fields this test does not care about keep their real defaults
    /// rather than a second set maintained here.
    fn repo_at(id: &str, source: &str, work: &std::path::Path) -> crate::repos::Repo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "source": source,
            "work": work.to_string_lossy(),
            "store": "",
        }))
        .unwrap()
    }

    /// The slug is read from the repo's remote, in either spelling GitHub accepts.
    ///
    /// This used to pin a different bug: reading only `repo.source` called an adopted-in-place repo
    /// "not GitHub", so the launcher placed no token while *also* unsetting the account `GH_TOKEN`
    /// and covering the ssh-agent — leaving the repo nothing to push with at all. Adoption is gone
    /// and `source` is always a remote, so what is left to hold is that both URL forms resolve: an
    /// SSH remote is not a repository skein may decline to recognise.
    #[test]
    fn the_slug_comes_from_the_repos_remote_in_either_url_form() {
        for source in [
            "git@github.com:acme/skein.git",
            "https://github.com/acme/skein.git",
        ] {
            let home = crate::testutil::tempdir();
            let work = (home.as_ref() as &std::path::Path).join("code/skein");
            clone_with_origin(&work, source);
            let repo = repo_at("skein", source, &work);
            assert_eq!(
                repo_slug(&repo).as_deref(),
                Some("acme/skein"),
                "no slug means no own-repo write token, so the box is given no way to push: {source}"
            );
        }
    }

    #[test]
    fn a_repo_with_no_remote_anywhere_has_nowhere_to_push() {
        // The one case the old behaviour got right, and it must stay right: no URL and no origin is
        // genuinely no GitHub identity, so no token is the honest answer rather than a missing one.
        //
        // Pinned through `fresh_home`, because a repo with no source sends `repo_slug` down
        // `repos::mirror_path`, which resolves `config::skein_home` — refused rather than answered
        // in a test since SKEIN-626. It only ever passed because a neighbour in this process had
        // left `$SKEIN_HOME` set; alone it looked for the mirror under the owner's real `~/.skein`.
        let (_lock, home, _env) = fresh_home();
        let work = (home.as_ref() as &std::path::Path).join("code/scratch");
        clone_with_origin(&work, "");
        assert_eq!(
            repo_slug(&repo_at("scratch", &work.to_string_lossy(), &work)),
            None
        );
        // Nor does a non-GitHub origin invent one — there is no App installation to mint against.
        let other = (home.as_ref() as &std::path::Path).join("code/elsewhere");
        clone_with_origin(&other, "git@gitlab.com:a/b.git");
        assert_eq!(
            repo_slug(&repo_at("elsewhere", &other.to_string_lossy(), &other)),
            None
        );
    }

    #[test]
    fn a_url_added_repo_never_consults_the_clone() {
        // `source` wins, so a clone whose origin was re-pointed by hand cannot quietly move which
        // repository the fleet mints tokens for.
        let home = crate::testutil::tempdir();
        let work = (home.as_ref() as &std::path::Path).join("code/thing");
        clone_with_origin(&work, "git@github.com:someone-else/elsewhere.git");
        let repo = repo_at("thing", "git@github.com:acme/thing.git", &work);
        assert_eq!(repo_slug(&repo).as_deref(), Some("acme/thing"));
    }

    /// What a box actually holds, across the states a first run passes through.
    #[test]
    fn a_box_holds_what_was_chosen_and_nothing_when_nothing_was() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        // Bound after `home`, so the pin goes back before the directory it names is removed.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // A fresh fleet: scoping on (its default) and nothing to serve it. Boxes hold no credential
        // skein placed — the state that used to be invisible, because the account token was seeded by
        // default and so this always answered "the account token".
        let mut config = crate::config::load_config();
        config.scope_git_to_repo = true;
        config.seed_gh_secret = false;
        crate::config::save_config(&config).unwrap();
        assert_eq!(box_credential(), BoxCredential::None);
        assert_eq!(
            box_credential().label(),
            "",
            "nothing chosen has nothing to name"
        );

        // Choosing the account token is a choice like any other — but choosing it is no longer
        // enough on its own to *have* one. `sbx secret set -g` was the host's half of the seeding
        // and it is gone (§13a), so the marker it left is the only evidence a token is in front of
        // these boxes, and `box_credential` reads it before claiming `Account` (SKEIN-576). A
        // fleet that has one was seeded before the move and carried the marker across on its
        // volume; this stands in for that fleet, because the states below are about what the
        // *config* selects and they need a fleet that has a credential to select.
        std::fs::write(home.join("gh-secret-seeded"), "2026-01-01T00:00:00Z\n").unwrap();
        config.seed_gh_secret = true;
        crate::config::save_config(&config).unwrap();
        assert_eq!(box_credential(), BoxCredential::Account);

        // A stored token now serves scoping, so that is what a box gets — not the account token, which
        // `box-session.sh` drops at startup for a scoped box.
        set_write_credential("mine", "one repo", &["a/one".into()]).unwrap();
        set_credential_token("mine", "github_pat_XYZ").unwrap();
        assert_eq!(
            box_credential(),
            BoxCredential::Scoped {
                app: String::new(),
                tokens: 1
            }
        );
        assert_eq!(box_credential().label(), "1 repository token");

        // Scoping switched off means the launcher keeps the account token whatever else is set up.
        // Reporting the stored token here would name a credential no box receives — the mutation that
        // proves this line: answer `Scoped` for every non-active state and it is this assertion that
        // catches it.
        config.scope_git_to_repo = false;
        crate::config::save_config(&config).unwrap();
        assert_eq!(
            box_credential(),
            BoxCredential::Account,
            "an unscoped box holds the fleet-wide credential however many tokens exist"
        );

        // …and with nothing seeded either, an unscoped fleet has simply nothing.
        config.seed_gh_secret = false;
        crate::config::save_config(&config).unwrap();
        assert_eq!(box_credential(), BoxCredential::None);
    }

    #[test]
    fn a_fresh_install_reads_as_not_set_up_rather_than_broken() {
        // The distinction the whole enum exists for. `scope_git_to_repo` defaults ON, so without
        // it every new user's first screen carries a red banner about a feature they have never
        // heard of — which is exactly the permanent-failure bug the registry check was just fixed
        // for. Red is earned by *trying*, not by defaulting.
        let (_lock, _home, _env) = fresh_home();
        assert!(crate::config::load_config().scope_git_to_repo);
        assert_eq!(scope_status(), ScopeStatus::NotConfigured);
    }

    #[test]
    fn a_credential_that_was_configured_and_cannot_work_is_a_failure() {
        let (_lock, _home, _env) = fresh_home();
        // Stored, named a repo, and never given a token: someone tried and stopped half way.
        set_write_credential("half", "", &["a/one".into()]).unwrap();
        match scope_status() {
            ScopeStatus::Unusable { refused, .. } => {
                assert!(
                    refused.is_empty(),
                    "a half-finished credential is not a refused one"
                )
            }
            other => panic!("a configured-but-unusable fleet must report a failure: {other:?}"),
        }
    }

    #[test]
    fn a_stored_token_alone_is_enough_to_be_active_with_no_app_at_all() {
        let (_lock, _home, _env) = fresh_home();
        set_write_credential("solo", "", &["a/one".into()]).unwrap();
        set_credential_token("solo", "t").unwrap();
        assert_eq!(
            scope_status(),
            ScopeStatus::Active {
                app: String::new(),
                tokens: 1
            },
            "someone using only their own tokens is configured, not half-configured"
        );
    }

    #[test]
    fn scoping_switched_off_is_never_a_fault() {
        let (_lock, home, _env) = fresh_home();
        let mut config = crate::config::load_config();
        config.scope_git_to_repo = false;
        std::fs::write(
            home.join("config.json"),
            serde_json::to_string(&config).unwrap(),
        )
        .unwrap();
        assert_eq!(scope_status(), ScopeStatus::Off);
    }

    #[test]
    fn nothing_is_scoped_until_there_is_an_app_to_mint_with() {
        // The property that makes shipping this safe. `scope_git_to_repo` defaults ON, and if that
        // took effect before a GitHub App existed, every box would be scoped with no write token for
        // even its own repo — the whole fleet unable to push, all at once, from a default. Scoping
        // is therefore gated on being *able* to mint, not merely on being asked to.
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let previous = std::env::var_os("SKEIN_HOME");
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        assert!(
            crate::config::load_config().scope_git_to_repo,
            "the default is on, or this test proves nothing"
        );
        assert!(
            app_credentials().is_err(),
            "a fresh home has no App configured"
        );
        assert!(
            !box_is_scoped("any-box"),
            "a fleet with no App must behave exactly as it did before this existed"
        );
        // Even an explicit per-box `repo` cannot scope a box there is no token for.
        assert!(!box_is_scoped("asked-for-it"));

        match previous {
            Some(v) => std::env::set_var("SKEIN_HOME", v),
            None => std::env::remove_var("SKEIN_HOME"),
        }
    }

    /// "The account token" is a claim about a credential, and only the marker is evidence for it.
    ///
    /// Both halves of the seeding are the host's — `gh auth token` reads its login, `sbx secret set`
    /// writes its keyring — so a skein inside the sandbox cannot put one there. `box_credential`'s
    /// label is what the first-run checklist reads as "boxes can push", and a fleet told that when
    /// it cannot learns otherwise from a 403 inside a box, minutes later and three layers from the
    /// cause.
    ///
    /// This used to be a two-armed test: seeding on meant `Account` on a host and `None` in the
    /// fleet. With one deployment left (SKEIN-576) `seed_gh_secret` on its own is never evidence,
    /// so the surviving question is the one that was always the interesting one — **what makes it
    /// `Account` again**. The marker, which travels with the volume: a fleet seeded on the host
    /// before the move still has the secret in sbx's store, and still reads `Account`.
    ///
    /// **What would make this fail**: dropping the `gh_secret_seeded().is_none()` guard from
    /// `box_credential`. The first assertion would then read `Account` off the config alone —
    /// which is the label that told a fleet its boxes could push when they could not.
    #[test]
    fn a_box_is_not_told_it_holds_a_token_nothing_ever_seeded() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let mut cfg = crate::config::load_config();
        cfg.seed_gh_secret = true;
        cfg.scope_git_to_repo = false;
        crate::config::save_config(&cfg).unwrap();

        assert_eq!(
            box_credential(),
            BoxCredential::None,
            "seeding is switched on and nothing ever ran it, and the fleet claimed an account \
             token anyway — neither half of the seeding could have put one there"
        );
        assert_eq!(box_credential().label(), "", "a claim was made anyway");

        // Seeded before the move: the secret is in sbx's store and the marker came across with the
        // volume, so the answer is the account token — on the same config that answered `None`
        // above, which is what makes the marker the thing being read rather than the config.
        std::fs::write(home.join("gh-secret-seeded"), "2026-01-01T00:00:00Z\n").unwrap();
        assert_eq!(
            box_credential(),
            BoxCredential::Account,
            "a fleet seeded on the host before the move was told it had lost its credential"
        );

        std::env::remove_var("SKEIN_HOME");
    }
}
