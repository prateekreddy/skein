//! `heal_fleet`: the repairs a running fleet gets on every tick.

use super::*;

/// Bring a fleet that already exists into line with the skein that has just started.
///
/// A fleet sandbox is long-lived and skein is not: the sandbox keeps the launcher and the cgroup
/// ceilings it was last given, and nothing about restarting the server replaced either. So an
/// upgrade landed in a state where the *host* had one skein and the *sandbox* had the last one's
/// idea of how to start a box — and where the two disagree about the spec they pass between them,
/// every box in that fleet stops starting until something reinstalls the launcher. That happened:
/// a launcher that could not read `docker=max/max` exited before tmux, so each reconnect found no
/// session, and the stale anchor pid it then entered read as `nsenter: cannot open
/// /proc/<pid>/ns/user` — an error about namespaces, for a fleet that needed a file copied.
///
/// Repairing it on server start rather than on the settings save that changed the number, because
/// the mismatch is not caused by a setting: it is caused by *this binary* being newer than the copy
/// out there, which is exactly what a restart means and nothing else observes.
///
/// Both halves are idempotent — the launcher is written whole, and the ceilings are values, not
/// deltas — so a fleet that was already current pays two `sbx exec` calls and changes nothing.
///
/// **Only a sandbox already awake.** Waking one costs a VM boot, and starting the cockpit is not a
/// request to run the fleet — `sbx ls` is asked instead of the sandbox itself, so a sleeping fleet
/// is left asleep. It is not left stale either: [`ensure_box_session`] reinstalls the launcher on
/// the path that wakes it, so the repair happens when the fleet is next actually used.
///
/// A fleet that is asleep and a fleet that could not be *asked* are handled the same way and said
/// differently. The second is reported, because everything below this point is skipped on a
/// question that timed out, and a skipped repair that says nothing is indistinguishable from one
/// that succeeded.
pub fn heal_fleet() -> Result<(), String> {
    let sandbox = fleet_sandbox();
    // "Asleep" and "sbx did not answer" both mean *don't touch it*, and they used to be the same
    // branch. They are not the same thing to say. Leaving a sleeping fleet asleep is the intent
    // above; a fleet skein could not *see* is a repair that quietly did not happen — and this gate
    // stands in front of the launcher, the agent and the docker config alike, so a daemon too busy
    // to answer within `fleet_boxes`'s budget skips all three and reports nothing. That is the same
    // stall the in-sandbox agent exists to survive, deciding whether the agent gets installed.
    // **In-fleet the question does not arise, and asking it skipped every repair.** `sbx ls` asks
    // about the HOST's machine, which an in-fleet process cannot reach, so `fleet_boxes` returns
    // `None` with a reason (`sbx.rs:92-105`) — and the arm below reads that as "could not see the
    // fleet" and returns. On every server start (`bin/skein-server.rs:110`) that skipped the
    // launcher, the in-sandbox agent and the docker config, and said so to a terminal nobody reads.
    //
    // Whether the fleet is awake is not something this process has to ask about: it is *running
    // inside it*. Same shape and same reason as `ensure_fleet`'s `in_fleet => Some(true)` above —
    // the deployment answers a question the transport cannot.
    // **Always awake** (SKEIN-576). The sandbox this process is inside is running by definition,
    // so the question has one answer and the early return it guarded is unreachable. The host arm
    // asked `sbx ls` and treated "asleep" and "sbx did not answer" alike — both meaning *do not
    // touch it* — and neither state can be observed from in here: a fleet that is asleep is not
    // one this process is running in.
    // Before the launcher, the same ordering and for the same reason as in `ensure_fleet`: the
    // cockpit's port must be held before anything that can make a box is in place. Here as well as
    // there because a fleet that has been up for days is otherwise repaired only at the next box
    // start — the argument the deleted agent's own repair tick made, applied to the door.
    if let Err(e) = ensure_fleet_door(&sandbox) {
        eprintln!(
            "skein: the cockpit's door is not open in {sandbox} ({e}); a box in this fleet can \
             bind :{} before skein does, which is architecture §9.4's squat",
            server_sandbox_port()
        );
    }
    install_launcher(&sandbox)?;
    // Best-effort and reported rather than fatal: this only decides where the *next* dockerd puts
    // its containers, so failing it costs the merged pool its enforcement, not the fleet its boxes.
    if let Err(e) = install_docker_config(&sandbox) {
        eprintln!(
            "skein: could not point dockerd at the workload cgroup ({e}); containers in {sandbox} \
             stay outside the ceiling"
        );
    }
    // The same call the cockpit's "apply now" makes, and for the same reason it is handed back to
    // the launcher rather than written from here: these numbers are a share of the *configured*
    // fleet size, and only the sandbox knows what it really got (see `fleet_limits`).
    let ceilings = format!(
        "SKEIN_FLEET_LIMITS={} SKEIN_FLEET_GUARANTEES={} {} --ceilings",
        sh_quote(&fleet_limits()),
        sh_quote(&fleet_guarantees()),
        sh_quote(&box_session_path())
    );
    own_sandbox(&sandbox)
        .exec(&ceilings, Duration::from_secs(30))
        .map(|_| ())
        .map_err(|e| format!("reapplying the shared ceilings in {sandbox}: {e}"))
}
