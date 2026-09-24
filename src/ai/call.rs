//! The model call itself: the one-shot, the call in a pull request's conversation or a named
//! turn, the ceiling in front of both destinations, and reading a sandbox run the way a
//! local one is read.

use super::*;

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
///
/// **Checked once, at the fork in [`claude_in_turn`], and not in either destination** (SKEIN-706).
/// It was written inside [`tried`], which is the local spawn — one of the two places a call can
/// run, and the one reached only after the box has been tried and has declined. So for every call
/// that HAD a box the ceiling was never consulted: the refusal existed on the path that did not
/// need it. The cure is not a second check in `fleet::model_call_in_box`; two checks of one rule
/// are two places for it to stop agreeing, which is [`crate::util::fleet_root`]'s argument about
/// its own default, made here about a limit. `claude_in_turn` is the single door — every caller in
/// the crate arrives through it, and
/// `the_ceiling_sits_in_front_of_both_destinations_and_not_inside_one` derives that from the source
/// rather than asserting it, so a third destination added below the check fails the build instead
/// of quietly escaping it.
///
/// **What this ceiling is not.** It is skein's judgement about what a prompt this large *means*,
/// and it is three times larger than the largest prompt skein can build. It is therefore not a
/// guard against `MAX_ARG_STRLEN`, which the in-box path still meets at 32 pages — 131,072 bytes on
/// ordinary 4 KiB-page hardware, which 3 of those 27 real prompts exceed. That failure, and what a
/// reader should be told when it happens, is still open (SKEIN-706).
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
    // **The ceiling is NOT here, and it used to be** (SKEIN-706). It sat at the top of this
    // function, which is one of the two destinations a model call has and not the door they share
    // — so it covered the local spawn and never once ran for a call that had a box, because
    // [`claude_in_turn`] tries [`crate::fleet::model_call_in_box`] *first* and only falls through
    // to here. The path that needed it least was the only path that had it. It is at the fork now,
    // in [`claude_in_turn`], in front of the choice rather than inside one arm of it; see
    // [`PROMPT_CEILING`] for why one check and not two.
    //
    // Already told, and told something that asking again cannot change. Answering from memory is
    // the difference between one Keychain dialog and one per pull request.
    if let Some(known) = standing_refusal() {
        return Err(known);
    }
    // [`agent_command`] rather than `Command::new`: this is the spawn that spends money, so it is
    // the one that has to refuse a test process which never said which binary to run (SKEIN-764).
    let mut command = agent_command(bin);
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
    // `Machine::Wherever` has no box to lose, so there is never anything on the answer beside the
    // answer. See [`Answered`].
    claude_in_turn(prompt, model, timeout, Turn::Alone, None, Machine::Wherever).map(|a| a.said)
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
/// more would spend three budgets on one reading. That includes a BOX that ran out of time
/// ([`Unread::BoxSlow`], SKEIN-818): before it ended the call, the box's timeout fell through to a
/// local spawn, whose `--resume` of a session that lives in the box answered with a refusal — and
/// a refusal steps down, back into the box, for another full budget. Measured by
/// `a_box_that_runs_out_of_time_is_asked_once_and_nothing_runs_here`. A missing binary or a refused login answers the
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
) -> Result<Answered, Unread> {
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
) -> Result<Answered, Unread> {
    // **The ceiling, in front of the fork below** (SKEIN-706). This is the door every model call
    // goes through — `claude_oneshot_telling`, `claude_in_conversation`'s ladder and
    // `review::checkout`'s two calls all arrive here, and the two destinations (a box, or the local
    // spawn in [`tried`]) are both chosen below this line. Checked here it is one check for both;
    // checked in either arm it is a check for one of them, which is what it was, and the arm it was
    // in was the arm that could not need it. See [`PROMPT_CEILING`].
    //
    // **Before the standing refusal in [`tried`], because this is a fact about THIS call.** A
    // cached refusal is a fact about the setup and answering from it is right for everything else;
    // a prompt bigger than skein will send is wrong whatever the setup is doing, and reporting a
    // stale "not logged in" for it would send the reader to fix something that is not the problem.
    if prompt.len() > PROMPT_CEILING {
        return Err(Unread::TooLarge {
            bytes: prompt.len(),
            limit: PROMPT_CEILING,
        });
    }
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
    // **The downgrade, on its way to the reader** (SKEIN-799). Set only on the fall-through below,
    // and carried out on the ANSWER rather than dropped there — because the call that follows it
    // succeeds, so there is no [`Unread`] to put it in and nothing else would ever mention it.
    // See [`Answered`] for why this is not a side channel.
    let mut outside_box = None;
    if !named {
        // **A box is asked first and answered last**: it is the most specific destination, and it
        // is the only one whose absence is ordinary. A review box that was never started, or was
        // destroyed when its pull request closed, is not an error — it is a reading that happens
        // the way every reading happened before §11. So a missing placement falls through to the
        // two below rather than being reported, and a box that ANSWERS is the answer, whatever it
        // said: `from_sandbox` already tells "the CLI refused" apart from "the script never ran".
        // The credential crosses into `fleet` still a `Secret` (SKEIN-536). Its bytes are exposed
        // in one place, `fleet::github_export`, at the moment they are written into the box.
        if let Machine::Box(name) = machine {
            match crate::fleet::model_call_in_box(
                name,
                &bin,
                &model,
                prompt,
                timeout,
                turn.args(),
                github,
            ) {
                Ok(ran) => return from_sandbox(ran, &bin, turn).map(Answered::as_addressed),
                // **A box that ran out of time ends the call here** (SKEIN-818). Every other `Err`
                // below cost nothing — no record, a spawn that failed on the spot — and is right to
                // fall through at once. This one has already spent the whole budget, and falling
                // through handed the local spawn THE SAME budget again; the owner's decision is
                // that spending it again is the reader's call, not skein's. Not remembered as a
                // standing refusal: it is a fact about this box and this moment, not the setup.
                Err(why) if box_ran_out_of_time(&why) => {
                    eprintln!(
                        "skein: {name} did not answer within its budget, so the call stops here \
                         rather than spend it again outside the box — {why}"
                    );
                    return Err(Unread::BoxSlow(timeout));
                }
                Err(why) => {
                    // Still said here, because the whole string belongs in a log: the PATH skein
                    // had, the errno, the program's own name. What goes to the reader is the
                    // sentence [`outside_box_because`] makes of it — see there for why those are
                    // two different things.
                    eprintln!(
                        "skein: {name} could not take this turn, so it runs where readings ran \
                         before — {why}"
                    );
                    outside_box = Some(outside_box_because(&why));
                }
            }
        }
        // A second destination used to sit here: a call shipped into the fleet sandbox, because
        // that is where `skein login` put the credential and a host-driven skein was somewhere
        // else. Skein is in that sandbox now (SKEIN-576), so
        // running the call here IS running it where the login is, and the fall-through below is
        // that destination rather than a fallback from it.
    }
    tried(&bin, &model, prompt, timeout, turn, github).map(|said| Answered { said, outside_box })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    /// The credential every stub GitHub below is called with.
    ///
    /// Prefixed `skein-test-` deliberately: a fixture that looked like a real token
    /// (`gho_…`, `ghp_…`) is indistinguishable from one in a grep, and this tree has already had
    /// to sweep a client's real strings out of its fixtures once.
    fn fixture_token() -> crate::secret::Secret {
        crate::secret::Secret::new("skein-test-github-token")
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
    ///
    /// **Driven through [`claude_in_turn`] and no longer through [`tried`]** (SKEIN-706). `tried`
    /// is one of two destinations; the check moved to the door in front of both, so asserting it
    /// where it used to live would assert it on the arm that never needed it. The sibling
    /// `the_ceiling_refuses_before_a_box_is_ever_reached` drives the other arm.
    #[cfg(unix)]
    #[test]
    fn a_prompt_bigger_than_skein_will_send_is_refused_with_its_size_and_the_limit() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &dir);
        env.set("SKEIN_FLEET_ROOT", &dir);
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

        // Pinned, because `claude_in_turn` reads it to decide whether this call goes into a box —
        // and because `agent_command` refuses a test process that never said which binary to run.
        env.set("SKEIN_CLAUDE_BIN", &bin);

        let over = "x".repeat(PROMPT_CEILING + 1);
        forget_refusal();
        let refused = claude_in_turn(
            &over,
            Some("m"),
            Duration::from_secs(30),
            Turn::Alone,
            None,
            Machine::Wherever,
        );
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
            claude_in_turn(
                &at_the_line,
                Some("m"),
                Duration::from_secs(30),
                Turn::Alone,
                None,
                Machine::Wherever,
            )
            .map(|a| a.said)
            .as_deref(),
            Ok("done"),
            "a prompt exactly at the ceiling was refused, so the limit is off by one — or has been \
             shrunk under what skein actually sends"
        );
    }

    /// **A call that has a box carries its prompt on stdin, and nowhere in the crossing's argv**
    /// (SKEIN-799).
    ///
    /// This test was written for SKEIN-706 to pin the defect — it asserted the prompt was in
    /// exactly one argv element, the last one — and it is that same test inverted. Both halves are
    /// asserted together for `the_prompt_travels_on_stdin_and_is_nowhere_in_the_process_list`'s
    /// reason: each alone is satisfiable in a way that loses the other, and this is the box arm of
    /// the call that one covers locally.
    ///
    /// **The `/proc/<pid>/cmdline` half is measured on a real process, not on the recorded argv.**
    /// The stand-in is spawned carrying the argv production built *as its own arguments*, so the
    /// file any process on this machine can read holds the bytes skein would have put in `ps`.
    /// That is SKEIN-706's own "done when", and it cannot be satisfied by a fixture that captured
    /// nothing, because the call's flags are asserted to be in the same capture.
    ///
    /// **600,000 bytes, and the number is the point.** `MAX_ARG_STRLEN` caps a single argv element
    /// at 32 pages: 131,072 on 4 KiB-page hardware and 524,288 on this box's 16 KiB pages. This
    /// payload is past both, so with the prompt back inside the script the crossing cannot be
    /// spawned at all — and [`claude_in_turn`] answers an unspawnable crossing by falling through
    /// in silence, which is the whole of SKEIN-799.
    ///
    /// This is the test that did not exist, and its absence is why SKEIN-706 survived SKEIN-684.
    /// Every review fixture pins `$SKEIN_CLAUDE_BIN`, which [`claude_in_turn`] reads as "run
    /// exactly this, and therefore run it HERE" — so every one of them skips the box branch before
    /// it is tried, and two of their comments went on claiming the prompt travels on stdin, which
    /// is true of the path their own fixture pins and of nothing else.
    ///
    /// **How a test reaches that branch without being able to spawn a real agent.** Three things,
    /// none of which touches [`agent_command`]'s guard:
    ///
    /// * `$SKEIN_CLAUDE_BIN` unset, so the branch is taken at all;
    /// * [`crate::testutil::placed`], so the box has a placement record to be reached through —
    ///   without one `model_call_in_box` returns `Err` before building anything;
    /// * a [`crate::place::seam`] stand-in that **succeeds**, so `from_sandbox` reads an answer and
    ///   `claude_in_turn` returns from the box path. It never falls through to [`tried`], so the
    ///   local spawn — the only thing that could run the owner's real `claude` — is never reached.
    ///   That fall-through is exactly what caught out
    ///   `a_failure_names_the_program_that_failed`, which is why its failure arms assert on
    ///   `model_call_in_box` directly; the arm that ANSWERS has no such hazard and can be driven
    ///   the whole way.
    ///
    /// The seam is handed the argv production would have spawned, so where the prompt's bytes are
    /// is a measurement rather than a reading of the source. `place::exec_argv` still ends
    /// `argv.push(self.wrap(script))` — the change is that the script it wraps no longer contains
    /// the prompt, and `Place::attempt` takes the prompt as a separate `feed` that becomes the
    /// child's stdin.
    #[cfg(unix)]
    #[test]
    fn a_call_with_a_box_reaches_the_box_and_carries_its_prompt_on_stdin() {
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home);
        env::remove_var("SKEIN_CLAUDE_BIN"); // or the call never crosses at all
        fs::write(
            home.join("config.json"),
            br#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();
        crate::testutil::placed("review-box");

        // A marker long enough that it cannot be a coincidence, and shaped so a grep for it finds
        // this test and nothing else.
        let marker = "SKEIN-799-PROMPT-OFF-ARGV-MARKER";
        let prompt = format!("{marker} ").repeat(19_000);
        assert!(
            prompt.len() > 524_288 && prompt.len() < PROMPT_CEILING,
            "the payload has to be past MAX_ARG_STRLEN on a 16 KiB-page machine and under the \
             ceiling, or this test passes with the prompt back inside the script: {} bytes",
            prompt.len()
        );

        // Where the stand-in reports what it was actually handed. The argv the seam RECORDS is
        // skein's own value; these two files are what a process on this machine could read off it.
        let argv_at = home.join("argv-the-crossing-was-spawned-with");
        let stdin_at = home.join("stdin-the-crossing-was-fed");

        let seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> = Default::default();
        let recorder = std::sync::Arc::clone(&seen);
        let (argv_to, stdin_to) = (argv_at.clone(), stdin_at.clone());
        let _at = crate::place::seam::install(Box::new(move |argv: &[String]| {
            recorder.lock().unwrap().push(argv.to_vec());
            // **The argv production built becomes the stand-in's own arguments**, so `$$`'s
            // cmdline below is the process list entry skein would have made. A stand-in that
            // discarded them would prove nothing about `/proc/<pid>/cmdline`.
            //
            // Succeeds, reaching the far side: the marker on stderr is what `from_sandbox` reads
            // to tell "the box answered" from "nothing in it ever ran".
            let mut stand_in = vec![
                "bash".to_string(),
                "-c".into(),
                format!(
                    "printf '%s\\n' \"$@\" > {argv}\n\
                     if [ -r \"/proc/$$/cmdline\" ]; then tr '\\0' '\\n' < \"/proc/$$/cmdline\" >> {argv}; fi\n\
                     cat > {stdin}\n\
                     printf '%s\\n' {reached} >&2\n\
                     printf 'the box read it'\n",
                    argv = argv_to.display(),
                    stdin = stdin_to.display(),
                    reached = crate::fleet::REACHED,
                ),
                "bash".into(),
            ];
            stand_in.extend(argv.iter().cloned());
            Some(stand_in)
        }));

        let said = claude_in_turn(
            &prompt,
            Some("m"),
            Duration::from_secs(30),
            Turn::Alone,
            None,
            Machine::Box("review-box"),
        );
        assert_eq!(
            said.map(|a| a.said).as_deref(),
            Ok("the box read it"),
            "a prompt past MAX_ARG_STRLEN never reached the box — which is the whole defect: the \
             crossing cannot be spawned, and `claude_in_turn` falls through to the local reading \
             without telling anybody"
        );

        // **Guarding against having captured nothing**, which is how a test like this passes for
        // the wrong reason: an empty recording satisfies every `any(...)` below.
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            1,
            "the crossing was not reached exactly once, so what follows is about nothing: {}",
            seen.len()
        );
        let argv = &seen[0];

        // The crossing really is the box's, and not some other fleet-scope command — and it really
        // carries the model call, or "the prompt is not in it" would hold of an empty argv.
        assert!(
            argv.iter().any(|a| a.contains("review-box")),
            "the argv reached is not this box's"
        );
        assert!(
            argv.iter().any(|a| a.contains("-p --model")),
            "the argv reached carries no model call, so the assertion below is about nothing"
        );

        // **The finding, inverted.** Not one element, not the last one, not anywhere.
        assert!(
            !argv.iter().any(|a| a.contains(marker)),
            "the prompt is on the crossing's argv, so it is capped at MAX_ARG_STRLEN and readable \
             in `ps` for the minutes the call runs — private repositories included"
        );

        // And the same thing asked of the kernel rather than of skein's own value.
        let handed = fs::read_to_string(&argv_at).expect("the stand-in captured no argv at all");
        assert!(
            handed.contains("-p --model"),
            "the capture holds no model call, so it would satisfy the next assertion however the \
             prompt was sent"
        );
        assert!(
            !handed.contains(marker),
            "the prompt is in the spawned process's /proc/<pid>/cmdline"
        );
        assert_eq!(
            fs::read_to_string(&stdin_at).expect("the stand-in was fed nothing at all"),
            prompt,
            "the box was not handed the prompt on stdin, or not all of it"
        );

        drop(_at);
        crate::place::forget_place("review-box");
        forget_refusal();
    }

    /// **The ceiling refuses a call with a box before the box is reached** — the half of
    /// [`PROMPT_CEILING`] that did not exist (SKEIN-706).
    ///
    /// The check sat in [`tried`], which a call with a box reaches only after
    /// `model_call_in_box` has been tried and has declined, so an over-ceiling prompt was handed to
    /// the crossing first and the refusal fired — if it fired at all — on the way back from it.
    /// This asserts the crossing is not reached AT ALL, which is what "nothing was spawned and no
    /// model was asked" has to mean on a path whose spawn is somebody else's process.
    ///
    /// Its sibling `a_prompt_bigger_than_skein_will_send_is_refused_with_its_size_and_the_limit`
    /// drives the local arm through the same door, and neither can pass while the check is in one
    /// of the two arms.
    #[cfg(unix)]
    #[test]
    fn the_ceiling_refuses_before_a_box_is_ever_reached() {
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home);
        env::remove_var("SKEIN_CLAUDE_BIN");
        fs::write(
            home.join("config.json"),
            br#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();
        crate::testutil::placed("review-box");

        let reached = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&reached);
        // Succeeds if it is ever asked, so a ceiling that failed to refuse would come back `Ok`
        // rather than erroring for some unrelated reason and looking like a refusal.
        let _at = crate::place::seam::install(Box::new(move |_argv: &[String]| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                format!(
                    "printf '%s\\n' {} >&2; printf 'the box read it'",
                    crate::fleet::REACHED
                ),
            ])
        }));

        let over = "x".repeat(PROMPT_CEILING + 1);
        assert_eq!(
            claude_in_turn(
                &over,
                Some("m"),
                Duration::from_secs(10),
                Turn::Alone,
                None,
                Machine::Box("review-box"),
            ),
            Err(Unread::TooLarge {
                bytes: PROMPT_CEILING + 1,
                limit: PROMPT_CEILING,
            }),
            "a prompt over the ceiling was sent to a box anyway, or refused as something else"
        );
        assert_eq!(
            reached.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the crossing was spawned with a prompt skein had already decided not to send"
        );

        // And a prompt under the ceiling still reaches the box, or the assertion above is bought by
        // refusing everything.
        forget_refusal();
        assert_eq!(
            claude_in_turn(
                "read this change.",
                Some("m"),
                Duration::from_secs(10),
                Turn::Alone,
                None,
                Machine::Box("review-box"),
            )
            .map(|a| a.said)
            .as_deref(),
            Ok("the box read it"),
            "a prompt well under the ceiling never reached the box"
        );
        assert_eq!(
            reached.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the crossing was not reached for a prompt that is nowhere near the ceiling"
        );

        drop(_at);
        crate::place::forget_place("review-box");
        forget_refusal();
    }

    /// **One check covers both destinations, derived from the source rather than asserted.**
    ///
    /// [`PROMPT_CEILING`]'s doc claims `claude_in_turn` is the single door in front of both places
    /// a model call can run. A claim like that is true on the day it is written and silent
    /// afterwards — which is precisely how the ceiling came to live in one arm: [`tried`] WAS the
    /// only destination once. So this counts rather than paraphrases.
    ///
    /// It reads every file of this module, strips each one's test module, and requires that every
    /// production call of [`tried`] and of [`crate::fleet::model_call_in_box`] is inside
    /// `claude_in_turn`'s body — which is below the check. Add a third destination beside them,
    /// or call either one from somewhere new, and this fails instead of the ceiling quietly not
    /// applying to it.
    #[test]
    fn the_ceiling_sits_in_front_of_both_destinations_and_not_inside_one() {
        // Every file of the module, not only this one: a destination called from a sibling file
        // would pass under the ceiling's nose just the same.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ai");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .expect("src/ai/")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "rs"))
            .collect();
        files.sort();
        let production = files
            .iter()
            .map(|path| {
                let src = std::fs::read_to_string(path).expect("a file of src/ai/");
                // Production only: everything from a file's own `mod tests` header on is ours.
                match src.find("\nmod tests {") {
                    Some(at) => src[..at].to_string(),
                    None => src,
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let production = production.as_str();

        let begins = production
            .find("pub(crate) fn claude_in_turn(")
            .expect("claude_in_turn is not in this file under that name");
        // To the next item at column zero, which ends the function.
        let ends = begins
            + production[begins..]
                .find("\n}\n")
                .expect("claude_in_turn's body does not end where this expects it");

        // The check itself is in that body. Without this the test would pass with no ceiling
        // anywhere at all.
        let checks_at = begins
            + production[begins..ends]
                .find("prompt.len() > PROMPT_CEILING")
                .expect("claude_in_turn does not check the ceiling at all");

        for destination in ["tried(&bin", "fleet::model_call_in_box("] {
            let sites: Vec<usize> = production
                .match_indices(destination)
                .map(|(at, _)| at)
                .collect();
            assert!(
                !sites.is_empty(),
                "no production call of `{destination}` was found, so this test is about nothing — \
                 it was probably renamed"
            );
            for site in sites {
                assert!(
                    (begins..ends).contains(&site),
                    "`{destination}` is called outside `claude_in_turn`, so a model call can reach \
                     a destination without passing the ceiling"
                );
                assert!(
                    site > checks_at,
                    "`{destination}` is reached before the ceiling is checked"
                );
            }
        }
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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_AI", "on");
        env.set("HOME", home);

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
        env.set("SKEIN_CLAUDE_BIN", &bin);

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
            said.map(|a| a.said).as_deref(),
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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_AI", "on");

        // A stub `claude` that answers with the HOME it was given.
        let bin = home.join("claude");
        fs::write(&bin, "#!/usr/bin/env bash\nprintf '%s' \"$HOME\"\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        env.set("SKEIN_CLAUDE_BIN", &bin);

        // An ambient HOME with no credential in it — the state a live fleet's server was in.
        let bare = home.join("bare");
        fs::create_dir_all(&bare).unwrap();
        env.set("HOME", &bare);

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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_AI", "on");

        // A stub that answers with the auth source it was handed.
        let bin = home.join("claude");
        fs::write(
            &bin,
            "#!/usr/bin/env bash\nprintf '%s' \"${ANTHROPIC_API_KEY:-none}\"\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        env.set("SKEIN_CLAUDE_BIN", &bin);
        env.set("ANTHROPIC_API_KEY", "sk-ant-stale");

        // A HOME carrying a login: the key is removed, and the call runs on the subscription.
        let mine = home.join("mine");
        fs::create_dir_all(mine.join(".claude")).unwrap();
        fs::write(
            mine.join(".claude/.credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#,
        )
        .unwrap();
        env.set("HOME", &mine);
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some("none"),
            "an inherited API key outranked the login skein manages"
        );

        // No login anywhere — the key is the only credential there is, and removing it would leave
        // the call with nothing. skein prefers its own login; it does not refuse keys.
        let bare = home.join("bare");
        fs::create_dir_all(&bare).unwrap();
        env.set("HOME", &bare);
        forget_refusal();
        assert_eq!(
            claude_oneshot("hi").as_deref(),
            Some("sk-ant-stale"),
            "skein took away the only credential the call had"
        );

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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_AI", "on");

        // A stub `claude` that answers with the scratch directory it was handed.
        let bin = home.join("claude");
        fs::write(
            &bin,
            "#!/usr/bin/env bash\nprintf '%s' \"${CLAUDE_CODE_TMPDIR:-the shared /tmp}\"\n",
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        env.set("SKEIN_CLAUDE_BIN", &bin);

        let live = br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","refreshTokenExpiresAt":32503680000000}}"#;

        // An ambient HOME that carries a login: the call stays on it, and so does its scratch.
        let mine = home.join("mine");
        fs::create_dir_all(mine.join(".claude")).unwrap();
        fs::write(mine.join(".claude/.credentials.json"), live).unwrap();
        env.set("HOME", &mine);
        assert_eq!(
            claude_oneshot("hi"),
            Some(crate::fleet::model_scratch_dir(&mine).display().to_string()),
            "the call wrote its scratch into a directory skein does not own"
        );

        // And when the call moves to the login skein keeps, the scratch moves with it — a temp
        // directory under a HOME the call is no longer using is the same bug wearing a hat.
        let bare = home.join("bare");
        fs::create_dir_all(&bare).unwrap();
        env.set("HOME", &bare);
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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("HOME", home);

        let bin = home.join("claude");
        let mut stub = |body: &str| {
            fs::write(&bin, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            env.set("SKEIN_CLAUDE_BIN", &bin);
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

        forget_refusal();
    }

    /// **A box that runs out of time is asked once, and nothing runs here after it** (SKEIN-818).
    ///
    /// The whole ladder is driven — [`claude_in_conversation`], not one [`claude_in_turn`] — because
    /// the spend the item is about was the ladder's as much as the fall-through's. Every crossing
    /// into the box and every local spawn appends one line to the same log, so the assertion is on
    /// what actually ran, in order, and not on what a function returned.
    ///
    /// The local stand-in refuses a named conversation the way the real CLI does when the session
    /// lives in the box and not here (`No conversation found`, exit 1) and answers a turn that names
    /// none. That is the shape the design agent read out of the code: the box times out, the local
    /// `--resume` refuses, a refusal steps down — back into the box. Seen when the fall-through was
    /// put back (the sabotage recorded in the commit): the log read `box local box local box local`,
    /// three full box budgets and three local spawns for one call, and the call SUCCEEDED from the
    /// third local one, so nothing on the answer would have said any of it happened.
    ///
    /// **What makes it fail:** the `box_ran_out_of_time` arm in `claude_in_turn` removed (the log
    /// gains `local`, and the box line repeats); or the ladder stepping down on `BoxSlow` (a second
    /// `box`).
    #[cfg(unix)]
    #[test]
    fn a_box_that_runs_out_of_time_is_asked_once_and_nothing_runs_here() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home);
        env.set("SKEIN_AI", "on");
        // Unset, so the box branch is reachable; the local binary is named through the seam.
        env::remove_var("SKEIN_CLAUDE_BIN");
        crate::testutil::placed("review-box");
        let log = home.join("ran.log");

        let stub = home.join("local-claude.sh");
        fs::write(
            &stub,
            format!(
                "#!/bin/sh\ncat >/dev/null\necho local >> '{log}'\n\
                 case \" $* \" in *' --resume '*|*' --session-id '*) \
                 echo 'No conversation found with session ID: x'; exit 1;; esac\n\
                 printf 'the local reading'\n",
                log = log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let _local = seam::stand_in(stub.to_string_lossy().into_owned());

        // The box: it is entered, says so, and never answers inside the budget.
        let entered = format!("echo box >> '{}'; exec sleep 5", log.display());
        let _at = crate::place::seam::install(Box::new(move |_: &[String]| {
            Some(vec!["sh".to_string(), "-c".into(), entered.clone()])
        }));

        let budget = Duration::from_millis(300);
        let began = std::time::Instant::now();
        let got = claude_in_conversation(
            "hi",
            None,
            budget,
            "3f1c9a52-0000-4000-8000-000000000818",
            home,
            None,
            Machine::Box("review-box"),
        );
        let ran = fs::read_to_string(&log).unwrap_or_default();
        let ran: Vec<&str> = ran.lines().collect();

        assert_eq!(
            ran,
            ["box"],
            "a box that ran out of time was followed by more spending — a local call, or a second \
             trip into the box — where the owner decided the call stops: {ran:?}"
        );
        assert_eq!(
            got,
            Err(Unread::BoxSlow(budget)),
            "the call did not end on the box's own timeout"
        );
        assert!(
            began.elapsed() < Duration::from_secs(4),
            "the box's budget was not enforced, so this is not the timeout arm at all"
        );
        assert!(
            Unread::BoxSlow(Duration::from_secs(900))
                .say()
                .starts_with("its box did not answer within 15m, and skein stopped there"),
            "the row's sentence is not the approved one"
        );

        crate::place::forget_place("review-box");
        forget_refusal();
    }
}
