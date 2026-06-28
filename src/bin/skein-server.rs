//! skein-server — the web cockpit. A tiny axum server over `skein` (lib) that exposes the
//! fleet as JSON + a live SSE stream, and serves a self-contained dark page (no build step).
//!
//! Roadmap: action endpoints (launch/diff/merge/archive) + a honker-backed job queue, and a
//! richer Svelte frontend for diffs. This v0 is read-only + live — the foundation.

use axum::response::sse::{Event, Sse};
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use skein::{load_views, BoxView};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

const INDEX: &str = include_str!("../web/index.html");
const ADDR: &str = "127.0.0.1:7878";

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/boxes", get(api_boxes))
        .route("/api/events", get(api_events));

    let listener = tokio::net::TcpListener::bind(ADDR)
        .await
        .unwrap_or_else(|e| panic!("skein-server: cannot bind {ADDR}: {e}"));
    println!("skein-server → http://{ADDR}");
    axum::serve(listener, app)
        .await
        .expect("skein-server: serve failed");
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

/// Snapshot of the fleet.
async fn api_boxes() -> Json<Vec<BoxView>> {
    Json(load_views().unwrap_or_default())
}

/// Live fleet stream: re-emits the fleet every 2s as an SSE `boxes` event.
/// (Roadmap: replace polling with a honker event subscription so it's push, not poll.)
async fn api_events() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).map(|_| {
        let payload =
            serde_json::to_string(&load_views().unwrap_or_default()).unwrap_or_else(|_| "[]".into());
        Ok(Event::default().event("boxes").data(payload))
    });
    Sse::new(stream)
}
