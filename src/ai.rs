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

/// Can skein start the model binary at all? Asked without spending a token.
///
/// `--version` rather than a prompt: the commonest failure by far is that the binary is not on the
/// PATH of the process running the server — which is not your shell's — and that question has an
/// answer that costs nothing. A binary that runs and then refuses is a different report, and the
/// call site that actually needs the model is where that one surfaces.
///
/// Not `program_on_path`: `$SKEIN_CLAUDE_BIN` may be an absolute path, and a PATH scan answers "no"
/// for one that works perfectly.
pub fn model_reachable() -> Result<(), Unread> {
    let (bin, _) = binary_and_model(None);
    let mut command = Command::new(&bin);
    command.arg("--version");
    match crate::util::output_with_timeout_why(&mut command, Duration::from_secs(10)) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(Unread::Refused {
            code: out
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".into()),
            said: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }),
        Err(why) => Err(Unread::Missing { bin, why }),
    }
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
    // `$SKEIN_AI_MODEL` still wins over the call site: it is the escape hatch that lets one env var
    // pin every AI call in a run, which is what the tests and a cost-conscious operator both need.
    let (bin, model) = binary_and_model(model);
    tried(&bin, &model, prompt, timeout).ok()
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
    /// `claude` could not be started at all — almost always not on this process's PATH.
    Missing { bin: String, why: String },
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
                "skein could not start `{bin}` ({why}). It is on the PATH of the process running                  skein-server that matters, not your shell's — start the server from a shell that                  has it, or set SKEIN_CLAUDE_BIN to its full path."
            ),
            Unread::Refused { code, said } if said.is_empty() => format!(
                "`claude` exited {code} without saying why. Run the same call by hand to see it:                  `claude -p --model claude-haiku-4-5 hello`."
            ),
            Unread::Refused { code, said } => {
                format!("`claude` exited {code}: {}", crate::util::clip(said, 240))
            }
            Unread::Slow(budget) => format!(
                "`claude` was still going after {}s. A larger diff needs longer than this call                  allows; nothing is wrong with the model.",
                budget.as_secs()
            ),
            Unread::Silent => {
                "`claude` answered with nothing at all, so there is nothing to vouch for.".into()
            }
        }
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
    let mut command = Command::new(bin);
    command.args(["-p", "--model", model, prompt]);
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
    })?;
    if !out.status.success() {
        return Err(Unread::Refused {
            code: out
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".into()),
            said: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match said.is_empty() {
        true => Err(Unread::Silent),
        false => Ok(said),
    }
}

/// The same call, reporting why rather than only that. Used where the reason reaches a person.
pub(crate) fn claude_oneshot_telling(
    prompt: &str,
    model: Option<&str>,
    timeout: Duration,
) -> Result<String, Unread> {
    let (bin, model) = binary_and_model(model);
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
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let stub = |name: &str, body: &str| {
            let at = dir.join(name);
            fs::write(&at, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
            fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
            at.display().to_string()
        };
        let quick = Duration::from_secs(5);

        // Not on PATH — the commonest one by far, and the one the old message never named. Its
        // sentence has to be about the SERVER's PATH: a person reads it, checks their shell, finds
        // `claude` right there, and concludes skein is broken.
        match tried("skein-no-such-binary", "m", "hi", quick) {
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

        // Ran and refused. Its stderr is the diagnosis — not logged in, a model it will not serve,
        // a rate limit — and it was being thrown away.
        let refused = stub("refuses", "echo 'Invalid API key' >&2; exit 3");
        match tried(&refused, "m", "hi", quick) {
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

        // Ran, succeeded, said nothing. Distinct from every other case: there is no fault to fix.
        let silent = stub("says-nothing", "exit 0");
        assert_eq!(tried(&silent, "m", "hi", quick), Err(Unread::Silent));

        // Still going when the budget ran out — and told apart from a failed spawn by the clock
        // rather than by parsing a message.
        let slow = stub("dawdles", "sleep 30");
        let began = std::time::Instant::now();
        let out = tried(&slow, "m", "hi", Duration::from_secs(1));
        assert_eq!(out, Err(Unread::Slow(Duration::from_secs(1))));
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the budget was not enforced"
        );

        // And the happy path still is one.
        let works = stub("answers", "echo '  a summary  '");
        assert_eq!(tried(&works, "m", "hi", quick), Ok("a summary".to_string()));
    }

    /// The health report asks about both switches, not one of them.
    ///
    /// `review_summaries` defaults ON and `ai_enrichment` defaults off, so a report that consulted
    /// only the second said "off" on the common configuration — while every review summary on that
    /// fleet was failing. The one place somebody would look, saying the feature was not in use.
    #[test]
    fn what_wants_the_model_names_every_switch_that_does() {
        let _g = crate::testutil::env_lock();
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
