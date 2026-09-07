//! The four endpoints, and where the warden listens.
//!
//! # Where it listens, and why that is the answer
//!
//! **Loopback, and the host's own address on each Docker bridge. Never `0.0.0.0`** — the owner
//! decided that, and §9.5 records it. §8 left the address open and §2.3 says only that `http`
//! reaches "GitHub, and the warden", so the reason lives here with the code.
//!
//! ## What was measured, and what it corrects
//!
//! The note this replaces asserted that a loopback listener "answers host processes and nothing
//! inside the sandbox", because a box reaches the host through the gateway address rather than
//! through `127.0.0.1`. **That is true on a Linux host and false on Docker Desktop**, and the
//! whole of §8.6 was built on it. From inside the fleet sandbox, against a warden bound to
//! `Ipv4Addr::LOCALHOST` on a macOS host, `host.docker.internal` resolves to `169.254.1.1` and
//! port 7879 answers — with this crate's own 401, so it is this process and not something else
//! on the port. Docker Desktop proxies the gateway address from the host side, so the connection
//! arrives at the host's loopback and the barrier the sentence described was never there.
//!
//! The rule that catches this is the repo's: derive, do not assert. A sentence about what a
//! kernel does reproduces or it drifts, and this one had drifted across two documents and a
//! health check before anybody opened a socket.
//!
//! ## So why widen at all
//!
//! Because the claim is right where it was always right. On a **Linux** host the gateway is a real
//! bridge address and a loopback listener genuinely does answer nothing inside the sandbox — so
//! the fleet's create and destroy would have no path at all there. Binding the bridge is what
//! gives that host what Docker Desktop hands this one for free, and [`bridge_addresses`] finds
//! nothing on a host that has no bridge, which is why the same code is right on both.
//!
//! ## Why not `0.0.0.0`
//!
//! It is one line and it works everywhere, and it puts port 7879 on whatever network the laptop is
//! attached to, with the shared secret as the only thing between a café and `sbx rm -f`. The
//! bridge is reachable from the sandbox and from nowhere else. §9.4's exposure — "reach to the
//! warden over the gateway, indistinguishable from skein by address or uid" — is the one this
//! deliberately accepts, and step 4a's shared secret is what pays for it: [`crate::secret`] is
//! checked before anything is routed, so reaching the port and being skein are different things.
//!
//! # What is here and what is not
//!
//! Four endpoints (§8.3). Two are doers behind Cargo features; two only report and have no feature
//! at all. A doer that was not built answers **404** — not 403, not "disabled": the difference
//! between "this warden will not" and "this warden cannot" is the whole of §8.3, and a client that
//! is told the wrong one retries the wrong thing.
//!
//! **Who is asking is answered twice, and the two answers are different questions.** A doer runs
//! because a human at the host confirmed it (§8.1) — that has not changed and is not a header. What
//! a header now answers is whether the caller is skein at all: [`crate::secret`] is checked before
//! anything is routed, so the two reporting endpoints are not readable by whoever can open the port
//! and §8.5's doorway cannot be spent by somebody who was never going to be approved. It is what
//! the bind widening above spends: the narrow bind used to stand beside the secret, and on a Linux
//! host it no longer does.

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
    /// What tells skein from anybody else who can open the port (§9.5 R5).
    pub secret: crate::secret::Secret,
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
    ///
    /// **It is a narrower claim than it was.** Since §9.5 R5, the caller held the shared secret, so
    /// the field says "whoever holds skein's secret says this is skein" rather than "whoever could
    /// open the port". That is worth the sentence: it is not a check, and it is no longer nothing.
    reported_by: String,
}

impl Warden {
    /// Serve until the listeners are dropped. One thread per listener, one thread per connection,
    /// one request per connection.
    ///
    /// Threads rather than a runtime: the warden takes a handful of requests a day, every one of
    /// them gated on a person, and an async runtime is the largest dependency it could acquire for
    /// the least reason.
    pub fn serve(self: Arc<Self>, mut listeners: Vec<TcpListener>) {
        // The last one is served on this thread so `serve` still blocks for as long as the warden
        // is up. Handing every listener to a spawned thread would return immediately and the
        // caller's `main` would exit under a warden that was working.
        let Some(here) = listeners.pop() else { return };
        for listener in listeners {
            let warden = Arc::clone(&self);
            std::thread::spawn(move || warden.accept_on(listener));
        }
        self.accept_on(here);
    }

    fn accept_on(self: Arc<Self>, listener: TcpListener) {
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
        // **Before the path is even looked at.** Putting it here rather than per endpoint is the
        // same argument the cockpit's gate makes: an endpoint added later is guarded on the day it
        // is added, rather than on the day somebody remembers.
        if self.secret.missing() {
            // Fails closed, and says which failure it is. "I cannot check" and "you are wrong" send
            // whoever is reading to completely different places, and the first is the one an
            // operator can fix.
            return Response::fault(
                503,
                &format!(
                    "this warden has no secret to check against ({}), so it refuses everything — \
                     it mints one at start when that path is writable",
                    self.secret.where_().display()
                ),
            );
        }
        if !self.secret.matches(&request.secret) {
            return Response::fault(
                401,
                "this warden does not know who is asking — skein presents the secret from under \
                 the mount cover, and nothing else can read it (architecture §9.5 R5)",
            );
        }
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/v1/fleet") => self.fleet(),
            ("POST", "/v1/audit") => self.audit(request),
            ("POST", "/v1/create") => self.doer(request, capability::Capability::Create),
            ("POST", "/v1/destroy") => self.doer(request, capability::Capability::Destroy),
            ("POST", "/v1/unpublish") => self.doer(request, capability::Capability::Unpublish),
            (_, "/v1/fleet")
            | (_, "/v1/audit")
            | (_, "/v1/create")
            | (_, "/v1/destroy")
            | (_, "/v1/unpublish") => {
                Response::fault(405, "that endpoint does not take this method")
            }
            _ => Response::fault(
                404,
                "this warden serves /v1/fleet, /v1/audit, /v1/create, /v1/destroy and \
                 /v1/unpublish",
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
            reported_by: claimed_by(&told.reported_by),
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
                        "this warden has no `approved` field, and adding one to the request would not \
                         create it: approval is a fact the approving side writes, confirmed \
                         by a human at the host (architecture §8.1). {said}"
                    ),
                    false => format!("that is not an operation: {said}"),
                };
                return Response::fault(400, &why);
            }
        };
        // Before the audit entry, because that entry names the operation id and an id is one of the
        // things being vetted — a log line written from an unvetted request is a log line the
        // requester composed. Before the doorway and the store for the same reason in a different
        // currency: a malformed request must not spend the one outstanding slot, and must not claim
        // an id.
        let op = match vetted(asked, which) {
            Ok(op) => op,
            Err(why) => return Response::fault(400, &why),
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
            #[cfg(feature = "unpublish")]
            capability::Capability::Unpublish => doer::unpublish(self.approver.as_ref(), &op),
            #[allow(unreachable_patterns)]
            _ => crate::outcome::Did::Never("this warden was built without that doer".into()),
        });

        match ran {
            // `settled` is for an operation that reached `sbx`; `refused` for one that did not.
            // They used to be the same line, so the one log that is supposed to settle an argument
            // recorded "ran and failed" for a create nobody had approved.
            Ok(Outcome::Refused(why)) => {
                let _ = self.log.record(&op.operation, "refused", &why);
                answer(&Outcome::Refused(why))
            }
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

/// Who a reporter is recorded as — which is never the warden itself.
///
/// `Entry::reported_by` carries two kinds of line: the warden's own account, written by
/// `audit::Log::record`, and a claim by whoever called `/v1/audit`. The field is the only thing
/// telling them apart, and it was taken verbatim from the request — so skein could file an entry
/// that read exactly like the warden's own, in the one log that exists because **skein cannot audit
/// itself** (§5). A claim to be the warden is kept rather than dropped: the attempt is worth more in
/// the record than out of it, and `claimed:` is the prefix that says which it was.
///
/// It is rewritten rather than refused because this endpoint's job is to accept what it is told —
/// §8.5 exempts it from the doorway for that reason. Refusing would let a reporter choose between
/// being recorded honestly and not being recorded at all.
///
/// **The match folds ASCII case, and the claim is kept as it was spelled.** The log is evidence a
/// person reads in a disagreement, so `Warden` sitting in a column of `warden`s is the forgery
/// again for every reader who is skimming and for every `grep -i` — an exact comparison is the
/// right check for a machine and the wrong one for the audience this log has. Folding it does not
/// cost the record anything, because what gets stored is the reporter's own spelling behind the
/// prefix (`claimed:Warden`), not the constant: marking a claim must not quietly edit it.
///
/// It is ASCII case and nothing more. A Unicode lookalike — `wardеn` with a Cyrillic `е` — is
/// stored verbatim and reads as the warden to a person, and no comparison here would settle that;
/// the claim this function makes is the narrow one it can keep.
fn claimed_by(reported_by: &str) -> String {
    let claim = reported_by.trim();
    match claim.eq_ignore_ascii_case(crate::audit::THE_WARDEN) {
        true => format!("claimed:{claim}"),
        false => reported_by.to_string(),
    }
}

/// Every environment key a doer may be handed, and nothing else reaches `sbx`.
///
/// **An allow-list, per verb, because the environment decides what runs and the argv does not say
/// so.** [`doer::run`] spells the program as the literal `"sbx"`, which is a relative name, and
/// Rust resolves a relative program through the `PATH` set on the `Command` — so `PATH` in a
/// request chooses which binary the host uid executes, while the approval text still reads
/// `sbx rm -f skein-fleet`. `LD_PRELOAD`, `DOCKER_HOST` and `DOCKER_CONFIG` are the same shape.
/// Rendering the environment (which every doer now does) is necessary and is not sufficient: a
/// person reading `PATH=/tmp/x sbx create …` is being shown the truth in a form that does not look
/// like the thing it is.
///
/// **Per verb rather than one list**, because the answer for two of the three is "none". skein sends
/// an environment on `create` alone — `Warden::destroy` and `Warden::unpublish` pass `&[]`
/// (`src/warden_client.rs`) — and `sbx rm -f` reads nothing from it. A shared list would have made
/// destroy carry a key it has no use for, which is the accident this is closing rather than a
/// smaller version of it.
///
/// **One key, and widening it means both ends move.** `skein::fleet::create_env` returns
/// `DOCKER_SANDBOXES_ROOT_SIZE` or nothing at all, and sbx takes disk from the environment because
/// its argv has no flag for it. That is the same trade `Asked`'s `deny_unknown_fields` makes, for
/// the same reason: there is one client, and a key the warden does not understand is exactly the
/// thing it should not pass to a privileged command.
fn env_a_doer_may_carry(which: capability::Capability) -> &'static [&'static str] {
    match which {
        capability::Capability::Create => &["DOCKER_SANDBOXES_ROOT_SIZE"],
        capability::Capability::Destroy | capability::Capability::Unpublish => &[],
    }
}

/// The most a single argument or environment value may be, and how many arguments there may be.
///
/// Not a security boundary — the guards above it are — but a bound on what can be put in front of a
/// person. An approval nobody can read to the end is one that gets answered by rhythm, which is the
/// failure §8.1 makes them type the id to avoid. The real `sbx create` line carries about fifteen
/// arguments plus one per mount, and sbx's own limit on those is 25.
const MOST_ARGS: usize = 128;
const LONGEST_VALUE: usize = 4096;

/// What a person can be shown without the terminal lying about it.
///
/// Printable ASCII and nothing else. The prompt is written to a terminal, and outside this range
/// live every way a string can render as something other than itself: `\n` and `\r` repaint the
/// lines above, `\x1b[` drives the cursor anywhere on the screen, `\x08` deletes what was already
/// drawn, and beyond ASCII the bidirectional overrides reorder a line without changing a byte of it.
///
/// A whitelist rather than a list of the dangerous ones, for `skein::util::valid_name`'s reason: a
/// deny-list has to anticipate every escape of every terminal it is ever read on, and there is no
/// version of this that is worth getting nearly right. The cost is a host path with a non-ASCII
/// character in it, which would arrive here inside a mount argument and be refused — visibly, with
/// the argument named, and skein's own fallback then offers the person the line to run by hand
/// (`skein::warden_client::perform_through`), so it is a detour rather than a dead end.
fn readable(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
}

/// The request, or why it is not one — checked once, at the wire, for every doer.
///
/// §8.4's rule is that the warden renders the resolved arguments it will itself execute. That was
/// true and it was not enough: what it renders is still made of bytes the requester chose, so the
/// rule needs the sentence under it, which is that **nothing can reach the approval text that the
/// approval text cannot show.** Everything below is that one sentence applied to each field.
fn vetted(asked: Asked, which: capability::Capability) -> Result<doer::Request, String> {
    // The same grammar the outcome store applies, rather than a second one beside it: the id names
    // a file there and a line of the prompt here, and two guards that agree today are two guards.
    crate::outcome::checked_id(&asked.operation)?;

    // `skein::util::valid_name`'s class, written out again because the warden deliberately cannot
    // import it (see this crate's manifest). A leading `-` argv-parses as a flag wherever a name
    // reaches a command — and `argv_destroy` puts this one straight after `rm -f`.
    let name = &asked.sandbox;
    let named = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('-')
        && !name.contains("..")
        && !name.chars().all(|c| c == '.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !named {
        return Err(format!(
            "{name:?} is not a sandbox name — letters, digits, `.`, `_` and `-`, up to 128 of them, \
             and not beginning with `-`"
        ));
    }

    if asked.args.len() > MOST_ARGS {
        return Err(format!(
            "the {} for {name} carries {} arguments, and more than {MOST_ARGS} is more than an \
             approval can put in front of a person",
            which.name(),
            asked.args.len()
        ));
    }
    for arg in &asked.args {
        if arg.len() > LONGEST_VALUE || !readable(arg) {
            return Err(format!(
                "an argument of the {} for {name} cannot be shown as what it is, so it cannot be \
                 approved: {arg:?}",
                which.name()
            ));
        }
    }

    let allowed = env_a_doer_may_carry(which);
    for (at, (key, value)) in asked.env.iter().enumerate() {
        // **Refused rather than merged, which is `argv_create`'s rule about `--name` in a second
        // currency.** `Command::envs` takes the last value for a repeated key, and `described_env`
        // renders every one of them — so `X=20g X=200g sbx create …` puts two answers on the screen
        // and runs the second. A person who reads the line from the left approves the first.
        if asked.env[..at].iter().any(|(seen, _)| seen == key) {
            return Err(format!(
                "`{key}` is given twice, and `sbx` would take the last one — so what ran would not \
                 be the first thing on the line that was approved"
            ));
        }
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "this warden does not pass `{key}` to a {}. The environment decides what a relative \
                 program name resolves to, so it is an allow-list and not a filter: {}",
                which.name(),
                match allowed.is_empty() {
                    true => format!("a {} carries none at all", which.name()),
                    false => format!("this one takes {}", allowed.join(", ")),
                }
            ));
        }
        if value.len() > LONGEST_VALUE || !readable(value) {
            return Err(format!(
                "the value of `{key}` cannot be shown as what it is, so it cannot be approved: \
                 {value:?}"
            ));
        }
    }

    Ok(doer::Request {
        operation: asked.operation,
        sandbox: asked.sandbox,
        args: asked.args,
        env: asked.env,
    })
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ran(Ok(said)) => format!("ran: {said}"),
        Outcome::Ran(Err(why)) => format!("ran and failed: {why}"),
        Outcome::Replayed(_) => "replayed a previous outcome".into(),
        Outcome::Undecided { started_at } => format!("undecided since {started_at}"),
        Outcome::Unknown => "past the retention window".into(),
        // Never reached: `doer` records a refusal as `refused` before this is consulted. Here so
        // that adding a variant is a compile error rather than a line in the log that says the
        // wrong thing.
        Outcome::Refused(why) => format!("nothing ran: {why}"),
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
        // A third answer to "what did the warden do", and the one the first version could not give:
        // it put the operation to a person and the answer was no. `ok: false` and 409 keep a client
        // that reads only those two on exactly the path it was on before — what is new is that the
        // state is not `ran`, and that asking again asks a person again rather than replaying this.
        Outcome::Refused(why) => Response::json(
            409,
            body("refused", serde_json::json!({ "ok": false, "error": why })),
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

/// The port skein's client asks when `$SKEIN_WARDEN` says nothing.
///
/// Here as well as in `warden_client` because the two crates deliberately do not depend on each
/// other (`tools/module-check.py` asserts it) — they agree by a constant each, and by the test below
/// that would fail if they stopped.
pub const WHERE_SKEIN_LOOKS: u16 = 7879;

/// Whether an interface is a Docker bridge, by the only two names one can have: `docker0` is the
/// default bridge, and a user-defined network gets `br-<id>`.
///
/// A name test rather than a subnet test on purpose. `172.16.0.0/12` is a private range Docker
/// happens to allocate from and a corporate VPN may hand out too, so a subnet rule would widen the
/// bind onto somebody's office network the day their VPN changed — which is the exact outcome
/// this whole derivation exists to avoid.
fn is_docker_bridge(iface: &str) -> bool {
    iface == "docker0" || iface.starts_with("br-")
}

/// The host's own addresses on its Docker bridges — the interface a sandbox reaches it on, and
/// nothing else.
///
/// Derived from two kernel tables rather than from a convention. `/proc/net/route` says which
/// subnets belong to a bridge; `/proc/net/fib_trie` says which addresses are the host's own. The
/// answer is the intersection. The obvious shortcut — "the bridge's address is the `.1` of its
/// subnet" — is true of every Docker install anybody has seen and is still a guess, and a guess
/// here binds a port on an address the host may not own or, worse, misses the one it does.
///
/// **Empty is a correct answer, not a failure.** On macOS and Windows the bridge lives inside
/// Docker's own Linux VM, there is no `/proc` on the host at all, and the loopback bind below is
/// already reachable from a sandbox — measured, see the module note. So the widening is Linux's
/// and nothing else's, and a host that has no bridge listens on loopback exactly as before.
pub fn bridge_addresses() -> Vec<Ipv4Addr> {
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    bridges_in(&read("/proc/net/route"), &read("/proc/net/fib_trie"))
}

/// [`bridge_addresses`] against the text of the two tables, so the derivation can be tested against
/// a host that is not this one.
fn bridges_in(route: &str, fib_trie: &str) -> Vec<Ipv4Addr> {
    // `/proc/net/route` prints a `__be32` with `%08X`, so on a little-endian host the hex is
    // byte-reversed and on a big-endian host it is not. `from_be` is exactly that difference and is
    // right on both — a `swap_bytes` would be right on one.
    let word = |hex: &str| u32::from_str_radix(hex, 16).ok().map(u32::from_be);
    let mut nets: Vec<(u32, u32)> = Vec::new();
    for line in route.lines().skip(1) {
        let mut field = line.split_whitespace();
        let (Some(iface), Some(dest), _, _, _, _, _, Some(mask)) = (
            field.next(),
            field.next(),
            field.next(),
            field.next(),
            field.next(),
            field.next(),
            field.next(),
            field.next(),
        ) else {
            continue;
        };
        if let (true, Some(dest), Some(mask)) = (is_docker_bridge(iface), word(dest), word(mask)) {
            nets.push((dest, mask));
        }
    }

    // A `/32 host LOCAL` in the trie is an address this machine answers to; the line before it is
    // the address itself. Every other entry is a route to somewhere, and binding one would fail.
    let mut found: Vec<Ipv4Addr> = Vec::new();
    let mut previous = "";
    for line in fib_trie.lines() {
        let trimmed = line.trim();
        if trimmed == "/32 host LOCAL" {
            if let Some(addr) = previous
                .trim()
                .strip_prefix("|-- ")
                .and_then(|a| a.parse::<Ipv4Addr>().ok())
            {
                let bits = u32::from(addr);
                if nets.iter().any(|(net, mask)| bits & mask == *net) && !found.contains(&addr) {
                    found.push(addr);
                }
            }
        }
        previous = line;
    }
    found.sort();
    found
}

/// Where the warden listens: loopback, and the host's own address on each Docker bridge. Never
/// `0.0.0.0` — see the module note for the decision and who made it.
///
/// Port 0 gives an ephemeral one, which is how the tests get an address without racing for a fixed
/// number — and is worth having in production too, for a second warden on a machine that already
/// has one.
///
/// A bridge address that is found and cannot be bound is an error rather than a warning. The whole
/// point of the widening is that skein inside the fleet can reach this process, and a warden that
/// started "successfully" on loopback alone would be unreachable from the only client it has —
/// which is the failure this replaces, arriving quietly instead of at startup.
pub fn bind(port: u16) -> std::io::Result<Vec<TcpListener>> {
    let mut listeners = vec![TcpListener::bind(SocketAddr::from((
        Ipv4Addr::LOCALHOST,
        port,
    )))?];
    for bridge in bridge_addresses() {
        listeners.push(TcpListener::bind(SocketAddr::from((bridge, port)))?);
    }
    Ok(listeners)
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
            secret: crate::secret::Secret::kept_in(dir),
        })
    }

    /// The secret this warden minted, so a test can present it. Read from disk rather than kept in
    /// a variable, because reading it is what skein does.
    fn held_by(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("secret")).unwrap_or_default()
    }

    fn ask(warden: &Warden, method: &str, path: &str, body: &str) -> Response {
        asked_with(
            warden,
            method,
            path,
            body,
            &held_by(warden.secret.where_().parent().unwrap()),
        )
    }

    fn asked_with(warden: &Warden, method: &str, path: &str, body: &str, secret: &str) -> Response {
        warden.route(&Request {
            method: method.into(),
            path: path.into(),
            body: body.as_bytes().to_vec(),
            secret: secret.into(),
        })
    }

    /// Every endpoint, including the two that only report, refuses a caller it cannot recognise.
    ///
    /// The narrow bind is the boundary today; this is what survives it widening at 4c, when skein is
    /// inside the sandbox and reaches the warden by the same address a box would. The reporting
    /// endpoints are in the test on purpose: "it only tells you things" is how an endpoint ends up
    /// outside a check, and what `/v1/fleet` tells you is what this host is running.
    #[test]
    fn a_caller_it_cannot_recognise_gets_nothing_at_all() {
        let dir = scratch("unknown");
        let warden = warden(&dir);
        let known = held_by(warden.secret.where_().parent().unwrap());
        for (method, path, body) in [
            ("GET", "/v1/fleet", ""),
            ("POST", "/v1/audit", r#"{"what":"x","reported_by":"skein"}"#),
            ("POST", "/v1/create", r#"{"operation":"o","sandbox":"s"}"#),
            ("POST", "/v1/destroy", r#"{"operation":"o","sandbox":"s"}"#),
            // Not an endpoint at all: it must answer 401 rather than 404, or the refusal maps the
            // surface for whoever is guessing.
            ("GET", "/v1/anything", ""),
        ] {
            for offered in ["", "not-the-secret", &format!("{known}x")] {
                let said = asked_with(&warden, method, path, body, offered);
                assert_eq!(
                    said.code, 401,
                    "{method} {path} answered {} to a caller holding {offered:?}",
                    said.code
                );
            }
        }
        // And the doorway is not spent by any of that: a caller that was never going to be approved
        // must not be able to use up the one operation a person can be asked about (§8.5).
        let said = ask(
            &warden,
            "POST",
            "/v1/create",
            r#"{"operation":"o","sandbox":"s"}"#,
        );
        assert_ne!(
            said.code, 429,
            "the refused callers spent the doorway, so a flood wins by refusal what it could not \
             win by approval: {}",
            said.body
        );
    }

    /// A warden that cannot read its own copy refuses everything, and says which failure it is.
    #[test]
    fn a_warden_with_no_secret_refuses_everyone_rather_than_letting_everyone_through() {
        let dir = scratch("blind");
        std::fs::create_dir_all(&dir).unwrap();
        // A home that cannot hold a file: `kept_in` can neither read nor mint.
        let blind = dir.join("wall");
        std::fs::write(&blind, "not a directory").unwrap();
        let warden = Warden {
            store: Store::new(dir.join("outcomes"), Duration::from_secs(3600)),
            log: Log::new(dir.join("warden.jsonl")),
            approver: Box::new(Unattended),
            doorway: Doorway::new(),
            secret: crate::secret::Secret::kept_in(&blind.join("home")),
        };
        assert!(warden.secret.missing());
        let said = asked_with(&warden, "GET", "/v1/fleet", "", "");
        assert_eq!(said.code, 503, "{}", said.body);
        assert!(
            said.body.contains("no secret to check against"),
            "the operator is told the caller was wrong, when the truth is that this warden cannot \
             check: {}",
            said.body
        );
    }

    /// The four endpoints, over a real socket, and the capability list the running warden reports.
    ///
    /// **Asked of the warden, not read off the source**, which is what the item requires: the list
    /// comes back over HTTP from a process that has already been built.
    /// Two tables from a Linux host with two Docker networks and an office LAN.
    ///
    /// The default route is in it deliberately: `0.0.0.0/0` with mask `0.0.0.0` matches EVERY
    /// address, so if the bridge-name filter were ever dropped this fixture hands back the
    /// machine's LAN address and its loopback rather than quietly still passing.
    const HOST_WITH_TWO_BRIDGES: (&str, &str) = (
        "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0
eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
br-1a2b3c4d5e6f\t000012AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
",
        "\
Main:
  +-- 0.0.0.0/0 3 0 5
     |-- 127.0.0.0
        /8 host LOCAL
     |-- 127.0.0.1
        /32 host LOCAL
     |-- 172.17.0.0
        /16 link UNICAST
     |-- 172.17.0.1
        /32 host LOCAL
     |-- 172.18.0.0
        /16 link UNICAST
     |-- 172.18.0.1
        /32 host LOCAL
     |-- 192.168.1.0
        /24 link UNICAST
     |-- 192.168.1.55
        /32 host LOCAL
",
    );

    /// The bind widens onto the bridge a sandbox reaches the host on, and onto nothing else.
    ///
    /// **What would make this fail**, which is the point of having it: dropping the `docker0`/`br-`
    /// name test hands back `192.168.1.55` — the address on whatever network the laptop is attached
    /// to, which is the outcome the owner ruled out when he chose the bridge over `0.0.0.0`.
    /// Dropping the `/32 host LOCAL` test hands back `172.17.0.0`, an address the host does not own
    /// and cannot bind, so the warden would refuse to start on a machine that was working.
    #[test]
    fn the_bind_widens_onto_the_docker_bridge_and_not_onto_the_office_network() {
        let (route, fib_trie) = HOST_WITH_TWO_BRIDGES;
        assert_eq!(
            bridges_in(route, fib_trie),
            vec![Ipv4Addr::new(172, 17, 0, 1), Ipv4Addr::new(172, 18, 0, 1),],
        );
    }

    /// A host with no Docker bridge listens on loopback exactly as before.
    ///
    /// This is the macOS and Windows case, where the bridge lives inside Docker's own Linux VM and
    /// the host has no `/proc` at all — and it is why the same code is right on every host. It is
    /// also the assertion that fails if anybody ever reaches for the shortcut the derivation's note
    /// argues against: "the bridge is the `.1` of a private subnet" invents `192.168.1.1` here, an
    /// address that belongs to the office router.
    #[test]
    fn a_host_with_no_bridge_widens_onto_nothing() {
        let (_, fib_trie) = HOST_WITH_TWO_BRIDGES;
        let no_bridges = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0
eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
";
        assert_eq!(bridges_in(no_bridges, fib_trie), Vec::<Ipv4Addr>::new());
        // And an absent `/proc` — every read empty — is the same answer rather than a panic.
        assert_eq!(bridges_in("", ""), Vec::<Ipv4Addr>::new());
    }

    #[test]
    fn a_running_warden_answers_on_loopback_and_says_what_it_can_do() {
        let _env = crate::env_lock();
        let dir = scratch("live");
        let listeners = bind(0).expect("bind");
        // Loopback is first and is the one this test speaks to. What every listener must NOT be is
        // unspecified: `0.0.0.0` is the one address the owner ruled out, and it is also the
        // one-character change that would make the rest of this file pass while putting `sbx rm -f`
        // on whatever network the laptop is attached to.
        for listener in &listeners {
            let bound = listener.local_addr().unwrap();
            assert!(
                !bound.ip().is_unspecified(),
                "the warden bound every interface on the machine: {bound}"
            );
        }
        let addr = listeners[0].local_addr().unwrap();
        assert!(
            addr.ip().is_loopback(),
            "the warden's first listener is the one a host process asks: {addr}"
        );
        let listener = listeners;
        std::env::set_var(
            "SKEIN_WARDEN_LS_CMD",
            r#"printf '[{"name":"skein-fleet"}]'"#,
        );
        // Minted here so the caller below can present it — which is exactly what skein does: read
        // the file the warden owns, out of a directory no box's mount view reaches.
        let secret = held_by(
            crate::secret::Secret::kept_in(&dir)
                .where_()
                .parent()
                .unwrap(),
        );
        assert!(!secret.is_empty());
        std::thread::spawn(move || warden(&dir).serve(listener));

        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .write_all(
                format!(
                    "GET /v1/fleet HTTP/1.1\r\nHost: x\r\n{}: {secret}\r\n\r\n",
                    crate::secret::HEADER
                )
                .as_bytes(),
            )
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
            // The argv skein really sends — `sbx create [flags] AGENT PATH [PATH...]`. A fixture
            // carrying only the tail of one is what let the warden prepend a second verb and a
            // second name to every create unnoticed (SKEIN-456).
            &format!(
                r#"{{"operation":"op-1","sandbox":"skein-fleet","args":[{}]}}"#,
                r#""create","--name","skein-fleet","-m","26g","--cpus","7","shell","/h/.skein""#
            ),
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
            raw.contains("\"asked\"") && raw.contains("\"refused\""),
            "{raw}"
        );
        // **`refused`, and not `settled`.** `settled` is written with `describe`, which renders a
        // doer's `Err` as "ran and failed" — so the log said a create had run and failed on a host
        // where nobody could approve one and `sbx` was never invoked. The one log that exists to
        // settle an argument was the one asserting the thing that did not happen.
        assert!(
            !raw.contains("\"settled\"") && !raw.contains("ran and failed"),
            "an operation that never reached its command was recorded as one that ran: {raw}"
        );
    }

    /// Asking twice: answered when it **ran**, put to a person again when it did **not**.
    ///
    /// The two halves are one test because each is worthless alone. The permitted case first — an
    /// approved operation reaches `sbx` exactly once however many times it is asked for, which is
    /// §8.2 and the only reason the operation id exists. Then the refused case, which used to be
    /// answered from the record for thirty days: the same argv, sandbox and environment always
    /// derive the same id (`skein::warden_client::operation_id_with_env`), so one mistyped id at
    /// the terminal — or one warden started under a supervisor, where there is no terminal at all
    /// and every doer refuses — took that operation off the table until an argument changed.
    ///
    /// **Executions are counted, not states.** A test that read only the reply bodies would pass
    /// against a warden that ran the command twice and recorded it once, which is the failure that
    /// matters: a fleet created twice, or destroyed twice. So `sbx` is a script on `PATH` that
    /// appends a line, and the assertion is the number of lines.
    #[test]
    #[cfg(feature = "create")]
    fn a_retry_is_replayed_when_it_ran_and_re_asked_when_nobody_approved_it() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // $PATH decides what every spawn in this process resolves to, and sibling tests hold this
        // lock for the same reason (SKEIN-307).
        let _env = crate::env_lock();
        let dir = scratch("replay");
        std::fs::create_dir_all(&dir).unwrap();
        let ran = dir.join("sbx-ran");
        let fake = dir.join("sbx");
        std::fs::write(
            &fake,
            // The path is quoted: `scratch` puts a `ThreadId(n)` in it, and an unquoted `(` is a
            // syntax error the shell reports as exit 2 — which arrives here as a doer that ran and
            // failed, and would have been read as one that was refused.
            format!("#!/bin/sh\necho \"$@\" >> '{}'\necho made\n", ran.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let real = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{real}", dir.display()));

        struct Counting {
            calls: AtomicUsize,
            answer: Result<(), String>,
        }
        impl Approver for Counting {
            fn approve(&self, _: &doer::Request, _: &str) -> Result<(), String> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.answer.clone()
            }
        }
        struct Shared(Arc<Counting>);
        impl Approver for Shared {
            fn approve(&self, r: &doer::Request, what: &str) -> Result<(), String> {
                self.0.approve(r, what)
            }
        }
        let warden_saying = |name: &str, counted: &Arc<Counting>| Warden {
            store: Store::new(dir.join(name).join("outcomes"), Duration::from_secs(3600)),
            log: Log::new(dir.join(name).join("warden.jsonl")),
            approver: Box::new(Shared(Arc::clone(counted))),
            doorway: Doorway::new(),
            secret: crate::secret::Secret::kept_in(&dir.join(name)),
        };
        let body = |op: &str| {
            format!(
                r#"{{"operation":"{op}","sandbox":"skein-fleet","args":[{}]}}"#,
                r#""create","--name","skein-fleet","-m","26g","--cpus","7","shell","/h/.skein""#
            )
        };

        // **Approved.** It runs, and the retry is answered rather than obeyed.
        let yes = Arc::new(Counting {
            calls: AtomicUsize::new(0),
            answer: Ok(()),
        });
        let w = warden_saying("yes", &yes);
        let first = ask(&w, "POST", "/v1/create", &body("op-ran"));
        let again = ask(&w, "POST", "/v1/create", &body("op-ran"));
        assert!(
            first.body.contains(r#""state":"ran""#) && first.code == 200,
            "an approved create must run: {}",
            first.body
        );
        assert!(
            again.body.contains(r#""state":"replayed""#),
            "a retry of something that ran must be answered from the record, and say so: {}",
            again.body
        );
        assert_eq!(yes.calls.load(Ordering::SeqCst), 1, "asked twice");
        assert_eq!(
            std::fs::read_to_string(&ran)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "the privileged command ran a second time for a retry — which is a fleet created twice"
        );

        // **Refused.** Nothing ran, so there is nothing to replay, and the person is asked again.
        let no = Arc::new(Counting {
            calls: AtomicUsize::new(0),
            answer: Err("the person said no".into()),
        });
        let w = warden_saying("no", &no);
        let once = ask(&w, "POST", "/v1/create", &body("op-refused"));
        let twice = ask(&w, "POST", "/v1/create", &body("op-refused"));
        std::env::set_var("PATH", real);

        for said in [&once, &twice] {
            assert_eq!(said.code, 409, "{}", said.body);
            assert!(
                said.body.contains(r#""state":"refused""#),
                "a refusal is neither a run nor a replay, and the state has to say which: {}",
                said.body
            );
        }
        assert!(
            !twice.body.contains("replayed"),
            "a refusal was answered from the record: {}",
            twice.body
        );
        assert_eq!(
            no.calls.load(Ordering::SeqCst),
            2,
            "asking again was answered with the old refusal instead of being put to a person — \
             which is the same id for thirty days after one mistyped confirmation"
        );
        assert_eq!(
            std::fs::read_to_string(&ran)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "a refused create reached `sbx`"
        );
    }

    /// An approver that records what it was shown and always says no.
    ///
    /// Refusing rather than approving on purpose: these tests are about what reaches the prompt, and
    /// a doer that went on to run `sbx` would be testing the host's `PATH` as well.
    #[cfg(all(feature = "create", feature = "destroy"))]
    struct Watching(std::sync::Mutex<Vec<String>>);
    #[cfg(all(feature = "create", feature = "destroy"))]
    impl Approver for Watching {
        fn approve(&self, _: &doer::Request, what: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(what.to_string());
            Err("not today".into())
        }
    }

    #[cfg(all(feature = "create", feature = "destroy"))]
    fn watched(dir: &std::path::Path, seen: &Arc<Watching>) -> Warden {
        struct Shared(Arc<Watching>);
        impl Approver for Shared {
            fn approve(&self, r: &doer::Request, what: &str) -> Result<(), String> {
                self.0.approve(r, what)
            }
        }
        Warden {
            store: Store::new(dir.join("outcomes"), Duration::from_secs(3600)),
            log: Log::new(dir.join("warden.jsonl")),
            approver: Box::new(Shared(Arc::clone(seen))),
            doorway: Doorway::new(),
            secret: crate::secret::Secret::kept_in(dir),
        }
    }

    /// The environment is an allow-list per verb, and what survives it is on the screen.
    ///
    /// **The environment decides what runs, and the argv does not say so.** `doer::run` spells the
    /// program as the literal `"sbx"` — a relative name — and Rust resolves a relative program
    /// through the `PATH` set on the `Command`, so a request could choose which binary the host uid
    /// executed while the approval still read `sbx rm -f skein-fleet`. `destroy` and `unpublish`
    /// did not render the environment at all, which made it invisible as well as unfiltered.
    ///
    /// Both halves are asserted, in this order: the key skein really sends is accepted **and shown**
    /// (`skein::fleet::create_env` returns `DOCKER_SANDBOXES_ROOT_SIZE` or nothing), and the ones it
    /// never sends are refused before a person is troubled at all.
    #[test]
    #[cfg(all(feature = "create", feature = "destroy"))]
    fn only_the_environment_a_verb_has_a_use_for_reaches_it_and_a_person_sees_that_one() {
        let dir = scratch("env");
        let seen = Arc::new(Watching(std::sync::Mutex::new(Vec::new())));
        let w = watched(&dir, &seen);
        let create = |env: &str| {
            format!(
                r#"{{"operation":"op-e","sandbox":"skein-fleet","args":[{}],"env":{env}}}"#,
                r#""create","--name","skein-fleet","-m","26g","shell","/h/.skein""#
            )
        };

        // Permitted, first — a refusal test against something that was never allowed proves nothing.
        let allowed = ask(
            &w,
            "POST",
            "/v1/create",
            &create(r#"[["DOCKER_SANDBOXES_ROOT_SIZE","200g"]]"#),
        );
        assert_eq!(allowed.code, 409, "{}", allowed.body);
        let shown = seen.0.lock().unwrap().clone();
        assert_eq!(
            shown.len(),
            1,
            "the create never reached a person: {shown:?}"
        );
        assert!(
            shown[0].contains("DOCKER_SANDBOXES_ROOT_SIZE=200g sbx create"),
            "the size a fleet is created at is most of what that command does, and it has to be in \
             front of the person approving it: {}",
            shown[0]
        );

        // And the ones that decide what `sbx` even is — then `GH_TOKEN` and `HOME`, which decide
        // nothing of the sort and are the assertions that tell an allow-list from a deny-list. A
        // list of the four dangerous names would refuse every key above and pass these two, and it
        // is these two that a caller sends without looking like an attack: skein holds a GitHub
        // token and reads `$HOME` on every path it has, so either arriving here reads as plumbing.
        // The rule is that a key the warden has no use for does not reach a privileged command,
        // whether or not anyone has thought of what it could do.
        for key in [
            "PATH",
            "LD_PRELOAD",
            "DOCKER_HOST",
            "DOCKER_CONFIG",
            "GH_TOKEN",
            "HOME",
        ] {
            let refused = ask(
                &w,
                "POST",
                "/v1/create",
                &create(&format!(r#"[["{key}","/tmp/mine"]]"#)),
            );
            assert_eq!(refused.code, 400, "{key} was accepted: {}", refused.body);
            assert!(
                refused.body.contains(key),
                "the refusal has to name the key, or an operator cannot fix it: {}",
                refused.body
            );
        }

        // `destroy` takes none at all: `sbx rm -f` reads nothing from the environment, and skein
        // sends it none (`Warden::destroy` passes `&[]`). A shared list would have handed destroy a
        // key it has no use for, which is a smaller version of the same accident.
        let destroy = ask(
            &w,
            "POST",
            "/v1/destroy",
            r#"{"operation":"op-d","sandbox":"skein-fleet",
                "env":[["DOCKER_SANDBOXES_ROOT_SIZE","200g"]]}"#,
        );
        assert_eq!(destroy.code, 400, "{}", destroy.body);
        assert!(
            destroy.body.contains("carries none at all"),
            "{}",
            destroy.body
        );

        // A key given twice: allowed both times, rendered both times, and `Command::envs` takes the
        // last. The same failure `argv_create` refuses two `--name`s for — a person reading the
        // line from the left approves a value that will not be the one in effect.
        let twice = ask(
            &w,
            "POST",
            "/v1/create",
            &create(
                r#"[["DOCKER_SANDBOXES_ROOT_SIZE","20g"],["DOCKER_SANDBOXES_ROOT_SIZE","900g"]]"#,
            ),
        );
        assert_eq!(twice.code, 400, "{}", twice.body);
        assert!(twice.body.contains("given twice"), "{}", twice.body);

        // Nobody was asked about any of the refused ones.
        assert_eq!(
            seen.0.lock().unwrap().len(),
            1,
            "a request the warden was going to refuse still spent a person's attention"
        );
    }

    /// Nothing reaches the approval text that the approval text cannot show.
    ///
    /// §8.4 says the warden renders the resolved arguments it will itself execute, and that was true
    /// while being insufficient: the render is still made of bytes the requester chose, and
    /// `approval::prompt` writes them to a terminal. A `\n` repaints the lines above it, `\x1b[`
    /// drives the cursor anywhere on the screen, and a bidirectional override reorders a line
    /// without changing a byte — so the id a person types could confirm a line they never saw.
    /// Only the operation id was guarded (`outcome::checked_id`), and it is not the only field on
    /// the screen.
    #[test]
    #[cfg(all(feature = "create", feature = "destroy"))]
    fn a_request_cannot_write_on_the_screen_it_is_being_approved_from() {
        let dir = scratch("bytes");
        let seen = Arc::new(Watching(std::sync::Mutex::new(Vec::new())));
        let w = watched(&dir, &seen);

        // Permitted first: the real argv, with the paths and colons and equals signs a create
        // carries, is not what this refuses.
        let ordinary = ask(
            &w,
            "POST",
            "/v1/create",
            &format!(
                r#"{{"operation":"op-ok","sandbox":"skein-fleet","args":[{}]}}"#,
                r#""create","--name","skein-fleet","-p","8317:8317","--kit","/h/.skein/kit","shell","/h/.skein""#
            ),
        );
        assert_eq!(
            ordinary.code, 409,
            "an ordinary create was refused: {}",
            ordinary.body
        );
        assert_eq!(seen.0.lock().unwrap().len(), 1);

        // The prompt repainted from inside an argument, three ways — as three requests rather than
        // one argv, because a single request refused for any one of its reasons satisfies an
        // assertion made over all three at once.
        //
        // **The second and third are what this was missing.** It was one argument carrying both a
        // newline and an escape, so a guard that refused `\n` and `\r` alone — the deny-list
        // `readable` is a whitelist instead of — satisfied it. A bare `\x1b[2K` erases the line it
        // is drawn on with no newline anywhere, and `\u{202e}` reorders one with no control
        // character anywhere; the mount path in front of each is what an argument really looks
        // like, because a refusal is only worth as much as the thing it is refusing is plausible.
        for (what, arg) in [
            (
                "a newline, which draws a second `will run` line under the real one",
                "x\n  will run    sbx ls",
            ),
            (
                "an erase-line escape, which unwrites the real line where it stands",
                "/h/.skein\u{1b}[2K  will run    sbx ls",
            ),
            (
                "a right-to-left override, which reorders a line without changing a byte",
                "/h/\u{202e}gpj.esruoc",
            ),
        ] {
            let body = serde_json::json!({
                "operation": "op-paint",
                "sandbox": "skein-fleet",
                "args": ["create", "--name", "skein-fleet", arg],
            })
            .to_string();
            let repaint = ask(&w, "POST", "/v1/create", &body);
            assert_eq!(
                repaint.code, 400,
                "an argument carrying {what} was accepted: {}",
                repaint.body
            );
        }

        // The same from the sandbox name, which reaches `argv_destroy` as well as the screen — and
        // a leading `-` there argv-parses as a flag straight after `rm -f`.
        for name in [
            "skein\nfleet",
            "skein-fleet\u{1b}[2K",
            "-rf",
            "../elsewhere",
            "skein\u{202e}teelf",
            "",
        ] {
            let body = serde_json::json!({"operation":"op-n","sandbox":name}).to_string();
            let refused = ask(&w, "POST", "/v1/destroy", &body);
            assert_eq!(refused.code, 400, "{name:?} was accepted: {}", refused.body);
            assert!(
                refused.body.contains("is not a sandbox name"),
                "{name:?}: {}",
                refused.body
            );
        }

        // And from an environment value, which `described_env` renders in front of the command —
        // the same two shapes, because the value of the one key a create carries is written onto
        // the same line of the same screen as the argv is.
        for (what, value) in [
            ("a newline", "200g\n  will run    sbx ls"),
            ("an erase-line escape", "200g\u{1b}[2K  will run    sbx ls"),
        ] {
            let body = serde_json::json!({
                "operation": "op-v",
                "sandbox": "skein-fleet",
                "args": ["create", "--name", "skein-fleet", "shell", "/h/.skein"],
                "env": [["DOCKER_SANDBOXES_ROOT_SIZE", value]],
            })
            .to_string();
            let sneaky = ask(&w, "POST", "/v1/create", &body);
            assert_eq!(
                sneaky.code, 400,
                "an environment value carrying {what} was accepted: {}",
                sneaky.body
            );
        }

        // Nobody was shown any of them, and nothing was written into the log under their ids —
        // the guard runs before the audit entry, which names the operation id.
        assert_eq!(
            seen.0.lock().unwrap().len(),
            1,
            "a request that could not be rendered was still put to a person"
        );
        let log = std::fs::read_to_string(w.log.path()).unwrap_or_default();
        for id in ["op-paint", "op-n", "op-v"] {
            assert!(
                !log.contains(id),
                "{id} was recorded before it was vetted: {log}"
            );
        }
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
            secret: crate::secret::Secret::kept_in(&dir),
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
            secret: crate::secret::Secret::kept_in(&dir),
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

    /// A reporter cannot file an entry as the warden — over the endpoint, which is where it could.
    ///
    /// `reported_by` is the only thing separating the warden's own account from a claim by whoever
    /// called it, and it was taken verbatim from the request. The audited thing could therefore
    /// write a line in the log that exists **because skein cannot audit itself** (§5), indexed the
    /// same way as the warden's own.
    ///
    /// The test this replaces was named for this property and could not see it: it handed
    /// `reported_by: "warden"` straight to `Log::append` — the writer, not the endpoint — and then
    /// asserted the *timestamp*. Nothing about it would have changed if `serve::audit` had copied
    /// the field, which it did.
    #[test]
    fn a_reporter_cannot_file_an_entry_as_the_warden() {
        let dir = scratch("forge");
        let w = warden(&dir);
        let filed = ask(
            &w,
            "POST",
            "/v1/audit",
            r#"{"operation":"op-4","what":"approved","detail":"by a human, honest",
                "reported_by":"warden"}"#,
        );
        assert_eq!(filed.code, 200, "{}", filed.body);
        // The warden's own account of the same operation, for the entry to be told apart from.
        w.log.record("op-4", "refused", "the warden's own").unwrap();

        let raw = std::fs::read_to_string(w.log.path()).unwrap();
        let entries: Vec<crate::audit::Entry> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries.len(), 2, "{raw}");
        assert_ne!(
            entries[0].reported_by,
            crate::audit::THE_WARDEN,
            "a reporter filed an entry indistinguishable from the warden's own: {raw}"
        );
        assert_eq!(entries[0].reported_by, "claimed:warden");
        // Kept, not dropped: an attempt to claim the name is worth more in the record than out of
        // it, and refusing would let a reporter choose between honesty and silence.
        assert_eq!(entries[0].what, "approved");
        assert_eq!(entries[1].reported_by, crate::audit::THE_WARDEN);

        // And a case variant, which an exact comparison lets straight through. `Warden` in a
        // column of `warden`s is the same forgery for a person reading the log, which is the only
        // reader it has. The stored value keeps the reporter's own spelling behind the prefix:
        // marking a claim must not edit it into the constant.
        let filed = ask(
            &w,
            "POST",
            "/v1/audit",
            r#"{"operation":"op-4","what":"approved","reported_by":" Warden "}"#,
        );
        assert_eq!(filed.code, 200, "{}", filed.body);
        let raw = std::fs::read_to_string(w.log.path()).unwrap();
        let third: crate::audit::Entry = serde_json::from_str(raw.lines().nth(2).unwrap()).unwrap();
        assert_eq!(third.reported_by, "claimed:Warden", "{raw}");
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
