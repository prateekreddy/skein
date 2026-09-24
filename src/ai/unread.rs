//! Why a model call produced nothing, told apart four ways, and what a reading carries back
//! about where it ran.

use super::*;

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
    /// **The box it was addressed to did not answer within the budget, and skein stopped there**
    /// (SKEIN-818). Told apart from [`Unread::Slow`] because the two want opposite next moves.
    ///
    /// Before this, a crossing that outlived its timeout fell through to the local spawn like any
    /// other box that could not take the turn, and handed it THE SAME budget it had just spent in
    /// full. A box that hangs on a large pull request therefore cost two of the largest budgets
    /// skein issues, on one reading. Worse, the local attempt could answer with a refusal (a
    /// `--resume` of a session that lives in the box and not here) and [`claude_in_conversation`]
    /// steps down on a refusal — back into the box, for another full budget, up to three times.
    ///
    /// The owner's decision (2026-09-23): the person decides whether to spend again. So this ends
    /// the call, the ladder does not step down on it, `review::after_merged` answers `Stop`, and
    /// the row offers the two ways on — read it again, or read it here instead.
    BoxSlow(Duration),
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

/// **What a model call came back with, and whether it ran where it was addressed** (SKEIN-799).
///
/// The string is the answer, and it is all every caller but one wants. The second field is the
/// fact that used to reach nobody: [`claude_in_turn`] is addressed to a [`Machine::Box`], the box
/// cannot take the turn, and the call runs here instead. That is not a failure — the reading
/// succeeds — so there is no [`Unread`] to carry it, and before this the whole of what was said
/// about it was one `eprintln!` to the server's stderr.
///
/// **A field on the answer rather than a side channel**, and the choice is not stylistic. The
/// cheaper shape is a thread-local that `claude_in_turn` writes and an interested caller reads
/// afterwards, which works exactly until something else makes a model call in between.
/// `review::summarise_and_draft` does: it runs `review::checkout::sweep` — a second model call, on
/// this thread — between the reading and the summary it builds from it, so the reading's reason
/// would be overwritten by the sweep's before anything looked at it, and nothing in the types
/// would say so. Here the compiler makes every caller say what it does with the field, and the
/// three that do not care say `.map(|a| a.said)` in one line each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answered {
    /// What the model said.
    pub said: String,
    /// Why this turn did not run in the box it was addressed to, in words for a reader — from
    /// [`outside_box_because`]. `None` on every call that had no box to lose, which is most of
    /// them.
    pub outside_box: Option<String>,
}

impl Answered {
    /// It ran where it was addressed, which is the ordinary answer.
    pub(super) fn as_addressed(said: String) -> Self {
        Answered {
            said,
            outside_box: None,
        }
    }
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
            // The owner's approved wording (SKEIN-818). The row puts `Not read — ` in front of it.
            Unread::BoxSlow(budget) => format!(
                "its box did not answer within {}, and skein stopped there rather than spend the \
                 same again reading it outside the box.",
                briefly(*budget)
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

/// A budget as a person would say it on a row: `15m`, `7m 30s`, `45s`.
///
/// Minutes because the budgets this is written for are `review::merged_budget`'s — five to
/// fifteen of them — and "900s" is a number the reader has to divide before it means anything.
fn briefly(budget: Duration) -> String {
    let secs = budget.as_secs();
    match (secs / 60, secs % 60) {
        (0, 0) => format!("{}ms", budget.as_millis()),
        (0, s) => format!("{s}s"),
        (m, 0) => format!("{m}m"),
        (m, s) => format!("{m}m {s}s"),
    }
}

/// **Did the crossing into a box run out of time**, as opposed to never being made at all?
///
/// Read off `util::run_bounded`'s own sentence, which is the only thing
/// [`crate::fleet::model_call_in_box`] hands back. One function because two places need the same
/// answer: [`outside_box_because`] words it for a reader, and [`claude_in_turn`] decides on it
/// that the call ends here (SKEIN-818). Two copies of the literal would be two places for it to
/// stop agreeing with the producer; `the_plain_reasons_are_the_ones_a_lost_box_really_answers_with`
/// drives the real producer through this.
pub(crate) fn box_ran_out_of_time(why: &str) -> bool {
    why.contains("did not finish within")
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

/// **Why a box could not take a turn, in words a reader can act on** (SKEIN-799).
///
/// [`crate::fleet::model_call_in_box`] answers failure with a `String`, and the strings it can
/// answer with are somebody else's sentences about a process: "`sh` could not be started: Argument
/// list too long (os error 7)" is true, and it is not something to put on a review row.
///
/// **Only the reasons that `Err` can actually distinguish**, which is a shorter list than it
/// looks, because most of the ways a box call goes wrong are not `Err` at all. A box that is
/// DOWN answers — the crossing runs, exits non-zero and says so — and comes back `Ok(Ran)` for
/// `from_sandbox` to turn into [`Unread::Unreachable`]; the call never falls through and this
/// function never sees it. What reaches here is only the ways the crossing could not be *made*:
///
/// * no placement record — the box was never started, or it was destroyed with its pull request.
///   `model_call_in_box`'s own first line, and the ordinary one.
/// * the crossing outlived the budget — `util::run_bounded`'s kill.
/// * the crossing could not be spawned — `util::spawn_failure`, which is either "not on this
///   process's PATH" (with the whole PATH spelled out, which [`for_a_row`] elides) or the OS's own
///   words for anything else. `E2BIG` is named apart inside that arm because it is the failure
///   SKEIN-799 was filed for: an argv skein could not fit through `execve`.
///
/// Anything else keeps its own sentence, clipped. **An honest fallback rather than a category**:
/// a made-up name for a reason skein cannot recognise is worse than the reason.
///
/// `the_plain_reasons_are_the_ones_a_lost_box_really_answers_with` produces every string above
/// from the real producers rather than quoting them here, so a reworded message fails a test
/// instead of quietly falling back to prose nobody meant a reader to see.
pub(crate) fn outside_box_because(why: &str) -> String {
    let why = why.trim();
    if why.contains("has no placement record") {
        return "skein has no record of where that box is, so it was never started or it is \
                gone"
            .into();
    }
    if box_ran_out_of_time(why) {
        return "getting into it did not finish in time".into();
    }
    if let Some(at) = why.find(SPAWN_REFUSED) {
        let os = why[at + SPAWN_REFUSED.len()..].trim();
        // The errno is for a log, not for a row: `(os error 7)` beside its own name says nothing
        // twice. The name in front of it is what the reader can look up.
        let os = os.split(" (os error ").next().unwrap_or(os).trim();
        return match os {
            "Argument list too long" => "the call was too big to send into it".into(),
            other => format!("skein could not start the program that reaches it: {other}"),
        };
    }
    if why.contains("is not on this process's PATH") {
        return "skein could not start the program that reaches it — it is not on the PATH the \
                server was started with"
            .into();
    }
    for_a_row(why)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            Unread::BoxSlow(Duration::from_secs(900)),
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
            Unread::BoxSlow(_) => 7,
        };
        let mut seen: Vec<usize> = every.iter().map(tag).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen,
            [0, 1, 2, 3, 4, 5, 6, 7],
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
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_AI", "on");
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
        forget_refusal();
    }

    /// **Every plain reason is produced by the thing that produces it** (SKEIN-799).
    ///
    /// [`outside_box_because`] reads sentences it does not own — `fleet::model_call_in_box`'s
    /// first line, and `util::spawn_failure`/`util::run_bounded` underneath it — so a mapping
    /// written from a literal quoted here would go on passing for ever after one of them was
    /// reworded, quietly handing a reader the raw OS string the mapping exists to replace. Every
    /// arm below therefore drives the REAL producer and maps what comes back.
    ///
    /// **What makes it fail:** rewording any of those messages, or dropping an arm from
    /// `outside_box_because`. Both land on the honest fallback, which is not what these assert.
    /// Seen to fail: with `"has no placement record"` changed to `"has no placement"` in the
    /// matcher, the first arm came back as the whole of `model_call_in_box`'s own sentence.
    ///
    /// **What is NOT here, and is not an omission.** A box that is DOWN never reaches this
    /// function: the crossing runs, exits non-zero and says so, and `model_call_in_box` answers
    /// `Ok(Ran)` for `from_sandbox` to turn into [`Unread::Unreachable`] — asserted by
    /// `a_failure_names_the_program_that_failed`, three arms of which are exactly that.
    #[cfg(unix)]
    #[test]
    fn the_plain_reasons_are_the_ones_a_lost_box_really_answers_with() {
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home);
        let call = |name: &str, timeout: Duration| {
            crate::fleet::model_call_in_box(name, "claude", "m", "hi", timeout, vec![], None)
                .expect_err("a crossing that could not be made is not a reading")
        };
        let stand_in = |argv: Vec<String>| {
            crate::place::seam::install(Box::new(move |_: &[String]| Some(argv.clone())))
        };

        // 1. No placement record — never started, or destroyed with its pull request. The ordinary
        //    one, and `model_call_in_box`'s own first line: no crossing is built at all.
        let gone = call("never-placed", Duration::from_secs(5));
        assert_eq!(
            outside_box_because(&gone),
            "skein has no record of where that box is, so it was never started or it is gone",
            "the box skein could not find was reported in `place_of`'s own words: {gone}"
        );

        crate::testutil::placed("review-box");

        // 2. The crossing could not be spawned because it is not there. `spawn_failure` spells the
        //    whole PATH into this one, deliberately (SKEIN-384) — and a row is the one place that
        //    dump must not go.
        {
            let _at = stand_in(vec!["skein-no-such-transport".into()]);
            let why = call("review-box", Duration::from_secs(5));
            let path = env::var("PATH").expect("this process has a PATH");
            assert!(
                why.contains(&path),
                "the producer stopped spelling out the PATH, so this arm no longer tests the \
                 elision it was written for: {why}"
            );
            let plain = outside_box_because(&why);
            assert_eq!(
                plain,
                "skein could not start the program that reaches it — it is not on the PATH the \
                 server was started with",
                "a spawn that found nothing was not recognised: {why}"
            );
            assert!(
                !plain.contains(&path),
                "the server's whole search path was about to be drawn on a review row: {plain}"
            );
        }

        // 3. The crossing was there and would not fit — `E2BIG` out of the real `execve`, which is
        //    the failure this item was filed for. Produced rather than quoted: nothing else can
        //    prove the errno's own words are what arrive.
        {
            let _at = stand_in(vec!["/bin/true".into(), "x".repeat(600_000)]);
            let why = call("review-box", Duration::from_secs(5));
            assert!(
                why.contains("Argument list too long"),
                "600,000 bytes on argv did not come back as E2BIG, so this arm proves nothing \
                 about the failure it is named for: {why}"
            );
            assert_eq!(
                outside_box_because(&why),
                "the call was too big to send into it",
                "the reader was handed an errno: {why}"
            );
        }

        // 4. The crossing was made and outlived its budget.
        {
            let _at = stand_in(vec!["sh".into(), "-c".into(), "exec sleep 2".into()]);
            let why = call("review-box", Duration::from_millis(100));
            assert!(
                why.contains("did not finish within"),
                "the budget's own message changed, so this arm is about nothing: {why}"
            );
            assert_eq!(
                outside_box_because(&why),
                "getting into it did not finish in time",
                "a crossing that ran out of time was not recognised: {why}"
            );
        }

        // 5. And a reason skein does not recognise keeps its own sentence. **The fallback is the
        //    point of it**: a category invented for a message nobody has seen would be skein
        //    telling a reader something it does not know.
        assert_eq!(
            outside_box_because("the moon was in the wrong phase"),
            "the moon was in the wrong phase",
            "skein invented a category for a reason it cannot recognise"
        );

        crate::place::forget_place("review-box");
    }

    /// **A reading that lost its box comes back saying so** — the arm SKEIN-799 is about, driven
    /// the whole way for the first time.
    ///
    /// It could not be driven before, and the reason is worth keeping: `$SKEIN_CLAUDE_BIN` carries
    /// two meanings, and [`claude_in_turn`] needs it UNSET to reach the box at all while
    /// [`agent_command`] needs it SET to allow the fall-through in a test process. Probed before
    /// this was written — the call printed its one stderr line and then panicked in
    /// `agent_command` — which is why [`seam`] exists. The stand-in says only the first half.
    ///
    /// **What makes it fail:** dropping `outside_box = Some(…)` from `claude_in_turn`'s `Err` arm,
    /// which is exactly what production did before this change; the answer then comes back `None`
    /// and the reading is indistinguishable from one that ran in its box. The second half asserts
    /// the other direction, so a `Some(…)` written unconditionally fails too.
    #[cfg(unix)]
    #[test]
    fn a_reading_whose_box_could_not_take_it_says_so_on_the_answer() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home);
        env.set("SKEIN_AI", "on");
        // Unset, and that is what makes the box branch reachable at all.
        env::remove_var("SKEIN_CLAUDE_BIN");
        fs::write(
            home.join("config.json"),
            br#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();
        crate::testutil::placed("review-box");

        // The local reading, which is the one that succeeds. Named through the seam rather than
        // through the variable, because naming it through the variable would skip the box.
        let stub = home.join("local-claude.sh");
        fs::write(
            &stub,
            "#!/bin/sh\ncat >/dev/null\nprintf 'the local reading'\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let _local = seam::stand_in(stub.to_string_lossy().into_owned());

        let ask = || {
            forget_refusal();
            claude_in_turn(
                "hi",
                None,
                Duration::from_secs(10),
                Turn::Alone,
                None,
                Machine::Box("review-box"),
            )
        };

        // The box could not take the turn: the crossing cannot be spawned.
        {
            let _at = crate::place::seam::install(Box::new(|_: &[String]| {
                Some(vec!["skein-no-such-transport".to_string()])
            }));
            let got = ask().expect("the local reading succeeded, so this is not an `Unread`");
            assert_eq!(
                got.said, "the local reading",
                "the fall-through did not run the local reading at all"
            );
            assert_eq!(
                got.outside_box.as_deref(),
                Some(
                    "skein could not start the program that reaches it — it is not on the PATH \
                     the server was started with"
                ),
                "the reading was downgraded out of its box and came back looking ordinary — which \
                 is the whole of SKEIN-799"
            );
        }

        // And a turn the box DID take says nothing, because nothing was lost. Without this the
        // assertion above would pass on an `outside_box` that is always set.
        {
            let _at = crate::place::seam::install(Box::new(|_: &[String]| {
                Some(vec![
                    "sh".to_string(),
                    "-c".into(),
                    format!(
                        "printf '%s\\n' {} >&2; printf 'the box read it'",
                        crate::fleet::REACHED
                    ),
                ])
            }));
            let got = ask().expect("the box answered");
            assert_eq!(got.said, "the box read it");
            assert_eq!(
                got.outside_box, None,
                "a reading that ran exactly where it was addressed was told it had been downgraded"
            );
        }

        crate::place::forget_place("review-box");
        forget_refusal();
    }
}
