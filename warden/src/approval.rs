//! Where a human says yes, and why it has to be here (§8.1, §8.4).
//!
//! # The loop this closes
//!
//! The cockpit is served by skein, inside the fleet, beside coding agents. If the approval card were
//! the cockpit's, the path would be: box → compromise skein → skein asserts "a human approved" →
//! warden runs a privileged host command. An earlier draft of the design made them the same
//! component. So the warden **renders and confirms its own approvals, on the host, outside the
//! fleet**, and never trusts an `approved` field on the wire.
//!
//! Generalised, because this is the shape of a whole class of bug: **approval is a fact the
//! approving side writes, never a field the requester supplies.** There is a live example of getting
//! it wrong in skein's own `substrate.rs` — it re-reads the request at install time and validates the
//! `state` and the *shape* of the package names, but the names come from that same re-read of a file
//! "writable by every box in the fleet". Approve `jq`, rewrite the file, get arbitrary names into a
//! root `apt-get`. It is unexploitable today only because the queue is still masked.
//!
//! # `/dev/tty`, and what it buys
//!
//! The surface is the controlling terminal, opened as `/dev/tty` rather than taken as stdin. That is
//! deliberate: `/dev/tty` reaches the person's terminal even when the warden's output is redirected
//! to a log, and — the half that matters — **it fails to open when there is no terminal**, which is
//! the honest test for "is there a human at the host". A warden started by a supervisor has no
//! approval surface, refuses everything, and says so at startup.
//!
//! **What it does not buy**, stated rather than implied: any process running as the same uid can
//! reach the warden's file descriptors. Today skein runs as that uid, so today's guarantee is
//! against a *box*, not against a compromised skein on the same machine. §9.5.1's uid split is what
//! closes the rest, and it is delivery step 4b. This is the strongest form available before it, and
//! it is strictly stronger than a flag on a request.
//!
//! # Typing the id back
//!
//! The prompt asks for the operation id, not for `y`. Three reasons and all three are real: it makes
//! "what you see is what will run" literal, because answering requires reading the line; it means a
//! person clearing a queue of prompts cannot approve the wrong one by rhythm; and it is the only
//! confirmation that cannot be given by a keystroke that was already in the buffer.

use crate::doer::{Approver, Request};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Mutex;

/// The terminal, and one conversation at a time on it.
pub struct Console {
    /// Prompts are serialised. Two threads interleaving on one terminal would produce a screen where
    /// the id under the cursor is not the id the answer will be matched against — which is the one
    /// property this surface exists to have.
    ///
    /// §8.5's one-outstanding-request rule is a different mechanism for a different reason, and it
    /// is not built yet; this is only the display being coherent.
    seat: Mutex<Seat>,
}

struct Seat {
    ask: Box<dyn BufRead + Send>,
    say: Box<dyn Write + Send>,
}

impl Console {
    /// The controlling terminal, or `None` when there is not one.
    ///
    /// `None` is not a failure to handle — it is the answer "nobody is here", and the caller turns
    /// it into a warden that refuses every doer.
    pub fn at_the_terminal() -> Option<Console> {
        let read = std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/tty")
            .ok()?;
        let write = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .ok()?;
        Some(Console::over(Box::new(read), Box::new(write)))
    }

    /// The same surface over any pair of streams. Its own constructor so the prompt is testable
    /// without a terminal — the thing under test is the text and what it accepts, not the tty.
    pub fn over(ask: Box<dyn Read + Send>, say: Box<dyn Write + Send>) -> Console {
        Console {
            seat: Mutex::new(Seat {
                ask: Box::new(BufReader::new(ask)),
                say,
            }),
        }
    }
}

/// What a person is shown, built from the warden's own parse and nothing else.
///
/// Its own function so it can be asserted on directly, and so that the one place a requester's
/// string appears — the sandbox name inside `what` — is the same string the doer passes to `sbx`.
/// **An operation id is correlation, not content** (§8.4): it identifies the request, and the
/// description comes from the resolved arguments, never from display text somebody sent.
///
/// **The closing lines used to say "nothing the requester wrote is displayed here", and that was
/// never true.** The sandbox name is the requester's string and so is every argument inside `what`
/// — §8.4's rule is that the warden *re-derives* the description from them, not that it invents
/// them. A prompt that overstates its own guarantee is worse than one that states it plainly,
/// because the guarantee is what the person is being asked to rely on. What is true is the sentence
/// this says instead, and `serve::vetted` is what makes it true: nothing reaches these lines that
/// they cannot render as itself, so there is no argument here that the terminal will draw as
/// something else.
pub fn prompt(request: &Request, what: &str) -> String {
    format!(
        "\n\
         ────────────────────────────────────────────────────────────\n\
         skein-warden: a privileged host command needs your approval\n\
         \n\
         \x20 operation   {}\n\
         \x20 sandbox     {}\n\
         \x20 will run    {}\n\
         \n\
         `will run` is this warden's own parse, environment included, and\n\
         it is what it will execute. There is no description on the wire.\n\
         \n\
         Type the operation id to approve. Anything else refuses.\n\
         > ",
        request.operation, request.sandbox, what
    )
}

impl Approver for Console {
    fn approve(&self, request: &Request, what: &str) -> Result<(), String> {
        let mut seat = self.seat.lock().unwrap_or_else(|e| e.into_inner());
        let seat = &mut *seat;
        seat.say
            .write_all(prompt(request, what).as_bytes())
            .map_err(|e| format!("the approval could not be shown: {e}"))?;
        seat.say
            .flush()
            .map_err(|e| format!("the approval could not be shown: {e}"))?;

        let mut answer = String::new();
        // A terminal that closed is a person who is no longer there, which refuses rather than
        // approves. Every way this can fail has to land on the same side.
        seat.ask
            .read_line(&mut answer)
            .map_err(|e| format!("the answer could not be read, so nothing was approved: {e}"))?;

        // Compared against the id the doer will run under, so approving is the same act as reading.
        if answer.trim() != request.operation {
            let _ = writeln!(seat.say, "refused.\n");
            return Err(format!(
                "{what} was refused at the host: the operation id was not confirmed. Operation {}.",
                request.operation
            ));
        }
        let _ = writeln!(seat.say, "approved.\n");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked() -> Request {
        Request {
            operation: "op-7f3a91".into(),
            sandbox: "skein-fleet".into(),
            args: vec!["--memory".into(), "26g".into()],
            env: Vec::new(),
        }
    }

    /// A shared buffer the console writes into, so the test can read what a person would see.
    #[derive(Clone, Default)]
    struct Screen(std::sync::Arc<Mutex<Vec<u8>>>);
    impl Write for Screen {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Screen {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    fn console(typed: &str) -> (Console, Screen) {
        let screen = Screen::default();
        (
            Console::over(
                Box::new(std::io::Cursor::new(typed.as_bytes().to_vec())),
                Box::new(screen.clone()),
            ),
            screen,
        )
    }

    /// Typing the id approves; typing anything else does not.
    ///
    /// `y` is in the list on purpose: it is what a person clearing prompts by rhythm would press,
    /// and this surface is built so that it does nothing.
    #[test]
    fn approving_means_typing_the_operation_id() {
        let (yes, screen) = console("op-7f3a91\n");
        assert!(yes.approve(&asked(), "`sbx rm -f skein-fleet`").is_ok());
        assert!(screen.text().contains("approved."));

        for typed in ["y\n", "yes\n", "\n", "op-7f3a92\n", " OP-7F3A91\n", ""] {
            let (no, screen) = console(typed);
            let why = no
                .approve(&asked(), "`sbx rm -f skein-fleet`")
                .expect_err(&format!("{typed:?} approved a fleet destroy"));
            assert!(
                why.contains("op-7f3a91"),
                "a refusal must name the operation: {why}"
            );
            assert!(why.contains("refused at the host"), "{why}");
            assert!(!screen.text().contains("approved."));
        }
    }

    /// What a person sees is the argv the warden will run, and the id it will run under.
    ///
    /// The requester has nowhere to put display text — `doer::Request` has no such field, and
    /// `serve` refuses a payload carrying one — so this asserts the other half: everything on the
    /// screen came from the warden's own parse.
    #[test]
    fn the_screen_shows_what_will_run_and_under_which_id() {
        let (console, screen) = console("op-7f3a91\n");
        let what = "`sbx rm -f skein-fleet` — THIS DESTROYS THE FLEET";
        console.approve(&asked(), what).unwrap();
        let seen = screen.text();
        assert!(seen.contains("op-7f3a91"), "{seen}");
        assert!(seen.contains("sbx rm -f skein-fleet"), "{seen}");
        assert!(seen.contains("THIS DESTROYS THE FLEET"), "{seen}");
        // **The screen says what it is, and no more than what it is.** It used to end "Nothing the
        // requester wrote is displayed here", which was false of the two lines above it — the
        // sandbox and every argument are the requester's strings, re-derived rather than invented.
        // Asserting the false sentence's absence as well as the true one's presence, because a
        // prompt that overstates its guarantee is the one failure this whole surface cannot have.
        assert!(
            seen.contains("this warden's own parse") && seen.contains("no description on the wire"),
            "the screen must say what it is, or a person cannot know to trust it: {seen}"
        );
        assert!(
            !seen.contains("Nothing the requester wrote"),
            "the prompt claims more than it can do: the sandbox and the argv ARE what the \
             requester wrote, checked and re-rendered. {seen}"
        );
        // The id on the screen is the id the answer is matched against, which is what makes
        // approving and reading the same act.
        assert!(prompt(&asked(), what).contains(&asked().operation));
    }

    /// A terminal that has gone refuses. Every failure lands on the same side.
    #[test]
    fn a_surface_that_cannot_be_used_refuses_rather_than_assumes() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("the terminal went away"))
            }
        }
        let console = Console::over(Box::new(Broken), Box::new(std::io::sink()));
        let why = console.approve(&asked(), "`sbx rm -f x`").unwrap_err();
        assert!(why.contains("nothing was approved"), "{why}");
    }

    /// Two prompts do not interleave on one screen.
    ///
    /// Not a race the tests can force reliably, so this asserts the property that makes it
    /// impossible: the seat is behind a lock, and a second caller waits for the first to finish.
    #[test]
    fn one_conversation_at_a_time() {
        let console = std::sync::Arc::new(console("op-7f3a91\nop-7f3a91\n").0);
        let held = console.seat.lock().unwrap();
        let other = std::sync::Arc::clone(&console);
        let waiting = std::thread::spawn(move || other.approve(&asked(), "`sbx create x`"));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !waiting.is_finished(),
            "a second prompt started over the first"
        );
        drop(held);
        assert!(waiting.join().unwrap().is_ok());
    }
}
