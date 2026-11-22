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
}
