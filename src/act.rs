//! An Act: a streaming, non-idempotent thing skein is doing, that any surface can watch (§2.5).
//!
//! # What this is for
//!
//! Creating a box was reachable only from a WebSocket. The cockpit opened
//! `/api/boxes/<name>/terminal?launch=<branch>` and the server ran the create inside the PTY it had
//! just made — so a surface that never opens a terminal could not create a box at all, and porting
//! the REST API alone would have lost the feature entirely.
//!
//! The PTY was not incidental. Creating a box is minutes of clone, substrate and provisioning, and a
//! person watching wants to see it; when it fails, that terminal is where the error is. So the fix is
//! not "make it a POST that returns 201" — an Act has no `desired` and no `check`, it is
//! **streaming and unacknowledged**, and what a caller needs is to start it, watch it, and still be
//! able to read what happened after the stream has closed.
//!
//! # Three properties, and each is a bug somebody has had
//!
//! **It outlives its watchers.** The output is buffered here, not piped to whoever asked. A browser
//! that reloads mid-create used to reconnect to a terminal that had closed and find nothing —
//! `fleet::remember_start_failure` exists because of exactly that, and it keeps the *reason*; this
//! keeps the transcript.
//!
//! **Many watchers, one run.** Two tabs watching is two receivers on one broadcast, not two creates.
//! It is the same rule as §10.1's "one producer, fanned out" and the same lesson: check-then-act once
//! gave every browser tab its own subprocess every tick.
//!
//! **Asking twice does not do it twice.** An Act is non-idempotent by definition, so the guard
//! cannot be "check whether it is needed" — it is that an id already running is refused, and the
//! caller is handed the one that is running.
//!
//! # Bounded on purpose
//!
//! The buffer has a cap and drops from the front, saying so where it dropped. An act that prints a
//! gigabyte is a build with a broken progress bar, and a cockpit that dies of it is worse than one
//! that shows the last megabyte. The registry has a retention window for the same reason the
//! warden's outcome store does: something has to say when a finished thing may be forgotten, and the
//! answer must not be "never".

use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How much of an act's output is kept. Enough for a create's whole transcript, bounded so a runaway
/// one cannot take the process down with it.
pub const KEPT: usize = 256 * 1024;

/// How long the transcript may still be arriving after the command itself has exited.
///
/// The child's exit closes its ends of the pipes, so in the ordinary case the pumps see EOF within
/// microseconds and this is never waited on at all. It exists for the case that is not ordinary — a
/// command that leaves a grandchild holding stdout — where the choice is between an act that never
/// ends and a transcript that is short. Two seconds buys the first without risking the second.
const DRAIN: Duration = Duration::from_secs(2);

/// How long a finished act stays readable. A person who reloads a browser gets their answer; a
/// server that has been up for a month does not accumulate every box it ever made.
pub const RETENTION: Duration = Duration::from_secs(30 * 60);

/// The Acts (§2.5), named — because leaving them unnamed is how they grow *beside* the primitives.
///
/// Converse is the second-most-frequent job skein has and had no primitive at all. These have no
/// `desired` and no `check`: they are streaming, non-idempotent, and **doing them twice is doing
/// them twice**. Forcing them into Operation makes "ensure, never do" a lie.
///
/// Every accessor below is an exhaustive match, so a new Act must say what it disturbs before it
/// compiles — which is the same discipline `signal::Signal` uses for cost, and for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Sending a message to an agent, or answering the question it stopped on.
    Say,
    /// Interrupting a turn.
    Interrupt,
    /// Putting a file into a box.
    Upload,
    /// Attaching a terminal to a box's session.
    Attach,
    /// Bringing a box up. The one Act that is also a long-running command, which is why
    /// [`begin`] exists.
    StartBox,
}

impl Act {
    pub const ALL: [Act; 5] = [
        Act::Say,
        Act::Interrupt,
        Act::Upload,
        Act::Attach,
        Act::StartBox,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Act::Say => "say",
            Act::Interrupt => "interrupt",
            Act::Upload => "upload",
            Act::Attach => "attach",
            Act::StartBox => "start-box",
        }
    }

    /// What this Act makes wrong, so the board never serves the value from before it.
    ///
    /// **An Act must settle the gate**, and that is not a nicety: a `Gate` serves its last good
    /// answer while refreshing behind the caller, so an Act whose effect nobody announced leaves the
    /// person who just did it looking at the state from before they did. Three box acts had exactly
    /// this bug and nothing caught it.
    ///
    /// Answering an agent changes what it is doing, which is a box's liveness-adjacent turn state —
    /// but that arrives as an *edge* from the box's own hooks rather than from a gate, which is why
    /// `Say` and `Interrupt` disturb nothing here. Saying so is the point: "nothing" is a
    /// declaration, and an Act that quietly declared nothing because nobody thought about it is the
    /// case this list exists to make visible.
    pub fn disturbs(self) -> &'static [crate::signal::Remembered] {
        use crate::signal::Remembered::*;
        match self {
            // The turn state comes back as an edge from the box, not from a gate skein holds.
            Act::Say | Act::Interrupt => &[],
            // A file in the box is bytes on the shared disk.
            Act::Upload => &[BoxDisk],
            // Attaching reads; it changes nothing anybody has remembered.
            Act::Attach => &[],
            // A box that was not there is there now: the sweep has not seen it, and its tree is new
            // on the disk.
            Act::StartBox => &[BoxLiveness, BoxDisk],
        }
    }

    /// **Every Act reports an outcome**, and this returns `true` for all of them by construction.
    ///
    /// An earlier draft of §2.5 called Acts "unacknowledged", and upload is the counter-example that
    /// settles it: an empty piece mid-stream is how chunked encoding spells "that was the last one",
    /// so an unacknowledged upload had a **silent truncation** mode — the file arrived, shorter, and
    /// nothing said so. That is a defect, not a design. Having no `check` is a different thing from
    /// having no result.
    ///
    /// A function rather than a comment because it is the kind of claim that quietly stops being
    /// true: a new Act whose result is dropped would have to return `false` here and fail its test.
    pub fn reports_outcome(self) -> bool {
        match self {
            Act::Say | Act::Interrupt | Act::Upload | Act::Attach | Act::StartBox => true,
        }
    }
}

/// What an act is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum State {
    /// Still going.
    Running,
    /// Finished, with the command's exit code. **Not `ok`/`failed`** — the code is what the caller
    /// renders and what a person quotes, and collapsing it loses the difference between a refusal
    /// and a crash.
    Ended { code: i32 },
}

/// An act as a caller sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Look {
    pub id: String,
    #[serde(flatten)]
    pub state: State,
    /// Everything it has said, up to [`KEPT`].
    pub output: String,
    /// RFC3339.
    pub started_at: String,
}

struct Running {
    output: Mutex<String>,
    state: Mutex<State>,
    since: Instant,
    started_at: String,
    say: tokio::sync::broadcast::Sender<String>,
}

fn registry() -> &'static Mutex<HashMap<String, std::sync::Arc<Running>>> {
    static ACTS: OnceLock<Mutex<HashMap<String, std::sync::Arc<Running>>>> = OnceLock::new();
    ACTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What a person is told when an act would not start.
///
/// **The sentence about the program is [`crate::util::spawn_failure`]'s, and it is now called
/// rather than copied** (SKEIN-429). This file carried that paragraph word for word, which is two
/// things to keep in step and one of them drifts — skein should say one thing about a program it
/// could not start.
///
/// The prefix is act's own, and it has to be said: an act runs under `sh -c`, so what could not be
/// started is the SHELL. "No such file or directory (os error 2)" on its own sends the reader to
/// look for the act's command, which was fine.
///
/// Its own function so it can be checked without arranging a failing spawn. The failure worth
/// checking here is a wording that drifted apart again, not an ENOENT.
fn did_not_start(id: &str, cmd: &Command, e: &std::io::Error) -> String {
    format!(
        "{id} runs under `sh -c`, and {}",
        crate::util::spawn_failure(cmd, e)
    )
}

/// Begin an act, or say who is already doing it.
///
/// `command` runs under `sh -c`, because every act skein has today is a command line it composes —
/// and composing one is what `sandbox::launch_command_as` already does.
pub fn begin(id: &str, command: &str) -> Result<Look, String> {
    if !crate::util::valid_name(id) {
        return Err(format!("{id:?} is not an act id"));
    }
    let mut acts = registry().lock().unwrap_or_else(|e| e.into_inner());
    // Finished acts past their window go here, so the registry is pruned by use rather than by a
    // timer — a server nobody is asking anything of has nothing to forget.
    acts.retain(|_, act| {
        let ended = matches!(
            *act.state.lock().unwrap_or_else(|e| e.into_inner()),
            State::Ended { .. }
        );
        !ended || act.since.elapsed() < RETENTION
    });
    if let Some(existing) = acts.get(id) {
        if matches!(
            *existing.state.lock().unwrap_or_else(|e| e.into_inner()),
            State::Running
        ) {
            return Err(format!(
                "{id} is already running — watch that one rather than starting a second. An act is \
                 not idempotent: doing it twice is doing it twice."
            ));
        }
    }

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| did_not_start(id, &cmd, &e))?;

    let (say, _) = tokio::sync::broadcast::channel(256);
    let act = std::sync::Arc::new(Running {
        output: Mutex::new(String::new()),
        state: Mutex::new(State::Running),
        since: Instant::now(),
        started_at: chrono::Utc::now().to_rfc3339(),
        say,
    });
    acts.insert(id.to_string(), std::sync::Arc::clone(&act));
    drop(acts);

    // stdout and stderr both, on their own threads, into one transcript — because a create's errors
    // and its progress are one story and interleaving them is how it reads on a terminal.
    //
    // Counted, because the state must not say `Ended` before they are done. A caller that reads the
    // transcript once, when the state flips, is the case this whole shape exists for — the browser
    // that reloads mid-create — and a create that failed on its last line of stderr would report
    // ended with the reason missing. It surfaced as a test that failed about one full run in ten.
    let draining = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let out = child.stdout.take();
    let err = child.stderr.take();
    for stream in [
        out.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        err.map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let act = std::sync::Arc::clone(&act);
        draining.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let left = std::sync::Arc::clone(&draining);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                act.append(&line);
                line.clear();
            }
            left.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }

    let waiting = std::sync::Arc::clone(&act);
    std::thread::spawn(move || {
        let code = child
            .wait()
            .ok()
            .and_then(|s| s.code())
            // Killed by a signal. `-1` rather than a pretend success, because "it stopped and nobody
            // knows why" is a real answer and a zero here would be a lie.
            .unwrap_or(-1);
        // **Drained before declared, and bounded.** Waiting unconditionally is right for every
        // ordinary act and hangs on a pathological one: a command that leaves something holding its
        // stdout never reaches EOF, and the act would stay `Running` for ever with the cockpit
        // showing a create that never finishes. So the pumps get a deadline, and if they miss it the
        // transcript says so rather than being quietly short.
        let deadline = Instant::now() + DRAIN;
        while draining.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                waiting.append(
                    "… the command ended with something still holding its output open; what \
                     follows was not captured …\n",
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        *waiting.state.lock().unwrap_or_else(|e| e.into_inner()) = State::Ended { code };
        // A watcher blocked on the stream has to learn that there will be no more, and a closed
        // channel is the only signal that cannot be mistaken for a slow act.
        let _ = waiting.say.send(String::new());
    });

    look(id).ok_or_else(|| format!("{id} vanished as it started"))
}

impl Running {
    fn append(&self, chunk: &str) {
        let mut held = self.output.lock().unwrap_or_else(|e| e.into_inner());
        held.push_str(chunk);
        if held.len() > KEPT {
            // Said where it happened, rather than silently. A transcript that begins mid-sentence
            // with no explanation reads as a bug in whatever produced it.
            let keep = held.len() - KEPT;
            let cut = held
                .char_indices()
                .map(|(i, _)| i)
                .find(|i| *i >= keep)
                .unwrap_or(held.len());
            *held = format!("… earlier output dropped …\n{}", &held[cut..]);
        }
        let _ = self.say.send(chunk.to_string());
    }
}

/// What an act is doing, and everything it has said.
pub fn look(id: &str) -> Option<Look> {
    let act = {
        let acts = registry().lock().unwrap_or_else(|e| e.into_inner());
        std::sync::Arc::clone(acts.get(id)?)
    };
    let state = act.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let output = act.output.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Some(Look {
        id: id.to_string(),
        state,
        output,
        started_at: act.started_at.clone(),
    })
}

/// Watch an act: everything it has said so far, and a receiver for the rest.
///
/// Both together and under one lock, because taking them separately is a race that loses whatever
/// the act said in between — which for a create is the line that mattered.
pub fn watch(id: &str) -> Option<(String, tokio::sync::broadcast::Receiver<String>)> {
    let act = {
        let acts = registry().lock().unwrap_or_else(|e| e.into_inner());
        std::sync::Arc::clone(acts.get(id)?)
    };
    let held = act.output.lock().unwrap_or_else(|e| e.into_inner());
    Some((held.clone(), act.say.subscribe()))
}

/// The act id for creating a box. Its own function so the route and the watcher cannot disagree.
pub fn creating(box_name: &str) -> String {
    format!("create-{box_name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wait for an act to finish. **Generously**, because this asserts an outcome and not a latency.
    ///
    /// The first version waited two seconds, which is less than one of these tests' own `sleep 2`
    /// and not enough for any of them on a loaded machine: it passed alone and failed in a full
    /// parallel run, which is the worst way for a budget to be wrong. A wait that is too long costs
    /// nothing when the act finishes, and a wait that is too short is a test that fails for reasons
    /// that have nothing to do with what it is about.
    fn settle(id: &str) -> Look {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            if let Some(seen) = look(id) {
                if !matches!(seen.state, State::Running) {
                    return seen;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{id} never finished");
    }

    /// [`begin`], with the environment held still across the spawn — and nothing else.
    ///
    /// An act runs under `sh -c`, and `sh` is found through the process-global `PATH`. So every
    /// test here *reads* a variable other tests in this same process *write*, and two of them write
    /// a `PATH` no shell lives in: `src/sbx.rs` sets `PATH=""` to prove a branch can be read
    /// without forking `git`, and `src/ai.rs` narrows it to a stub directory to prove `sbx` is
    /// absent. Either one landing inside this spawn is
    /// `act-slow could not be started: No such file or directory (os error 2)` — a red test in the
    /// one file the change did not touch, which is what SKEIN-421 cost a stream. Measured before
    /// this guard: 1 failure in 20 `cargo test --lib` runs, in `act-drain`, on an unmodified tree.
    ///
    /// The crate's single env lock is what makes taking it here sufficient rather than hopeful:
    /// every writer takes the same one, and `tools/env-lock-check.py` is what keeps that true.
    ///
    /// **Across the spawn and nothing else.** The child gets its own copy of the environment the
    /// moment it exists, so there is nothing left to race with afterwards — and these acts sleep
    /// for seconds each, which held under the lock would serialize them against every env-taking
    /// test in the crate for no property gained.
    fn begin_undisturbed(id: &str, command: &str) -> Result<Look, String> {
        let _env = crate::testutil::env_lock();
        begin(id, command)
    }

    /// One sentence about a program skein could not start, and one place it is written.
    ///
    /// This file used to carry `util::spawn_failure`'s paragraph word for word (SKEIN-429). The
    /// failure worth checking is therefore not an ENOENT — it is the day someone improves the
    /// wording in one of two places, so the assertion is that act's line still ENDS in util's,
    /// whatever util's has become.
    ///
    /// Checked against a made-up error rather than a failing spawn, because arranging one means
    /// taking `sh` off the process's PATH, and doing that is the bug SKEIN-428 has just finished
    /// removing from this crate.
    #[test]
    fn an_act_that_would_not_start_names_the_shell_and_the_path_it_looked_on() {
        let _env = crate::testutil::env_lock();
        let cmd = Command::new("sh");

        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let said = did_not_start("act-x", &cmd, &missing);
        assert!(
            said.starts_with("act-x runs under `sh -c`, and"),
            "the reader is not told which act, or what it runs a command under: {said}"
        );
        assert!(
            said.ends_with(&crate::util::spawn_failure(&cmd, &missing)),
            "act has gone back to writing its own version of util's sentence: {said}"
        );
        // The PATH itself. By the time the reader goes to check, they are checking their own
        // shell's, which is the one that works — so it cannot be recovered later.
        let path = std::env::var("PATH").unwrap_or_default();
        assert!(
            !path.is_empty() && said.contains(&path),
            "the message never says which PATH skein looked on: {said}"
        );

        // A permission bit is not a PATH problem, and reporting it as one sends the reader to the
        // wrong file entirely.
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let said = did_not_start("act-x", &cmd, &denied);
        assert!(
            !said.contains("PATH"),
            "a fault that had nothing to do with the PATH was reported as a missing one: {said}"
        );
        assert!(
            said.contains(&denied.to_string()),
            "the OS's own reason was thrown away: {said}"
        );
    }

    /// The transcript is whole at the moment the state says it ended.
    ///
    /// The case this whole shape exists for is a caller that reads **once**, when the state flips —
    /// `/v2`'s create watcher does exactly that, and the browser reloading mid-create is the story
    /// in the doc above. The pumps run on their own threads, so an act that declared itself ended
    /// while they were still draining would hand that caller a create whose failure is missing its
    /// last line. It surfaced as a flake: one full parallel run in ten.
    ///
    /// A burst on stderr immediately before exiting, because stderr is the pipe that carries the
    /// reason and the one most likely to still be in flight.
    #[test]
    fn what_it_said_is_all_there_the_moment_it_says_it_ended() {
        // Enough that the pump cannot possibly be finished when the child exits: the pipe holds
        // 64 KiB, so a burst larger than that is still in flight at the moment `wait()` returns.
        // Two hundred lines was not enough — the sabotage that removes the drain passed against it,
        // which made the test a description of the fix rather than a check on it.
        let lines = 20_000;
        begin_undisturbed(
            "act-drain",
            &format!(
                "i=0; while [ $i -lt {lines} ]; do echo line-$i >&2; i=$((i+1)); done; exit 7"
            ),
        )
        .unwrap();
        // Polled tightly, so the first sighting of `Ended` is the one asserted on rather than a
        // later one that had time to catch up.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let done = loop {
            let seen = look("act-drain").expect("registered");
            if !matches!(seen.state, State::Running) {
                break seen;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "act-drain never finished"
            );
        };
        assert_eq!(done.state, State::Ended { code: 7 });
        let got = done
            .output
            .lines()
            .filter(|l| l.starts_with("line-"))
            .count();
        assert_eq!(
            got,
            lines,
            "it declared itself ended with {} of {lines} lines still to come",
            lines - got
        );
    }

    /// An act whose output somebody else is holding open still ends.
    ///
    /// The other side of the drain, and the reason it is bounded: a command that leaves a grandchild
    /// with the pipe never reaches EOF, and waiting for it would leave a create showing `running`
    /// for ever. It ends, and the transcript says what it could not capture rather than being
    /// quietly short.
    #[test]
    fn an_act_whose_pipe_is_held_open_still_ends() {
        // `sh` exits at once; the background `sleep` inherits stdout and keeps the pipe open.
        begin_undisturbed("act-held", "sleep 30 & echo started; exit 0").unwrap();
        let done = settle("act-held");
        assert_eq!(done.state, State::Ended { code: 0 });
        assert!(
            done.output.contains("still holding its output open"),
            "a truncated transcript said nothing about being truncated: {:?}",
            done.output
        );
    }

    /// It runs, it says what it said, and it is still readable after it has ended.
    ///
    /// The last part is the whole point: a browser that reloads mid-create used to reconnect to a
    /// terminal that had closed and find nothing at all.
    #[test]
    fn an_act_outlives_the_thing_that_started_it() {
        begin_undisturbed("act-hello", "echo one; echo two >&2; exit 3").unwrap();
        let done = settle("act-hello");
        assert_eq!(
            done.state,
            State::Ended { code: 3 },
            "the exit code is the answer"
        );
        assert!(done.output.contains("one"), "{:?}", done.output);
        assert!(
            done.output.contains("two"),
            "stderr is part of the transcript, not a separate story: {:?}",
            done.output
        );
        // Still there, a moment later, with nobody watching.
        assert_eq!(
            look("act-hello").map(|l| l.state),
            Some(State::Ended { code: 3 })
        );
    }

    /// Asking twice does not do it twice, and the refusal says what to do instead.
    #[test]
    fn a_second_ask_is_refused_while_the_first_is_running() {
        begin_undisturbed("act-slow", "sleep 2; echo done").unwrap();
        let why = begin_undisturbed("act-slow", "echo a second one").unwrap_err();
        assert!(why.contains("already running"), "{why}");
        assert!(
            why.contains("doing it twice"),
            "the refusal must say why it is not simply retried: {why}"
        );
        // And once it has ended, the id can be used again — a create that failed is one somebody
        // fixes and asks for again.
        let done = settle("act-slow");
        assert_eq!(done.state, State::Ended { code: 0 });
        assert!(begin_undisturbed("act-slow", "echo again").is_ok());
    }

    /// A watcher gets what it missed and then the rest, with no gap between the two.
    #[test]
    fn a_watcher_arriving_late_is_not_missing_the_beginning() {
        begin_undisturbed("act-watch", "echo first; sleep 1; echo second").unwrap();
        // **Waited for, not slept through.** A fixed pause has to be long enough for the first line
        // on a loaded machine and short enough to be inside the second's window, and on a machine
        // busy enough it is neither — which showed up as this test failing about one run in three
        // while the code it tests was fine. Polling for the thing the test is waiting on has no such
        // window: it is as fast as the machine and as patient as it needs to be.
        let began = std::time::Instant::now();
        while !look("act-watch").is_some_and(|l| l.output.contains("first")) {
            assert!(
                began.elapsed() < Duration::from_millis(900),
                "the act produced nothing in almost the whole gap before its second line"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let (so_far, mut rest) = watch("act-watch").expect("watchable");
        assert!(
            so_far.contains("first"),
            "the buffer did not carry what was missed"
        );
        assert!(!so_far.contains("second"));

        let heard = std::thread::spawn(move || {
            let mut all = String::new();
            while let Ok(chunk) = rest.blocking_recv() {
                if chunk.is_empty() {
                    break;
                }
                all.push_str(&chunk);
            }
            all
        })
        .join()
        .unwrap();
        assert!(
            heard.contains("second"),
            "the rest never arrived: {heard:?}"
        );
        settle("act-watch");
    }

    /// Every Act says what it makes wrong, and every one reports an outcome.
    ///
    /// Two properties that fail silently. An Act that does not settle the gate leaves the person who
    /// just did the thing looking at the state from before they did it — three box acts had exactly
    /// that bug and nothing caught it. And an Act that drops its result is the upload defect's
    /// shape: the file arrives, shorter, and nothing says so.
    #[test]
    fn an_act_declares_what_it_disturbs_and_always_reports() {
        for act in Act::ALL {
            assert!(
                act.reports_outcome(),
                "{} does not report an outcome — having no `check` is a different thing from \
                 having no result, and an upload that dropped its result truncated silently",
                act.name()
            );
            assert!(!act.name().is_empty());
        }
        // The ones that change something a gate remembers say so, and the ones that do not say that
        // — `&[]` is a declaration here, not an oversight, and the doc on `disturbs` says which is
        // which and why.
        assert_eq!(
            Act::StartBox.disturbs(),
            &[
                crate::signal::Remembered::BoxLiveness,
                crate::signal::Remembered::BoxDisk
            ]
        );
        assert_eq!(
            Act::Upload.disturbs(),
            &[crate::signal::Remembered::BoxDisk]
        );
        assert!(Act::Attach.disturbs().is_empty(), "attaching reads");
    }

    /// What is **not** an Act, kept where somebody would otherwise add it.
    ///
    /// Both were miscategorised once. Takeover is a privileged snapshot, then an Operation, then a
    /// paid model call, plus durable rollback state — it creates a *replacement* box on the other
    /// runtime and keeps the original as the way back. Merging a pull request is an Operation of
    /// class `destructive`, and Acts have no class field at all.
    ///
    /// Asserted by absence, which is the only way to assert it: the list is the whole claim.
    #[test]
    fn takeover_and_merge_are_not_acts() {
        let named: Vec<&str> = Act::ALL.iter().map(|a| a.name()).collect();
        for operation in ["takeover", "merge", "merge-pr", "resize", "destroy"] {
            assert!(
                !named.contains(&operation),
                "`{operation}` is an Operation — it has a check, or a class, or both, and an Act \
                 has neither"
            );
        }
        assert_eq!(named.len(), Act::ALL.len());
    }

    /// An id that is not one is refused before anything runs.
    ///
    /// Deliberately the **same** guard as a box name, `util::valid_name`, because an act id is
    /// derived from the box it is about (`create-<box>`) — a stricter rule here would refuse to
    /// create a box whose name skein itself considers legal, and the refusal would arrive as a
    /// mysterious "not an act id" about a name the user never typed.
    #[test]
    fn an_id_that_is_not_one_starts_nothing() {
        for bad in ["../escape", "", "a/b", "x\0y"] {
            let why = begin(bad, "echo should not run").unwrap_err();
            assert!(why.contains("is not an act id"), "{bad:?}: {why}");
        }
        // And every id skein mints for a box it would accept is itself acceptable.
        for name in ["web-main", "gadget-demo-optimize-AI", "a b"] {
            assert!(
                crate::util::valid_name(&creating(name)),
                "skein would create a box called {name:?} and could not name the act for it"
            );
        }
    }
}
