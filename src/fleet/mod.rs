//! The one sandbox that hosts many boxes.
//!
//! An sbx sandbox is a microVM, and its memory is a *reservation*: skein's original model gives each
//! box one, so five boxes reserve five times the peak even though four of them are idle in an editor.
//! On a 36 GB machine that is what makes the host start refusing work. The boxes' needs are spiky and
//! rarely simultaneous — a build wants 5 GB for two minutes, editing wants almost nothing — so the
//! fix is to make memory a pool they share rather than N ceilings that sum.
//!
//! So: one sandbox, many boxes, each in its own bwrap namespace with its own `/tmp`, `$HOME` and
//! checkout (see `box-session.sh`). Inside a VM, cgroup limits are *ceilings* rather than
//! reservations — a box capped at 8 GB that uses 200 MB costs 200 MB — which is the whole reason
//! this shape wins.
//!
//! **A fleet always has a name, and nothing in this module checks for one** (SKEIN-756). This note
//! used to read "everything here is inert until [`crate::place::fleet_sandbox`] names a sandbox",
//! and seventeen functions below opened with an `if sandbox.is_empty()` that acted on it. None of
//! them could be taken: [`crate::config::load_config`] repairs a blank or whitespace-only
//! `fleet_sandbox` to the default before anybody reads it, so [`crate::place::fleet_sandbox`] —
//! which is that field, trimmed — cannot answer empty. `config.rs` states the repair, and
//! [`crate::config::tests`] and [`tests::a_fleet_always_has_a_name_so_this_module_need_not_ask`]
//! assert it from the two ends.
//!
//! Six of those seventeen returned the same four words, *no fleet sandbox configured*, which
//! `docs/recovery-survey.md` ranked the second-worst message in the tree — a refusal that reads as
//! something a person can act on and that no person can reach. **One statement of an invariant is a
//! tripwire; seventeen are a fiction with a maintenance cost.** The one that stays is `skein
//! doctor`'s (`src/bin/skein.rs`), whose whole job is reporting on invariants and which says so in
//! those words.
//!
//! Boxes already running as their own VM keep running that way: [`crate::place::place_of`] follows
//! a box's own record, so turning this on never retroactively reinterprets one.
//!
//! ## Layout
//!
//! One file per question, and this one is wiring: every name that resolved as `crate::fleet::X`
//! before the split still does, through the `pub use` of each file below, at the visibility it
//! had. Tests sit in a `mod tests` beside the code they test; helpers two files share are in
//! `testkit.rs`.
//!
//! `fleet_root` alone is named rather than globbed: `crate::util::*` exports one too, and the
//! definition in `paths.rs` shadowed it when both were in one file. It still does.

use crate::config::skein_home;
use crate::config::*;
use crate::kit::KIT_STARTUP_SH;
use crate::place::{anchor_probe, parse_anchor_probe};
use crate::place::{
    own_sandbox, place_of, placed_boxes, record_place, shared_record, Place, PlaceRecord,
};
use crate::repos::agent_for_box;
use crate::repos::{
    branch_of, is_ssh_url, launch_spec, load_repos, repo_for_box, repo_origin_url,
    write_launch_spec_for_agent, Repo,
};
use crate::util::valid_name;
use crate::util::*;
use chrono::Utc;
use std::time::Duration;

mod checkout;
mod containers;
mod create;
mod credentials;
mod declared;
mod disk;
mod fleetlogin;
mod heal;
mod hosts;
mod install;
mod kit;
mod launcher;
mod limits;
mod liveness;
mod login;
mod model;
mod paths;
mod resize;
mod resources;
mod server;
mod snapshot;
mod start;
mod stop;
mod substrate;
#[cfg(test)]
mod testkit;
mod transcript;
mod unowned;

pub use checkout::*;
pub use containers::*;
pub use create::*;
use credentials::*;
pub use declared::*;
pub use disk::*;
pub use fleetlogin::*;
pub use heal::*;
pub use hosts::*;
pub use install::*;
pub use kit::*;
pub use launcher::*;
pub use limits::*;
pub use liveness::*;
pub use login::*;
pub use model::*;
pub use paths::fleet_root;
pub use paths::*;
pub use resize::*;
pub use resources::*;
pub use server::*;
pub use snapshot::*;
pub use start::*;
pub use stop::*;
pub use substrate::*;
pub use transcript::*;
pub use unowned::*;

/// What a sandbox run reports. Re-exported so a caller that only wants to run something in the
/// fleet names `fleet` alone — the type is `place`'s and reaching for it directly would be a second
/// dependency for one struct.
pub use crate::place::{fleet_sandbox, Ran};

/// A remembered answer an act can make wrong — the names live in [`crate::signal`], the gates here.
///
/// Split that way because they are two different facts. *Which* remembered answers exist is part of
/// the signal primitive's declaration (§2.2), and an Act declares what it disturbs without having to
/// know that a gate exists at all; *where* each one is kept is this module's business, and nothing
/// else's.
pub use crate::signal::Remembered;

#[cfg(test)]
mod tests {
    /// **The invariant the seventeen deleted guards were checking, asserted once from this side**
    /// (SKEIN-756).
    ///
    /// Every function in this module used to open with `if sandbox.is_empty()`. None of those
    /// branches could be taken: [`crate::config::load_config`] repairs a blank or whitespace-only
    /// `fleet_sandbox` to the default before any caller sees it, and
    /// [`crate::place::fleet_sandbox`] is that field trimmed. Deleting seventeen dead refusals is
    /// only sound while that holds, so it is asserted here rather than assumed — `config.rs` has
    /// its own test of the repair, and this is the one that says the *caller* gets it.
    ///
    /// **What would make this fail**: removing the repair from `config::load_config`. Seen to fail
    /// before it was believed — with those three lines deleted, the `""` and `"   "` cases fail
    /// here, while `{}` still passes on the serde default alone, which is why all three are here.
    ///
    /// The last case is the non-vacuity: a name somebody chose comes back unchanged, so a
    /// `fleet_sandbox` that ignored the file and answered the default would not pass this.
    #[test]
    fn a_fleet_always_has_a_name_so_this_module_need_not_ask() {
        let _lock = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let home = (dir.as_ref() as &std::path::Path).join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Both roots pinned. `fleet_sandbox` reads only `$SKEIN_HOME` — but an unpinned
        // `$SKEIN_FLEET_ROOT` answers `/boxes`, which on this machine is the owner's live fleet,
        // and a test that leaves it unpinned is the shape SKEIN-530 was.
        let mut pins = crate::testutil::env_pins();
        pins.set(
            "SKEIN_FLEET_ROOT",
            (dir.as_ref() as &std::path::Path).join("fleet"),
        );
        pins.set("SKEIN_HOME", &home);
        let config = home.join("config.json");
        assert!(
            !crate::place::fleet_sandbox().is_empty(),
            "with no config.json at all the fleet came back unnamed"
        );
        for written in [
            r#"{}"#,
            r#"{"fleet_sandbox":""}"#,
            r#"{"fleet_sandbox":"   "}"#,
        ] {
            std::fs::write(&config, written).unwrap();
            assert!(
                !crate::place::fleet_sandbox().is_empty(),
                "a config written as {written} left the fleet unnamed, and the seventeen refusals \
                 that used to catch that are gone"
            );
        }
        std::fs::write(&config, r#"{"fleet_sandbox":"probe-fleet"}"#).unwrap();
        assert_eq!(crate::place::fleet_sandbox(), "probe-fleet");
    }
}
