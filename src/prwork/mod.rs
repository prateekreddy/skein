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
/// `$SKEIN_PR_WORKFLOWS=on|off` overrides, so it can be turned off from the command line that
/// starts the server — no cockpit, no config edit, on a fleet that is doing something you want
/// stopped now.
pub fn enabled() -> bool {
    match std::env::var("SKEIN_PR_WORKFLOWS").ok().as_deref() {
        Some("on" | "1" | "true" | "yes") => true,
        Some("off" | "0" | "false" | "no") => false,
        _ => crate::config::load_config().pr_workflows,
    }
}
