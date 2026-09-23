//! Whether GitHub can be reached direct from here, and what the sandbox proxy does with a
//! credential sent through it.

use super::*;

/// Whether a DIRECT GitHub connection lands, told apart from what GitHub answers once it does.
///
/// The distinction is the whole point (SKEIN-548, SKEIN-926): a 401/403 is GitHub answering, and a
/// blocked egress policy is GitHub never being reached. Only the second is skein's to explain with a
/// policy command; the first is an ordinary auth answer nobody should be told to change a firewall
/// over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GithubReach {
    /// The connection landed and GitHub returned some HTTP status — reachable, whatever the status.
    Reachable,
    /// The connection could not be made at all — refused, timed out, or DNS failed.
    Blocked,
}

/// Turn a direct-reachability outcome into the health line. Pure, so the blocked-vs-answer wording
/// — the user-visible half — is proven without a network.
pub(crate) fn github_reach_line(reach: GithubReach, fleet: &str) -> HealthCheck {
    match reach {
        // Not a fault: GitHub is reachable, so the scoped path presents this box's/host's own token
        // and GitHub is the one enforcing it.
        GithubReach::Reachable => HealthCheck::satisfied(
            "available — GitHub is reachable directly, so the token skein holds is the one GitHub \
             sees",
        ),
        // The approved wording (SKEIN-548), split across the diagnosis and its recipe. `{fleet}` is
        // the real sandbox name, derived from config on the host where this runs.
        GithubReach::Blocked => HealthCheck::unsatisfied(
            "GitHub is blocked by the sandbox's network policy",
            format!(
                "on your host run:  sbx policy allow network --sandbox {fleet} \
                 github.com,api.github.com"
            ),
        ),
    }
}

/// Probe `target` DIRECT (never through the proxy) and classify the outcome. A real HTTP status —
/// including 401/403 — is [`GithubReach::Reachable`]; only a failure to connect is
/// [`GithubReach::Blocked`]. `curl` without `-f` exits 0 for any response and writes `000` with a
/// non-zero exit when it could not connect, so the http_code alone decides it.
pub(crate) fn probe_github_reach_at(target: &str) -> GithubReach {
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "--noproxy",
            "*",
            "-I",
            "-o",
            "/dev/null",
            "-m",
            "5",
            "--connect-timeout",
            "3",
            "-w",
            "%{http_code}",
            target,
        ])
        .output();
    match out {
        Ok(out) => {
            let code = String::from_utf8_lossy(&out.stdout);
            let code = code.trim();
            if code.len() == 3 && code != "000" && code.bytes().all(|b| b.is_ascii_digit()) {
                GithubReach::Reachable
            } else {
                GithubReach::Blocked
            }
        }
        // curl failed to even spawn. Presence is the caller's concern; here that reads as no
        // connection.
        Err(_) => GithubReach::Blocked,
    }
}

/// The `gh` health line: curl is present (the caller has checked), so this answers reachability.
///
/// **It must not spend the box's shared api.github.com budget from a test** (SKEIN-693), and it must
/// not probe the network on a polled endpoint gratuitously — so a test that wants to exercise this
/// pins `$SKEIN_GITHUB_REACH_URL` at its own listener, and without that pin an in-test call reports
/// reachable rather than reaching out. In production it probes `github.com` — the web host, not the
/// rate-limited REST API — because all that matters is whether a connection to GitHub can be made.
pub(super) fn github_reach_health(fleet: &str) -> HealthCheck {
    let pinned = std::env::var("SKEIN_GITHUB_REACH_URL")
        .ok()
        .filter(|v| !v.is_empty());
    match pinned {
        Some(target) => github_reach_line(probe_github_reach_at(&target), fleet),
        None if crate::util::in_test() => HealthCheck::satisfied("available"),
        None => github_reach_line(probe_github_reach_at("https://github.com/"), fleet),
    }
}

// ---------- what the sandbox proxy does with a credential (SKEIN-548) ----------

/// **The credential the probe sends, and the only one it ever sends.**
///
/// `skein-test-` by convention across this tree — `tests/github_reach_live.rs:47` sends the same
/// shape for the same reason — so that a copy of it in a log, a terminal or a health report is not
/// a disclosure. The probe reads a **status code and one rate-limit header** back, and discards
/// the rest of the response unread: no body, and no credential of anybody's in either direction.
/// That is not politeness, it is the property that makes this check safe to run unattended, on a
/// polled endpoint, on somebody else's fleet.
pub(crate) const PROBE_CREDENTIAL: &str = "skein-test-not-a-credential";

/// Where the probe asks, and it is `/rate_limit` for two reasons rather than one.
///
/// It discriminates as sharply as `/user` — an invalid credential is `401` on both — and **it does
/// not spend the rate limit**, so a check that runs every hour for the life of a fleet costs
/// nothing from the 60-an-hour anonymous pool that every box behind one egress IP shares. Both
/// halves of that were measured here on 2026-09-21 rather than read from documentation: the pool
/// was exhausted at the time by ordinary traffic (`x-ratelimit-used: 60`), `/user` answered `403`
/// rate-limit-exceeded in that state, and `/rate_limit` answered `200` in the same second. It also
/// carries the ceiling the arm below needs, which `/user` does not.
const PROBE_TARGET: &str = "https://api.github.com/rate_limit";

/// GitHub's hourly ceiling for a request nobody authenticated — measured through the proxy and
/// direct on 2026-09-21, `x-ratelimit-limit: 60` both ways. An authenticated one is orders above
/// it (SKEIN-927 recorded 5000 on the day injection was live), so the two never collide and the
/// exact authenticated figure does not need to be written down here.
const ANONYMOUS_CEILING: u32 = 60;

/// **What the sandbox proxy did with a credential it was handed** (SKEIN-548).
///
/// The question `NO_PROXY` cannot answer. `src/box-session.sh:2098` routes a scoped box's `git`
/// and `gh` around the proxy, and the launcher says in its own comment that this narrows the
/// normal path rather than containing anything — "a variable anything can set again is not a
/// containment". So what matters is what the proxy does to a request that *is* on it, and the only
/// way to find that out is to send something that cannot possibly be valid and see whether it
/// works anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProxyCredential {
    /// A credential that cannot be valid came back **authenticated** — accepted, and with an
    /// hourly ceiling above [`ANONYMOUS_CEILING`]. Something other than the credential that was
    /// sent answered for it, which is the injection SKEIN-548 measured on 2026-09-06.
    Injected { ceiling: u32 },
    /// **Nothing account-wide was added**, which is the claim this check actually makes and covers
    /// both ways of not adding one: the invalid credential arrived and GitHub refused it, or the
    /// request was answered anonymously.
    ///
    /// The second is not a hypothetical corner. sbx v0.43.0's note — quoted on SKEIN-548 — is that
    /// the proxy "no longer forwards a client-supplied credential the proxy did not issue" to a
    /// managed provider host, and a strip with nothing put back is exactly an anonymous `200`. A
    /// probe that read the status alone would call that injection and paint the banner red over a
    /// proxy that had added nothing at all.
    Untouched,
    /// No proxy is configured in this environment, so there is nothing in the path to inject.
    Absent,
    /// Neither an acceptance nor a refusal came back. `why` says what did.
    Unanswered(String),
}

/// Turn the reading into the line. **Pure**, so every sentence the owner sees is proven without a
/// network and without a credential of any kind.
///
/// Only [`ProxyCredential::Injected`] is a fault, and only an *authenticated* answer produces one
/// — so an `Unanswered` can never hide an injection and can never manufacture one. That ordering is
/// the same one [`token_expiry_line`] keeps, for the same reason: this is a check whose false
/// positive would put a red banner across a working fleet.
pub(crate) fn proxy_injection_line(seen: ProxyCredential, fleet: &str) -> HealthCheck {
    match seen {
        ProxyCredential::Injected { ceiling } => HealthCheck::unsatisfied(
            format!(
                "the sandbox proxy answers GitHub as the account: a deliberately invalid \
                 credential came back authenticated through it, on an hourly ceiling of {ceiling} \
                 where an unauthenticated request gets {ANONYMOUS_CEILING}. So anything in a box \
                 that routes through $HTTPS_PROXY reaches every repository the account can, \
                 whatever token that box holds"
            ),
            format!(
                "on your HOST, set what this sandbox injects — `sbx secret set github --sandbox \
                 {fleet}` — to a token bounded to the repositories the fleet should reach, or to a \
                 dummy value, which turns injection off and leaves boxes on skein's own per-repo \
                 tokens. Set it again after any fleet rebuild: `sbx rm` deletes a sandbox-scoped \
                 secret along with the sandbox"
            ),
        ),
        ProxyCredential::Untouched => HealthCheck::satisfied(
            "the sandbox proxy adds no credential of its own to GitHub — a deliberately invalid \
             one sent through it was not answered as anybody, so a box reaches GitHub as whatever \
             token it actually holds",
        ),
        ProxyCredential::Absent => HealthCheck::satisfied(
            "no proxy is configured here, so there is nothing in the path to put a credential on a \
             request that carries none",
        ),
        ProxyCredential::Unanswered(why) => HealthCheck::unknown(format!(
            "skein could not tell whether the sandbox proxy injects a credential — {why}"
        )),
    }
}

/// Send [`PROBE_CREDENTIAL`] to `target` **through `proxy`** and classify what comes back.
///
/// `--noproxy ''` empties curl's bypass list rather than inheriting it, and that is load-bearing:
/// in a scoped box `$NO_PROXY` names exactly the GitHub hosts (`src/box-session.sh:2098`), so a
/// probe that honoured it would go direct and answer a question nobody asked — "does GitHub refuse
/// a garbage token", to which the answer is always yes.
///
/// `-D -` puts the response **headers** on stdout, because the status alone is not enough to tell
/// an injected credential from a stripped one — see [`ProxyCredential::Untouched`]. The body still
/// goes to `/dev/null`: nothing this reads is anybody's secret, and nothing it does not read can
/// become one.
pub(crate) fn probe_proxy_injection_at(proxy: &str, target: &str) -> ProxyCredential {
    let header = format!("Authorization: Bearer {PROBE_CREDENTIAL}");
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "-x",
            proxy,
            "--noproxy",
            "",
            "-D",
            "-",
            "-o",
            "/dev/null",
            "-m",
            "8",
            "--connect-timeout",
            "4",
            "-H",
            header.as_str(),
            "-w",
            "\nskein-http-code %{http_code}",
            target,
        ])
        .output();
    let out = match out {
        Ok(out) => out,
        // curl is not installed, or could not be started. Presence is the `gh` line's concern; here
        // it reads as a question that was not asked rather than as an answer.
        Err(why) => return ProxyCredential::Unanswered(format!("curl did not run: {why}")),
    };
    let answer = String::from_utf8_lossy(&out.stdout);
    let code = answer
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("skein-http-code "))
        .unwrap_or("")
        .trim()
        .to_string();
    // The one header this reads, and the reason it is read at all: it is the difference between a
    // request somebody was authenticated for and one nobody was.
    let ceiling = answer
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("x-ratelimit-limit")
                .then(|| value.trim().parse::<u32>().ok())?
        })
        .next_back();
    match (code.as_str(), ceiling) {
        // Accepted, and accepted as somebody: a ceiling above the anonymous one is GitHub saying it
        // authenticated this request, and it cannot have authenticated the string that was sent.
        ("200", Some(ceiling)) if ceiling > ANONYMOUS_CEILING => {
            ProxyCredential::Injected { ceiling }
        }
        // Accepted anonymously. The credential was dropped on the way rather than replaced, so
        // nothing account-wide was added, which is what this check is about.
        ("200", Some(_)) => ProxyCredential::Untouched,
        // Accepted with no ceiling to read at all. Not an injection this can stand behind — and
        // this check does not guess, because a red banner nobody can confirm is one people learn
        // to scroll past (SKEIN-913).
        ("200", None) => ProxyCredential::Unanswered(
            "the probe was accepted but carried no x-ratelimit-limit, so whether anybody was \
             authenticated for it cannot be told from here"
                .to_string(),
        ),
        // GitHub refusing the probe's own credential is the whole point: it arrived as sent.
        ("401", _) => ProxyCredential::Untouched,
        // curl's own code for "no connection was made", and its empty output when it died first.
        ("000" | "", _) => {
            ProxyCredential::Unanswered("the probe could not reach the proxy at all".to_string())
        }
        // A rate limit is the common one, and it is genuinely not an answer to this question: an
        // unauthenticated `403` and an injected-but-throttled `403` look identical from here.
        (other, _) => ProxyCredential::Unanswered(format!(
            "the probe was answered {other}, which is neither an acceptance nor a refusal"
        )),
    }
}

/// Ask the proxy the question, and say so on the board when the answer is yes.
///
/// **Behind a gate for [`token_expiry_health`]'s reason**: `/api/health` is polled every fifteen
/// seconds by every open board, and this costs an HTTP request. **An hour**, and the interval is
/// argued from measurement rather than from taste — this is a property of the substrate and not of
/// anything skein installs, and it changed under this fleet inside six days (the dates are in
/// `docs/threat-model.md`) with nothing in the tree to say so. A day would have been wrong.
///
/// **It does not reach out from a test.** The rule [`github_reach_health`] and
/// [`token_expiry_health`] both keep: a unit test that depends on a network fails for somebody
/// else's reason, and this one would spend the box's shared api.github.com budget as well. A test
/// that wants the live path pins `$SKEIN_PROXY_PROBE_URL` at its own listener, exactly as
/// `$SKEIN_GITHUB_REACH_URL` does for the neighbour above.
pub(super) fn proxy_injection_health(fleet: &str) -> HealthCheck {
    static GATE: crate::util::Gate<HealthCheck> = crate::util::Gate::new();
    let proxy = ["HTTPS_PROXY", "https_proxy"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    // No proxy is a complete answer, not a gap — and it is the answer on every deployment that is
    // not inside an sbx sandbox, which is most of them.
    let Some(proxy) = proxy else {
        return proxy_injection_line(ProxyCredential::Absent, fleet);
    };
    let pinned = std::env::var("SKEIN_PROXY_PROBE_URL")
        .ok()
        .filter(|value| !value.is_empty());
    match pinned {
        Some(target) => proxy_injection_line(probe_proxy_injection_at(&proxy, &target), fleet),
        None if crate::util::in_test() => HealthCheck::unknown("not asked from a test"),
        None => {
            let fleet = fleet.to_string();
            GATE.get(std::time::Duration::from_secs(60 * 60), move || {
                Some(proxy_injection_line(
                    probe_proxy_injection_at(&proxy, PROBE_TARGET),
                    &fleet,
                ))
            })
            .unwrap_or_else(|| HealthCheck::unknown("skein has not been able to ask the proxy yet"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::testkit::*;

    /// **The blocked-egress message, and the rule that a 401/403 is not a block (SKEIN-548).** The
    /// user-visible half: only a genuine failure to reach GitHub prints the `sbx policy allow
    /// network` hint, and it carries the real fleet name. An auth answer clears the line.
    ///
    /// Counterfactual: if [`github_reach_line`] emitted the hint for `Reachable`, or dropped the
    /// fleet name, or left the fix empty for a block, an assertion here fails. Proven by sabotage —
    /// swapping the two arms made `the fleet name` / `is not a fault` fire.
    #[test]
    fn the_policy_hint_is_only_for_a_real_block() {
        let blocked = github_reach_line(GithubReach::Blocked, "skein-fleet-xyz");
        assert!(blocked.is_fault(), "a blocked GitHub must read as a fault");
        assert!(
            blocked
                .detail
                .contains("blocked by the sandbox's network policy"),
            "the diagnosis lost its wording: {}",
            blocked.detail
        );
        assert!(
            blocked.fix.contains(
                "sbx policy allow network --sandbox skein-fleet-xyz github.com,api.github.com"
            ),
            "the fix must be the exact copyable command with the real fleet name: {}",
            blocked.fix
        );

        let reachable = github_reach_line(GithubReach::Reachable, "skein-fleet-xyz");
        assert!(
            !reachable.is_fault(),
            "a reachable GitHub is not a fault, so nothing here should mention a firewall: {} / {}",
            reachable.detail,
            reachable.fix
        );
        assert!(
            !reachable.detail.contains("blocked") && !reachable.fix.contains("sbx policy"),
            "a 401/403 answer must NOT print the policy hint: {} / {}",
            reachable.detail,
            reachable.fix
        );
    }

    /// **The probe tells a connect failure apart from an HTTP answer, by sabotage of the surface it
    /// runs against.** A listener that answers 401 is [`GithubReach::Reachable`]; a dead port is
    /// [`GithubReach::Blocked`]. The counterfactual is real: if `probe_github_reach_at` treated any
    /// non-200 as blocked, the 401 case would flip; if it treated a refused connection as reachable,
    /// the dead-port case would. Both were watched to fail before this was trusted.
    #[test]
    fn a_401_is_reachable_and_a_dead_port_is_blocked() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        // Skip rather than fail where the harness has no curl — the same rule the rest of the file
        // holds; the probe is a wrapper around it. Through `testutil::skip` and not a bare `return`
        // so a run that asked for no skips refuses instead of passing in silence (SKEIN-790).
        if !crate::github::have_curl() {
            return crate::testutil::skip(
                "no curl, and the reachability probe under test is a wrapper around it",
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let _ = ready_tx.send(());
            if let Ok((mut sock, _)) = listener.accept() {
                let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
                let _ = sock.flush();
            }
        });
        // Confirmed accepting before the probe starts, for the same reason `fake_proxy` in this
        // module is (SKEIN-1024): `probe_github_reach_at` carries its own fixed wall-clock budget
        // (`-m 5`), and a brand-new thread racing that budget for its first scheduler slot is a
        // guess about the box's load, not a fact this test controls.
        wait_until_accepting(ready_rx);
        let reachable = probe_github_reach_at(&format!("http://127.0.0.1:{port}/"));
        // SKEIN-1027: this used to `.join()` a plain blocking `accept()` with no deadline of its
        // own at all, so a probe that never reached this listener for any reason would hang the
        // test forever. `finish_responder` unblocks it the same way `fake_proxy` now does.
        finish_responder(port, handle);
        assert_eq!(
            reachable,
            GithubReach::Reachable,
            "an HTTP 401 is GitHub answering — reachable, not blocked"
        );

        // Port 1 on loopback refuses immediately: a connection that cannot be made at all.
        let blocked = probe_github_reach_at("http://127.0.0.1:1/");
        assert_eq!(
            blocked,
            GithubReach::Blocked,
            "a refused connection is a block, not an answer"
        );
    }

    /// **A proxy that accepts what cannot be valid is the fault; everything else is not
    /// (SKEIN-548).** The user-visible half of the detection the owner asked for: a red line only
    /// when a credential was substituted, and the recipe that clears it names the real sandbox,
    /// because `sbx secret set` without `--sandbox <name>` writes the GLOBAL secret and would widen
    /// the very thing it was run to narrow.
    ///
    /// **The concrete change that makes this fail, named before it was written:** swapping the
    /// `Injected` and `Untouched` arms of [`proxy_injection_line`]. Planted, and
    /// `an accepted garbage credential is the fault` failed. Dropping `{fleet}` from the recipe
    /// fails `the recipe must name the sandbox`; making `Absent` unsatisfied fails
    /// `no proxy is not a fault`.
    #[test]
    fn only_an_accepted_garbage_credential_reads_as_injection() {
        let injected =
            proxy_injection_line(ProxyCredential::Injected { ceiling: 5000 }, "thing-fleet");
        assert!(
            injected.is_fault(),
            "an accepted garbage credential is the fault this check exists for: {}",
            injected.detail
        );
        assert!(
            injected.detail.contains("5000") && injected.detail.contains("60"),
            "the diagnosis shows both ceilings, because the gap between them IS the evidence: {}",
            injected.detail
        );
        assert!(
            injected.detail.contains("answers GitHub as the account"),
            "the diagnosis lost its wording: {}",
            injected.detail
        );
        assert!(
            injected
                .fix
                .contains("sbx secret set github --sandbox thing-fleet"),
            "the recipe must name the sandbox, or it writes the global secret: {}",
            injected.fix
        );

        let untouched = proxy_injection_line(ProxyCredential::Untouched, "thing-fleet");
        assert!(
            !untouched.is_fault(),
            "a refused garbage credential is the proxy behaving: {}",
            untouched.detail
        );
        assert!(
            untouched.fix.is_empty(),
            "nothing to fix means no recipe: {}",
            untouched.fix
        );

        let absent = proxy_injection_line(ProxyCredential::Absent, "thing-fleet");
        assert!(
            !absent.is_fault(),
            "no proxy is not a fault — it is most deployments: {}",
            absent.detail
        );

        let unsure = proxy_injection_line(
            ProxyCredential::Unanswered("the probe was answered 403".to_string()),
            "thing-fleet",
        );
        assert_eq!(
            unsure.level,
            Level::Unknown,
            "a reading that is neither an acceptance nor a refusal must not be a fault, and must \
             not be a pass: {}",
            unsure.detail
        );
        assert!(
            unsure.detail.contains("403"),
            "an unknown says what it saw, or nobody can act on it: {}",
            unsure.detail
        );
    }

    /// **An acceptance is an injection only when somebody was authenticated for it**, against a
    /// real proxy socket. A property of the mechanism rather than of a string: curl is given `-x`
    /// and the listener answers as the proxy would, headers and all.
    ///
    /// The third case is the one worth having. An anonymous `200` — the shape sbx v0.43.0's
    /// credential-stripping produces — must NOT read as an injection, and a status-only probe
    /// cannot tell it from one. That is the false positive SKEIN-913 is about: a check that goes
    /// red for a reason the reader can see is not theirs is a check they learn to read past.
    ///
    /// **The concrete changes that make this fail, named before it was written:** returning
    /// `Untouched` from the authenticated arm of [`probe_proxy_injection_at`] — planted, and
    /// `an authenticated 200 is a substituted credential` failed; and dropping the
    /// `ceiling > ANONYMOUS_CEILING` guard so any `200` is an injection — planted, and
    /// `an anonymous 200 added no credential` failed.
    #[test]
    fn only_an_authenticated_acceptance_through_the_proxy_is_injection() {
        if !crate::github::have_curl() {
            return crate::testutil::skip(
                "no curl, and the injection probe under test is a wrapper around it",
            );
        }
        // Authenticated: GitHub's ceiling for a credential it recognised, far above the anonymous
        // one. SKEIN-927 recorded exactly this on the day injection was live.
        let (port, served) =
            fake_proxy("HTTP/1.1 200 OK\r\nx-ratelimit-limit: 5000\r\nContent-Length: 0\r\n\r\n");
        let injected =
            probe_proxy_injection_at(&format!("http://127.0.0.1:{port}"), PROBE_TARGET_FOR_TESTS);
        let _ = finish_responder(port, served);
        assert_eq!(
            injected,
            ProxyCredential::Injected { ceiling: 5000 },
            "an authenticated 200 is a substituted credential: the one that was sent cannot be \
             valid, so somebody else's was"
        );

        // Anonymous: accepted, and nobody was authenticated for it. A credential was dropped on the
        // way, not added — the opposite of what this check reports.
        let (port, served) = fake_proxy(format!(
            "HTTP/1.1 200 OK\r\nx-ratelimit-limit: {ANONYMOUS_CEILING}\r\nContent-Length: 0\r\n\r\n"
        ));
        let stripped =
            probe_proxy_injection_at(&format!("http://127.0.0.1:{port}"), PROBE_TARGET_FOR_TESTS);
        let _ = finish_responder(port, served);
        assert_eq!(
            stripped,
            ProxyCredential::Untouched,
            "an anonymous 200 added no credential, and must not paint the banner red"
        );

        let (port, served) = fake_proxy("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        let untouched =
            probe_proxy_injection_at(&format!("http://127.0.0.1:{port}"), PROBE_TARGET_FOR_TESTS);
        let _ = finish_responder(port, served);
        assert_eq!(
            untouched,
            ProxyCredential::Untouched,
            "a 401 is the probe's own credential arriving as sent"
        );

        // Port 1 on loopback refuses immediately: no proxy answered at all.
        let blind = probe_proxy_injection_at("http://127.0.0.1:1", PROBE_TARGET_FOR_TESTS);
        assert!(
            matches!(blind, ProxyCredential::Unanswered(_)),
            "a proxy that cannot be reached answers nothing, which is not a pass and not a fault: \
             {blind:?}"
        );
    }

    /// **The probe sends a credential that cannot be anybody's, and sends it to the proxy.**
    ///
    /// This is the security assertion of the pair, and it is about what leaves the machine rather
    /// than about what comes back. It reads the bytes the probe actually put on the socket and
    /// requires that the only `Authorization` on them is [`PROBE_CREDENTIAL`], which is prefixed
    /// `skein-test-` so that it cannot be mistaken for — or used as — a real one.
    ///
    /// **The concrete change that makes this fail, named before it was written:** dropping the
    /// `skein-test-` prefix from [`PROBE_CREDENTIAL`]. Planted, and
    /// `the probe's credential must be unmistakably not a credential` failed. Sending the request
    /// direct instead of through the proxy fails `the probe must go THROUGH the proxy`.
    #[test]
    fn the_probe_puts_nothing_but_a_marked_non_credential_on_the_wire() {
        if !crate::github::have_curl() {
            return crate::testutil::skip("no curl, and this reads what curl put on the socket");
        }
        assert!(
            PROBE_CREDENTIAL.starts_with("skein-test-"),
            "the probe's credential must be unmistakably not a credential: {PROBE_CREDENTIAL}"
        );
        let (port, served) = fake_proxy("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        let _ =
            probe_proxy_injection_at(&format!("http://127.0.0.1:{port}"), PROBE_TARGET_FOR_TESTS);
        let request = finish_responder(port, served);
        // First, because it is the one an empty request answers: a probe that reached the proxy at
        // all is the precondition for anything the bytes say about what it sent.
        assert!(
            request.starts_with(&format!("GET {PROBE_TARGET_FOR_TESTS} ")),
            "the probe must go THROUGH the proxy — an absolute-form request line is what a proxy \
             is asked, and a direct one would answer a different question: {request:?}"
        );
        let authorizations: Vec<&str> = request
            .lines()
            .filter(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            .collect();
        assert_eq!(
            authorizations,
            vec![format!("Authorization: Bearer {PROBE_CREDENTIAL}").as_str()],
            "exactly one Authorization, and it is the marked non-credential: {request:?}"
        );
    }

    /// A target the tests can reach a loopback listener with. `http`, because a proxy is asked for
    /// an absolute-form `GET` rather than a `CONNECT` — which is exactly the request shape under
    /// test — and a host that can never resolve, so a test that lost its `-x` fails instead of
    /// reaching out.
    const PROBE_TARGET_FOR_TESTS: &str = "http://api.github.invalid/rate_limit";

    /// Ends a single-shot responder thread with **no wall clock at all**.
    ///
    /// **SKEIN-1024, second round:** a fixed deadline counted from when the responder thread
    /// STARTS races the very probe it exists to protect, because the probe can be slow to even
    /// *begin* — a `curl` slow to fork under load, a wait on `env_lock` — for reasons that have
    /// nothing to do with whether the responder is listening. Planting an 11s delay between the
    /// readiness signal and the probe starting, with the old 10s-from-thread-start watchdog still
    /// in place, reproduced the ticket's exact failure again: the watchdog fired first, the
    /// responder accepted the watchdog's own empty connection and returned, and the real probe's
    /// connection — arriving after the listener was already dropped — was refused. Worse, that
    /// watchdog thread was detached (`drop` does not stop a thread), so on an ordinary run it
    /// still fired 10s after thread-start regardless of whether the real connection had already
    /// been served; by then the ephemeral port can belong to a LATER test's fake server, and the
    /// watchdog steals that server's one accept.
    ///
    /// There is nothing to guess at here: a production probe already bounds itself (`curl -m`), so
    /// by the time it returns to its caller it has either gotten an answer or given up for good.
    /// One connection from THIS thread, now — synchronously, the instant our own test code gets
    /// here, never on a timer and never from a detached thread — either unblocks a responder still
    /// waiting (the probe never reached it, or gave up before the responder was scheduled), or is
    /// refused against a socket nothing is listening on any more, because the real connection
    /// already arrived and the responder has already returned and dropped the listener. Either way
    /// at most one connection is EVER accepted — the real one or this one, never both — because the
    /// responder calls `accept()` exactly once, and never after this function returns, because
    /// nothing here runs on a delay.
    fn finish_responder<T>(port: u16, handle: std::thread::JoinHandle<T>) -> T {
        let _ = std::net::TcpStream::connect(("127.0.0.1", port));
        handle.join().expect("the fake server's thread panicked")
    }

    /// A listener that answers one request as a proxy would, and hands the request back.
    ///
    /// A plain blocking `accept()`, with no deadline of its own: the sabotage this exists to catch
    /// — taking `-x` off the probe, so it never reaches the proxy at all — cannot make this HANG,
    /// because [`finish_responder`] unblocks it deterministically the instant the probe (which
    /// bounds itself) returns, whether or not a real connection ever arrived.
    fn fake_proxy(answer: impl Into<String>) -> (u16, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let answer = answer.into();
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let _ = ready_tx.send(());
            let mut seen = String::new();
            if let Ok((mut sock, _)) = listener.accept() {
                let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut buf = [0u8; 4096];
                if let Ok(read) = sock.read(&mut buf) {
                    seen = String::from_utf8_lossy(&buf[..read]).to_string();
                }
                let _ = sock.write_all(answer.as_bytes());
                let _ = sock.flush();
            }
            seen
        });
        wait_until_accepting(ready_rx);
        (port, handle)
    }
}
