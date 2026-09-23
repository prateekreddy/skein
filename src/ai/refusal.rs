//! A refusal that will not change by asking again: remembered, dated, forgotten when the
//! credential changes or its hour is up, and repeated back instead of asked again.

use super::*;

/// What [`crate::util::spawn_failure`] says in front of the OS's own words. Named because
/// [`outside_box_because`] splits on it, and a literal spelled twice is a literal that drifts.
pub(super) const SPAWN_REFUSED: &str = "could not be started: ";

/// A refusal that will not change by asking again, remembered so it is not asked again.
///
/// **Why this exists.** A host whose `claude` cannot log in answers every call instantly, and the
/// review queue asks once per pull request. On macOS each of those attempts pops a system Keychain
/// dialog. Somebody opened the review tab and got a modal, repeatedly, from a fleet that had
/// already been told the answer six times in the same second.
///
/// Only the refusals that are ABOUT THE SETUP are remembered — a missing binary, or a CLI that ran
/// and refused. A timeout is a slow diff and the next one may be fine; an empty answer is about that
/// one prompt. Remembering those would turn one bad moment into a dead feature.
///
/// Cleared by a call that works, and by [`forget_refusal`] — which anything explicitly asked for
/// calls first, because "read this one" is a person saying they think it will work now.
///
/// **And it is never read directly.** Every reader goes through [`refusal_still_standing`], which
/// is where the rule below lives; a read of this mutex that skipped it would be the bug this
/// module was carrying.
static REFUSED: std::sync::Mutex<Option<Standing>> = std::sync::Mutex::new(None);

/// How long a refusal nothing has contradicted is allowed to keep speaking.
///
/// **The backstop, for the refusals evidence cannot reach.** A credential being rewritten answers
/// "is this still true" for an auth refusal and for nothing else: a `claude` that is not on PATH, a
/// sandbox that is not answering, an unreachable transport — those are conditions that get fixed
/// out there, with no file in here to notice it by. Held forever, they end the same way, which is
/// how this was met on a live fleet: a state a person cannot clear, that outlived what produced it,
/// and that nothing but a restart ends.
///
/// An hour, and the trade is stated rather than tuned. What it costs is one real call per hour per
/// surface in a fleet that is genuinely broken — on macOS, at most one Keychain dialog an hour. What
/// this memo exists to stop is one dialog *per pull request*, six in a second; an hour still stops
/// that completely. What it buys is that no refusal here can outlive a restart-shaped fix.
const REFUSAL_LIFE: Duration = Duration::from_secs(60 * 60);

/// A remembered refusal, with the two facts a surface needs beyond the sentence: WHEN it happened,
/// and which runtime's credential was being used when it did.
#[derive(Debug, Clone)]
struct Standing {
    why: Unread,
    at_ms: i64,
    runtime: &'static str,
    /// **The credential that was refused**, as [`crate::fleet::login_fingerprint`] sees it
    /// (SKEIN-348). What contradicts a refusal is a DIFFERENT credential, and nothing else.
    ///
    /// This used to be judged on the file's mtime, and that rule cleared the refusal on the very
    /// event that proved it: an OAuth client rewrites its credentials file when a refresh attempt
    /// FAILS, so the harder the CLI retried, the more thoroughly skein forgot it had been refused.
    /// Measured on a live fleet while every model call was coming back "OAuth session expired":
    /// `logins: ['claude']`, `expired_logins: []`, and no banner. Their words: "there is no popup
    /// though."
    ///
    /// `None` where there was no readable credential to fingerprint when the refusal was recorded —
    /// which a later readable one legitimately contradicts, because a credential appeared.
    credential: Option<u64>,
}

impl Standing {
    /// Is this refusal about the CREDENTIAL — the only kind a new credential can contradict?
    ///
    /// The same test [`auth_refusal`] applies, and deliberately the same one: what makes a refusal
    /// worth showing on the login banner is exactly what makes it answerable by logging in, so a
    /// second, looser spelling here would clear refusals nothing had contradicted.
    fn about_the_credential(&self) -> bool {
        match &self.why {
            Unread::Refused { said, .. } => says_the_credential_is_dead(said),
            _ => false,
        }
    }
}

/// Do these words mean "this credential is not good any more"?
///
/// A substring list because the CLIs' wording is theirs to change; the cost of a miss is the banner
/// not appearing, which is where this started.
fn says_the_credential_is_dead(said: &str) -> bool {
    let lower = said.to_lowercase();
    // Every shape seen from `claude` and `codex`.
    [
        "oauth session expired",
        "could not be refreshed",
        "failed to authenticate",
        "please run /login",
        "not logged in",
        "invalid api key",
        "unauthorized",
        "401",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// The remembered refusal, **if it still describes anything**.
///
/// **A refusal is a fact about one moment, not a standing state.** This is `prq::what_github_said`'s
/// rule at the credential layer, and the fourth sighting of the pattern SKEIN-281 named: a
/// per-process memo holding a failure as a fact, outliving the condition that produced it, with
/// nothing a person can clear. There it was a rate-limited lookup cached as an answer nobody gave;
/// here it is a dead credential remembered after somebody replaced it.
///
/// The question this answers came from a live fleet — *"when login is complete from other session
/// or something does the bar go away?"* It did not, and it could not: [`forget_refusal`] runs from
/// THIS process's `skein login`, from a call that then succeeds, and from a person pressing read. A
/// `/login` inside a box, a second skein, the desktop app — none of them reach this memory. So the
/// bar stayed up over a credential that was fine, saying something true about the past, and a person
/// reading it concluded their login had failed.
///
/// Two ways out, and it needs both:
///
///   * **Evidence.** A credential written after the refusal is a different credential
///     ([`crate::fleet::login_written_ms`]), so nothing the model said about the old one applies.
///     No press, no restart, and it works however the login happened. Only for a refusal that is
///     ABOUT the credential — a rewritten token says nothing about a `claude` that is not on PATH.
///   * **A clock.** For every other kind there is no file to notice a fix by, so [`REFUSAL_LIFE`]
///     bounds it. A refusal that cannot be contradicted by evidence must expire on a clock rather
///     than on a restart.
///
/// **Cleared, not merely hidden.** A stale refusal is dropped from the memo here, so the next call
/// is made for real and re-plants one if it is still true — the same shape as `what_github_said`
/// declining to write down what it was never told. Hiding it from the banner while the call path
/// went on declining would have fixed the sentence and left the fleet mute.
fn refusal_still_standing() -> Option<Standing> {
    let mut held = match REFUSED.lock() {
        Ok(held) => held,
        // Poison-tolerant: the value is a remembered answer, with no invariant a panicking caller
        // could have left half-written.
        Err(poisoned) => poisoned.into_inner(),
    };
    let standing = held.clone()?;
    // **A DIFFERENT credential contradicts a refusal. A rewritten one does not** (SKEIN-348).
    // `is_some_and`, so a credential that cannot be read right now clears nothing: "I could not
    // look" and "it is different" are different answers and only the second may forgive a refusal.
    let contradicted = standing.about_the_credential()
        && crate::fleet::login_fingerprint(standing.runtime)
            .is_some_and(|now| Some(now) != standing.credential);
    let aged = now_ms().saturating_sub(standing.at_ms) > REFUSAL_LIFE.as_millis() as i64;
    if contradicted || aged {
        *held = None;
        return None;
    }
    Some(standing)
}

/// Now, in epoch milliseconds. One spelling, because a refusal's age is compared against a file's
/// mtime and the two have to be counted from the same place.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// What the model itself said about a credential — evidence the credential FILE does not carry.
///
/// `fleet::expired_logins` reads `refreshTokenExpiresAt` and believes it. A token can be revoked,
/// or fail to refresh, long before that date: the file still reads live and every model call comes
/// back `Failed to authenticate: OAuth session expired and could not be refreshed`. Reported live
/// from a live fleet, whose cockpit showed the sentence on a pull request row and no banner
/// anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRefusal {
    /// `claude` or `codex` — whichever binary was refused.
    pub runtime: &'static str,
    /// When skein was told, in epoch milliseconds. Not when the credential died, which nothing here
    /// can know: the honest stamp is the moment it was found out.
    pub at_ms: i64,
    /// The refusal's own words, for the surface that shows them.
    pub said: String,
}

/// The standing refusal when it is about AUTHENTICATION, and `None` for every other kind.
///
/// Deliberately narrow: a model that is rate limited, missing from the PATH or slow has said
/// nothing about the credential, and reporting those as a dead login would send somebody to log in
/// again over a problem logging in cannot fix.
///
/// Through [`refusal_still_standing`], never the memo itself: a banner is exactly the surface this
/// is about, and one that read the raw memory would go on showing a login as dead after somebody
/// fixed it somewhere else.
pub fn auth_refusal() -> Option<AuthRefusal> {
    let standing = refusal_still_standing()?;
    let said = match &standing.why {
        Unread::Refused { said, .. } => said.clone(),
        _ => return None,
    };
    says_the_credential_is_dead(&said).then_some(AuthRefusal {
        runtime: standing.runtime,
        at_ms: standing.at_ms,
        said,
    })
}

/// Stop declining, and try the next call for real. Called by `skein login` and by an explicit re-read.
pub fn forget_refusal() {
    if let Ok(mut held) = REFUSED.lock() {
        *held = None;
    }
}

/// Test probes for the refusal memory, which is deliberately private otherwise.
/// `fleet::after_login` promises to clear the standing refusal, and clearing has no effect
/// observable from outside this module — these let its test plant one and watch it go. Hold
/// `testutil::env_lock` around them: `REFUSED` is process-global, and every test that manufactures
/// refusals serializes on that lock (see `a_model_call_that_fails_says_which_failure_it_was`).
#[cfg(test)]
pub(crate) fn plant_refusal_for_test() {
    plant_refusal_saying("planted by a test");
}

/// The same, with the refusal's own words — for a test that needs an AUTH-shaped one, which is
/// what `auth_refusal` reads and what the cockpit's login banner is driven by.
#[cfg(test)]
pub(crate) fn plant_refusal_saying(said: &str) {
    remember_refusal(
        &Unread::Refused {
            code: "1".into(),
            said: said.into(),
        },
        "claude",
        Turn::Alone,
    );
}

/// The same, dated — for the two rules in [`refusal_still_standing`], both of which are about WHEN
/// a refusal happened and neither of which a test can reach by waiting.
#[cfg(test)]
pub(crate) fn plant_refusal_aged(said: &str, ago: Duration) {
    plant_refusal_saying(said);
    if let Ok(mut held) = REFUSED.lock() {
        if let Some(standing) = held.as_mut() {
            standing.at_ms -= ago.as_millis() as i64;
        }
    }
}

#[cfg(test)]
pub(crate) fn refusal_standing_for_test() -> bool {
    standing_refusal().is_some()
}

/// The refusal being repeated back, if there is one that still describes anything.
pub(super) fn standing_refusal() -> Option<Unread> {
    refusal_still_standing().map(|standing| standing.why)
}

pub(super) fn remember_refusal(why: &Unread, bin: &str, turn: Turn<'_>) {
    // **A call that named a conversation is not evidence about the runtime** (SKEIN-376). Asking to
    // resume a session the sandbox no longer has is answered `No conversation found with session
    // ID: <id>` and exit 1 — an ordinary answer to an ordinary question, and remembering it would
    // make one pull request's forgotten session refuse every model call skein makes until the
    // standing refusal aged out.
    //
    // Nothing is lost by declining to remember here, and that is the part worth checking rather
    // than assuming: [`claude_in_conversation`] always ends its ladder at [`Turn::Alone`], so a
    // login that is genuinely broken still refuses a call that names no conversation, and THAT is
    // the one remembered. The memo keeps its whole job — one Keychain dialog rather than one per
    // pull request — and stops covering the one case where it was answering the wrong question.
    if !matches!(turn, Turn::Alone) {
        return;
    }
    // A setup problem, not a bad moment. See the type above.
    if !matches!(
        why,
        Unread::Missing { .. }
            | Unread::Refused { .. }
            | Unread::Unreachable { .. }
            | Unread::AbsentInSandbox { .. }
    ) {
        return;
    }
    if let Ok(mut held) = REFUSED.lock() {
        *held = Some(Standing {
            why: why.clone(),
            at_ms: now_ms(),
            // Which credential was in play. `codex` names itself in the binary; everything else
            // skein asks is `claude`, including a `$SKEIN_CLAUDE_BIN` pointed at a stub.
            runtime: match bin.contains("codex") {
                true => "codex",
                false => "claude",
            },
            credential: crate::fleet::login_fingerprint(match bin.contains("codex") {
                true => "codex",
                false => "claude",
            }),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A refusal is a fact about one moment. Two things end it, and neither is a restart.
    ///
    /// Asked of a live fleet: *"when login is complete from other session or something does the
    /// bar go away?"* It did not. `forget_refusal` runs from THIS process's `skein login`, from a
    /// call that then succeeds, and from a person pressing read — a `/login` in a box, a second
    /// skein or the desktop app reaches none of them, so the memo outlived the credential it was
    /// about. The fourth sighting of the pattern SKEIN-281 named, and `prq::what_github_said` is
    /// where the rule is written: a per-process memo holds only what the world actually said, for
    /// as long as it is still saying it.
    ///
    /// Driven on the memo itself and NOT only on the banner, because hiding a stale refusal from
    /// `auth_refusal` while `tried` went on answering from it would fix the sentence and leave every
    /// summary declining.
    #[test]
    fn a_refusal_ends_when_the_credential_changes_or_when_its_hour_is_up() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let home = dir.join("fleet");
        // Both HOMEs the call chooses between, pointed somewhere this test owns: the ambient one
        // counts too, and a developer's real credential must not decide this.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("HOME", dir.join("ambient"));
        let credential = home.join("fleet-home/.claude/.credentials.json");
        fs::create_dir_all(credential.parent().unwrap()).unwrap();
        forget_refusal();

        // ---- evidence: a login completed anywhere at all ----
        plant_refusal_aged(
            "`claude` exited 1: Failed to authenticate: OAuth session expired and could not be \
             refreshed",
            Duration::from_secs(600),
        );
        assert!(
            refusal_standing_for_test() && auth_refusal().is_some(),
            "the refusal was not planted, so nothing below is testing anything"
        );
        // Nobody presses anything and nothing calls `forget_refusal`: a credential simply appears,
        // which is all a login in a box or a second skein leaves behind.
        fs::write(&credential, br#"{"claudeAiOauth":{"accessToken":"fresh"}}"#).unwrap();
        assert!(
            auth_refusal().is_none(),
            "the banner still reports a login the fleet has since replaced — a true statement about \
             the past, read by a person as their login having failed"
        );
        assert!(
            !refusal_standing_for_test(),
            "the banner cleared and the call path did not, so every summary goes on declining over \
             a credential that is fine"
        );

        // ---- A REWRITE IS NOT A REPLACEMENT (SKEIN-348) ----
        //
        // The case that broke this in the field. An OAuth client rewrites its credentials file when
        // a refresh ATTEMPT FAILS — it keeps timestamps and attempt state in there — so the file is
        // newer, and its bytes differ, while the token is the same dead token. Judged on mtime (and
        // judged on the file's bytes) that reads as a fresh login, and the banner clears on the very
        // event that proves the credential is dead. Measured on a live fleet, with every model
        // call coming back "OAuth session expired": `logins: ['claude']`, `expired_logins: []`, and
        // no banner. Their words: "there is no popup though."
        forget_refusal();
        fs::write(
            &credential,
            br#"{"claudeAiOauth":{"accessToken":"same-dead-token","refreshedAt":1}}"#,
        )
        .unwrap();
        plant_refusal_saying(
            "`claude` exited 1: Failed to authenticate: OAuth session expired and could not be \
             refreshed",
        );
        // The failing client writes again: same token, new bookkeeping, later mtime.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(
            &credential,
            br#"{"claudeAiOauth":{"accessToken":"same-dead-token","refreshedAt":2}}"#,
        )
        .unwrap();
        assert!(
            auth_refusal().is_some(),
            "a failed refresh rewrote the credential file and skein read that as a new login, so \
             the one condition the banner exists for is the one it cannot report"
        );
        // And the real thing still clears it: a DIFFERENT token is a different credential.
        fs::write(
            &credential,
            br#"{"claudeAiOauth":{"accessToken":"a-genuinely-new-token"}}"#,
        )
        .unwrap();
        assert!(
            auth_refusal().is_none(),
            "logging in again left a new token and the refusal outlived it"
        );

        // ---- and evidence is not a licence to forget everything ----
        // A refusal younger than the credential is still the current fact, and clearing it here
        // would turn this memo off altogether — one Keychain dialog per pull request, which is what
        // it exists to stop.
        plant_refusal_saying("`claude` exited 1: Failed to authenticate: OAuth session expired");
        assert!(
            auth_refusal().is_some(),
            "a refusal that happened AFTER the credential was written was thrown away, so the memo \
             holds nothing and the fleet asks once per pull request again"
        );

        // ---- the clock, for the refusals no file can contradict ----
        forget_refusal();
        // Not auth-shaped, so `login_written_ms` says nothing about it however many logins happen —
        // exactly the case that used to end only at a restart.
        plant_refusal_aged("`claude` exited 1: rate limit reached", REFUSAL_LIFE / 2);
        assert!(
            refusal_standing_for_test(),
            "a refusal from half an hour ago was already forgotten, so the memo does not hold long \
             enough to be worth having"
        );
        plant_refusal_aged("`claude` exited 1: rate limit reached", REFUSAL_LIFE * 2);
        assert!(
            !refusal_standing_for_test(),
            "a refusal older than its life is still speaking, so a fix made out there ends only \
             when somebody restarts the server"
        );

        forget_refusal();
    }

    /// A refusal about the setup is asked once, not once per row.
    ///
    /// A host whose `claude` cannot log in answers instantly, and the review queue asks once per
    /// pull request. On macOS every one of those pops a Keychain dialog — somebody opened the tab
    /// and got a modal, repeatedly, from a fleet that had been told the answer six times in the same
    /// second. Reported as "why does it keep asking me".
    #[cfg(unix)]
    #[test]
    fn a_refusal_about_the_setup_is_only_asked_once() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        // Shared with every other test in this module, and a panic skips the cleanup at the end:
        // clear the remembered refusal on the way IN. Without it a sibling's failure makes this
        // one's stub never run, and only in a parallel run.
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_AI", "on");
        env.set("HOME", home);
        forget_refusal();

        // Counts how many times it is actually run.
        let ran = home.join("ran");
        let mut stub = |body: &str| {
            let at = home.join("claude");
            fs::write(
                &at,
                format!("#!/usr/bin/env bash\necho x >> {}\n{body}\n", ran.display()),
            )
            .unwrap();
            fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
            env.set("SKEIN_CLAUDE_BIN", &at);
        };
        let times = || {
            fs::read_to_string(&ran)
                .map(|t| t.lines().count())
                .unwrap_or(0)
        };

        stub("echo 'Not logged in · Please run /login'; exit 1");
        for _ in 0..6 {
            assert!(claude_oneshot("hi").is_none());
        }
        assert_eq!(
            times(),
            1,
            "a `claude` that cannot log in was run once per call — on macOS that is one system \
             dialog per pull request, for an answer already given"
        );
        // And the reason is still the real one, not a shrug about having given up.
        let said = claude_oneshot_telling("hi", None, Duration::from_secs(5))
            .unwrap_err()
            .say();
        assert!(
            said.contains("Not logged in"),
            "the remembered refusal lost its reason: {said}"
        );

        // A person asking explicitly gets a real attempt: a standing refusal must never make a
        // button do nothing.
        forget_refusal();
        assert_eq!(times(), 1, "clearing it must not itself run anything");
        assert!(claude_oneshot("hi").is_none());
        assert_eq!(times(), 2, "an explicit ask did not reach the model");

        // A timeout is NOT remembered — that is a slow diff, and the next one may be fine.
        // Remembering it would turn one bad moment into a dead feature.
        forget_refusal();
        stub("sleep 30");
        for _ in 0..2 {
            let _ = claude_oneshot_telling("hi", None, Duration::from_millis(300));
        }
        assert_eq!(
            times(),
            4,
            "a timeout was remembered as though it were a broken setup"
        );

        // And success clears whatever was standing.
        forget_refusal();
        stub("echo ok");
        assert_eq!(claude_oneshot("hi").as_deref(), Some("ok"));
        assert!(
            standing_refusal().is_none(),
            "a working call left a refusal standing"
        );

        forget_refusal();
    }
}
