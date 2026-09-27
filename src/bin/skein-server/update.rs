//! Updating skein and its agents: the update check, start, log and cancel, the restart onto a
//! new build, and who holds the cockpit's port.

use super::*;

/// Update the agent CLIs every box in this fleet shares.
///
/// **Blocking on purpose, unlike the check behind it.** `fleet::runtime_updates` must never make
/// the board wait; this is somebody pressing a button and watching for the answer, so it says what
/// moved rather than returning immediately and leaving them to guess. It is an npm install, so it
/// is slow — the page says so before it starts.
pub(super) async fn api_update_agents() -> Json<serde_json::Value> {
    let sandbox = skein::place::fleet_sandbox();
    let out = tokio::task::spawn_blocking(move || skein::fleet::update_runtimes(&sandbox)).await;
    Json(match out {
        Ok(Ok(said)) => serde_json::json!({ "ok": true, "text": said }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// What Settings -> Update draws: skein's own three revisions, and the agent CLIs beside them.
///
/// **Never blocks.** The remote is whatever the last reading found — `update::available` refreshes
/// behind the caller — and `runtime_updates` has the same rule for the same reason. A settings pane
/// polls this while it is open, and a poller that could wait on GitHub is a pane that hangs when
/// GitHub does.
pub(super) async fn api_update() -> Json<serde_json::Value> {
    let found = tokio::task::spawn_blocking(|| {
        // The token is looked up here rather than inside `update`, so that module needs no opinion
        // about credentials. Absent is fine and common: the repository is public, and an update
        // check that refused without a login would be a check nobody on a fresh fleet ever gets.
        //
        // **The credential for skein's own repository**, by the one resolver every repository-scoped
        // call uses (SKEIN-953). This check is where that bug was seen: the host token was the
        // first repository token in the file, scoped to some other repository, so GitHub answered
        // 401 while the token that covers skein's repository sat one line further down. A read, so
        // the read token may answer it. Where the token came from travels with the answer, because
        // the sentence for a refused one names where to replace it, and that differs by source.
        let (source, token) = match skein::update::slug_of(&skein::fleet::skein_source_url()) {
            Ok(slug) => skein::prq::credential_for_repo(&slug, skein::prq::Need::Read),
            Err(_) => (skein::prq::GhToken::None, None),
        };
        (
            skein::update::available(token),
            skein::fleet::runtime_updates(),
            skein::update::running(&skein::place::fleet_sandbox()),
            source.key(),
        )
    })
    .await;
    Json(match found {
        Ok((skein, runtimes, running, token_source)) => serde_json::json!({
            "skein": skein,
            "runtimes": runtimes,
            "running": running,
            "token_source": token_source,
        }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    })
}

/// Start the update, and answer at once. What it is doing comes back on `/api/update/log`.
///
/// It ends by replacing this process, so there is nothing useful to await: a handler that held the
/// request would be a handler whose reply is written by a binary that no longer exists.
pub(super) async fn api_update_start() -> Json<serde_json::Value> {
    let sandbox = skein::place::fleet_sandbox();
    let started = tokio::task::spawn_blocking(move || skein::update::start(&sandbox)).await;
    Json(match started {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// The build's output from `from` onwards, and whether it has finished.
///
/// A file read by offset rather than a stream, because the thing being watched ends by killing the
/// connection that would be carrying it — see the note at the top of `skein::update`.
pub(super) async fn api_update_log(
    axum::extract::Query(q): axum::extract::Query<UpdateLogQuery>,
) -> Json<skein::update::Reading> {
    // `spawn_blocking` because reading the log now also settles it, and settling asks the sandbox
    // whether the run is still there — a question that goes over a socket and must not be asked on
    // the async executor, however cheap it usually is.
    let from = q.from.unwrap_or(0);
    let read = tokio::task::spawn_blocking(move || {
        skein::update::log_from(&skein::place::fleet_sandbox(), from)
    })
    .await;
    Json(read.unwrap_or_default())
}

#[derive(serde::Deserialize)]
pub(super) struct UpdateLogQuery {
    from: Option<u64>,
}

/// Stop the update run the pane was showing — the one named in the body, and no other (SKEIN-1037).
///
/// The pane offers this only once the log has been quiet for `update::QUIET_TOO_LONG`, but the rule
/// that it can stop nothing except that one run is `update::cancel`'s, not the page's.
pub(super) async fn api_update_cancel(Json(body): Json<UpdateCancel>) -> Json<serde_json::Value> {
    let sandbox = skein::place::fleet_sandbox();
    let out = tokio::task::spawn_blocking(move || skein::update::cancel(&sandbox, &body.run)).await;
    Json(match out {
        Ok(Ok(skein::update::Cancelled::Stopped)) => {
            serde_json::json!({ "ok": true, "ended": "cancelled" })
        }
        Ok(Ok(skein::update::Cancelled::AlreadyEnded)) => {
            serde_json::json!({ "ok": true, "ended": "finished" })
        }
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

#[derive(serde::Deserialize)]
pub(super) struct UpdateCancel {
    #[serde(default)]
    run: String,
}

/// "Restart on new build": the doorway's in-place reload, answered before it lands — see
/// `update::restart`. What the page does next is poll `/api/health` for the new `build`.
pub(super) async fn api_restart() -> Json<serde_json::Value> {
    let sandbox = skein::place::fleet_sandbox();
    let out = tokio::task::spawn_blocking(move || skein::update::restart(&sandbox)).await;
    Json(match out {
        Ok(Ok(holder)) => serde_json::json!({ "ok": true, "holder": holder }),
        Ok(Err(skein::update::NotRestarted::NothingNewer)) => {
            serde_json::json!({ "ok": false, "why": "nothing-newer" })
        }
        Ok(Err(skein::update::NotRestarted::NoDoorway(holder))) => {
            serde_json::json!({ "ok": false, "why": "no-doorway", "holder": holder })
        }
        Err(e) => serde_json::json!({ "ok": false, "why": "failed", "error": e.to_string() }),
    })
}

/// Who holds the cockpit's port, asked by whichever server answers — `GET /api/update/restart`,
/// read by the page when a restart did not bring the new build up.
pub(super) async fn api_holder() -> Json<Option<skein::update::PortHolder>> {
    let sandbox = skein::place::fleet_sandbox();
    Json(
        tokio::task::spawn_blocking(move || skein::update::port_holder(&sandbox))
            .await
            .ok(),
    )
}
