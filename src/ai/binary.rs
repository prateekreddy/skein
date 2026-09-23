//! Which agent binary is spawned, whether anybody chose it, and the test seam that says so
//! for this process alone.

use super::*;

/// The agent CLI skein spawns — `$SKEIN_CLAUDE_BIN`, else `claude` on the path.
///
/// Its own function because two things ask it now: the call itself, and [`model_choices`], which
/// asks that same binary what models it will take. A second copy of this would be a second answer
/// to "which claude", and the whole point of the override is that there is one.
///
/// The refusal that belongs with this is at the SPAWN, in [`agent_command`], and not here. See
/// there for why — the short version is that naming the binary and running it are done in different
/// places, and only one of them costs anything.
pub(crate) fn claude_bin() -> String {
    env::var("SKEIN_CLAUDE_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_AGENT_BIN.into())
}

/// What `claude_bin` answers when nobody has said otherwise: a bare name, resolved on `$PATH`.
pub(crate) const DEFAULT_AGENT_BIN: &str = "claude";

/// Did somebody CHOOSE the agent binary, or is [`claude_bin`] falling back?
fn agent_bin_named() -> bool {
    env::var_os("SKEIN_CLAUDE_BIN").is_some_and(|v| !v.is_empty())
}

/// **Which binary the LOCAL arm spawns, said by this process and by nothing else** (SKEIN-799).
///
/// It exists to break an exclusion that made one arm of [`claude_in_turn`] untestable, and the
/// exclusion is exact rather than awkward. `$SKEIN_CLAUDE_BIN` carries two meanings at once —
/// *which* binary ([`claude_bin`]) and *therefore here* (`claude_in_turn`'s `named`) — and
/// [`agent_command`]'s guard keys on [`agent_bin_named`], the same variable read the same way. So
/// for any call in a test process:
///
/// * the box branch is reached only while the variable is UNSET, and
/// * the fall-through to [`tried`] survives only while it is SET.
///
/// Which means no test could ever complete the fall-through — the arm where a box is lost and the
/// reading succeeds anyway, which is the whole of SKEIN-799. Probed before this was written: the
/// call prints its one stderr line and then panics in `agent_command`.
///
/// This says the first half without the second. It is consulted at the SPAWN, which is already
/// inside the local arm, so it cannot mean "run here" — by the time it is read, here is where the
/// call is. That is [`crate::place::seam`]'s argument for being a compile-time substitution rather
/// than a `$PATH` entry or a variable, made about the other destination: a variable is settable by
/// the very test the guard exists for.
///
/// **It cannot be used to spawn the real agent**, which would make it a hole in the guard rather
/// than a seam beside it: [`stand_in`] refuses [`DEFAULT_AGENT_BIN`] itself, so the one thing
/// `agent_command` is there to prevent is the one thing this cannot ask for.
#[cfg(debug_assertions)]
pub mod seam {
    use std::sync::Mutex;

    static INSTALLED: Mutex<Option<String>> = Mutex::new(None);

    /// Spawn `path` instead of the agent, until the returned guard is dropped.
    ///
    /// A guard rather than a set/clear pair, for [`crate::place::seam::install`]'s reason: a test
    /// that panics between them leaves the substitution in place for whatever runs next in the
    /// same process.
    pub fn stand_in(path: impl Into<String>) -> Installed {
        let path = path.into();
        assert!(
            path != super::DEFAULT_AGENT_BIN,
            "`{path}` is the agent itself, and standing it in for itself would spawn the owner's \
             real CLI against their real login — which is the one thing `ai::agent_command` \
             exists to refuse. Name a stub: `testutil::write_claude_stub` writes one."
        );
        *INSTALLED.lock().unwrap() = Some(path);
        Installed
    }

    /// Takes the substitution away again on drop.
    pub struct Installed;

    impl Drop for Installed {
        fn drop(&mut self) {
            if let Ok(mut held) = INSTALLED.lock() {
                *held = None;
            }
        }
    }

    /// What production asks: is this process spawning something else instead?
    pub fn taken() -> Option<String> {
        INSTALLED.lock().ok()?.clone()
    }
}

/// The seam's absence, in a build that ships. Every call is compiled away — the same shape
/// [`crate::place::seam`] has, for the same reason.
#[cfg(not(debug_assertions))]
pub mod seam {
    #[inline(always)]
    pub fn taken() -> Option<String> {
        None
    }
}

/// The agent CLI, about to be spawned **in this process** — and a test process that never said
/// which binary to run is refused rather than handed `claude` off `$PATH` (SKEIN-764).
///
/// The same rule [`crate::util::fleet_root`], [`crate::config::skein_home`] and
/// [`crate::warden_client::Warden::send_within`] hold for the fleet, the home and the warden. What
/// it costs when it is missing is unlike all three: those are the owner's *state*, and this is the
/// owner's *money*. A test that reaches a model call with nothing pinned runs the real agent
/// against the real login and is billed for it, and the only trace it leaves in the run is that it
/// took longer.
///
/// **The guard is here rather than in [`claude_bin`], for [`crate::warden_client::Warden`]'s
/// reason.** That module puts its refusal at `send_within`, which opens a connection, and not at
/// `configured`, which computes an address — because computing one is harmless and a guard on it
/// would refuse the test that asserts what the default address *is*. The same split is real here
/// and is not hypothetical: `ai::tests::a_failure_names_the_program_that_failed` **has** to run with
/// `$SKEIN_CLAUDE_BIN` unset, because [`claude_in_turn`] reads that variable to decide whether the
/// call goes into the box, and a pinned one means "run exactly this, here" and skips the crossing
/// the test is about. It names `claude` four times and spawns it never — every arm stands the
/// crossing in through [`crate::place::seam`].
///
/// It keys on `bin == `[`DEFAULT_AGENT_BIN`]` && !`[`agent_bin_named`]`()`, which is the warden's
/// `defaulted` re-derived at the spawn rather than carried on a struct: the eight tests that call
/// [`tried`] with a stub path of their own did the right thing by a different route, and asking
/// only "is the variable set" would refuse every one of them for it.
///
/// **And the arm this catches is one that has already fired.** `a_failure_names_the_program_that_
/// failed`'s own note records it: driving the unreachable-crossing case through [`claude_in_turn`]
/// rather than through [`crate::fleet::model_call_in_box`] falls through to [`tried`], "and this
/// one did, once, before that was noticed". The fall-through is still there in production, which is
/// correct — a box that cannot be reached is not an error — so nothing but a check at the spawn can
/// tell that reading apart from a real one.
pub(super) fn agent_command(bin: &str) -> Command {
    // **In front of the guard, not behind it.** A stand-in is this process saying which binary the
    // local spawn runs, which is exactly what the guard is asking for — so a call that has one has
    // already answered, and reaching the assertion below would refuse a test for not saying
    // something it said by another route. `seam::stand_in` refuses the agent's own name, so this
    // can never be the spawn the guard is about. See [`seam`].
    if let Some(instead) = seam::taken() {
        return Command::new(instead);
    }
    assert!(
        !(crate::util::in_test() && bin == DEFAULT_AGENT_BIN && !agent_bin_named()),
        "$SKEIN_CLAUDE_BIN is unset in a test process (${marker}), and skein is about to spawn \
         `{DEFAULT_AGENT_BIN}` off $PATH. Refusing: that is the real agent CLI against the owner's \
         real login, so a test that gets here SPENDS REAL MONEY and looks exactly like one that \
         did not, apart from taking longer. Point the variable at a stub — \
         `testutil::write_claude_stub` writes one that answers the prompts skein sends — or at \
         `/bin/false` if the call is not what is being asserted. If the crossing into a box is the \
         subject and the variable must stay unset (see \
         `ai::tests::a_failure_names_the_program_that_failed`), stand the crossing in with \
         `place::seam::install` so this local fall-through is never reached.",
        marker = crate::util::TEST_MARKER,
    );
    Command::new(bin)
}

/// Which binary and which model this call will use, after both override layers.
pub(super) fn binary_and_model(model: Option<&str>) -> (String, String) {
    let bin = claude_bin();
    let model = env::var("SKEIN_AI_MODEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| model.unwrap_or("claude-haiku-4-5").to_string());
    (bin, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// A test process that never said which agent binary to run does not get `claude` off `$PATH`
    /// (SKEIN-764).
    ///
    /// The other three guards in this family protect the owner's *state*; this one protects the
    /// owner's *money*. A test that reaches [`tried`] with nothing pinned spawns the real CLI
    /// against the real login and is billed for it, and the only trace it leaves in the run is that
    /// it took longer than its neighbours.
    ///
    /// **Both directions in one test.** A binary the caller NAMED has to be attempted and the
    /// unnamed default refused: a check that only exercised the refusal would pass with
    /// [`agent_command`] reduced to `Command::new(bin)` *and* with it reduced to an unconditional
    /// panic, and the second would break the eight tests that reach `tried` with a stub path of
    /// their own — which is why the guard keys on [`DEFAULT_AGENT_BIN`] and not on the variable
    /// alone.
    ///
    /// **What makes it fail:** deleting the `assert!` from [`agent_command`]. The `catch_unwind`
    /// below then comes back `Ok` — with `claude` actually spawned on the machine running the
    /// suite, which is the behaviour this exists to stop.
    #[cfg(unix)]
    #[test]
    fn spawning_the_agent_refuses_a_binary_nobody_chose() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // `tried` resolves the home for the credential and the scratch directory, and
        // `config::skein_home` refuses an unpinned test rather than answering with the real one.
        env::set_var("SKEIN_HOME", &dir);
        let was = env::var_os("SKEIN_CLAUDE_BIN");
        env::remove_var("SKEIN_CLAUDE_BIN");
        let quick = Duration::from_secs(5);

        // Named by the caller: attempted, and reported on its own terms. `Missing` is the answer
        // for a name that is not on `$PATH`, which is exactly what proves the spawn was tried.
        forget_refusal();
        let named = tried("skein-no-such-binary", "m", "hi", quick, Turn::Alone, None);
        assert!(
            matches!(named, Err(Unread::Missing { .. })),
            "a binary the caller named was not even attempted, so the refusal below is the only \
             behaviour this path has left: {named:?}"
        );

        // Nobody's choice: refused before the spawn.
        forget_refusal();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let answered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tried(DEFAULT_AGENT_BIN, "m", "hi", quick, Turn::Alone, None)
        }));
        std::panic::set_hook(hook);
        forget_refusal();
        if let Some(v) = was {
            env::set_var("SKEIN_CLAUDE_BIN", v);
        }
        env::remove_var("SKEIN_HOME");

        let said = match answered {
            Ok(outcome) => panic!(
                "the default agent binary was spawned in a test process instead of being refused: \
                 {outcome:?}"
            ),
            Err(e) => e
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "<not a string>".into()),
        };
        assert!(
            said.contains("SKEIN_CLAUDE_BIN"),
            "the refusal has to name the variable to set, or it tells a contributor nothing: {said}"
        );
    }
}
