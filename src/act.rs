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

/// How long a finished act stays readable. A person who reloads a browser gets their answer; a
/// server that has been up for a month does not accumulate every box it ever made.
pub const RETENTION: Duration = Duration::from_secs(30 * 60);

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

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{id} could not be started: {e}"))?;

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
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                act.append(&line);
                line.clear();
            }
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

    fn settle(id: &str) -> Look {
        for _ in 0..200 {
            if let Some(seen) = look(id) {
                if !matches!(seen.state, State::Running) {
                    return seen;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{id} never finished");
    }

    /// It runs, it says what it said, and it is still readable after it has ended.
    ///
    /// The last part is the whole point: a browser that reloads mid-create used to reconnect to a
    /// terminal that had closed and find nothing at all.
    #[test]
    fn an_act_outlives_the_thing_that_started_it() {
        begin("act-hello", "echo one; echo two >&2; exit 3").unwrap();
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
        begin("act-slow", "sleep 2; echo done").unwrap();
        let why = begin("act-slow", "echo a second one").unwrap_err();
        assert!(why.contains("already running"), "{why}");
        assert!(
            why.contains("doing it twice"),
            "the refusal must say why it is not simply retried: {why}"
        );
        // And once it has ended, the id can be used again — a create that failed is one somebody
        // fixes and asks for again.
        let done = settle("act-slow");
        assert_eq!(done.state, State::Ended { code: 0 });
        assert!(begin("act-slow", "echo again").is_ok());
    }

    /// A watcher gets what it missed and then the rest, with no gap between the two.
    #[test]
    fn a_watcher_arriving_late_is_not_missing_the_beginning() {
        begin("act-watch", "echo first; sleep 0.3; echo second").unwrap();
        // Long enough for the first line and not the second.
        std::thread::sleep(Duration::from_millis(120));
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
