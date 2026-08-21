//! Asking the host warden to do the two things skein must not do itself (§14's `warden-client`).
//!
//! **Why this exists before it has to.** Create and destroy both terminate skein — create because it
//! does not exist yet, destroy because it will not afterwards — so fleet lifecycle cannot live inside
//! the fleet, permanently (§8). Routing them through the warden *while skein is still on the host* is
//! delivery step 3, and the whole argument for doing it in that order: **both callers are exercised
//! before anything moves.** The wire, the operation ids and the timeout semantics are all shaken out
//! against a real warden while there is still an alternative.
//!
//! # Three rules that are easy to lose
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

use crate::util::sh_quote;
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Where the warden listens. Loopback, and §8.6 says why.
pub const DEFAULT_PORT: u16 = 7879;

/// How long to wait for a reply. Longer than a create takes, because the person at the host has to
/// read the prompt and type an id before the work even starts.
const REPLY: Duration = Duration::from_secs(1800);

/// What came back, in the terms a caller has to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// It happened, and this is what the warden's command said.
    Ran(String),
    /// It happened before, under this same operation id.
    Replayed(String),
    /// It ran and failed, or a person refused it.
    Failed(String),
    /// Accepted and never settled — the warden died between running and recording.
    ///
    /// **Not a failure.** For a destroy, "we do not know" and "it did not happen" license opposite
    /// actions, and a caller that collapses them destroys a fleet twice.
    Undecided(String),
    /// Older than the warden's retention window. Never re-run, and not recoverable.
    Unknown(String),
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
        match self {
            Answered::Ran(d)
            | Answered::Replayed(d)
            | Answered::Failed(d)
            | Answered::Undecided(d)
            | Answered::Unknown(d) => d,
        }
    }
}

/// The warden, as skein addresses it.
pub struct Warden {
    host: String,
    port: u16,
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
}

impl Warden {
    /// The warden this host is configured to ask. `$SKEIN_WARDEN` overrides `host:port`.
    pub fn configured() -> Warden {
        let raw = std::env::var("SKEIN_WARDEN").unwrap_or_default();
        let (host, port) = match raw.trim().rsplit_once(':') {
            Some((h, p)) if !h.is_empty() => (h.to_string(), p.parse().unwrap_or(DEFAULT_PORT)),
            _ => ("127.0.0.1".to_string(), DEFAULT_PORT),
        };
        Warden { host, port }
    }

    pub fn at(host: &str, port: u16) -> Warden {
        Warden {
            host: host.to_string(),
            port,
        }
    }

    /// What the warden can see, and what it says it can do.
    pub fn look(&self) -> Result<Sighting, String> {
        let body = self.send("GET", "/v1/fleet", "")?;
        serde_json::from_str(&body.1)
            .map_err(|e| format!("the warden's listing was unreadable: {e}"))
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
        self.send("POST", "/v1/audit", &body).map(|_| ())
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
        let mut stream = TcpStream::connect((self.host.as_str(), self.port)).map_err(|e| {
            format!(
                "the host warden is not answering on {}:{} ({e}). Fleet create and destroy go \
                 through it — start it with `skein-warden`, somewhere a person can approve what it \
                 asks (architecture §8).",
                self.host, self.port
            )
        })?;
        stream.set_read_timeout(Some(REPLY)).ok();
        stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.host,
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

/// Turn a reply into the five states, keeping every one distinct.
fn read_answer(code: u16, said: &str, operation: &str) -> Result<Answered, String> {
    let parsed: Said = serde_json::from_str(said).unwrap_or(Said {
        state: String::new(),
        said: String::new(),
        error: said.to_string(),
        started_at: String::new(),
    });
    let detail = match parsed.error.trim().is_empty() {
        true => parsed.said.clone(),
        false => parsed.error.clone(),
    };
    match (code, parsed.state.as_str()) {
        (200, "replayed") => Ok(Answered::Replayed(detail)),
        (200, _) => Ok(Answered::Ran(detail)),
        (_, "undecided") => Ok(Answered::Undecided(format!(
            "{operation} was accepted at {} and never settled — it may have happened. Ask the \
             warden about it before doing anything that assumes it did not.",
            parsed.started_at
        ))),
        (410, _) | (_, "unknown") => Ok(Answered::Unknown(format!(
            "{operation} is older than the warden's retention window, so what happened to it is \
             not recoverable — and it will not be run again. {detail}"
        ))),
        // A 404 on a doer is §8.3: it is not in that warden's binary. Reported as a failure with the
        // reason, never as "it did not work" — the two send a person to different places.
        (404, _) => Ok(Answered::Failed(format!(
            "this warden was built without that operation. {detail}"
        ))),
        (429, _) => Ok(Answered::Failed(detail)),
        (_, _) => Ok(Answered::Failed(detail)),
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
}
