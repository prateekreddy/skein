//! Doing what a workflow decided — the half with consequences.
//!
//! [`crate::workflow`] is a vocabulary and a decision and has no way to touch anything. This is the
//! part that merges pull requests and deletes branches on its own, which is what the owner asked
//! for: *"merge automatically and delete the branch"*, reaffirmed after the risk was put to them.
//!
//! Three things make that recoverable rather than a decision nobody can see, and they exist BEFORE
//! any action can fire rather than after the first surprise.
//!
//! # The switch
//!
//! [`enabled`] is off by default and one key turns every workflow in the fleet off, reachable
//! without the cockpit. Not a per-workflow flag: the moment somebody wants this stopped, they want
//! it stopped, and hunting through several places for the one that is still on is not a thing to
//! ask of a person who has just watched something merge.
//!
//! # The record
//!
//! Every action reaches the host's audit log — the one skein does not own, through the warden, the
//! same sink a box's own lifecycle events use. With the authority on it: which workflow, which
//! step. "Skein merged #41" is a fact; "*ship-mine*, step 4, merged #41" is one somebody can act
//! on, because it says which line to change so it does not happen again.
//!
//! # No blind retries
//!
//! An action that fails because the world moved is not a transient error. A merge that 409s because
//! somebody pushed while skein was deciding must not be attempted again on the next poll: the same
//! decision was made from facts that are now provably stale, and a loop that re-tries it is a loop
//! that eventually wins the race. So a failure STOPS that pull request's workflow, in writing, with
//! the reason — and it stays stopped until a person clears it.
//!
//! That is deliberately stronger than "retry a few times". The actions here are outward-facing and
//! most are hard to undo; the cost of stopping too eagerly is that somebody presses a button, and
//! the cost of retrying too eagerly is a merge nobody asked for.

mod acts;
mod facts;
mod perform;
mod rows;
mod sweep;
#[cfg(test)]
mod testkit;

// Every name this module had before it became a directory, at the path it had. The submodules are
// private and the re-exports are globs on purpose: the split moved where the text lives, not what
// anything outside can reach, and a hand-written list of names is a second place for the two to
// disagree. `crate::prwork::perform` still resolves, and so does every other item — which is what
// keeps `src/bin/skein-server/`, this module's heaviest caller, out of the diff entirely.
pub use acts::*;
pub use facts::*;
pub use perform::*;
pub use rows::*;
pub use sweep::*;

/// May skein act on pull requests at all?
///
/// **Off unless it is switched on.** Every other default in skein leans toward showing you more;
/// this one leans the other way, because the thing being defaulted is not a reading but a merge.
/// `$SKEIN_PR_WORKFLOWS=off` holds it off from the command line that starts the server — no
/// cockpit, no config edit, on a fleet that is doing something you want stopped now. It can only
/// hold it off: `=on` does not beat the pause button ([`crate::config::env_holds_off`]).
pub fn enabled() -> bool {
    crate::config::load_config().pr_workflows && !crate::config::env_holds_off("SKEIN_PR_WORKFLOWS")
}

#[cfg(test)]
mod switch_tests {
    /// **`$SKEIN_PR_WORKFLOWS=on` does not beat the pause button** (the owner, 2026-09-27: an
    /// environment variable may only turn things off). The pause posts `{pr_workflows: false}`,
    /// which the Settings route merges into `config.json` with `update_config`; this does the same
    /// and reads back what `GET /api/workflows` answers, `enabled()`.
    ///
    /// What would make it fail: `enabled()` reading `=on` as a yes again, which is the bug — the
    /// button wrote false, the env read back true, the toast said "workflows resumed" and merges
    /// carried on. And `held_by_env` naming the variable while it holds nothing, or not naming it
    /// once `=off` does.
    #[test]
    fn a_yes_in_the_environment_does_not_beat_the_pause() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home).set("SKEIN_PR_WORKFLOWS", "on");
        crate::testutil::switch_on(|c| c.pr_workflows = true);
        assert!(
            super::enabled(),
            "switched on in Settings, and nothing holds it off"
        );

        // The pause, as the review panel sends it.
        crate::config::update_config(|c| {
            c.pr_workflows = false;
            Ok(())
        })
        .unwrap();
        assert!(
            !super::enabled(),
            "$SKEIN_PR_WORKFLOWS=on beat the pause: the person switched workflows off and they run"
        );
        assert_eq!(
            crate::config::held_by_env().get("pr_workflows"),
            None,
            "a yes holds nothing, so Settings must not say it is held"
        );

        // And the one thing the variable is for: holding a switched-on fleet off.
        crate::testutil::switch_on(|c| c.pr_workflows = true);
        env.set("SKEIN_PR_WORKFLOWS", "off");
        assert!(
            !super::enabled(),
            "$SKEIN_PR_WORKFLOWS=off no longer stops the fleet"
        );
        assert_eq!(
            crate::config::held_by_env().get("pr_workflows"),
            Some(&"SKEIN_PR_WORKFLOWS"),
            "held off by the environment, and Settings is not told which variable"
        );
    }
}
