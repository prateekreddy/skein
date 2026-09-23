//! Whether the fleet is up, remembered behind a gate, and what a disturbing act forgets.

use super::*;

/// Micro-cache over the fleet's liveness sweep, for the same reason [`crate::sbx::fleet_boxes`] has one:
/// the board asks per box, and a refresh must not become one `sbx exec` per box per tick. A
/// [`crate::util::Gate`] for the same reason too — see the note there — since this is the `sbx exec` skein
/// runs most often, and the one that kept a slow daemon slow.
pub(super) static LIVENESS_GATE: crate::util::Gate<std::collections::HashMap<String, bool>> =
    crate::util::Gate::new();

/// Forget the remembered sweep, so the next caller waits for the truth instead of being handed the
/// last picture.
///
/// Every act that changes what the sweep would see goes through [`disturbing_liveness`] rather than
/// calling this at the end of its own body — see the note there for why the end of an act turned out
/// not to be one place.
///
/// That serve-stale behaviour is deliberate and worth keeping — it is what stops the board blanking
/// on a slow tick — which is exactly why the caller who knows better has to say so. It is also the
/// one property that couples acting to observing: without it the board shows the value from before
/// the act, which is the thing anybody who just pressed a button is looking straight at.
///
/// Restarting the *agent* inside a box is deliberately not one of these: the tmux server is what
/// liveness reads, and it survives — the box was running before and is running after.
///
/// Public for the case that is outside skein: something *else* stopped a box, so the gate is
/// holding an answer that is not merely stale but wrong.
///
/// The integration test needs it because `cfg!(test)` is false from `tests/`: the library it links
/// was built without it, so the "no gate under test" escape inside this module does not apply there,
/// and one test was being served the previous test's fleet.
pub fn forget_fleet_liveness() {
    LIVENESS_GATE.invalidate();
}

impl Remembered {
    fn forget(self) {
        match self {
            Remembered::BoxLiveness => LIVENESS_GATE.invalidate(),
            // **Both readers of the same fact.** "Which sandboxes exist" has two sources and
            // which one answers depends on where skein is standing: `sbx ls` on a host, the
            // warden's sighting in the fleet, where `sbx ls` cannot be asked at all (SKEIN-576).
            // Settling only the first left the second remembering "no sandboxes" through the
            // freshness window after a create — the same staleness this enum exists to name, at
            // the source that answers in the deployment skein is moving to.
            Remembered::SandboxListing => {
                crate::sbx::forget_fleet_boxes();
                crate::warden_client::forget_sighting();
            }
            Remembered::BoxDisk => DISK_GATE.invalidate(),
            Remembered::FleetResources => RESOURCE_GATE.invalidate(),
        }
    }
}

/// Run an act, and settle everything it makes wrong however it ends.
///
/// The general form of [`disturbing_liveness`], and the reason it is general: two wrappers is the
/// shape that lets a third gate be forgotten. An act says what it disturbs, in one place, and the
/// guard settles all of it on `Drop` — so early return, `?` and a panic are all covered, and adding
/// a branch to an act cannot reintroduce the bug.
pub fn disturbing<T>(what: &[Remembered], act: impl FnOnce() -> T) -> T {
    struct Settle<'a>(&'a [Remembered]);
    impl Drop for Settle<'_> {
        fn drop(&mut self) {
            for remembered in self.0 {
                remembered.forget();
            }
        }
    }
    let _settle = Settle(what);
    act()
}

/// Run an act that changes a box's liveness, and settle the gate however the act ends.
///
/// A wrapper rather than a line at the end of each act, because **the end of an act is not one
/// place**. Every one of the three sites had a return the invalidation sat after: `stop_box` left by
/// two branches and only the shared one said anything, `destroy_box` by three, and `start_box`
/// carries a `?` on nearly every line of its inner half. A guard that settles on `Drop` is passed
/// through by all of them — early return, `?`, and a panic alike — so there is no path left to
/// forget, and adding a fourth branch to any of these acts cannot reintroduce the bug.
///
/// Deliberately settles on failure too. A half-run act is the case where the remembered answer is
/// *most* likely wrong: a start that died after its session came up leaves a box the sweep has never
/// seen, and the gate would keep saying so.
pub fn disturbing_liveness<T>(act: impl FnOnce() -> T) -> T {
    disturbing(&[Remembered::BoxLiveness], act)
}

/// Which boxes in the fleet sandbox have a live session, in one pass over the whole fleet.
///
/// A shared box's liveness *is* its tmux server: box alive ⇔ server alive ⇔ namespace joinable. That
/// question cannot be answered from the host. The anchor pid belongs to the sandbox's pid namespace,
/// so `/proc/<pid>` on the host asks about an unrelated process — and on macOS there is no `/proc`
/// at all, which reported every running box as stopped. Skein runs in the fleet now, where that
/// `/proc` is the local one, so the sweep is a read rather than the `sbx exec` it used to choose
/// between (SKEIN-521, SKEIN-615).
///
/// Every box at once because the board refreshes all of them, and a stopped sandbox answers for none
/// of them: an empty map means "cannot tell", which the caller reports rather than inventing.
pub fn fleet_liveness() -> std::collections::HashMap<String, bool> {
    let sandbox = fleet_sandbox();
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        Duration::from_millis(1500)
    };
    LIVENESS_GATE
        .get(fresh, move || {
            // The anchors skein holds, so the sweep can ask `/proc` about the process it recorded
            // rather than asking each box's socket whether *something* is listening on it.
            let anchors: Vec<(String, u32, String, u64)> = crate::place::placed_boxes(&sandbox)
                .into_iter()
                .map(|(name, record)| (name, record.ns_pid, record.generation, record.ns_start))
                .collect();
            // In-fleet `/proc` and the boxes' sockets are local, so the sweep is a read and a
            // connect rather than an exec (SKEIN-60). Still one pass for the whole fleet: the
            // anchors are gathered once above and the undecided boxes come from one directory
            // listing, so this stays `Scale::PerPass`.
            Some(crate::place::local_liveness(&fleet_root(), &anchors))
        })
        .unwrap_or_default()
}
