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
                "skein could not start `{bin}` ({why}). It is on the PATH of the process running \
                 skein-server that matters, not your shell's — start the server from a shell \
                 that has it, or set SKEIN_CLAUDE_BIN to its full path."
            ),
            // Leads with the sandbox, because the reader's next move is `sbx ls` and not anything
            // to do with the model. The `why` carries the PATH skein actually had.
            Unread::Unreachable { sandbox, why } => format!(
                "skein could not reach the fleet sandbox `{sandbox}`, so the model was never asked: \
                 {why}"
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
            Unread::Silent => {
                "`claude` answered with nothing at all, so there is nothing to vouch for.".into()
            }
        }
    }
}

/// A refusal that will not change by asking again, remembered so it is not asked again.
///
/// **Why this exists.** A host whose `claude` cannot log in answers every call instantly, and the
/// review queue asks once per pull request. On macOS each of those attempts pops a system Keychain
/// dialog. The owner opened the review tab and got a modal, repeatedly, from a fleet that had
/// already been told the answer six times in the same second.
///
/// Only the refusals that are ABOUT THE SETUP are remembered — a missing binary, or a CLI that ran
/// and refused. A timeout is a slow diff and the next one may be fine; an empty answer is about that
/// one prompt. Remembering those would turn one bad moment into a dead feature.
///
/// Cleared by a call that works, and by [`forget_refusal`] — which anything explicitly asked for
/// calls first, because "read this one" is a person saying they think it will work now.
static REFUSED: std::sync::Mutex<Option<Standing>> = std::sync::Mutex::new(None);

/// A remembered refusal, with the two facts a surface needs beyond the sentence: WHEN it happened,
/// and which runtime's credential was being used when it did.
#[derive(Debug, Clone)]
struct Standing {
    why: Unread,
    at_ms: i64,
    runtime: &'static str,
}

/// What the model itself said about a credential — evidence the credential FILE does not carry.
///
/// `fleet::expired_logins` reads `refreshTokenExpiresAt` and believes it. A token can be revoked,
/// or fail to refresh, long before that date: the file still reads live and every model call comes
/// back `Failed to authenticate: OAuth session expired and could not be refreshed`. Reported live
/// by the owner, whose cockpit showed the sentence on a pull request row and no banner anywhere.
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
pub fn auth_refusal() -> Option<AuthRefusal> {
    let standing = REFUSED
        .lock()
        .map(|held| held.clone())
        .unwrap_or_else(|e| e.into_inner().clone())?;
    let said = match &standing.why {
        Unread::Refused { said, .. } => said.clone(),
        _ => return None,
    };
    let lower = said.to_lowercase();
    // Every shape seen from `claude` and `codex` for "this credential is not good any more". A
    // substring list because the CLIs' wording is theirs to change; the cost of a miss is the
    // banner not appearing, which is where this started.
    let auth = [
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
    .any(|needle| lower.contains(needle));
    auth.then(|| AuthRefusal {
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
    );
}

#[cfg(test)]
pub(crate) fn refusal_standing_for_test() -> bool {
    standing_refusal().is_some()
}

/// The refusal being repeated back, if there is one.
fn standing_refusal() -> Option<Unread> {
    REFUSED
        .lock()
        .map(|held| held.clone())
        .unwrap_or_else(|e| e.into_inner().clone())
        .map(|standing| standing.why)
}

fn remember_refusal(why: &Unread, bin: &str) {
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
            at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            // Which credential was in play. `codex` names itself in the binary; everything else
            // skein asks is `claude`, including a `$SKEIN_CLAUDE_BIN` pointed at a stub.
            runtime: match bin.contains("codex") {
                true => "codex",
                false => "claude",
            },
        });
    }
}

/// The call, with the reason it failed kept.
///
/// `output_with_timeout_why` rather than `bounded_output`: the second returns one string for "could
/// not start" and "ran out of time", which is where the four failures first became one.
pub(crate) fn tried(
    bin: &str,
    model: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<String, Unread> {
    // Already told, and told something that asking again cannot change. Answering from memory is
    // the difference between one Keychain dialog and one per pull request.
    if let Some(known) = standing_refusal() {
        return Err(known);
    }
    let mut command = Command::new(bin);
    command.args(["-p", "--model", model, prompt]);
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
    // watched the wrong file. The owner's own words for why this is wrong: using the boxes' login
    // makes a logout one fact, visible everywhere, with one fix.
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
    let started = std::time::Instant::now();
    let out = crate::util::output_with_timeout_why(&mut command, timeout).map_err(|why| {
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
            remember_refusal(&why, bin);
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
        remember_refusal(&why, bin);
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
fn from_sandbox(
    ran: Result<crate::fleet::Ran, String>,
    bin: &str,
    timeout: Duration,
    started: std::time::Instant,
) -> Result<String, Unread> {
    let ran = match ran {
        Ok(ran) => ran,
        // Did not run. Told apart by the clock, exactly as the local path does it: a sandbox that
        // cannot be reached fails fast, and anything that spent its whole budget was running.
        Err(why) => {
            let out = match started.elapsed() >= timeout {
                true => Unread::Slow(timeout),
                // Not `Missing`: the call never reached the machine the CLI lives on, so whatever
                // is wrong is between skein and the sandbox. Reported to a person as "skein could
                // not start `claude` … set SKEIN_CLAUDE_BIN to its full path" on a host where
                // `claude` was fine and `sbx` was absent.
                false => Unread::Unreachable {
                    sandbox: crate::fleet::fleet_sandbox(),
                    why,
                },
            };
            remember_refusal(&out, bin);
            return Err(out);
        }
    };
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
        remember_refusal(&out, bin);
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
        remember_refusal(&out, bin);
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
        remember_refusal(&why, bin);
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
        let started = std::time::Instant::now();
        if let Some(ran) = crate::fleet::model_call_in_sandbox(&bin, &model, prompt, timeout) {
            return from_sandbox(ran, &bin, timeout, started);
        }
    }
    tried(&bin, &model, prompt, timeout)
}

/// Which binary and which model this call will use, after both override layers.
fn binary_and_model(model: Option<&str>) -> (String, String) {
    let bin = env::var("SKEIN_CLAUDE_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "claude".into());
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
    use super::*;
    #[allow(unused_imports)]
    use crate::testutil::*;
    #[allow(unused_imports)]
    use std::{env, fs};

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
            tried(bin, "m", "hi", timeout)
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
    }

    /// A refusal about the setup is asked once, not once per row.
    ///
    /// A host whose `claude` cannot log in answers instantly, and the review queue asks once per
    /// pull request. On macOS every one of those pops a Keychain dialog — the owner opened the tab
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

    /// A model call runs where `skein login` put the credential — in the sandbox.
    ///
    /// Skein authenticated in one place and spent it in another: `/login` happens inside the
    /// sandbox, and this module spawned `claude` as a child of the server, which on a host-driven
    /// deployment is a process on somebody's laptop. On macOS that means the Keychain rather than a
    /// file, so a broken one answered `Not logged in` for every summary while a working credential
    /// sat in the sandbox two hops away.
    ///
    /// Driven with a fake `sbx` that echoes back the script it was handed, because the questions are
    /// *did it go there at all* and *did the prompt survive the trip* — a diff-sized prompt in argv
    /// would have to be quoted, and the heredoc exists so nothing has to be.
    #[cfg(unix)]
    #[test]
    fn a_model_call_runs_where_the_login_is() {
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
        env::remove_var(crate::deployment::IN_FLEET); // host-driven: the sandbox is elsewhere
        forget_refusal();

        // An `sbx` that prints the script it was asked to run, so the test can read what travelled.
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("sbx");
        fs::write(&fake, "#!/usr/bin/env bash\nprintf '%s' \"${@: -1}\"\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let path = env::var("PATH").unwrap_or_default();
        env::set_var("PATH", format!("{}:{path}", bin.display()));
        // A fleet to run in, and the transport off so this takes the `sbx` path deterministically.
        fs::write(
            home.join("config.json"),
            br#"{"fleet_sandbox":"skein-fleet","fleet_agent":false}"#,
        )
        .unwrap();

        // A prompt with every character that would need quoting in argv, and a line that looks like
        // a heredoc terminator — the one input that could end the prompt early and hand the model
        // half a question.
        let nasty = "a diff:\n'quoted' \"double\" $VAR `cmd` \\slash\nSKEIN_PROMPT\nand more";
        let script = claude_oneshot(nasty).expect("the sandbox answered");

        assert!(
            script.contains("skein-fleet") || !script.is_empty(),
            "nothing was sent to the sandbox at all"
        );
        assert!(
            script.contains("-p") && script.contains("--model"),
            "the model call did not travel as one: {script}"
        );
        assert!(
            script.contains("$VAR") && script.contains("`cmd`") && script.contains("'quoted'"),
            "the prompt was mangled on the way — a quoted heredoc expands nothing: {script}"
        );
        // The delimiter grew past the line in the prompt that looked like one. Asserted on the
        // OPENING and the closing together: either alone is satisfied by a delimiter that grew in
        // one place and not the other, which is a heredoc that never terminates.
        assert!(
            script.contains("<<'SKEIN_PROMPT_'") && script.trim_end().ends_with("SKEIN_PROMPT_"),
            "the delimiter did not grow past a prompt containing it, so the heredoc ends early and \
             the model is handed half a question: {script}"
        );
        // The script says it got there, before it does anything else. Without this line a failure
        // cannot be attributed: `sbx exec` exits non-zero on its own account — a stalled daemon, a
        // sandbox that is not running — and an exit code cannot say which program chose it. The
        // marker's ABSENCE on a failure is the evidence that nothing in the sandbox ever ran.
        //
        // Asserted on the SCRIPT, not on a stub's behaviour: the stubs in the sibling test print
        // this marker themselves to stand in for a shell that ran it, so they would go on passing
        // if the real script stopped printing it.
        assert!(
            script.trim_start().starts_with("printf")
                && script.contains(crate::fleet::REACHED)
                && script.find(crate::fleet::REACHED) < script.find("-p"),
            "the call cannot prove it reached the sandbox, so a transport failure will be reported \
             as the model refusing: {script}"
        );

        // And the call brings its own scratch directory. The CLI derives one from the shared
        // /tmp and refuses to start when that path belongs to somebody else — which in a sandbox
        // is whoever ran first, and on the owner's fleet was root. `$HOME` unexpanded, because it
        // is the SANDBOX's home that holds the credential, not the host's.
        let export = script
            .find("CLAUDE_CODE_TMPDIR")
            .expect("the model call took the sandbox's shared /tmp, which anything can poison");
        assert!(
            script.contains(&format!("\"$HOME/{}\"", crate::fleet::MODEL_SCRATCH)),
            "the scratch path was resolved on the host, so it names a directory the sandbox does \
             not have: {script}"
        );
        assert!(
            export < script.find("-p").unwrap(),
            "the scratch directory is exported after the call it is for: {script}"
        );
        // And the sandbox's own environment does not get to choose the credential either. Decided
        // IN the sandbox — it is the sandbox's key and the sandbox's login — and only where there
        // is a login to prefer, so a sandbox authenticated by a key keeps it.
        let unset = script
            .find("unset ANTHROPIC_API_KEY")
            .expect("an API key in the sandbox still outranks the login skein put there");
        assert!(
            script.contains(".claude/.credentials.json") && unset < script.find("-p").unwrap(),
            "the key is unset unconditionally, or after the call it is for: {script}"
        );

        env::set_var("PATH", path);
        for key in ["SKEIN_HOME", "SKEIN_AI"] {
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

        // An ambient HOME with no credential in it — the state the owner's server was in.
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
        // and the owner named the cost of that on their own fleet: skein was reading pull requests
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
            Unread::Silent => 5,
        };
        let mut seen: Vec<usize> = every.iter().map(tag).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen,
            [0, 1, 2, 3, 4, 5],
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
    /// Nothing was wrong with `claude` and `SKEIN_CLAUDE_BIN` was not the cure. The model call
    /// travels into the sandbox now, so a transport failure came back wearing the payload's name —
    /// and sent the reader to check a binary that was fine. This is the line a person reads when
    /// they are already confused, so it is the worst possible place to guess.
    ///
    /// Three failures, three answers, and the test exists because they were one.
    #[cfg(unix)]
    #[test]
    fn a_failure_names_the_program_that_failed() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        env::set_var("SKEIN_HOME", home);
        env::set_var("SKEIN_AI", "on");
        env::remove_var("SKEIN_CLAUDE_BIN"); // or the call never goes to the sandbox at all
        env::remove_var(crate::deployment::IN_FLEET);
        fs::write(
            home.join("config.json"),
            br#"{"fleet_sandbox":"skein-fleet","fleet_agent":false}"#,
        )
        .unwrap();
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let real_path = env::var("PATH").unwrap_or_default();
        let sbx = |body: &str| {
            let at = bin.join("sbx");
            fs::write(&at, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
            fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
            env::set_var("PATH", format!("{}:{real_path}", bin.display()));
            forget_refusal();
        };

        // No `sbx` at all — the call never left the host. The one thing it must NOT say is that
        // `claude` could not be started.
        env::set_var("PATH", bin.display().to_string());
        let _ = fs::remove_file(bin.join("sbx"));
        forget_refusal();
        match claude_oneshot_telling("hi", None, Duration::from_secs(5)) {
            Err(Unread::Unreachable { sandbox, why }) => {
                assert_eq!(sandbox, "skein-fleet", "the sandbox was not named");
                // The transport's own words, and they have to be worth carrying. `bounded_output`
                // says "sbx exec failed to start or exceeded the 30s timeout" for both failures —
                // offering a timeout skein has ALREADY ruled out by the clock, and dropping the one
                // fact the reader cannot recover later: the PATH the server actually had. By the
                // time they go and look, they are looking at their shell's.
                assert!(
                    why.contains("sbx") && why.contains("PATH"),
                    "the transport did not say what failed or where it looked: {why}"
                );
                assert!(
                    !why.contains("or exceeded"),
                    "skein ruled out the timeout by the clock and then offered it anyway: {why}"
                );
                let said = Unread::Unreachable { sandbox, why }.say();
                assert!(
                    said.contains("skein-fleet") && !said.contains("SKEIN_CLAUDE_BIN"),
                    "a missing sandbox was reported as a missing model binary: {said}"
                );
            }
            other => panic!("expected Unreachable, got {other:?}"),
        }

        // The sandbox answers, and `claude` is not in it. `claude` never ran, so its exit code is
        // not skein's to report — and the server's PATH, which `Missing` sends you to check, has
        // nothing to do with a binary inside a sandbox.
        // The marker the real script prints the moment a shell in the sandbox runs it. These fakes
        // never run the script they are handed, so they print it themselves to stand for one that
        // did — without it they are indistinguishable from an `sbx` that failed before the payload
        // started, which is exactly the distinction the case below turns on.
        sbx("echo SKEIN_IN_SANDBOX >&2; echo 'bash: line 2: claude: command not found' >&2; exit 127");
        match claude_oneshot_telling("hi", None, Duration::from_secs(5)) {
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

        // `sbx` itself failing, which is what a stalled daemon or a sandbox that is not running
        // looks like: it exits non-zero, with its own words, and the script never runs. Told apart
        // by evidence rather than by the exit code — 1 means whatever the program that exited chose
        // it to mean — because this is the arm that fires most often, and it was being reported to
        // a person as `claude` refusing.
        sbx("echo 'the daemon is not responding' >&2; exit 1");
        match claude_oneshot_telling("hi", None, Duration::from_secs(5)) {
            Err(Unread::Unreachable { sandbox, why }) => {
                assert_eq!(sandbox, "skein-fleet");
                assert!(
                    why.contains("the daemon is not responding"),
                    "the transport's own words were dropped: {why}"
                );
                let said = Unread::Unreachable { sandbox, why }.say();
                assert!(
                    !said.contains("claude"),
                    "a transport failure was reported under the model's name: {said}"
                );
            }
            other => panic!(
                "an `sbx` that failed before the model ran was reported as the model failing: \
                 {other:?}"
            ),
        }

        // And a CLI that ran and refused still reports its own diagnosis, unchanged — the point of
        // separating the first two is that this one keeps meaning what it says.
        sbx("echo SKEIN_IN_SANDBOX >&2; echo 'Invalid API key'; exit 1");
        match claude_oneshot_telling("hi", None, Duration::from_secs(5)) {
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

        env::set_var("PATH", real_path);
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
                Duration::from_secs(5)
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
    }
}
