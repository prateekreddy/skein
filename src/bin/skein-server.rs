//! skein-server — the web cockpit. A tiny axum server over `skein` (lib):
//!   GET /                          the dark web board (self-contained, no build)
//!   GET /api/boxes                 fleet snapshot (JSON)
//!   GET /api/events                live fleet stream (SSE)
//!   GET /api/boxes/:name/terminal  WebSocket ↔ PTY running `sbx run --name <box>`  (the single-pane bit)
//!
//! The terminal reuses wheels: portable-pty (server PTY) + xterm.js (browser). We write only the
//! WS↔PTY bridge. Bind is loopback-only; remote access = `tailscale serve` (the tailnet is the
//! auth boundary) proxying to this loopback port. See README "Remote access".

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
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
// Vendored, not CDN-loaded: the terminal must work in the firewalled sbx network the tool lives in.
const XTERM_JS: &str = include_str!("../web/vendor/xterm.min.js");
const XTERM_CSS: &str = include_str!("../web/vendor/xterm.min.css");
const FIT_JS: &str = include_str!("../web/vendor/addon-fit.min.js");
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// Cap concurrent embedded terminals so a flood of WS connections can't exhaust PTYs / file
/// descriptors on the host. Each live terminal holds one permit for its whole session.
static PTY_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(24);

#[tokio::main]
async fn main() {
    // Pick up a local .env so the registry/repo paths needn't be typed each run (real env vars
    // still win; a malformed file is reported, not silently half-applied). See skein::load_dotenv.
    skein::load_dotenv();
    // Install skein's turn-state probe into the shared store (idempotent), so every box reports
    // working/waiting/needs-input + task without the repo shipping hooks. Best-effort.
    if let Err(e) = skein::ensure_probe_all() {
        eprintln!("skein: turn-state probe not installed ({e}); boxes will show live/stale only");
    }
    // Install skein's own sbx kit (idempotent) so launching a box needs no repo-side kit.
    if let Err(e) = skein::ensure_kit() {
        eprintln!("skein: kit not installed ({e}); native launch will fall back to $SKEIN_KIT");
    }
    // Seed the host gh token into sbx (global) so boxes can fetch/push/open PRs. Best-effort and
    // quiet — many setups rely on a proxy injecting credentials instead. Skip with $SKEIN_NO_GH_SECRET.
    if let Err(e) = skein::ensure_gh_secret() {
        eprintln!("skein: gh token not seeded ({e}); boxes may not push without it");
    }
    // Load the configured SSH key into the host ssh-agent so sbx forwards it into boxes (SSH push).
    // No-op when none is configured. Best-effort.
    if let Err(e) = skein::ensure_ssh_key() {
        eprintln!("skein: ssh key not loaded ({e}); SSH git push from boxes may fail");
    }
    // Bind is loopback-only by default; $SKEIN_ADDR overrides it. For remote access prefer
    // `tailscale serve` proxying to this loopback port (see README) over an off-loopback bind.
    let addr = std::env::var("SKEIN_ADDR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_ADDR.into());
    let app = Router::new()
        .route("/", get(index))
        .route("/vendor/xterm.js", get(vendor_xterm_js))
        .route("/vendor/xterm.css", get(vendor_xterm_css))
        .route("/vendor/addon-fit.js", get(vendor_fit_js))
        .route("/api/boxes", get(api_boxes))
        .route("/api/repos", get(api_repos).post(api_add_repo))
        .route("/api/repos/:id", axum::routing::delete(api_remove_repo))
        .route("/api/settings", get(api_settings).post(api_set_settings))
        .route("/api/boxes/:name/diff", get(api_diff))
        .route("/api/boxes/:name/session", get(api_session))
        .route("/api/mailbox", get(api_mailbox).post(api_mailbox_send))
        .route("/api/boxes/:name/ship", get(api_ship))
        .route("/api/boxes/:name/pr", post(api_pr))
        .route("/api/boxes/:name/merge", post(api_merge))
        .route("/api/boxes/:name/resume", post(api_resume))
        .route("/api/boxes/:name/narrate", get(api_narrate))
        .route("/api/resume-batch", post(api_resume_batch))
        .route("/api/collisions", get(api_collisions))
        .route("/api/boxes/:name/stop", post(api_stop))
        .route("/api/boxes/:name/destroy", post(api_destroy))
        .route(
            "/api/boxes/:name/paste-image",
            // screenshots are bigger than axum's 2 MB default body cap — allow up to 25 MB.
            post(api_paste_image).layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024)),
        )
        .route("/api/events", get(api_events))
        .route("/api/boxes/:name/terminal", get(terminal));

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("skein-server: cannot bind {addr}: {e}"));
    println!("skein-server → http://{addr}");
    axum::serve(listener, app)
        .await
        .expect("skein-server: serve failed");
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

async fn vendor_xterm_js() -> Response {
    static_asset(XTERM_JS, "application/javascript; charset=utf-8")
}
async fn vendor_fit_js() -> Response {
    static_asset(FIT_JS, "application/javascript; charset=utf-8")
}
async fn vendor_xterm_css() -> Response {
    static_asset(XTERM_CSS, "text/css; charset=utf-8")
}
fn static_asset(body: &'static str, ct: &'static str) -> Response {
    ([(axum::http::header::CONTENT_TYPE, ct)], body).into_response()
}

/// Origin guard for the terminal upgrade. Browsers always send `Origin` on a WebSocket handshake
/// and page JS cannot forge it, so rejecting unexpected origins blocks drive-by cross-origin
/// connections (WS is exempt from same-origin policy) and DNS-rebinding against the terminal.
/// Allowed: loopback (local use); any `*.ts.net` host (Tailscale `serve`/`funnel`, which terminates
/// TLS and proxies to our loopback bind, so the page origin is the tailnet name); and any host in
/// `$SKEIN_ALLOWED_ORIGINS` (comma-separated) for other reverse proxies. Non-browser clients send
/// no `Origin` and are allowed — they can't reach an off-loopback bind without being on the tailnet.
fn origin_ok(headers: &axum::http::HeaderMap) -> bool {
    let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        return true;
    };
    let authority = origin.split("://").nth(1).unwrap_or("");
    let authority = authority.split('/').next().unwrap_or(authority);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest) // [::1]:port -> ::1
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    if matches!(host, "localhost" | "127.0.0.1" | "::1") || host.ends_with(".ts.net") {
        return true;
    }
    std::env::var("SKEIN_ALLOWED_ORIGINS").is_ok_and(|list| {
        list.split(',')
            .map(str::trim)
            .any(|h| !h.is_empty() && h == host)
    })
}

/// Snapshot of the fleet.
async fn api_boxes() -> Json<Vec<BoxView>> {
    Json(load_views().unwrap_or_default())
}

/// The branch-vs-base patch a box last reported (plain text; empty when none yet).
async fn api_diff(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
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

/// The free session digest for a box — "what happened here" assembled from commits, diff,
/// the agent's journal, and its last reported message. No model tokens spent. 404 if unknown.
async fn api_session(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::session_digest(&name)).await {
        Ok(Some(d)) => Json(d).into_response(),
        _ => (StatusCode::NOT_FOUND, "no such box").into_response(),
    }
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

/// List the repos skein manages.
async fn api_repos() -> Json<Vec<skein::Repo>> {
    Json(skein::load_repos())
}

#[derive(Deserialize)]
struct AddRepoReq {
    source: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    agent: String,
}

/// Register a repo: clone a URL (or adopt a local path), provision its store + kit, record it.
/// `git clone` can take a while, so run the blocking work off the async runtime.
async fn api_add_repo(Json(r): Json<AddRepoReq>) -> Response {
    if r.source.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "missing source").into_response();
    }
    let res = tokio::task::spawn_blocking(move || {
        let id = (!r.id.trim().is_empty()).then(|| r.id.trim().to_string());
        let agent = (!r.agent.trim().is_empty()).then(|| r.agent.trim().to_string());
        skein::add_repo(r.source.trim(), id.as_deref(), agent.as_deref())
    })
    .await;
    match res {
        Ok(Ok(repo)) => {
            // Warn up-front if origin is SSH (push from a box won't work — HTTPS needed).
            let warning = skein::ssh_remote_warning(&repo.work);
            Json(serde_json::json!({ "repo": repo, "warning": warning })).into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

/// Unregister a repo (files left on disk).
async fn api_remove_repo(Path(id): Path<String>) -> Response {
    match skein::remove_repo(&id) {
        Ok(repo) => Json(repo).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// Read skein's app settings (the cockpit's toggles).
async fn api_settings() -> Json<skein::Config> {
    Json(skein::load_config())
}

/// Update skein's app settings.
async fn api_set_settings(Json(c): Json<skein::Config>) -> Response {
    match skein::save_config(&c) {
        Ok(()) => {
            // Apply a newly-set SSH key immediately (load into the agent) so the user needn't restart.
            if let Err(e) = skein::ensure_ssh_key() {
                eprintln!("skein: ssh key not loaded ({e})");
            }
            Json(c).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
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

/// Merge-readiness for a box (PR state + CI checks), host-side via `gh`.
async fn api_ship(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let s = tokio::task::spawn_blocking(move || skein::ship_status(&name))
        .await
        .unwrap_or_default();
    Json(s).into_response()
}

/// Open a PR for the box's branch (host-side). Returns {ok, url|error}.
async fn api_pr(Path(name): Path<String>) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let r = tokio::task::spawn_blocking(move || skein::create_pr(&name)).await;
    Json(match r {
        Ok(Ok(url)) => serde_json::json!({ "ok": true, "url": url }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// Merge the box's PR (host-side via `gh`). Returns {ok, msg|error}.
async fn api_merge(Path(name): Path<String>) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let r = tokio::task::spawn_blocking(move || skein::merge_pr(&name)).await;
    Json(match r {
        Ok(Ok(msg)) => serde_json::json!({ "ok": true, "msg": msg }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

#[derive(Deserialize, Default)]
struct ResumeReq {
    #[serde(default)]
    prompt: String,
}

/// Resume a paused box (the one-click "continue" — step 6). Fire-and-forget: kicks off the box's
/// agent headless and returns immediately; the inbox follows the box's own hooks. Returns {ok} or
/// {ok:false, error}. Only ever called from an explicit click; batch use is gated to `proceed` boxes.
async fn api_resume(
    Path(name): Path<String>,
    body: Option<Json<ResumeReq>>,
) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let prompt = body.map(|Json(b)| b.prompt).unwrap_or_default();
    let r = tokio::task::spawn_blocking(move || skein::resume_box(&name, &prompt)).await;
    Json(match r {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// Lazy AI narration of a box's last turn — the rationed Haiku fallback for the digest when the box
/// keeps no journal (step 7). Returns {summary} (null when AI is off / unavailable). Called on demand
/// only (Session-tab open), cached per turn-end; never per fleet tick.
async fn api_narrate(Path(name): Path<String>) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "summary": null }));
    }
    let s = tokio::task::spawn_blocking(move || skein::narrate(&name))
        .await
        .ok()
        .flatten();
    Json(serde_json::json!({ "summary": s }))
}

#[derive(Deserialize)]
struct BatchReq {
    #[serde(default)]
    names: Vec<String>,
}

/// Batch-resume the boxes paused on a trivial "proceed?" (step 6). With AI on (step 7) each is first
/// run past the conservative safety gate; genuine decisions are held back. Returns {ok, resumed, held}.
async fn api_resume_batch(Json(r): Json<BatchReq>) -> Json<serde_json::Value> {
    let (resumed, held) = tokio::task::spawn_blocking(move || skein::resume_batch(&r.names))
        .await
        .unwrap_or_default();
    Json(serde_json::json!({ "ok": true, "resumed": resumed, "held": held }))
}

/// Files two or more boxes have both changed — the collision radar (step 9). Cached host-side.
async fn api_collisions() -> Json<Vec<skein::Collision>> {
    Json(
        tokio::task::spawn_blocking(skein::collisions)
            .await
            .unwrap_or_default(),
    )
}

/// Stop a box: halt the sandbox (frees compute; resume later via attach). Non-destructive — the box
/// stays listed and goes stale until resumed. Returns {ok} or {ok:false, error}.
async fn api_stop(Path(name): Path<String>) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let r = tokio::task::spawn_blocking(move || skein::stop_box(&name)).await;
    Json(match r {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// Destroy a box: tear the sandbox down (sbx rm — reclaims its resources) then delist it.
/// Destructive. Returns {ok} or {ok:false, error}.
async fn api_destroy(Path(name): Path<String>) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let r = tokio::task::spawn_blocking(move || skein::destroy_box(&name)).await;
    Json(match r {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// Receive a pasted image (raw bytes, `Content-Type: image/*`) and stream it into the box, returning
/// its in-box path for the agent to read. The agent can't see the user's clipboard (it's in the
/// microVM), so this bridges a browser paste to a file the agent can open.
async fn api_paste_image(
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let ext = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| ct.split(';').next())
        .and_then(|ct| ct.trim().strip_prefix("image/"))
        .unwrap_or("png")
        .to_string();
    let bytes = body.to_vec();
    let r =
        tokio::task::spawn_blocking(move || skein::save_pasted_image(&name, &ext, &bytes)).await;
    Json(match r {
        Ok(Ok(path)) => serde_json::json!({ "ok": true, "path": path }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
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
    headers: axum::http::HeaderMap,
) -> Response {
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin terminal blocked").into_response();
    }
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let launch = q.get("launch").filter(|s| !s.is_empty()).cloned();
    let shell = q
        .get("shell")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    ws.on_upgrade(move |socket| terminal_session(socket, name, launch, shell))
}

/// The WS↔PTY bridge: spawn `sbx run --name <box>` in a PTY, pipe bytes both ways, honour resizes.
/// Override the spawned command with $SKEIN_ATTACH_CMD (run via `sh -c`) for local testing.
async fn terminal_session(
    mut socket: WebSocket,
    name: String,
    launch: Option<String>,
    shell: bool,
) {
    // Hold a permit for the whole session; reject (rather than queue) when the cap is hit so a
    // hung browser can't silently stall new terminals. Dropped on every return → released.
    let _permit = match PTY_LIMIT.try_acquire() {
        Ok(p) => p,
        Err(_) => {
            let _ = socket
                .send(Message::Text(
                    "skein: too many terminals open — close one and retry".into(),
                ))
                .await;
            return;
        }
    };
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
    // $SKEIN_SHELL_CMD overrides the shell command, $SKEIN_ATTACH_CMD the agent attach (both run via
    // `sh -c`, `{name}`/`{dir}` substituted). Default agent: reconnect to the box's tmux+`claude
    // --continue` session; default shell: `sbx exec -it <box> /bin/bash` — a plain terminal.
    let override_var = if shell {
        "SKEIN_SHELL_CMD"
    } else {
        "SKEIN_ATTACH_CMD"
    };
    let mut cmd = if let Some(branch) = &launch {
        // create-a-box mode: run the launch command in a PTY so the user watches it come up
        let mut b = CommandBuilder::new("sh");
        b.arg("-c");
        b.arg(skein::launch_command(&name, branch));
        b
    } else {
        match std::env::var(override_var) {
            Ok(c) if !c.is_empty() => {
                let c = c
                    .replace("{name}", &skein::sh_quote(&name))
                    .replace("{dir}", &skein::sh_quote(&dir));
                let mut b = CommandBuilder::new("sh");
                b.arg("-c");
                b.arg(c);
                b
            }
            _ => {
                let mut b = CommandBuilder::new("sbx");
                let argv = if shell {
                    skein::shell_argv(&name)
                } else {
                    skein::attach_argv(&name, &dir)
                };
                for a in argv {
                    b.arg(a);
                }
                b
            }
        }
    };
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    // Run launch/attach from the repo ($SKEIN_REPO, else cwd) so a *relative* command resolves —
    // e.g. SKEIN_LAUNCH_CMD='dev-sandbox/setup-sandbox.sh {branch}' works without an absolute path.
    let run_dir = std::env::var("SKEIN_REPO")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    if let Some(dir) = run_dir {
        cmd.cwd(dir);
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

    // Periodic ping surfaces a browser that vanished without a Close frame, so we reap the PTY
    // promptly instead of leaving `sbx` running until the box happens to emit output.
    let mut keepalive = tokio::time::interval(Duration::from_secs(30));
    keepalive.tick().await; // the first tick fires immediately — discard it

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
            _ = keepalive.tick() => {
                if socket.send(Message::Ping(Vec::new())).await.is_err() { break; }
            }
        }
    }

    // Reap the child so it doesn't linger as a zombie. Kill, then wait off the async runtime
    // (Child::wait blocks); dropping `master`/the reader closes the PTY so descendants get SIGHUP.
    let _ = child.kill();
    let _ = tokio::task::spawn_blocking(move || child.wait()).await;
}

#[cfg(test)]
mod tests {
    use super::origin_ok;
    use axum::http::{header::ORIGIN, HeaderMap, HeaderValue};

    fn with_origin(o: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(o) = o {
            h.insert(ORIGIN, HeaderValue::from_str(o).unwrap());
        }
        h
    }

    #[test]
    fn origin_guard_allows_local_and_tailnet_blocks_others() {
        assert!(origin_ok(&with_origin(None))); // non-browser client
        assert!(origin_ok(&with_origin(Some("http://127.0.0.1:7878"))));
        assert!(origin_ok(&with_origin(Some("http://localhost:7878"))));
        assert!(origin_ok(&with_origin(Some("https://box.my-tnet.ts.net"))));
        assert!(!origin_ok(&with_origin(Some("https://evil.com"))));
        // a look-alike that only *contains* ts.net must not pass
        assert!(!origin_ok(&with_origin(Some(
            "https://box.ts.net.evil.com"
        ))));
    }

    #[test]
    fn origin_guard_honours_allowlist() {
        std::env::set_var("SKEIN_ALLOWED_ORIGINS", "proxy.local, other.host");
        assert!(origin_ok(&with_origin(Some("https://proxy.local"))));
        assert!(!origin_ok(&with_origin(Some("https://nope.local"))));
        std::env::remove_var("SKEIN_ALLOWED_ORIGINS");
    }
}
