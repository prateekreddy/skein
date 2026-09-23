//! Asking the host warden to do the two things skein must not do itself (§14's `warden-client`).
//!
//! **Why this exists before it has to.** Create and destroy both terminate skein — create because it
//! does not exist yet, destroy because it will not afterwards — so fleet lifecycle cannot live inside
//! the fleet, permanently (§8). Routing them through the warden *while skein is still on the host* is
//! delivery step 3, and the whole argument for doing it in that order: **both callers are exercised
//! before anything moves.** The wire, the operation ids and the timeout semantics are all shaken out
//! against a real warden while there is still an alternative.
//!
//! # Four rules that are easy to lose
//!
//! **A timeout is not a failure.** §8.2's outcome store is what makes a repeat safe, so a client that
//! could not read the reply retries with the *same* operation id and is told what happened. `unknown`
//! stays `unknown` — it is never rendered as "it did not happen", because for a destroy those two
//! license opposite actions.
//!
//! **The advertised capability set decides what skein OFFERS; it never decides what skein BELIEVES.**
//! A malicious endpoint advertises whatever makes skein show a button. So [`Warden::capabilities`]
//! may be used to hide an action from a person, and may never stand in for a check or be recorded as
//! evidence that something happened.
//!
//! **A warden that has the capability performs it; otherwise the person is prompted.** One rule per
//! operation, and [`perform`] is where it lives — not two global modes, because the middle case
//! already exists: a reduced warden (`warden/src/capability.rs`) means the doer it was built with is
//! approved at the host and the one it lacks is typed by hand. Nothing about it is configured;
//! selection is detection.
//!
//! **The operation id is minted here, once, and reused on every retry.** A fresh id per attempt turns
//! the store into a counter of attempts and each one into a separate execution — the exact failure it
//! exists to prevent. It is derived from what is being done rather than from a clock, so a retry
//! after a restart still names the same operation.
//!
//! # What it is not
//!
//! Not a second lease. `attempt.rs` already stops two skeins from proposing the same create at the
//! same moment, and the warden's own doorway (§8.5) stops two proposals reaching one person. Those
//! answer different questions and neither is this: this only carries a request and reports what came
//! back.

use crate::util::{sh_quote, Gate};
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Where the warden listens. Loopback, and §8.6 says why.
pub const DEFAULT_PORT: u16 = 7879;

/// The header the shared secret travels in. Spelled here rather than imported: this crate does not
/// depend on the warden's, deliberately (`tools/module-check.py` asserts it), so the two ends agree
/// by a constant each and by the roundtrip test that would fail if they stopped.
pub const SECRET_HEADER: &str = "x-skein-warden";

/// How long to wait for a reply. Longer than a create takes, because the person at the host has to
/// read the prompt and type an id before the work even starts.
const REPLY: Duration = Duration::from_secs(1800);

/// The same, for an audit entry — which nobody types anything for.
///
/// **Its own number, and the difference is the point.** An entry reported on the doer's timeout
/// would let an unresponsive warden hold a box destroy open for half an hour, and the log exists to
/// record what skein did, not to become a way of stopping it.
const AUDIT_REPLY: Duration = Duration::from_secs(5);

/// How long to wait for the connection itself.
///
/// `TcpStream::connect` has no timeout of its own: an address that accepts nothing and refuses
/// nothing — a firewall that drops — hangs until the kernel gives up, minutes later. Every call here
/// is to a process on the same machine, so seconds is generous.
const CONNECT: Duration = Duration::from_secs(5);

/// How long [`Warden::glance`] waits for a reply — a probe's budget, not a doer's.
const GLANCE: Duration = Duration::from_secs(2);

/// How long the health panel's answer about the warden is reused before asking again.
///
/// The panel refreshes every fifteen seconds and there are as many panels as open tabs, so this is
/// a permanent load rather than an occasional call — the same reasoning that put a [`Gate`] in
/// front of `sbx ls`. A refused connection on loopback returns instantly and none of this matters;
/// a `$SKEIN_WARDEN` pointing at a host that drops packets is where it does.
const SIGHTING_FRESH: Duration = Duration::from_secs(10);

static SIGHTING_GATE: Gate<Sighting> = Gate::new();

/// Why a [`sighting`] failed, in the terms the ADVICE differs on.
///
/// Not a nicety. "It is not there" and "it answered and would not tell me" send a reader to opposite
/// places — one builds and starts a binary, the other looks at a secret, a version, or at what is
/// actually squatting that port — and a check that gives the first answer to the second question is
/// worse than one that says nothing. It shipped that way this morning and told somebody to build a
/// warden they were plainly running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unseen {
    /// Nothing accepted the connection.
    Unreachable,
    /// Something answered, and it was not the answer this asks for.
    Answered,
}

/// What the last [`sighting`] failure was, when there was one.
pub fn sighting_trouble() -> Option<Unseen> {
    SIGHTING_KIND.lock().ok().and_then(|k| *k)
}

static SIGHTING_KIND: std::sync::Mutex<Option<Unseen>> = std::sync::Mutex::new(None);

/// The last [`sighting`] failure in words. Remembered beside the gate rather than re-derived,
/// because asking again is a different question — a warden that has just come back would answer
/// while the panel is still explaining the failure.
static SIGHTING_WHY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// **Forget what the warden was last seen to have.**
///
/// The sighting is remembered for [`SIGHTING_FRESH`], and a create makes it wrong the instant it
/// succeeds — the answer to "does this fleet exist" now comes from here in-fleet, where `sbx ls`
/// cannot answer at all (SKEIN-576). Without this, `fleet::create_fleet_operation` goes on
/// reporting `unsatisfied` against a sandbox the warden has just made, and a person reading
/// "the warden sees no sandboxes" after a successful create asks for a second one.
///
/// The same rule and the same reason as [`crate::sbx::forget_fleet_boxes`]: **an act settles the
/// answers it disturbed**, and the act is the only thing that knows it happened.
pub fn forget_sighting() {
    SIGHTING_GATE.invalidate();
}

/// What the configured warden can see, remembered for [`SIGHTING_FRESH`].
///
/// `None` means it could not be asked, and [`sighting_failure`] says why in the words the panel
/// prints. Separated for the reason `sbx::fleet_boxes` and `sbx::fleet_failure` are: "nothing there"
/// and "could not ask" are different things to say, and a check that says the first when it means
/// the second sends somebody to look at the wrong machine.
pub fn sighting() -> Option<Sighting> {
    // Zero in tests: they point `$SKEIN_WARDEN` at a different fake per case and run in parallel, so
    // a process-wide gate would serve one test's warden to another. Same reasoning as `FLEET_GATE`.
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        SIGHTING_FRESH
    };
    SIGHTING_GATE.get(fresh, || match Warden::configured().glance() {
        Ok(seen) => {
            if let Ok(mut slot) = SIGHTING_KIND.lock() {
                *slot = None;
            }
            remember_sighting_failure(None);
            Some(seen)
        }
        Err(why) => {
            // The connect error is the one the client itself writes, and it is the only failure
            // where nothing was on the other end. Everything else — a refusal, a body that would
            // not parse — means something answered.
            let kind = match why.contains("is not answering on") {
                true => Unseen::Unreachable,
                false => Unseen::Answered,
            };
            if let Ok(mut slot) = SIGHTING_KIND.lock() {
                *slot = Some(kind);
            }
            remember_sighting_failure(Some(why));
            None
        }
    })
}

fn remember_sighting_failure(why: Option<String>) {
    if let Ok(mut slot) = SIGHTING_WHY.lock() {
        *slot = why;
    }
}

/// Somebody has set the WARDEN's variable on a process that is a CLIENT.
///
/// `$SKEIN_WARDEN_PORT` is read by `skein-warden` and by nothing here. Setting it on `skein` or
/// `skein-server` looks like it should move where they ask and does not — they go on asking
/// [`DEFAULT_PORT`], which is now whatever else is listening there. The failure that follows is a
/// refusal from a stranger rather than a connection error, so it reads as the warden being broken.
///
/// **Two variables because they are two questions, not one setting spelled twice.** `serve::bind`
/// takes a port and no host at all: the warden works out its own addresses — loopback, and each
/// Docker bridge — from the machine it is standing on, and a `host:port` variable on that side
/// would be a way to widen the bind by configuration, which is the one thing §8.6 rules out. The
/// client needs a full address for the opposite reason: it reaches the host from inside the
/// sandbox, where the host is not `127.0.0.1`. They share a number and nothing else.
///
/// None of which helps somebody who set the wrong one, so this says so where they are looking.
pub fn misdirected() -> Option<String> {
    let port = std::env::var("SKEIN_WARDEN_PORT").ok()?;
    let port = port.trim();
    if port.is_empty() {
        return None;
    }
    // Both set is somebody who knows what they are doing, whether or not the two agree — and if they
    // disagree, `where_it_asks` already prints the one that counts.
    if std::env::var("SKEIN_WARDEN").is_ok_and(|v| !v.trim().is_empty()) {
        return None;
    }
    Some(format!(
        "`$SKEIN_WARDEN_PORT={port}` is set here, and it is the WARDEN's variable \u{2014} this \
         process reads `$SKEIN_WARDEN`, so it is still asking {asking}. Set \
         `SKEIN_WARDEN={host}:{port}` as well. They are two variables because the warden takes a \
         port and works its own addresses out from the machine it is on (architecture \u{a7}8.6), \
         while a client has to name one \u{2014} and the one a client needs is the HOST, which \
         from inside the sandbox is not loopback.",
        asking = where_it_asks(),
        host = default_host()
    ))
}

/// The address this process asks, as a person would type it.
///
/// Printed rather than assumed: `$SKEIN_WARDEN` moves it, and a diagnostic that says a warden is not
/// answering without saying *where it looked* is unfalsifiable by the person reading it.
pub fn where_it_asks() -> String {
    let warden = Warden::configured();
    format!("{}:{}", warden.host, warden.port)
}

/// Why the warden could not be asked, for the health banner and `skein doctor`.
pub fn sighting_failure() -> Option<String> {
    SIGHTING_WHY.lock().ok().and_then(|why| why.clone())
}

/// A non-200 in the warden's own words, or the first of the body when it did not send any.
fn refusal(code: u16, said: &str) -> String {
    let why = serde_json::from_str::<serde_json::Value>(said)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| said.chars().take(200).collect());
    format!("the warden refused to say what it can see ({code}): {why}")
}

/// What came back, in the terms a caller has to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// It happened, and this is what the warden's command said.
    Ran(Reply),
    /// It happened before, under this same operation id.
    Replayed(Reply),
    /// It ran and failed, or a person refused it.
    Failed(Reply),
    /// Accepted and never settled — the warden died between running and recording.
    ///
    /// **Not a failure.** For a destroy, "we do not know" and "it did not happen" license opposite
    /// actions, and a caller that collapses them destroys a fleet twice.
    Undecided(Reply),
    /// Older than the warden's retention window. Never re-run, and not recoverable.
    Unknown(Reply),
}

/// What the warden said, and what it could not write down while it was saying it.
///
/// **Beside every state, not a state of its own** (SKEIN-554). A warden whose audit log cannot be
/// written still runs an approved command — the owner decided that on 2026-09-15, over refusing —
/// and says so in its reply as `unrecorded`, one entry per line it could not write, each naming the
/// log's path and the error. That can be true of a run, a refusal, an undecided operation or a
/// replay alike, so it rides with all of them; a sixth variant would have had to pick which answer
/// the warning replaces, and every choice hides one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reply {
    detail: String,
    unrecorded: Vec<String>,
}

impl From<&str> for Reply {
    fn from(detail: &str) -> Reply {
        Reply {
            detail: detail.to_string(),
            unrecorded: Vec::new(),
        }
    }
}

impl Answered {
    /// Did the thing definitely happen?
    ///
    /// Deliberately three-valued at the call site rather than two: `None` is `Undecided` or
    /// `Unknown`, and a caller that treats those as `false` is the bug this whole path is about.
    pub fn happened(&self) -> Option<bool> {
        match self {
            Answered::Ran(_) | Answered::Replayed(_) => Some(true),
            Answered::Failed(_) => Some(false),
            Answered::Undecided(_) | Answered::Unknown(_) => None,
        }
    }

    /// What a person is told.
    pub fn detail(&self) -> &str {
        &self.reply().detail
    }

    /// The audit lines the warden could not write for this operation, each naming the log's path
    /// and why. Empty when every line was written. See [`Reply`].
    pub fn unrecorded(&self) -> &[String] {
        &self.reply().unrecorded
    }

    fn reply(&self) -> &Reply {
        match self {
            Answered::Ran(r)
            | Answered::Replayed(r)
            | Answered::Failed(r)
            | Answered::Undecided(r)
            | Answered::Unknown(r) => r,
        }
    }
}

/// The warden, as skein addresses it.
pub struct Warden {
    host: String,
    port: u16,
    /// Nobody said where the warden is, so [`default_host`] and [`DEFAULT_PORT`] were used.
    ///
    /// Carried rather than re-derived at the send, because the two things this separates are done
    /// in different places and only one of them is a hazard: [`Warden::configured`] computes an
    /// *address* — a string [`where_it_asks`] prints into a diagnostic — while
    /// [`Warden::send_within`] opens a *connection* to one. The test guard is on the second, so a
    /// test may still assert what the default address is (`the_warden_is_looked_for_on_the_host…`
    /// does, and that is the whole content of SKEIN-475) without being refused for it.
    defaulted: bool,
}

/// Where the shared secret is (§9.5 R5), derived the same way on both sides.
///
/// **From the volume root, because that is what the cover is derived over.** R5 puts the secret
/// "under the cover of requirement 2", and the cover follows `$SKEIN_HOME`; a secret at a fixed
/// `~/.skein/warden` held R5 only while the volume sat at its default — repoint the volume (the
/// documented backup flow does: `SKEIN_HOME=~/.skein-backup-<date> skein repoint`) and the cover
/// moved while the secret stayed behind, uncovered. So the home is `{$SKEIN_HOME | ~/.skein}/warden`
/// — under the cover for every volume location, by construction. `$SKEIN_WARDEN_HOME` stays as the
/// explicit override at both ends, for tests and development, and setting it is the operator
/// deliberately stepping outside the covered world: nothing re-derives a cover over it.
///
/// **Not read from `config`.** The rule this module keeps — and `docs/modules.toml` states — is that
/// a client which had to ask `config` or `fleet` anything is a client the thing it is talking about
/// could shape. So the chain above is resolved from the environment directly, reproducing
/// `config::skein_home` (`src/config.rs`) rather than calling it — the same reading, including
/// "empty is unset" — and `warden/src/lib.rs`'s `home()` spells the identical chain on the other
/// end, with the roundtrip test to fail if they drift.
///
/// **And it does not ask where skein is running.** There is one deployment (SKEIN-576), and there
/// was nothing to ask even before that: in the fleet the volume is mounted at its host path, so the same chain
/// resolves to the same file from both sides of the move; nothing about reading the secret differs
/// by where skein is standing. The warden's own home is the half that changes at 4c — its record
/// moves off the volume (`warden/src/lib.rs` `audit_home()`, SKEIN-218) — and that is the warden's
/// decision on the host, never this reader's.
///
/// **skein only reads.** One minter — the warden, which owns the directory — because two would each
/// write a different value and the mismatch would look exactly like an intruder, which is the
/// loudest possible failure for the most boring possible cause. The old fixed default is kept only
/// as a read-side courtesy for upgrade skew: a warden started before the home followed the volume
/// still holds the bytes at `~/.skein/warden/secret`, and moving that file is the warden's job
/// (`warden/src/secret.rs`, `adopt_left_behind`), never this reader's.
fn secret() -> String {
    let read = |home: &std::path::Path| {
        std::fs::read_to_string(home.join("secret"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    if let Some(overridden) = std::env::var_os("SKEIN_WARDEN_HOME").filter(|s| !s.is_empty()) {
        return read(std::path::Path::new(&overridden)).unwrap_or_default();
    }
    let host_home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    let volume = std::env::var_os("SKEIN_HOME")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(&host_home).join(".skein"));
    read(&volume.join("warden")).unwrap_or_else(|| {
        // The skew courtesy: nothing at the derived home yet, so answer a warden that read the
        // old default at ITS start. On a default install the two paths are the same file.
        read(&std::path::PathBuf::from(&host_home).join(".skein/warden")).unwrap_or_default()
    })
}

/// What the observation endpoint reports.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Sighting {
    #[serde(default)]
    pub sandboxes: Vec<String>,
    /// What the far end says it can do. **Never evidence** — see the module note.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Deserialize)]
struct Said {
    #[serde(default)]
    state: String,
    #[serde(default)]
    said: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    started_at: String,
    /// Audit lines the warden could not write for this operation — see [`Reply`].
    #[serde(default)]
    unrecorded: Vec<String>,
}

/// The address the warden is at when nobody has said.
///
/// **The warden is on the host and skein is not.** `127.0.0.1` from in here is the SANDBOX, so a
/// loopback default was not merely unhelpful, it named the wrong computer — and the failure it
/// produced was "the warden is not running", which sends a reader to start one that was already
/// running. It took a deployment argument while a host-driven skein existed, where the two shared
/// a machine; there is one deployment now (SKEIN-576) and one answer.
///
/// `host.docker.internal` is the alias every sandbox has for its host, and it is the same name the
/// rest of skein already uses to cross that boundary. Measured rather than assumed: from inside the
/// fleet it resolves to `169.254.1.1` and the warden answers there.
const fn default_host() -> &'static str {
    "host.docker.internal"
}

impl Warden {
    /// The warden this skein is configured to ask. `$SKEIN_WARDEN` overrides `host:port`.
    pub fn configured() -> Warden {
        let raw = std::env::var("SKEIN_WARDEN").unwrap_or_default();
        match raw.trim().rsplit_once(':') {
            Some((h, p)) if !h.is_empty() => Warden {
                host: h.to_string(),
                port: p.parse().unwrap_or(DEFAULT_PORT),
                defaulted: false,
            },
            // Unset, empty, or without a `host:port` to read — nobody said, so the default answers
            // and the address is marked as nobody's choice. See [`Warden::defaulted`].
            _ => Warden {
                host: default_host().to_string(),
                port: DEFAULT_PORT,
                defaulted: true,
            },
        }
    }

    pub fn at(host: &str, port: u16) -> Warden {
        Warden {
            host: host.to_string(),
            port,
            defaulted: false,
        }
    }

    /// What the warden can see, and what it says it can do.
    ///
    /// **The status code is read.** Every field of [`Sighting`] has a serde default, so a refusal —
    /// `{"error": "…"}` — parses perfectly into a listing of no sandboxes and no capabilities. A
    /// caller would then be told the host is running nothing, which is the same shape as a working
    /// answer and the opposite of the truth. Found by pointing this at a warden that did not
    /// recognise the caller.
    pub fn look(&self) -> Result<Sighting, String> {
        let (code, said) = self.send("GET", "/v1/fleet", "")?;
        if code != 200 {
            return Err(refusal(code, &said));
        }
        serde_json::from_str(&said).map_err(|e| format!("the warden's listing was unreadable: {e}"))
    }

    /// The same question, on a probe's budget rather than a doer's.
    ///
    /// [`REPLY`] is half an hour, because a create waits on a person. Nothing waits on a person
    /// here: this is asked to fill in a line on the health panel, every fifteen seconds, and a
    /// warden that needs longer than a couple of seconds to say what it can see is one the panel
    /// should describe as not answering.
    fn glance(&self) -> Result<Sighting, String> {
        let (code, said) = self.send_within("GET", "/v1/fleet", "", GLANCE)?;
        if code != 200 {
            return Err(refusal(code, &said));
        }
        serde_json::from_str(&said).map_err(|e| format!("the warden's listing was unreadable: {e}"))
    }

    /// Ask for the fleet sandbox to be created.
    ///
    /// `argv` is what `sbx` would have been given. It travels as data the warden re-parses and
    /// re-renders (§8.4) — skein does not get to say what the person at the host is shown.
    pub fn create(
        &self,
        sandbox: &str,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<Answered, String> {
        self.doer("create", sandbox, argv, env)
    }

    /// Ask for it to be destroyed.
    pub fn destroy(&self, sandbox: &str) -> Result<Answered, String> {
        self.doer("destroy", sandbox, &[], &[])
    }

    /// Withdraw a host port mapping.
    ///
    /// The argv is sent in full and the warden checks it rather than trusting it — in particular it
    /// refuses `--publish` by name, because a withdrawal endpoint that can be talked into opening a
    /// port is a publish capability nobody declared (`warden/src/doer.rs::argv_unpublish`).
    pub fn unpublish(
        &self,
        sandbox: &str,
        host_port: u16,
        sandbox_port: u16,
    ) -> Result<Answered, String> {
        let argv = vec![
            "ports".to_string(),
            sandbox.to_string(),
            "--unpublish".to_string(),
            format!("{host_port}:{sandbox_port}/tcp"),
        ];
        self.doer("unpublish", sandbox, &argv, &[])
    }

    /// Tell the warden something worth recording. Best-effort by design: an audit sink that could
    /// fail a lifecycle operation would be a reason to stop auditing.
    pub fn record(&self, operation: &str, what: &str, detail: &str) -> Result<(), String> {
        let body = serde_json::json!({
            "operation": operation,
            "what": what,
            "detail": detail,
            "reported_by": "skein",
        })
        .to_string();
        let (code, said) = self.send_within("POST", "/v1/audit", &body, AUDIT_REPLY)?;
        match code {
            200 => Ok(()),
            // The code is read here for the same reason `look` reads it: `{"error": …}` is a
            // perfectly well-formed reply, and a caller that ignored the number would report a
            // refusal as a recorded entry.
            other => Err(format!(
                "the warden did not record it ({other}): {said:.200}"
            )),
        }
    }

    fn doer(
        &self,
        verb: &str,
        sandbox: &str,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<Answered, String> {
        // The environment is part of the operation's identity, not decoration: a create at
        // `DOCKER_SANDBOXES_ROOT_SIZE=200g` is a different operation from the same argv at the
        // default, and giving them one id would let the second be answered with the first's outcome.
        let operation = operation_id_with_env(verb, sandbox, argv, env);
        let body = serde_json::json!({
            "operation": operation,
            "sandbox": sandbox,
            "args": argv,
            "env": env,
        })
        .to_string();
        let (code, said) = self.send("POST", &format!("/v1/{verb}"), &body)?;
        read_answer(code, &said, &operation)
    }

    /// One request, one reply, connection closed — the only shape the warden speaks (§8.6).
    fn send(&self, method: &str, path: &str, body: &str) -> Result<(u16, String), String> {
        self.send_within(method, path, body, REPLY)
    }

    /// Connect, with a timeout — see [`CONNECT`].
    fn connect(&self) -> std::io::Result<TcpStream> {
        use std::net::ToSocketAddrs;
        let mut last = std::io::Error::other("no address to try");
        for addr in (self.host.as_str(), self.port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, CONNECT) {
                Ok(stream) => return Ok(stream),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// One request, one reply, on this process's own deadline — and **a test process that has not
    /// said which warden to ask is refused rather than sent to whatever is on the host**, the same
    /// rule [`crate::util::fleet_root`] and [`crate::config::skein_home`] already hold for the two
    /// paths (SKEIN-762).
    ///
    /// The guard is here, at the connection, rather than in [`Warden::configured`], because
    /// computing the default address is harmless and printing it is a feature —
    /// [`where_it_asks`] puts it in the message a person reads when the warden is not answering,
    /// and a guard on the address would refuse that as well as this.
    ///
    /// It keys on [`Warden::defaulted`] rather than on `$SKEIN_WARDEN` being set, which is not the
    /// same question: eleven call sites reach a fixture warden through [`Warden::at`] with a port
    /// their own `TcpListener` chose, and asking about the variable would refuse every one of them
    /// for having done the right thing by a different route.
    ///
    /// **What this was costing.** Four lib tests destroy a fixture box, `sandbox::destroy_box`
    /// calls [`reported`], and each of them opened a real TCP connection to whatever warden this
    /// machine can reach — refused 401, so nothing landed, but a round trip per test and an
    /// unbounded wait against a warden that accepted and stalled. It was found by reading a test's
    /// output, which is the part SKEIN-530 is about: the reach was real and no gate could see it.
    fn send_within(
        &self,
        method: &str,
        path: &str,
        body: &str,
        reply: Duration,
    ) -> Result<(u16, String), String> {
        assert!(
            !(self.defaulted && crate::util::in_test()),
            "$SKEIN_WARDEN is unset in a test process (${marker}). Refusing to open a connection \
             to {host}:{port}: that is whatever warden this machine can reach — the owner's, on \
             any machine running one — and a test that asks it something reaches a real service \
             outside its fixture (SKEIN-762). Set $SKEIN_WARDEN to this test's own fake, or to \
             `127.0.0.1:1` where nothing listens if the call is not what is being asserted.",
            marker = crate::util::TEST_MARKER,
            host = self.host,
            port = self.port,
        );
        let mut stream = self.connect().map_err(|e| {
            format!(
                "the host warden is not answering on {}:{} ({e}). Fleet create and destroy go \
                 through it — start it with `skein-warden`, somewhere a person can approve what it \
                 asks (architecture §8).",
                self.host, self.port
            )
        })?;
        stream.set_read_timeout(Some(reply)).ok();
        stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
        // Presented on every request, including the two that only report: after 4c the bind is not
        // the boundary any more, and "it only tells you things" is how an endpoint ends up outside
        // a check. Sent even when it is empty — the warden's refusal then names the file, which is
        // the thing an operator can act on, and a client that stayed silent instead would report
        // the warden as unreachable.
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             {}: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.host,
            SECRET_HEADER,
            secret(),
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| format!("the warden closed the connection: {e}"))?;
        let mut raw = String::new();
        stream
            .read_to_string(&mut raw)
            .map_err(|e| format!("the warden's reply could not be read: {e}"))?;
        let code: u16 = raw
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| format!("the warden answered something that is not HTTP: {raw:.120}"))?;
        let said = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        Ok((code, said.to_string()))
    }
}

/// Report something skein did, into a log skein does not own (§9.5 R6).
///
/// **After the fact, carrying the outcome.** An entry written before and an entry written after are
/// different claims, and only one of them can be checked: "skein destroyed this box" is settled by
/// the box being gone, while "skein is about to" is settled by nothing. The cost is stated rather
/// than hidden — a crash between the act and this call leaves no line, and the acts reported here
/// are ones whose result is visible elsewhere.
///
/// **It cannot fail what it records.** Nothing is returned, a failure goes to stderr, and the
/// timeouts are the short ones: an audit sink that could stop a box being destroyed would be a
/// reason to stop auditing (`record`'s own note). The line names the warden, because a fleet whose
/// acts are going unrecorded is something an operator wants to find out from the operation rather
/// than from an empty log later.
///
/// Not spawned. A thread per entry would report acts in whatever order the scheduler ran them, and
/// the warden stamps its own arrival time — so the log's order would stop being the order things
/// happened, which is most of what makes it able to settle an argument.
pub fn reported(operation: &str, what: &str, detail: &str) {
    if let Err(e) = Warden::configured().record(operation, what, detail) {
        eprintln!("skein: {what} was not recorded in the host audit log ({e}) — it still happened");
    }
}

/// Turn a reply into the five states, keeping every one distinct.
fn read_answer(code: u16, said: &str, operation: &str) -> Result<Answered, String> {
    let parsed: Said = serde_json::from_str(said).unwrap_or(Said {
        state: String::new(),
        said: String::new(),
        error: said.to_string(),
        started_at: String::new(),
        unrecorded: Vec::new(),
    });
    let detail = match parsed.error.trim().is_empty() {
        true => parsed.said.clone(),
        false => parsed.error.clone(),
    };
    // Whatever the state, what the warden could not write down comes with it (SKEIN-554).
    let reply = |detail: String| Reply {
        detail,
        unrecorded: parsed.unrecorded.clone(),
    };
    match (code, parsed.state.as_str()) {
        (200, "replayed") => Ok(Answered::Replayed(reply(detail))),
        (200, _) => Ok(Answered::Ran(reply(detail))),
        (_, "undecided") => Ok(Answered::Undecided(reply(format!(
            "{operation} was accepted at {} and never settled — it may have happened. Ask the \
             warden about it before doing anything that assumes it did not.",
            parsed.started_at
        )))),
        (410, _) | (_, "unknown") => Ok(Answered::Unknown(reply(format!(
            "{operation} is older than the warden's retention window, so what happened to it is \
             not recoverable — and it will not be run again. {detail}"
        )))),
        // A 404 on a doer is §8.3: it is not in that warden's binary. Reported as a failure with the
        // reason, never as "it did not work" — the two send a person to different places.
        (404, _) => Ok(Answered::Failed(reply(format!(
            "this warden was built without that operation. {detail}"
        )))),
        (429, _) => Ok(Answered::Failed(reply(detail))),
        (_, _) => Ok(Answered::Failed(reply(detail))),
    }
}

/// The operation id for a piece of work — the same every time it is asked for.
///
/// **Derived, not minted.** A fresh id per attempt makes every retry a new operation to the warden,
/// which turns at-most-once into once-per-attempt and is the exact failure the store exists to
/// prevent. Deriving it from the verb, the sandbox and the arguments means a retry after a skein
/// restart still names the same operation — and a create with *different* arguments is correctly a
/// different one.
pub fn operation_id(verb: &str, sandbox: &str, argv: &[String]) -> String {
    operation_id_with_env(verb, sandbox, argv, &[])
}

/// The same, including the environment the command will carry — see [`Warden::create`].
pub fn operation_id_with_env(
    verb: &str,
    sandbox: &str,
    argv: &[String],
    env: &[(String, String)],
) -> String {
    let described: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in std::iter::once(verb)
        .chain(std::iter::once(sandbox))
        .chain(argv.iter().map(String::as_str))
        .chain(described.iter().map(String::as_str))
        .flat_map(|part| part.bytes().chain(std::iter::once(0x1f)))
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Only characters the warden's id guard accepts, and short enough to be typed back at its
    // approval prompt — which is what confirming one costs.
    format!("{verb}-{}-{hash:016x}", slug(sandbox))
}

fn slug(raw: &str) -> String {
    raw.chars()
        .map(|c| match c.is_ascii_alphanumeric() {
            true => c,
            false => '-',
        })
        .collect()
}

/// The line a person runs when the warden could not be reached, so a blocked fleet is not a dead end.
pub fn by_hand(argv: &[String]) -> String {
    format!(
        "sbx {}",
        argv.iter()
            .map(|a| sh_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// A privileged act: one of the three things skein must not — and in the fleet cannot — do itself.
///
/// **Enumerated, not open.** These are what `docs/sources.toml`'s `[sbx]` row reaches for and what
/// `warden/src/` has doers for: `sbx create` (`warden/src/doer.rs`, `argv_create`), `sbx rm -f`
/// (`argv_destroy` — and a resize rides on it, because `warden/src/capability.rs` says removing
/// `Destroy` removes resize with it), and `sbx ports … --publish` — which no warden performs, so
/// it is a recipe a person runs (`fleet::publish_cockpit_port`). A
/// fourth would need a doer, a prompt and a reason, which is the point of making the list a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Make the fleet sandbox. `argv` and `env` are exactly what `sbx` would be given — the same
    /// pair [`Warden::create`] sends, so the line a person is offered and the line the warden would
    /// have run are one string built once.
    Create {
        sandbox: String,
        argv: Vec<String>,
        env: Vec<(String, String)>,
    },
    /// Destroy it. Also half of a resize, which is why a warden without `destroy` cannot resize.
    Destroy { sandbox: String },
    /// Forward a sandbox port to the host. `HOST:SANDBOX/tcp` is sbx's spelling and not a guess —
    /// `fleet::publish_cockpit_port` puts the same mapping in its recipe.
    Publish {
        sandbox: String,
        host_port: u16,
        sandbox_port: u16,
    },
    /// Withdraw one. The mirror of [`Act::Publish`] and, unlike it, something a warden can do:
    /// `capability::Capability::Unpublish` exists and `Publish` deliberately does not, because
    /// closing an opening and opening one are not the same act (§9.4).
    Unpublish {
        sandbox: String,
        host_port: u16,
        sandbox_port: u16,
    },
}

impl Act {
    /// The command, exactly as it must be typed.
    ///
    /// Every argument quoted, for [`by_hand`]'s reason — a line a person pastes has to be right for
    /// the argument that needs quoting, and quoting uniformly is how that stays true when somebody
    /// adds one. The environment is quoted too, which is where this differs from the warden's own
    /// approval text (`warden/src/doer.rs`, `described_env`): that text is read, this line is run.
    pub fn command(&self) -> String {
        match self {
            Act::Create { sandbox, argv, env } => {
                let described: String = env
                    .iter()
                    .map(|(k, v)| format!("{k}={} ", sh_quote(v)))
                    .collect();
                // `argv` in full and nothing prepended, which this got wrong: `fleet::create_argv`
                // ALREADY begins `["create", "--name", <sandbox>]`, so adding a verb and a name in
                // front rendered `sbx create skein-fleet create --name skein-fleet …` — a line that
                // fails if typed, in the one place whose entire job is a line a person can type.
                //
                // It survived its own tests because they pass a synthetic argv (`["-m", "26g"]`)
                // with no verb in it. Wiring this to the real caller is what showed it, which is
                // the argument for `Act` carrying what `Warden::create` actually sends rather than
                // a description of it — as this type's own doc says it does.
                let _ = sandbox;
                format!("{described}{}", by_hand(argv))
            }
            Act::Destroy { sandbox } => by_hand(&["rm".to_string(), "-f".into(), sandbox.clone()]),
            Act::Publish {
                sandbox,
                host_port,
                sandbox_port,
            } => by_hand(&[
                "ports".to_string(),
                sandbox.clone(),
                "--publish".into(),
                format!("{host_port}:{sandbox_port}/tcp"),
            ]),
            Act::Unpublish {
                sandbox,
                host_port,
                sandbox_port,
            } => by_hand(&[
                "ports".to_string(),
                sandbox.clone(),
                "--unpublish".into(),
                format!("{host_port}:{sandbox_port}/tcp"),
            ]),
        }
    }

    /// What skein wants it for, in terms of what the person is trying to do.
    ///
    /// Not what the command does — they can read that off the line above it. What is stuck without
    /// it, which is the only thing that makes running somebody else's privileged command reasonable.
    pub fn why(&self) -> String {
        match self {
            Act::Create { sandbox, .. } => format!(
                "skein cannot make the sandbox it runs inside: create terminates skein, because \
                 there is no skein until the fleet exists (architecture §8). {sandbox} is where \
                 every box, the cockpit and skein itself live, so this is the first command and \
                 nothing else can happen before it."
            ),
            Act::Destroy { sandbox } => format!(
                "a resize is a destroy and a recreate — `warden/src/capability.rs` says so, which \
                 is why a warden built without `destroy` cannot resize either — and today \
                 `skein resize` is the only thing that asks for it. It has already copied every box \
                 out of {sandbox} before this line is offered, so what this destroys is a sandbox \
                 whose contents are on disk elsewhere."
            ),
            Act::Publish {
                host_port,
                sandbox_port,
                ..
            } => format!(
                "nothing on the host can reach a port inside the sandbox until it is forwarded, so \
                 the cockpit at :{sandbox_port} is unreachable from a browser until :{host_port} \
                 maps to it. A wrong number is recoverable — `sbx ports <sandbox> --unpublish` \
                 takes a mapping back — but it is a mapping into a sandbox on your machine, and \
                 skein asks rather than choosing a host port for you."
            ),
            Act::Unpublish {
                sandbox,
                host_port,
                sandbox_port,
            } => format!(
                "the host's :{host_port} still forwards into {sandbox}:{sandbox_port} and nothing \
                 useful is behind it — a probe that judged a live port dead, or an agent that never \
                 came up. Withdrawing it frees the number and closes a way in that skein is no \
                 longer using. This is the one port act that only ever CLOSES something, which is \
                 why a warden may do it where a publish is always put to you."
            ),
        }
    }

    /// What happens if they decline — plainly, because a person who cannot see the cost of "no"
    /// has not been given a choice.
    ///
    /// The one that gets skipped, and the reason `docs/live-check.md` writes "what a failure means"
    /// under every command on the page. Declining is a supported outcome and each of these says
    /// what skein does next, not what breaks.
    pub fn if_declined(&self) -> String {
        match self {
            Act::Create { sandbox, .. } => format!(
                "there is no {sandbox}, so no box starts and the cockpit has nowhere to serve \
                 from — and nothing else is lost, because skein has not done anything yet. \
                 Creating the sandbox is idempotent, so running the line later is the same \
                 operation rather than a second one."
            ),
            Act::Destroy { sandbox } => format!(
                "{sandbox} stays exactly as it is, at the size it already has. A resize stops \
                 here with the sandbox untouched, and every box's copy stays on disk where it \
                 was written, so nothing is lost by saying no — only the resize does not happen."
            ),
            Act::Publish {
                host_port,
                sandbox_port,
                ..
            } => format!(
                "no mapping is made, so :{sandbox_port} stays reachable only from inside the \
                 sandbox and the cockpit cannot be opened on the host at :{host_port}. Nothing is \
                 spent, and nothing is closed off: a mapping can be made later, and taken back \
                 with `sbx ports <sandbox> --unpublish`."
            ),
            Act::Unpublish {
                sandbox,
                host_port,
                sandbox_port,
            } => format!(
                "the mapping stays, so :{host_port} on the host goes on forwarding into \
                 {sandbox}:{sandbox_port}. Nothing breaks — skein does not use it and will not \
                 reuse the number — but the way in stays open until somebody runs the line above. \
                 Declining costs a port and an opening, never a working fleet."
            ),
        }
    }

    /// The doer that would perform this, or `None` where no warden has one.
    ///
    /// `Publish` is `None` and that is the rule's clearest case rather than a gap: there is no
    /// `/v1/ports` endpoint in `warden/src/serve.rs`, so no warden anywhere can be asked for it and
    /// "no capability is available" is simply always true. Asking first and prompting on the 404
    /// would say the same thing after a round trip and a misleading error.
    fn asked_of(&self, warden: &Warden) -> Option<Result<Answered, String>> {
        match self {
            Act::Create { sandbox, argv, env } => Some(warden.create(sandbox, argv, env)),
            Act::Destroy { sandbox } => Some(warden.destroy(sandbox)),
            Act::Publish { .. } => None,
            Act::Unpublish {
                sandbox,
                host_port,
                sandbox_port,
            } => Some(warden.unpublish(sandbox, *host_port, *sandbox_port)),
        }
    }

    /// The prompt for this act, carrying what the warden said when it was asked and would not.
    pub fn prompt(&self, warden_said: Option<String>) -> Prompt {
        Prompt {
            command: self.command(),
            why: self.why(),
            if_declined: self.if_declined(),
            warden_said,
            unrecorded: Vec::new(),
        }
    }
}

/// A privileged command put to the person, in `docs/live-check.md`'s voice.
///
/// **Three parts, and it is not shippable without the third.** The command, why skein wants it, and
/// what happens if they decline. The third is the one that gets skipped: a person who cannot see
/// the cost of "no" has not been given a choice, they have been given an instruction with a
/// decoration. `docs/live-check.md` already writes every entry this way — the command, what a pass
/// looks like, and what a failure *means*, "because half of these fail in a way that looks like
/// something else" — and this reuses that voice rather than inventing a second one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Exactly as it must be typed.
    pub command: String,
    /// What skein wants it for, in terms of what the person is trying to do.
    pub why: String,
    /// What happens if they say no. Declining is a supported outcome, not an error state.
    pub if_declined: String,
    /// What the warden said when it was asked first, where there was a warden to ask.
    ///
    /// `None` means nothing was asked — either there is no doer anywhere for this act, or the
    /// endpoint could not be reached at all and the transport error names the address itself.
    pub warden_said: Option<String>,
    /// Audit lines the warden could not write while it was asked (SKEIN-554). A refusal goes
    /// unrecorded as easily as a run does, so it is carried here too. Not rendered: how a person is
    /// told is the owner's to decide, so [`Prompt::render`] is unchanged.
    pub unrecorded: Vec<String>,
}

impl Prompt {
    /// The prompt as a person reads it — one block, on stderr or in a pane.
    pub fn render(&self) -> String {
        let mut said = format!(
            "skein needs a privileged command run at the host, and it cannot run this one \
             itself:\n\n    {}\n\nWhy: {}\n\nIf you don't: {}",
            self.command, self.why, self.if_declined
        );
        if let Some(warden) = &self.warden_said {
            said.push_str(&format!("\n\nThe warden was asked first: {warden}"));
        }
        said
    }
}

/// What became of a privileged act: a warden did it, or a person is being asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Performed {
    /// A warden with the capability performed it, with approval and audit — as today.
    Warden(Answered),
    /// Nobody did it, and this is what a person is shown. **Not an error**: the caller carries on
    /// from here, and a declined prompt is an outcome rather than a failure.
    Prompt(Prompt),
    /// It may have happened and nothing can say. Never a prompt — see [`perform_through`].
    Uncertain(Answered),
}

/// **The rule**: for each privileged act, a warden that has that capability performs it; otherwise
/// the person is prompted.
///
/// One rule per operation, and deliberately not two global modes. The middle case already exists:
/// `warden/src/capability.rs` ships reduced builds with `Create` or `Destroy` compiled out, so a
/// create-only warden simply means destroy prompts and create does not. Two modes cannot express
/// that, and they would also put the prompt path behind an opt-in — which is how a fallback rots,
/// because the people who need it on the day are the ones who never turned it on.
///
/// **Selection is detection, not configuration.** There is no setting and no first-run question: a
/// warden that is running and does it, does it. Today a warden that is not there is a dead end
/// (`send_within`'s "start it with `skein-warden`"), and *that message is what became this prompt*.
///
/// # Driven by what happened, never by what was advertised
///
/// The capability list on `/v1/fleet` is a courtesy to the UI — `warden/src/capability.rs` says so
/// itself, because a malicious endpoint advertises whatever makes skein show a button. So nothing
/// here reads it. A warden that advertises `destroy` and then refuses falls through to a prompt,
/// exactly like one that never claimed it, and so does an unpaired warden — which refuses
/// *everything* with a 503 (`warden/src/serve.rs`, `secret.missing()`), including the two endpoints
/// that only report. That second case is the one that made this test-worthy: a mispaired
/// `$SKEIN_HOME` otherwise reads as a fleet that can do nothing, the symptom `docs/live-check.md`
/// §4 already warns about, rather than as a fleet whose commands you now type yourself.
///
/// # The one thing that is not prompted, and why
///
/// `Undecided` and `Unknown` are not "it did not happen" (§8.2), and offering a person the command
/// after either would be offering them a *second* execution: a create that may already have made
/// the sandbox, or a destroy of a fleet that may already be gone. The three-valued
/// [`Answered::happened`] is what this keys on, so the carve-out cannot drift from the states it is
/// about.
pub fn perform(act: &Act) -> Performed {
    perform_through(&Warden::configured(), act)
}

/// The rule, against a named warden. [`perform`] is this against the configured one.
pub fn perform_through(warden: &Warden, act: &Act) -> Performed {
    let Some(asked) = act.asked_of(warden) else {
        // No doer exists anywhere for this act, so there is nothing to detect and nobody to ask.
        return Performed::Prompt(act.prompt(None));
    };
    match asked {
        // A transport failure is the "no warden at all" case, and the client's own error already
        // names the address it tried — so it is carried into the prompt rather than re-derived.
        Err(why) => Performed::Prompt(act.prompt(Some(why))),
        Ok(answered) => match answered.happened() {
            Some(true) => Performed::Warden(answered),
            None => Performed::Uncertain(answered),
            // It did not happen, and every way of not happening lands here: a doer this warden was
            // built without (404), a refusal for an unreadable or mismatched secret (503, 401), a
            // doorway that is full (429), a person who said no, and a command that ran and failed.
            // They are not told apart because at this point they do not differ: the act has not
            // happened, no warden is going to do it, and the next move is the person's.
            Some(false) => {
                let mut prompt = act.prompt(Some(answered.detail().to_string()));
                prompt.unrecorded = answered.unrecorded().to_vec();
                Performed::Prompt(prompt)
            }
        },
    }
}

impl Performed {
    /// The audit lines the warden could not write for this act, whatever became of it.
    ///
    /// **The one question a surface asks to find out whether a privileged act went unrecorded**
    /// (SKEIN-554). The owner's decision is that an approved command still runs when the warden's
    /// log cannot be written, and that the person is told. This carries the fact to where skein
    /// receives the outcome and says nothing itself: neither caller of [`perform`] in
    /// `src/fleet/` reads it yet (`grep -rn 'Performed::Warden' src/fleet/`), and the wording
    /// and where it appears are the owner's to choose.
    pub fn unrecorded(&self) -> &[String] {
        match self {
            Performed::Warden(answered) | Performed::Uncertain(answered) => answered.unrecorded(),
            Performed::Prompt(prompt) => &prompt.unrecorded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same work asks under the same id, and different work does not.
    ///
    /// This is what makes a retry safe: the warden's store keys on the id, so an id that changed per
    /// attempt would make every retry a new operation and every operation a second execution.
    #[test]
    fn the_same_work_keeps_the_same_operation_id_across_attempts() {
        let argv = vec!["create".to_string(), "--name".into(), "skein-fleet".into()];
        let first = operation_id("create", "skein-fleet", &argv);
        assert_eq!(first, operation_id("create", "skein-fleet", &argv));

        // Different arguments are a different operation — a create at 26g is not the one at 8g.
        let bigger = vec![
            "create".to_string(),
            "--name".into(),
            "skein-fleet".into(),
            "-m".into(),
            "26g".into(),
        ];
        assert_ne!(first, operation_id("create", "skein-fleet", &bigger));
        assert_ne!(first, operation_id("destroy", "skein-fleet", &argv));
        assert_ne!(first, operation_id("create", "other-fleet", &argv));
        // And the environment counts: a create at 200 GB is not the one at sbx's default, so
        // answering the second with the first's outcome would hand back a fleet of the wrong size.
        assert_ne!(
            first,
            operation_id_with_env(
                "create",
                "skein-fleet",
                &argv,
                &[("DOCKER_SANDBOXES_ROOT_SIZE".into(), "200g".into())]
            )
        );

        // And it survives the warden's id guard: letters, digits, `-` and `_`, at most 128.
        assert!(first.len() <= 128);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{first}"
        );
        assert!(
            operation_id("create", "a/b c", &[])
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "a sandbox name the warden would refuse must not produce an id it refuses"
        );
    }

    /// Both crates agree on the port a client asks by default.
    ///
    /// They do not depend on each other, on purpose, so this is the only thing keeping the two
    /// constants in step — and if they drifted the symptom would be a warden that starts on the
    /// port skein is not asking, saying nothing about it.
    #[test]
    fn the_default_port_is_the_one_the_warden_says_skein_looks_at() {
        assert_eq!(DEFAULT_PORT, 7879);
    }

    /// Five states in, five states out, and the two that are not answers stay unanswered.
    #[test]
    fn a_timeout_is_not_a_failure_and_unknown_is_not_a_no() {
        let ran = read_answer(200, r#"{"state":"ran","ok":true,"said":"made"}"#, "op-1").unwrap();
        assert_eq!(ran, Answered::Ran("made".into()));
        assert_eq!(ran.happened(), Some(true));

        let again = read_answer(
            200,
            r#"{"state":"replayed","ok":true,"said":"made"}"#,
            "op-1",
        )
        .unwrap();
        assert_eq!(again.happened(), Some(true));
        assert!(matches!(again, Answered::Replayed(_)));

        let refused = read_answer(
            409,
            r#"{"state":"ran","ok":false,"error":"refused at the host"}"#,
            "op-1",
        )
        .unwrap();
        assert_eq!(refused.happened(), Some(false));

        // The two that must never read as "it did not happen".
        let undecided = read_answer(
            409,
            r#"{"state":"undecided","started_at":"2026-08-21T10:00:00Z"}"#,
            "op-1",
        )
        .unwrap();
        assert_eq!(
            undecided.happened(),
            None,
            "undecided must not read as a no"
        );
        assert!(
            undecided.detail().contains("may have happened"),
            "{}",
            undecided.detail()
        );

        let unknown = read_answer(
            410,
            r#"{"state":"unknown","error":"past the window"}"#,
            "op-1",
        )
        .unwrap();
        assert_eq!(unknown.happened(), None, "unknown must not read as a no");
        assert!(unknown.detail().contains("will not be run again"));

        // A doer that is not in that warden's binary is a failure with a reason, not a mystery.
        let absent = read_answer(404, r#"{"error":"built without `destroy`"}"#, "op-1").unwrap();
        assert!(
            absent.detail().contains("built without"),
            "{}",
            absent.detail()
        );
        assert_eq!(absent.happened(), Some(false));

        // And a reply that is not JSON at all still lands somewhere honest.
        let junk = read_answer(500, "gateway error", "op-1").unwrap();
        assert_eq!(junk, Answered::Failed("gateway error".into()));
    }

    /// The default address names the machine the warden is on, which is not the one skein is on.
    ///
    /// **What would make this fail**: spelling `default_host` as `127.0.0.1`. From inside the
    /// sandbox that is the SANDBOX, so skein would report "the warden is not running" about a
    /// warden that was running the whole time, and send somebody to start a second one. That was
    /// the live bug (SKEIN-475), and it is what the first assertion holds shut.
    ///
    /// It used to check both deployments, because host-driven skein shared a machine with the
    /// warden and loopback was right there. There is one deployment now (SKEIN-576) — skein is in
    /// the sandbox, the warden is on the host — so there is one crossing and one default, and the
    /// loopback half went with the deployment that made it true.
    #[test]
    fn the_warden_is_looked_for_on_the_host_because_skein_is_not_on_it() {
        let _g = crate::testutil::env_lock();
        std::env::remove_var("SKEIN_WARDEN");

        assert_eq!(
            Warden::configured().host,
            "host.docker.internal",
            "127.0.0.1 from in here is the sandbox, and the warden is not in it"
        );

        // And the override still wins, which is the escape for a fleet whose host is not reachable
        // under that alias — the default names a machine, and naming machines is what breaks.
        std::env::set_var("SKEIN_WARDEN", "10.1.2.3:9999");
        let named = Warden::configured();
        assert_eq!((named.host.as_str(), named.port), ("10.1.2.3", 9999));

        std::env::remove_var("SKEIN_WARDEN");
    }

    /// The secret is read from under the volume, wherever the volume is (§9.5 R5).
    ///
    /// The middle rung is the one that was broken: `$SKEIN_HOME` set, `$SKEIN_WARDEN_HOME` not —
    /// the repointed-volume case, where a fixed `~/.skein/warden` sat outside the cover.
    #[test]
    fn the_secret_is_read_from_under_the_volume() {
        let _g = crate::testutil::env_lock();
        let volume = crate::testutil::tempdir();
        // Bound after `volume`, so the pins go back before the directory they name is removed.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &*volume).unset("SKEIN_WARDEN_HOME");
        std::fs::create_dir_all(volume.join("warden")).unwrap();
        std::fs::write(volume.join("warden/secret"), "under-the-cover\n").unwrap();
        assert_eq!(secret(), "under-the-cover");

        // The explicit override wins over the volume — the operator leaving the covered world.
        let outside = crate::testutil::tempdir();
        std::fs::write(outside.join("secret"), "deliberately-elsewhere\n").unwrap();
        env.set("SKEIN_WARDEN_HOME", &*outside);
        assert_eq!(secret(), "deliberately-elsewhere");
        // And empty is unset, the same reading `skein_home()` gives `$SKEIN_HOME`.
        env.set("SKEIN_WARDEN_HOME", "");
        assert_eq!(secret(), "under-the-cover");
    }

    /// While the derived home is empty, a warden still holding the old default's bytes is answered.
    ///
    /// Read-only, deliberately: the warden owns the move (`warden/src/secret.rs`,
    /// `adopt_left_behind`), and this courtesy exists for the window between upgrading skein and
    /// restarting a long-running warden. The moment the derived home has a secret, it wins.
    #[test]
    fn a_warden_still_on_the_old_default_is_answered_until_the_derived_home_fills() {
        let _g = crate::testutil::env_lock();
        let host_home = crate::testutil::tempdir();
        let volume = crate::testutil::tempdir();
        // Bound after both directories, so the pins go back before either is removed — and from
        // `Drop`, so `$HOME` comes back on the path where an assertion below unwinds past the
        // `match` that used to restore it.
        let mut env = crate::testutil::env_pins();
        env.set("HOME", &*host_home)
            .set("SKEIN_HOME", &*volume)
            .unset("SKEIN_WARDEN_HOME");

        std::fs::create_dir_all(host_home.join(".skein/warden")).unwrap();
        std::fs::write(host_home.join(".skein/warden/secret"), "old-pairing\n").unwrap();
        assert_eq!(secret(), "old-pairing", "the skew window was not answered");
        assert!(
            host_home.join(".skein/warden/secret").exists(),
            "the reader moved the file — writing is the warden's alone"
        );

        std::fs::create_dir_all(volume.join("warden")).unwrap();
        std::fs::write(volume.join("warden/secret"), "moved-in\n").unwrap();
        assert_eq!(
            secret(),
            "moved-in",
            "the derived home did not win once it had a secret"
        );
    }

    /// A warden that is not there says what to do about it.
    #[test]
    fn an_unreachable_warden_names_itself_and_the_fix() {
        // Port 1 on loopback: nothing listens there, and connecting fails immediately.
        let why = Warden::at("127.0.0.1", 1).look().unwrap_err();
        assert!(why.contains("not answering on 127.0.0.1:1"), "{why}");
        assert!(
            why.contains("skein-warden"),
            "the error has to say how to fix it: {why}"
        );
    }

    /// The hand-run line is the same argv, so a blocked fleet is never a dead end.
    #[test]
    fn the_by_hand_line_is_the_argv_that_would_have_run() {
        let line = by_hand(&["create".into(), "--name".into(), "skein fleet".into()]);
        // Every argument quoted, including the ones that do not need it: a line a person pastes has
        // to be right for the argument that *does*, and quoting uniformly is how that stays true
        // when somebody adds one.
        assert_eq!(line, "sbx 'create' '--name' 'skein fleet'");
    }

    /// A far end that behaves the way a *correct* warden never would.
    ///
    /// `tests/warden_roundtrip.rs` spins a real warden and is the right tool for the wire. It is the
    /// wrong tool for these cases: the two states the rule has to survive — advertising a doer and
    /// then refusing it, and refusing everything because it cannot read its own secret — are not
    /// arrangements of a working warden, so they cannot be reached by configuring one. Answers are
    /// keyed on the path, because "advertises on `/v1/fleet` and refuses on `/v1/destroy`" is the
    /// whole of the first case.
    fn fake_warden(reply: impl Fn(&str) -> (u16, String) + Send + Sync + 'static) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut raw = [0u8; 4096];
                let read = stream.read(&mut raw).unwrap_or(0);
                let asked = String::from_utf8_lossy(&raw[..read]).to_string();
                let path = asked.split_whitespace().nth(1).unwrap_or("").to_string();
                let (code, body) = reply(&path);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {code} Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        port
    }

    /// **Shaped like what `fleet::create_argv` actually returns**, verb and all.
    ///
    /// It used to be `["-m", "26g"]` — the tail after `create <sandbox>` — and that fixture is why
    /// `Act::command` shipped prepending a verb of its own: against a synthetic argv with no verb
    /// in it, doubling one looked right. Wired to the real caller it rendered
    /// `sbx create skein-fleet create --name skein-fleet …`, which fails if typed. A fixture that
    /// does not have the shape of the real value tests the fixture.
    fn creating() -> Act {
        Act::Create {
            sandbox: "skein-fleet".into(),
            argv: vec![
                "create".into(),
                "--name".into(),
                "skein-fleet".into(),
                "-m".into(),
                "26g".into(),
            ],
            env: vec![("DOCKER_SANDBOXES_ROOT_SIZE".into(), "200g".into())],
        }
    }

    fn destroying() -> Act {
        Act::Destroy {
            sandbox: "skein-fleet".into(),
        }
    }

    fn publishing() -> Act {
        Act::Publish {
            sandbox: "skein-fleet".into(),
            host_port: 7878,
            sandbox_port: 7878,
        }
    }

    fn withdrawing() -> Act {
        Act::Unpublish {
            sandbox: "skein-fleet".into(),
            host_port: 7878,
            sandbox_port: 7878,
        }
    }

    /// **A withdrawal goes to the warden; a publish never does. That asymmetry IS the design.**
    ///
    /// The two acts differ by one flag and are otherwise the same `sbx ports` line, so it would be
    /// easy — and wrong — to give the warden both. Opening a host port puts a listener into the
    /// network namespace every box shares, which architecture §9.4 makes a prompted act on purpose;
    /// closing one can only ever take something away. `capability::Capability::Unpublish` exists and
    /// there is deliberately no `Publish`, and this is the assertion that keeps it that way: the two
    /// halves are driven against the SAME fake warden, one turn apart, and must not behave alike.
    ///
    /// The fake says yes to everything, so a passing publish arm cannot mean the request merely
    /// failed — it means no request was made, which is what `Act::asked_of` returning `None` is for.
    #[test]
    fn a_port_is_withdrawn_by_the_warden_and_never_opened_by_it() {
        let port = fake_warden(|_| {
            (
                200,
                r#"{"state":"ran","ok":true,"said":"done"}"#.to_string(),
            )
        });
        let warden = Warden::at("127.0.0.1", port);

        match perform_through(&warden, &withdrawing()) {
            Performed::Warden(answered) => assert_eq!(answered.happened(), Some(true)),
            other => panic!(
                "a warden that says yes did not withdraw the mapping, so skein is still asking a \
                 person to undo its own port: {other:?}"
            ),
        }
        match perform_through(&warden, &publishing()) {
            Performed::Prompt(prompt) => assert_eq!(
                prompt.warden_said, None,
                "the same warden was asked to OPEN a port — §9.4 puts that to a person, always"
            ),
            other => panic!("a publish reached a warden: {other:?}"),
        }
    }

    /// Constraint 1: the advertised list is not evidence, so advertising and then refusing prompts.
    ///
    /// This is the shape of a malicious endpoint and of an honest broken one alike — `/v1/fleet`
    /// says `destroy`, `/v1/destroy` will not do it — and the two are indistinguishable from here,
    /// which is exactly why `warden/src/capability.rs` calls its own list "a courtesy to the
    /// client's UI, not evidence about anything". A rule that believed the list would dead-end here
    /// with a fleet that cannot be resized and no line to run.
    #[test]
    fn a_warden_that_advertises_destroy_and_then_refuses_prompts_instead_of_dead_ending() {
        let port = fake_warden(|path| match path {
            "/v1/fleet" => (
                200,
                r#"{"sandboxes":["skein-fleet"],"capabilities":["create","destroy"]}"#.to_string(),
            ),
            _ => (
                409,
                r#"{"error":"nobody approved it at the host"}"#.to_string(),
            ),
        });
        let warden = Warden::at("127.0.0.1", port);

        // It really does advertise it — otherwise this test would pass against a warden that never
        // claimed the doer, which is a different and much easier case.
        assert!(
            warden
                .look()
                .unwrap()
                .capabilities
                .contains(&"destroy".to_string()),
            "the fake did not advertise the doer this case is about"
        );

        match perform_through(&warden, &destroying()) {
            Performed::Prompt(prompt) => {
                assert_eq!(prompt.command, "sbx 'rm' '-f' 'skein-fleet'");
                assert!(
                    prompt
                        .warden_said
                        .as_deref()
                        .unwrap_or_default()
                        .contains("nobody approved it"),
                    "the prompt drops what the warden actually said: {:?}",
                    prompt.warden_said
                );
            }
            other => {
                panic!("a warden that refused what it advertised was not prompted for: {other:?}")
            }
        }
    }

    /// Constraint 2: an unpaired warden prompts, rather than reading as a fleet that can do nothing.
    ///
    /// A warden that cannot read its own secret fails closed and refuses **everything** with a 503
    /// (`warden/src/serve.rs`, `secret.missing()`), including the two endpoints that only report —
    /// and skein presents the secret on every request, so a mispaired `$SKEIN_HOME` produces exactly
    /// this. `docs/live-check.md` §4 warns that it "reads as a broken warden rather than as a
    /// disagreement"; under the rule it reads as a fleet whose privileged commands you now type
    /// yourself, which is a thing a person can act on.
    #[test]
    fn an_unpaired_warden_prompts_for_every_act_rather_than_reading_as_a_fleet_that_can_do_nothing()
    {
        let port = fake_warden(|_| {
            (
                503,
                r#"{"error":"this warden has no secret to check against (/home/x/.skein/warden/secret), so it refuses everything"}"#
                    .to_string(),
            )
        });
        let warden = Warden::at("127.0.0.1", port);

        // The state itself: even the read-only endpoint is refused, which is what makes this look
        // like a warden that can do nothing rather than one that will not do something.
        assert!(
            warden.look().is_err(),
            "the fake is not refusing the reporting endpoint, so it is not the unpaired case"
        );

        for act in [creating(), destroying(), publishing()] {
            match perform_through(&warden, &act) {
                Performed::Prompt(prompt) => assert!(
                    prompt.command.contains("sbx "),
                    "{act:?} was prompted without a command to run: {prompt:?}"
                ),
                other => panic!("{act:?} dead-ended against an unpaired warden: {other:?}"),
            }
        }
    }

    /// A reduced build prompts for the doer it lacks, and does not for the one it has.
    ///
    /// The middle case is why this is one rule per operation and not two global modes: a
    /// create-only warden (`warden --no-default-features --features create`) is a fleet where
    /// create is approved at the host and destroy is typed by hand, and no single switch can say
    /// that.
    #[test]
    fn a_reduced_warden_prompts_only_for_the_doer_it_was_built_without() {
        let port = fake_warden(|path| match path {
            "/v1/create" => (
                200,
                r#"{"state":"ran","ok":true,"said":"made"}"#.to_string(),
            ),
            _ => (
                404,
                r#"{"error":"this warden was built without `destroy`. Nothing can turn it on"}"#
                    .to_string(),
            ),
        });
        let warden = Warden::at("127.0.0.1", port);

        assert!(
            matches!(
                perform_through(&warden, &creating()),
                Performed::Warden(Answered::Ran(_))
            ),
            "the doer this warden has was not left to it"
        );
        match perform_through(&warden, &destroying()) {
            Performed::Prompt(prompt) => assert_eq!(prompt.command, "sbx 'rm' '-f' 'skein-fleet'"),
            other => panic!("a doer compiled out of the warden dead-ended: {other:?}"),
        }
    }

    /// Every prompt carries three things, and the third is the one that gets skipped.
    ///
    /// The command, why skein wants it, and what happens if the person declines. A prompt without
    /// the third is an instruction with a decoration: somebody who cannot see the cost of "no" has
    /// not been given a choice. Asserted over all three acts, because the way this rots is one act
    /// growing a prompt that only says what to type.
    #[test]
    fn every_prompt_says_the_command_why_it_is_wanted_and_what_declining_costs() {
        for act in [creating(), destroying(), publishing()] {
            let prompt = act.prompt(None);
            assert!(
                prompt.command.contains("sbx "),
                "{act:?} has no command to type: {prompt:?}"
            );
            for (what, said) in [("why", &prompt.why), ("cost of no", &prompt.if_declined)] {
                assert!(
                    said.len() > 40 && said != &prompt.command,
                    "{act:?} says nothing under {what}: {said:?}"
                );
            }
            let read = prompt.render();
            for part in [&prompt.command, &prompt.why, &prompt.if_declined] {
                assert!(read.contains(part), "the rendering drops a part: {read}");
            }
            assert!(
                read.contains("If you don't:"),
                "the rendering does not put the cost of declining where it can be seen: {read}"
            );
        }

        // The environment is part of the command, not a footnote: a create at 200 GB and the same
        // create at sbx's default 20 GB are different operations, and the line has to be the one
        // that produces the fleet skein was configured for.
        assert_eq!(
            creating().command(),
            "DOCKER_SANDBOXES_ROOT_SIZE='200g' sbx 'create' '--name' 'skein-fleet' '-m' '26g'",
            "the line offered is not the argv the warden would have run — `Act::Create` carries \
             what `Warden::create` sends, so rendering it means `sbx` plus that argv and nothing \
             added in front"
        );
    }

    /// An act that may have happened is never offered to a person to run again.
    ///
    /// The carve-out in the rule, and it is the same distinction §8.2 exists for: `undecided` and
    /// `unknown` are not "it did not happen". Prompting there would offer a *second* execution — a
    /// create over a sandbox that may already exist, a destroy of a fleet that may already be gone —
    /// which is the failure the operation store was built to prevent, arriving through the fallback
    /// instead of through a retry.
    #[test]
    fn an_act_that_may_have_happened_is_never_turned_into_a_prompt() {
        let port = fake_warden(|path| match path {
            "/v1/create" => (
                409,
                r#"{"state":"undecided","started_at":"2026-08-27T10:00:00Z"}"#.to_string(),
            ),
            _ => (
                410,
                r#"{"state":"unknown","error":"past the window"}"#.to_string(),
            ),
        });
        let warden = Warden::at("127.0.0.1", port);

        for act in [creating(), destroying()] {
            match perform_through(&warden, &act) {
                Performed::Uncertain(answered) => assert_eq!(
                    answered.happened(),
                    None,
                    "{act:?} was called uncertain over an answer that is not"
                ),
                other => panic!(
                    "{act:?} may already have happened and was offered for running again: {other:?}"
                ),
            }
        }
    }

    /// A publish is prompted without asking anything, and names how the mapping is taken back.
    ///
    /// There is no `/v1/ports` in `warden/src/serve.rs`, so no warden anywhere has this capability
    /// and "otherwise the person is prompted" is simply always the answer — proved against a fake
    /// that would say yes to anything, so a passing test cannot mean the request merely failed.
    ///
    /// This doc used to say the wording was louder than the other two because **sbx has no
    /// unpublish**, while the assertions below already checked the opposite — the drift SKEIN-457
    /// exists to end, sitting inside the test that catches it. What is true: the mapping IS
    /// recoverable, by a call skein does not have, so the person who runs the publish is also the
    /// only one who can undo it. That is worth saying to them, and it is not the same as permanence.
    #[test]
    fn publishing_a_port_is_prompted_without_asking_any_warden_and_names_how_it_is_taken_back() {
        let port = fake_warden(|_| {
            (
                200,
                r#"{"state":"ran","ok":true,"said":"done"}"#.to_string(),
            )
        });
        match perform_through(&Warden::at("127.0.0.1", port), &publishing()) {
            Performed::Prompt(prompt) => {
                assert_eq!(
                    prompt.command,
                    "sbx 'ports' 'skein-fleet' '--publish' '7878:7878/tcp'"
                );
                assert_eq!(
                    prompt.warden_said, None,
                    "something was asked for an act no warden has an endpoint for"
                );
                // The prompt has to say what a wrong number COSTS, and for years it said the cost
                // was permanence — "sbx has no unpublish". `sbx ports --help` takes `--unpublish`,
                // so that was a false statement made to a person at the moment they were deciding,
                // and this assertion is what kept it there. What is true and worth saying is that
                // the recovery exists and is named.
                assert!(
                    prompt.why.contains("--unpublish"),
                    "the prompt does not tell the person how a wrong port is taken back: {}",
                    prompt.why
                );
                assert!(
                    !prompt.why.contains("no unpublish"),
                    "the prompt still tells a person a wrong port cannot be undone: {}",
                    prompt.why
                );
            }
            other => panic!("a publish was not put to a person: {other:?}"),
        }
    }

    /// What the warden could not write down reaches whoever asked — in every state, as far as
    /// [`Performed`] (SKEIN-554).
    ///
    /// The warden runs an approved command when its log cannot be written and says so in the reply
    /// as `unrecorded` (`warden/src/serve.rs`, `Unrecorded`). A client that parsed the state and
    /// dropped the field would be the silent drop the owner ruled out, moved one process along. So
    /// each arm of `Performed` is driven: a run, a refusal that becomes a prompt, and an undecided
    /// answer — because an `asked` line is as missing from a refusal as a `settled` line is from a
    /// run. The control is the same reply without the field, which must carry nothing.
    #[test]
    fn what_the_warden_could_not_record_reaches_whoever_asked() {
        const WALL: &str = "open /h/.skein-warden/audit.jsonl: Is a directory (os error 21)";
        let quiet = read_answer(200, r#"{"state":"ran","ok":true,"said":"made"}"#, "op-1").unwrap();
        assert!(
            quiet.unrecorded().is_empty(),
            "a reply with no `unrecorded` field was read as carrying one: {quiet:?}"
        );

        let port = fake_warden(|path| match path {
            "/v1/create" => (
                200,
                format!(r#"{{"state":"ran","ok":true,"said":"made","unrecorded":["{WALL}"]}}"#),
            ),
            "/v1/destroy" => (
                409,
                format!(r#"{{"state":"refused","ok":false,"error":"no","unrecorded":["{WALL}"]}}"#),
            ),
            _ => (
                409,
                format!(
                    r#"{{"state":"undecided","started_at":"2026-09-15T10:00:00Z","unrecorded":["{WALL}"]}}"#
                ),
            ),
        });
        let warden = Warden::at("127.0.0.1", port);
        for (act, arm) in [
            (creating(), "Warden"),
            (destroying(), "Prompt"),
            (withdrawing(), "Uncertain"),
        ] {
            let performed = perform_through(&warden, &act);
            let reached = match &performed {
                Performed::Warden(_) => "Warden",
                Performed::Prompt(_) => "Prompt",
                Performed::Uncertain(_) => "Uncertain",
            };
            assert_eq!(
                reached, arm,
                "{act:?} landed in the wrong arm: {performed:?}"
            );
            assert_eq!(
                performed.unrecorded(),
                [WALL],
                "{act:?}: the warden said its log could not take this, and skein dropped it: \
                 {performed:?}"
            );
        }
    }
}
