//! One at a time, and not too many — the residual risk §8 leaves (§8.5).
//!
//! Every individual operation is confirmed by a person at the host, so the attack that survives is
//! **volume**. A compromised skein controls *what* is proposed and *when*: enough prompts that
//! somebody approves the wrong one, or so many that the real one is buried in them. §8.5 names three
//! parts and this is all three.
//!
//! **One outstanding request at a time.** A second is *refused, naming the first* — not queued and
//! not dropped. Queueing is the flood with extra steps; dropping silently leaves a caller unable to
//! tell "busy" from "lost", which is exactly the ambiguity §8.2 exists to remove. Naming the
//! outstanding operation is what lets the caller ask about *that* one instead.
//!
//! **A rate limit**, in requests per minute — §10's units, where the budget with teeth is expressed
//! per unit of wall-clock rather than per unit of work. The number is low on purpose: these are
//! operations a person confirms one at a time, so anything a human can keep up with is above it.
//!
//! **The read endpoint is exempt, and that is the subtle part.** Fleet observation reads and decides
//! nothing, and it is the check that gates skein's own first run (§8.3). Rate-limiting it into
//! unavailability would turn a safety measure into the thing that stops skein starting — the flood
//! would have achieved by refusal what it could not achieve by approval. So the gate here is applied
//! to the doers and to nothing else, and a test floods the doers and then reads.
//!
//! The **timeout** §11.5 names is the approval surface's, not this module's: a prompt nobody answers
//! holds the one outstanding slot for ever, which turns "one at a time" into "one, ever". It is not
//! built yet, and the seat lock in `approval.rs` is the thing that would carry it.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How many doer requests a minute, whatever their outcome.
///
/// Counted on *arrival*, not on approval: a flood of refused proposals is the attack, so counting
/// only the ones that got through would count nothing while it happened.
pub const PER_MINUTE: usize = 12;

/// Why a request was not let through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Something is already in front of a person. The id is theirs to ask about.
    Outstanding { operation: String },
    /// Too many, too fast.
    TooMany { per_minute: usize },
}

impl Refused {
    /// What the caller is told. Says what to do next, because "slow down" without an alternative is
    /// how a client ends up retrying in a loop.
    pub fn why(&self) -> String {
        match self {
            Refused::Outstanding { operation } => format!(
                "another operation is already waiting for a person at the host: {operation}. One at \
                 a time, deliberately (architecture §8.5) — ask about that one, or wait for it."
            ),
            Refused::TooMany { per_minute } => format!(
                "more than {per_minute} operations were proposed in a minute, which is more than a \
                 person can confirm one at a time (architecture §8.5). Nothing was run."
            ),
        }
    }
}

/// The doorway. Doers pass through it; the reporting endpoints do not.
#[derive(Debug, Default)]
pub struct Doorway {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// The one operation currently in front of a person.
    holding: Option<String>,
    /// When recent requests arrived, oldest first.
    arrivals: VecDeque<Instant>,
}

/// Held for as long as an operation is in front of a person, and releases on drop.
///
/// A guard rather than a pair of calls, because every path out of a doer — the approval refusing, a
/// panic, a `?` on the command — has to release the slot. Forgetting one turns "one at a time" into
/// "one, ever", and the failure would look like a warden that had simply stopped answering.
#[derive(Debug)]
pub struct Turn<'a> {
    doorway: &'a Doorway,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let mut inner = self.doorway.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.holding = None;
    }
}

impl Doorway {
    pub fn new() -> Doorway {
        Doorway::default()
    }

    /// Take the slot for `operation`, or say why not.
    pub fn enter(&self, operation: &str) -> Result<Turn<'_>, Refused> {
        self.enter_at(operation, Instant::now())
    }

    /// The same, with the clock passed in — so the rate limit is tested by moving time rather than
    /// by sleeping through it.
    pub fn enter_at(&self, operation: &str, now: Instant) -> Result<Turn<'_>, Refused> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(held) = &inner.holding {
            return Err(Refused::Outstanding {
                operation: held.clone(),
            });
        }
        // Counted on arrival and before the slot is taken, so a flood is measured even though every
        // request after the first is refused for the other reason.
        let minute = Duration::from_secs(60);
        while inner
            .arrivals
            .front()
            .is_some_and(|at| now.duration_since(*at) >= minute)
        {
            inner.arrivals.pop_front();
        }
        inner.arrivals.push_back(now);
        if inner.arrivals.len() > PER_MINUTE {
            return Err(Refused::TooMany {
                per_minute: PER_MINUTE,
            });
        }
        inner.holding = Some(operation.to_string());
        Ok(Turn { doorway: self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second request is told about the first, by name.
    #[test]
    fn a_second_request_is_refused_and_names_the_one_in_front_of_the_person() {
        let door = Doorway::new();
        let first = door.enter("op-1").expect("the first one goes through");
        match door.enter("op-2") {
            Err(Refused::Outstanding { operation }) => {
                assert_eq!(
                    operation, "op-1",
                    "the caller must be told which one to ask about"
                );
                let why = Refused::Outstanding { operation }.why();
                assert!(
                    why.contains("op-1") && why.contains("One at a time"),
                    "{why}"
                );
            }
            other => panic!("a second operation was queued or let through: {other:?}"),
        }
        // And the slot is released on drop, on every path out — including one that never returned.
        drop(first);
        assert!(door.enter("op-2").is_ok(), "the slot was never given back");
    }

    /// The guard releases even when the work panics, which is the path a pair of calls would miss.
    #[test]
    fn a_panic_inside_a_turn_still_gives_the_slot_back() {
        let door = Doorway::new();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _turn = door.enter("op-boom").unwrap();
            panic!("the doer fell over");
        }));
        assert!(
            door.enter("op-next").is_ok(),
            "a panic left the warden holding a slot for ever"
        );
    }

    /// Counted on arrival, so a flood of refusals is still a flood.
    #[test]
    fn too_many_in_a_minute_is_refused_even_when_none_of_them_ran() {
        let door = Doorway::new();
        let start = Instant::now();
        for n in 0..PER_MINUTE {
            // Each one taken and released at once, so the limit is what refuses rather than the slot.
            drop(
                door.enter_at(&format!("op-{n}"), start)
                    .expect("within the limit"),
            );
        }
        match door.enter_at("op-over", start) {
            Err(Refused::TooMany { per_minute }) => assert_eq!(per_minute, PER_MINUTE),
            other => panic!(
                "the {}th request in a minute went through: {other:?}",
                PER_MINUTE + 1
            ),
        }
        // A minute later the window has moved, and the same caller is welcome again.
        assert!(door
            .enter_at("op-later", start + Duration::from_secs(61))
            .is_ok());
    }
}
