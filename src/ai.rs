//! Rationed, lazy AI enrichment over the Claude subscription — no API key.
//!
//! skein runs *inside* an `sbx run` box where `claude` is logged in, so every call here rides the
//! SAME rate-limit window as the fleet doing the real work. That is the whole reason this is
//! opt-in, on demand only, cached per turn-end, and never a per-tick fleet sweep.
//!
//! The governing rule, which every function here obeys: **AI may only add scrutiny, never remove
//! it.** The batch-resume gate can hold a box back; it can never clear one the free heuristic
//! wouldn't already have cleared. A flaky, garbled or absent answer therefore fails toward asking
//! you.

use crate::config::load_config;
use crate::signals::{session_signal, SessionSignal};
use crate::util::valid_name;
use std::env;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// skein runs *inside* an `sbx run` box where `claude` is logged in on the subscription, so every AI
/// call rides the SAME rate-limit window as the fleet doing the real work. AI is therefore OFF unless
/// you opt in with `$SKEIN_AI=on`, and even then it is lazy (on demand only), cached per turn-end, and
/// never a per-tick fleet sweep. The governing rule: AI may only *add* scrutiny, never remove it.
pub fn ai_enabled() -> bool {
    // `$SKEIN_AI` wins when set — same precedence as every other skein setting, and it is what
    // lets the tests stub this without touching the user's config. Otherwise the toggle in
    // Settings → Boxes decides, so the feature is discoverable rather than folklore.
    match env::var("SKEIN_AI").ok().as_deref() {
        Some("on" | "1" | "true" | "yes") => true,
        Some("off" | "0" | "false" | "no") => false,
        _ => load_config().ai_enrichment,
    }
}

/// Whether skein may read a pull request you have already opened a queue to look at.
///
/// A **second** switch, defaulting the opposite way to [`ai_enabled`], and they are separate on
/// purpose: one gates background enrichment that runs whether or not you asked for it, the other
/// gates work you asked for by opening the queue.
///
/// It lives here rather than in `review` because `health` has to be able to ask, and `health` does
/// not depend on `review`. Two copies of the rule would have been the alternative, and the bug this
/// was found by is what two views of the same question costs: the health check asked only about
/// [`ai_enabled`], reported "off", and said nothing at all about a fleet whose summaries were
/// switched ON and failing on every pull request.
pub fn summaries_enabled() -> bool {
    match env::var("SKEIN_REVIEW_AI").ok().as_deref() {
        Some("on" | "1" | "true" | "yes") => true,
        Some("off" | "0" | "false" | "no") => false,
        _ => load_config().review_summaries,
    }
}

/// What, if anything, wants the model — named, so a report can say which switch it is talking about.
pub fn model_wanted() -> Vec<&'static str> {
    let mut wanted = Vec::new();
    if ai_enabled() {
        wanted.push("box summaries");
    }
    if summaries_enabled() {
        wanted.push("review summaries");
    }
    wanted
}

/// Can skein make the model call it actually makes? Asked by running one.
///
/// **It used to ask `--version`**, on the grounds that the commonest failure is a binary that is not
/// on the server's PATH and that question is free. It is free because it answers a *different*
/// question. Measured against the real CLI: `claude --version` prints its version happily inside a
/// sandbox whose temp directory the CLI will refuse to use, and `claude -p` in the same shell exits
/// 1 — so `skein doctor` said `model … claude runs here` on a fleet where every single summary was
/// coming back unread. A check that cannot fail the way the feature fails is not a check.
///
/// So this runs the real thing, through [`claude_oneshot_telling`] — same binary, same HOME, same
/// temp directory, and the same trip into the sandbox when that is where the login lives. One
/// haiku-sized token of spend, only when a person types `doctor`, and it is the only wiring in
/// skein that reports on the model without guessing.
///
/// [`forget_refusal`] first: a person running `doctor` is asking for the current answer, not for
/// the circuit breaker's memory of an older one.
pub fn model_reachable() -> Result<(), Unread> {
    forget_refusal();
    let out = claude_oneshot_telling(
        "Reply with the single word: ok",
        None,
        Duration::from_secs(30),
    );
    // A refusal found by `doctor` is not remembered — the *next* real call should try for itself
    // rather than inherit a verdict from a diagnostic.
    forget_refusal();
    out.map(|_| ())
}

/// One-shot headless Haiku over the subscription: `claude -p --model <haiku>`. Returns trimmed
/// stdout, or None when AI is disabled / `claude` is absent / the call fails or times out — every
/// caller treats None as "fall back to the free deterministic path". `$SKEIN_CLAUDE_BIN` and
/// `$SKEIN_AI_MODEL` override the binary and model (and let tests stub the call).
pub(crate) fn claude_oneshot(prompt: &str) -> Option<String> {
    if !ai_enabled() {
        return None;
    }
    claude_oneshot_with(prompt, None, Duration::from_secs(30))
}

/// [`claude_oneshot`] with the model and time budget named at the call site.
///
/// Two tiers exist because two jobs do. Classifying a diff is cheap work a small model does well;
/// explaining what a change means at product level is not, and giving both the same 30s is how the
/// expensive one silently starts failing — which, under the rule that AI may only add scrutiny,
/// degrades to "read it yourself" rather than to a wrong answer, but degrades all the same.
///
/// **This function does not check whether AI is switched on — its caller must.** There are two
/// budgets now and they default opposite ways: [`ai_enabled`] gates the background enrichment that
/// runs whether or not you asked for it, while [`crate::review::summaries_enabled`] gates reading a
/// PR you have already opened a queue to look at. A single gate in here would force one policy on
/// both, and the wrong one on whichever it wasn't written for.
///
/// `None` on every failure path: absent binary, non-zero exit, timeout, or empty output. Callers
/// must treat `None` as "fall back", never as an answer.
pub(crate) fn claude_oneshot_with(
    prompt: &str,
    model: Option<&str>,
    timeout: Duration,
) -> Option<String> {
    // The model and the machine are both decided in `claude_oneshot_telling` now, so this is the
    // Option-shaped door onto the same call: every caller that treats `None` as "fall back to the
    // free deterministic path" keeps working, and the ones that show a person why use the other.
    claude_oneshot_telling(prompt, model, timeout).ok()
}

/// Why a model call produced nothing — because "it produced nothing" is four different problems.
///
/// They were one `None`, and the caller rendered every one of them as "the model call failed or
/// timed out". Reported from a live fleet as "all summarization fails", with that sentence as the
/// entire evidence — and the failure had come back in two seconds, so of the two things the message
/// named, it was not the second.
///
/// Each variant carries its own cure, because they have four different ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// `claude` could not be started at all — almost always not on this process's PATH. **Local
    /// spawns only.** A call that was to run in the sandbox and never got there is `Unreachable`:
    /// saying "could not start `claude`" for a missing `sbx` sends a person to check a binary that
    /// is fine, and to a cure (`SKEIN_CLAUDE_BIN`) for a problem they do not have.
    Missing { bin: String, why: String },
    /// The sandbox the call was to run in could not be reached, so the model was never asked.
    /// About the transport — `sbx`, or the sandbox being down — and about nothing else.
    Unreachable { sandbox: String, why: String },
    /// The sandbox answered, and the CLI is not installed in it. Distinct from `Missing`, whose
    /// cure is the PATH of the process running skein-server: that PATH has no bearing on this one.
    AbsentInSandbox { bin: String, sandbox: String },
    /// It ran and refused. Its own stderr is the diagnosis: not logged in, a model it will not
    /// serve, a rate limit.
    Refused { code: String, said: String },
    /// It was still going when the budget ran out.
    Slow(Duration),
    /// The prompt was bigger than [`PROMPT_CEILING`], so nothing was spawned and no model was
    /// asked. Carries the two numbers, because the reader's question is "by how much".
    ///
    /// **Refused rather than attempted**, which is the whole of what this variant buys. Nothing
    /// measured the prompt before SKEIN-684; a prompt too big to send arrived as
    /// [`Unread::Missing`] — the `E2BIG` from `execve` looks exactly like any other failed spawn —
    /// and told the reader that `claude` could not be started, sending them to check a PATH that
    /// was fine.
    TooLarge { bytes: usize, limit: usize },
    /// It succeeded and said nothing.
    Silent,
}

impl Unread {
    /// The sentence to show, cure included. One line, because it lands in a row on a board.
    pub fn say(&self) -> String {
        match self {
            // Named as a PATH problem rather than as "not installed", because that is what it
            // nearly always is: the server inherits the PATH of whatever launched it, which on a
            // desktop is often not the shell where `claude` was installed.
            Unread::Missing { bin, why } => format!(
                "skein could not start `{bin}`{}. It is on the PATH of the process running \
                 skein-server that matters, not your shell's — start the server from a shell \
                 that has it, or set SKEIN_CLAUDE_BIN to its full path.",
                match for_a_row(why).as_str() {
                    "" => String::new(),
                    short => format!(" ({short})"),
                }
            ),
            // Leads with the sandbox, because the reader's next move is `sbx ls` and not anything
            // to do with the model. The `why` carries the PATH skein actually had — and it stays
            // in [`Unread::detail`], not here.
            Unread::Unreachable { sandbox, why } => format!(
                "skein could not reach the fleet sandbox `{sandbox}`, so the model was never \
                 asked{}. Check it with `sbx ls`, or run `skein doctor` for the search path \
                 skein had.",
                match for_a_row(why).as_str() {
                    "" => String::new(),
                    short => format!(": {short}"),
                }
            ),
            Unread::AbsentInSandbox { bin, sandbox } => format!(
                "`{bin}` is not installed in the fleet sandbox `{sandbox}` — which is where skein \
                 makes model calls, because that is where your login is. The PATH of the process \
                 running skein-server has no bearing on this one."
            ),
            Unread::Refused { code, said } if said.is_empty() => format!(
                "`claude` exited {code} without saying why. Run the same call by hand to see it: \
                 `claude -p --model claude-haiku-4-5 hello`."
            ),
            Unread::Refused { code, said } => {
                format!("`claude` exited {code}: {}", crate::util::clip(said, 240))
            }
            Unread::Slow(budget) => format!(
                "`claude` was still going after {}s. A larger diff needs longer than this call \
                 allows; nothing is wrong with the model.",
                budget.as_secs()
            ),
            // Both numbers, and the overrun between them: "too large" alone leaves the reader
            // unable to tell a prompt that missed by a hundred bytes from one that missed by four
            // times, and those want different answers.
            Unread::TooLarge { bytes, limit } => format!(
                "this prompt is {bytes} bytes and skein will not send more than {limit}, so no \
                 model was asked. Nothing is wrong with the setup — the change being read is \
                 larger than anything skein is built to hand a model in one call."
            ),
            Unread::Silent => {
                "`claude` answered with nothing at all, so there is nothing to vouch for.".into()
            }
        }
    }

    /// Everything [`Unread::say`] left out, for a surface that has room for it — `skein doctor`.
    ///
    /// `None` when the sentence already carries the whole diagnosis, which is most of the time.
    /// **Derived, not decided**: this asks [`Unread::say`] whether it kept the transport's words,
    /// so the day a message stops eliding something is the day this stops offering it, with no
    /// second rule to keep in step.
    pub fn detail(&self) -> Option<String> {
        let why = match self {
            Unread::Missing { why, .. } | Unread::Unreachable { why, .. } => why.trim(),
            _ => return None,
        };
        match why.is_empty() || self.say().contains(why) {
            true => None,
            false => Some(why.to_string()),
        }
    }
}

/// A transport's own diagnosis, cut down to what a row can carry — the environment left out of it.
///
/// **The bug this exists to end** (SKEIN-384). [`crate::util::spawn_failure`] puts the process's
/// ENTIRE PATH in the message, on purpose and correctly: it is the one fact a reader cannot look up
/// afterwards, because by then they are looking at their shell's PATH, which is a different PATH.
/// That is right in a diagnostic and wrong in a queue row. Seen in the review pane: a row whose
/// reading failed because `sbx` was absent carried ~300 characters of search path beside a pull
/// request title — longer than the row it was attached to, and the page draws the same string
/// twice per open row (`src/web/index.html:5511` and `:5895`).
///
/// So the row gets the sentence and [`Unread::detail`] keeps the dump.
///
/// **Elided by VALUE, not by shape.** What comes out is `$PATH` itself, read from this process at
/// the moment the sentence is written — so there is no pattern here that can drift away from the
/// message it was written against. A single-entry PATH is left alone: `/bin` is a substring of
/// ordinary prose, and a real search path is not.
fn for_a_row(why: &str) -> String {
    let mut short = why.trim().to_string();
    if let Ok(path) = env::var("PATH") {
        if path.contains(':') && !path.is_empty() {
            // The parenthetical goes whole where it is one, so the sentence does not keep an empty
            // pair of brackets; anywhere else the path is stood in for, so the sentence still reads.
            short = short.replace(&format!(" ({path})"), "").replace(&path, "…");
        }
    }
    // The first sentence only. In these messages what follows it is the explanation — "a shell you
    // start by hand may well find it…" — and the explanation is what `detail` is for.
    if let Some(end) = short.find(". ") {
        short.truncate(end);
    }
    crate::util::clip(short.trim().trim_end_matches('.').trim(), 120)
}

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
fn standing_refusal() -> Option<Unread> {
    refusal_still_standing().map(|standing| standing.why)
}

fn remember_refusal(why: &Unread, bin: &str, turn: Turn<'_>) {
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

/// **Which machine a turn runs on**, and therefore where its conversation is filed.
///
/// A third fact beside [`Turn`]'s id and directory, and it travels for the same reason those two
/// do: a conversation opened in one place can only be resumed in that place. Claude Code keys
/// sessions on the working directory, and a box has its own — its own `$HOME`, its own
/// `~/.claude/projects`, its own filesystem. So a round that opened in a review box and a round
/// that resumes in the sandbox are not two rounds of one conversation; they are two cold reads,
/// and the feature looks like it works while doing nothing at all. That failure has happened once
/// already (SKEIN-376) with only the directory unpinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Machine<'a> {
    /// Wherever skein's model calls already go, which is **this process**: skein runs inside the
    /// fleet sandbox, and that is where `skein login` put the credential (SKEIN-576). It used to be
    /// a choice — a host shipped the call in through `sbx exec` — and the shipping went with the
    /// host, so this is now the name for "no box was opened for this reading".
    Wherever,
    /// **This pull request's own review box** — `docs/pr-review.md` §11. Reached through its
    /// placement record, standing in a checkout of the commit under review.
    ///
    /// A failure here is not a reason to give up on the reading: the caller falls back to
    /// [`Machine::Wherever`], which is what every reading did before review boxes existed.
    Box(&'a str),
}

/// Which conversation a model call belongs to.
///
/// **Every call skein made before this was [`Turn::Alone`]** — a fresh context, thrown away, paying
/// to be told the diff again on every question about it. That is the right shape for a one-shot
/// classification and the wrong one for a review, which is a conversation: the reader asks a second
/// thing about the change the model just read, and there is no reason to buy the reading twice.
///
/// The CLI supplies both halves and skein CHOOSES the id, which is the part that matters — there is
/// no id to discover, store or keep in sync, so a caller that can name its conversation can resume
/// it. Verified against the real CLI (2026-08-26): `--session-id` on an id that already exists
/// fails with an empty stdout and exit 1, so a collision arrives through [`Unread::Refused`] rather
/// than as an error message parsed as an answer.
///
/// **A conversation carries the directory it is filed under, because it is not findable without
/// it** (SKEIN-376). Claude Code stores sessions under `~/.claude/projects/<slugified-cwd>/`, so
/// `--resume` only finds what a call in the SAME working directory created. Measured against the
/// installed CLI (2026-08-26): a session opened in one directory and resumed from another answers
/// `No conversation found with session ID: <id>` and exits 1, and the same resume from the
/// directory that opened it answers from memory. The id and the directory are therefore one fact,
/// and they travel together so that no caller can pin one and forget the other — unpinned, every
/// resume misses, every round is a cold read, and the feature looks like it works while doing
/// nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Turn<'a> {
    /// No conversation. The context dies with the call, and the directory does not matter.
    Alone,
    /// The first turn of one, under an id skein picked, in the directory it will be found in.
    Opening { id: &'a str, at: &'a Path },
    /// A later turn of one. The model still has what it was shown; do not send it again.
    Resuming { id: &'a str, at: &'a Path },
}

impl<'a> Turn<'a> {
    /// The flags this turn adds to the command line, in order.
    pub(crate) fn args(&self) -> Vec<&'a str> {
        match self {
            Turn::Alone => Vec::new(),
            Turn::Opening { id, .. } => vec!["--session-id", id],
            Turn::Resuming { id, .. } => vec!["--resume", id],
        }
    }

    /// Where the call must run for this conversation to be found. `None` only for [`Turn::Alone`],
    /// which has nothing to find.
    pub(crate) fn at(&self) -> Option<&'a Path> {
        match self {
            Turn::Alone => None,
            Turn::Opening { at, .. } | Turn::Resuming { at, .. } => Some(at),
        }
    }
}

/// The conversation a pull request's readings belong to — **derived, never stored** (SKEIN-376).
///
/// `<repo_id>#<number>` hashed into a uuid, so the same pull request produces the same id on every
/// round of every process, on any machine, with no mapping file to write, garbage-collect, or let
/// drift from the thing it names. A stored id can point at the wrong pull request; a derived one
/// cannot be wrong without the pull request itself being different.
///
/// It is keyed on the REPO as well as the number even though [`Turn`] already pins a per-repo
/// directory, and the redundancy is deliberate: two pull requests sharing a conversation is a
/// review answering about the wrong change, and that must not become possible the day somebody
/// changes where the call runs.
///
/// Version nibble 8 — RFC 9562's "custom" — because that is what this is: an id whose bits come
/// from the name rather than from a random source, and saying so costs nothing.
pub(crate) fn conversation_for(repo_id: &str, number: u64) -> String {
    let d = sha256(format!("{repo_id}#{number}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// SHA-256, hand-rolled for the same reason [`crate::apiauth::token`] is: this is the whole of what
/// a dependency would be used for, and it is a closed algorithm with a published answer.
///
/// **Proven against `sha256sum` rather than against itself.** An implementation can agree with its
/// own expectations and disagree with the world — `tracking.rs` says the same thing about the same
/// hash for the same reason — so the test that guards this shells out and compares.
fn sha256(msg: &[u8]) -> [u8; 32] {
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut data = msg.to_vec();
    let bits = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());
    for block in data.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut z) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = z
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            z = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, add) in h.iter_mut().zip([a, b, c, d, e, f, g, z]) {
            *slot = slot.wrapping_add(add);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// The most prompt skein will hand a model call, in bytes.
///
/// **Skein's own number, because the operating system no longer supplies one.** The prompt travels
/// on stdin ([`crate::util::output_with_timeout_fed`]), and a pipe has no size limit — so what is
/// left is a judgement about what a prompt this large *means*, and the answer is that it means a
/// caller has a bug. Measured 2026-09-08 against 27 real pull requests, reconstructed from the
/// owner's own review store and put through the real prompt builders: the largest prompt skein has
/// ever built was 305,366 bytes — one pull request of the busiest repository in that store, whose
/// 510,931-byte diff was truncated to
/// `review::asking::CRITIQUE_BYTES` — and the largest it *can* build is that truncation plus the
/// merged prompt's 4,858-byte scaffold plus the author's description — a little over 313,000. One
/// mebibyte is three times the ceiling of the design, which is room for the diff budgets to grow
/// before anybody has to think about this again, and small enough that a prompt built by accident
/// is refused in a sentence rather than spent as a fifteen-minute model call.
pub(crate) const PROMPT_CEILING: usize = 1 << 20;

/// The call, with the reason it failed kept.
///
/// `output_with_timeout_fed` rather than `bounded_output`: the second returns one string for "could
/// not start" and "ran out of time", which is where the four failures first became one.
pub(crate) fn tried(
    bin: &str,
    model: &str,
    prompt: &str,
    timeout: Duration,
    turn: Turn<'_>,
    github: Option<&crate::secret::Secret>,
) -> Result<String, Unread> {
    // **Before the standing refusal, because this is a fact about THIS call.** A cached refusal is
    // a fact about the setup and answering from it is right for everything below; a prompt bigger
    // than skein will send is wrong whatever the setup is doing, and reporting a stale "not logged
    // in" for it would send the reader to fix something that is not the problem.
    if prompt.len() > PROMPT_CEILING {
        return Err(Unread::TooLarge {
            bytes: prompt.len(),
            limit: PROMPT_CEILING,
        });
    }
    // Already told, and told something that asking again cannot change. Answering from memory is
    // the difference between one Keychain dialog and one per pull request.
    if let Some(known) = standing_refusal() {
        return Err(known);
    }
    let mut command = Command::new(bin);
    command.args(["-p", "--model", model]);
    command.args(turn.args());
    // **And the prompt is NOT here.** It goes on stdin, below (SKEIN-684). It used to be
    // `command.arg(prompt)` — the whole diff as one positional argument — which cost two things:
    //
    // * **a ceiling nothing checked.** Linux caps a single argv element at `MAX_ARG_STRLEN`, 32
    //   pages, independent of the much larger `ARG_MAX` total: measured on this box by spawning
    //   `/bin/true` with one argument of each length, 524,287 bytes ran and 524,288 was `E2BIG`,
    //   the cap counting the terminating NUL — so the boundary is 32 pages exactly. That is a
    //   16 KiB-page machine; on the 4 KiB pages of most hardware it is 131,072, and 3 of
    //   27 real prompts measured for SKEIN-684 were over that — the largest 305,366 bytes.
    // * **the payload in `ps`**, for as long as the call ran. `/proc/<pid>/cmdline` is world
    //   readable, and what skein puts in it is the diff of a pull request, private repositories
    //   included. SKEIN-516's rule is no secret on argv or in a URL; the GitHub credential has
    //   always obeyed it, and the diff was never considered under it.
    //
    // **How the CLI merges the two, established rather than assumed**, by pointing `claude` at a
    // local HTTP server standing in for the API (`ANTHROPIC_BASE_URL`) and reading the request it
    // sent. With a positional prompt alone the user message is that text; with stdin alone it is
    // the piped text, byte for byte the same message; with both it is `argv`, a newline, then
    // stdin. So dropping the positional argument and piping the same bytes sends the model exactly
    // what it was being sent before. `fleet::model_call_script` had already reached this shape from
    // the other side — it heredocs the prompt into `claude -p` with no positional argument — so
    // the two paths now agree about where a prompt goes.
    // **Where the call runs, because that is where its conversation is filed** (SKEIN-376). Without
    // this the spawn inherits the SERVER's directory, which is wherever somebody started it — so a
    // second round asking to resume looks in a different `~/.claude/projects/<cwd>` than the first
    // round wrote to, finds nothing, and reads the whole diff again. Nothing fails, which is the
    // problem: the resume falls back to a cold read and the saving quietly never happens.
    if let Some(at) = turn.at() {
        command.current_dir(at);
    }
    // **Which HOME the credential is read from**, because that is where this failed.
    //
    // `claude` finds its login at `$HOME/.claude/.credentials.json` and nowhere else — verified by
    // running it with HOME pointed at an empty directory, which reproduces the exact message a live
    // fleet was reporting: `Not logged in · Please run /login`. This spawned the CLI with no
    // environment at all, so it read whatever HOME the SERVER was started with. Meanwhile skein
    // keeps the fleet's login under `fleet-home`, reports it as `logins: ["claude"]`, and seeds
    // every box from it. It had the credential and was looking somewhere else.
    //
    // **The FLEET's login first** — the same credential every box is seeded and healed from, and the
    // one `skein login` writes. The ambient HOME used to win whenever it carried a usable login,
    // which meant skein could be reading pull requests on one account while every box worked on
    // another: a logout then showed up in one place and not the other, `expired_logins` (which
    // reads `fleet-home`) described a credential this call never touched, and the cockpit's banner
    // watched the wrong file. Why that is wrong, in the words it was reported in: using the boxes'
    // login makes a logout one fact, visible everywhere, with one fix.
    //
    // `login_home()` answers only for a credential that can still be refreshed, so this prefers the
    // fleet's when it works and falls back to the ambient one when it does not — it can never pick
    // a dead credential over a live one.
    let ambient = env::var_os("HOME").map(std::path::PathBuf::from);
    // Whether this call ends up on a login skein knows about — NOT merely whether a HOME exists.
    // The difference decides whether an inherited API key is removed below, and getting it wrong
    // means taking the only credential away from somebody who authenticates with a key.
    let (home, on_a_login) = match crate::fleet::login_home() {
        Some(own) => {
            command.env("HOME", &own);
            (Some(own), true)
        }
        None => {
            let usable = ambient
                .as_deref()
                .is_some_and(crate::fleet::refreshable_login_at);
            (ambient, usable)
        }
    };
    // **And which temp directory it writes into**, which is the same question asked about a
    // different directory — and the next thing that stopped a live fleet dead. The CLI refuses to
    // start when the path it derives from the shared `/tmp` is owned by somebody else, and in a
    // sandbox something else always ran first. See [`crate::fleet::MODEL_SCRATCH`].
    //
    // This path matters MOST in-fleet: there the sandbox call is skipped, because skein is already
    // inside the sandbox — so this spawn is the only one there is, in the very /tmp that is shared.
    if let Some(home) = home {
        command.env("CLAUDE_CODE_TMPDIR", crate::fleet::model_scratch_dir(&home));
    }
    // **And which credential it authenticates with**, which is the third time the same question has
    // been answered by the ambient environment rather than by skein. An `ANTHROPIC_API_KEY`
    // inherited from whatever launched the server outranks the subscription login skein seeds every
    // box from — see [`crate::fleet::MODEL_AUTH_OVERRIDES`].
    //
    // Gated on there being a login to prefer, not on there being a HOME: a host whose ONLY
    // credential is a key keeps it, because taking that away leaves the call with no
    // authentication at all — a worse failure than the one this fixes.
    if on_a_login {
        for key in crate::fleet::MODEL_AUTH_OVERRIDES {
            command.env_remove(key);
        }
    }
    // **And whether it can act on GitHub as you.** Decided 2026-08-27: a review
    // session gets the credential, so it reads the pull request and posts its own review rather
    // than handing an artefact back for skein to marshal.
    //
    // On the environment here rather than in a file, because this spawn is a CHILD of skein — the
    // env is not visible in `ps` on any modern kernel, and there is no second machine for a file to
    // be written on. The sandbox path has the opposite constraint and answers it the opposite way
    // (`fleet::github_export`): there, a file with mode 600, because an argument or an exported
    // shell line would sit in the sandbox's process list for the minutes the call runs.
    //
    // Both spellings, because tools disagree about which they read: `gh` prefers `GH_TOKEN`, the
    // GitHub Actions ecosystem writes `GITHUB_TOKEN`, and a session that reaches for the other one
    // finding nothing is indistinguishable from having no credential at all.
    //
    // **Set or REMOVED, never inherited.** Found by the test below, on this box: the server's own
    // environment carried a `GH_TOKEN`, so every model call already had one — including the cheap
    // summary ladder, which has nothing to do on GitHub and was handed the credential anyway. That
    // is the same defect [`crate::fleet::MODEL_AUTH_OVERRIDES`] exists for one field up, in its own
    // words: a value "inherited from whatever launched the server" outranking skein's own decision.
    // Which credential a model call CARRIES is skein's to decide, not the launching shell's — and
    // that is the whole of what this controls. It is not a bound on what the call can reach: in the
    // fleet sandbox the proxy answers a request carrying no credential as the account
    // (SKEIN-548, open; `crate::gitgate`'s module note has the measurement).
    match github.map(|t| t.expose().trim()).filter(|t| !t.is_empty()) {
        Some(token) => {
            command.env("GH_TOKEN", token);
            command.env("GITHUB_TOKEN", token);
        }
        None => {
            command.env_remove("GH_TOKEN");
            command.env_remove("GITHUB_TOKEN");
        }
    }
    let started = std::time::Instant::now();
    let out =
        crate::util::output_with_timeout_fed(&mut command, prompt.as_bytes().to_vec(), timeout)
            .map_err(|why| {
                // Told apart by the clock rather than by parsing the message: a spawn that fails does so
                // immediately, and anything that used its whole budget was running.
                match started.elapsed() >= timeout {
                    true => Unread::Slow(timeout),
                    false => Unread::Missing {
                        bin: bin.to_string(),
                        why,
                    },
                }
            });
    let out = match out {
        Ok(out) => out,
        Err(why) => {
            remember_refusal(&why, bin, turn);
            return Err(why);
        }
    };
    if !out.status.success() {
        let why = Unread::Refused {
            code: out
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".into()),
            // **Both streams, stdout first.** `claude -p` puts its diagnosis on STDOUT and leaves
            // stderr for unrelated noise — measured:
            //
            //   $ claude -p --model definitely-not-a-real-model x
            //   exit 1
            //   stdout: There's an issue with the selected model (…). It may not exist or you may
            //           not have access to it.
            //   stderr: Warning: no stdin data received in 3s…
            //
            // Reading stderr alone is why a live fleet was told "`claude` exited 1 without saying
            // why" for a failure the CLI had explained in full, on the other pipe.
            said: [&out.stdout, &out.stderr]
                .iter()
                .map(|raw| String::from_utf8_lossy(raw).trim().to_string())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" / "),
        };
        remember_refusal(&why, bin, turn);
        return Err(why);
    }
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match said.is_empty() {
        true => Err(Unread::Silent),
        false => {
            // It works. Whatever was wrong before is not wrong now.
            forget_refusal();
            Ok(said)
        }
    }
}

/// Read a sandbox run the same way a local one is read, so a call means the same thing wherever it
/// ran. The classification is the point of [`Unread`]; running somewhere else must not blur it.
///
/// **A `Ran`, and not a `Result<Ran, String>`** (SKEIN-619). This used to open by classifying a
/// call that never reached the far side at all: `Unread::Slow` where it had spent its whole
/// budget, `Unread::Unreachable` where it had failed fast. That arm had two producers and now has
/// none. One was the call `fleet` shipped into the fleet sandbox, deleted with the host-driven
/// deployment it belonged to (SKEIN-618 names it): skein runs inside that sandbox now, so there is
/// no far side left to cross to. The other is [`crate::fleet::model_call_in_box`], whose `Err` the
/// sole caller
/// below handles itself — it says so on stderr and lets the reading run where readings ran before
/// it — and that fall-through is deliberate rather than incidental: a review box that was never
/// started, or was destroyed when its pull request closed, is ordinary rather than an error, so
/// reporting the failure INSTEAD of falling through would turn an ordinary absence into a refusal.
///
/// Neither classification went with the arm, which is why this is a deletion rather than a loss. A
/// crossing that ARRIVES and comes back non-zero without the [`crate::fleet::REACHED`] marker is
/// still `Unread::Unreachable`, a few lines below; the local path still raises `Unread::Slow` off
/// its own clock, in [`tried`]. Only the arm nothing could produce is gone — and with it the
/// `timeout` and `started` this took for no other purpose than telling those two apart.
fn from_sandbox(ran: crate::fleet::Ran, bin: &str, turn: Turn<'_>) -> Result<String, Unread> {
    // **Did the script reach the sandbox at all?** `sbx exec` exits non-zero with its own message
    // when the daemon is not responding, when the sandbox is not running, or when it does not
    // exist — and the payload never runs. Read as an exit code alone that is indistinguishable
    // from the CLI refusing, which is how a fleet whose sandbox was fine got told
    // "`claude` exited 1: …" for a failure `claude` was never part of.
    //
    // The marker is printed by the script before it does anything else, so on a failure its ABSENCE
    // is evidence that nothing in the sandbox ever ran. See [`crate::fleet::REACHED`].
    let reached = ran.err.contains(crate::fleet::REACHED);
    // And it is skein's own bookkeeping, not something to show a person.
    let err = ran
        .err
        .lines()
        .filter(|line| line.trim() != crate::fleet::REACHED)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    let said_all = format!("{} {}", String::from_utf8_lossy(&ran.out).trim(), err);
    if ran.code != 0 && !reached {
        let out = Unread::Unreachable {
            sandbox: crate::fleet::fleet_sandbox(),
            why: match said_all.trim().is_empty() {
                true => format!("`sbx exec` exited {}, saying nothing", ran.code),
                false => crate::util::clip(said_all.trim(), 240),
            },
        };
        remember_refusal(&out, bin, turn);
        return Err(out);
    }
    // The sandbox answered, and the shell in it could not find the CLI. `claude` never ran, so
    // reporting its exit code would be reporting a number it did not produce — and the cure is in
    // the sandbox, not on the server's PATH.
    if ran.code == 127 && said_all.to_lowercase().contains("command not found") {
        let out = Unread::AbsentInSandbox {
            bin: bin.to_string(),
            sandbox: crate::fleet::fleet_sandbox(),
        };
        remember_refusal(&out, bin, turn);
        return Err(out);
    }
    if ran.code != 0 {
        // Both streams, stdout first — `claude -p` puts its diagnosis there, and `Place::exec`
        // would have thrown it away, which is why this path uses `Place::attempt`.
        let why = Unread::Refused {
            code: ran.code.to_string(),
            said: [String::from_utf8_lossy(&ran.out).trim().to_string(), err]
                .into_iter()
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" / "),
        };
        remember_refusal(&why, bin, turn);
        return Err(why);
    }
    let said = String::from_utf8_lossy(&ran.out).trim().to_string();
    match said.is_empty() {
        true => Err(Unread::Silent),
        false => {
            forget_refusal();
            Ok(said)
        }
    }
}

/// The same call, reporting why rather than only that. Used where the reason reaches a person.
pub(crate) fn claude_oneshot_telling(
    prompt: &str,
    model: Option<&str>,
    timeout: Duration,
) -> Result<String, Unread> {
    // No credential: this is the cheap summary ladder, and a call that only describes a change
    // has nothing to do on GitHub. The token goes to the calls that ACT (`claude_in_conversation`).
    // A one-shot has no conversation, so it has nowhere it must run: `Wherever` is not a fallback
    // here, it is the whole truth.
    claude_in_turn(prompt, model, timeout, Turn::Alone, None, Machine::Wherever)
}

/// One turn of THIS pull request's own conversation, resuming whatever earlier rounds left in it.
///
/// **The ladder asks the world rather than consulting a record** (SKEIN-376). Skein cannot see into
/// the sandbox to find out whether the session is still there, and a note saying "I have read this
/// before" would be a second source of truth that goes stale the moment a sandbox is recreated —
/// which is a thing skein deliberately lets happen silently. So the call tries the resume and reads
/// the answer.
///
/// Measured against the installed CLI (2026-08-26): `--resume` on an id it does not hold exits 1
/// straight away with `No conversation found with session ID: <id>` and spends nothing. So the
/// resume-first order costs one failed spawn on a pull request's first round and saves buying the
/// whole diff again on every round after it.
///
/// **Only a refusal steps down.** A call that ran out of time was working, and retrying it twice
/// more would spend three budgets on one reading. A missing binary or a refused login answers the
/// same way whichever flag it is handed — and `ai`'s standing refusal makes the second and third
/// attempts free — so stepping down there costs nothing and keeps the ladder one rule instead of a
/// list of exceptions.
///
/// The LAST failure is the one returned. "No conversation found" is true and useless: it describes
/// the attempt skein made on the reader's behalf, not the reason they have no reading.
pub(crate) fn claude_in_conversation(
    prompt: &str,
    model: Option<&str>,
    budget: Duration,
    id: &str,
    at: &Path,
    github: Option<&crate::secret::Secret>,
    machine: Machine<'_>,
) -> Result<String, Unread> {
    let ladder = [
        Turn::Resuming { id, at },
        Turn::Opening { id, at },
        // A conversation could not be had at all. The reading still happens — losing the memory
        // costs recall on the next round, and refusing to read costs the reader the review.
        Turn::Alone,
    ];
    let mut last = Unread::Silent;
    for turn in ladder {
        match claude_in_turn(prompt, model, budget, turn, github, machine) {
            Ok(said) => return Ok(said),
            Err(Unread::Refused { code, said }) => last = Unread::Refused { code, said },
            Err(other) => return Err(other),
        }
    }
    Err(last)
}

/// The same call, in a named conversation.
///
/// Every word of [`claude_oneshot_telling`]'s reasoning below applies unchanged — which HOME, which
/// temp directory, which credential, and the trip into the sandbox where the login lives. The only
/// difference is that the CLI is told which conversation this turn belongs to, and that has to be
/// decided HERE, beside the binary, for the same reason the binary is: a conversation belongs to
/// the machine the call runs on, and these two paths run it in two different places.
pub(crate) fn claude_in_turn(
    prompt: &str,
    model: Option<&str>,
    timeout: Duration,
    turn: Turn<'_>,
    github: Option<&crate::secret::Secret>,
    machine: Machine<'_>,
) -> Result<String, Unread> {
    let (bin, model) = binary_and_model(model);
    // **In the sandbox, where `skein login` put the credential.** Skein authenticated in one place
    // and spent it in another: the `/login` you type happens inside the sandbox, and this spawned
    // `claude` as a child of the server — on a host-driven deployment, a process on somebody's
    // laptop. On macOS that means the Keychain rather than a file, and a broken one answered
    // `Not logged in` for every summary while the sandbox held a working credential two hops away.
    //
    // Wrong in both deployments, not merely before the in-fleet move: the login is in the sandbox
    // either way.
    //
    // The decision belongs here rather than in `tried`, because this is where the binary is CHOSEN
    // — and `$SKEIN_CLAUDE_BIN` naming one means "run exactly this", which is also "run it here": a
    // path somebody named on this machine is not a path in the sandbox. `tried` is left meaning one
    // thing, "run it locally", which is what its callers in `model_reachable` and the tests want.
    let named = env::var_os("SKEIN_CLAUDE_BIN").is_some_and(|v| !v.is_empty());
    if !named {
        // **A box is asked first and answered last**: it is the most specific destination, and it
        // is the only one whose absence is ordinary. A review box that was never started, or was
        // destroyed when its pull request closed, is not an error — it is a reading that happens
        // the way every reading happened before §11. So a missing placement falls through to the
        // two below rather than being reported, and a box that ANSWERS is the answer, whatever it
        // said: `from_sandbox` already tells "the CLI refused" apart from "the script never ran".
        // `expose()` at the two sandbox seams below, and nowhere else on this path. `fleet`'s two
        // model-call entry points still take `Option<&str>`, so the credential becomes bare
        // characters for the length of the call and is a `Secret` on either side of it. That is the
        // last `&str` left on the GitHub credential's path out of this crate (SKEIN-519); it stays
        // until `src/fleet.rs` takes a `&Secret`, which is where the argument then has to be made
        // about `github_export` writing the same value to a file.
        let exposed = github.map(|t| t.expose());
        if let Machine::Box(name) = machine {
            match crate::fleet::model_call_in_box(
                name,
                &bin,
                &model,
                prompt,
                timeout,
                turn.args(),
                exposed,
            ) {
                Ok(ran) => return from_sandbox(ran, &bin, turn),
                Err(why) => eprintln!(
                    "skein: {name} could not take this turn, so it runs where readings ran before                      — {why}"
                ),
            }
        }
        // A second destination used to sit here: a call shipped into the fleet sandbox, because
        // that is where `skein login` put the credential and a host-driven skein was somewhere
        // else. Skein is in that sandbox now (SKEIN-576), so
        // running the call here IS running it where the login is, and the fall-through below is
        // that destination rather than a fallback from it.
    }
    tried(&bin, &model, prompt, timeout, turn, github)
}

/// **Which models this `claude` will accept, asked of `claude` itself** (SKEIN-451).
///
/// A dropdown rather than a text box, and one "that is aware of what is possible", as asked for.
/// A list written down here would be a list that goes stale the week a model ships — so it is
/// parsed out of `claude --help`, which names them:
///
/// ```text
///   --model <model>    Model for the current session. Provide
///                      an alias for the latest model (e.g.
///                      'fable', 'opus', or 'sonnet') or a
///                      model's full name (e.g.
///                      'claude-fable-5').
/// ```
///
/// Only the ALIASES, which is everything quoted before the words "full name". The full-name example
/// is an example of a form, not a model anybody should be offered: `claude-fable-5` is real today
/// and will not be forever, while `fable` is defined to mean the latest of its line. A reader who
/// wants an exact build can still type one — the setting stays free text underneath.
///
/// Pure, and separate from the spawn, so the thing that decides what a person is offered can be
/// tested without a CLI on the machine running the test. Run against the real binary on
/// 2026-08-27 (`claude --help`, version 2.1.247) it answers `["fable", "opus", "sonnet"]`.
fn parse_model_aliases(help: &str) -> Vec<String> {
    let Some(at) = help.find("--model <model>") else {
        return Vec::new();
    };
    // To the end of that flag's paragraph: the next line that introduces another flag. Without this
    // bound the scan runs into `--fallback-model`'s prose and offers whatever it happens to quote.
    let rest = &help[at..];
    let block = rest.find("\n  -").map(|end| &rest[..end]).unwrap_or(rest);
    // And stop at the full-name clause, for the reason in the doc above.
    let block = match block.find("full name") {
        Some(end) => &block[..end],
        None => block,
    };
    // **Scanned, not split on quotes.** `model's full name` puts an apostrophe in the middle of
    // the prose, so pairing quotes off in order reads `s full name (e.g. ` as a quoted token and
    // offers `s` as a model. The first two tests written here both caught it. So each candidate
    // must look like a model name in its own right — lowercase letters, digits and dashes, nothing
    // else — and a run that does not is skipped rather than shifting every pair after it.
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = block.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\'' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let Some(close) = (start..chars.len()).find(|&j| chars[j] == '\'') else {
            break;
        };
        let name: String = chars[start..close].iter().collect();
        let looks_like_a_model = !name.is_empty()
            && name.len() <= 40
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if looks_like_a_model && !out.contains(&name) {
            out.push(name);
        }
        // From the closing quote either way: an apostrophe that opened nothing must not consume
        // the quote that opens the next real name.
        i = if looks_like_a_model { close + 1 } else { i + 1 };
    }
    out
}

/// The models to offer, remembered — **read, never asked** on the caller's clock, the same rule
/// [`runtime_updates`] follows and for the same reason: this is drawn on a settings page that must
/// not wait on a process spawn.
///
/// Empty means "skein could not ask", and the page falls back to a free-text box — which is what
/// the setting has always been, so nothing is lost when this cannot answer.
pub fn model_choices() -> Vec<String> {
    let known = MODELS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if known.is_none() && !ASKING_MODELS.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(|| {
            let found = ask_model_choices();
            *MODELS.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
            ASKING_MODELS.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
    known.unwrap_or_default()
}

/// `claude --help`, and nothing else. No network, no sandbox hop: the flag's own help text is the
/// same wherever the CLI runs, and this is the one question about it that costs nothing to ask.
fn ask_model_choices() -> Vec<String> {
    let bin = claude_bin();
    let Ok(out) = std::process::Command::new(&bin).arg("--help").output() else {
        return Vec::new();
    };
    parse_model_aliases(&String::from_utf8_lossy(&out.stdout))
}

static MODELS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);
static ASKING_MODELS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The agent CLI skein spawns — `$SKEIN_CLAUDE_BIN`, else `claude` on the path.
///
/// Its own function because two things ask it now: the call itself, and [`model_choices`], which
/// asks that same binary what models it will take. A second copy of this would be a second answer
/// to "which claude", and the whole point of the override is that there is one.
pub(crate) fn claude_bin() -> String {
    env::var("SKEIN_CLAUDE_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude".into())
}

/// Which binary and which model this call will use, after both override layers.
fn binary_and_model(model: Option<&str>) -> (String, String) {
    let bin = claude_bin();
    let model = env::var("SKEIN_AI_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| model.unwrap_or("claude-haiku-4-5").to_string());
    (bin, model)
}

/// Memoize an AI result by a turn-end-scoped key, so repeat views of the same paused box don't
/// re-spend tokens; a new signal (new key) recomputes. Negatives are cached too — a flaky/None
/// answer shouldn't be retried on every poll within the same turn.
pub(crate) fn ai_cached(key: &str, compute: impl FnOnce() -> Option<String>) -> Option<String> {
    use std::sync::OnceLock;
    type Cache = std::collections::HashMap<String, Option<String>>;
    static CACHE: OnceLock<std::sync::Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(Cache::new()));
    if let Ok(m) = cache.lock() {
        if let Some(v) = m.get(key) {
            return v.clone();
        }
    }
    let v = compute();
    if let Ok(mut m) = cache.lock() {
        m.insert(key.to_string(), v.clone());
    }
    v
}

/// The text a box last reported (the blocking prompt when waiting on you, else its last message).
pub(crate) fn signal_text(sig: &SessionSignal) -> &str {
    if sig.kind == "notification" {
        &sig.prompt
    } else {
        &sig.last_message
    }
}

/// A one-line AI narration of what a box last did or is asking — the lazy fallback for the digest
/// when the box keeps no journal. One rationed Haiku call, cached per turn-end; None when AI is off
/// or unavailable (the digest then just shows commits + the raw last message). On demand only.
pub fn narrate(name: &str) -> Option<String> {
    if !valid_name(name) || !ai_enabled() {
        return None;
    }
    let sig = session_signal(name)?;
    let text = signal_text(&sig);
    if text.trim().is_empty() {
        return None;
    }
    let key = format!("narrate:{name}:{}", sig.ts);
    let prompt = format!(
        "Summarise in ONE plain sentence (max 20 words) what this autonomous coding agent just did \
         or is asking. No preamble, no quotes — just the sentence.\n\nAgent message:\n{}",
        text.chars().take(2000).collect::<String>()
    );
    ai_cached(&key, || claude_oneshot(&prompt))
}

/// Conservative AI safety check for batch-resume: given a box the heuristic tagged a trivial
/// "proceed?", ask Haiku whether it is actually a real decision the human should make. Returns
/// `Some(true)` = HOLD it back, unless Haiku affirmatively says ROUTINE — so a flaky, garbled, or
/// absent answer errs toward asking you, never toward auto-continuing. `None` means AI is off (the
/// caller then trusts the heuristic verdict). AI can only *add* a hold here, never grant a continue
/// the heuristic wouldn't already allow.
pub(crate) fn ai_says_hold(name: &str) -> Option<bool> {
    if !ai_enabled() {
        return None;
    }
    let sig = session_signal(name)?;
    let text = signal_text(&sig);
    if text.trim().is_empty() {
        return None;
    }
    let key = format!("gate:{name}:{}", sig.ts);
    let prompt = format!(
        "An autonomous coding agent ended its turn with the message below. Reply with ONE word only: \
         ROUTINE if it is merely asking permission to continue with obvious, safe next steps; or \
         DECISION if it is asking the human to make a real choice or judgement the agent should not \
         make alone.\n\nMessage:\n{}",
        text.chars().take(2000).collect::<String>()
    );
    let ans = ai_cached(&key, || claude_oneshot(&prompt))?;
    // err toward HOLD: only an explicit ROUTINE clears a box for auto-continue
    Some(!ans.to_uppercase().contains("ROUTINE"))
}

#[cfg(test)]
mod tests {

    /// **The dropdown's options come from the CLI, not from a list in here** (SKEIN-451).
    ///
    /// The picker was asked for "aware of what is possible". Anything written down in
    /// skein is a list that goes stale the week a model ships, so this parses `claude --help`. The
    /// fixture is that help text verbatim, wrapped exactly as the CLI wraps it — the wrapping is
    /// the hard part, because the aliases are split across lines and a naive line-wise scan finds
    /// none of them.
    #[test]
    fn the_models_offered_are_the_ones_this_claude_says_it_takes() {
        let help = "  --mcp-config <configs...>             Load MCP servers from a JSON file\n                    \x20 --model <model>                       Model for the current session. Provide\n                    \x20                                       an alias for the latest model (e.g.\n                    \x20                                       'fable', 'opus', or 'sonnet') or a\n                    \x20                                       model's full name (e.g.\n                    \x20                                       'claude-fable-5').\n                    \x20 -n, --name <name>                     Set a display name for this session\n";
        assert_eq!(
            super::parse_model_aliases(help),
            vec!["fable", "opus", "sonnet"],
            "the aliases the CLI names are not what would be offered"
        );
    }

    /// Two bounds, and both are load-bearing. The full-name example is a form rather than a choice
    /// — `claude-fable-5` is real today and will not be forever, while `fable` is defined to mean
    /// the latest of its line — and the scan must stop before the NEXT flag's prose, or it offers
    /// whatever that happens to quote.
    #[test]
    fn the_model_scan_stops_at_the_full_name_example_and_at_the_next_flag() {
        let help = "  --model <model>   Provide an alias (e.g. 'opus') or a model's full name \
                    (e.g. 'claude-fable-5').\n  --other <x>       takes 'yes' or 'no'\n";
        let got = super::parse_model_aliases(help);
        assert_eq!(
            got,
            vec!["opus"],
            "the scan ran past its own paragraph: {got:?}"
        );

        // And an answer it cannot read is no answer, never a guess: the page falls back to the
        // free-text box the setting has always been.
        assert!(
            super::parse_model_aliases("claude: command not found").is_empty(),
            "prose with no --model flag in it was turned into a list of models"
        );
    }

    /// SHA-256, checked against `sha256sum` rather than against itself — an implementation can
    /// agree with its own expectations and disagree with the world, and this one decides which
    /// conversation a pull request gets.
    #[test]
    fn the_hash_the_conversation_id_is_derived_from_agrees_with_sha256sum() {
        use std::io::Write;
        for subject in [
            "",
            "acme#41",
            "a much longer subject than one block of sixty-four bytes, \
                         so the padding and the second block are both exercised here",
        ] {
            let mut child = std::process::Command::new("sha256sum")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("sha256sum is on this machine");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(subject.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            let want = String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .unwrap()
                .to_string();
            assert_eq!(
                super::sha256(subject.as_bytes())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
                want,
                "skein's hash disagrees with sha256sum on {subject:?}, so the conversation id it \
                 derives is not the one it says it is"
            );
        }
    }
    use super::*;

    /// The credential every stub GitHub below is called with.
    ///
    /// Prefixed `skein-test-` deliberately: a fixture that looked like a real token
    /// (`gho_…`, `ghp_…`) is indistinguishable from one in a grep, and this tree has already had
    /// to sweep a client's real strings out of its fixtures once.
    fn fixture_token() -> crate::secret::Secret {
        crate::secret::Secret::new("skein-test-github-token")
    }

    #[allow(unused_imports)]
    use crate::testutil::*;
    #[allow(unused_imports)]
    use std::{env, fs};

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
        env::set_var("SKEIN_HOME", &home);
        env::set_var("HOME", dir.join("ambient"));
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
        for key in ["SKEIN_HOME", "HOME"] {
            env::remove_var(key);
        }
    }

    /// Each way a model call can fail says which one it was.
    ///
    /// They were one `None` and one sentence — "the model call failed or timed out" — for four
    /// problems with four different fixes. Reported as "all summarization fails", with that sentence
    /// as the whole of the evidence, for a failure that had come back in two seconds.
    #[cfg(unix)]
    #[test]
    fn a_model_call_that_fails_says_which_failure_it_was() {
        use std::os::unix::fs::PermissionsExt;
        // **The lock is for the breaker, not the environment** — this test sets no env var, and
        // that is exactly why it was left out. `REFUSED` is process-global on purpose (a broken
        // setup must be asked about once, not once per pull request), and this is the one test that
        // MANUFACTURES refusals. Clearing it before each of its own calls protects this test and
        // nobody else: a refusal left set between two cases here was read by whichever sibling was
        // between calls, which then saw `None` from a stub that would have answered fine. One run
        // in three.
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
        let dir = dir.as_ref() as &std::path::Path;
        let stub = |name: &str, body: &str| {
            let at = dir.join(name);
            fs::write(&at, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
            fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
            at.display().to_string()
        };
        let quick = Duration::from_secs(5);
        // Each case below has to actually REACH the binary. `tried` remembers a refusal about the
        // setup and answers the next call from memory — deliberately, so a host that cannot log in
        // is asked once rather than once per pull request — and that is a different property, tested
        // in `a_refusal_about_the_setup_is_only_asked_once`. Here it would mean every case after the
        // first got the first one's answer.
        let ask = |bin: &str, timeout| {
            forget_refusal();
            tried(bin, "m", "hi", timeout, Turn::Alone, None)
        };

        // Not on PATH — the commonest one by far, and the one the old message never named. Its
        // sentence has to be about the SERVER's PATH: a person reads it, checks their shell, finds
        // `claude` right there, and concludes skein is broken.
        match ask("skein-no-such-binary", quick) {
            Err(Unread::Missing { bin, .. }) => {
                assert_eq!(bin, "skein-no-such-binary");
                let said = Unread::Missing {
                    bin: bin.clone(),
                    why: "no such file".into(),
                }
                .say();
                assert!(
                    said.contains("skein-server") && said.contains("SKEIN_CLAUDE_BIN"),
                    "a missing binary must say whose PATH decides and how to override it: {said}"
                );
            }
            other => panic!("expected Missing, got {other:?}"),
        }

        // Ran and refused. Its diagnosis was being thrown away — not logged in, a model it will
        // not serve, a rate limit.
        let refused = stub("refuses", "echo 'Invalid API key' >&2; exit 3");
        match ask(&refused, quick) {
            Err(Unread::Refused { code, said }) => {
                assert_eq!(code, "3");
                assert!(
                    said.contains("Invalid API key"),
                    "stderr was dropped: {said:?}"
                );
                assert!(Unread::Refused { code, said }
                    .say()
                    .contains("Invalid API key"));
            }
            other => panic!("expected Refused, got {other:?}"),
        }

        // **And on stdout**, which is where `claude -p` actually puts it. Measured against the real
        // CLI: an unknown model exits 1 with the explanation on STDOUT and an unrelated stdin
        // warning on stderr. Reading stderr alone told a live fleet "`claude` exited 1 without
        // saying why" for a failure the CLI had explained in full — so the noisy stream must not be
        // allowed to hide the useful one.
        let talkative = stub(
            "explains-on-stdout",
            "echo \"issue with the selected model\"; echo 'Warning: no stdin data' >&2; exit 1",
        );
        match ask(&talkative, quick) {
            Err(Unread::Refused { said, .. }) => assert!(
                said.contains("issue with the selected model"),
                "the CLI explained itself on stdout and the explanation was dropped: {said:?}"
            ),
            other => panic!("expected Refused, got {other:?}"),
        }

        // Ran, succeeded, said nothing. Distinct from every other case: there is no fault to fix.
        let silent = stub("says-nothing", "exit 0");
        assert_eq!(ask(&silent, quick), Err(Unread::Silent));

        // Still going when the budget ran out — and told apart from a failed spawn by the clock
        // rather than by parsing a message.
        let slow = stub("dawdles", "sleep 30");
        let began = std::time::Instant::now();
        let out = ask(&slow, Duration::from_secs(1));
        assert_eq!(out, Err(Unread::Slow(Duration::from_secs(1))));
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the budget was not enforced"
        );

        // And the happy path still is one.
        let works = stub("answers", "echo '  a summary  '");
        assert_eq!(ask(&works, quick), Ok("a summary".to_string()));
        env::remove_var("SKEIN_HOME");
    }

    /// **The credential reaches the call, under both names.**
    ///
    /// Decided 2026-08-27: a review session gets the GitHub token, so it reads the
    /// pull request and posts its own review instead of handing an artefact back for skein to
    /// marshal. Everything that follows from that decision is worth nothing if the token does not
    /// arrive, and a session with no credential does not fail loudly — it says it could not reach
    /// GitHub and carries on, which reads exactly like a model that chose not to.
    ///
    /// Both spellings, because tools disagree: `gh` prefers `GH_TOKEN`, the Actions ecosystem
    /// writes `GITHUB_TOKEN`, and a session reaching for the other one finding nothing is
    /// indistinguishable from having none at all.
    #[test]
    fn a_review_call_carries_the_github_credential_into_the_model() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
        let dir = dir.as_ref() as &std::path::Path;
        let bin = dir.join("claude-echoing-its-credential");
        fs::write(
            &bin,
            "#!/usr/bin/env bash\nprintf '%s|%s\\n' \"${GH_TOKEN:-none}\" \"${GITHUB_TOKEN:-none}\"\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let bin = bin.display().to_string();
        let quick = Duration::from_secs(10);

        forget_refusal();
        let with = tried(&bin, "m", "hi", quick, Turn::Alone, Some(&fixture_token()));
        assert_eq!(
            with.as_deref(),
            Ok("skein-test-github-token|skein-test-github-token"),
            "the session cannot reach GitHub, so it can neither read the pull request nor post \
             what it found — and it will say so in its own words rather than failing"
        );

        // And a call given none is given none: the cheap summary ladder has nothing to do on
        // GitHub, and a credential handed to a call that does not need it is a credential in one
        // more process than it had to be.
        forget_refusal();
        let without = tried(&bin, "m", "hi", quick, Turn::Alone, None);
        assert_eq!(
            without.as_deref(),
            Ok("none|none"),
            "a call that was passed no credential picked one up from the ambient environment, so \
             which credential a model call carries is decided by however skein-server was started"
        );
        env::remove_var("SKEIN_HOME");
    }

    /// **The prompt goes on stdin, and is nowhere in the process list** (SKEIN-684).
    ///
    /// One test for two properties, because they are one change and each alone is satisfiable in a
    /// way that loses the other: a prompt written to a file and named on argv would be off the
    /// process list and still capped, and a bigger cap would raise the ceiling and leave every
    /// diff readable in `ps`.
    ///
    /// **It is spawned, not asserted about.** The fixture prints the argv it was handed — both the
    /// shell's own `"$@"` and, where the kernel offers it, `/proc/$$/cmdline`, which is the file
    /// any process on the machine can read — and copies its stdin to another file. What is checked
    /// is those two files.
    ///
    /// **600,000 bytes, and the number is the point.** Linux caps a *single* argv element at
    /// `MAX_ARG_STRLEN`, 32 pages: 524,288 on this box's 16 KiB pages (measured — `/bin/true` with
    /// one argument of 524,287 bytes runs and 524,288 is `E2BIG`, the cap counting the terminating
    /// NUL) and 131,072 on 4 KiB pages. This
    /// payload is past both, so under the old `command.arg(prompt)` the spawn could not happen at
    /// all — and reported itself as [`Unread::Missing`], "skein could not start `claude`", sending
    /// the reader to check a PATH that was fine.
    ///
    /// The argv capture is checked for the flags as well as against the marker. Without that, a
    /// fixture that wrote an empty file would satisfy "the payload is not in the argv" perfectly.
    #[cfg(unix)]
    #[test]
    fn the_prompt_travels_on_stdin_and_is_nowhere_in_the_process_list() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        env::set_var("SKEIN_HOME", &dir);
        // Pinned beside it: `$SKEIN_FLEET_ROOT` refuses to fall back to `/boxes` under test, and
        // an unpinned one is this fixture operating the live fleet.
        env::set_var("SKEIN_FLEET_ROOT", &dir);
        let dir = dir.as_ref() as &std::path::Path;
        let argv_at = dir.join("argv-it-was-handed");
        let stdin_at = dir.join("stdin-it-was-fed");
        let bin = dir.join("claude-that-reports-how-it-was-called");
        fs::write(
            &bin,
            format!(
                "#!/usr/bin/env bash\n\
                 printf '%s\\n' \"$@\" > {argv}\n\
                 if [ -r \"/proc/$$/cmdline\" ]; then tr '\\0' '\\n' < \"/proc/$$/cmdline\" >> {argv}; fi\n\
                 cat > {stdin}\n\
                 echo done\n",
                argv = argv_at.display(),
                stdin = stdin_at.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

        let marker = "SKEIN-684-PAYLOAD-MARKER";
        let prompt = format!("{marker} ").repeat(24_000);
        assert!(
            prompt.len() > 524_288,
            "the payload has shrunk under MAX_ARG_STRLEN on a 16 KiB-page machine, so this test \
             would pass with the prompt back on argv: {} bytes",
            prompt.len()
        );

        forget_refusal();
        let said = tried(
            &bin.display().to_string(),
            "m",
            &prompt,
            Duration::from_secs(30),
            Turn::Alone,
            None,
        );
        assert_eq!(
            said.as_deref(),
            Ok("done"),
            "a prompt past MAX_ARG_STRLEN never reached the program — which is the whole defect, \
             reported as a binary that could not be started"
        );

        let handed = fs::read_to_string(&argv_at).unwrap();
        assert!(
            handed.contains("-p") && handed.contains("--model"),
            "the fixture captured no argv at all, so the assertion below would hold however the \
             prompt was sent: {handed:?}"
        );
        assert!(
            !handed.contains(marker),
            "the prompt is on argv, so every diff skein reads — private repositories included — \
             is in /proc/<pid>/cmdline for as long as the call runs"
        );
        assert_eq!(
            fs::read_to_string(&stdin_at).unwrap(),
            prompt,
            "the model was not handed the prompt, or not all of it"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **A prompt too big to send is refused in a sentence, not spent as a failed spawn.**
    ///
    /// The refusal has to name both numbers: "too large" leaves the reader unable to tell a prompt
    /// that missed by a hundred bytes from one that missed by four times, and those want different
    /// answers from them.
    ///
    /// **And the boundary is asserted from both sides.** A ceiling only refuses if it also lets
    /// things through: shrink [`PROMPT_CEILING`] and the second half of this test fails, which is
    /// what stops the refusal being bought by refusing everything.
    #[cfg(unix)]
    #[test]
    fn a_prompt_bigger_than_skein_will_send_is_refused_with_its_size_and_the_limit() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_FLEET_ROOT", &dir);
        let dir = dir.as_ref() as &std::path::Path;
        let ran_at = dir.join("it-was-spawned");
        let bin = dir.join("claude-that-records-being-run");
        fs::write(
            &bin,
            format!(
                "#!/usr/bin/env bash\ntouch {ran}\ncat > /dev/null\necho done\n",
                ran = ran_at.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let bin = bin.display().to_string();

        let over = "x".repeat(PROMPT_CEILING + 1);
        forget_refusal();
        let refused = tried(&bin, "m", &over, Duration::from_secs(30), Turn::Alone, None);
        assert_eq!(
            refused,
            Err(Unread::TooLarge {
                bytes: PROMPT_CEILING + 1,
                limit: PROMPT_CEILING,
            }),
            "a prompt over the ceiling was sent anyway, or was refused as something else"
        );
        assert!(
            !ran_at.exists(),
            "the binary was spawned with a prompt skein had already decided not to send"
        );
        let said = refused.unwrap_err().say();
        assert!(
            said.contains(&(PROMPT_CEILING + 1).to_string())
                && said.contains(&PROMPT_CEILING.to_string()),
            "the refusal names neither the size nor the limit, so the reader cannot tell whether \
             this missed by a hundred bytes or by four times: {said}"
        );

        // And exactly at the ceiling it goes through. Without this, a ceiling of one byte would
        // pass every assertion above.
        forget_refusal();
        let at_the_line = "x".repeat(PROMPT_CEILING);
        assert_eq!(
            tried(
                &bin,
                "m",
                &at_the_line,
                Duration::from_secs(30),
                Turn::Alone,
                None
            )
            .as_deref(),
            Ok("done"),
            "a prompt exactly at the ceiling was refused, so the limit is off by one — or has been \
             shrunk under what skein actually sends"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_FLEET_ROOT");
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
        env::set_var("SKEIN_HOME", home);
        env::set_var("SKEIN_AI", "on");
        env::set_var("HOME", home);
        forget_refusal();

        // Counts how many times it is actually run.
        let ran = home.join("ran");
        let stub = |body: &str| {
            let at = home.join("claude");
            fs::write(
                &at,
                format!("#!/usr/bin/env bash\necho x >> {}\n{body}\n", ran.display()),
            )
            .unwrap();
            fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
            env::set_var("SKEIN_CLAUDE_BIN", &at);
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

        for key in ["SKEIN_HOME", "SKEIN_AI", "SKEIN_CLAUDE_BIN", "HOME"] {
            env::remove_var(key);
        }
        forget_refusal();
    }

    /// **The ladder and the pin, driven end to end through a real spawn** (SKEIN-376).
    ///
    /// The stub answers the way the installed CLI does — measured 2026-08-26: `--resume` on an id
    /// it does not hold exits 1 saying `No conversation found with session ID: <id>`, while
    /// `--session-id` opens one and answers. So a pull request read for the first time must climb
    /// from resume to open by itself, without skein keeping a note of which rounds it has done.
    ///
    /// It also records the directory each attempt ran in, which is the half that would otherwise
    /// ship broken in silence: a conversation is filed under the working directory, so a call that
    /// does not pin one opens its session wherever the server was started and every later resume
    /// looks somewhere else, finds nothing, and pays for the whole diff again. Nothing fails, so
    /// only an assertion catches it.
    #[test]
    fn a_first_reading_opens_the_conversation_a_later_one_resumes_and_both_run_where_it_is_filed() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        env::set_var("SKEIN_HOME", home);
        env::set_var("SKEIN_AI", "on");
        env::set_var("HOME", home);

        let log = home.join("attempts");
        let at = home.join("filed-here");
        fs::create_dir_all(&at).unwrap();
        let bin = home.join("claude");
        // **The stub SCANS its arguments rather than counting them** — SKEIN-396's lesson, learned
        // the expensive way: five fixtures pinned to `"$4"` all broke silently the day a flag was
        // added before the prompt, and each reported a different innocent failure. `$PWD` rather
        // than `pwd` because what is asserted is the directory the CHILD was started in.
        fs::write(
            &bin,
            format!(
                "#!/usr/bin/env bash\n\
                 flag=none\n\
                 for a in \"$@\"; do\n\
                 \x20 case \"$a\" in --resume|--session-id) flag=$a;; esac\n\
                 done\n\
                 printf '%s %s\\n' \"$flag\" \"$PWD\" >> {log}\n\
                 if [ \"$flag\" = --resume ]; then\n\
                 \x20 echo 'No conversation found with session ID' >&2; exit 1\n\
                 fi\n\
                 printf 'the answer'\n",
                log = log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        env::set_var("SKEIN_CLAUDE_BIN", &bin);

        let said = claude_in_conversation(
            "read this",
            None,
            Duration::from_secs(30),
            "the-id",
            &at,
            None,
            Machine::Wherever,
        );
        assert_eq!(
            said.as_deref(),
            Ok("the answer"),
            "a pull request skein has never read got no reading at all — the resume it tries first \
             was treated as the answer instead of as the question it is"
        );

        let tried = fs::read_to_string(&log).unwrap_or_default();
        let steps: Vec<&str> = tried.lines().collect();
        assert_eq!(
            steps.len(),
            2,
            "the ladder did not climb: skein asked {} times, so a first reading either never tries \
             to resume (and every round is a cold read) or never opens one (and there is nothing \
             for the next round to resume). Attempts: {tried:?}",
            steps.len()
        );
        assert!(
            steps[0].starts_with("--resume ") && steps[1].starts_with("--session-id "),
            "the order is wrong — skein must ASK whether the conversation is still there rather \
             than assume, or a pull request read before pays for its whole diff again: {tried:?}"
        );
        let where_it_ran = at.canonicalize().unwrap();
        for step in &steps {
            let ran_in = std::path::PathBuf::from(step.split_once(' ').unwrap().1)
                .canonicalize()
                .unwrap();
            assert_eq!(
                ran_in, where_it_ran,
                "the call ran in the wrong directory, so its conversation is filed where no later \
                 round will look for it — every resume misses and the whole feature does nothing \
                 while appearing to work: {tried:?}"
            );
        }

        // **And the failed resume is not remembered as a standing refusal.** It is an ordinary
        // answer to an ordinary question; remembering it would make one forgotten session refuse
        // every model call skein makes until the memo aged out.
        assert!(
            standing_refusal().is_none(),
            "a session the sandbox no longer has was recorded as skein being unable to read at \
             all, so the next pull request is refused before it is tried"
        );

        for key in ["SKEIN_HOME", "SKEIN_AI", "SKEIN_CLAUDE_BIN", "HOME"] {
            env::remove_var(key);
        }
        forget_refusal();
    }

    /// A model call uses the login skein holds when the one it inherits will not do.
    ///
    /// The bug: `claude` reads its credential from `$HOME/.claude/.credentials.json` and nowhere
    /// else, and this module spawned it with no environment — so it used whatever HOME the server
    /// was started with. A live fleet answered `Not logged in · Please run /login` for every single
    /// summary while its own health report said `logins: ["claude"]`, because skein's copy was under
    /// `fleet-home` and the call was looking somewhere else.
    ///
    /// Asserted on the HOME the child actually receives, via a stub that prints it — the property is
    /// which credential the call reads, and nothing short of the spawned environment shows that.
    #[cfg(unix)]
    #[test]
    fn a_model_call_falls_back_to_the_login_skein_keeps() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        // Shared with every other test in this module, and a panic skips the cleanup at the end:
        // clear the remembered refusal on the way IN. Without it a sibling's failure makes this
        // one's stub never run, and only in a parallel run.
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        env::set_var("SKEIN_HOME", home);
        env::set_var("SKEIN_AI", "on");

        // A stub `claude` that answers with the HOME it was given.
        let bin = home.join("claude");
        fs::write(&bin, "#!/usr/bin/env bash\nprintf '%s' \"$HOME\"\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        env::set_var("SKEIN_CLAUDE_BIN", &bin);

        // An ambient HOME with no credential in it — the state a live fleet's server was in.
        let bare = home.join("bare");
        fs::create_dir_all(&bare).unwrap();
        env::set_var("HOME", &bare);

        // Nothing anywhere yet: the call is left alone, so a host that works another way is not
        // moved off whatever it was doing.
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some(bare.to_string_lossy().as_ref()),
            "skein redirected a call while it had no login of its own to redirect it to"
        );

        // Now skein has one. `refreshTokenExpiresAt` far in the future — that field and NOT
        // `expiresAt`, which is the access token and expires hourly on a perfectly good login.
        let fleet_home = home.join("fleet-home/.claude");
        fs::create_dir_all(&fleet_home).unwrap();
        fs::write(
            fleet_home.join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#,
        )
        .unwrap();
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some(home.join("fleet-home").to_string_lossy().as_ref()),
            "the call still read a HOME with no credential while skein was holding one"
        );

        // And when BOTH carry a usable login, the fleet's wins. This assertion used to say the
        // opposite — an ambient login was left alone so a host that already worked was not moved —
        // and the cost of that was named on a live fleet: skein was reading pull requests
        // on one credential while every box worked on another, so a logout showed up in one place
        // and not the other, and the cockpit's banner (which watches `fleet-home`) described a
        // credential these calls never touched. One login, one logout, one fix, seen everywhere.
        let mine = bare.join(".claude");
        fs::create_dir_all(&mine).unwrap();
        fs::write(
            mine.join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#,
        )
        .unwrap();
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some(home.join("fleet-home").to_string_lossy().as_ref()),
            "the call read the ambient credential while skein held the one every box uses"
        );

        // A refresh token that has died is not a login. This is the one expiry that means anything:
        // `expiresAt` in the past is the ordinary state of a good credential.
        fs::write(
            mine.join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":1}}"#,
        )
        .unwrap();
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some(home.join("fleet-home").to_string_lossy().as_ref()),
            "a dead refresh token was treated as a login"
        );

        // The fallback still holds in the direction that matters: skein's own credential dead and
        // the ambient one alive means the ambient one is used. `login_home()` answers only for a
        // refreshable credential, so preferring the fleet's can never mean preferring a dead one.
        fs::write(
            mine.join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#,
        )
        .unwrap();
        fs::write(
            fleet_home.join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":1}}"#,
        )
        .unwrap();
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some(bare.to_string_lossy().as_ref()),
            "skein preferred its own dead credential over the live one in front of it"
        );

        for key in ["SKEIN_HOME", "SKEIN_AI", "SKEIN_CLAUDE_BIN", "HOME"] {
            env::remove_var(key);
        }
    }

    /// Every sentence skein shows for an unread call arrives as one line, with no blank runs in it.
    ///
    /// Observed in `skein doctor`, and on the board:
    ///
    /// ```text
    /// ✗ model  … It is on the PATH of the process running                  skein-server that
    /// matters
    /// ```
    ///
    /// Eighteen spaces mid-sentence. A Rust string literal that spans lines keeps the newline AND
    /// every space of the source indentation unless the line ends with `\` — and a literal that has
    /// been reflowed by an editor keeps them on ONE line, which is what happened here. The defect
    /// is invisible where it is written: the source looks like a neatly wrapped paragraph.
    ///
    /// So it is asserted on the rendered sentence, which is the only place it can be seen. These
    /// are one-line messages by contract — they land in a row on a board — so a newline in one is
    /// the same defect wearing its original shape.
    #[test]
    fn a_failure_message_arrives_as_one_line() {
        let every = [
            Unread::Missing {
                bin: "claude".into(),
                why: "not on PATH".into(),
            },
            Unread::Unreachable {
                sandbox: "skein-fleet".into(),
                why: "`sbx` is not on this process's PATH".into(),
            },
            Unread::AbsentInSandbox {
                bin: "claude".into(),
                sandbox: "skein-fleet".into(),
            },
            Unread::Refused {
                code: "1".into(),
                said: String::new(),
            },
            Unread::Refused {
                code: "1".into(),
                said: "Invalid API key".into(),
            },
            Unread::Slow(Duration::from_secs(30)),
            Unread::TooLarge {
                bytes: PROMPT_CEILING + 1,
                limit: PROMPT_CEILING,
            },
            Unread::Silent,
        ];
        // The compiler keeps this list honest: a new variant stops this match compiling, and the
        // count below stops somebody adding an arm without adding a sentence to check.
        let tag = |u: &Unread| match u {
            Unread::Missing { .. } => 0,
            Unread::Unreachable { .. } => 1,
            Unread::AbsentInSandbox { .. } => 2,
            Unread::Refused { .. } => 3,
            Unread::Slow(_) => 4,
            Unread::TooLarge { .. } => 5,
            Unread::Silent => 6,
        };
        let mut seen: Vec<usize> = every.iter().map(tag).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen,
            [0, 1, 2, 3, 4, 5, 6],
            "a variant has no sentence checked here"
        );

        for unread in &every {
            let said = unread.say();
            assert!(
                !said.contains('\n'),
                "a message that lands in a row on a board arrived as more than one row: {said:?}"
            );
            assert!(
                !said.contains("  "),
                "source indentation was carried into the sentence a person reads: {said:?}"
            );
        }
    }

    /// A failed reading gives the row a sentence; the server's search path stays in the diagnostic.
    ///
    /// **SKEIN-384**, seen while driving the review pane. A row whose reading failed because `sbx`
    /// was not on the server's PATH rendered this, in full, in the queue:
    ///
    /// ```text
    /// not read — skein could not reach the fleet sandbox `skein-fleet`, so the model was never
    /// asked: `sbx` is not on this process's PATH (/home/agent/.local/bin:/usr/local/share/
    /// npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/home/agent/
    /// .claude/plugins/cache/sync/sync/1.2.3/bin). A shell you start by hand may well find it —
    /// what matters is the PATH the server was started with. — …
    /// ```
    ///
    /// Longer than the pull request row it was attached to, and the page draws the same string
    /// twice on an open row (`src/web/index.html:5511` for the gist, `:5895` for the body). **The
    /// diagnosis is not the defect** — it is genuinely the fact a reader cannot look up afterwards,
    /// because by the time they check they are checking their shell's PATH. Where it is put is the
    /// defect, so it moves to [`Unread::detail`] and `skein doctor` prints it
    /// (`src/bin/skein.rs`), and [`Unread::say`] names the move that reaches it.
    ///
    /// The `why` here is **derived** — produced by skein's own spawn failure against a PATH this
    /// test really sets — so it cannot go on passing against a message the code no longer writes.
    #[test]
    fn a_failed_reading_tells_the_reader_what_broke_not_where_the_server_looked() {
        let _guard = crate::testutil::env_lock();
        let real_path = env::var("PATH").unwrap_or_default();
        // A real PATH from the report, near enough: nine entries, ~180 characters.
        env::set_var(
            "PATH",
            "/home/agent/.local/bin:/usr/local/share/npm-global/bin:/usr/local/sbin:\
             /usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/home/agent/.claude/plugins/cache/\
             sync/sync/1.2.3/bin",
        );
        let dump = env::var("PATH").unwrap();
        let why =
            crate::util::output_with_timeout_why(&mut Command::new("sbx"), Duration::from_secs(5))
                .expect_err("`sbx` was found on a PATH built not to contain it");
        assert!(
            why.contains(&dump),
            "the transport no longer carries the search path, so this test is no longer about the \
             message it was written against: {why}"
        );

        for unread in [
            Unread::Unreachable {
                sandbox: "skein-fleet".into(),
                why: why.clone(),
            },
            Unread::Missing {
                bin: "sbx".into(),
                why: why.clone(),
            },
        ] {
            let row = unread.say();
            assert!(
                !row.contains(&dump),
                "the queue row is printing the server's whole search path beside a pull request \
                 title instead of saying what failed — {row}"
            );
            // The row is a scanning surface, and the page draws this string twice on an open row.
            // The number is the observed defect's own: the search path ALONE was longer than this.
            assert!(
                row.chars().count() < 300,
                "a failed reading is still an environment dump in the queue at {} characters — {row}",
                row.chars().count()
            );
            // §law 1 — never a statement without the move it implies. The dump was moved out; the
            // cure must not have gone with it.
            let move_implied: &[&str] = match unread {
                Unread::Unreachable { .. } => &["skein-fleet", "sbx ls", "skein doctor"],
                _ => &["SKEIN_CLAUDE_BIN", "skein-server"],
            };
            for expected in move_implied {
                assert!(
                    row.contains(expected),
                    "the row tells the reader their reading failed and nothing they can do about \
                     it — `{expected}` is missing from: {row}"
                );
            }
            // And nothing was destroyed on the way: the one fact the reader cannot look up later
            // is still reachable, in the place the sentence sends them to.
            let detail = unread
                .detail()
                .expect("the search path was dropped, not moved — nowhere left to read it");
            assert!(
                detail.contains(&dump),
                "`skein doctor` can no longer show the PATH skein actually had, which is the only \
                 reason the dump was worth keeping: {detail}"
            );
        }

        // **The reason and the offer are not one string.** With nothing at all to say about why,
        // the row must still name the failure and still carry the move — the failure mode where a
        // silent transport leaves a reader a sentence with a hole in it.
        let mute = Unread::Unreachable {
            sandbox: "skein-fleet".into(),
            why: String::new(),
        }
        .say();
        for expected in ["skein-fleet", "was never asked", "sbx ls"] {
            assert!(
                mute.contains(expected),
                "a transport that failed without a word left the row unable to say what happened \
                 or what to do — `{expected}` is missing from: {mute}"
            );
        }
        assert!(
            !mute.contains(": .") && !mute.contains("()"),
            "the row is offering the reader an empty diagnosis where a reason should be: {mute}"
        );
        assert_eq!(
            Unread::Unreachable {
                sandbox: "skein-fleet".into(),
                why: String::new(),
            }
            .detail(),
            None,
            "`skein doctor` is promising a diagnosis that does not exist"
        );

        env::set_var("PATH", real_path);
    }

    /// A model call runs on the login skein manages, not on a key it inherited.
    ///
    /// Reported live, the moment the temp-directory fix let the call run at all:
    ///
    /// ```text
    /// `claude` exited 1: Invalid API key · Fix external API key / ⚠ claude.ai connectors are
    /// disabled because ANTHROPIC_API_KEY or another auth source is set and takes precedence over
    /// your claude.ai login
    /// ```
    ///
    /// The third answer the ambient environment was giving on skein's behalf, after which HOME and
    /// which temp directory. Measured against the real CLI, and worse than the message says: a
    /// stale key made it HANG until the budget ran out, so the same misconfiguration also reads as
    /// "`claude` was still going after 30s" — which sends the reader to look at diff sizes.
    ///
    /// The second half of this test is the half that matters: somebody whose only credential is a
    /// key must keep it. "Prefer the login skein manages" is not "refuse keys".
    #[cfg(unix)]
    #[test]
    fn a_model_call_runs_on_the_login_not_an_inherited_key() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        env::set_var("SKEIN_HOME", home);
        env::set_var("SKEIN_AI", "on");

        // A stub that answers with the auth source it was handed.
        let bin = home.join("claude");
        fs::write(
            &bin,
            "#!/usr/bin/env bash\nprintf '%s' \"${ANTHROPIC_API_KEY:-none}\"\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        env::set_var("SKEIN_CLAUDE_BIN", &bin);
        env::set_var("ANTHROPIC_API_KEY", "sk-ant-stale");

        // A HOME carrying a login: the key is removed, and the call runs on the subscription.
        let mine = home.join("mine");
        fs::create_dir_all(mine.join(".claude")).unwrap();
        fs::write(
            mine.join(".claude/.credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#,
        )
        .unwrap();
        env::set_var("HOME", &mine);
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some("none"),
            "an inherited API key outranked the login skein manages"
        );

        // No login anywhere — the key is the only credential there is, and removing it would leave
        // the call with nothing. skein prefers its own login; it does not refuse keys.
        let bare = home.join("bare");
        fs::create_dir_all(&bare).unwrap();
        env::set_var("HOME", &bare);
        forget_refusal();
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some("sk-ant-stale"),
            "skein took away the only credential the call had"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_AI",
            "SKEIN_CLAUDE_BIN",
            "HOME",
            "ANTHROPIC_API_KEY",
        ] {
            env::remove_var(key);
        }
        forget_refusal();
    }

    /// A failure names the program that failed, not the one it was carrying.
    ///
    /// Observed on a host with no `sbx`, from `skein doctor`:
    ///
    /// ```text
    /// ✗ model  skein could not start `claude` (sbx exec failed to start or exceeded the 30s
    /// timeout) … or set SKEIN_CLAUDE_BIN to its full path.
    /// ```
    ///
    /// Nothing was wrong with `claude` and `SKEIN_CLAUDE_BIN` was not the cure. A transport failure
    /// came back wearing the payload's name — and sent the reader to check a binary that was fine.
    /// This is the line a person reads when they are already confused, so it is the worst possible
    /// place to guess.
    ///
    /// **Driven into a review box**, which is the one crossing a model call still makes: skein runs
    /// inside the fleet sandbox now, so a call to `Machine::Wherever` is a local process with no
    /// transport to fail (SKEIN-576). The crossing is stood in for through the execution seam
    /// rather than by a fake `sbx` on `$PATH` — there is no `sbx` to fake, and a fixture that put
    /// one there would be bypassed while the real command ran (SKEIN-592).
    ///
    /// Four failures, four answers, and the test exists because the first three were one.
    #[cfg(unix)]
    #[test]
    fn a_failure_names_the_program_that_failed() {
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        env::set_var("SKEIN_HOME", home);
        env::set_var("SKEIN_AI", "on");
        env::remove_var("SKEIN_CLAUDE_BIN"); // or the call never crosses at all
        fs::write(
            home.join("config.json"),
            br#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();
        // The box has to be placed to be reached — that is what `model_call_in_box` addresses.
        crate::testutil::placed("review-box");

        let ask = |timeout: Duration| {
            forget_refusal();
            claude_in_turn(
                "hi",
                None,
                timeout,
                Turn::Alone,
                None,
                Machine::Box("review-box"),
            )
        };
        // What a fleet-scope crossing runs, said by this process and by nothing else.
        let stand_in = |argv: Vec<&'static str>| {
            crate::place::seam::install(Box::new(move |_: &[String]| {
                Some(argv.iter().map(|a| a.to_string()).collect())
            }))
        };

        // The crossing itself could not be started — the shape of the original bug, where the
        // transport was missing and `claude` was blamed for it.
        //
        // **Asserted on `model_call_in_box` rather than through `claude_in_turn`, deliberately.**
        // A box that cannot be reached is not an error to the caller: it falls back to the reading
        // every reading had before review boxes existed, which means a test that drove this arm
        // through `claude_in_turn` would go on to spawn the real `claude` on the machine running
        // the suite — and this one did, once, before that was noticed. The message is produced
        // here, so this is where it is read.
        {
            let _at = stand_in(vec!["skein-no-such-transport"]);
            let why = crate::fleet::model_call_in_box(
                "review-box",
                "claude",
                "sonnet",
                "hi",
                Duration::from_secs(5),
                vec![],
                None,
            )
            .expect_err("a crossing that cannot start is not a reading");
            // The transport's own words, and they have to be worth carrying. `bounded_output` says
            // "failed to start or exceeded the 30s timeout" for both failures — offering a timeout
            // skein has ALREADY ruled out by the clock, and dropping the one fact the reader cannot
            // recover later: the PATH the server actually had. By the time they go and look, they
            // are looking at their shell's.
            assert!(
                why.contains("skein-no-such-transport") && why.contains("PATH"),
                "the transport did not say what failed or where it looked: {why}"
            );
            assert!(
                !why.contains("or exceeded"),
                "skein ruled out the timeout by the clock and then offered it anyway: {why}"
            );
            // And it does not send the reader after the payload. `SKEIN_CLAUDE_BIN` was the cure
            // offered for this exact failure on a machine where `claude` was fine — which is the
            // whole of what this test is named for. Asserted on that advice rather than on the
            // word "claude": this process's own PATH has a `.claude` directory on it, so the
            // looser check passes or fails on where the suite happens to be running.
            assert!(
                !why.contains("SKEIN_CLAUDE_BIN"),
                "the reader was sent to fix the payload for a failure it was not part of: {why}"
            );
        }

        // The far side answered, and `claude` is not in it. `claude` never ran, so its exit code is
        // not skein's to report — and the server's PATH, which `Missing` sends you to check, has
        // nothing to do with a binary inside a box.
        //
        // The marker is what the real script prints the moment a shell on the far side runs it.
        // These stand-ins never run the script they are handed, so they print it themselves to
        // stand for one that did — without it they are indistinguishable from a crossing that
        // failed before the payload started, which is exactly the distinction the case below
        // turns on.
        {
            let _at = stand_in(vec![
                "sh",
                "-c",
                "echo SKEIN_IN_SANDBOX >&2; echo 'bash: line 2: claude: command not found' >&2; \
                 exit 127",
            ]);
            match ask(Duration::from_secs(5)) {
                Err(Unread::AbsentInSandbox { bin, sandbox }) => {
                    assert_eq!((bin.as_str(), sandbox.as_str()), ("claude", "skein-fleet"));
                    let said = Unread::AbsentInSandbox { bin, sandbox }.say();
                    assert!(
                        said.contains("claude") && said.contains("skein-fleet"),
                        "the reader is not told what is missing or where: {said}"
                    );
                }
                other => panic!("expected AbsentInSandbox, got {other:?}"),
            }
        }

        // The crossing ran and failed on its own account, which is what a box that is not running
        // looks like: non-zero, with its own words, and the script never started. Told apart by
        // evidence rather than by the exit code — 1 means whatever the program that exited chose it
        // to mean — because this is the arm that fires most often, and it was being reported to a
        // person as `claude` refusing.
        {
            let _at = stand_in(vec![
                "sh",
                "-c",
                "echo 'that box is not running' >&2; exit 1",
            ]);
            match ask(Duration::from_secs(5)) {
                Err(Unread::Unreachable { sandbox, why }) => {
                    assert_eq!(sandbox, "skein-fleet");
                    assert!(
                        why.contains("that box is not running"),
                        "the transport's own words were dropped: {why}"
                    );
                    let said = Unread::Unreachable { sandbox, why }.say();
                    assert!(
                        !said.contains("claude"),
                        "a transport failure was reported under the model's name: {said}"
                    );
                }
                other => panic!(
                    "a crossing that failed before the model ran was reported as the model \
                     failing: {other:?}"
                ),
            }
        }

        // And a CLI that ran and refused still reports its own diagnosis, unchanged — the point of
        // separating the first three is that this one keeps meaning what it says.
        {
            let _at = stand_in(vec![
                "sh",
                "-c",
                "echo SKEIN_IN_SANDBOX >&2; echo 'Invalid API key'; exit 1",
            ]);
            match ask(Duration::from_secs(5)) {
                Err(Unread::Refused { code, said }) => {
                    assert_eq!(code, "1");
                    assert!(
                        said.contains("Invalid API key"),
                        "the diagnosis was lost: {said}"
                    );
                    assert!(
                        !said.contains(crate::fleet::REACHED),
                        "skein's own bookkeeping was shown to a person: {said}"
                    );
                }
                other => panic!("expected Refused, got {other:?}"),
            }
        }

        crate::place::forget_place("review-box");
        for key in ["SKEIN_HOME", "SKEIN_AI"] {
            env::remove_var(key);
        }
        forget_refusal();
    }

    /// A model call brings its own temp directory, and it is under the HOME it is already using.
    ///
    /// The bug, reported live with every review summary failing:
    ///
    /// ```text
    /// `claude` exited 1: Temp directory /tmp/claude-1000 is owned by uid 0, expected 1000.
    /// Refusing to use it — another user may have pre-created it.
    /// ```
    ///
    /// The CLI derives its scratch from the shared `/tmp` and refuses to start when that path is
    /// owned by somebody else — deliberately, against a planted directory. Skein had already taken
    /// charge of WHICH HOME the call reads its credential from and left the temp directory to
    /// whoever ran first, which in a sandbox is always something. Verified against the real CLI:
    /// poison the derived path and it refuses; set this and the same call answers.
    ///
    /// Asserted on the environment the child actually receives, because nothing shorter shows it.
    #[cfg(unix)]
    #[test]
    fn a_model_call_brings_its_own_temp_directory() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        env::set_var("SKEIN_HOME", home);
        env::set_var("SKEIN_AI", "on");

        // A stub `claude` that answers with the scratch directory it was handed.
        let bin = home.join("claude");
        fs::write(
            &bin,
            "#!/usr/bin/env bash\nprintf '%s' \"${CLAUDE_CODE_TMPDIR:-the shared /tmp}\"\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        env::set_var("SKEIN_CLAUDE_BIN", &bin);

        let live = br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#;

        // An ambient HOME that carries a login: the call stays on it, and so does its scratch.
        let mine = home.join("mine");
        fs::create_dir_all(mine.join(".claude")).unwrap();
        fs::write(mine.join(".claude/.credentials.json"), live).unwrap();
        env::set_var("HOME", &mine);
        assert_eq!(
            claude_oneshot("hi"),
            Some(crate::fleet::model_scratch_dir(&mine).display().to_string()),
            "the call wrote its scratch into a directory skein does not own"
        );

        // And when the call moves to the login skein keeps, the scratch moves with it — a temp
        // directory under a HOME the call is no longer using is the same bug wearing a hat.
        let bare = home.join("bare");
        fs::create_dir_all(&bare).unwrap();
        env::set_var("HOME", &bare);
        let fleet_home = home.join("fleet-home");
        fs::create_dir_all(fleet_home.join(".claude")).unwrap();
        fs::write(fleet_home.join(".claude/.credentials.json"), live).unwrap();
        assert_eq!(
            claude_oneshot("hi"),
            Some(
                crate::fleet::model_scratch_dir(&fleet_home)
                    .display()
                    .to_string()
            ),
            "the credential moved and the scratch directory did not"
        );

        for key in ["SKEIN_HOME", "SKEIN_AI", "SKEIN_CLAUDE_BIN", "HOME"] {
            env::remove_var(key);
        }
        forget_refusal();
    }

    /// The model check runs the call it reports on, not a cheaper one.
    ///
    /// `model_reachable` asked `claude --version`, because a missing binary is the commonest
    /// failure and that question costs nothing. Measured against the real CLI: inside a sandbox
    /// whose temp directory it refuses to use, `--version` prints `2.1.221 (Claude Code)` and exits
    /// 0 while `-p` exits 1. So `skein doctor` — the one place a person goes to ask why — reported
    /// the model fine on a fleet where every summary was failing.
    ///
    /// The stub here is exactly that host: fine when asked its version, refusing when asked a
    /// question.
    #[cfg(unix)]
    #[test]
    fn the_model_check_runs_the_call_it_reports_on() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        env::set_var("SKEIN_HOME", home);
        env::set_var("HOME", home);

        let bin = home.join("claude");
        let stub = |body: &str| {
            fs::write(&bin, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            env::set_var("SKEIN_CLAUDE_BIN", &bin);
        };

        stub(
            "case \"$1\" in --version) echo '2.1.221 (Claude Code)'; exit 0 ;; esac\n\
             echo 'Temp directory /tmp/claude-1000 is owned by uid 0, expected 1000.'; exit 1",
        );
        match model_reachable() {
            Err(Unread::Refused { said, .. }) => assert!(
                said.contains("Temp directory"),
                "the refusal was reported without its reason: {said}"
            ),
            other => panic!(
                "a host that answers --version and refuses -p was reported as working: {other:?}"
            ),
        }

        // A host where the real call works still reports working.
        stub("printf ok");
        assert_eq!(model_reachable(), Ok(()));

        // And the diagnostic leaves no verdict behind for the next real call to inherit.
        stub("echo 'Invalid API key' >&2; exit 3");
        assert!(model_reachable().is_err());
        stub("printf ok");
        assert_eq!(
            tried(
                &bin.display().to_string(),
                "m",
                "hi",
                Duration::from_secs(5),
                Turn::Alone,
                None
            ),
            Ok("ok".to_string()),
            "a refusal found by `doctor` was remembered and answered the next real call"
        );

        for key in ["SKEIN_HOME", "SKEIN_CLAUDE_BIN", "HOME"] {
            env::remove_var(key);
        }
        forget_refusal();
    }

    /// The health report asks about both switches, not one of them.
    ///
    /// `review_summaries` defaults ON and `ai_enrichment` defaults off, so a report that consulted
    /// only the second said "off" on the common configuration — while every review summary on that
    /// fleet was failing. The one place somebody would look, saying the feature was not in use.
    #[test]
    fn what_wants_the_model_names_every_switch_that_does() {
        let _g = crate::testutil::env_lock();
        // Shared with every other test in this module, and a panic skips the cleanup at the end:
        // clear the remembered refusal on the way IN. Without it a sibling's failure makes this
        // one's stub never run, and only in a parallel run.
        forget_refusal();
        // No deployment override here: this reads switches and calls no model. Setting one would be
        // a variable nothing in this test depends on, left behind for whichever test ran next —
        // which is what it was, and what broke `board`'s cover test in a parallel run.
        env::set_var("SKEIN_AI", "off");
        env::set_var("SKEIN_REVIEW_AI", "on");
        assert_eq!(
            model_wanted(),
            vec!["review summaries"],
            "review summaries are on and the report does not mention them — which is exactly the \
             fleet that reported every summary failing while health said `ai: off`"
        );

        env::set_var("SKEIN_AI", "on");
        assert_eq!(model_wanted(), vec!["box summaries", "review summaries"]);

        env::set_var("SKEIN_REVIEW_AI", "off");
        assert_eq!(model_wanted(), vec!["box summaries"]);

        env::set_var("SKEIN_AI", "off");
        assert!(model_wanted().is_empty());
        env::remove_var("SKEIN_AI");
        env::remove_var("SKEIN_REVIEW_AI");
    }

    #[test]
    #[cfg(unix)]
    fn narrate_uses_stubbed_claude_and_respects_kill_switch() {
        if Command::new("sh").arg("-c").arg("true").output().is_err() {
            return;
        }
        let _g = env_lock();
        // Shared with every other test in this module, and a panic skips the cleanup at the end:
        // clear the remembered refusal on the way IN. Without it a sibling's failure makes this
        // one's stub never run, and only in a parallel run.
        forget_refusal();
        let dir = tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        env::set_var("SKEIN_HOME", &dir);
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            r#"{"thing-n":{"branch":"x","dir":"/d","lastSeen":"","status":"waiting"}}"#,
        )
        .unwrap();
        write_session(&dir, "thing-n", "Refactored the parser module today.");
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_CLAUDE_BIN", write_claude_stub(&dir));

        env::remove_var("SKEIN_AI"); // kill switch: off → no spend, None
        assert_eq!(narrate("thing-n"), None);

        env::set_var("SKEIN_AI", "on");
        assert_eq!(
            narrate("thing-n").as_deref(),
            Some("It wired up the parser.")
        );

        env::remove_var("SKEIN_AI");
        env::remove_var("SKEIN_CLAUDE_BIN");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
    }
}
