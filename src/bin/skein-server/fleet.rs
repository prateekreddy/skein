//! The fleet sandbox: its substrate requests, load, resources and plan, the machine's sandboxes,
//! doorstep and pressure, and create, resize, save and the per-box ceilings.

use super::*;

/// Push the current per-box ceilings onto every running box.
///
/// Separate from resize, and cheap where that is expensive: a cgroup limit is live, so this changes
/// the cap on a running box with no restart, no snapshot and nothing to restore. 200 with the boxes
/// that could not be adjusted — one box missing its cgroup must not stop the rest being corrected.
pub(super) async fn api_fleet_limits() -> Response {
    match skein::fleet::apply_box_limits() {
        Ok(failed) => Json(serde_json::json!({ "failed": failed })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// What boxes have asked the fleet to install.
pub(super) async fn api_substrate() -> Json<Vec<skein::substrate::Request>> {
    // Blocking: it execs into the sandbox to read the queue.
    Json(
        tokio::task::spawn_blocking(skein::substrate::fleet_requests)
            .await
            .unwrap_or_default(),
    )
}

#[derive(serde::Deserialize)]
pub(super) struct DecideReq {
    approve: bool,
    /// Whether an approval is also recorded, so a rebuilt sandbox reinstalls it unprompted.
    /// Defaults to true — the cockpit sends `false` only when its owner unticks it.
    #[serde(default = "yes")]
    remember: bool,
    /// **What the page actually showed.** The decision is made on these, not on a re-read.
    ///
    /// The queue lives in the sandbox and every box can write it, so between the render that
    /// produced the card and the click on it — seconds to minutes — the box that filed the request
    /// can change what it says. Re-reading by id at click time approves whatever it says *then*.
    /// Echoing the rendered fields back means the thing approved is the thing seen.
    #[serde(rename = "box", default)]
    box_name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    packages: Vec<String>,
}

fn yes() -> bool {
    true
}

/// Approve or deny one request.
///
/// The decision is recorded synchronously and the install is *not* awaited: apt on a cold index is
/// minutes, and a cockpit button that hangs for minutes is one its owner clicks again. The request's
/// own state is the progress — `approved` while it runs, then `installed` or `failed` — and the
/// panel polls it like everything else on the board.
pub(super) async fn api_substrate_decide(
    Path(id): Path<String>,
    Json(r): Json<DecideReq>,
) -> Response {
    let decided = {
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            let rendered = skein::substrate::Request {
                id,
                box_name: r.box_name,
                kind: r.kind,
                packages: r.packages,
                state: "pending".into(),
                ..Default::default()
            };
            skein::substrate::fleet_decide(&rendered, r.approve, r.remember)
        })
        .await
    };
    match decided {
        Ok(Ok(req)) => {
            if r.approve {
                // Detached deliberately: nothing here reads the result, because the request file is
                // where the result goes and that is what the cockpit is already watching.
                // By box as well as id: an id is chosen by the box that filed it, so another box can
                // file the same one, and only the pair names this decision (ISO-7).
                let asker = req.box_name.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = skein::substrate::fleet_install(&asker, &id);
                });
            }
            Json(req).into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Per-box CPU, memory and process count.
///
/// Its own endpoint rather than a field on `/api/fleet/resources`, because the two are asked for at
/// different moments and cost different amounts: the gauge strip polls every 30 seconds and must
/// stay cheap, while this measures a rate over half a second and is only wanted when something looks
/// wrong. Folding it in would have put that half-second into every poll.
pub(super) async fn api_fleet_load() -> Json<Vec<skein::fleet::BoxLoad>> {
    Json(
        tokio::task::spawn_blocking(skein::fleet::box_loads)
            .await
            .unwrap_or_default(),
    )
}

/// Every sandbox on this machine — asked, not pushed.
///
/// It is the one thing that costs a subprocess and nothing waits on it, so it left the two-second
/// tick and became a question. A failure is a 503 with the reason rather than an empty list:
/// "nothing else is here" and "sbx could not be asked" are different answers, and a machine with
/// three fleets on it reporting as empty is the one that matters.
pub(super) async fn api_machine_sandboxes() -> Response {
    match tokio::task::spawn_blocking(skein::machine::sandboxes).await {
        Ok(Ok(rows)) => Json(rows).into_response(),
        Ok(Err(why)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": why })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// How hard the front door is being leaned on — `signal::Signal::MachineDoorstep`.
///
/// `knock` makes a flood **harmless** (§9.4) and that is exactly what makes it **invisible**: the
/// cockpit keeps answering, and the only trace is a counter inside this process. A steady eviction
/// rate means one of two opposite things — something is flooding the port, or the room is too small
/// for how the cockpit is really used and honest handshakes are being displaced — and neither can be
/// told from "the board felt slow once".
///
/// Not a log line per eviction, which was the obvious alternative and is the same denial by another
/// route: a flood would become a log flood. A number that is read when somebody asks costs nothing
/// however hard the door is pushed.
///
/// Behind the token like everything else, so the flooder cannot watch its own progress.
pub(super) async fn api_machine_doorstep() -> Json<serde_json::Value> {
    let door = skein::knock::doorstep();
    Json(serde_json::json!({
        "room": skein::knock::ROOM,
        "knocking": door.knocking(),
        "turned_away": door.turned_away(),
        "grace_secs": skein::knock::grace().as_secs(),
    }))
}

/// How hard the fleet is being squeezed — `signal::Signal::MachinePressure`.
///
/// The counters are files under `/sys/fs/cgroup` and `/proc` in the sandbox this process runs in,
/// so this is a handful of reads and no subprocess. Asked when somebody wants it, never on a tick.
/// It used to be an HTTP call to the in-sandbox agent, which could fail to answer; it cannot now
/// (SKEIN-521), so the 503 arm that meant "no agent" is gone with it.
///
/// A rate rather than a total, because everything the kernel keeps here is monotonic since boot: a
/// raw `98305` says the same enormous thing for ever and never says whether it is happening now.
pub(super) async fn api_machine_pressure() -> Response {
    match tokio::task::spawn_blocking(skein::fleet::pressure).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// What the fleet's VM is using right now: memory, disk, load.
///
/// `spawn_blocking` for the same reason `api_events` uses it — behind this is an `sbx exec`, and
/// running one on an async worker stalls every terminal websocket that worker is pumping.
///
/// 204 rather than an error when there is no fleet: a board with each box in its own sandbox has no
/// single machine to gauge, and that is a normal configuration rather than something to warn about.
pub(super) async fn api_fleet_resources() -> Response {
    match tokio::task::spawn_blocking(skein::fleet::fleet_resources).await {
        Ok(Some(r)) => Json(r).into_response(),
        _ => StatusCode::NO_CONTENT.into_response(),
    }
}

/// What the fleet sandbox takes, and from what.
///
/// The fleet sandbox is the largest thing skein builds on someone's machine, it is sized by
/// `fleet_memory` and its neighbours, and it cannot be changed afterwards without a rebuild: sbx
/// fixes memory, CPUs and disk at creation. So the numbers are worth showing beside what the machine
/// has, even where nothing on this page can set them.
///
/// So this is what the fleet pane has to say about the sandbox in one call: what the host has, what
/// skein would take of it, why sbx could not be asked, and the lines to run on the host in place of
/// a rebuild.
///
/// **It no longer says whether the sandbox is there, and there is no longer a question to ask**
/// (SKEIN-627). `exists` was a tri-state here, and the create-fleet dialog was its only reader:
/// `exists === false` was the one state that meant "there is none", and it opened the dialog. From
/// inside the sandbox `fleet::fleet_exists` answers `Some(true)` for the fleet this process is
/// standing in and `None` for every other name — `Some(false)` cannot arise — so the field carried
/// one value, nothing branched on it, and the dialog it existed for is deleted.
pub(super) async fn api_fleet_plan() -> Json<serde_json::Value> {
    let (host, sandbox, refusal) = tokio::task::spawn_blocking(|| {
        let sandbox = skein::place::fleet_sandbox();
        // What the rebuild route would refuse with, verbatim — null on a host, where it refuses
        // nothing. It is here because the page HIDES that button in-fleet, and a hidden control
        // with no replacement is a dead end: this is the `sbx` lines to run on the host instead.
        // Rendering the refusal itself rather than the page composing its own means what somebody
        // is told here and what pressing would have said cannot drift.
        let refusal = skein::fleet::fleet_lifecycle_refusal("rebuild", true);
        (skein::fleet::host_capacity(), sandbox, refusal)
    })
    .await
    .unwrap_or_else(|_| (skein::fleet::host_capacity(), String::new(), None));
    let proposed = skein::fleet::proposed_fleet_size(&host);
    Json(serde_json::json!({
        "sandbox": sandbox,
        "why": skein::sbx::fleet_failure(),
        "lifecycle_refusal": refusal,
        "host": host,
        "proposed": proposed,
    }))
}

/// Create a fleet sandbox, at the size in the request.
///
/// **No cockpit surface calls this any more, and it is kept deliberately** (SKEIN-627). The
/// create-fleet dialog was its only caller, and the dialog is deleted because the state that opened
/// it — `exists === false` — cannot arise for a skein running inside its own fleet. What is deleted
/// with it is creating *this* fleet, which was always the impossible one. Creating a
/// **differently-named** sandbox is still a coherent act, the warden is still on the host with the
/// capability, and `fleet::request_fleet_create` still carries the attempt lease it needs — so the
/// route stays rather than being rebuilt the day something wants to reach it.
///
/// The numbers are saved before the create, not after: `create_argv` and `create_env` read the
/// config, so a size that was only passed here would be ignored by the very command it is for. It
/// also means the sandbox and the settings agree afterwards, which is what a later resize starts
/// from.
///
/// **In-fleet this asks the warden rather than refusing** (SKEIN-576). It used to refuse before the
/// config write, on the reasoning that fleet lifecycle lives on the host — but §7.5's argument is
/// about where the *doer* runs, and it never said who may ask. §2.3 already has `http` reaching
/// "GitHub, and the warden", and the warden is on the host with the capability. So this is the
/// explicit act a person initiates, and `fleet::request_fleet_create` is what puts it.
///
/// What is NOT restored is a fallback: with no warden reachable this refuses and prints the line,
/// exactly as `docs/delivery.md:143` requires. Nothing here runs `sbx`, in either deployment.
pub(super) async fn api_fleet_create(Json(r): Json<ResizeReq>) -> Response {
    let out = tokio::task::spawn_blocking(move || {
        let sandbox = skein::config::update_config(|config| {
            for (field, value) in [
                (&mut config.fleet_memory, &r.memory),
                (&mut config.fleet_cpus, &r.cpus),
                (&mut config.fleet_disk, &r.disk),
            ] {
                // Empty means "leave what is configured", so a client sending only what it changed
                // does not clear the rest.
                if !value.trim().is_empty() {
                    *field = value.trim().to_string();
                }
            }
            Ok(config.fleet_sandbox.trim().to_string())
        })?;
        if sandbox.is_empty() {
            return Err("no fleet sandbox is named (fleet_sandbox is empty)".to_string());
        }
        // **The SERVING mount set, which is what `create_line` hands the same act** (SKEIN-678).
        // The two differ by one entry — the volume root — and it is the entry the install cannot
        // start without: `bootstrap.sh` finds the volume by scanning mountinfo for a mount point
        // ending in `/.skein`, and refuses rather than guessing when it finds none. `fleet_mounts`
        // contributes only the two directories *beneath* the volume, so a fleet created from here
        // came up with nothing for that scan to find and stopped at the refusal. sbx fixes mounts
        // at create and no verb adds one afterwards, so the sandbox had to be destroyed and remade.
        skein::fleet::request_fleet_create(&sandbox, &skein::fleet::fleet_serve_mounts())
            .map(|said| (sandbox, said))
    })
    .await;
    match out {
        Ok(Ok((sandbox, said))) => {
            Json(serde_json::json!({ "sandbox": sandbox, "said": said })).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Change the fleet sandbox's memory/CPUs, carrying every box across.
///
/// Its own endpoint rather than a side effect of saving settings, because it is destructive and
/// slow: sbx fixes both at creation, so this rebuilds the sandbox and every box in it. Saving the
/// numbers alone only changes what the NEXT create uses — which is why the settings pane offers this
/// separately rather than appearing to apply them and quietly doing nothing.
///
/// It used to answer 200 with the boxes that failed to come back. There is no such list any more:
/// nothing brings a box back from here, because the destroy takes this process with it
/// (SKEIN-679), so the phases that would have produced one are gone from
/// [`skein::fleet::resize_fleet`].
///
/// Refused in-fleet, and it is the sharpest case of the two: a rebuild is a destroy followed by a
/// create, the destroy is the half that cannot be undone, and it is the half that would succeed.
pub(super) async fn api_fleet_resize(Json(r): Json<ResizeReq>) -> Response {
    if let Some(why) = skein::fleet::fleet_lifecycle_refusal("rebuild", true) {
        return (StatusCode::CONFLICT, why).into_response();
    }
    match skein::fleet::resize_fleet(&r.memory, &r.cpus, &r.disk, r.drop_docker) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Copy every box's work out of the sandbox and onto the host, and say where each one went.
///
/// **The one route on this pane that does something rather than refusing it** (SKEIN-680). Its
/// neighbours — create and resize — are lifecycle, which §7.5 puts outside the fleet permanently. A
/// save is not lifecycle: it reads the boxes and writes the host, destroys nothing, stops nothing,
/// and needs no box to be idle, so it is exactly the act this deployment *can* offer, and the
/// button for it is what makes the refusal beside it something other than a dead end.
///
/// **A partial save answers 200, and that is deliberate.** The body carries one entry per box with
/// its own error, because the box that failed is the one whose work is still only inside the
/// sandbox — a 500 would throw away the report naming it, along with the paths of every box that
/// did make it out. What a 500 means here is the whole act refusing before it wrote anything: no
/// census, no room on the host, a name that is not a box.
///
/// Blocking rather than an Act (§2.5), like the resize it replaces the first third of. A save is
/// minutes of `tar` and its transcript is four lines, not a build log; what a person waits for is
/// the report, which is the response.
pub(super) async fn api_fleet_save() -> Response {
    match tokio::task::spawn_blocking(|| skein::fleet::save_boxes(&[])).await {
        Ok(Ok(boxes)) => Json(serde_json::json!({ "boxes": boxes })).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct ResizeReq {
    #[serde(default)]
    memory: String,
    #[serde(default)]
    cpus: String,
    /// Root filesystem size. Empty keeps the configured one — see [`skein::config::Config::fleet_disk`].
    #[serde(default)]
    disk: String,
    /// Proceed even though the rebuild destroys locally-built images and named volumes.
    ///
    /// Defaults to false, so a client that predates this field gets the refusal rather than the
    /// destruction — which is the right way round for a field that means "yes, lose it".
    #[serde(default)]
    drop_docker: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Neither lifecycle route can do its work by a path that skips the check**, and this keeps
    /// it so — but the two checks are no longer the same check (SKEIN-576).
    ///
    /// `api_fleet_resize` still asks the deployment first, and that guard is the sharpest in the
    /// file: a rebuild is a destroy followed by a create, the destroy is the half that cannot be
    /// undone, and in-fleet it is the half that would succeed — taking the machine this process is
    /// on with it (SKEIN-467).
    ///
    /// `api_fleet_create` no longer refuses in-fleet, because §7.5 is about where the *doer* runs
    /// and never about who may ask. What replaces the refusal is narrower and stronger: the route
    /// may reach a create **only** through `fleet::request_fleet_create`, which asks the warden and,
    /// with no warden, refuses and prints. So what is asserted here is that it calls that and does
    /// not call `ensure_fleet` — the caller it used to have, which would create as a side effect of
    /// making sure a fleet was ready.
    ///
    /// Read out of the source rather than by calling the handlers, and that is not laziness:
    /// `api_fleet_create` writes the settings and `api_fleet_resize` destroys the sandbox, so a
    /// test that drove the gate through them would — on the day the gate broke, which is the only
    /// day it matters — do the exact irreversible thing it exists to prevent. Same technique as
    /// `cockpit_routes` below, which reads the router out of this file for its own reason.
    #[test]
    fn neither_lifecycle_route_reaches_its_work_by_a_path_that_skips_the_check() {
        // Production code only. This test names both handlers in its own body, and a scan that
        // read itself would find the guard in its own assertion.
        let production: String = server_production()
            .lines()
            .take_while(|l| !l.starts_with("#[cfg(test)]"))
            .collect::<Vec<_>>()
            .join("\n");
        let handler = |name: &str| -> &str {
            production
                .split(name)
                .nth(1)
                .unwrap_or_else(|| panic!("{name} is gone"))
        };

        // The destroy half, unchanged: the deployment is asked before anything is destroyed.
        let resize = handler("async fn api_fleet_resize");
        let guard = resize.find("fleet_lifecycle_refusal(").unwrap_or_else(|| {
            panic!(
                "api_fleet_resize no longer asks where skein is running — in-fleet what it calls \
                 next destroys the machine this process is on (docs/architecture.md §7.5)"
            )
        });
        let doing = resize
            .find("skein::fleet::resize_fleet(")
            .expect("api_fleet_resize no longer resizes");
        assert!(
            guard < doing,
            "api_fleet_resize does its work before it checks the deployment, so the refusal \
             arrives after the damage"
        );

        // The create half: one way in, and it is the one that asks the warden.
        let create = handler("async fn api_fleet_create");
        assert!(
            create.contains("skein::fleet::request_fleet_create("),
            "api_fleet_create reaches a create by some path other than the explicit act, which is \
             the only path that refuses when no warden answers"
        );
        assert!(
            !create.contains("skein::fleet::ensure_fleet("),
            "api_fleet_create is back to creating through `ensure_fleet`, which creates as a side \
             effect of making a fleet ready and has no warden-less refusal of its own"
        );
    }
}
