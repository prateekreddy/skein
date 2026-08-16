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
/// Deliberately a whitelist of shapes rather than a blacklist of tricks. apt and npm both accept
/// names far narrower than this, so nothing legitimate is lost, and the rejected set includes the
/// only two that matter: a leading `-` (which apt reads as an option, not a package) and any `..`
/// (a path escape, however it is spelled). An empty list is refused too — `apt-get install` with no
/// arguments is not a no-op worth running as root.
fn package_is_nameable(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('-')
        && !p.contains("..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+@/-".contains(c))
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
        if let Some(bad) = self.packages.iter().find(|p| !package_is_nameable(p)) {
            return Some(format!("{bad:?} is not a package name"));
        }
        if self.id.is_empty() || self.id.contains('/') || self.id.contains("..") {
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
    Ok(parse_requests(&out))
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
        // The id is validated by the caller, but quoted anyway: validation that is only correct
        // because of a check somewhere else is validation waiting to be moved.
        sh_quote(id).trim_matches('\''),
        sh_quote(state),
        if remember { "true" } else { "false" },
    )
}

/// Approve or deny a request. Approving does not install — [`install`] does, so the cockpit can
/// answer immediately and the wait for apt belongs to the caller.
pub fn decide(sandbox: &str, id: &str, approve: bool, remember: bool) -> Result<Request, String> {
    let mut found = list(sandbox)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no request {id}"))?;
    if let Some(why) = found.problem() {
        return Err(format!("refusing to act on this request: {why}"));
    }
    if !found.is_pending() {
        return Err(format!("request {id} is already {}", found.state));
    }
    let state = if approve { "approved" } else { "denied" };
    own_sandbox(sandbox).exec(
        &decision_script(id, state, approve && remember),
        Duration::from_secs(30),
    )?;
    found.state = state.into();
    found.remember = approve && remember;
    Ok(found)
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

/// Install an approved request, then record the outcome on it.
///
/// Slow by nature — apt on a cold index is minutes — so callers run it off the request thread.
pub fn install(sandbox: &str, id: &str) -> Result<Request, String> {
    let req = list(sandbox)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("no request {id}"))?;
    // Re-checked here and not merely at approval: approval and install are separate calls, and the
    // file between them is writable by every box in the fleet.
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
    let script = format!(
        "f={}/{}.json; [ -f \"$f\" ] || exit 0; t=$(mktemp \"$(dirname \"$f\")/.tmp.XXXXXX\") || exit 1; \
         jq --arg s {} --arg l {} '.state=$s | .log=$l' \"$f\" >\"$t\" && mv -f \"$t\" \"$f\" || {{ rm -f \"$t\"; exit 1; }}",
        sh_quote(&requests_dir()),
        sh_quote(id).trim_matches('\''),
        sh_quote(state),
        sh_quote(&tail),
    );
    let _ = own_sandbox(sandbox).exec(&script, Duration::from_secs(30));

    // Recorded only once it actually installed. Recording on approval would put a package that
    // apt could not find into every future launch, where it fails again and takes the rest of the
    // substrate install down with it.
    if state == "installed" && req.remember {
        record(&req)?;
    }
    let mut done = req;
    done.state = state.into();
    done.log = tail;
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
pub fn fleet_decide(id: &str, approve: bool, remember: bool) -> Result<Request, String> {
    decide(&crate::place::fleet_sandbox(), id, approve, remember)
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
        if package_is_nameable(p) && !list.contains(p) {
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
    let next = merged(manifest(), req);
    let body = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
    let home = crate::config::skein_home();
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    crate::util::write_atomic(&manifest_path(), &home, body.as_bytes())
}

/// The packages a rebuilt sandbox must reinstall, as one `ensure_substrate` can splice in.
///
/// Filtered through [`package_is_nameable`] on the way out as well as on the way in. The manifest is
/// an ordinary file on the host: it can be hand-edited, and it is replayed onto a root command line
/// at every launch, which makes it the one place a bad name would persist rather than fail once.
pub fn approved_packages() -> (Vec<String>, Vec<String>) {
    let m = manifest();
    let keep = |v: Vec<String>| -> Vec<String> {
        v.into_iter().filter(|p| package_is_nameable(p)).collect()
    };
    (keep(m.apt), keep(m.npm))
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
