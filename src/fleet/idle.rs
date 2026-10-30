//! Is a box idle: the one reading every "only when it is idle" decision asks (SKEIN-1372).
//!
//! **Idle** means between turns with nothing waiting: the agent's turn has ended, no question is
//! open to the person, and no subagent is still running. Today skein can only see that through the
//! board's fused turn state, which reads the box's hooks and its screen (`docs/inventory.md` §9.1):
//! `waiting` is "the turn ended and the agent is at its prompt", and every other state is not idle.
//! That is the fallback, and for Codex and for an older Claude Code it stays the only reading.
//!
//! **The exact reading** is [`ExactTurn`]: the three facts themselves, as the agent's own engine
//! knows them. skein's plugin becoming a Claude Code mod (SKEIN-1358) is what can supply it. When it
//! is given, it wins outright; when it is not, [`idle`] falls back to the fused state. One function,
//! so the rule cannot be spelled two ways by two callers.

/// The three facts that make a box idle, as an agent's own engine reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExactTurn {
    /// The last turn has ended.
    pub turn_complete: bool,
    /// Questions open to the person: a permission prompt, a question tool.
    pub open_asks: u32,
    /// Subagents still running.
    pub running_subagents: u32,
}

/// The answer, with the reason when it is not idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Idle {
    Idle,
    Busy(String),
    /// skein cannot tell. Never read as idle.
    Unknown,
}

/// **The one reader.** `exact` when the box's engine supplied it; otherwise `fused`, the board's
/// turn state for the box.
pub fn idle(exact: Option<&ExactTurn>, fused: &str) -> Idle {
    if let Some(e) = exact {
        return if !e.turn_complete {
            Idle::Busy("turn".into())
        } else if e.open_asks > 0 {
            Idle::Busy("asking".into())
        } else if e.running_subagents > 0 {
            Idle::Busy("subagents".into())
        } else {
            Idle::Idle
        };
    }
    match fused {
        "waiting" => Idle::Idle,
        "" | "unknown" | "stale" => Idle::Unknown,
        other => Idle::Busy(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Only `waiting` is idle by the fallback, and the exact reading wins when it is given.**
    ///
    /// What would make it fail: reading anything but `waiting` as idle (`needs-input` is a person
    /// being asked); reading an unknown state as idle; the fused state overriding an exact reading
    /// that says a subagent still runs.
    #[test]
    fn idle_is_a_finished_turn_with_nothing_open_and_nothing_running() {
        assert_eq!(idle(None, "waiting"), Idle::Idle);
        for busy in ["working", "needs-input", "compacting"] {
            assert_eq!(idle(None, busy), Idle::Busy(busy.into()), "{busy}");
        }
        for unknown in ["", "unknown", "stale"] {
            assert_eq!(idle(None, unknown), Idle::Unknown, "{unknown:?}");
        }
        let done = ExactTurn {
            turn_complete: true,
            ..Default::default()
        };
        assert_eq!(idle(Some(&done), "working"), Idle::Idle);
        let sub = ExactTurn {
            running_subagents: 1,
            ..done
        };
        assert_eq!(idle(Some(&sub), "waiting"), Idle::Busy("subagents".into()));
        let ask = ExactTurn {
            open_asks: 1,
            ..done
        };
        assert_eq!(idle(Some(&ask), "waiting"), Idle::Busy("asking".into()));
    }
}
