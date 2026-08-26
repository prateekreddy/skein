//! System packages for the whole fleet: what a box asked for, and what its owner approved.
//!
//! A box cannot install one and never will be able to — it is a user namespace mapping a single
//! uid, so `sudo` there is unfixable rather than unconfigured (`box-session.sh` says so at length).
//! But the *sandbox* around those boxes has a working root, and `ensure_substrate` already uses it
//! to install tmux and jq. So the capability was never missing; only a way to ask for it was, and
//! the shim inside a box has been telling agents to "ask for it in the fleet" with nowhere to ask.
//!
//! This is that place. A box files a request, its owner approves it in the cockpit, and the install
//! runs once for every box in the sandbox.
//!
//! **What this gate is, precisely.** It is a chokepoint and an audit trail. It is not a wall between
//! boxes, and this fleet's owner has deliberately not asked for one: boxes share a uid, so any box
//! can read another's files whatever this module does. Calling that containment would be a lie
//! someone later relies on.
//!
//! It *used* to be weaker still. This paragraph read "any box can already read the agent token out
//! of the fleet root and run what it likes at fleet scope" — true when written, and the reason the
//! gate could only ever be an audit trail: a box that wanted a package did not have to ask, it could
//! `cat` the token and run `sudo apt-get` itself. `box-session.sh` now binds an empty file over that
//! token inside every box, so the ask is the only way in and the record is complete.
//!
//! What remains, and is worth naming rather than implying otherwise: the queue is a directory any
//! box can write, so a request proves *that* it was filed, never *by whom*. A box can file one
//! naming a different box. The approval is a person reading it, which is why the name on a request
//! is context for that person and never an input to a decision made here.
//!
//! **Where the queue lives, and why not the shared store.** The `.claude` store is per-repo; one
//! sandbox holds boxes from several repos. Scoping a fleet-wide decision to whichever repo asked
//! first would hide it from every other repo it also changes. It also keeps runtime state out of the
//! shared store, which is a standing rule of this project.
//!
//! **Why the validation happens twice.** The shim validates package names before filing, and this
//! module validates them again before installing. That is not belt-and-braces for its own sake: the
//! queue is a directory in the fleet root that any box can write to directly, so a request that
//! reached disk proves nothing about what produced it. The check that matters is the one on this
//! side of the wire, immediately before the name is spliced into a command running as root.

use crate::place::own_sandbox;
use crate::util::sh_quote;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// One box's ask, and the fleet's answer to it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: String,
    /// The box that asked. `box` is a Rust keyword, so the field is renamed rather than the JSON.
    #[serde(default, rename = "box")]
    pub box_name: String,
    /// `apt` or `npm`. Anything else is refused at [`Request::problem`].
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub asked: String,
    /// `pending` → `approved` → `installed` | `failed`, or `denied`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub decided: String,
    /// Whether approving this also records it, so a rebuilt sandbox reinstalls it unprompted.
    /// Defaults true: a fleet rebuilt without its approved packages is a fleet whose boxes silently
    /// lose a toolchain their owner already said yes to.
    #[serde(default = "yes")]
    pub remember: bool,
    /// The tail of apt's or npm's output when the install failed. The only thing that ever explains
    /// *why*, so it is carried back to the box rather than left in a log inside the sandbox.
    #[serde(default)]
    pub log: String,
}

fn yes() -> bool {
    true
}

/// Every package skein is willing to name on a privileged command line.
///
/// **This is an argv-injection guard and never a privilege guard, and the difference matters.** Any
/// approved apt or npm package is arbitrary root code by design: maintainer scripts and lifecycle
/// scripts run as root, at fleet scope, and — once remembered — at every future launch. Nothing
/// here makes a package safe. It makes the *shape* of a name safe, so that what runs is a package
/// install and not an option skein never meant to pass.
///
/// A whitelist of shapes rather than a blacklist of tricks, and it is **per kind**, which it was
/// not. It admitted `/` for every kind, so `apt-get install ./x.deb` installed a file out of the
/// box's own tree and `npm install -g /path` ran that path's lifecycle scripts — both as root, both
/// then written into the manifest and replayed at every launch. `/` is legitimate in exactly one
/// place, an npm scope, so that is the only place it is allowed.
fn package_is_nameable(kind: &str, p: &str) -> bool {
    if p.is_empty() || p.starts_with('-') || p.starts_with('.') || p.contains("..") {
        return false;
    }
    let body = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
    };
    match kind {
        // `@scope/name`, and nothing else with a slash in it. Not "at most one slash": the scope
        // has to be a scope, or `x/../../y` is one slash and a path.
        "npm" => match p.strip_prefix('@') {
            Some(scoped) => match scoped.split_once('/') {
                Some((scope, name)) => body(scope) && body(name),
                None => false,
            },
            None => body(p),
        },
        _ => body(p),
    }
}

/// Every request id skein is willing to act on.
///
/// An id is three things and used to be checked as one of them. It is a **filename in the queue**
/// — `<queue>/<id>.json`, inside the sandbox. It is a **filename on the host** — [`decision_path`]
/// puts it under `$SKEIN_HOME/substrate`. And it is a **shell word**, because [`decision_script`]
/// and [`log_script`] name the queue file in scripts the host runs. The old check — not empty, no
/// slash, no `..` — is a path check, and it left `;`, `$`, a backtick, a pipe, an ampersand, a
/// space and a newline all legal in the one field on a request that a box writes and nothing else
/// constrains.
///
/// A whitelist of shapes, like [`package_is_nameable`] above it and for the same stated reason: a
/// blacklist is a list of the attacks somebody thought of. Every id this fleet files is
/// `date -u +%Y%m%d-%H%M%S-$$` (`box-session.sh`, `ask`), which this admits with room to spare.
///
/// **Its own copy rather than [`crate::gitgate`]'s**, deliberately. The two queues are separate
/// gates with separate id spaces, and a shared check would mean one of them could not tighten
/// without the other. It is also not what makes the scripts safe — they quote — which is why
/// having two is cheap: neither is load-bearing alone.
fn id_is_nameable(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

impl Request {
    /// Why this request must not be installed, or `None` if it may be.
    ///
    /// Returned as a reason rather than a bool because every caller has to *show* it: a request the
    /// cockpit silently drops looks to the box that filed it exactly like one nobody got around to.
    pub fn problem(&self) -> Option<String> {
        if self.kind != "apt" && self.kind != "npm" {
            return Some(format!("unknown package source {:?}", self.kind));
        }
        if self.packages.is_empty() {
            return Some("no packages named".into());
        }
        if let Some(bad) = self
            .packages
            .iter()
            .find(|p| !package_is_nameable(&self.kind, p))
        {
            return Some(format!("{bad:?} is not a package name"));
        }
        if !id_is_nameable(&self.id) {
            return Some(format!("unusable request id {:?}", self.id));
        }
        None
    }

    pub fn is_pending(&self) -> bool {
        self.state == "pending"
    }
}

/// Where requests and the approved-package manifest live **inside the sandbox**.
pub fn substrate_dir() -> String {
    format!("{}/.skein/substrate", crate::fleet::fleet_root())
}

fn requests_dir() -> String {
    format!("{}/requests", substrate_dir())
}

/// The record of what this fleet's owner has approved — on the **host**, beside `repos.json`.
///
/// Not in the fleet root with the queue, and the difference is the entire point of recording. The
/// fleet root lives inside the sandbox and dies with it, so a manifest kept there would be lost by
/// exactly the event it exists to survive: a rebuild would come back without the packages its owner
/// had already said yes to, and every box would start asking again. The queue stays in the sandbox
/// because boxes must be able to write it; the record does not, because only the host reads it.
fn manifest_path() -> std::path::PathBuf {
    crate::config::skein_home().join("substrate.json")
}

/// Parse the array `jq -s` produces from the queue.
///
/// Malformed entries are dropped rather than failing the read: one unparseable file — a box writing
/// a request while this runs, a half-finished hand edit — must not blank the cockpit's whole list.
pub fn parse_requests(json: &str) -> Vec<Request> {
    let mut out: Vec<Request> = serde_json::from_str::<Vec<serde_json::Value>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Request>(v).ok())
        .filter(|r: &Request| !r.id.is_empty())
        .collect();
    // Newest last, so the cockpit shows a stable order and the oldest ask is at the top.
    out.sort_by(|a, b| a.asked.cmp(&b.asked).then(a.id.cmp(&b.id)));
    out
}

/// Every request the fleet knows about, oldest first.
pub fn list(sandbox: &str) -> Result<Vec<Request>, String> {
    // `jq -s` over the glob, and the `[]` fallback for a queue that does not exist yet — which is
    // every fleet until the first ask, and must read as "nothing pending" rather than an error.
    let script = format!(
        "d={}; ls \"$d\"/*.json >/dev/null 2>&1 || {{ echo '[]'; exit 0; }}; jq -s '.' \"$d\"/*.json 2>/dev/null || echo '[]'",
        sh_quote(&requests_dir())
    );
    let out = own_sandbox(sandbox).exec(&script, Duration::from_secs(30))?;
    // The queue says what was ASKED; the host says what was DECIDED, and where they disagree the
    // host wins outright. A box can rewrite its own file after approval — changing the packages,
    // or setting the state back to `pending` to be asked about again — and neither reaches here.
    Ok(decided_over(parse_requests(&out)))
}

/// The host's decision wins over the box's copy of it, request by request.
///
/// Separate and pure because it is the rule, not a detail of reading: the queue says what was
/// ASKED and the host says what was DECIDED, and a box that rewrites its own file after approval —
/// changing the packages, or setting the state back to `pending` to be asked again — must not
/// reach anything downstream of this.
fn decided_over(asked: Vec<Request>) -> Vec<Request> {
    asked
        .into_iter()
        .map(|asked| decision(&asked.id).unwrap_or(asked))
        .collect()
}

/// The script that records a decision against a request.
///
/// Split out from [`decide`] so the shape can be asserted without a sandbox: this writes into a
/// file a box can also write, so it must never do so by shelling a value in unquoted.
fn decision_script(id: &str, state: &str, remember: bool) -> String {
    format!(
        "f={}/{}.json; [ -f \"$f\" ] || {{ echo 'no such request' >&2; exit 1; }}; \
         t=$(mktemp \"$(dirname \"$f\")/.tmp.XXXXXX\") || exit 1; \
         jq --arg s {} --argjson r {} --arg d \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \
            '.state=$s | .remember=$r | .decided=$d' \"$f\" >\"$t\" \
           && mv -f \"$t\" \"$f\" || {{ rm -f \"$t\"; exit 1; }}",
        sh_quote(&requests_dir()),
        // Validated by the caller *and* quoted here — and the `.trim_matches('\'')` that used to
        // sit on this line took the quotes back off, so the sentence above about never shelling a
        // value in unquoted described the one line that did. The id is written by a box.
        //
        // Why the quoting is sound as written: `f=` takes a single word, and the shell joins
        // adjacent quoted and unquoted pieces into one. `'<dir>'/'<id>'.json` is that word, and its
        // only unquoted parts are this module's own literals — a slash and `.json`. Every byte
        // either value contributed sits inside single quotes, where nothing is expanded at all;
        // `sh_quote` is what keeps that true of an id containing a quote of its own.
        sh_quote(id),
        sh_quote(state),
        if remember { "true" } else { "false" },
    )
}

/// Where the fleet's decisions live: on the **host**, one file per request.
///
/// This is the artifact, and it is the whole fix. The queue in the sandbox is a box's *input* and
/// nothing else — a box can rewrite its own request at any moment, including while its owner is
/// reading it. So the decision is not written there and is never read back from there.
fn decision_path(id: &str) -> Option<std::path::PathBuf> {
    id_is_nameable(id).then(|| {
        crate::config::skein_home()
            .join("substrate")
            .join(format!("{id}.json"))
    })
}

/// The decision skein recorded for `id`, if it has made one.
pub fn decision(id: &str) -> Option<Request> {
    let body = std::fs::read_to_string(decision_path(id)?).ok()?;
    serde_json::from_str(&body).ok()
}

/// Approve or deny **the request the caller was looking at**. Approving does not install —
/// [`install`] does, so the cockpit can answer immediately and the wait for apt belongs to it.
///
/// `rendered` is not a convenience and not an optimisation: it is the request as it was shown to
/// the person who clicked, and it is what gets approved. The rule "the approving side writes the
/// artifact" is necessary and was never sufficient, because the artifact used to be written from a
/// **re-read by id** — which moved the window from approve-to-install (machine scale) out to
/// render-to-click (human scale, seconds to minutes). Larger, not smaller. So the approving side
/// keeps the bytes it rendered and acts on those, and does not re-open the file at all.
///
/// A decision is made **once**. A second call for the same id is refused rather than overwriting,
/// so a box cannot get a fresh decision by resurrecting a request under an id already answered.
pub fn decide(
    sandbox: &str,
    rendered: &Request,
    approve: bool,
    remember: bool,
) -> Result<Request, String> {
    if let Some(why) = rendered.problem() {
        return Err(format!("refusing to act on this request: {why}"));
    }
    let path = decision_path(&rendered.id).ok_or("unusable request id")?;
    if let Some(already) = decision(&rendered.id) {
        return Err(format!(
            "request {} is already {} — a decision is made once",
            rendered.id, already.state
        ));
    }
    let mut decided = rendered.clone();
    decided.state = if approve { "approved" } else { "denied" }.into();
    decided.remember = approve && remember;
    decided.decided = chrono::Utc::now().to_rfc3339();
    decided.log = String::new();
    write_decision(&path, &decided)?;

    // Courtesy only, and it must stay that way: the box reads its own file to learn what happened,
    // and skein never reads that answer back. Best-effort because a box that has deleted or locked
    // its request has told skein nothing skein needs — the decision above is the record.
    let _ = own_sandbox(sandbox).exec(
        &decision_script(&rendered.id, &decided.state, decided.remember),
        Duration::from_secs(30),
    );
    Ok(decided)
}

fn write_decision(path: &std::path::Path, req: &Request) -> Result<(), String> {
    let dir = path.parent().ok_or("no decisions directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::to_vec_pretty(req).map_err(|e| e.to_string())?;
    crate::util::write_atomic(path, dir, &body)
}

/// The install itself, as a script.
///
/// It reuses `ensure_substrate`'s hard-won discipline rather than a fresh `apt-get install`, because
/// every line of that discipline was paid for: `update` first (a fresh image's empty index reports a
/// real package as having "no installation candidate"), the dpkg-lock wait (a sandbox still running
/// its own first-boot apt refuses a second one), and keeping the log (without it a failure says
/// "missing" and nothing about the mirror, the lock, or the name).
pub fn install_script(kind: &str, packages: &[String]) -> String {
    let names = packages
        .iter()
        .map(|p| sh_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    if kind == "npm" {
        return format!(
            "log=$(mktemp); timeout 300 sudo npm install -g {names} >\"$log\" 2>&1; rc=$?; \
             tail -n 25 \"$log\"; rm -f \"$log\"; exit $rc"
        );
    }
    format!(
        "log=$(mktemp); waited=0; \
         while [ \"$waited\" -lt 120 ]; do \
           if sudo fuser /var/lib/dpkg/lock-frontend /var/lib/apt/lists/lock >/dev/null 2>&1; then \
             sleep 3; waited=$((waited + 3)); else break; fi; \
         done; \
         {{ timeout 180 sudo apt-get update -qq; \
            timeout 600 sudo apt-get install -y -qq {names}; }} >\"$log\" 2>&1; rc=$?; \
         tail -n 25 \"$log\"; rm -f \"$log\"; exit $rc"
    )
}

/// The script that carries an install's outcome back to the box's own copy of its request.
///
/// Split out for the same reason as [`decision_script`], and it was the one that was not: this is
/// the second script in this module that writes into a file any box can write, so it had the same
/// unquoted `id` and no test could see it because it had no name. A seam is what makes a shape
/// assertable, and a shape nobody can assert is one nobody checks.
fn log_script(id: &str, state: &str, tail: &str) -> String {
    format!(
        "f={}/{}.json; [ -f \"$f\" ] || exit 0; t=$(mktemp \"$(dirname \"$f\")/.tmp.XXXXXX\") || exit 1; \
         jq --arg s {} --arg l {} '.state=$s | .log=$l' \"$f\" >\"$t\" && mv -f \"$t\" \"$f\" || {{ rm -f \"$t\"; exit 1; }}",
        sh_quote(&requests_dir()),
        // Quoted, and it stays quoted — see [`decision_script`] for why `'<dir>'/'<id>'.json` is
        // one shell word with nothing expandable in it.
        sh_quote(id),
        sh_quote(state),
        sh_quote(tail),
    )
}

/// Install an approved request, then record the outcome on it.
///
/// Slow by nature — apt on a cold index is minutes — so callers run it off the request thread.
pub fn install(sandbox: &str, id: &str) -> Result<Request, String> {
    // The artifact, never the queue. This was the third id-keyed read of a box-writable file, and
    // closing render-to-click while leaving click-to-install open would have moved the window
    // rather than shut it: a box whose request was approved could still swap the package list
    // before apt saw it.
    let req = decision(id).ok_or_else(|| format!("skein has no decision recorded for {id}"))?;
    // Checked again even though `decide` checked it: this is the last thing between a name and a
    // root command line, and a decision file on the host can be hand-edited.
    if let Some(why) = req.problem() {
        return Err(format!("refusing to install: {why}"));
    }
    if req.state != "approved" {
        return Err(format!("request {id} is {}, not approved", req.state));
    }

    let outcome = own_sandbox(sandbox).exec(
        &install_script(&req.kind, &req.packages),
        Duration::from_secs(900),
    );
    let (state, log) = match &outcome {
        Ok(out) => ("installed", out.clone()),
        Err(e) => ("failed", e.clone()),
    };
    let tail: String = log
        .chars()
        .rev()
        .take(4000)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    // The outcome lands on the ARTIFACT first, because that is skein's record.
    let mut done = req.clone();
    done.state = state.into();
    done.log = tail.clone();
    if let Some(path) = decision_path(id) {
        write_decision(&path, &done)?;
    }
    // Then the box's own copy, so an agent can read why its install failed. Courtesy, best-effort,
    // and never read back.
    let _ = own_sandbox(sandbox).exec(&log_script(id, state, &tail), Duration::from_secs(30));

    // Recorded only once it actually installed. Recording on approval would put a package that
    // apt could not find into every future launch, where it fails again and takes the rest of the
    // substrate install down with it.
    if state == "installed" && req.remember {
        record(&req)?;
    }
    outcome
        .map(|_| done.clone())
        .map_err(|e| format!("{e}\n{}", done.log))
}

/// Everything the fleet has been asked for, for the cockpit.
pub fn fleet_requests() -> Vec<Request> {
    // An unreachable sandbox reads as an empty queue rather than an error: this is polled beside
    // the board, and a fleet that is down should not paint the panel red about packages.
    list(&crate::place::fleet_sandbox()).unwrap_or_default()
}

/// Approve a request and install it, or deny it. Returns once the *decision* is recorded; the
/// install that follows an approval is left to the caller to run, because apt takes minutes and
/// nothing about the answer should wait for it.
pub fn fleet_decide(rendered: &Request, approve: bool, remember: bool) -> Result<Request, String> {
    decide(&crate::place::fleet_sandbox(), rendered, approve, remember)
}

/// Run the install for an already-approved request.
pub fn fleet_install(id: &str) -> Result<Request, String> {
    install(&crate::place::fleet_sandbox(), id)
}

/// The approved-package manifest: what a rebuilt sandbox must reinstall.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub apt: Vec<String>,
    #[serde(default)]
    pub npm: Vec<String>,
}

/// Fold a request into the manifest, keeping it sorted and duplicate-free.
///
/// Pure, because this is the function whose output ends up on a root command line at every future
/// fleet launch — the one place where a bad name would persist rather than fail once.
pub fn merged(mut m: Manifest, req: &Request) -> Manifest {
    let list = if req.kind == "npm" {
        &mut m.npm
    } else {
        &mut m.apt
    };
    for p in &req.packages {
        if package_is_nameable(&req.kind, p) && !list.contains(p) {
            list.push(p.clone());
        }
    }
    list.sort();
    m
}

/// Read the manifest of packages this fleet's owner has approved.
///
/// A missing or unreadable file is an empty manifest, never an error: this is consulted on the
/// launch path, and a fleet that refuses to start because nobody has ever approved a package would
/// be a spectacular way to fail closed.
pub fn manifest() -> Manifest {
    std::fs::read_to_string(manifest_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Add an approved request to the manifest.
fn record(req: &Request) -> Result<(), String> {
    // Under the lock. Two approvals landing together is not exotic here — the cockpit installs off
    // the request thread, so two boxes answered in quick succession finish whenever apt does — and
    // a lost update means a package its owner approved is missing from every future launch.
    crate::util::update_json(&manifest_path(), |m: &mut Manifest| {
        *m = merged(std::mem::take(m), req);
        Ok(())
    })
}

/// The packages a rebuilt sandbox must reinstall, as one `ensure_substrate` can splice in.
///
/// Filtered through [`package_is_nameable`] on the way out as well as on the way in. The manifest is
/// an ordinary file on the host: it can be hand-edited, and it is replayed onto a root command line
/// at every launch, which makes it the one place a bad name would persist rather than fail once.
pub fn approved_packages() -> (Vec<String>, Vec<String>) {
    let m = manifest();
    let keep = |kind: &str, v: Vec<String>| -> Vec<String> {
        v.into_iter()
            .filter(|p| package_is_nameable(kind, p))
            .collect()
    };
    (keep("apt", m.apt), keep("npm", m.npm))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(kind: &str, packages: &[&str]) -> Request {
        Request {
            id: "20260812-101010-1".into(),
            box_name: "web-main".into(),
            kind: kind.into(),
            packages: packages.iter().map(|s| s.to_string()).collect(),
            state: "pending".into(),
            remember: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_name_that_apt_would_read_as_an_option_is_not_a_package() {
        // The whole reason the check exists: `--force-yes` in a package list is an argument to the
        // privileged command, not a thing to install.
        assert!(req("apt", &["--allow-downgrades"]).problem().is_some());
        assert!(req("apt", &["-o"]).problem().is_some());
    }

    #[test]
    fn a_package_name_cannot_climb_out_of_its_name() {
        assert!(req("apt", &["../../etc/shadow"]).problem().is_some());
        assert!(req("npm", &["a..b"]).problem().is_some());
    }

    #[test]
    fn the_names_real_packages_actually_have_are_allowed() {
        // Scoped npm packages and apt's own punctuation must survive the filter, or the check has
        // simply moved the failure rather than prevented one.
        assert!(req("npm", &["@anthropic-ai/claude-code"])
            .problem()
            .is_none());
        assert!(req("apt", &["libatk1.0-0", "g++", "python3-dev"])
            .problem()
            .is_none());
    }

    /// A slash is a path, except in the one place npm makes it a scope.
    ///
    /// The filter used to admit `/` for every kind, which is not a near miss: `apt-get install
    /// ./x.deb` installs a file out of the box's own tree and `npm install -g /path` runs that
    /// path's lifecycle scripts — both as root, for the whole fleet, and then written into the
    /// manifest and replayed at every launch. Neither is an exploit of a bug; both are apt and npm
    /// doing exactly what they are for.
    /// What was on screen is what gets approved, and what gets approved is what gets installed.
    ///
    /// The rule "the approving side writes the artifact" was already implemented and was still
    /// wrong, because the artifact was written from a **re-read by id** at click time. That does
    /// not close the window, it moves it: from approve-to-install, which is machine scale, out to
    /// render-to-click, which is a person reading a card — seconds to minutes. This is the test
    /// that the bytes decided on are the bytes rendered.
    #[test]
    fn the_packages_approved_are_the_ones_that_were_on_screen() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        let rendered = req("apt", &["libnss3"]);
        // No sandbox here, so the courtesy write-back into the box's file fails and is ignored —
        // which is itself the point: the decision does not depend on the box's file at all.
        let decided = decide("no-such-sandbox", &rendered, true, true).expect("decided");
        assert_eq!(decided.state, "approved");
        assert_eq!(decided.packages, vec!["libnss3".to_string()]);

        // The box now rewrites its own request to name something else. Everything downstream still
        // sees what was approved.
        let swapped = Request {
            packages: vec!["evil".into()],
            state: "pending".into(),
            ..rendered.clone()
        };
        let shown = decided_over(vec![swapped]);
        assert_eq!(shown[0].packages, vec!["libnss3".to_string()]);
        assert_eq!(shown[0].state, "approved", "and it cannot ask again");

        // A decision is made once, so a resurrected request under a used id gets no second answer.
        let again = decide("no-such-sandbox", &rendered, true, true);
        assert!(
            again.unwrap_err().contains("already"),
            "a second decision on one id must be refused"
        );
    }

    /// The install reads the artifact, and there is nothing else for it to read.
    ///
    /// This was the third id-keyed read of a box-writable file. Closing render-to-click and leaving
    /// click-to-install open would have moved the window rather than shut it.
    #[test]
    fn an_install_with_no_recorded_decision_has_nothing_to_install() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let said = install("no-such-sandbox", "20260812-101010-1").unwrap_err();
        assert!(
            said.contains("no decision recorded"),
            "an install must not be able to fall back to the queue: {said}"
        );
    }

    #[test]
    fn a_slash_is_a_path_unless_it_is_an_npm_scope() {
        for bad in ["./x.deb", "/tmp/x.deb", "x/../../etc/y", ".hidden"] {
            assert!(
                req("apt", &[bad]).problem().is_some(),
                "{bad:?} reached a root command line"
            );
            assert!(
                req("npm", &[bad]).problem().is_some(),
                "{bad:?} reached a root command line"
            );
        }
        // The scope has to be a scope. "At most one slash" would admit `@a/../b`.
        assert!(req("npm", &["@scope/name"]).problem().is_none());
        assert!(req("npm", &["@scope/../name"]).problem().is_some());
        assert!(req("npm", &["scope/name"]).problem().is_some());
        // apt has no scopes, so it has no slashes either.
        assert!(req("apt", &["@scope/name"]).problem().is_some());
    }

    #[test]
    fn a_request_naming_nothing_is_refused() {
        assert!(req("apt", &[]).problem().is_some());
    }

    #[test]
    fn only_the_two_package_sources_skein_can_actually_drive_are_accepted() {
        assert!(req("curl", &["x"]).problem().is_some());
        assert!(req("apt", &["x"]).problem().is_none());
        assert!(req("npm", &["x"]).problem().is_none());
    }

    #[test]
    fn an_id_that_could_address_another_file_is_refused() {
        // The id becomes a path. `../` in it would let a request rewrite something outside the queue.
        let mut r = req("apt", &["tmux"]);
        r.id = "../../.skein/fleet-agent.token".into();
        assert!(r.problem().is_some());
    }

    #[test]
    fn one_unparseable_request_does_not_hide_the_others() {
        let json = r#"[{"id":"a","kind":"apt","packages":["tmux"],"asked":"2026-01-01"},
                       {"kind":"apt"},
                       "not an object",
                       {"id":"b","kind":"npm","packages":["x"],"asked":"2026-01-02"}]"#;
        let got = parse_requests(json);
        assert_eq!(got.len(), 2, "the two good entries survive: {got:?}");
        assert_eq!(got[0].id, "a", "oldest first");
        assert_eq!(got[1].id, "b");
    }

    #[test]
    fn an_empty_queue_reads_as_nothing_pending_rather_than_a_failure() {
        assert!(parse_requests("[]").is_empty());
        assert!(parse_requests("").is_empty());
    }

    #[test]
    fn remember_defaults_to_true_for_a_request_written_before_the_field_existed() {
        let r: Request =
            serde_json::from_str(r#"{"id":"a","kind":"apt","packages":["tmux"]}"#).unwrap();
        assert!(
            r.remember,
            "a fleet rebuild must not silently drop an approved package"
        );
    }

    #[test]
    fn the_manifest_grows_without_duplicating_or_reordering() {
        let m = merged(Manifest::default(), &req("apt", &["tmux", "jq"]));
        assert_eq!(m.apt, vec!["jq", "tmux"]);
        let m = merged(m, &req("apt", &["jq", "ripgrep"]));
        assert_eq!(
            m.apt,
            vec!["jq", "ripgrep", "tmux"],
            "jq is not added twice"
        );
        assert!(
            m.npm.is_empty(),
            "an apt request never lands in the npm list"
        );
    }

    #[test]
    fn a_bad_name_cannot_reach_the_manifest_even_if_it_reached_the_queue() {
        // The manifest is replayed at every future launch, so a name that slipped in would be a
        // permanent one. This is the last filter before that.
        let m = merged(Manifest::default(), &req("apt", &["--evil", "tmux"]));
        assert_eq!(m.apt, vec!["tmux"]);
    }

    #[test]
    fn every_package_reaches_the_install_command_quoted() {
        let s = install_script("apt", &["libnss3".into(), "g++".into()]);
        assert!(s.contains("'libnss3'") && s.contains("'g++'"), "{s}");
        assert!(
            s.contains("apt-get update"),
            "the index is refreshed first: {s}"
        );
        assert!(
            s.contains("lock-frontend"),
            "the dpkg lock is waited out: {s}"
        );
    }

    #[test]
    fn npm_installs_globally_or_it_has_not_installed_for_every_box() {
        let s = install_script("npm", &["prettier".into()]);
        assert!(s.contains("npm install -g"), "{s}");
        assert!(
            !s.contains("apt-get"),
            "an npm request must not run apt: {s}"
        );
    }

    /// The field a **box** wrote reaches the shell inside quotes.
    ///
    /// Named for the id, and that is why it exists beside the test below rather than inside it.
    /// That one is called "never splices a value in unquoted" and asserts it of `state` — a value
    /// this module picks from its own literals, which no box has ever been able to influence. It
    /// therefore passed for as long as `id` was spliced in bare — and `id` is the one field a box
    /// writes that reaches a shell at all. A test aimed at the safe field is not a weaker version
    /// of this one.
    #[test]
    fn a_request_id_reaches_the_decision_script_only_inside_its_own_quotes() {
        let id = "20260812-1-1'; touch /tmp/skein-pwned; :'$(id)`id`";
        for s in [
            decision_script(id, "approved", true),
            log_script(id, "installed", "apt said something"),
        ] {
            let quoted = sh_quote(id);
            assert!(
                s.contains(&format!("/{quoted}.json")),
                "the id is the filename and arrives as one single-quoted word: {s}"
            );
            // Everything the id contributed, taken away. What is left is this module's own script,
            // and none of the box's bytes may survive in it — a bare copy beside the quoted one is
            // the same hole with a witness.
            let rest = s.replace(&quoted, "");
            assert!(
                !rest.contains("touch") && !rest.contains("$(id)") && !rest.contains('`'),
                "nothing from the id appears outside the quotes it was wrapped in: {rest}"
            );
        }
    }

    /// The same property, settled by a shell instead of by reading one.
    ///
    /// `$(…)` needs no quote-balancing to work — it expands inside double quotes and inside none,
    /// and only single quotes stop it — so it is exactly what an unquoted splice costs, and a
    /// marker file is evidence rather than an argument. Nothing else in either script runs: the
    /// file the id names does not exist, so both give up on the line after the assignment.
    #[test]
    fn a_request_id_cannot_run_a_command_when_a_decision_or_a_log_is_written() {
        let dir = crate::testutil::tempdir();
        for which in ["decision", "log"] {
            let marker = dir.join(format!("pwned-{which}"));
            let id = format!("$(touch {})", marker.display());
            let script = match which {
                "decision" => decision_script(&id, "denied", false),
                _ => log_script(&id, "failed", "apt said something"),
            };
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .current_dir(&*dir)
                .output()
                .expect("sh");
            assert!(
                !marker.exists(),
                "the id ran a command while the {which} script was being read:\n{script}\n{out:?}"
            );
        }
    }

    /// An id is a filename and a shell word, so it is checked as both.
    ///
    /// It used to be checked as neither, quite: "not empty, no slash, no `..`" is a path check, and
    /// it leaves `;`, `$`, a backtick, a pipe, a space and a newline legal in a value that reaches
    /// `sh -c` in the sandbox and names a file in `$SKEIN_HOME` on the host.
    #[test]
    fn an_id_that_is_not_a_plain_name_is_refused() {
        let ok = req("apt", &["tmux"]);
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
            let mut r = ok.clone();
            r.id = bad.into();
            assert!(
                r.problem().is_some(),
                "{bad:?} is not a request id, and this is the check between it and a shell"
            );
            assert!(
                decision_path(bad).is_none(),
                "{bad:?} must not name a file in $SKEIN_HOME either"
            );
            // And the refusal is on the acting path, not only in the struct: `decide` returns here
            // before it reads anything and before it reaches the sandbox, which is why naming one
            // that does not exist proves the point rather than needing one that does.
            assert!(
                decide("no-such-sandbox", &r, false, false)
                    .unwrap_err()
                    .contains("refusing to act"),
                "{bad:?} must be refused by the call that runs the script, not merely describable"
            );
        }
    }

    #[test]
    fn a_decision_never_splices_a_value_into_the_script_unquoted() {
        let s = decision_script("20260812-1-1", "approved", true);
        assert!(s.contains("--arg s 'approved'"), "{s}");
        assert!(s.contains(".remember=$r"), "{s}");
        assert!(
            s.contains("mv -f"),
            "the request is replaced atomically: {s}"
        );
    }

    #[test]
    fn declining_to_remember_is_carried_into_the_decision() {
        assert!(decision_script("a", "approved", false).contains("--argjson r false"));
        assert!(decision_script("a", "approved", true).contains("--argjson r true"));
    }
}
