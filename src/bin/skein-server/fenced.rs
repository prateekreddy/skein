//! The cockpit of a fleet that does not own its volume.
//!
//! A router of its own rather than a check on each route and tick: what is not listed here does not
//! exist in this process, so a fenced server cannot write to the volume through a route or a
//! background task added later. It serves the pages, says who owns the volume, and takes it over
//! when a person asks. Everything else is a 409.

use super::*;
use skein::fence::{self, Standing, TakeOverError, Took};

pub(super) fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/v2", get(board_v2))
        .route("/vendor/xterm.js", get(|| asset("xterm.min.js")))
        .route("/vendor/xterm.css", get(|| asset("xterm.min.css")))
        .route("/vendor/addon-fit.js", get(|| asset("addon-fit.min.js")))
        .route("/vendor/marked.js", get(|| asset("marked.min.js")))
        .route("/vendor/cockpit.js", get(|| asset("cockpit.js")))
        .route("/vendor/fence.js", get(|| asset("fence.js")))
        .route("/assets/*path", get(any_asset))
        // `build` because the install waits for this build to answer.
        .route(
            "/api/health",
            get(|| async {
                Json(serde_json::json!({
                    "ok": false,
                    "fenced": true,
                    "build": skein::health::BUILD_REVISION,
                }))
            }),
        )
        .route("/api/fleet/owner", get(api_fleet_owner))
        .route("/api/fleet/take-over", post(api_take_over))
        .fallback(|| async {
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "ok": false,
                    "fenced": true,
                    "error": "this fleet does not own its volume",
                })),
            )
        })
        .layer(axum::middleware::from_fn(gate))
        .layer(axum::middleware::from_fn(security_headers))
}

/// Who owns the volume, for the banner.
pub(super) async fn api_fleet_owner() -> Json<serde_json::Value> {
    let home = skein::config::skein_home();
    Json(match fence::standing(&home) {
        Standing::Fenced(owner) => serde_json::json!({
            "fenced": true,
            "claim": fence::claim_command(&owner),
            "this_fleet": fence::fleet_id_here(),
            "owner": owner,
        }),
        _ => serde_json::json!({ "fenced": false }),
    })
}

/// The owner the page was shown. A take-over from anyone else is refused.
#[derive(Deserialize)]
pub(super) struct TakeOver {
    fleet_id: String,
    epoch: u64,
}

/// Take the volume, then let the doorway start the server again as its owner.
pub(super) async fn api_take_over(Json(asked): Json<TakeOver>) -> Response {
    let home = skein::config::skein_home();
    let out =
        tokio::task::spawn_blocking(move || fence::take_over(&home, &asked.fleet_id, asked.epoch))
            .await;
    match out {
        Ok(Ok(Took::Already(owner))) => {
            Json(serde_json::json!({ "ok": true, "owner": owner, "restarting": false }))
                .into_response()
        }
        Ok(Ok(Took::Now(owner))) => {
            fence::release_log();
            // The doorway restarts whatever exits. Without one, nothing would come back.
            let restarting = skein::doorway::inherited_only();
            if restarting {
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    std::process::exit(0);
                });
            }
            Json(serde_json::json!({ "ok": true, "owner": owner, "restarting": restarting }))
                .into_response()
        }
        Ok(Err(TakeOverError::Stale(owner))) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "ok": false,
                "why": "stale",
                "error": "The owner changed after this page loaded, so nothing was taken over.",
                "owner": owner,
            })),
        )
            .into_response(),
        Ok(Err(TakeOverError::Failed(why))) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "why": "failed", "error": why })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "why": "failed", "error": e.to_string() })),
        )
            .into_response(),
    }
}
