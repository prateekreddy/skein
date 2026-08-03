//! skein-server — the web cockpit. A tiny axum server over `skein` (lib):
//!   GET /                          the dark web board (self-contained, no build)
//!   GET /api/boxes                 fleet snapshot (JSON)
//!   GET /api/events                live fleet stream (SSE)
//!   GET /api/boxes/:name/terminal  WebSocket ↔ PTY ↔ persistent in-box tmux session
//!
//! The terminal reuses wheels: portable-pty (server PTY) + xterm.js (browser). We write only the
//! WS↔PTY bridge. Bind is loopback-only by default; for remote access either `tailscale serve`
//! proxies to this loopback port, or `$SKEIN_ADDR` binds off-loopback and the box is reached at its
//! tailnet address directly. The tailnet is the auth boundary either way. See README "Remote access".

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
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
// marked.js renders repo markdown in the Files tab — the "read the docs without leaving skein" bit.
const MARKED_JS: &str = include_str!("../web/vendor/marked.min.js");
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
    // Cross-project mailbox relay: a box only ever mounts its own project's store, so a message
    // addressed "all-projects" / "project:<id>" / to a vmid living in another project can only be
    // delivered by the host, which already reads every managed store. Its own standalone loop
    // (not piggybacked on the per-connection SSE ticker below) so relay keeps running even when no
    // browser tab has the cockpit open.
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            if let Err(e) = skein::relay_cross_project_mail() {
                eprintln!("skein: mailbox relay: {e}");
            }
        }
    });
    // Bind is loopback-only by default; $SKEIN_ADDR overrides it — e.g. `SKEIN_ADDR=0.0.0.0:7878`
    // to listen on every interface (reachable at the box's tailnet IP/hostname), or a specific host
    // like `SKEIN_ADDR=<tailnet-ip>:7878`. The terminal's origin guard already trusts `*.ts.net` and
    // tailnet IPs, so either the `tailscale serve` hostname or a raw off-loopback bind works with no
    // per-host config; the tailnet is the auth boundary in both cases (see README "Remote access").
    let addr = std::env::var("SKEIN_ADDR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_ADDR.into());
    let app = Router::new()
        .route("/", get(index))
        .route("/vendor/xterm.js", get(vendor_xterm_js))
        .route("/vendor/xterm.css", get(vendor_xterm_css))
        .route("/vendor/addon-fit.js", get(vendor_fit_js))
        .route("/vendor/marked.js", get(vendor_marked_js))
        .route("/api/boxes/:name/files", get(api_files))
        .route("/api/boxes/:name/file", get(api_file))
        .route("/api/boxes", get(api_boxes))
        .route("/api/health", get(api_health))
        .route("/api/runtimes", get(api_runtimes))
        .route("/api/repos", get(api_repos).post(api_add_repo))
        .route("/api/repos/:id", axum::routing::delete(api_remove_repo))
        .route("/api/repos/:id/pull", post(api_pull_repo))
        .route("/api/repos/:id/settings", post(api_set_repo_settings))
        .route("/api/settings", get(api_settings).post(api_set_settings))
        .route("/api/fleet/resize", post(api_fleet_resize))
        .route("/api/fleet/limits", post(api_fleet_limits))
        .route("/api/sync", get(api_sync_status))
        .route("/api/sync/connections", post(api_save_connection))
        .route(
            "/api/sync/connections/:id",
            axum::routing::delete(api_remove_connection),
        )
        .route(
            "/api/sync/connections/:id/token",
            axum::routing::delete(api_forget_connection_token),
        )
        .route("/api/boxes/:name/sync", post(api_sync_provision))
        .route("/api/boxes/:name/sync/refresh", post(api_sync_refresh))
        .route("/api/pick-path", post(api_pick_path))
        .route("/api/boxes/:name/diff", get(api_diff))
        .route("/api/boxes/:name/session", get(api_session))
        .route("/api/boxes/:name/statusline", get(api_statusline))
        .route("/api/mailbox", get(api_mailbox).post(api_mailbox_send))
        .route("/api/boxes/:name/ship", get(api_ship))
        .route("/api/boxes/:name/pr", post(api_pr))
        .route("/api/boxes/:name/merge", post(api_merge))
        .route(
            "/api/boxes/:name/verify",
            get(api_verify_last).post(api_verify_run),
        )
        .route("/api/boxes/:name/transcript", get(api_transcript))
        .route("/api/boxes/:name/repin", post(api_repin))
        .route("/api/boxes/:name/resume", post(api_resume))
        .route("/api/boxes/:name/restart-agent", post(api_restart_agent))
        .route("/api/boxes/:name/takeover", post(api_takeover))
        .route("/api/boxes/:name/narrate", get(api_narrate))
        .route("/api/resume-batch", post(api_resume_batch))
        .route("/api/boxes/:name/stop", post(api_stop))
        .route("/api/boxes/:name/destroy", post(api_destroy))
        .route(
            "/api/boxes/:name/upload",
            // Attachments (screenshots, PDFs, videos, whole folders) dwarf axum's 2 MB default body
            // cap. The handler streams the body straight into the box instead of buffering it, so the
            // cap is disabled here and enforced per-upload by UPLOAD_CAP as the bytes go past.
            post(api_upload).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/api/events", get(api_events))
        .route("/api/boxes/:name/terminal", get(terminal));

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("skein-server: cannot bind {addr}: {e}"));
    println!("skein-server → http://{addr}");
    // Flag an off-loopback bind so it's never a surprise that the port is reachable from the network.
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
    if !matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]") {
        println!(
            "  bound off-loopback — reachable from the network; rely on the tailnet/ACLs as the auth boundary"
        );
    }

    // Our own accept loop instead of `axum::serve`, solely so we can set TCP_NODELAY per
    // connection. The terminal sends one keystroke per packet; with Nagle's algorithm on, those
    // tiny writes get held back and coalesced with the next, which shows up as the echo arriving
    // in bursts. `serve_connection_with_upgrades` keeps the WebSocket upgrade path intact.
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder as ConnBuilder;
    use hyper_util::service::TowerToHyperService;
    use tower::Service;
    let mut make = app.into_make_service();
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let _ = stream.set_nodelay(true);
        let svc = match make.call(()).await {
            Ok(s) => s,
            Err(e) => match e {}, // IntoMakeService is Infallible — this arm is unreachable
        };
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let _ = ConnBuilder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, TowerToHyperService::new(svc))
                .await;
        });
    }
}

async fn index() -> Response {
    // The UI is embedded in and version-coupled to this binary. Reusing an older document after a
    // server restart mixes stale JS/CSS with new API behaviour, so the browser must revalidate it.
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        INDEX,
    )
        .into_response()
}

async fn vendor_xterm_js() -> Response {
    static_asset(XTERM_JS, "application/javascript; charset=utf-8")
}
async fn vendor_fit_js() -> Response {
    static_asset(FIT_JS, "application/javascript; charset=utf-8")
}
async fn vendor_marked_js() -> Response {
    static_asset(MARKED_JS, "application/javascript; charset=utf-8")
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
/// Allowed: loopback (local use); any `*.ts.net` host and any Tailscale IP (the tailnet is the auth
/// boundary — reached either via `tailscale serve`, whose origin is the `.ts.net` name, or by hitting
/// an off-loopback bind at the box's raw tailnet address); and any host in `$SKEIN_ALLOWED_ORIGINS`
/// (comma-separated) for other reverse proxies. Non-browser clients send no `Origin` and are allowed.
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
    if matches!(host, "localhost" | "127.0.0.1" | "::1")
        || host.ends_with(".ts.net")
        || is_tailnet_ip(host)
    {
        return true;
    }
    std::env::var("SKEIN_ALLOWED_ORIGINS").is_ok_and(|list| {
        list.split(',')
            .map(str::trim)
            .any(|h| !h.is_empty() && h == host)
    })
}

/// Is `host` an IP literal inside Tailscale's assigned ranges — CGNAT `100.64.0.0/10` (IPv4) or the
/// `fd7a:115c:a1e0::/48` ULA prefix (IPv6)? Lets a raw tailnet address be a valid terminal origin
/// when the operator binds off-loopback, without listing each box's IP in `$SKEIN_ALLOWED_ORIGINS`.
/// A non-IP host (e.g. `evil.com`) never parses, so this only ever widens access to the tailnet.
fn is_tailnet_ip(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            let o = v4.octets();
            o[0] == 100 && (64..=127).contains(&o[1]) // 100.64.0.0/10
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 // fd7a:115c:a1e0::/48
        }
        Err(_) => false,
    }
}

/// Snapshot of the fleet. `load_views` is blocking (subprocess `sbx ls` + per-box `git` + journal
/// reads), so it runs on the blocking pool, never inline on an async worker — see the note on
/// `api_events` for why blocking a worker here would stall concurrent terminal websockets.
async fn api_boxes() -> Json<Vec<BoxView>> {
    let views = tokio::task::spawn_blocking(|| load_views().unwrap_or_default())
        .await
        .unwrap_or_default();
    Json(views)
}

/// Provider-neutral custom footer. Claude renders it natively from stdin; Codex maps its latest
/// token_count event (the `/status` data source) through the same renderer for the browser terminal.
async fn api_statusline(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::agent_statusline(&name)).await {
        Ok(Ok(line)) => Json(serde_json::json!({ "line": line })).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct TakeoverReq {
    target: String,
}

async fn api_takeover(Path(name): Path<String>, Json(request): Json<TakeoverReq>) -> Response {
    if !skein::valid_name(&name) || !skein::valid_runtime(&request.target) {
        return (StatusCode::BAD_REQUEST, "invalid box or target runtime").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::replace_box(&name, &request.target)).await {
        Ok(Ok(replacement)) => Json(replacement).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// A repo's own settings. Absent field = leave it alone; empty string = clear it. One request can
/// carry all three, so the settings pane saves a card, not a keystroke.
#[derive(Deserialize)]
struct RepoSettingsReq {
    check: Option<String>,
    /// a Plane project URL or bare uuid — what this repo's tracker tokens bind to
    plane_project: Option<String>,
    /// which work-tracking connection this repo claims through, by id; empty = not tracked
    sync_connection: Option<String>,
}

async fn api_set_repo_settings(
    Path(id): Path<String>,
    Json(req): Json<RepoSettingsReq>,
) -> Response {
    match skein::set_repo_settings(
        &id,
        req.check.as_deref(),
        req.plane_project.as_deref(),
        req.sync_connection.as_deref(),
    ) {
        Ok(repo) => Json(repo).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// The configured work-tracking connections. Never carries a Plane token — only whether one is
/// stored, which is the whole question the settings screen needs answered.
async fn api_sync_status() -> Json<skein::SyncStatus> {
    Json(skein::sync_status())
}

#[derive(Deserialize)]
struct ConnectionReq {
    /// absent ⇒ create one, with an id derived from the gateway host
    id: Option<String>,
    #[serde(default)]
    label: String,
    gateway_url: String,
    /// absent ⇒ leave whatever is stored alone. A blank token field means "I came here to change
    /// the URL", never "forget my credential" — forgetting has its own route.
    token: Option<String>,
}

/// Create or update a connection, optionally storing its token in the same act. The token is
/// write-only by design: no route reads one back, so a credential that reaches the host cannot
/// leave it again.
async fn api_save_connection(Json(req): Json<ConnectionReq>) -> Response {
    match skein::upsert_connection(
        req.id.as_deref(),
        &req.label,
        &req.gateway_url,
        req.token.as_deref().filter(|t| !t.trim().is_empty()),
    ) {
        Ok(_) => Json(skein::sync_status()).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Forget a connection entirely. Refused while a repo still selects it — see `remove_connection`.
async fn api_remove_connection(Path(id): Path<String>) -> Response {
    match skein::remove_connection(&id) {
        Ok(()) => Json(skein::sync_status()).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Forget one connection's stored token, keeping the connection itself.
async fn api_forget_connection_token(Path(id): Path<String>) -> Response {
    match skein::set_connection_token(&id, "") {
        Ok(()) => Json(skein::sync_status()).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Mint this box a tracker token and register the `sync` MCP server inside it. Spends a network
/// round trip and creates a real credential, so — like verify — it only ever happens on a click.
async fn api_sync_provision(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::sync_provision_box(&name)).await {
        Ok(Ok(note)) => Json(serde_json::json!({ "ok": true, "note": note })).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// Re-apply the work-tracking documents to a box that already has them.
///
/// `?replace=1` covers boxes installed before skein recorded what it wrote, where stale and edited
/// cannot be told apart. Even then it refuses documents it can see were edited, so this is never the
/// blunt instrument its name suggests.
async fn api_sync_refresh(
    Path(name): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let force = q.get("replace").is_some_and(|v| v == "1" || v == "true");
    match tokio::task::spawn_blocking(move || skein::sync_refresh_box(&name, force)).await {
        Ok(Ok(note)) => Json(serde_json::json!({ "ok": true, "note": note })).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// The box's conversation as its own record has it — survives a reboot, a server restart, a page
/// reload and the scrollback limit, none of which the rendered terminal does. `?bytes=` is how much
/// of the tail to read; the cockpit doubles it to page backwards.
async fn api_transcript(
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let bytes = q
        .get("bytes")
        .and_then(|b| b.parse::<u64>().ok())
        .unwrap_or(256 * 1024);
    match tokio::task::spawn_blocking(move || skein::read_transcript(&name, bytes)).await {
        Ok(Ok(view)) => Json(view).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// Run the box's check command inside it and report the outcome. Slow by nature (it's a test
/// suite), single-flight fleet-wide, and refused while the agent is mid-turn — see
/// [`skein::run_verify`]. Only ever reached by a click: nothing schedules this.
async fn api_verify_run(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::run_verify(&name)).await {
        Ok(Ok(record)) => Json(record).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// The last check recorded for a box, output and all. 404 when none has ever run.
async fn api_verify_last(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match skein::read_verify(&name) {
        Some(record) => Json(record).into_response(),
        None => (StatusCode::NOT_FOUND, "no check has run in this box yet").into_response(),
    }
}

/// The branch-vs-base patch a box last reported (plain text; empty when none yet).
async fn api_diff(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    // Computed inside the box, so it forks a git in a sandbox — off the async runtime, like every
    // other blocking box call.
    let view = tokio::task::spawn_blocking(move || skein::box_diff(&name))
        .await
        .ok()
        .flatten();
    match view {
        Some(view) => Json(view).into_response(),
        None => Json(serde_json::json!({
            "patch": "",
            "base": "",
            "source": "none",
            "note": "no diff yet — start the box, or wait for it to finish a turn",
        }))
        .into_response(),
    }
}

/// List a directory in a box's host-side workspace (`?path=rel/dir`, default root). Traversal,
/// absolute paths, and symlink escapes are rejected in the lib (resolve_in_workspace).
async fn api_files(Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let rel = q.get("path").cloned().unwrap_or_default();
    let res = tokio::task::spawn_blocking(move || skein::list_box_files(&name, &rel))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    match res {
        Ok(listing) => Json(listing).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e).into_response(),
    }
}

/// Read a file in a box's host-side workspace (`?path=rel/file`). Text-ish types are served as
/// text/plain (the UI renders markdown itself), images with their own type so <img> works, and
/// anything else as octet-stream. `X-Truncated: 1` marks a read capped at FILE_READ_CAP.
async fn api_file(Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let rel = q.get("path").cloned().unwrap_or_default();
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let res = tokio::task::spawn_blocking(move || skein::read_box_file(&name, &rel))
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    let (bytes, truncated) = match res {
        Ok(v) => v,
        Err(e) => return (StatusCode::NOT_FOUND, e).into_response(),
    };
    let ct = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ if bytes.contains(&0) => "application/octet-stream", // NUL byte ⇒ not text
        _ => "text/plain; charset=utf-8",
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, ct),
            (
                axum::http::HeaderName::from_static("x-truncated"),
                if truncated { "1" } else { "0" },
            ),
        ],
        bytes,
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

/// Runtime choices come from the core adapter registry so every client stays in sync when a new
/// provider is added.
async fn api_runtimes() -> Json<Vec<skein::RuntimeInfo>> {
    Json(skein::supported_runtimes())
}

async fn api_health() -> Json<skein::HealthReport> {
    Json(
        tokio::task::spawn_blocking(skein::health_report)
            .await
            .unwrap_or_else(|error| skein::HealthReport {
                ok: false,
                registry: skein::HealthCheck {
                    ok: false,
                    detail: error.to_string(),
                },
                sbx: skein::HealthCheck {
                    ok: false,
                    detail: "health task failed".into(),
                },
                git: skein::HealthCheck {
                    ok: false,
                    detail: "health task failed".into(),
                },
                gh: skein::HealthCheck {
                    ok: false,
                    detail: "health task failed".into(),
                },
                ai: skein::HealthCheck {
                    ok: true,
                    detail: "health task failed".into(),
                },
                probes: skein::HealthCheck {
                    ok: false,
                    detail: "health task failed".into(),
                },
                mailbox: skein::HealthCheck {
                    ok: false,
                    detail: "health task failed".into(),
                },
                dark_boxes: Vec::new(),
                stale_boxes: Vec::new(),
                runtimes: skein::supported_runtimes(),
            }),
    )
}

#[derive(Deserialize)]
struct AddRepoReq {
    source: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    agent: String,
    /// Shared-data folder for the repo (its `.claude` store). Empty ⇒ skein manages one under its home.
    #[serde(default)]
    store: String,
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
        let store = (!r.store.trim().is_empty()).then(|| r.store.trim().to_string());
        skein::add_repo(
            r.source.trim(),
            id.as_deref(),
            agent.as_deref(),
            store.as_deref(),
        )
    })
    .await;
    match res {
        Ok(Ok(repo)) => {
            // Warn up-front if the push path is shaky (no origin, or SSH without a loaded key).
            let warning = skein::remote_warning(&repo.work);
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

/// Pull the latest code into a repo's working clone (fast-forward only). `git pull` hits the network,
/// so run the blocking work off the async runtime.
async fn api_pull_repo(Path(id): Path<String>) -> Response {
    let res = tokio::task::spawn_blocking(move || skein::pull_repo(&id)).await;
    match res {
        Ok(Ok(summary)) => Json(serde_json::json!({ "summary": summary })).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
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

/// Push the current per-box ceilings onto every running box.
///
/// Separate from resize, and cheap where that is expensive: a cgroup limit is live, so this changes
/// the cap on a running box with no restart, no snapshot and nothing to restore. 200 with the boxes
/// that could not be adjusted — one box missing its cgroup must not stop the rest being corrected.
async fn api_fleet_limits() -> Response {
    match skein::apply_box_limits() {
        Ok(failed) => Json(serde_json::json!({ "failed": failed })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Change the fleet sandbox's memory/CPUs, carrying every box across.
///
/// Its own endpoint rather than a side effect of saving settings, because it is destructive and
/// slow: sbx fixes both at creation, so this rebuilds the sandbox and every box in it. Saving the
/// numbers alone only changes what the NEXT create uses — which is why the settings pane offers this
/// separately rather than appearing to apply them and quietly doing nothing.
///
/// 200 with the boxes that failed to come back: their work is already snapshotted on the host, so a
/// partial return is a retry (`skein start <box>`), not a failure of the resize.
async fn api_fleet_resize(Json(r): Json<ResizeReq>) -> Response {
    match skein::resize_fleet(&r.memory, &r.cpus) {
        Ok(failed) => Json(serde_json::json!({ "failed": failed })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct ResizeReq {
    #[serde(default)]
    memory: String,
    #[serde(default)]
    cpus: String,
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

#[derive(Deserialize, Default)]
struct PickPathReq {
    kind: String, // "file" → file picker; anything else → folder picker
}

/// Pop the host's native folder/file dialog (skein-server runs on the host). Returns {ok, path} on a
/// pick, {ok, cancelled} if dismissed, or {ok:false, error} when no GUI picker is available (headless
/// / remote — the user types the path instead).
async fn api_pick_path(Json(r): Json<PickPathReq>) -> Json<serde_json::Value> {
    let kind = if r.kind == "file" { "file" } else { "folder" }.to_string();
    let res = tokio::task::spawn_blocking(move || skein::pick_path(&kind)).await;
    Json(match res {
        Ok(Ok(Some(path))) => serde_json::json!({ "ok": true, "path": path }),
        Ok(Ok(None)) => serde_json::json!({ "ok": true, "cancelled": true }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

#[derive(Deserialize)]
struct RepinReq {
    branch: String,
}

/// Re-pin an existing box's launch spec to a different branch, without relaunching it — for a box
/// whose agent has moved off its recorded branch (e.g. branch-per-slice work) and keeps getting
/// checked back onto the stale one every reconnect. Takes effect on the box's next reconnect.
/// Returns {ok, error?}.
async fn api_repin(Path(name): Path<String>, Json(r): Json<RepinReq>) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let branch = r.branch;
    let res = tokio::task::spawn_blocking(move || skein::repin_branch(&name, &branch)).await;
    Json(match res {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
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

#[derive(Deserialize, Default)]
struct RestartAgentReq {
    #[serde(default)]
    runtime: String,
}

async fn api_restart_agent(
    Path(name): Path<String>,
    body: Option<Json<RestartAgentReq>>,
) -> Json<serde_json::Value> {
    if !skein::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let runtime = body
        .map(|Json(value)| value.runtime)
        .filter(|value| !value.trim().is_empty());
    let result = tokio::task::spawn_blocking(move || {
        skein::restart_agent_session(&name, runtime.as_deref())
    })
    .await;
    Json(match result {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(error)) => serde_json::json!({ "ok": false, "error": error }),
        Err(error) => serde_json::json!({ "ok": false, "error": error.to_string() }),
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

/// Receive one attachment — pasted, dropped, or picked — and stream it into the box, returning its
/// in-box path for the agent to reference. The agent lives in the microVM: it can't see the user's
/// clipboard or filesystem, so this is the only bridge from "a file on my laptop" to "a path the
/// agent can open". Any type: image, PDF, video, archive, source file.
///
/// Raw body (not multipart) so the bytes go straight from the socket to `sbx exec -i … cat >` with no
/// buffering — a 2 GB video costs the host no memory. `X-Skein-Name` carries the file's name
/// (percent-encoded; may include a relative dir when a folder is dropped) and `X-Skein-Drop` groups
/// every file of one drop into a single `/tmp/skein-drop-<batch>/` tree.
async fn api_upload(
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Json<serde_json::Value> {
    match stream_upload(&name, &headers, body).await {
        Ok(path) => serde_json::json!({ "ok": true, "path": path }).into(),
        Err(e) => serde_json::json!({ "ok": false, "error": e }).into(),
    }
}

/// Per-attachment ceiling. Streaming means the *host* never buffers the upload, but the box's /tmp is
/// finite — this keeps a runaway (or fat-fingered) upload from filling the sandbox's disk.
const UPLOAD_CAP: u64 = 2 * 1024 * 1024 * 1024;

async fn stream_upload(
    name: &str,
    headers: &axum::http::HeaderMap,
    body: axum::body::Body,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt as _;
    let hdr = |k: &'static str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let batch = hdr("x-skein-drop");
    let mut rel = skein::pct_decode(&hdr("x-skein-name"));
    if rel.trim().is_empty() {
        // A clipboard paste often has no filename. Name it from the content type so the suffix still
        // says what it is (an agent keys off `.png` to treat it as an image).
        let ext = hdr("content-type")
            .split(';')
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        let ext = if ext.is_empty() { "bin".into() } else { ext };
        rel = format!("paste.{ext}");
    }
    let (dir, path) = skein::drop_dest(&batch, &rel)?;
    let argv = skein::box_write_argv(name, &dir, &path)?;
    let mut child = tokio::process::Command::new("sbx")
        .args(&argv)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("sbx exec not runnable: {e}"))?;
    let mut sink = child.stdin.take().ok_or("no stdin pipe")?;
    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    // Collect the failure instead of returning from inside the loop: the partial file has to be
    // cleaned up on the way out. An over-cap upload is exactly the case that would otherwise leave
    // gigabytes of junk in the box's /tmp — the thing the cap exists to prevent.
    let mut failed: Option<String> = None;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                failed = Some(format!("upload interrupted: {e}"));
                break;
            }
        };
        total += chunk.len() as u64;
        if total > UPLOAD_CAP {
            failed = Some(format!("too large (cap {} MB)", UPLOAD_CAP / (1024 * 1024)));
            break;
        }
        if let Err(e) = sink.write_all(&chunk).await {
            failed = Some(format!("writing file to box: {e}"));
            break;
        }
    }
    sink.shutdown().await.ok();
    drop(sink); // EOF for `cat`
    if let Some(e) = failed {
        let _ = child.kill().await;
        discard_partial(name, &path).await;
        return Err(e);
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("sbx exec failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "sbx exec failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(path)
}

/// Best-effort removal of a half-written attachment, so a failed upload leaves nothing for the agent
/// to mistake for the real file. Bounded: a wedged box must not hold the response open.
async fn discard_partial(name: &str, path: &str) {
    let inner = format!("rm -f {}", skein::sh_quote(path));
    let child = tokio::process::Command::new("sbx")
        .args(["exec", name, "sh", "-c", &inner])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();
    if let Ok(mut child) = child {
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    }
}

/// Live fleet stream: re-emits the fleet every 2s as an SSE `boxes` event.
/// (Roadmap: replace polling with a honker subscription so it's push, not poll.)
async fn api_events() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // `.then` (async), NOT `.map` (sync): `load_views` shells out to `sbx ls` and a per-box `git`
    // subprocess plus journal/diff reads — 1-2s of synchronous work for a busy fleet. Running it
    // inline on the async worker (as `.map` did) blocks that worker for the whole computation every
    // 2s, and any terminal websocket scheduled on the same worker is starved for that window. That
    // was the "typing lags only when the box is idle" freeze: mid-stream the output flood masks the
    // gap, but at rest a lone keystroke's echo waits out the stall. Offload to the blocking pool so
    // the async runtime stays free to pump the terminal sockets — matching every other blocking
    // `skein::`/`load_*` call in this file.
    let stream =
        IntervalStream::new(tokio::time::interval(Duration::from_secs(2))).then(|_| async {
            let views = tokio::task::spawn_blocking(|| load_views().unwrap_or_default())
                .await
                .unwrap_or_default();
            let payload = serde_json::to_string(&views).unwrap_or_else(|_| "[]".into());
            Ok(Event::default().event("boxes").data(payload))
        });
    Sse::new(stream)
}

/// Upgrade to a WebSocket that bridges the browser terminal to a PTY.
/// `?launch=<branch>` creates the box, then attaches to its first persistent agent session.
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
    let agent = q.get("agent").filter(|a| skein::valid_runtime(a)).cloned();
    let handoff = q
        .get("handoff")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let from = q.get("from").filter(|a| skein::valid_runtime(a)).cloned();
    ws.on_upgrade(move |socket| terminal_session(socket, name, launch, shell, agent, handoff, from))
}

/// The WS↔PTY bridge: attach to an in-box tmux session through `sbx exec`, pipe bytes both ways,
/// and honour resizes.
/// Override the spawned command with $SKEIN_ATTACH_CMD (run via `sh -c`) for local testing.
async fn terminal_session(
    mut socket: WebSocket,
    name: String,
    launch: Option<String>,
    shell: bool,
    agent: Option<String>,
    handoff: bool,
    from: Option<String>,
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
    let target_agent = agent.unwrap_or_else(|| skein::agent_for_box(&name));
    if handoff && !shell {
        let hn = name.clone();
        let ht = target_agent.clone();
        let hf = from.clone();
        match tokio::task::spawn_blocking(move || skein::prepare_handoff(&hn, hf.as_deref(), &ht))
            .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                let _ = socket
                    .send(Message::Text(format!(
                        "skein: handoff brief failed: {e}\r\n"
                    )))
                    .await;
            }
            Err(e) => {
                let _ = socket
                    .send(Message::Text(format!(
                        "skein: handoff task failed: {e}\r\n"
                    )))
                    .await;
            }
        }
    }

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
    // Default: reconnect to the box's *existing* provider-specific tmux session,
    // rooted at the dir the box registered.
    //
    // $SKEIN_ATTACH_CMD fully overrides it (run via `sh -c`) — `{name}` and `{dir}` in the
    // value are substituted first, so you can tune the exact sbx invocation per box without
    // recompiling, e.g. SKEIN_ATTACH_CMD='sbx exec -it {name} tmux attach -t skein-agent'
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
        // create-a-box mode: provision without an agent attach, then enter the same tmux-backed
        // agent session all future UI reloads reconnect to.
        let mut b = CommandBuilder::new("sh");
        b.arg("-c");
        b.arg(skein::launch_command_with_agent(
            &name,
            branch,
            Some(&target_agent),
        ));
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
                    skein::attach_argv_as(&name, &dir, &target_agent)
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
                Some(Ok(Message::Binary(b))) => {
                    let _ = in_tx.send(b).await;
                }
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
    fn origin_guard_allows_raw_tailnet_ip_but_not_public_ip() {
        // A box reached by its raw Tailscale address (off-loopback bind) — CGNAT 100.64.0.0/10 v4
        // and the fd7a:115c:a1e0::/48 v6 prefix — is a valid origin without any per-IP allowlisting.
        assert!(origin_ok(&with_origin(Some("http://100.64.0.30:7878"))));
        assert!(origin_ok(&with_origin(Some("http://100.64.0.1:7878"))));
        assert!(origin_ok(&with_origin(Some(
            "http://[fd7a:115c:a1e0::1]:7878"
        ))));
        // 100.x outside the /10, ordinary LAN/public IPs, and non-tailnet v6 must still be blocked.
        assert!(!origin_ok(&with_origin(Some("http://100.128.0.1:7878"))));
        assert!(!origin_ok(&with_origin(Some("http://192.168.1.10:7878"))));
        assert!(!origin_ok(&with_origin(Some("http://8.8.8.8"))));
        assert!(!origin_ok(&with_origin(Some("http://[2001:db8::1]:7878"))));
    }

    #[test]
    fn origin_guard_honours_allowlist() {
        std::env::set_var("SKEIN_ALLOWED_ORIGINS", "proxy.local, other.host");
        assert!(origin_ok(&with_origin(Some("https://proxy.local"))));
        assert!(!origin_ok(&with_origin(Some("https://nope.local"))));
        std::env::remove_var("SKEIN_ALLOWED_ORIGINS");
    }
}
