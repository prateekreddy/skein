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
//! **Who a request is from is the directory it is in.** This paragraph used to end "a request
//! proves *that* it was filed, never *by whom*", and that was true of a queue that was one shared
//! read-write directory: any box could file under another box's name, and the only thing against it
//! was `$SKEIN_BOX` — an environment variable of a process the box owns. The queue is now
//! `requests/<box>/`, created by the launcher outside the namespace and bound read-write into that
//! box alone (`box-session.sh`, the drop-box block), which is the order architecture §8.4 asks for:
//! bind the artifact, make the request path per box, then unmask. So [`list`] takes the box from
//! the path and overwrites the field, and no value a box wrote decides who it is.
//!
//! What that does *not* buy, and is worth naming rather than implying otherwise: boxes share a uid
//! and the queue root is readable, so a box can still read every other box's asks. This gate is a
//! chokepoint and an audit trail, as the paragraph above says; the per-box path makes the trail
//! attributable, not the fleet compartmented.
//!
//! **Where the queue lives, and why not the shared store.** The `.claude` store is per-repo; one
//! sandbox holds boxes from several repos. Scoping a fleet-wide decision to whichever repo asked
//! first would hide it from every other repo it also changes. It also keeps runtime state out of the
//! shared store, which is a standing rule of this project.
//!
//! **Why the validation happens twice.** The shim validates package names before filing, and this
//! module validates them again before installing. That is not belt-and-braces for its own sake: the
//! queue is a directory in the fleet root that the box it belongs to writes directly, so the path
//! says which box a request is from and nothing at all about what produced it — an agent bypassing
//! the shim writes the same file. The check that matters is the one on this side of the wire,
//! immediately before the name is spliced into a command running as root.

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
/// An id is three things and used to be checked as one of them. It is a **filename in the queue** —
/// `<queue>/<box>/<id>.json`, inside the sandbox. It is a **filename on the host**, which
/// [`decision_path`] puts under `$SKEIN_HOME/substrate`. And it is a **shell word**, because
/// [`decision_script`] and [`log_script`] name the queue file in scripts the host runs. The box name
/// beside it is now two of those three — a directory in the queue and a word in the same scripts —
/// and is checked at [`Request::problem`] by [`crate::util::valid_name`], which is what
/// [`crate::gitgate`] already asked of it.
///
/// The old check — not empty, no slash, no `..` — is a path check, and it left `;`, `$`, a
/// backtick, a pipe, an ampersand, a space and a newline all legal in the one field on a request
/// that a box writes and nothing else constrains.
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
        // The box is a **path component** now that the queue is per box: [`decision_script`] and
        // [`log_script`] name `<queue>/<box>/<id>.json`, so it is checked exactly as the id is, and
        // for the second reason too — a request whose box the host cannot name is one it cannot say
        // who filed. That is what an empty name means here: [`list_script`] stamps the empty string
        // on a file found directly under the queue root, from before it was split per box. Such a
        // request is shown and refused, rather than dropped, because a person seeing an ask they
        // cannot act on is recoverable and an ask nobody sees is not.
        if !crate::util::valid_name(&self.box_name) {
            return Some(format!(
                "unusable box name {:?} — skein cannot tell which box filed this",
                self.box_name
            ));
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

/// The queue root, and **one directory below it is a box's identity**.
///
/// A request lives at `requests/<box>/<id>.json`. The launcher creates that directory outside the
/// box's mount namespace and binds it — alone — read-write into that box, so a request that landed
/// there landed there because that box wrote it. The root itself is read-only in every box, which
/// is what makes the one writable directory mean something. Everything else about a request is a
/// value the box chose, the `box` field included, which is why [`list`] overwrites that field from
/// the path rather than reading it.
fn requests_dir() -> String {
    format!("{}/requests", substrate_dir())
}

/// One box's drop-box in that queue — the directory the launcher makes and binds, and the only path
/// under `.skein` a box can write.
///
/// It exists because the drop-box has to be *removable* by the same spelling that reads it:
/// destroying a box leaves this directory behind, which `fleet::forget_departed_box` now sweeps
/// (SKEIN-736), and a `format!("{}/requests/{name}", …)` written over there would be a second
/// spelling of one location, free to drift from this one the day the layout changes.
pub(crate) fn box_requests_dir(box_name: &str) -> String {
    format!("{}/{box_name}", requests_dir())
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

/// The script that reads the queue, **stamping each request with the box whose directory it is in**.
///
/// Split out from [`list`] for the reason [`decision_script`] is: it is the only thing that decides
/// who a request is from, and a shape nobody can assert is one nobody checks. `tests` runs it
/// against a real directory tree.
///
/// `.box = <the directory>` overwrites whatever the file said, rather than comparing the two and
/// refusing a disagreement. Refusing would lose the ask — a person never sees a request that was
/// dropped — and buys nothing: there is no case where the field is right and the path is wrong.
///
/// A file directly under the queue root, from before it was split per box, is stamped with the
/// **empty** name. It is shown, because an ask that vanishes looks to the box that filed it exactly
/// like one nobody got to, and it cannot be acted on, because [`Request::problem`] refuses a box
/// name that is not a name and nothing can say now which box wrote it.
fn list_script() -> String {
    // `jq -n` with `inputs` rather than `jq -s`: `input_filename` tracks the file each value came
    // from only while they are being pulled one at a time, and that filename is the whole point.
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

/// Every request the fleet knows about, oldest first.
pub fn list(sandbox: &str) -> Result<Vec<Request>, String> {
    let out = own_sandbox(sandbox).exec(&list_script(), Duration::from_secs(30))?;
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
///
/// `box_name` is a path component now that the queue is per box, so it is quoted for the same
/// reason the id is — and refused before it gets here, by [`Request::problem`], for the same reason
/// too. It comes from the artifact, which took it from the directory the request was read out of.
fn decision_script(box_name: &str, id: &str, state: &str, remember: bool) -> String {
    format!(
        "f={}/{}/{}.json; [ -f \"$f\" ] || {{ echo 'no such request' >&2; exit 1; }}; \
         t=$(mktemp \"$(dirname \"$f\")/.tmp.XXXXXX\") || exit 1; \
         jq --arg s {} --argjson r {} --arg d \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \
            '.state=$s | .remember=$r | .decided=$d' \"$f\" >\"$t\" \
           && mv -f \"$t\" \"$f\" || {{ rm -f \"$t\"; exit 1; }}",
        sh_quote(&requests_dir()),
        sh_quote(box_name),
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

/// The decision skein recorded for `id` — with **"nobody has answered this" kept apart from
/// "skein cannot tell"**.
///
/// The generic form is [`crate::util::read_json_or_why`], and the distinction it keeps is the one
/// [`decide`]'s guard turns on: `Ok(None)` is an id nobody has answered, `Ok(Some(_))` is the
/// answer, and `Err` is a decision file that is *there* and will not parse — the zero-length file a
/// crash between [`crate::util::write_atomic`]'s write and its rename used to leave, or a hand
/// edit. A guard that reads the third as the first has no way to know it is looking at a request
/// that was already answered.
fn decision_or_why(id: &str) -> Result<Option<Request>, String> {
    let path = decision_path(id).ok_or_else(|| format!("unusable request id {id:?}"))?;
    crate::util::read_json_or_why(&path)
}

/// The decision skein recorded for `id`, if it has made one — an unreadable file reading as none.
///
/// **For the readers, and there is one left.** [`decided_over`] paints the cockpit's list, and a
/// decision it cannot read falls back to showing the box's own copy of the request: wrong, and
/// wrong in the direction a person sees, because the row reappears as pending with a button on it.
/// Pressing that button is [`decide`], which reads [`decision_or_why`] and refuses — so the
/// unreadable file costs a confusing row and never a second privileged act. Everything that
/// *decides* something asks [`decision_or_why`] instead.
pub fn decision(id: &str) -> Option<Request> {
    decision_or_why(id).ok().flatten()
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
/// **Including when skein cannot read the decision it may already have made** (SKEIN-418) — see the
/// refusal below for why that direction is the safe one.
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
    match decision_or_why(&rendered.id) {
        Ok(None) => {}
        Ok(Some(already)) => {
            return Err(format!(
                "request {} is already {} — a decision is made once",
                rendered.id, already.state
            ))
        }
        // **A guard is not a store, so it does not get to fall back to a default** (SKEIN-418).
        // `record`'s file has a value in it and refusing there is about not destroying it; this
        // file's value is that it EXISTS, and the whole of the guard is asking whether it does.
        // Read through `.ok()`, an unparseable one answered "no decision" — the guard passed, and
        // the approval that followed both ran an apt/npm install as root inside every box for the
        // second time and wrote its record over the first decision, which may have been a denial.
        //
        // Refusing is the safe direction because the two mistakes are not the same size. Refusing
        // an id that was never answered costs a person one file to move and one press again, and
        // they are already at the cockpit. Approving one that was answered is a privileged act
        // performed twice from a single yes, with the evidence of the first destroyed on the way.
        Err(why) => {
            return Err(format!(
                "refusing to decide {} — skein cannot read the decision it may already have made \
                 ({why}). A decision is made once, and that file is the only record of whether \
                 this one was; skein cannot tell an id nobody has answered from one already \
                 approved or denied. Approving now would run a privileged install a second time \
                 and replace the first decision. The file is left alone; fix or move it, then \
                 decide again.",
                rendered.id
            ))
        }
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
        &decision_script(
            &rendered.box_name,
            &rendered.id,
            &decided.state,
            decided.remember,
        ),
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
fn log_script(box_name: &str, id: &str, state: &str, tail: &str) -> String {
    format!(
        "f={}/{}/{}.json; [ -f \"$f\" ] || exit 0; t=$(mktemp \"$(dirname \"$f\")/.tmp.XXXXXX\") || exit 1; \
         jq --arg s {} --arg l {} '.state=$s | .log=$l' \"$f\" >\"$t\" && mv -f \"$t\" \"$f\" || {{ rm -f \"$t\"; exit 1; }}",
        sh_quote(&requests_dir()),
        sh_quote(box_name),
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
    //
    // Unreadable and absent are told apart here too (SKEIN-418), for the person rather than for
    // safety: this call already fails closed either way — no decision, no install — but "skein has
    // no decision recorded" sends somebody looking for a decision that is on disk in front of them.
    let req = decision_or_why(id)
        .map_err(|why| format!("refusing to install {id} — skein cannot read the decision recorded for it ({why}). The file is left alone; fix or move it, then decide again."))?
        .ok_or_else(|| format!("skein has no decision recorded for {id}"))?;
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
    let _ = own_sandbox(sandbox).exec(
        &log_script(&req.box_name, id, state, &tail),
        Duration::from_secs(30),
    );

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
/// be a spectacular way to fail closed. What comes back is only ever *fewer* packages than were
/// approved, which costs a rebuilt box a reinstall and nothing else.
///
/// **That argument covers this read and does not reach [`record`]**, which writes (SKEIN-359).
/// Reading empty and then writing the result back is how the approvals themselves are lost, and
/// they are not recoverable by asking again — nobody remembers which twelve packages a fleet was
/// told to keep. So the write refuses; this stays soft, and the two are the same file.
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
    // a lost update means a package its owner approved is missing from every future launch. The
    // same is true of a manifest that will not parse, only all at once: `update_json` refuses
    // rather than writing this one approval over every earlier one.
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
        // `decide` builds the box-side script out of `requests_dir()`, which reads
        // `$SKEIN_FLEET_ROOT` — and unset, that is `/boxes`, the live fleet on any machine running
        // skein. Nothing here asserts the root, so the fixture only has to be somewhere that is
        // not somebody's infrastructure.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));

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

        // Put back, or a `$SKEIN_FLEET_ROOT` left set makes every later test that reads the
        // DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// **A decision skein cannot read is not the same answer as no decision** (SKEIN-418).
    ///
    /// "A decision is made once" was a guard over a read that answered `None` to both, so a
    /// decision file that was present and unparseable — the zero-length file a crash between
    /// `write_atomic`'s write and its rename used to leave behind — let the same request be
    /// answered a second time. What is on the other end of that second yes is an `apt`/`npm`
    /// install running as root in every box in the fleet, and the write that follows replaces the
    /// record of the decision that was actually made. So the first decision here is a **denial**:
    /// what the guard is protecting is not "do not install twice" but "the answer that was given
    /// stands", and a corrupt file must not turn a no into a yes.
    #[test]
    fn a_decision_file_skein_cannot_read_does_not_let_a_request_be_approved_a_second_time() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // A fixture rather than the `/boxes` default, for the reason given in
        // `the_packages_approved_are_the_ones_that_were_on_screen`: `decide` reads the fleet root
        // to address the box's own copy of the request.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));

        let rendered = req("apt", &["libnss3"]);
        let denied = decide("no-such-sandbox", &rendered, false, false).expect("decided");
        assert_eq!(denied.state, "denied");
        let path = decision_path(&rendered.id).expect("the id names a file");

        for corrupt in [
            &b""[..],
            &b"{\"id\":\"20260812-101010-1\",\"state\":\"den"[..],
        ] {
            std::fs::write(&path, corrupt).unwrap();

            let why = decide("no-such-sandbox", &rendered, true, true).expect_err(
                "a request whose decision skein could not read was approved a second time — a \
                 denial became an approval, and a root install can now run from it",
            );
            assert!(
                why.contains("cannot read") && why.contains(&rendered.id),
                "the refusal has to say skein could not read it and name the request: {why}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "the decision skein could not read was replaced by a second one"
            );

            // And the install path says the same thing rather than "no decision recorded", which
            // would send somebody looking for a file that is sitting there.
            let said = install("no-such-sandbox", &rendered.id).unwrap_err();
            assert!(
                said.contains("cannot read"),
                "an unreadable decision reads to the installer as one nobody ever made: {said}"
            );
        }

        // It recovers by itself once the file parses, and the answer that comes back is the one
        // that was given: still denied, and still refusing a second decision.
        std::fs::write(&path, serde_json::to_vec_pretty(&denied).unwrap()).unwrap();
        let why = decide("no-such-sandbox", &rendered, true, true).unwrap_err();
        assert!(why.contains("already denied"), "{why}");

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The install reads the artifact, and there is nothing else for it to read.
    ///
    /// This was the third id-keyed read of a box-writable file. Closing render-to-click and leaving
    /// click-to-install open would have moved the window rather than shut it.
    #[test]
    fn an_install_with_no_recorded_decision_has_nothing_to_install() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        // Bound after `home`, so the pin goes back before the directory it names is removed.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        let said = install("no-such-sandbox", "20260812-101010-1").unwrap_err();
        assert!(
            said.contains("no decision recorded"),
            "an install must not be able to fall back to the queue: {said}"
        );
    }

    /// A slash is a path, except in the one place npm makes it a scope.
    ///
    /// The filter used to admit `/` for every kind, which is not a near miss: `apt-get install
    /// ./x.deb` installs a file out of the box's own tree and `npm install -g /path` runs that
    /// path's lifecycle scripts — both as root, for the whole fleet, and then written into the
    /// manifest and replayed at every launch. Neither is an exploit of a bug; both are apt and npm
    /// doing exactly what they are for.
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

    /// **Who a request is from is the directory it is in, and the field is not asked.**
    ///
    /// The queue was one shared read-write directory, so `box` was whatever the requester typed
    /// into its own file and `$SKEIN_BOX` was the only thing standing against a box filing under a
    /// neighbour's name — an environment variable of a process the box owns. It is per box now, and
    /// this runs [`list_script`] over a real directory tree to settle that the reader takes the
    /// name from the path: the first request *lies* in its file, and must come back attributed to
    /// the directory it was found in.
    ///
    /// The last case is a file directly under the queue root — one filed before the split. It comes
    /// back with no box at all and [`Request::problem`] refuses it, which is the honest answer:
    /// nothing can say now which box wrote it, and dropping it silently would leave the box that
    /// filed it waiting on an approval nobody was ever shown.
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
                r#"{"id":"a","box":"api","kind":"apt","packages":["tmux"],"asked":"2026-01-01"}"#,
            ),
            (
                queue.join("api"),
                "b.json",
                r#"{"id":"b","box":"api","kind":"apt","packages":["jq"],"asked":"2026-01-02"}"#,
            ),
            (
                queue.clone(),
                "c.json",
                r#"{"id":"c","box":"web-main","kind":"apt","packages":["rg"],"asked":"2026-01-03"}"#,
            ),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(name), body).unwrap();
        }
        // A box can write anything into its own file, an array included, and one of those must not
        // take the panel away from every other box.
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
            "the request said `api` and was found in `web-main`; the path is what says who filed it"
        );
        assert_eq!(by("b").box_name, "api");
        assert_eq!(
            by("c").box_name,
            "",
            "a request from before the split has no directory to be attributed by"
        );
        assert!(
            by("c").problem().is_some(),
            "an unattributable request must not be actionable"
        );
        assert!(by("a").problem().is_none(), "{:?}", by("a").problem());
        // Put back, because the env lock serialises the tests that take it and does not
        // restore what one of them changed: a `$SKEIN_FLEET_ROOT` left set makes every
        // later test that reads the DEFAULT read this one's temp directory instead.
        std::env::remove_var("SKEIN_FLEET_ROOT");
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
        // Both scripts address the queue through `requests_dir()`, so building one reads
        // `$SKEIN_FLEET_ROOT`. The value is not what is being asserted — what the box's bytes did
        // to the script around it is — so any directory that is not the live `/boxes` will do, and
        // the env lock is what keeps this pin from landing in a neighbour's test.
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        let nasty = "20260812-1-1'; touch /tmp/skein-pwned; :'$(id)`id`";
        // The box name is the second value a box's bytes reach these scripts through, since the
        // queue was split per box and the name became a path component. One at a time, so a hole
        // in either is attributed rather than covered for by the other.
        for (box_name, id) in [("web-main", nasty), (nasty, "20260812-1-1")] {
            for s in [
                decision_script(box_name, id, "approved", true),
                log_script(box_name, id, "installed", "apt said something"),
            ] {
                let quoted = sh_quote(id);
                assert!(
                    s.contains(&format!("/{quoted}.json")),
                    "the id is the filename and arrives as one single-quoted word: {s}"
                );
                assert!(
                    s.contains(&format!("/{}/", sh_quote(box_name))),
                    "the box is the directory and arrives as one single-quoted word: {s}"
                );
                // Everything the box's values contributed, taken away. What is left is this
                // module's own script, and none of the box's bytes may survive in it — a bare copy
                // beside the quoted one is the same hole with a witness.
                let rest = s.replace(&sh_quote(nasty), "");
                assert!(
                    !rest.contains("touch") && !rest.contains("$(id)") && !rest.contains('`'),
                    "something arrived outside the quotes it was wrapped in: {rest}"
                );
            }
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// The same property, settled by a shell instead of by reading one.
    ///
    /// `$(…)` needs no quote-balancing to work — it expands inside double quotes and inside none,
    /// and only single quotes stop it — so it is exactly what an unquoted splice costs, and a
    /// marker file is evidence rather than an argument. Nothing else in either script runs: the
    /// file the id names does not exist, so both give up on the line after the assignment.
    #[test]
    fn a_request_id_cannot_run_a_command_when_a_decision_or_a_log_is_written() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // These scripts are RUN, and the first thing each does is stat a path under the fleet root.
        // Unpinned that is `/boxes/.skein/substrate/requests/…` on the owner's live fleet — the
        // one queue in the fleet that boxes can write, reached here with a payload designed to be
        // hostile. The fixture is the same directory the marker files are watched in, so the whole
        // test acts inside one tree that is deleted when it ends.
        std::env::set_var("SKEIN_FLEET_ROOT", &*dir);
        for which in ["decision", "log"] {
            for field in ["id", "box"] {
                let marker = dir.join(format!("pwned-{which}-{field}"));
                let payload = format!("$(touch {})", marker.display());
                let (box_name, id) = match field {
                    "id" => ("web-main", payload.as_str()),
                    _ => (payload.as_str(), "20260812-1-1"),
                };
                let script = match which {
                    "decision" => decision_script(box_name, id, "denied", false),
                    _ => log_script(box_name, id, "failed", "apt said something"),
                };
                let out = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&script)
                    .current_dir(&*dir)
                    .output()
                    .expect("sh");
                assert!(
                    !marker.exists(),
                    "the {field} ran a command while the {which} script was being read:\n{script}\n{out:?}"
                );
            }
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
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
        // `decision_script` reads the fleet root to address the queue; the fixture keeps that read
        // off `/boxes` without changing anything asserted below.
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        let s = decision_script("web-main", "20260812-1-1", "approved", true);
        assert!(s.contains("--arg s 'approved'"), "{s}");
        assert!(s.contains(".remember=$r"), "{s}");
        assert!(
            s.contains("mv -f"),
            "the request is replaced atomically: {s}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    #[test]
    fn declining_to_remember_is_carried_into_the_decision() {
        // Same read of `$SKEIN_FLEET_ROOT`, same fixture, for the same reason.
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &*root);
        assert!(decision_script("b", "a", "approved", false).contains("--argjson r false"));
        assert!(decision_script("b", "a", "approved", true).contains("--argjson r true"));
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **One approval is not how a fleet forgets the packages it already approved** (SKEIN-359).
    ///
    /// `record` merges one request into the whole manifest and writes it back. Read through
    /// `update_json`, whose read used to answer `Manifest::default()` for a file it could not
    /// parse, approving one `apt install` replaced every package the fleet's owner had ever said
    /// yes to with that one — and nothing asks again, so the loss shows up as a rebuilt box coming
    /// back without half its tools, weeks later.
    ///
    /// The read on the launch path stays soft on purpose and is asserted here too: the argument for
    /// `manifest()` answering empty is that it costs a reinstall, and that argument never reached
    /// the write.
    #[test]
    fn a_package_manifest_skein_cannot_read_is_never_written_over() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        record(&req("apt", &["ripgrep"])).unwrap();
        record(&req("npm", &["typescript"])).unwrap();
        let path = manifest_path();

        for corrupt in [&b""[..], &b"{\"apt\":[\"ripgrep\""[..]] {
            std::fs::write(&path, corrupt).unwrap();
            assert!(
                manifest().apt.is_empty(),
                "the launch path is deliberately soft: an unreadable manifest is no packages"
            );
            let why = record(&req("apt", &["jq"]))
                .expect_err("an approval over an unreadable manifest reported success");
            assert!(
                why.contains("substrate.json") && why.contains("cannot read"),
                "the refusal has to name the file and say it could not be read: {why}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                corrupt,
                "the unreadable manifest was replaced by one approval"
            );
        }

        std::env::remove_var("SKEIN_HOME");
    }
}
