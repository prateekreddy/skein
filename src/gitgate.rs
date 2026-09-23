//! Which repositories a box may push to, and how it asks for one it may not.
//!
//! Every box in this fleet used to hold the same GitHub credential: a user token with `repo`,
//! `admin:public_key`, `gist` and `read:org`, reaching **460 repositories** read and write, plus a
//! forwarded ssh-agent socket signing for anything that key could reach. Ten boxes, one identity.
//! An agent that misread a remote could push to any of them, and `admin:public_key` let one add a
//! key to the account — access that outlives the sandbox and appears nowhere in skein.
//!
//! What replaces it is two credentials with different shapes:
//!
//! * **read** — a read-only credential (`contents: read` + `metadata: read`): a per-owner App
//!   installation token, or the optional cross-repo read PAT. It is delivered as a FILE the host
//!   places under the box's `git-tokens/` — `read/<owner>`, falling back to `read/_any` — which the
//!   credential helper hands over for a repo with no write token (`git-credential-skein.sh`, and
//!   `refresh_tokens`/`token_file` here place them). It cannot write anywhere, which is the point.
//!   It is deliberately NOT delivered as `GH_TOKEN`: a scoped box `unset`s `GH_TOKEN` in
//!   `box-session.sh` so that nothing in the environment carries a credential, and `GH_TOKEN` holds
//!   only this box's own-repo WRITE token when one exists, for `gh pr create` against that one repo.
//! * **write** — a GitHub App installation token scoped to **one repository**, valid an hour,
//!   minted by the host and dropped into the box's own host-mounted state directory. Or, for
//!   someone who would rather not install an App across their account, a fine-grained PAT they
//!   minted themselves — stored per repository, and **only** per repository.
//!
//! So no write-capable credential sits in a box's environment at all, and the App's private key
//! never leaves the host.
//!
//! **Why a stored token may cover exactly one repo.** Because the credential helper cannot contain
//! anything. It runs *inside* the box, as the same uid as the agent, so whatever it can read the
//! agent can read — the token file, its environment, its argv. It picks which credential to hand
//! over; it cannot stop anyone taking the other one. A token covering three repositories is
//! therefore write access to three repositories for every box that receives it, however carefully
//! the helper offers it for one. One repo per token means the credential a box holds is already
//! exactly as narrow as its rights, so nothing has to be trusted to stay in its lane. (The other
//! way to make a broad credential safe is for it never to enter the box — a host-side git proxy —
//! which is a different design and not this one.)
//!
//! **Why the host pushes tokens rather than the box asking for one.** A credential helper has to
//! answer inside a single `git` invocation, which wants a synchronous channel — and there isn't a
//! reliable one from a box to the cockpit (measured: `host.docker.internal:7878` answers 500 through
//! the gateway). But the box's state directory is *already* a host mount, because that is where its
//! conversation lives. The host refreshes a token file there before the hour is out; the helper only
//! ever reads a file. No new transport, and nothing to be down.
//!
//! **What this gate is, precisely.** Unlike [`crate::substrate`], the boundary here is real: GitHub
//! enforces it server-side, so a box acting with a token scoped to one repository cannot touch
//! another with it.
//!
//! **And what bounds the box, not just the token — SKEIN-548, now closed for the git path.**
//! Measured from inside a live box on 2026-09-06/07/11 and again 2026-09-15: the sandbox routes HTTP
//! through a proxy that TERMINATES TLS for the GitHub hosts, so a request carrying no Authorization
//! header — or a deliberately invalid one, or this box's own per-repo token — came back
//! authenticated as the account, while the same request sent direct is refused. `git` inherited it,
//! which made the token a placeholder rather than the credential anything authenticated with. The
//! close is not in this module: the launcher (`src/box-session.sh`) puts the
//! GitHub hosts in `NO_PROXY` for a scoped box, so git and gh reach GitHub DIRECT and present the
//! token this module places — GitHub then enforces it, and a box's REACH is bounded by its token
//! after all. What is still not bounded by this module is a process that deliberately re-routes
//! through the proxy; the substrate's deny-by-default egress (SKEIN-926) is what answers that. A
//! `fleet`-scoped box keeps the proxy and the account-wide token on purpose — the honest opt-out.
//!
//! It is also no longer only a fleet→GitHub wall. A box's token file used to be readable by every
//! other box — same uid, and every box's state directory in view — so scoping one box was worth
//! whatever the *least* careful box in the fleet did. [`crate::fleet::session_script`] now gives each
//! box a mount namespace in which no other box's directory exists, which is what makes a per-box
//! token mean per-box. Two exceptions, both deliberate and both named: the workshop box opts out
//! (see [`crate::fleet::box_is_privileged`]), and the credential helper still cannot contain
//! anything *within* a box — hence one repository per token, below.
//!
//! **How this module reaches GitHub: through [`crate::github`], and through nothing else.** It used
//! to have its own curl wrapper — a `--config` document for the JWT, a spawn, a JSON parse — which
//! was the second copy of a client that already existed, and the poorer copy of it. Three things it
//! did not have, each one bought by a live failure over there:
//!
//! * **The HTTP status.** It never asked for one, so "is this an error" was guessed from the body:
//!   a `message` and neither a `token` nor an `id`. GitHub's refusals routinely carry an `id`, and
//!   one of those came back as *success* — which [`check_token`] then read as a live token that has
//!   lost its push rights, an answer that discards a working credential.
//! * **The request body off argv.** It spent one as `-d <body>`, readable by every process on the
//!   host, in the same call whose credential the `--config` document beside it existed to hide.
//! * **The rate-limit hold, and a deadline skein can describe.** `--max-time 20` ends a call; it
//!   cannot tell anyone whether GitHub said nothing or was still talking, and it does not stop the
//!   next doomed request from being sent.
//!
//! So an App-token mint now waits behind the same hold as the review queue, is cut off by the same
//! deadline with the same sentence, and reads GitHub's status rather than sniffing its body.

use crate::secret::Secret;
use crate::util::sh_quote;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::Duration;

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
fn id_is_nameable(id: &str) -> bool {
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
fn slug_is_nameable(slug: &str) -> bool {
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

// ───────────────────────────── where things live ─────────────────────────────

/// The request queue, **inside the sandbox**: boxes must be able to write it, so it cannot be on the
/// host. Same reasoning as [`crate::substrate::substrate_dir`], and beside it for the same reason.
pub fn gitgate_dir() -> String {
    format!("{}/.skein/gitgate", crate::fleet::fleet_root())
}

/// The queue root, and **one directory below it is a box's identity**.
///
/// A request lives at `requests/<box>/<id>.json`. The launcher creates that directory outside the
/// box's mount namespace and binds it — alone — read-write into that box, so the path a request was
/// read from is the one thing about it no box chose. Everything else, the `box` field included, is
/// a value the requester wrote, which is why [`list`] overwrites that field from the path.
///
/// **This queue is where getting it wrong costs the most.** [`decide`] builds the grant from the
/// request's box as well as its repo, and [`refresh_tokens`] writes the minted installation token
/// into the box the grant names — so a box able to file under another box's name could have a live
/// GitHub write token placed in a box of its choosing, off one approval a person read as somebody
/// else's ask. Architecture §8.4 orders the three steps for exactly this: bind the artifact, make
/// the request path per box, *then* unmask the queue.
fn requests_dir() -> String {
    format!("{}/requests", gitgate_dir())
}

/// One box's drop-box in that queue, for the same reason as
/// [`crate::substrate::box_requests_dir`]: the directory outlives the box that asked through it,
/// and whatever removes it has to address it by the spelling that reads it (SKEIN-736).
pub(crate) fn box_requests_dir(box_name: &str) -> String {
    format!("{}/{box_name}", requests_dir())
}

/// The grant record, on the **host**, beside `repos.json`.
///
/// Not in the fleet root with the queue: the fleet root dies with the sandbox, and a grant that
/// vanished on rebuild would have every box asking again for access its owner already approved.
/// The queue is in the sandbox because boxes write it; the record is not, because only the host does.
fn grants_path() -> std::path::PathBuf {
    crate::config::skein_home().join("git-grants.json")
}

/// Where the fleet owner's answer to one request is kept: on the **host**, one file per request,
/// under the box that asked (SKEIN-940).
///
/// The queue says what was ASKED and this says what was DECIDED, and the two are kept apart because
/// only one of them is a box's to write. The queue file is bound read-write into the asking box,
/// so its `state` is that box's word like every other field in it — and it used to be the only
/// record a denial ever had, and the only thing the cockpit read to decide whether to offer Grant.
/// A box that wrote `"state": "granted"` into its own ask was shown as answered, with nothing to
/// press, and its owner was never asked. [`grants_path`] could not stand in for this: it is keyed
/// by box and repository rather than by request, and it holds approvals only.
///
/// Beside [`grants_path`] and for its reason: `$SKEIN_HOME` is the volume on an in-fleet fleet, and
/// the launcher covers the volume in every box except for that box's own state, bound read-only. The
/// bwrap test `a_box_cannot_answer_its_own_git_write_request` runs a box that tries to write here.
///
/// **Under the box, because an id is not an identity** — the reason
/// [`crate::substrate`]'s decisions are filed the same way (ISO-7). A box can read every other box's
/// queue and file the id it saw a neighbour use; keyed by id alone, the neighbour's answer would be
/// painted onto the copy.
pub fn decision_path(box_name: &str, id: &str) -> Option<std::path::PathBuf> {
    (crate::util::valid_name(box_name) && id_is_nameable(id)).then(|| {
        crate::config::skein_home()
            .join("gitgate")
            .join(box_name)
            .join(format!("{id}.json"))
    })
}

/// Where a box finds the token for a repository it may write.
///
/// Inside the box's own host-mounted state directory, which is the whole reason this design needs no
/// box→host channel. One file per repository rather than one per box: a box with a grant holds two
/// tokens with different scopes, and a single file could only ever hold the narrower one.
pub fn token_file(box_name: &str, slug: &str) -> String {
    format!(
        "{}/git-tokens/{}",
        crate::fleet::box_state(box_name),
        slug.replace('/', "%2F")
    )
}

// ───────────────────────────── the queue ─────────────────────────────

/// Parse the array `jq -s` produces from the queue.
///
/// Malformed entries are dropped rather than failing the read: one unparseable file — a box writing
/// a request while this runs — must not blank the cockpit's whole list.
pub fn parse_requests(json: &str) -> Vec<Request> {
    let mut out: Vec<Request> = serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Request>(v).ok())
        .filter(|r: &Request| !r.id.is_empty())
        .collect();
    out.sort_by(|a, b| a.asked.cmp(&b.asked).then(a.id.cmp(&b.id)));
    out
}

/// The script that reads the queue, **stamping each request with the box whose directory it is in**.
///
/// Split out for the reason [`decision_script`] is, and it carries more: it is the only thing that
/// decides who a request is from, and on this queue that decides which box a token lands in. The
/// sibling is [`crate::substrate`]'s, deliberately written the same way — two queues, one shape.
///
/// The field is overwritten rather than compared, and a request found directly under the queue root
/// (from before the split) is stamped with the **empty** name: [`Request::problem`] already refuses
/// a box name that is not a name, so such a request is shown to its owner and cannot be acted on,
/// which is the honest answer when nothing can say any more which box wrote it.
fn list_script() -> String {
    // `jq -n` with `inputs` rather than `jq -s`: `input_filename` tracks the file each value came
    // from only while they are pulled one at a time, and that filename is the whole point.
    // `select(type=="object")` because a box can put a JSON array in its own file, and `.box =` on
    // an array is a jq error that would blank the panel for every box.
    format!(
        "d={}; set --; for f in \"$d\"/*/*.json \"$d\"/*.json; do [ -f \"$f\" ] && set -- \"$@\" \"$f\"; done; \
         [ $# -gt 0 ] || {{ echo '[]'; exit 0; }}; \
         jq -n --arg d \"$d\" '[inputs | select(type==\"object\") \
           | .box = (input_filename | ltrimstr($d + \"/\") | split(\"/\") | if length == 2 then .[0] else \"\" end)]' \
           \"$@\" 2>/dev/null || echo '[]'",
        sh_quote(&requests_dir())
    )
}

/// Every access request the fleet knows about, oldest first.
pub fn list(sandbox: &str) -> Result<Vec<Request>, String> {
    let out = crate::place::own_sandbox(sandbox).exec(&list_script(), Duration::from_secs(30))?;
    Ok(decided_over(parse_requests(&out)))
}

/// The host's answer wins over the box's copy of it, request by request (SKEIN-940).
///
/// Separate and pure because it is the rule, not a detail of reading — the same rule as
/// [`crate::substrate`]'s function of the same name. What the host decided replaces the three fields
/// a decision owns: the state, when, and **the repository that was decided on**, so a box that
/// edits its ask after the answer cannot make the row describe a different grant. What survives from
/// the box is the ask itself — its reason and when it asked — because that is the part only the box
/// can know, and the server does not keep it when a person decides.
fn decided_over(asked: Vec<Request>) -> Vec<Request> {
    asked
        .into_iter()
        .map(|mut asked| match decision(&asked.box_name, &asked.id) {
            Some(host) => {
                asked.state = host.state;
                asked.decided = host.decided;
                asked.repo = host.repo;
                asked
            }
            None => undecided(asked),
        })
        .collect()
}

/// A request the host has not answered, as it is allowed to describe itself.
///
/// `pending` rather than dropping the row: an ask that vanishes looks, to the box that filed it,
/// exactly like one nobody got to, and there is a person who can answer this one. Whatever `state`
/// and `decided` the file carried are the box's own bytes, and are thrown away here.
fn undecided(mut asked: Request) -> Request {
    asked.state = "pending".into();
    asked.decided = String::new();
    asked
}

/// The answer skein recorded for this request — with **"nobody has answered this" kept apart from
/// "skein cannot tell"**, the distinction [`decide`]'s guard turns on and the one
/// [`crate::substrate`]'s function of the same name keeps (SKEIN-418). `Ok(None)` is a request
/// nobody has answered, `Ok(Some(_))` is the answer, and `Err` is a record that is *there* and will
/// not read.
fn decision_or_why(box_name: &str, id: &str) -> Result<Option<Request>, String> {
    let path = decision_path(box_name, id).ok_or_else(|| format!("unusable request id {id:?}"))?;
    crate::util::read_json_or_why::<Request>(&path)
}

/// The answer skein recorded for this request, if there is one — **for the reader**, [`decided_over`].
/// An unreadable file reads as none, which puts the row back in front of its owner with a button on
/// it: the direction a person can see and fix, rather than a request silently counted as answered.
/// Pressing that button is [`decide`], which asks [`decision_or_why`] and refuses, so the unreadable
/// file costs a confusing row and never a second answer (SKEIN-1034).
fn decision(box_name: &str, id: &str) -> Option<Request> {
    decision_or_why(box_name, id).ok().flatten()
}

fn write_decision(path: &std::path::Path, req: &Request) -> Result<(), String> {
    let dir = path.parent().ok_or("no decisions directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::to_vec_pretty(req).map_err(|e| e.to_string())?;
    crate::util::write_atomic(path, dir, &body)
}

/// The script that records a decision against a request.
///
/// Split out from [`decide`] so its shape can be asserted without a sandbox: this writes into a file
/// the box it belongs to can also write, so it must never shell a value in unquoted.
///
/// `box_name` is a path component now that the queue is per box, so it is quoted like the id and,
/// like the id, refused before it arrives — [`Request::problem`] already required it to be a name.
fn decision_script(box_name: &str, id: &str, state: &str) -> String {
    format!(
        "f={}/{}/{}.json; [ -f \"$f\" ] || {{ echo 'no such request' >&2; exit 1; }}; \
         t=$(mktemp \"$(dirname \"$f\")/.tmp.XXXXXX\") || exit 1; \
         jq --arg s {} --arg d \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \
            '.state=$s | .decided=$d' \"$f\" >\"$t\" \
           && mv -f \"$t\" \"$f\" || {{ rm -f \"$t\"; exit 1; }}",
        sh_quote(&requests_dir()),
        sh_quote(box_name),
        // Validated by the caller *and* quoted here, and the `.trim_matches('\'')` that used to sit
        // on this line took the quotes straight back off — so the comment claiming it was "quoted
        // anyway" described the opposite of what the code did, and the doc above promising never to
        // shell a value in unquoted was describing the one line that did. An id is a value a box
        // wrote, so `$(…)` in it ran the moment a person pressed approve or deny.
        //
        // Why the quoting is sound as written: `f=` takes one word, and shell concatenates
        // adjacent quoted and unquoted pieces into it. `'<dir>'/'<id>'.json` is therefore a single
        // word whose only unquoted parts are this module's own literals — a slash and `.json` —
        // while every byte either value contributed is inside single quotes, where nothing at all
        // is expanded. `sh_quote` is what makes that true of an id containing a quote of its own.
        sh_quote(id),
        sh_quote(state),
    )
}

/// Approve or deny a request. Approving records the grant host-side; the token that makes it usable
/// is minted by the refresher, so the cockpit answers immediately rather than waiting on GitHub.
/// Approve or deny **the request the approver was looking at**.
///
/// `rendered` rather than an id, and the difference is the whole of this fix. The queue lives in
/// the sandbox and every box can write it, so re-reading by id at click time grants whatever the
/// file says *then* — and the grant is built from the request's **box** as well as its repo, so the
/// swap is not merely "a different repository". It is *put a live installation token in a box of my
/// choosing*: the box field decides which box `refresh_tokens` writes the minted token into.
///
/// So box, repo and expiry all travel from the render. Nothing about the grant comes from a read
/// that happened after the person decided.
///
/// **A request is answered once** (SKEIN-1034), as [`crate::substrate`]'s are. A second answer used
/// to go straight through: a denial after a grant rewrote the row as denied while the grant it
/// never touched stayed live and the refresher kept minting its token, and a grant after a grant
/// quietly re-recorded it with a fresh expiry. Changing one's mind about an approval is
/// [`revoke`], which acts on the grant itself; a second answer here is refused, and so is one where
/// skein cannot read whether there was a first.
pub fn decide(
    sandbox: &str,
    rendered: &Request,
    approve: bool,
    hours: Option<i64>,
) -> Result<Request, String> {
    if let Some(why) = rendered.problem() {
        return Err(format!("refusing to act on this request: {why}"));
    }
    let path = decision_path(&rendered.box_name, &rendered.id).ok_or("unusable request id")?;
    match decision_or_why(&rendered.box_name, &rendered.id) {
        Ok(None) => {}
        Ok(Some(already)) => {
            return Err(format!(
                "request {} is already {} — a request is answered once; to take back a grant, \
                 revoke it",
                rendered.id, already.state
            ))
        }
        // **A guard is not a store, so it does not get to fall back to a default** — substrate's
        // reason (SKEIN-418), and the size of the two mistakes is the same here. Refusing an id
        // nobody answered costs a person one file to move and one press again. Answering one that
        // was answered either writes a grant over a denial or records a denial over a live grant
        // that goes on being honoured, and destroys the first answer on the way.
        Err(why) => {
            return Err(format!(
                "refusing to answer {} — skein cannot read the answer it may already have given \
                 ({why}). A request is answered once, and that file is the only record of whether \
                 this one was. The file is left alone; fix or move it, then answer again.",
                rendered.id
            ))
        }
    }
    let state = if approve { "granted" } else { "denied" };
    // The grant first, because it is what skein acts on: an answer recorded as granted with no
    // grant behind it would be a row telling its owner something that is not true.
    if approve {
        record(rendered, hours)?;
    }
    let mut done = rendered.clone();
    done.state = state.into();
    done.decided = chrono::Utc::now().to_rfc3339();
    // The answer, where [`list`] reads it and no box can write it (SKEIN-940). A denial had no
    // record anywhere else.
    write_decision(&path, &done).map_err(|e| {
        format!(
            "{} but skein could not record the answer ({e}), so the request will still be shown as \
             waiting",
            if approve {
                "the grant is recorded and will be honoured,"
            } else {
                "the request was denied,"
            }
        )
    })?;
    // Courtesy only, and it must stay that way: the box reads its own file to learn what happened,
    // and skein never reads that answer back — [`list`] takes the state from the record above.
    // Best-effort, because a box that deletes its request has changed nothing that matters.
    let _ = crate::place::own_sandbox(sandbox).exec(
        &decision_script(&rendered.box_name, &rendered.id, state),
        Duration::from_secs(30),
    );
    Ok(done)
}

/// Every write request the fleet has been asked for, for the cockpit.
///
/// An unreachable sandbox reads as an empty queue rather than an error: this is polled beside the
/// board, and a fleet that is down should not paint this panel red about GitHub.
pub fn fleet_requests() -> Vec<Request> {
    list(&crate::place::fleet_sandbox()).unwrap_or_default()
}

/// Approve or deny a request. `hours` is `None` for a grant that never expires.
pub fn fleet_decide(
    rendered: &Request,
    approve: bool,
    hours: Option<i64>,
) -> Result<Request, String> {
    decide(&crate::place::fleet_sandbox(), rendered, approve, hours)
}

// ───────────────────────────── the grant record ─────────────────────────────

/// Every grant on record, expired ones included — the cockpit shows those too, because "this box had
/// access until Tuesday" is the answer to a question the list exists to answer.
///
/// A file that will not parse reads as no grants, and that is the safe direction *for this read*:
/// [`access`] denies, the token refresher places nothing, and a box is locked out rather than let
/// in. It is said out loud once, because "no grants" and "skein cannot read your grants" look
/// identical on the screen and only one of them is fixable. The writes below refuse outright —
/// [`record`] and [`revoke`] go through [`crate::util::update_json`], which will not write a
/// default over a file it could not read.
pub fn grants() -> Vec<Grant> {
    match crate::util::read_json_or_why::<Vec<Grant>>(&grants_path()) {
        Ok(found) => found.unwrap_or_default(),
        Err(why) => {
            // Once per process, for `config::load_config`'s reason: this is on the path of the
            // token refresher and of every cockpit poll, and a line per call buries the one line
            // that matters under thousands of copies of itself.
            static TOLD: std::sync::Once = std::sync::Once::new();
            TOLD.call_once(|| {
                eprintln!(
                    "skein: cannot read your GitHub grants ({why}) — every box will be refused \
                     write access until that file parses, and skein will refuse to write over it. \
                     Fix or move the file."
                );
            });
            Vec::new()
        }
    }
}

/// Add a grant, replacing any earlier one for the same box and repository.
///
/// Replacing rather than appending: re-approving after an expiry is the common case, and two records
/// for one pair would leave [`access`] answering from whichever happened to be first.
pub fn merged(mut all: Vec<Grant>, next: Grant) -> Vec<Grant> {
    all.retain(|g| !(g.box_name == next.box_name && same_repo(&g.repo, &next.repo)));
    all.push(next);
    all.sort_by(|a, b| a.box_name.cmp(&b.box_name).then(a.repo.cmp(&b.repo)));
    all
}

fn record(req: &Request, hours: Option<i64>) -> Result<(), String> {
    let now = chrono::Utc::now();
    let grant = Grant {
        box_name: req.box_name.clone(),
        repo: req.repo.clone(),
        granted: now.to_rfc3339(),
        expires: match hours {
            // A grant with no hours is permanent, and says so by holding no date at all.
            None => String::new(),
            Some(h) => (now + chrono::Duration::hours(h)).to_rfc3339(),
        },
    };
    // Under the lock, with the existing grants read inside it: `merged` replaces the entry for one
    // (box, repo) pair and keeps the rest, so a stale read here would drop whatever grant another
    // approval had just recorded. And `update_json` refuses on a grants file it cannot read rather
    // than writing this one grant over it — approving one box is not how a fleet loses the access
    // every other box was given (SKEIN-359).
    crate::util::update_json(&grants_path(), |all: &mut Vec<Grant>| {
        *all = merged(std::mem::take(all), grant);
        Ok(())
    })
}

/// Withdraw a grant early. The token file is removed by the next refresh, and until then the grant
/// is already gone from [`access`] — so a revoke is effective the moment it is recorded.
///
/// Refuses on a grants file it cannot read, like [`record`]. A revoke is *already* the fail-closed
/// direction, so answering Ok on a file that was never read would be the one shape that is worse
/// than either: every other grant destroyed, and a person told the withdrawal they asked for is the
/// only thing that happened.
pub fn revoke(box_name: &str, repo: &str) -> Result<(), String> {
    crate::util::update_json(&grants_path(), |all: &mut Vec<Grant>| {
        all.retain(|g| !(g.box_name == box_name && same_repo(&g.repo, repo)));
        Ok(())
    })
}

/// The live grants for one box, which is what the token refresher acts on.
pub fn live_grants_for(box_name: &str, now: chrono::DateTime<chrono::Utc>) -> Vec<Grant> {
    grants()
        .into_iter()
        .filter(|g| g.box_name == box_name && g.is_live(now))
        .collect()
}

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

/// Any user PAT this fleet already holds, for the host's own GitHub calls.
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
        return Err("no GitHub App configured: Settings → GitHub App ID".into());
    }
    // An App id is a number, and [`jwt_claim`] interpolates it straight into a JSON claim. Checked
    // rather than trusted: `config.json` is an ordinary file that can be hand-edited, and an `id`
    // holding a quote would rewrite the claim around it rather than merely being a wrong id. The
    // resulting JWT is signed, so GitHub would refuse it either way — this turns an obscure refusal
    // into a message that names the actual problem, and closes the injection on its own terms.
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "the GitHub App ID must be the numeric id, not {id:?} — Settings → GitHub App ID"
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The grant that gets recorded is the one that was on screen — box included.
    ///
    /// "The approving side writes the artifact" was already true here: `record` writes the grant on
    /// the host. It was still wrong, because the grant was built from a **re-read by id** at click
    /// time, and the window that opened is not approve-to-install but render-to-click — a person
    /// reading a card, seconds to minutes.
    ///
    /// And the swap is worse than "a different repository". `refresh_tokens` writes the minted
    /// installation token into the box the grant names, so a request rewritten between render and
    /// click puts a live write credential in a box of the requester's choosing.
    #[test]
    fn the_grant_recorded_is_the_one_that_was_shown() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // `decide` addresses the box's copy of the request through `requests_dir()`, which reads
        // `$SKEIN_FLEET_ROOT` — unset, that is `/boxes`, a live fleet on any machine running
        // skein. The grant is written on the host under `$SKEIN_HOME`, so nothing asserted here
        // carries the root: the fixture only has to keep the read off somebody's infrastructure.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));

        let rendered = Request {
            id: "20260812-101010-1".into(),
            box_name: "web-main".into(),
            repo: "acme/web".into(),
            state: "pending".into(),
            ..Default::default()
        };
        // No sandbox, so the courtesy write-back into the box's file fails and is ignored — which
        // is the point: nothing about the grant depends on that file.
        let done = decide("no-such-sandbox", &rendered, true, Some(24)).expect("granted");
        assert_eq!(done.state, "granted");

        let recorded = grants();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            (recorded[0].box_name.as_str(), recorded[0].repo.as_str()),
            ("web-main", "acme/web"),
            "box and repo both travel from the render, or the token lands somewhere nobody chose"
        );
        assert!(
            !recorded[0].expires.is_empty(),
            "and so does the expiry: a grant meant for a day must not become permanent"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        // And `$SKEIN_HOME`, which this test did not put back until SKEIN-693: the env lock
        // serialises the tests that take it and restores nothing, so a home left set is the next
        // test's store — this temp directory, read after it has been deleted, by a test that
        // pinned none of its own. REMOVED rather than restored to whatever the process started
        // with, and that is the direction to err in: `config::skein_home` refuses an unset home in
        // a test process (SKEIN-626), so an unpinned reader after this fails loudly, while a
        // restored outer value would quietly send it at the owner's real `~/.skein`.
        std::env::remove_var("SKEIN_HOME");
    }

    fn grant(box_name: &str, repo: &str, expires: &str) -> Grant {
        Grant {
            box_name: box_name.into(),
            repo: repo.into(),
            granted: "2026-08-13T00:00:00Z".into(),
            expires: expires.into(),
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-08-13T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
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

    #[test]
    fn one_unparseable_request_does_not_hide_the_others() {
        let json = r#"[{"id":"a","box":"x","repo":"o/r","asked":"2026-01-01"},
                       {"box":"x"},
                       "not an object",
                       {"id":"b","box":"y","repo":"o/s","asked":"2026-01-02"}]"#;
        let got = parse_requests(json);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].id, "a", "oldest first");
    }

    /// **Who a request is from is the directory it is in, and on this queue that is which box a
    /// token lands in.**
    ///
    /// [`decide`] builds the grant from the request's box as well as its repo, and
    /// [`refresh_tokens`] writes the minted installation token into the box the grant names. While
    /// the queue was one shared read-write directory, the `box` field was whatever the requester
    /// typed into its own file — so an approval a person read as one box's ask put a live GitHub
    /// write token in another's. This runs [`list_script`] over a real tree: the first request lies
    /// in its file and must come back attributed to the directory it was found in.
    ///
    /// The last case is a request from before the split. It comes back with no box, and
    /// [`Request::problem`] refuses it — shown, so its owner sees an ask exists, and unactionable,
    /// because nothing can say now which box would receive the token.
    #[test]
    fn the_box_a_request_is_from_is_the_directory_it_is_in() {
        if std::process::Command::new("sh")
            .args(["-c", "command -v jq >/dev/null"])
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            crate::testutil::skip("no jq, so the queue cannot be read at all");
            return;
        }
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        let queue = std::path::Path::new(&requests_dir()).to_path_buf();
        for (dir, name, body) in [
            (
                queue.join("web-main"),
                "a.json",
                r#"{"id":"a","box":"api","repo":"o/r","asked":"2026-01-01"}"#,
            ),
            (
                queue.join("api"),
                "b.json",
                r#"{"id":"b","box":"api","repo":"o/s","asked":"2026-01-02"}"#,
            ),
            (
                queue.clone(),
                "c.json",
                r#"{"id":"c","box":"web-main","repo":"o/t","asked":"2026-01-03"}"#,
            ),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), body).unwrap();
        }
        std::fs::write(queue.join("web-main/arr.json"), "[1,2]").unwrap();

        let out = std::process::Command::new("bash")
            .arg("-lc")
            .arg(list_script())
            .output()
            .expect("bash");
        let got = parse_requests(&String::from_utf8_lossy(&out.stdout));
        let by = |id: &str| {
            got.iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("no request {id} in {got:?}"))
                .clone()
        };
        assert_eq!(got.len(), 3, "one request per readable file: {got:?}");
        assert_eq!(
            by("a").box_name,
            "web-main",
            "the request said `api` and was found in `web-main`; a grant is built from this name"
        );
        assert_eq!(by("b").box_name, "api");
        assert_eq!(
            by("c").box_name,
            "",
            "a request from before the split has no directory to be attributed by"
        );
        assert!(
            by("c").problem().is_some(),
            "an unattributable request must not be grantable"
        );
        assert!(by("a").problem().is_none(), "{:?}", by("a").problem());
        // Put back, because the env lock serialises the tests that take it and does not
        // restore what one of them changed: a `$SKEIN_FLEET_ROOT` left set makes every
        // later test that reads the DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    #[test]
    fn a_decision_never_splices_a_value_into_the_script_unquoted() {
        // Building the script reads `$SKEIN_FLEET_ROOT` through `requests_dir()`; the fixture
        // keeps that off the `/boxes` default without changing anything asserted below.
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        let s = decision_script("web-main", "20260813-1-1", "granted");
        assert!(s.contains("--arg s 'granted'"), "{s}");
        assert!(
            s.contains("mv -f"),
            "the request is replaced atomically: {s}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// The field that actually carries a stranger's bytes reaches the shell inside quotes.
    ///
    /// Named for the **id**, and that is the whole point of it existing beside the test above.
    /// That one is called "never splices a value in unquoted" and asserts it of `state` — which
    /// this module chooses between two of its own string literals and which no box has ever been
    /// able to influence. So it passed for as long as `id` was spliced in bare — and `id` is the
    /// one field a box writes that reaches the shell at all, so it is the only one that could ever
    /// have been the injection. A test aimed at the safe field is not a weaker version of this
    /// one; it is a test of nothing.
    #[test]
    fn a_request_id_reaches_the_decision_script_only_inside_its_own_quotes() {
        // Same read of `$SKEIN_FLEET_ROOT`, same fixture: what is asserted is what the box's bytes
        // did to the script around the root, never the root itself.
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        let nasty = "20260813-1-1'; touch /tmp/skein-pwned; :'$(id)`id`";
        // The box name is the second value a box's bytes reach this script through, since the
        // queue was split per box and the name became a path component. Both are checked, and
        // one at a time, so a hole in either is attributed rather than masked by the other.
        for (box_name, id) in [("web-main", nasty), (nasty, "20260813-1-1")] {
            let s = decision_script(box_name, id, "granted");
            let quoted = sh_quote(id);
            assert!(
                s.contains(&format!("/{quoted}.json")),
                "the id is the filename and arrives as one single-quoted word: {s}"
            );
            assert!(
                s.contains(&format!("/{}/", sh_quote(box_name))),
                "the box is the directory and arrives as one single-quoted word: {s}"
            );
            // Now take away everything the box's values contributed. What is left is this module's
            // own script, and none of the box's bytes may survive in it: a bare copy standing
            // beside the quoted one is the same hole with a witness.
            let rest = s.replace(&sh_quote(nasty), "");
            assert!(
                !rest.contains("touch") && !rest.contains("$(id)") && !rest.contains('`'),
                "something arrived outside the quotes it was wrapped in: {rest}"
            );
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// The same property, settled by a shell instead of by reading one.
    ///
    /// `$(…)` is the shape that needs no quote-balancing to work: it expands inside double quotes
    /// and inside no quotes at all, and only single quotes stop it. So it is exactly what an
    /// unquoted splice costs, and the marker file is the evidence rather than an argument about
    /// one. Nothing else in the script runs — the file the id names does not exist, so
    /// `[ -f "$f" ]` fails and it exits long before `mktemp`.
    ///
    /// The scenario is not hypothetical. The queue this writes into lives in the sandbox and every
    /// box can write it, so `id` is a value a box chose; `decide` runs this the moment a person
    /// presses approve or deny. The approval gate is the trust boundary, so a payload that runs
    /// when it is *used* is a box helping itself to the host through the one act meant to stop it.
    #[test]
    fn a_request_id_cannot_run_a_command_when_the_decision_script_does() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // This script is RUN, and its first act is to stat a path under the fleet root. Unpinned
        // that is `/boxes/.skein/gitgate/requests/…` on the owner's live fleet — the queue boxes
        // write — reached here with a payload built to be hostile. The fixture is the same
        // directory the marker is watched in, so the whole test acts inside one tree that goes
        // when it ends.
        std::env::set_var("SKEIN_FLEET_ROOT", &*dir);
        let marker = dir.join("pwned");
        let payload = format!("$(touch {})", marker.display());
        for (box_name, id) in [
            ("web-main", payload.as_str()),
            (payload.as_str(), "20260813-1-1"),
        ] {
            let script = decision_script(box_name, id, "denied");
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .current_dir(&*dir)
                .output()
                .expect("sh");
            assert!(
                !marker.exists(),
                "a value a box wrote ran a command while the script was being read:\n{script}\n{out:?}"
            );
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
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

    #[test]
    fn a_token_file_is_named_so_a_repo_cannot_address_another_boxs_file() {
        // `token_file` builds its path under `fleet::box_state_root`, which is `config::skein_home`
        // — so unpinned this containment test was naming a file inside the owner's live
        // `~/.skein/boxes/web-main`, which is a real box on this machine. It only ever passed
        // because a neighbour had left `$SKEIN_HOME` set (SKEIN-646); the guard added by SKEIN-626
        // refuses it alone. Nothing here writes, but a test about where a path may not point is a
        // poor place to be pointing at live state.
        let (_lock, _home, _env) = fresh_home();
        let p = token_file("web-main", "acme/thing");
        assert!(
            p.ends_with("acme%2Fthing"),
            "the slash is encoded, or the slug becomes a directory: {p}"
        );
        assert!(p.contains("web-main"), "{p}");
    }

    /// A fresh `$SKEIN_HOME`, plus the guards that put it back.
    ///
    /// The pins come **last** so they drop **first**: bindings from one `let` are dropped in
    /// reverse, so `$SKEIN_HOME` stops naming the temp directory before the temp directory is
    /// removed, and it is put back on the unwinding path as well as the passing one.
    fn fresh_home() -> (
        crate::testutil::EnvGuard,
        crate::testutil::TempDir,
        crate::testutil::EnvPins,
    ) {
        let lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        (lock, home, env)
    }

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
        set_credential_token("mine", "github_pat_XYZ").unwrap();
        // A second, unrelated credential, so the fleet can still issue *something* after the first
        // is forgotten. Without it `can_issue_write_tokens` goes false, the box stops being scoped,
        // and the token is taken by the un-scoping path instead — which is a different fix, tested
        // below. This one has to fail to mint while the box is still very much scoped.
        set_write_credential("other", "elsewhere", &["b/two".into()]).unwrap();
        set_credential_token("other", "github_pat_OTHER").unwrap();

        let path = box_holding("worker", "a/one");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "github_pat_XYZ");

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
        set_credential_token("mine", "github_pat_XYZ").unwrap();
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
        set_credential_token("mine", "github_pat_XYZ").unwrap();
        let path = box_holding("worker", "a/one");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a placed token was {mode:o}");

        // And the same rule for the credential store the host keeps.
        let stored = crate::config::skein_home().join("github-pats").join("mine");
        let mode = std::fs::metadata(&stored).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the stored token was {mode:o}");
        drop(home);
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
    fn a_stored_token_is_preferred_over_the_app_for_the_repos_it_covers() {
        // The precedence is the feature, not an optimisation. Someone stores a PAT precisely because
        // they did not want an App reaching across their account — deferring to the App would
        // override that choice with the exact thing it was made to avoid.
        let (_lock, _home, _env) = fresh_home();
        set_write_credential("mine", "my one repo", &["a/one".into()]).unwrap();
        set_credential_token("mine", "github_pat_XYZ").unwrap();

        let (found, token) = credential_for("a/one").expect("a stored token covers a/one");
        assert_eq!(token.expose(), "github_pat_XYZ");
        assert_eq!(found.label, "my one repo");
        assert_eq!(
            mint_token("a/one").unwrap().expose(),
            "github_pat_XYZ",
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
        set_credential_token("wide", "t").unwrap();

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
        set_credential_token("half", "t").unwrap();
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
            assert!(set_credential_token(bad, "t").is_err(), "{bad:?}");
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
        set_credential_token("gone", "t").unwrap();
        remove_write_credential("gone").unwrap();
        assert!(write_credentials().is_empty());
        assert!(!credential_has_token("gone"), "the token file outlived it");
        assert!(credential_for("a/one").is_none());
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
        set_credential_token("alpha", "ghp_alpha").unwrap();
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

    /// **Approving one box's access is not how a fleet loses every grant it has given.**
    ///
    /// The same shape one file over (SKEIN-359): `record` replaces the entry for one (box, repo)
    /// pair and writes the whole list back, so a grants file read as empty turned an approval into
    /// a mass revocation — and answered Ok, so the cockpit showed the approval as recorded. A
    /// revoke is the same write and the same loss, which is why both are asserted here: a revoke
    /// already fails closed, and answering Ok for one that destroyed the rest is the shape nobody
    /// would go looking for.
    #[test]
    fn a_grant_list_skein_cannot_read_is_never_written_over_by_an_approval() {
        let (_lock, _home, _env) = fresh_home();
        let asked = |box_name: &str, repo: &str| Request {
            id: "20260826-101010-1".into(),
            box_name: box_name.into(),
            repo: repo.into(),
            reason: "to push".into(),
            asked: chrono::Utc::now().to_rfc3339(),
            state: "pending".into(),
            decided: String::new(),
        };
        record(&asked("web-main", "acme/web"), Some(4)).unwrap();
        record(&asked("api-main", "acme/api"), None).unwrap();
        assert_eq!(grants().len(), 2);

        let path = grants_path();
        std::fs::write(&path, b"").unwrap();
        assert!(
            grants().is_empty(),
            "an unreadable grants file must read as no access, never as access"
        );

        let why = record(&asked("web-main", "acme/third"), Some(4))
            .expect_err("an approval over an unreadable grants file reported success");
        assert!(
            why.contains("cannot read") && why.contains("grants"),
            "the refusal has to name the file and say it could not be read: {why}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"",
            "the unreadable grants file was replaced by an approval"
        );

        assert!(
            revoke("web-main", "acme/web").is_err(),
            "withdrawing one grant is not how the others are withdrawn"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"",
            "the unreadable grants file was replaced by a revoke"
        );
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

    // ─────────────── who answers a request (SKEIN-940) ───────────────

    /// A fresh `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` together. Both, because [`decide`]'s courtesy
    /// write names the queue under the fleet root, and an unpinned fleet root is `/boxes`, the
    /// owner's live queue.
    fn fresh_fleet() -> (
        crate::testutil::EnvGuard,
        crate::testutil::TempDir,
        crate::testutil::EnvPins,
    ) {
        let lock = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", dir.join("home"));
        env.set("SKEIN_FLEET_ROOT", dir.join("fleet"));
        (lock, dir, env)
    }

    /// A request as a box files it, with whatever state the box chose to write into it.
    fn asked_by(box_name: &str, id: &str, repo: &str, state: &str) -> Request {
        Request {
            id: id.into(),
            box_name: box_name.into(),
            repo: repo.into(),
            reason: "one change in the sibling".into(),
            asked: "2026-09-22T12:00:00Z".into(),
            decided: match state {
                "pending" => String::new(),
                _ => "2026-09-22T12:00:01Z".into(),
            },
            state: state.into(),
        }
    }

    /// **A box cannot answer its own request by writing the answer into it** (SKEIN-940).
    ///
    /// Every field of a queue file is the asking box's word, `state` included, and the cockpit
    /// offers Grant and Deny only on a row whose state is `pending`. So a box that wrote `granted`
    /// was drawn as answered, with nothing to press, and its owner was never asked. Nobody has
    /// answered either of these, so both must come back `pending` with no decision time, whatever
    /// the file said — while the ask itself, the reason and when, is still the box's to state.
    ///
    /// **What would make this fail**: [`decided_over`] falling back to the box's own request
    /// (`None => asked`), which is the shape `list` had before this.
    #[test]
    fn a_request_the_host_never_answered_is_pending_whatever_its_own_file_says() {
        let (_lock, _dir, _env) = fresh_fleet();
        let shown = decided_over(vec![
            asked_by("web-main", "20260922-120000-1", "acme/thing", "granted"),
            asked_by("web-main", "20260922-120000-2", "acme/thing", "denied"),
        ]);
        for r in &shown {
            assert_eq!(
                r.state, "pending",
                "a request nobody answered is shown as {:?} because its own file says so — the \
                 cockpit draws no Grant button on it and its owner is never asked",
                r.state
            );
            assert_eq!(
                r.decided, "",
                "the box's own decision time survived onto a request nobody decided"
            );
            assert_eq!(
                r.reason, "one change in the sibling",
                "the ask itself was lost"
            );
        }
    }

    /// **The owner's answer is the one shown, and a box cannot take it back or re-describe it.**
    ///
    /// Both answers, because a denial had no record anywhere but the box's own file: with only the
    /// grant record on the host, a denied request would come back as waiting, and a box could have
    /// written `pending` over a denial to be asked again. After deciding, the box rewrites both of
    /// its files — `pending` again, and a different repository.
    ///
    /// **What would make this fail**: [`decide`] not writing [`decision_path`] (both rows come back
    /// `pending`, first assertion), or [`decided_over`] keeping the box's `repo` (the row describes a
    /// repository nobody granted, third assertion).
    #[test]
    fn the_owners_answer_is_recorded_where_the_box_cannot_take_it_back() {
        let (_lock, _dir, _env) = fresh_fleet();
        // The courtesy write into the box's file is a crossing; it is best-effort and nothing here
        // is about it.
        let _crossing = crate::place::seam::doing_nothing();
        let denied = asked_by("web-main", "20260922-120000-3", "acme/thing", "pending");
        let granted = asked_by("web-main", "20260922-120000-4", "acme/other", "pending");
        decide("example-fleet", &denied, false, None).expect("deny");
        decide("example-fleet", &granted, true, Some(24)).expect("grant");

        let rewritten = [&denied, &granted].map(|r| Request {
            state: "pending".into(),
            decided: String::new(),
            repo: "acme/crown-jewels".into(),
            ..r.clone()
        });
        let shown = decided_over(rewritten.to_vec());
        assert_eq!(
            [shown[0].state.as_str(), shown[1].state.as_str()],
            ["denied", "granted"],
            "the box wrote `pending` over its owner's answers and was believed"
        );
        assert!(
            shown.iter().all(|r| !r.decided.is_empty()),
            "an answered request lost when it was answered: {shown:?}"
        );
        assert_eq!(
            [shown[0].repo.as_str(), shown[1].repo.as_str()],
            ["acme/thing", "acme/other"],
            "the row names the repository the box wrote afterwards, not the one its owner decided"
        );
        assert_eq!(
            grants().len(),
            1,
            "only the approval is a grant; a denial must not become one"
        );
    }

    /// **An answer to one box is not an answer to another box that files the same id.**
    ///
    /// Every box can read every other box's queue, so a box can file, in its own drop-box, the id
    /// it watched a neighbour's request be answered under. The answer is keyed by the box the
    /// request came from, which is the directory it was read from and not a field anyone wrote.
    ///
    /// **What would make this fail**: [`decision_path`] dropping the box from the path, so one
    /// id's answer is shown on every box's request that uses it.
    #[test]
    fn an_answer_to_one_box_is_not_an_answer_to_another_box_using_the_same_id() {
        let (_lock, _dir, _env) = fresh_fleet();
        let _crossing = crate::place::seam::doing_nothing();
        let id = "20260922-120000-5";
        decide(
            "example-fleet",
            &asked_by("other-main", id, "acme/thing", "pending"),
            true,
            Some(24),
        )
        .expect("grant the neighbour");
        let shown = decided_over(vec![asked_by("web-main", id, "acme/thing", "granted")]);
        assert_eq!(
            shown[0].state, "pending",
            "a copy of a neighbour's id was shown with the neighbour's answer"
        );
    }

    /// **A request is answered once, and a second answer changes nothing** (SKEIN-1034).
    ///
    /// Granted, then denied: the denial is refused, and the grant, its expiry and the recorded
    /// answer are byte-for-byte what the first answer left. Then granted again, for the fresh-expiry
    /// half of the bug. Then a request whose answer is on disk and will not parse: refused too, and
    /// nothing is granted or written.
    ///
    /// **What would make this fail**: drop the guard in [`decide`] (the denial goes through — first
    /// assertion — and overwrites the record while the grant stays live); or read the guard through
    /// `.ok().flatten()`, as [`decision`] does, so an unreadable answer passes as none (the last
    /// three assertions).
    #[test]
    fn a_request_is_answered_once_and_a_second_answer_changes_nothing() {
        let (_lock, _dir, _env) = fresh_fleet();
        let _crossing = crate::place::seam::doing_nothing();
        let asked = asked_by("web-main", "20260922-120000-6", "acme/thing", "pending");
        decide("example-fleet", &asked, true, Some(24)).expect("the first answer");
        let path = decision_path(&asked.box_name, &asked.id).expect("a nameable request");
        let (record_before, grants_before) = (std::fs::read(&path).expect("the answer"), grants());
        assert_eq!(
            grants_before.len(),
            1,
            "the first answer did not grant, so this proves nothing"
        );

        let denied = decide("example-fleet", &asked, false, None);
        assert!(
            denied.as_ref().is_err_and(|e| e.contains("already granted")),
            "a denial after a grant was accepted ({denied:?}) — the row now reads denied while the \
             grant stays live and its token keeps being minted"
        );
        let again = decide("example-fleet", &asked, true, Some(1));
        assert!(
            again.is_err(),
            "a second grant was accepted, re-recording the grant with an expiry nobody chose last"
        );
        assert_eq!(
            std::fs::read(&path).expect("the answer"),
            record_before,
            "a refused answer still rewrote the recorded one"
        );
        assert_eq!(
            grants(),
            grants_before,
            "a refused answer still changed the grant"
        );

        // An answer on disk that will not parse — the half-written file a crash leaves.
        let torn = asked_by("web-main", "20260922-120000-7", "acme/thing", "pending");
        let torn_path = decision_path(&torn.box_name, &torn.id).expect("a nameable request");
        std::fs::write(&torn_path, b"").expect("plant an unreadable answer");
        let answered = decide("example-fleet", &torn, true, None);
        assert!(
            answered
                .as_ref()
                .is_err_and(|e| e.contains("cannot read the answer")),
            "an answer skein cannot read was taken for none, and the request was answered again: \
             {answered:?}"
        );
        assert_eq!(
            std::fs::read(&torn_path).expect("the planted file"),
            b"",
            "the unreadable answer was written over — it was the only record of the first one"
        );
        assert_eq!(
            grants(),
            grants_before,
            "a request whose answer skein could not read was granted anyway"
        );
    }
}
