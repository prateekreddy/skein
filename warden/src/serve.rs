//! The four endpoints, and where the warden listens.
//!
//! # Where it listens, and why that is the answer
//!
//! **Loopback only.** §8 leaves the address open and §2.3 says only that `http` reaches "GitHub, and
//! the warden", so this is decided here with its reason.
//!
//! A box reaches the host through the gateway address — `host.docker.internal`, which resolves to
//! the host's bridge IP, not to `127.0.0.1`. A listener bound to loopback therefore answers host
//! processes and nothing inside the sandbox. Today skein runs on the host, which is exactly the
//! arrangement delivery step 3 wants: **both callers exercised before anything moves.**
//!
//! Step 4 — skein moves inside — is precisely when this has to change, and §9.5 is where it gets
//! decided. Widening the bind now would create the exposure §9.4 describes ("reach to the warden
//! over the gateway, indistinguishable from skein by address or uid") before anything needed it, and
//! the mechanism that would make it safe — the shared secret under the mount cover — is step 4a.
//! So: bind narrow, and let the move be the thing that opens it, deliberately.
//!
//! # What is here and what is not
//!
//! Four endpoints (§8.3). Two are doers behind Cargo features; two only report and have no feature
//! at all. A doer that was not built answers **404** — not 403, not "disabled": the difference
//! between "this warden will not" and "this warden cannot" is the whole of §8.3, and a client that
//! is told the wrong one retries the wrong thing.
//!
//! **Nothing here authenticates.** Connecting is not authenticating (§9.4), and the warden's answer
//! to "who is asking" is not a header — it is a human at the host confirming the operation (§8.1).
//! Until that surface exists, [`crate::doer::Unattended`] refuses every doer, so the endpoints are
//! reachable, honest about themselves, and unable to do anything.

use crate::audit::Log;
use crate::capability;
use crate::doer::{self, Approver};
use crate::flooding::Doorway;
use crate::outcome::{Outcome, Store};
use crate::wire::{read_request, Request, Response};
use serde::Deserialize;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

/// How long a connection may take to send its one request.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything an endpoint needs, assembled once.
pub struct Warden {
    pub store: Store,
    pub log: Log,
    pub approver: Box<dyn Approver>,
    /// One operation in front of a person at a time, and not too many in a minute (§8.5).
    ///
    /// **Doers only.** Fleet observation and the audit sink do not pass through it — see the note in
    /// `flooding.rs` on why rate-limiting the check that gates skein's own first run would let a
    /// flood win by refusal what it could not win by approval.
    pub doorway: Doorway,
}

/// What a doer is asked for, on the wire.
///
/// **`deny_unknown_fields` is the load-bearing attribute.** §8.1's rule is that approval is a fact
/// the approving side writes, never a field the requester supplies — and serde's default is to
/// *ignore* what it does not recognise, which would make `{"approved": true}` a field that is
/// silently dropped rather than one that does not exist. Those are the same outcome and a very
/// different message: a requester who sends it has misunderstood the boundary, and being told so is
/// how they find out.
///
/// It also forecloses the other half of §8.4 by construction. There is no field here for display
/// text, so the warden cannot be handed a description that disagrees with the arguments it will run
/// — and sending one is an error rather than something quietly ignored.
///
/// The cost is a wire that cannot be extended without both ends moving. For this component that is
/// the right trade: there is one client, and a field the warden does not understand is exactly the
/// thing it should not accept.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asked {
    operation: String,
    sandbox: String,
    #[serde(default)]
    args: Vec<String>,
    /// Environment for the command. Part of what will run, so part of what a person is shown.
    #[serde(default)]
    env: Vec<(String, String)>,
}

/// What the audit sink is told, on the wire.
#[derive(Debug, Deserialize)]
struct Told {
    #[serde(default)]
    operation: String,
    what: String,
    #[serde(default)]
    detail: String,
    /// Who is claiming this. Recorded as a claim — this endpoint cannot check one, and pretending
    /// otherwise would put a false attribution in the one log that is supposed to settle arguments.
    reported_by: String,
}

impl Warden {
    /// Serve until the listener is dropped. One thread per connection, one request per connection.
    ///
    /// Threads rather than a runtime: the warden takes a handful of requests a day, every one of
    /// them gated on a person, and an async runtime is the largest dependency it could acquire for
    /// the least reason.
    pub fn serve(self: Arc<Self>, listener: TcpListener) {
        for stream in listener.incoming().flatten() {
            let warden = Arc::clone(&self);
            std::thread::spawn(move || warden.answer_one(stream));
        }
    }

    fn answer_one(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
        let reply = match read_request(&mut stream) {
            Ok(request) => self.route(&request),
            Err(refusal) => refusal,
        };
        let _ = reply.write_to(&mut stream);
    }

    /// The four endpoints.
    pub fn route(&self, request: &Request) -> Response {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/v1/fleet") => self.fleet(),
            ("POST", "/v1/audit") => self.audit(request),
            ("POST", "/v1/create") => self.doer(request, capability::Capability::Create),
            ("POST", "/v1/destroy") => self.doer(request, capability::Capability::Destroy),
            (_, "/v1/fleet") | (_, "/v1/audit") | (_, "/v1/create") | (_, "/v1/destroy") => {
                Response::fault(405, "that endpoint does not take this method")
            }
            _ => Response::fault(
                404,
                "this warden serves /v1/fleet, /v1/audit, /v1/create and /v1/destroy",
            ),
        }
    }

    /// Fleet observation, and the capability list with it.
    ///
    /// The list rides on the observation rather than having an endpoint of its own — §8 says four,
    /// and "what this warden can do" is part of what there is to see. It is a courtesy to the
    /// client's UI and nothing more: **advertisement decides what skein offers; it never decides
    /// what skein believes.**
    fn fleet(&self) -> Response {
        match crate::sightings::look() {
            Ok(seen) => Response::json(
                200,
                serde_json::json!({
                    "sandboxes": seen.sandboxes,
                    "capabilities": capability::linked(),
                })
                .to_string(),
            ),
            // The capabilities still answer. A listing that failed is not a warden that cannot say
            // what it is, and a client that could not tell those apart would treat a wedged `sbx`
            // daemon as a warden with no doers.
            Err(why) => Response::json(
                500,
                serde_json::json!({
                    "error": format!("the sandboxes could not be listed: {why}"),
                    "capabilities": capability::linked(),
                })
                .to_string(),
            ),
        }
    }

    fn audit(&self, request: &Request) -> Response {
        let told: Told = match serde_json::from_slice(&request.body) {
            Ok(told) => told,
            Err(e) => return Response::fault(400, &format!("that is not an audit entry: {e}")),
        };
        let entry = crate::audit::Entry {
            // Stamped here. A reporter that supplies its own time supplies the order of the log.
            at: chrono::Utc::now().to_rfc3339(),
            operation: told.operation,
            what: told.what,
            detail: told.detail,
            reported_by: told.reported_by,
        };
        match self.log.append(&entry) {
            Ok(()) => Response::json(200, r#"{"recorded":true}"#),
            Err(why) => Response::fault(500, &format!("it could not be recorded: {why}")),
        }
    }

    /// A doer, if this warden has one.
    fn doer(&self, request: &Request, which: capability::Capability) -> Response {
        // 404, because it is not here — see the module note on why that is not 403.
        if !capability::is_linked(which) {
            return Response::fault(
                404,
                &format!(
                    "this warden was built without `{}`. Nothing can turn it on: the doer is not in \
                     the binary (architecture §8.3).",
                    which.name()
                ),
            );
        }
        let asked: Asked = match serde_json::from_slice(&request.body) {
            Ok(asked) => asked,
            Err(e) => {
                // Named, because the field somebody is most likely to send is the one whose absence
                // is the whole design, and "unknown field `approved`" alone does not explain why.
                let said = e.to_string();
                let why = match said.contains("approved") || said.contains("approve") {
                    true => format!(
                        "this warden has no `approved` field, and adding one to the request would                          not create it: approval is a fact the approving side writes, confirmed by                          a human at the host (architecture §8.1). {said}"
                    ),
                    false => format!("that is not an operation: {said}"),
                };
                return Response::fault(400, &why);
            }
        };
        let op = doer::Request {
            operation: asked.operation,
            sandbox: asked.sandbox,
            args: asked.args,
            env: asked.env,
        };
        let _ = self.log.record(&op.operation, "asked", which.name());

        // Before anything else that costs a person attention. Held for the whole operation and
        // released on every path out, including a panic — see `flooding::Turn`.
        let _turn = match self.doorway.enter(&op.operation) {
            Ok(turn) => turn,
            Err(refused) => {
                let why = refused.why();
                let _ = self.log.record(&op.operation, "refused", &why);
                return Response::fault(429, &why);
            }
        };

        // At-most-once, around the whole of it. The approval is inside, so a retry of an operation a
        // person already refused is answered with the refusal rather than asking them again — which
        // is how approval fatigue is manufactured (§8.5).
        let ran = self.store.once(&op.operation, || match which {
            #[cfg(feature = "create")]
            capability::Capability::Create => doer::create(self.approver.as_ref(), &op),
            #[cfg(feature = "destroy")]
            capability::Capability::Destroy => doer::destroy(self.approver.as_ref(), &op),
            #[allow(unreachable_patterns)]
            _ => Err("this warden was built without that doer".into()),
        });

        match ran {
            Ok(outcome) => {
                let _ = self
                    .log
                    .record(&op.operation, "settled", &describe(&outcome));
                answer(&outcome)
            }
            Err(why) => {
                let _ = self.log.record(&op.operation, "refused", &why);
                Response::fault(409, &why)
            }
        }
    }
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ran(Ok(said)) => format!("ran: {said}"),
        Outcome::Ran(Err(why)) => format!("ran and failed: {why}"),
        Outcome::Replayed(_) => "replayed a previous outcome".into(),
        Outcome::Undecided { started_at } => format!("undecided since {started_at}"),
        Outcome::Unknown => "past the retention window".into(),
    }
}

/// Turn an outcome into a reply.
///
/// **`state` and `ok` are two fields because they are two questions**, and the first draft of this
/// collapsed them: `Ran(Err)` and `Replayed(Err)` both rendered as `"failed"`, so a client could not
/// tell an operation that had just failed from one that failed an hour ago and is being answered
/// rather than re-run. That distinction is the entire point of §8.2 — a caller that reads a replay
/// as a fresh attempt has learnt nothing from the retry it just made. Its own test caught it.
///
/// So: `state` is what the **warden** did (ran it, answered from the record, cannot say), and `ok`
/// is what the **operation** did. The status code follows `ok`, because that is what a client's
/// error handling keys on.
fn answer(outcome: &Outcome) -> Response {
    let body = |state: &str, extra: serde_json::Value| {
        let mut value = serde_json::json!({ "state": state });
        if let (Some(map), Some(more)) = (value.as_object_mut(), extra.as_object()) {
            map.extend(more.clone());
        }
        value.to_string()
    };
    match outcome {
        Outcome::Ran(Ok(said)) => Response::json(
            200,
            body("ran", serde_json::json!({ "ok": true, "said": said })),
        ),
        Outcome::Replayed(Ok(said)) => Response::json(
            200,
            body("replayed", serde_json::json!({ "ok": true, "said": said })),
        ),
        Outcome::Ran(Err(why)) => Response::json(
            409,
            body("ran", serde_json::json!({ "ok": false, "error": why })),
        ),
        Outcome::Replayed(Err(why)) => Response::json(
            409,
            body("replayed", serde_json::json!({ "ok": false, "error": why })),
        ),
        // Not an error and not a success: the honest answer to "did it happen?" is that nobody
        // knows, and a client that treats this as failure retries a destroy.
        Outcome::Undecided { started_at } => Response::json(
            409,
            body("undecided", serde_json::json!({ "started_at": started_at })),
        ),
        Outcome::Unknown => Response::json(
            410,
            body(
                "unknown",
                serde_json::json!({
                    "error": "this operation is older than the warden's retention window; its \
                              outcome is not recoverable and it will not be run again"
                }),
            ),
        ),
    }
}

/// Bind loopback on `port`. See the module note for why loopback and nothing else.
///
/// Port 0 gives an ephemeral one, which is how the tests get an address without racing for a fixed
/// number — and is worth having in production too, for a second warden on a machine that already
/// has one.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doer::Unattended;
    use std::io::{Read, Write};

    fn scratch(what: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "skein-warden-serve-{what}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn warden(dir: &std::path::Path) -> Arc<Warden> {
        Arc::new(Warden {
            store: Store::new(dir.join("outcomes"), Duration::from_secs(3600)),
            log: Log::new(dir.join("warden.jsonl")),
            approver: Box::new(Unattended),
            doorway: Doorway::new(),
        })
    }

    fn ask(warden: &Warden, method: &str, path: &str, body: &str) -> Response {
        warden.route(&Request {
            method: method.into(),
            path: path.into(),
            body: body.as_bytes().to_vec(),
        })
    }

    /// The four endpoints, over a real socket, and the capability list the running warden reports.
    ///
    /// **Asked of the warden, not read off the source**, which is what the item requires: the list
    /// comes back over HTTP from a process that has already been built.
    #[test]
    fn a_running_warden_answers_on_loopback_and_says_what_it_can_do() {
        let _env = crate::env_lock();
        let dir = scratch("live");
        let listener = bind(0).expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        assert!(
            addr.ip().is_loopback(),
            "the warden bound something a box could reach: {addr}"
        );
        std::env::set_var(
            "SKEIN_WARDEN_LS_CMD",
            r#"printf '[{"name":"skein-fleet"}]'"#,
        );
        std::thread::spawn(move || warden(&dir).serve(listener));

        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"GET /v1/fleet HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut said = String::new();
        stream.read_to_string(&mut said).unwrap();
        std::env::remove_var("SKEIN_WARDEN_LS_CMD");

        assert!(said.starts_with("HTTP/1.1 200 OK"), "{said}");
        let body = said.split("\r\n\r\n").nth(1).unwrap_or_default();
        let seen: serde_json::Value = serde_json::from_str(body).expect(body);
        assert_eq!(seen["sandboxes"][0], "skein-fleet");
        let can: Vec<String> = serde_json::from_value(seen["capabilities"].clone()).unwrap();
        assert_eq!(
            can,
            capability::linked()
                .iter()
                .map(|c| c.name().to_string())
                .collect::<Vec<_>>(),
            "what it says it can do must be what it was built with"
        );
    }

    /// A doer that is here refuses, because nobody can approve it yet — and the refusal is a 409
    /// naming the missing surface, not a 404 or a 500.
    #[test]
    #[cfg(all(feature = "create", feature = "destroy"))]
    fn a_doer_with_no_approval_surface_refuses_and_says_so() {
        let dir = scratch("refuse");
        let w = warden(&dir);
        let refused = ask(
            &w,
            "POST",
            "/v1/create",
            r#"{"operation":"op-1","sandbox":"skein-fleet","args":["--memory","26g"]}"#,
        );
        assert_eq!(refused.code, 409, "{}", refused.body);
        assert!(
            refused.body.contains("no approval surface"),
            "{}",
            refused.body
        );

        // And it is in the log, both halves: what was asked and what was decided.
        let raw = std::fs::read_to_string(w.log.path()).unwrap();
        assert!(
            raw.contains("\"asked\"") && raw.contains("\"settled\""),
            "{raw}"
        );
    }

    /// Asking twice is answered, not obeyed — the outcome store wrapped around the whole doer,
    /// approval included, so a refusal is not re-put to a person on every retry.
    #[test]
    #[cfg(feature = "create")]
    fn a_repeated_operation_is_replayed_rather_than_re_approved() {
        struct Counting(std::sync::atomic::AtomicUsize);
        impl Approver for Counting {
            fn approve(&self, _: &doer::Request, _: &str) -> Result<(), String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("the person said no".into())
            }
        }
        let dir = scratch("replay");
        let counted = Arc::new(Counting(std::sync::atomic::AtomicUsize::new(0)));
        let w = Warden {
            store: Store::new(dir.join("outcomes"), Duration::from_secs(3600)),
            log: Log::new(dir.join("warden.jsonl")),
            approver: Box::new(CountingRef(Arc::clone(&counted))),
            doorway: Doorway::new(),
        };
        struct CountingRef(Arc<Counting>);
        impl Approver for CountingRef {
            fn approve(&self, r: &doer::Request, what: &str) -> Result<(), String> {
                self.0.approve(r, what)
            }
        }
        let body = r#"{"operation":"op-2","sandbox":"skein-fleet"}"#;
        let first = ask(&w, "POST", "/v1/create", body);
        let again = ask(&w, "POST", "/v1/create", body);
        assert_eq!(first.code, 409);
        assert_eq!(again.code, 409);
        assert!(
            first.body.contains(r#""state":"ran""#),
            "the first attempt must say it ran: {}",
            first.body
        );
        assert!(
            again.body.contains(r#""state":"replayed""#),
            "a retry must be answered from the record, and say that it was: {}",
            again.body
        );
        assert_eq!(
            counted.0.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a retry put the same question to a person a second time"
        );
    }

    /// A requester cannot claim its own approval, and cannot say what a person will be shown.
    ///
    /// The done-when of §8.1, at the wire. Both are the same mechanism: there is no field for
    /// either, and `deny_unknown_fields` makes sending one an error rather than something silently
    /// dropped — which matters, because "ignored" and "does not exist" look identical from the
    /// outside and only one of them is a boundary somebody can rely on.
    #[test]
    #[cfg(feature = "destroy")]
    fn a_request_cannot_approve_itself_or_choose_what_a_person_is_shown() {
        let dir = scratch("claims");
        let w = warden(&dir);

        let claimed = ask(
            &w,
            "POST",
            "/v1/destroy",
            r#"{"operation":"op-9","sandbox":"skein-fleet","approved":true}"#,
        );
        assert_eq!(claimed.code, 400, "{}", claimed.body);
        assert!(
            claimed
                .body
                .contains("approval is a fact the approving side writes"),
            "the refusal has to say why the field does not exist: {}",
            claimed.body
        );

        // And the display half: a request that tries to describe itself as something harmless while
        // carrying the arguments of a destroy. There is nowhere to put the description.
        let two_faced = ask(
            &w,
            "POST",
            "/v1/destroy",
            r#"{"operation":"op-10","sandbox":"skein-fleet","display":"a harmless health check"}"#,
        );
        assert_eq!(two_faced.code, 400, "{}", two_faced.body);
        assert!(
            two_faced.body.contains("unknown field"),
            "{}",
            two_faced.body
        );

        // Nothing was recorded as asked, because nothing parsed as an operation.
        let log = std::fs::read_to_string(w.log.path()).unwrap_or_default();
        assert!(!log.contains("op-9") && !log.contains("op-10"), "{log}");
    }

    /// End to end with a person at the terminal: the same request refused, then approved.
    ///
    /// The doer runs `sbx`, which is not here — so the approved case is expected to fail at the
    /// command, and that is the assertion. `ran` versus `refused at the host` is the difference
    /// between "the approval was consulted and passed" and "it was never asked".
    #[test]
    #[cfg(feature = "destroy")]
    fn an_approval_at_the_terminal_is_what_lets_a_doer_reach_its_command() {
        use crate::approval::Console;
        let dir = scratch("terminal");
        let refuser = Warden {
            store: Store::new(dir.join("no"), Duration::from_secs(3600)),
            log: Log::new(dir.join("no.jsonl")),
            approver: Box::new(Console::over(
                Box::new(std::io::Cursor::new(
                    b"y
"
                    .to_vec(),
                )),
                Box::new(std::io::sink()),
            )),
            doorway: Doorway::new(),
        };
        let said_no = ask(
            &refuser,
            "POST",
            "/v1/destroy",
            r#"{"operation":"op-11","sandbox":"skein-fleet"}"#,
        );
        assert_eq!(said_no.code, 409);
        assert!(
            said_no.body.contains("refused at the host"),
            "{}",
            said_no.body
        );

        let approver = Warden {
            store: Store::new(dir.join("yes"), Duration::from_secs(3600)),
            log: Log::new(dir.join("yes.jsonl")),
            approver: Box::new(Console::over(
                Box::new(std::io::Cursor::new(
                    b"op-12
"
                    .to_vec(),
                )),
                Box::new(std::io::sink()),
            )),
            doorway: Doorway::new(),
        };
        let said_yes = ask(
            &approver,
            "POST",
            "/v1/destroy",
            r#"{"operation":"op-12","sandbox":"skein-fleet"}"#,
        );
        assert!(
            !said_yes.body.contains("refused at the host"),
            "the approval was not consulted: {}",
            said_yes.body
        );
        assert!(
            said_yes.body.contains("could not run `sbx`") || said_yes.body.contains("exited"),
            "an approved operation must reach its command: {}",
            said_yes.body
        );
    }

    /// The audit sink takes what it is told and records who told it — as a claim.
    #[test]
    fn the_audit_sink_records_a_report_as_a_report() {
        let dir = scratch("audit");
        let w = warden(&dir);
        let ok = ask(
            &w,
            "POST",
            "/v1/audit",
            r#"{"operation":"op-3","what":"approved","detail":"git-write for box x","reported_by":"skein"}"#,
        );
        assert_eq!(ok.code, 200, "{}", ok.body);
        let raw = std::fs::read_to_string(w.log.path()).unwrap();
        let entry: crate::audit::Entry = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(entry.reported_by, "skein");
        assert_eq!(entry.what, "approved");

        assert_eq!(ask(&w, "POST", "/v1/audit", "not json").code, 400);
    }

    /// A flood of proposals is refused, and the endpoint that lets skein start is untouched by it.
    ///
    /// The subtle half of §8.5 and the reason the doorway is on the doers only: rate-limiting fleet
    /// observation would let a flood achieve by refusal what it could not achieve by approval — skein
    /// unable to run the check that gates its own first run. So this floods until the limit bites,
    /// and then reads.
    #[test]
    #[cfg(feature = "create")]
    fn a_flood_is_refused_and_the_reading_endpoint_still_answers() {
        let _env = crate::env_lock();
        let dir = scratch("flood");
        let w = warden(&dir);
        std::env::set_var(
            "SKEIN_WARDEN_LS_CMD",
            r#"printf '[{"name":"skein-fleet"}]'"#,
        );

        let mut refusals = Vec::new();
        for n in 0..(crate::flooding::PER_MINUTE + 4) {
            let body = format!(r#"{{"operation":"op-flood-{n}","sandbox":"skein-fleet"}}"#);
            refusals.push(ask(&w, "POST", "/v1/create", &body));
        }
        let flooded = refusals
            .iter()
            .filter(|r| r.code == 429 && r.body.contains("more than"))
            .count();
        assert!(
            flooded >= 4,
            "the rate limit never bit: {:?}",
            refusals.iter().map(|r| r.code).collect::<Vec<_>>()
        );

        // And the check skein starts on is answerable throughout.
        let seen = ask(&w, "GET", "/v1/fleet", "");
        std::env::remove_var("SKEIN_WARDEN_LS_CMD");
        assert_eq!(
            seen.code, 200,
            "a flood of proposals made skein unable to start: {}",
            seen.body
        );
        assert!(seen.body.contains("skein-fleet"), "{}", seen.body);

        // The audit sink too — the account of what just happened must survive the thing it is
        // accounting for.
        assert_eq!(
            ask(
                &w,
                "POST",
                "/v1/audit",
                r#"{"what":"noticed a flood","reported_by":"skein"}"#
            )
            .code,
            200
        );
    }

    /// Everything else is a 404 or a 405, and neither is a doer that quietly does nothing.
    #[test]
    fn an_endpoint_that_is_not_there_says_which_ones_are() {
        let dir = scratch("routes");
        let w = warden(&dir);
        let missing = ask(&w, "GET", "/v1/anything", "");
        assert_eq!(missing.code, 404);
        assert!(missing.body.contains("/v1/create"), "{}", missing.body);
        assert_eq!(ask(&w, "GET", "/v1/create", "").code, 405);
        assert_eq!(ask(&w, "POST", "/v1/fleet", "").code, 405);
    }
}
