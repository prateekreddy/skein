//! Where a box's work actually happens, and the one way to reach it.
//!
//! A box is an identity: a name, a branch, a repo, a conversation. *Where it runs* is a separate
//! thing. skein's original model fused them — one sbx sandbox per box, named after the box — so
//! "the box" and "the sandbox" were the same string in six different helpers, and every feature
//! that touched a box hardcoded that assumption.
//!
//! [`Place`] separates them. `place_of(box)` is a lookup, not an identity, and every call into a
//! box goes through [`Place::exec`] / [`Place::write`] / [`Place::bytes`]. That is the whole point:
//! changing what backs a box — several boxes sharing one sandbox, each with its own HOME, tree and
//! cgroup — became a change to `place_of` rather than a sweep through every feature.
//!
//! There are two shapes, and the address says which one it is rather than skein guessing:
//!
//! - [`Where::Shared`] — a box inside the fleet's sandbox, in its own bwrap namespace. **This is
//!   the only shape a box has**: `place_of` resolves a box name to this or to nothing at all.
//!   Memory is a pool the boxes share instead of N reservations that sum, and `/tmp` and `$HOME`
//!   have to be made private deliberately, because a shared VM does not hand them over.
//! - [`Where::SandboxItself`] — a whole sandbox, addressed as itself: no box inside it to enter,
//!   because the address IS the sandbox. In practice that is the fleet's own sandbox, which is how
//!   [`crate::fleet`] provisions the thing the boxes then live in. It is also the shape skein's
//!   original per-box microVMs had, and the reason [`Place::unreachable_from_fleet`] exists: an
//!   address of this shape naming a sandbox *other than* the one this process stands in is one
//!   that in-fleet skein has no way to reach.
//!
//! ## Layout
//!
//! One file per question, and this one is wiring and the test seam: every name that resolved as
//! `crate::place::X` before the split still does, through the `pub use` of each file below, at the
//! visibility it had. `seam` stays here, where `place::seam` has always been. Tests sit in a
//! `mod tests` beside the code they test.

use crate::config::skein_home;
use crate::config::*;
use crate::util::valid_name;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// **The test seam for fleet-scope execution** — and the shape of it is the whole point (SKEIN-592).
///
/// # Why this exists
///
/// A fixture that wants to stand in for what a fleet-scope script runs used to do it by putting a
/// fake `sbx` on `$PATH`. There is no `sbx` hop — the script runs on this machine — so the fake is
/// bypassed and the *real* command runs. `tests/resize_rules.rs` read this box's live Docker
/// volumes that way and named three belonging to other people's work. That path only read;
/// resize's other arm destroys a sandbox and copies a volume, and the distance between the two is
/// one branch.
///
/// **The reach of that has grown since, which is an argument for this seam rather than against
/// it.** The test above got to the hopless path by declaring the in-fleet deployment; a run that
/// declared nothing took the `sbx` hop and the fake caught it. SKEIN-521 deleted the host-driven
/// alternative, so there is nothing left to declare and no run that hops — every fleet-scope
/// script now runs straight at this machine, and a fixture that forgets this seam reaches the real
/// one by default.
///
/// The `$PATH` route cannot be reopened to fix it. Fleet-scope scripts run under [`Place::shell`]'s
/// **fixed** PATH, which is ISO-1: `~/.local/bin` is bound read-write into every box on a shared
/// uid, so a box that drops a `sudo` there would otherwise have it run at fleet scope. That is a
/// property to keep, not to trade for testability.
///
/// # What makes this safe, stated as a property rather than a hope
///
/// **Nothing a running box can set selects it.** Not an environment variable, not a `$PATH` entry,
/// not a file, not a config key. The substitution is installed by *calling a Rust function in this
/// process*, which is something only this program's own test code can do — and a box is on the
/// other side of a process boundary from all of it. An env-var-driven hook would be ISO-1 deleted
/// and re-spelled under a new name: the property ISO-1 buys is that a box cannot change what a
/// fleet-scope script resolves to, and a hook a box could set is exactly that property gone.
///
/// **And a shipped skein has no seam at all.** The module is behind `debug_assertions`, which
/// `bootstrap.sh` turns off — it builds `--release` (`bootstrap.sh`, `cargo build --release`). In
/// that binary [`taken`] is a function returning `None` with nothing behind it: no static, no lock,
/// no branch on anything.
///
/// [`tests::no_box_can_reach_the_execution_seam_and_a_shipped_skein_has_none`] asserts both halves
/// against the source, because they
/// are properties of what the code *is allowed to contain* rather than of what it computes.
#[cfg(debug_assertions)]
pub mod seam {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Given the argv a fleet-scope command would have run, the argv to run instead — or `None` to
    /// leave it alone. A rewrite rather than a replacement of the whole execution, so the timeout,
    /// the output capture and the exit-code handling stay exactly the ones production uses.
    pub type Substitute = Box<dyn Fn(&[String]) -> Option<Vec<String>> + Send + Sync>;

    static INSTALLED: Mutex<Option<Substitute>> = Mutex::new(None);

    /// Put a substitution in place until the returned guard is dropped.
    ///
    /// A guard rather than a bare `install`/`clear` pair: a test that panics between them would
    /// leave the substitution in place for whatever ran next in the same process, which is the same
    /// shape of cross-test leak as an environment variable nobody put back.
    pub fn install(f: Substitute) -> Installed {
        *INSTALLED.lock().unwrap() = Some(f);
        Installed
    }

    /// The substitution for a test that reaches a crossing only on the way to something else:
    /// every fleet-scope command succeeds, silently, having done nothing.
    ///
    /// One implementation rather than one per suite, because there were about to be two — the lib's
    /// `testutil` and `tests/common/mod.rs` cannot see each other, and a second copy of a rule is
    /// the copy that stops agreeing. It lives beside [`install`] so both can reach it.
    ///
    /// `:` rather than a recording fake: what these tests assert is the DECISION in front of the
    /// crossing — who was asked about, what was spent — and something that succeeds having done
    /// nothing is the smallest thing that lets the decision be reached. A test that asserts on the
    /// argv writes its own substitution and reads it back, which is what `tests/fleet_move.rs` and
    /// `tests/resize_rules.rs` do.
    pub fn doing_nothing() -> Installed {
        install(Box::new(|_argv: &[String]| {
            Some(vec!["sh".to_string(), "-c".into(), ":".into()])
        }))
    }

    /// Removes the substitution on drop.
    pub struct Installed;

    impl Drop for Installed {
        fn drop(&mut self) {
            if let Ok(mut held) = INSTALLED.lock() {
                *held = None;
            }
        }
    }

    /// What production asks: is this argv being stood in for?
    pub fn taken(argv: &[String]) -> Option<Vec<String>> {
        INSTALLED.lock().ok()?.as_ref()?(argv)
    }

    /// Is there a stand-in at all — which is **not** the question [`taken`] answers.
    ///
    /// `taken` returning `None` has two readings and only one of them is a mistake: a substitution
    /// that inspects the argv and hands this one back is a fixture deciding to let it run
    /// (`tests/fleet_move.rs`'s `run` arm does exactly that), while no substitution at all is a
    /// fixture that never thought about it. [`super::Place::spawning`] refuses the second and
    /// allows the first, so it has to be able to tell them apart.
    pub fn installed() -> bool {
        INSTALLED.lock().is_ok_and(|held| held.is_some())
    }

    static REAL: AtomicUsize = AtomicUsize::new(0);

    /// **This process means its fleet-scope commands to run**, so [`super::Place::spawning`] does
    /// not refuse them, until the returned guard is dropped.
    ///
    /// The declared exemption from that guard, and it exists because two shapes carry
    /// `$SKEIN_TEST` and cannot install a substitution:
    ///
    /// * **A skein spawned by a test harness.** `src/bin/skein.rs` and `src/bin/skein-server.rs`
    ///   both say this in `main`. A `skein-server` started by `tests/server.rs` or by
    ///   `tests/ui/harness/server.mjs` inherits the marker from cargo's `[env]` table — correctly,
    ///   because [`crate::config::skein_home`] and [`crate::util::fleet_root`] must still refuse it
    ///   an unpinned path (SKEIN-685) — but a [`Substitute`] is a Rust closure and the test that
    ///   would write one is on the other side of a process boundary. What keeps that server inside
    ///   its fixture is the root it was handed, which is what those two guards are for.
    /// * **A suite whose subject IS the real command.** `tests/fleet_launch.rs` starts a box in a
    ///   fixture fleet and asserts it is usable; standing in for the crossing would delete what it
    ///   proves.
    ///
    /// Said by CALLING SOMETHING rather than by setting a variable, for [`install`]'s reason: a
    /// variable is settable by the very test the guard exists for, and by a box besides.
    pub fn real_crossings() -> Real {
        REAL.fetch_add(1, Ordering::SeqCst);
        Real
    }

    /// Takes the exemption away again on drop.
    pub struct Real;

    impl Drop for Real {
        fn drop(&mut self) {
            REAL.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Has this process declared its crossings real? See [`real_crossings`].
    pub fn meant() -> bool {
        REAL.load(Ordering::SeqCst) > 0
    }
}

/// The seam's absence, in a build that ships. Every call is compiled away.
#[cfg(not(debug_assertions))]
pub mod seam {
    #[inline(always)]
    pub fn taken(_argv: &[String]) -> Option<Vec<String>> {
        None
    }

    #[inline(always)]
    pub fn installed() -> bool {
        false
    }

    /// A shipped skein is never a test harness, so its crossings are real by construction and the
    /// declaration is a value rather than a state. `main` still calls [`real_crossings`], because
    /// one `main` for both builds is the point of compiling this module away rather than the calls.
    #[inline(always)]
    pub fn meant() -> bool {
        true
    }

    #[inline(always)]
    pub fn real_crossings() -> Real {
        Real
    }

    pub struct Real;
}

mod address;
mod argv;
mod crossing;
mod record;
mod run;

pub use address::*;
pub use crossing::*;
pub use record::*;
pub use run::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// **A box cannot reach the execution seam**, and a shipped skein does not have one.
    ///
    /// The seam stands in for what a fleet-scope script runs, so anything that could select it from
    /// outside this process would be ISO-1 deleted and re-spelled under a new name: ISO-1 buys the
    /// property that a box cannot change what a fleet-scope script resolves to, and `~/.local/bin`
    /// is bound read-WRITE into every box on a shared uid. A hook a box could set is that property
    /// gone, with the added insult of being the mechanism that was added to make the tests safe.
    ///
    /// Asserted against the source, because both halves are properties of what the module is
    /// *allowed to contain* rather than of what it computes — the same technique as
    /// `tests/fix_lines.rs` and `neither_lifecycle_route_reaches_its_work_by_a_path_that_skips_the_check`.
    /// A behavioural test cannot cover this: it would have to enumerate the variable names nobody
    /// has thought of yet, which is the wrong quantifier. This one says "consults nothing".
    ///
    /// **What makes this fail**, and it is the obvious change somebody reaches for when a fixture
    /// is awkward to install: making the executor selectable at runtime — an `std::env::var` in the
    /// seam, a path it reads, a config key it consults. Any of those, and the first assertion
    /// fires. Dropping the `debug_assertions` gate fires the second.
    #[test]
    fn no_box_can_reach_the_execution_seam_and_a_shipped_skein_has_none() {
        let source = include_str!("mod.rs");
        // The module as written, from its declaration to the one that replaces it in a release
        // build. Bounded rather than "to the end of the file" so the test module below — which
        // legitimately installs substitutions — is not what gets scanned.
        let start = source
            .find("#[cfg(debug_assertions)]\npub mod seam {")
            .expect("the seam module is gone, or no longer behind `debug_assertions`");
        // To the module's own closing brace — the first `}` at column 0 after it opens — and not
        // to the next thing that looks like a boundary. An earlier version of this ended the span
        // at the release module, and deleting that module silently widened the scan into the test
        // code below, which reads environment variables for its own reasons: the assertion still
        // fired, for entirely the wrong reason, and said so in a message about ISO-1.
        let end = source[start..]
            .find("\n}\n")
            .expect("the seam module has no closing brace at column 0")
            + start;
        let module = &source[start..end];

        // **It consults nothing.** Every way of asking the world what to do, by the spelling this
        // codebase uses for it.
        for reach in [
            "std::env::var",
            "env::var",
            "var_os",
            "read_to_string",
            "File::open",
            "load_config",
            "fleet_sandbox()",
            "PATH",
        ] {
            assert!(
                !module.contains(reach),
                "the execution seam reads `{reach}`, which makes what a fleet-scope script runs \
                 selectable from outside this process — a box writes into `~/.local/bin` on a \
                 shared uid, and ISO-1 exists because of it"
            );
        }

        // **And the release build has no seam.** Found by its own exact declaration rather than by
        // where it happens to sit, so deleting it fails here rather than widening the scan above.
        let ships = source
            .find("#[cfg(not(debug_assertions))]\npub mod seam {")
            .expect("nothing replaces the seam in a release build, so a shipped skein has one");
        let shipped = &source[ships..];
        let shipped = &shipped[..shipped
            .find("\n}\n")
            .expect("the release seam has no closing brace at column 0")];
        assert!(
            !shipped.contains("static") && !shipped.contains("Mutex"),
            "the release build carries the machinery to hold a substitution:\n{shipped}"
        );
        assert!(
            shipped.contains("None"),
            "the release build's seam does not answer `None`, so it stands in for something:\n\
             {shipped}"
        );
    }

    /// The seam actually stands in — otherwise the two assertions above guard nothing.
    ///
    /// Paired with the test above deliberately: "nothing can reach it" is cheap to satisfy by
    /// having it not work at all, and a guard on a mechanism that does nothing is the shape of the
    /// tests this repo has been bitten by. This one drives a real fleet-scope `exec` through the
    /// substitution and reads back what the substitute printed.
    #[test]
    fn the_seam_stands_in_for_what_a_fleet_scope_command_would_have_run() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        std::env::set_var("SKEIN_HOME", dir.join("home"));

        let here = own_sandbox("skein-fleet");
        let installed = seam::install(Box::new(|_argv: &[String]| {
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                "printf %s the-substitute".into(),
            ])
        }));
        let stood_in = here
            .exec("echo the-real-thing", std::time::Duration::from_secs(20))
            .expect("the substitute did not run");
        assert_eq!(stood_in, "the-substitute", "the seam did not stand in");
        drop(installed);

        // And it is gone again once the guard is dropped, which is what stops one test's
        // substitution from being the next test's world.
        //
        // **Asked of the seam rather than by running the command again**, which is what this used
        // to do: it ran `echo the-real-thing` with nothing installed, before and after, and
        // compared the two. Both of those are now refused — `Place::spawning` will not run a
        // fleet-scope command for real in a test process (SKEIN-530) — and refusing them is
        // correct, because "it runs for real when nothing stands in" is a property of PRODUCTION
        // and the two spawns only ever demonstrated it here by being harmless. What the drop has
        // to leave behind is an empty seam, and that is asked directly, in one assertion that
        // cannot pass for having run something innocuous.
        assert!(
            !seam::installed() && seam::taken(&["sh".to_string()]).is_none(),
            "dropping the guard left the substitution in place"
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }
}
