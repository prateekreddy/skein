//! The fleet's agent CLI and Rust toolchain versions: the view, the owner's "Roll out" and
//! "Remove", and the two loops that fetch new versions and move idle boxes onto a rollout
//! (SKEIN-1372, `skein::fleet::versions`).

use super::*;

/// Each component's current and previous version, every version the depot holds with its size,
/// tests and users, and the running boxes still waiting to restart onto the current one.
pub(super) async fn api_versions() -> Json<serde_json::Value> {
    let found = tokio::task::spawn_blocking(skein::fleet::versions::view).await;
    Json(match found {
        Ok(components) => serde_json::json!({ "components": components }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    })
}

#[derive(serde::Deserialize)]
pub(super) struct RolloutReq {
    component: String,
    entry: String,
}

/// The owner's "Roll out": the entry becomes what every box of the component starts on. Running
/// boxes are not touched; each moves when it is idle.
pub(super) async fn api_versions_rollout(Json(r): Json<RolloutReq>) -> Response {
    match tokio::task::spawn_blocking(move || {
        skein::fleet::versions::rollout(&r.component, &r.entry)
    })
    .await
    {
        Ok(Ok(rolled)) => Json(rolled).into_response(),
        Ok(Err(e)) => (StatusCode::CONFLICT, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct VersionsCleanReq {
    kind: String,
    /// The entries the person confirmed, exactly as the view showed them.
    #[serde(default)]
    names: Vec<String>,
}

/// Remove the versions the person confirmed, each only if nothing keeps it at that moment.
pub(super) async fn api_versions_clean(Json(r): Json<VersionsCleanReq>) -> Response {
    match tokio::task::spawn_blocking(move || skein::fleet::versions::clean(&r.kind, &r.names))
        .await
    {
        Ok(Ok(done)) => Json(done).into_response(),
        Ok(Err(e)) => (StatusCode::CONFLICT, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// How long after the server starts the first fetch pass runs, and then how often. Six hours, the
/// agent CLI update check's own period (`fleet::substrate`'s `UPDATE_CHECK_EVERY`); not at boot,
/// so starting the cockpit is not a network call.
const FETCH_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const FIRST_FETCH_AFTER: Duration = Duration::from_secs(10 * 60);

/// **New versions, fetched and tested on skein's own clock** (the owner's decision 1): the npm
/// registry is asked for each CLI's newest version, a missing one is filled and tested, and a repo
/// toolchain a box needs is fetched. Nothing is rolled out here.
pub(super) async fn watch_versions() {
    tokio::time::sleep(FIRST_FETCH_AFTER).await;
    let mut tick = tokio::time::interval(Duration::from_secs(5 * 60));
    let mut last_fetch: Option<std::time::Instant> = None;
    loop {
        tick.tick().await;
        let due = last_fetch.is_none_or(|t| t.elapsed() >= FETCH_EVERY);
        if due {
            last_fetch = Some(std::time::Instant::now());
        }
        let pass = tokio::task::spawn_blocking(move || {
            let sandbox = skein::place::fleet_sandbox();
            let mut said = Vec::new();
            if due {
                said.extend(skein::fleet::versions::watch_pass(&sandbox));
            }
            said.extend(skein::fleet::versions::serve_repo_toolchains(&sandbox));
            said
        })
        .await;
        for what in pass.unwrap_or_default() {
            eprintln!("skein: {what}");
        }
    }
}

static IDLE: std::sync::OnceLock<skein::fleet::versions::IdleMemory> = std::sync::OnceLock::new();

/// **Boxes behind a rollout move when they are idle** (the owner's decision 2), once a minute, by
/// `fleet::versions::restart_onto_rollouts`: the board's turn state, read again at the restart, and
/// the cockpit's own whole-box restart hold, so a person's press and this never restart one box
/// twice at once.
pub(super) async fn watch_rollout_restarts() {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        let pass = tokio::task::spawn_blocking(|| {
            let boxes: Vec<skein::fleet::versions::BoxNow> = load_views()
                .into_iter()
                .map(|v| skein::fleet::versions::BoxNow {
                    name: v.name,
                    state: v.state,
                })
                .collect();
            skein::fleet::versions::restart_onto_rollouts(
                &boxes,
                std::time::Instant::now(),
                IDLE.get_or_init(Default::default),
                |name| {
                    skein::fleet::running_record(name)
                        .map(|r| skein::fleet::why_cover_is_stale(name, &r))
                        .unwrap_or_default()
                },
                |name| {
                    let state = load_views()
                        .into_iter()
                        .find(|v| v.name == name)
                        .map(|v| v.state)
                        .unwrap_or_default();
                    skein::fleet::idle::idle(None, &state)
                },
                |name| {
                    let _held = Restarting::take(name)
                        .ok_or_else(|| format!("{name} is already being restarted"))?;
                    // The whole box, through `skein restart`'s own path (SKEIN-1326), so it starts
                    // on what a start binds now; then its agent reopened on its conversation with
                    // nothing sent to it (SKEIN-1071's reopen), so it is waiting where it was.
                    skein::sandbox::restart_box(name, None)?;
                    skein::sandbox::reopen_agent_session(name)
                },
            )
        })
        .await;
        for what in pass.unwrap_or_default() {
            eprintln!("skein: {what}");
        }
    }
}
