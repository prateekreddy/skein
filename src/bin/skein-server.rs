//! skein-server — the web cockpit. A tiny axum server over `skein` (lib):
//!   GET /                          the dark web board (self-contained, no build)
//!   GET /api/boxes                 fleet snapshot (JSON)
//!   GET /api/events                live fleet stream (SSE)
//!   GET /api/boxes/:name/terminal  WebSocket ↔ PTY running `sbx run --name <box>`  (the single-pane bit)
//!
//! The terminal reuses wheels: portable-pty (server PTY) + xterm.js (browser). We write only the
//! WS↔PTY bridge. Bind is localhost-only; remote access = tunnel + auth (roadmap phase 4).

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use skein::{load_views, BoxView};
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{Read, Write};
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
        .route("/api/boxes/:name/diff", get(api_diff))
        .route("/api/mailbox", get(api_mailbox).post(api_mailbox_send))
        .route("/api/events", get(api_events))
        .route("/api/boxes/:name/terminal", get(terminal));

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

/// The branch-vs-base patch a box last reported (plain text; empty when none yet).
async fn api_diff(Path(name): Path<String>) -> Response {
    let body = skein::read_diff(&name)
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| {
            "# no diff reported yet — the box writes one when its agent pauses (Stop hook)\n".into()
        });
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

/// All cross-box messages, newest first.
async fn api_mailbox() -> Json<Vec<skein::Message>> {
    Json(skein::load_mailbox())
}

#[derive(Deserialize)]
struct SendReq {
    to: String,
    body: String,
    #[serde(default)]
    kind: String,
}

/// Post a message into the shared mailbox (from skein). `to` is a vmid or "broadcast".
async fn api_mailbox_send(Json(r): Json<SendReq>) -> Response {
    if r.body.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "empty body").into_response();
    }
    let to = if r.to.trim().is_empty() {
        "broadcast"
    } else {
        r.to.trim()
    };
    match skein::send_message(to, &r.kind, &r.body) {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Live fleet stream: re-emits the fleet every 2s as an SSE `boxes` event.
/// (Roadmap: replace polling with a honker subscription so it's push, not poll.)
async fn api_events() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).map(|_| {
        let payload = serde_json::to_string(&load_views().unwrap_or_default())
            .unwrap_or_else(|_| "[]".into());
        Ok(Event::default().event("boxes").data(payload))
    });
    Sse::new(stream)
}

/// Upgrade to a WebSocket that bridges the browser terminal to a PTY.
/// `?launch=<branch>` runs the box-creation command instead of attaching to an existing box.
async fn terminal(
    ws: WebSocketUpgrade,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let launch = q.get("launch").filter(|s| !s.is_empty()).cloned();
    ws.on_upgrade(move |socket| terminal_session(socket, name, launch))
}

/// The WS↔PTY bridge: spawn `sbx run --name <box>` in a PTY, pipe bytes both ways, honour resizes.
/// Override the spawned command with $SKEIN_ATTACH_CMD (run via `sh -c`) for local testing.
async fn terminal_session(mut socket: WebSocket, name: String, launch: Option<String>) {
    let pair = match native_pty_system().openpty(PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("skein: pty error: {e}")))
                .await;
            return;
        }
    };

    // Build the command. Propagate env + cwd so `sbx`/`sh` resolve on PATH.
    // Default: reconnect to the box's *existing* agent session (tmux + `claude --continue`),
    // rooted at the dir the box registered.
    //
    // $SKEIN_ATTACH_CMD fully overrides it (run via `sh -c`) — `{name}` and `{dir}` in the
    // value are substituted first, so you can tune the exact sbx invocation per box without
    // recompiling, e.g.  SKEIN_ATTACH_CMD='sbx run --name {name} -- claude --continue'
    let dir = skein::lookup_dir(&name).unwrap_or_default();
    let mut cmd = if let Some(branch) = &launch {
        // create-a-box mode: run the launch command in a PTY so the user watches it come up
        let mut b = CommandBuilder::new("sh");
        b.arg("-c");
        b.arg(skein::launch_command(branch));
        b
    } else {
        match std::env::var("SKEIN_ATTACH_CMD") {
            Ok(c) if !c.is_empty() => {
                let c = c.replace("{name}", &name).replace("{dir}", &dir);
                let mut b = CommandBuilder::new("sh");
                b.arg("-c");
                b.arg(c);
                b
            }
            _ => {
                let mut b = CommandBuilder::new("sbx");
                for a in skein::attach_argv(&name, &dir) {
                    b.arg(a);
                }
                b
            }
        }
    };
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("skein: spawn failed: {e}")))
                .await;
            return;
        }
    };
    drop(pair.slave); // release the slave fd in the parent so EOF propagates on child exit

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("skein: pty reader: {e}")))
                .await;
            return;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("skein: pty writer: {e}")))
                .await;
            return;
        }
    };
    let master = pair.master; // kept for resize

    // PTY output → channel (blocking read on a thread).
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Channel → PTY input (blocking write on a thread).
    let (in_tx, mut in_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    std::thread::spawn(move || {
        while let Some(bytes) = in_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });

    loop {
        tokio::select! {
            out = out_rx.recv() => match out {
                Some(bytes) => {
                    if socket.send(Message::Binary(bytes)).await.is_err() { break; }
                }
                None => break, // PTY closed (child exited)
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(b))) => { let _ = in_tx.send(b).await; }
                Some(Ok(Message::Text(t))) => {
                    // resize control frame: {"resize":{"cols":N,"rows":M}}
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        if let Some(r) = v.get("resize") {
                            let cols = r.get("cols").and_then(|x| x.as_u64()).unwrap_or(100) as u16;
                            let rows = r.get("rows").and_then(|x| x.as_u64()).unwrap_or(30) as u16;
                            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }

    let _ = child.kill();
}
