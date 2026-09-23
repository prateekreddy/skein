//! What a box asks for and what it is given — a `Request`, a `Grant`, the access a box has to
//! a repository — and the names a request may carry, down to the repository slug a remote
//! spells.

use super::*;

/// How long an approved cross-repo grant lasts when its approver does not say "keep it".
///
/// Deliberately unlike [`crate::substrate`]'s `remember`, which is permanent. A package that a fleet
/// approved once should survive a rebuild; **write access to someone else's repository should not**.
/// The usual reason to grant it is a single change in a sibling repo, and an approval that outlives
/// that reason is one nobody remembers giving.
pub const DEFAULT_GRANT_HOURS: i64 = 24;

/// What a box may do with a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Its own repo: push freely.
    Write,
    /// A repo it holds a live cockpit grant for.
    Granted,
    /// Anything else: the read token serves, and this box is given nothing to push with.
    Read,
}

/// One box's ask for write access to a repository that is not its own.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: String,
    /// The box that asked. `box` is a Rust keyword, so the field is renamed rather than the JSON.
    #[serde(default, rename = "box")]
    pub box_name: String,
    /// `owner/name`, as [`slug_from_url`] normalises it.
    #[serde(default)]
    pub repo: String,
    /// Whatever the box said it was for. Free text, shown to the approver and never executed.
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub asked: String,
    /// `pending` → `granted` | `denied`. **As [`list`] returns it, this is the host's answer**, from
    /// [`decision_path`]; whatever the box's own file says here is thrown away (SKEIN-940).
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub decided: String,
}

impl Request {
    /// Why this request must not be acted on, or `None` if it may be.
    ///
    /// Returned as a reason rather than a bool for the same purpose as substrate's: a request the
    /// cockpit drops silently looks, to the box that filed it, exactly like one nobody got to.
    pub fn problem(&self) -> Option<String> {
        if !id_is_nameable(&self.id) {
            return Some(format!("unusable request id {:?}", self.id));
        }
        if !crate::util::valid_name(&self.box_name) {
            return Some(format!("unusable box name {:?}", self.box_name));
        }
        match slug_is_nameable(&self.repo) {
            true => None,
            false => Some(format!("{:?} is not a repository", self.repo)),
        }
    }

    pub fn is_pending(&self) -> bool {
        self.state == "pending"
    }
}

/// An approved cross-repo write, as recorded on the host.
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
    pub fn is_live(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        if self.expires.trim().is_empty() {
            return true;
        }
        chrono::DateTime::parse_from_rfc3339(self.expires.trim())
            .map(|e| e > now)
            .unwrap_or(false)
    }
}

/// Every request id skein is willing to act on.
///
/// An id is two things at once and used to be checked as only the first. It is a **filename** —
/// `<queue>/<id>.json` — so `..` and a slash have to go. It is also a **shell word**, because
/// [`decision_script`] names that file in a script the host runs, and the old check ("not empty, no
/// slash, no `..`") left `;`, `$`, a backtick, a pipe, an ampersand, a space and a newline all
/// legal. The strict [`crate::util::valid_name`] was being applied a few lines below to `box_name`,
/// which never reaches a shell at all: the validation was real, and it was pointed at the wrong
/// column.
///
/// A whitelist of shapes rather than a blacklist of tricks, the same way [`slug_is_nameable`] and
/// [`crate::substrate`]'s package check are, and for the same reason: a blacklist is a list of the
/// attacks somebody thought of. Every id this fleet actually files is `date -u +%Y%m%d-%H%M%S-$$`
/// (`box-session.sh`, `request_write`), which this admits with room to spare.
///
/// This is **not** what makes [`decision_script`] safe — that script quotes the id, and must,
/// because the queue is a directory any box can write and a check here would then be the only
/// thing standing between a box and a shell. Two independent reasons the same value cannot inject
/// is the intent; either alone would be one mistake away from not being a reason at all.
pub(super) fn id_is_nameable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// Every repository name skein is willing to put in a URL or a token request.
///
/// GitHub's own rules are narrower than this; the shapes refused here are the ones that would stop
/// the string being a repository at all — an empty side, a path escape, or anything that could end
/// the path and start something else.
pub(super) fn slug_is_nameable(slug: &str) -> bool {
    let Some((owner, name)) = slug.split_once('/') else {
        return false;
    };
    let ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && !s.contains("..")
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    };
    ok(owner) && ok(name)
}

/// `owner/name` from any spelling of a GitHub remote, or `None` if it is not one.
///
/// Handles the four forms that actually appear in this fleet's remotes and in git's own credential
/// query: `git@github.com:owner/name.git`, `https://github.com/owner/name.git`,
/// `ssh://git@github.com/owner/name`, and the bare `owner/name` a grant record holds.
///
/// Non-GitHub hosts return `None` rather than a slug. They have no App installation and no token to
/// mint, so treating them as a repository would produce a grant that can never be honoured.
pub fn slug_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // A filesystem path is not a remote. skein used to adopt repos in place, so a `repos.json`
    // written before that was refused can still hold `/Users/…/code/thing` in `repo.source` — and
    // without this that parses to `Users/…`, a repository that does not exist, which would be
    // minted against and refused with a message about the wrong thing.
    if url.starts_with('/') || url.starts_with('.') || url.starts_with('~') {
        return None;
    }
    // Strip a scheme and any userinfo, leaving host + path however it was spelled.
    let rest = url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(url)
        .rsplit_once('@')
        .map(|(_, r)| r)
        .unwrap_or_else(|| url.split_once("://").map(|(_, r)| r).unwrap_or(url));

    // `host:owner/name` (scp-like) or `host/owner/name`. The colon form is only scp-like when what
    // follows is not a port, which is the one ambiguity in the grammar.
    let (host, path) = if let Some((h, p)) = rest.split_once(':') {
        if p.chars()
            .take_while(|c| *c != '/')
            .all(|c| c.is_ascii_digit())
            && p.contains('/')
        {
            // host:port/path
            let p = p.split_once('/').map(|(_, r)| r).unwrap_or("");
            (h, p)
        } else {
            (h, p)
        }
    } else if let Some((h, p)) = rest.split_once('/') {
        (h, p)
    } else {
        // A bare `owner/name` has no host at all — the shape a grant record holds.
        return slug_is_nameable(rest).then(|| rest.to_string());
    };

    if !host.eq_ignore_ascii_case("github.com") && !host.is_empty() {
        // `owner/name` reaches here as host=`owner`, path=`name`; anything with a real non-GitHub
        // host does not, and must not.
        let whole = format!("{host}/{path}");
        return (host_is_not_a_host(host) && slug_is_nameable(&whole)).then_some(whole);
    }

    slug_from_path(path)
}

/// Does this look like an owner rather than a hostname?
///
/// The bare `owner/name` form and `host/owner/name` are the same shape with a different number of
/// segments, so one of them has to be decided by what the first segment looks like. A dot is the
/// tell: GitHub owners cannot contain one, hostnames of interest here always do.
fn host_is_not_a_host(first: &str) -> bool {
    !first.contains('.')
}

/// `owner/name` from the path part of a URL or git's credential query.
///
/// Exactly two segments, never a prefix of a longer path. Taking the first two would turn any deep
/// path into a plausible-looking repository — which is how `/Users/me/code/thing` became `Users/me`
/// and would have been minted against.
pub fn slug_from_path(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches('/');
    let (owner, name) = path.split_once('/')?;
    let name = name.trim_end_matches(".git");
    let slug = format!("{owner}/{name}");
    slug_is_nameable(&slug).then_some(slug)
}

/// Do these two names mean the same repository?
///
/// Case-insensitively, because GitHub preserves the case an owner typed but resolves without it —
/// so a remote saying `Acme/Thing` and a registry saying `acme/thing` are one repo, and
/// comparing them exactly would file an approval request for the box's *own* repository.
pub fn same_repo(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

/// What `box_name` may do with `target`, given its own repo and the live grants.
pub fn access(
    own: &str,
    target: &str,
    box_name: &str,
    grants: &[Grant],
    now: chrono::DateTime<chrono::Utc>,
) -> Access {
    if same_repo(own, target) {
        return Access::Write;
    }
    let granted = grants
        .iter()
        .any(|g| g.box_name == box_name && same_repo(&g.repo, target) && g.is_live(now));
    if granted {
        Access::Granted
    } else {
        Access::Read
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitgate::testkit::*;

    fn grant(box_name: &str, repo: &str, expires: &str) -> Grant {
        Grant {
            box_name: box_name.into(),
            repo: repo.into(),
            granted: "2026-08-13T00:00:00Z".into(),
            expires: expires.into(),
        }
    }

    #[test]
    fn every_spelling_of_a_github_remote_names_the_same_repository() {
        for url in [
            "git@github.com:acme/thing.git",
            "https://github.com/acme/thing.git",
            "https://github.com/acme/thing",
            "ssh://git@github.com/acme/thing.git",
            "acme/thing",
        ] {
            assert_eq!(slug_from_url(url).as_deref(), Some("acme/thing"), "{url}");
        }
    }

    #[test]
    fn a_repo_registered_from_a_local_path_is_not_mistaken_for_a_github_one() {
        // Registration refuses a path now, but a `repos.json` written before it did still carries
        // one. Reading `/Users/me/code/x` as the repository `Users/me` would have the host mint
        // against a repo that does not exist and refuse the box with a message about the wrong
        // thing entirely.
        assert_eq!(slug_from_url("/Users/me/code/thing"), None);
        assert_eq!(slug_from_url("./relative/path"), None);
        assert_eq!(slug_from_url("~/code/thing"), None);
        // And the deep-path form, which is the same mistake reached through a URL.
        assert_eq!(slug_from_url("https://github.com/a/b/tree/main/src"), None);
    }

    #[test]
    fn a_remote_that_is_not_github_has_no_repository_to_grant() {
        // There is no App installation to mint against, so calling it a repo would produce a grant
        // that can never be honoured — a refusal that reads as a bug forever after.
        assert_eq!(slug_from_url("git@gitlab.com:a/b.git"), None);
        assert_eq!(slug_from_url("https://bitbucket.org/a/b.git"), None);
        assert_eq!(slug_from_url(""), None);
    }

    #[test]
    fn the_case_an_owner_typed_never_files_a_request_against_the_boxs_own_repo() {
        // GitHub preserves case and resolves without it. Comparing exactly would have a box whose
        // remote says `Acme/Thing` asking permission to push to itself.
        assert_eq!(
            access("acme/thing", "Acme/Thing", "b", &[], now()),
            Access::Write
        );
    }

    #[test]
    fn a_box_may_write_its_own_repo_and_only_read_any_other() {
        assert_eq!(access("a/own", "a/own", "b", &[], now()), Access::Write);
        assert_eq!(access("a/own", "a/other", "b", &[], now()), Access::Read);
    }

    #[test]
    fn a_live_grant_opens_one_repo_and_no_more() {
        let g = [grant("b", "a/other", "2026-08-14T00:00:00Z")];
        assert_eq!(access("a/own", "a/other", "b", &g, now()), Access::Granted);
        assert_eq!(access("a/own", "a/third", "b", &g, now()), Access::Read);
    }

    #[test]
    fn a_grant_belongs_to_the_box_it_was_given_to() {
        // Grants are keyed by box, not by repo: approving one box's ask must not quietly approve
        // every other box of the same repo.
        let g = [grant("b", "a/other", "")];
        assert_eq!(access("a/own", "a/other", "b", &g, now()), Access::Granted);
        assert_eq!(
            access("a/own", "a/other", "elsewhere", &g, now()),
            Access::Read
        );
    }

    #[test]
    fn an_expired_grant_is_not_a_grant() {
        let g = [grant("b", "a/other", "2026-08-13T11:59:00Z")];
        assert_eq!(access("a/own", "a/other", "b", &g, now()), Access::Read);
    }

    #[test]
    fn a_grant_with_no_expiry_is_the_permanent_one() {
        assert!(grant("b", "a/other", "").is_live(now()));
    }

    #[test]
    fn an_expiry_nothing_can_parse_counts_as_expired_rather_than_forever() {
        // The alternative fails open: a corrupted date would silently upgrade a 24-hour grant into
        // a permanent one, and nothing would ever report it.
        assert!(!grant("b", "a/other", "whenever").is_live(now()));
    }

    #[test]
    fn re_approving_replaces_the_grant_rather_than_stacking_a_second_one() {
        let all = merged(
            vec![grant("b", "a/other", "2026-01-01T00:00:00Z")],
            grant("b", "A/Other", ""),
        );
        assert_eq!(all.len(), 1, "one pair, one record: {all:?}");
        assert!(all[0].is_live(now()), "the newer grant is the live one");
    }

    #[test]
    fn a_repository_name_cannot_climb_out_of_itself() {
        // The slug reaches a URL and a token request. `..` in it would address something else.
        assert!(!slug_is_nameable("../../etc/shadow"));
        assert!(!slug_is_nameable("a/../b"));
        assert!(!slug_is_nameable("-flag/x"));
        assert!(!slug_is_nameable("only-one-part"));
        assert!(slug_is_nameable("acme/thing"));
        assert!(slug_is_nameable("a.b_c/d.e-f"));
    }

    #[test]
    fn a_request_naming_an_impossible_box_or_repo_is_refused() {
        let ok = Request {
            id: "20260813-1".into(),
            box_name: "example-work".into(),
            repo: "acme/thing".into(),
            state: "pending".into(),
            ..Default::default()
        };
        assert!(ok.problem().is_none(), "{:?}", ok.problem());

        let mut bad = ok.clone();
        bad.repo = "../../x".into();
        assert!(bad.problem().is_some());

        let mut bad = ok.clone();
        bad.id = "../../.skein/fleet-agent.token".into();
        assert!(bad.problem().is_some());

        let mut bad = ok;
        bad.box_name = "../elsewhere".into();
        assert!(bad.problem().is_some());
    }

    /// An id is a filename and a shell word, so it is checked like both.
    ///
    /// The check used to be "not empty, no slash, no `..`" — a path check on a value that also
    /// reaches a shell, which leaves `;`, `$`, a backtick, a pipe, a space and a newline all
    /// legal. The strict [`crate::util::valid_name`] was being applied to `box_name` instead,
    /// which is not the field that gets shelled: the validation was real and pointed at the wrong
    /// column.
    #[test]
    fn an_id_that_is_not_a_plain_name_is_refused() {
        let ok = Request {
            id: "20260813-143000-4210".into(),
            box_name: "web-main".into(),
            repo: "acme/thing".into(),
            ..Default::default()
        };
        assert!(ok.problem().is_none(), "{:?}", ok.problem());

        for bad in [
            "a;touch /tmp/x",
            "$(id)",
            "`id`",
            "a|b",
            "a&b",
            "a b",
            "a\nb",
            "a>b",
            "a'b",
            "a\"b",
            "-a",
            "..",
            "a/b",
            "",
        ] {
            let mut req = ok.clone();
            req.id = bad.into();
            assert!(
                req.problem().is_some(),
                "{bad:?} is not a request id, and this is the check between it and a shell"
            );
            // And the refusal is on the acting path, not only in the struct: `decide` returns here
            // without reaching the sandbox at all, which is why naming a sandbox that does not
            // exist proves the point rather than needing one that does.
            assert!(
                decide("no-such-sandbox", &req, false, None)
                    .unwrap_err()
                    .contains("refusing to act"),
                "{bad:?} must be refused by the call that runs the script, not merely describable"
            );
        }
    }
}
