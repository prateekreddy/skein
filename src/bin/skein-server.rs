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

/// The server takes no subcommands — it is configured entirely by environment ($SKEIN_ADDR et al).
/// It used to ignore argv outright, so `skein-server doctor` booted the cockpit and swallowed the
/// word "doctor": you get a running server and no hint that your command went nowhere. Refuse
/// instead, and say where the subcommand actually lives.
fn refuse_unknown_args(args: &[String]) -> Option<String> {
    match args.first().map(String::as_str) {
        None => None,
        Some("--help" | "-h") => Some(format!(
            "skein-server {} — the web cockpit; no subcommands.\n\
             configure with the environment: $SKEIN_ADDR (default {DEFAULT_ADDR}), $SKEIN_REGISTRY.\n\
             fleet commands live on the other binary: `skein ls`, `skein doctor`, `skein attach <box>`.",
            env!("CARGO_PKG_VERSION")
        )),
        Some("--version" | "-v") => Some(format!("skein-server {}", env!("CARGO_PKG_VERSION"))),
        Some(other) => Some(format!(
            "skein-server takes no arguments (got {other:?}) — it is the web cockpit, not the CLI.\n\
             did you mean `skein {other}`?   (build both: cargo build --release)"
        )),
    }
}

#[tokio::main]
async fn main() {
    // Argv check first: before the port bind, and before ensure_probe_all/ensure_kit write anything.
    // A mistyped invocation should change nothing on disk.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(msg) = refuse_unknown_args(&args) {
        let help = matches!(args[0].as_str(), "--help" | "-h" | "--version" | "-v");
        if help {
            println!("{msg}");
        } else {
            eprintln!("skein-server: {msg}");
        }
        std::process::exit(if help { 0 } else { 2 });
    }
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
    // Bring an existing fleet sandbox into line with this binary: it keeps the launcher and the
    // ceilings it was last given, and an upgrade that changes what skein passes the launcher stops
    // every box in that fleet starting until the copy out there is replaced. A restart is the only
    // moment that mismatch is observable. Skips a sleeping fleet rather than booting a VM to fix it.
    if let Err(e) = skein::heal_fleet() {
        eprintln!("skein: could not heal the fleet sandbox ({e}); boxes may start with a stale launcher or stale ceilings");
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
    // Keep every running box's GitHub write token ahead of its expiry.
    //
    // An App installation token lives one hour, so this refreshes on a wide margin rather than close
    // to the edge: a token that lapses does not fail loudly, it silently demotes the box to the
    // read-only credential and the next push comes back 403 with nothing to explain it. Twenty
    // minutes gives two clear misses before that happens.
    //
    // It is also the only thing that *withdraws* a token — expiry and revocation both take effect
    // here — so it runs whether or not a browser has the cockpit open, and does nothing at all
    // until a GitHub App is configured.
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(20 * 60));
        loop {
            tick.tick().await;
            let _ = tokio::task::spawn_blocking(|| {
                // The board's own list, so a destroyed box is not minted for and a box skein cannot
                // currently see is simply skipped this round rather than losing its token.
                for view in load_views().unwrap_or_default() {
                    for problem in skein::gitgate::refresh_tokens(&view.name) {
                        eprintln!("skein: git token for {}: {problem}", view.name);
                    }
                }
            })
            .await;
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
        .route("/api/fleet/resources", get(api_fleet_resources))
        .route("/api/fleet/load", get(api_fleet_load))
        .route("/api/fleet/transport", get(api_fleet_transport))
        .route("/api/fleet/substrate", get(api_substrate))
        .route("/api/fleet/substrate/:id", post(api_substrate_decide))
        .route("/api/fleet/git-grants", get(api_git_grants))
        .route("/api/fleet/git-grants/:id", post(api_git_grant_decide))
        .route(
            "/api/fleet/git-grants/:name/:repo",
            axum::routing::delete(api_git_grant_revoke),
        )
        .route("/api/boxes/:name/git-scope", post(api_set_box_git_scope))
        .route("/api/fleet/git-probe", post(api_git_probe))
        .route("/api/fleet/git-credentials", post(api_git_credential))
        .route(
            "/api/fleet/git-credentials/:id",
            axum::routing::delete(api_git_credential_remove),
        )
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
        .route("/api/boxes/:name/tracking", post(api_set_box_tracking))
        .route("/api/boxes/:name/identity", post(api_set_box_identity))
        .route("/api/boxes/:name/disk", post(api_set_box_disk))
        .route("/api/boxes/:name/settings", get(api_box_settings))
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

/// Record which work tracker a box claims through — or that it claims through none.
///
/// Written *before* the box launches, so provisioning finds the answer already there rather than
/// minting a token against the repo's default and having it corrected afterwards. Absent
/// `connection` clears the override and returns the box to its repo's setting.
async fn api_set_box_tracking(Path(name): Path<String>, Json(r): Json<TrackingReq>) -> Response {
    match skein::set_box_tracking(&name, r.connection.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Who a box commits as, when it should not be the configured default.
///
/// Recorded before the box starts, like its tracking choice: the identity is written into the box's
/// HOME during provisioning, and a correction afterwards would arrive after the first commit.
async fn api_set_box_identity(Path(name): Path<String>, Json(r): Json<IdentityReq>) -> Response {
    let who = match (r.name.as_deref(), r.email.as_deref()) {
        (None, None) => None,
        (n, e) => Some((n.unwrap_or_default(), e.unwrap_or_default())),
    };
    match skein::set_box_identity(&name, who) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Everything settable about ONE box, and what it would be if nothing were set.
///
/// Both halves, because a per-box panel that shows only overrides shows mostly blanks: the useful
/// question is "what does this box do today, and is that its own choice or the default?" — so each
/// field carries the override (possibly empty) and the value in force.
async fn api_box_settings(Path(name): Path<String>) -> Response {
    if !skein::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let config = skein::load_config();
    let repo = skein::repo_for_box(&name);
    let (git_name, git_email) = match &repo {
        Some(repo) => skein::box_identity(&name, repo),
        None => (config.git_name.clone(), config.git_email.clone()),
    };
    let own_identity = skein::box_identity_override(&name);
    Json(serde_json::json!({
        "name": name,
        "repo": repo.as_ref().map(|r| r.id.clone()).unwrap_or_default(),
        // the box's own choice, empty when it inherits
        "connection": skein::box_tracking(&name).unwrap_or_default(),
        // "" is a real answer (claims nowhere); absent is inheritance. A bare string cannot say which.
        "has_tracking_override": skein::box_tracking(&name).is_some(),
        "own_git_name": own_identity.clone().map(|(n, _)| n).unwrap_or_default(),
        "own_git_email": own_identity.map(|(_, e)| e).unwrap_or_default(),
        // The box's own answer, "" when it inherits — same grammar as tracking above. `effective`
        // is what it will actually come up with, which is not derivable in the page: it depends on
        // the fleet default *and* on whether a write token can be issued at all.
        "git_scope": std::fs::read_to_string(
            std::path::Path::new(&skein::box_state(&name)).join("git-scope"),
        )
        .unwrap_or_default()
        .trim()
        .to_string(),
        "effective_git_scope": if skein::gitgate::box_is_scoped(&name) { "repo" } else { "fleet" },
        "git_scope_available": skein::gitgate::can_issue_write_tokens(),
        "own_disk": std::fs::read_to_string(
            std::path::Path::new(&skein::box_state(&name)).join("disk"),
        )
        .unwrap_or_default()
        .trim()
        .to_string(),
        // and what is actually in force
        "effective_connection": skein::connection_for_box(&name).map(|c| c.label).unwrap_or_default(),
        "effective_git_name": git_name,
        "effective_git_email": git_email,
        "effective_disk_mb": skein::box_disk_limit(&name),
        // Asked of the box, because nothing host-side records it: the token lands in the box's own
        // `~/.config/sync/env`. One round trip, and only when someone opens this panel — the board
        // refreshes every 2s and could never pay for this per box.
        "wired": skein::sbx_guest_output(
            &name,
            "test -s \"$HOME/.config/sync/env\" && echo wired",
            std::time::Duration::from_secs(15),
        )
        .unwrap_or_default()
        .contains("wired"),
        "agent": skein::agent_for_box(&name),
        "repo_connection": repo
            .and_then(|r| skein::connection_for_repo(&r))
            .map(|c| c.label)
            .unwrap_or_default(),
    }))
    .into_response()
}

/// Change one box's disk allowance. Live — nothing restarts, and the next board refresh reads it.
///
/// It can be live precisely because the limit is accounting rather than a kernel quota: the fleet's
/// disk is one filesystem shared by every box, so this is the number skein measures against, not a
/// wall the box hits. Absent `limit` restores the fleet default; an empty one means unlimited.
async fn api_set_box_disk(Path(name): Path<String>, Json(r): Json<DiskReq>) -> Response {
    match skein::set_box_disk_limit(&name, r.limit.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct DiskReq {
    #[serde(default)]
    limit: Option<String>,
}

#[derive(serde::Deserialize)]
struct IdentityReq {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

#[derive(serde::Deserialize)]
struct TrackingReq {
    /// `Some("<id>")` to track there, `Some("")` for untracked, absent to inherit the repo.
    #[serde(default)]
    connection: Option<String>,
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
                memory: skein::HealthCheck {
                    ok: true,
                    detail: "health task failed".into(),
                },
                // `ok: true` like the other opt-in checks: the health task falling over says
                // nothing about whether scoping is configured, and a red line here would blame
                // GitHub for a panic somewhere else entirely.
                gitgate: skein::HealthCheck {
                    ok: true,
                    detail: "health task failed".into(),
                },
                logins: Vec::new(),
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

/// Update skein's app settings — MERGED onto what is stored, never replacing it.
///
/// Every field of `Config` has a serde default, so deserializing a partial body into one silently
/// turns each absent field into its default and writes that back. The settings screen sends the
/// fields it renders, which is not all of them — so saving anything at all cleared `fleet_sandbox`,
/// and skein forgot the fleet existed: every box read as legacy, the board emptied, and the next
/// `skein start` would have built a whole microVM for a box that already had a namespace.
///
/// Merging at the JSON layer rather than fixing the one client, because the failure is structural: a
/// body that omits a key means "leave it alone" in every REST API anyone has ever used, and the next
/// field added to this screen would otherwise reintroduce exactly this bug.
async fn api_set_settings(Json(patch): Json<serde_json::Value>) -> Response {
    let Some(patch) = patch.as_object() else {
        return (StatusCode::BAD_REQUEST, "settings must be an object").into_response();
    };
    let current = skein::load_config();
    let mut merged = match serde_json::to_value(&current) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => return (StatusCode::INTERNAL_SERVER_ERROR, "unreadable config").into_response(),
    };
    for (key, value) in patch {
        merged.insert(key.clone(), value.clone());
    }
    let c: skein::Config = match serde_json::from_value(serde_json::Value::Object(merged)) {
        Ok(c) => c,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
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

/// What the fleet's VM is using right now: memory, disk, load.
///
/// `spawn_blocking` for the same reason `api_events` uses it — behind this is an `sbx exec`, and
/// running one on an async worker stalls every terminal websocket that worker is pumping.
///
/// 204 rather than an error when there is no fleet: a board with each box in its own sandbox has no
/// single machine to gauge, and that is a normal configuration rather than something to warn about.
/// How skein is reaching the fleet right now. Its own endpoint rather than a field on the resources
/// above, because that one asks the sandbox and 204s when the sandbox will not answer — and "the
/// sandbox is unreachable" is exactly when you want to know which transport was being used.
/// What boxes have asked the fleet to install.
/// What boxes have asked to write, and what has already been granted.
///
/// Both in one response, because the question the panel answers is "who can write where" and a
/// pending ask and a live grant are two states of one answer. Reading the queue execs into the
/// sandbox; the grants are a host file, so they survive a fleet that is down.
async fn api_git_grants() -> Json<serde_json::Value> {
    let requests = tokio::task::spawn_blocking(skein::gitgate::fleet_requests)
        .await
        .unwrap_or_default();
    let grants = skein::gitgate::grants();
    let now = chrono::Utc::now();
    Json(serde_json::json!({
        "requests": requests,
        // `live` is computed here rather than in the page: an expiry is a comparison against the
        // host's clock, and a browser in another timezone with a skewed clock would draw a grant as
        // live that the host has already stopped honouring.
        "grants": grants.iter().map(|g| serde_json::json!({
            "box": g.box_name,
            "repo": g.repo,
            "granted": g.granted,
            "expires": g.expires,
            "live": g.is_live(now),
        })).collect::<Vec<_>>(),
        "app_ready": skein::gitgate::app_credentials().is_ok(),
        "app_problem": skein::gitgate::app_credentials().err().unwrap_or_default(),
        // Whether a write token can be issued *at all* — by App or by a stored PAT. This is what
        // scoping is gated on, so it is the honest "is this switched on" answer; `app_ready` alone
        // would read as off for someone using nothing but their own tokens.
        "ready": skein::gitgate::can_issue_write_tokens(),
        // Descriptions only. The tokens themselves live in 0600 files and are never served — the
        // cockpit learns whether one is set, never what it is.
        "credentials": skein::gitgate::write_credentials().iter().map(|c| serde_json::json!({
            "id": c.id,
            "label": c.label,
            "repo": c.repo(),
            "has_token": skein::gitgate::credential_has_token(&c.id),
            // Carried so an unusable entry can explain itself. One hand-edited to cover three
            // repositories is listed and refused, and a list that silently omitted it would leave
            // someone staring at a repo whose token "is configured" and does not work.
            "problem": c.problem().unwrap_or_default(),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(serde::Deserialize)]
struct CredentialReq {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    repos: Vec<String>,
    /// Absent leaves whatever token is stored alone, so editing the repo list does not silently
    /// clear the credential. An empty string is a deliberate "forget it".
    #[serde(default)]
    token: Option<String>,
}

/// Store a fine-grained PAT and the repositories it covers.
async fn api_git_credential(Json(r): Json<CredentialReq>) -> Response {
    if let Err(e) = skein::gitgate::set_write_credential(&r.id, &r.label, &r.repos) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    if let Some(token) = r.token {
        if let Err(e) = skein::gitgate::set_credential_token(&r.id, &token) {
            return (StatusCode::BAD_REQUEST, e).into_response();
        }
    }
    // Boxes whose repo this now covers can be given it without waiting for the next tick.
    tokio::task::spawn_blocking(|| {
        for view in load_views().unwrap_or_default() {
            let _ = skein::gitgate::refresh_tokens(&view.name);
        }
    });
    StatusCode::NO_CONTENT.into_response()
}

async fn api_git_credential_remove(Path(id): Path<String>) -> Response {
    match skein::gitgate::remove_write_credential(&id) {
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
        Ok(()) => {
            // Withdrawn from every box that held it, rather than left live for up to a tick.
            tokio::task::spawn_blocking(|| {
                for view in load_views().unwrap_or_default() {
                    let _ = skein::gitgate::refresh_tokens(&view.name);
                }
            });
            StatusCode::NO_CONTENT.into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct GrantReq {
    approve: bool,
    /// How long the grant lasts. Absent ⇒ the 24-hour default; `0` ⇒ never expires.
    ///
    /// Unlike a package approval, which is permanent by design, write access to someone else's
    /// repository is usually wanted for one change — so the default expires and "keep it" is the
    /// deliberate choice rather than the accidental one.
    #[serde(default)]
    hours: Option<i64>,
}

/// Approve or deny one write request.
async fn api_git_grant_decide(Path(id): Path<String>, Json(r): Json<GrantReq>) -> Response {
    let hours = match r.hours {
        None => Some(skein::gitgate::DEFAULT_GRANT_HOURS),
        Some(0) => None,
        Some(h) if h > 0 => Some(h),
        Some(h) => {
            return (StatusCode::BAD_REQUEST, format!("{h} is not a duration")).into_response()
        }
    };
    let decided = {
        let id = id.clone();
        tokio::task::spawn_blocking(move || skein::gitgate::fleet_decide(&id, r.approve, hours))
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
    };
    match decided {
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
        Ok(req) => {
            // The grant is recorded; the token that makes it usable is minted off the request
            // thread, because it is two round trips to GitHub and the answer should not wait on
            // them. The box picks it up the moment it lands.
            if r.approve {
                let box_name = req.box_name.clone();
                tokio::task::spawn_blocking(move || skein::gitgate::refresh_tokens(&box_name));
            }
            Json(req).into_response()
        }
    }
}

/// Ask GitHub whether a token could actually be issued for each managed repo.
///
/// Its own route rather than a field on the panel's GET, because it spends real round trips — one
/// per repo — and the panel is polled. This only ever runs on a click.
async fn api_git_probe() -> Json<Vec<skein::gitgate::ProbeResult>> {
    Json(
        tokio::task::spawn_blocking(skein::gitgate::probe_credentials)
            .await
            .unwrap_or_default(),
    )
}

/// Withdraw a grant. Effective immediately for the decision, and within a tick for the token.
async fn api_git_grant_revoke(Path((name, repo)): Path<(String, String)>) -> Response {
    // The repo arrives percent-encoded (`owner%2Fname`) because it is one path segment holding a
    // slash. axum decodes it before it reaches here.
    match skein::gitgate::revoke(&name, &repo) {
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
        Ok(()) => {
            tokio::task::spawn_blocking(move || skein::gitgate::refresh_tokens(&name));
            StatusCode::NO_CONTENT.into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct ScopeReq {
    /// `"repo"`, `"fleet"`, or absent to go back to following the fleet default.
    #[serde(default)]
    scope: Option<String>,
}

/// Flip one box's GitHub scope. Takes effect at the box's next start.
async fn api_set_box_git_scope(Path(name): Path<String>, Json(r): Json<ScopeReq>) -> Response {
    match skein::gitgate::set_box_scope(&name, r.scope.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn api_substrate() -> Json<Vec<skein::substrate::Request>> {
    // Blocking: it execs into the sandbox to read the queue.
    Json(
        tokio::task::spawn_blocking(skein::substrate::fleet_requests)
            .await
            .unwrap_or_default(),
    )
}

#[derive(serde::Deserialize)]
struct DecideReq {
    approve: bool,
    /// Whether an approval is also recorded, so a rebuilt sandbox reinstalls it unprompted.
    /// Defaults to true — the cockpit sends `false` only when its owner unticks it.
    #[serde(default = "yes")]
    remember: bool,
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
async fn api_substrate_decide(Path(id): Path<String>, Json(r): Json<DecideReq>) -> Response {
    let decided = {
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            skein::substrate::fleet_decide(&id, r.approve, r.remember)
        })
        .await
    };
    match decided {
        Ok(Ok(req)) => {
            if r.approve {
                // Detached deliberately: nothing here reads the result, because the request file is
                // where the result goes and that is what the cockpit is already watching.
                tokio::task::spawn_blocking(move || {
                    let _ = skein::substrate::fleet_install(&id);
                });
            }
            Json(req).into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_fleet_transport() -> Json<skein::Transport> {
    // Blocking: it opens a socket to the agent. Cheap, but not on an async worker.
    Json(
        tokio::task::spawn_blocking(skein::transport_state)
            .await
            .unwrap_or_default(),
    )
}

/// Per-box CPU, memory and process count.
///
/// Its own endpoint rather than a field on `/api/fleet/resources`, because the two are asked for at
/// different moments and cost different amounts: the gauge strip polls every 30 seconds and must
/// stay cheap, while this measures a rate over half a second and is only wanted when something looks
/// wrong. Folding it in would have put that half-second into every poll.
async fn api_fleet_load() -> Json<Vec<skein::BoxLoad>> {
    Json(
        tokio::task::spawn_blocking(skein::box_loads)
            .await
            .unwrap_or_default(),
    )
}

async fn api_fleet_resources() -> Response {
    match tokio::task::spawn_blocking(skein::fleet_resources).await {
        Ok(Some(r)) => Json(r).into_response(),
        _ => StatusCode::NO_CONTENT.into_response(),
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
    match skein::resize_fleet(&r.memory, &r.cpus, &r.disk, r.drop_docker) {
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
    /// Root filesystem size. Empty keeps the configured one — see [`skein::Config::fleet_disk`].
    #[serde(default)]
    disk: String,
    /// Proceed even though the rebuild destroys locally-built images and named volumes.
    ///
    /// Defaults to false, so a client that predates this field gets the refusal rather than the
    /// destruction — which is the right way round for a field that means "yes, lose it".
    #[serde(default)]
    drop_docker: bool,
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

/// How long one attachment may take end to end. Generous, because the limit that matters is the
/// user's patience and their upstairs bandwidth — a 900 MB video over a slow link is a legitimate
/// upload, not a stall. It exists so that a box which stops reading cannot hold the connection (and
/// the agent's child) forever.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(3600);

/// Where an upload's bytes go: the in-sandbox agent when there is one, `sbx exec -i` when there is
/// not. Both stream, so neither the host nor the box holds the whole file; the difference is only
/// which channel into the sandbox carries it — and which of the two still answers during a stall.
enum Sink {
    /// The agent's connection, driven from a blocking thread.
    ///
    /// A thread and a channel rather than a direct call because [`skein::AgentWrite`] is
    /// synchronous — it owns a plain `TcpStream` — and writing to it from this async loop would
    /// block a tokio worker for the length of the upload. That is the freeze this file already
    /// documents twice: every terminal websocket scheduled on that worker starves until it ends.
    /// The channel is bounded, so backpressure still reaches the browser rather than the queue
    /// growing to the size of the file.
    Agent {
        chunks: tokio::sync::mpsc::Sender<Option<Vec<u8>>>,
        done: Option<tokio::task::JoinHandle<Result<(), String>>>,
    },
    Child {
        child: tokio::process::Child,
        stdin: tokio::process::ChildStdin,
    },
}

impl Sink {
    /// Feed the agent from a blocking thread. `None` on the channel is the end marker: the writer
    /// has to tell "that was everything" from "the caller gave up", because only the first commits.
    fn over(write: skein::AgentWrite) -> Sink {
        let (chunks, mut pieces) = tokio::sync::mpsc::channel::<Option<Vec<u8>>>(8);
        let done = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let mut write = write;
            while let Some(piece) = pieces.blocking_recv() {
                match piece {
                    Some(chunk) => write.push(&chunk)?,
                    None => return write.finish(),
                }
            }
            // The sender went away without ending the body. Dropping `write` here closes the
            // connection mid-chunk, which is what tells the agent to kill the half-written file.
            Err("upload abandoned".into())
        });
        Sink::Agent {
            chunks,
            done: Some(done),
        }
    }

    async fn push(&mut self, chunk: &[u8]) -> Result<(), String> {
        use tokio::io::AsyncWriteExt as _;
        match self {
            Sink::Agent { chunks, done } => {
                if chunks.send(Some(chunk.to_vec())).await.is_ok() {
                    return Ok(());
                }
                // The writer is gone, so it failed — and *its* error says why ("No space left on
                // device"), where this end only knows that a channel closed.
                match Self::verdict(done.take()).await {
                    Err(e) => Err(e),
                    Ok(()) => Err("the write into the box ended early".into()),
                }
            }
            Sink::Child { stdin, .. } => stdin
                .write_all(chunk)
                .await
                .map_err(|e| format!("writing file to box: {e}")),
        }
    }

    async fn finish(self) -> Result<(), String> {
        match self {
            Sink::Agent { chunks, done } => {
                // A closed channel is not an error to report here: the writer failed, and joining
                // it below produces the reason.
                let _ = chunks.send(None).await;
                Self::verdict(done).await
            }
            Sink::Child { child, stdin } => {
                use tokio::io::AsyncWriteExt as _;
                let mut stdin = stdin;
                stdin.shutdown().await.ok();
                drop(stdin); // EOF for `cat`
                let out = child
                    .wait_with_output()
                    .await
                    .map_err(|e| format!("sbx exec failed: {e}"))?;
                if out.status.success() {
                    return Ok(());
                }
                Err(format!(
                    "sbx exec failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }
    }

    /// Give up, leaving nothing running. The partial file is removed by the caller either way.
    async fn abandon(self) {
        match self {
            Sink::Agent { chunks, done } => {
                drop(chunks);
                if let Some(handle) = done {
                    let _ = handle.await;
                }
            }
            Sink::Child { mut child, stdin } => {
                drop(stdin);
                let _ = child.kill().await;
            }
        }
    }

    async fn verdict(
        done: Option<tokio::task::JoinHandle<Result<(), String>>>,
    ) -> Result<(), String> {
        match done {
            Some(handle) => handle
                .await
                .unwrap_or_else(|e| Err(format!("the write into the box failed: {e}"))),
            None => Err("the write into the box failed".into()),
        }
    }
}

async fn stream_upload(
    name: &str,
    headers: &axum::http::HeaderMap,
    body: axum::body::Body,
) -> Result<String, String> {
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

    // Which channel carries it is decided here, before a single byte is read, and that ordering is
    // the whole reason it is decided on the declared length rather than the real one: an upload is
    // read off a network socket exactly once, so by the time the truth is known there is no second
    // copy to fall back with. A browser sending a file or a blob always declares it; anything that
    // does not, or that declares more than the agent will carry, takes `sbx exec -i`, which streams
    // from a pipe and has no ceiling.
    let declared: Option<u64> = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let carried = match declared {
        Some(n) if n <= skein::AGENT_WRITE_CAP => {
            let (box_name, dir, path) = (name.to_string(), dir.clone(), path.clone());
            tokio::task::spawn_blocking(move || {
                skein::begin_box_write(&box_name, &dir, &path, UPLOAD_TIMEOUT)
            })
            .await
            .ok()
            .flatten()
        }
        _ => None,
    };
    let mut sink = match carried {
        Some(write) => Sink::over(write),
        None => {
            // The argv carries its own program: where the box lives decides that too.
            let argv = skein::box_write_argv(name, &dir, &path)?;
            let mut child = tokio::process::Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("sbx exec not runnable: {e}"))?;
            let stdin = child.stdin.take().ok_or("no stdin pipe")?;
            Sink::Child { child, stdin }
        }
    };
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
        if let Err(e) = sink.push(&chunk).await {
            failed = Some(e);
            break;
        }
    }
    if let Some(e) = failed {
        sink.abandon().await;
        discard_partial(name, &path).await;
        return Err(e);
    }
    sink.finish().await?;
    Ok(path)
}

/// Best-effort removal of a half-written attachment, so a failed upload leaves nothing for the agent
/// to mistake for the real file. Bounded: a wedged box must not hold the response open.
async fn discard_partial(name: &str, path: &str) {
    let inner = format!("rm -f {}", skein::sh_quote(path));
    // Through the placement, like the write it is undoing — `sbx exec <box>` names no sandbox in the
    // fleet, so the cleanup would fail exactly when the upload it is cleaning up did. And through
    // the agent when there is one, for the same reason: the moment a half-written file most needs
    // removing is the moment a fresh `sbx exec` is least likely to come back.
    let name = name.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        skein::place_of(&name).map(|place| place.exec(&inner, Duration::from_secs(10)))
    })
    .await;
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
    // A fleet box's tmux server does not survive its sandbox cycling, while its tree, private HOME
    // and cgroup do. Restart the session before we address its namespace, or the terminal opens on
    // `nsenter: cannot open /proc/<pid>/ns/user` — a namespace error for a box that just needs
    // starting again. A no-op for a live box and for one that owns its sandbox.
    if launch.is_none() {
        // A box that does not exist cannot be attached to, and trying is not harmless: `sbx exec`
        // names a sandbox, sbx says it has never heard of it, the browser reconnects, and the loop
        // buries the real error from the failed start under a message about a sandbox that was
        // never meant to exist. Say what is wrong once and stop, rather than forever and mislead.
        let boxed = name.clone();
        if let Ok(Some(why)) =
            tokio::task::spawn_blocking(move || skein::absent_box_reason(&boxed)).await
        {
            let _ = socket.send(Message::Text(format!("skein: {why}"))).await;
            return;
        }
        let boxed = name.clone();
        if let Ok(Err(e)) =
            tokio::task::spawn_blocking(move || skein::ensure_box_session(&boxed)).await
        {
            let _ = socket.send(Message::Text(format!("skein: {e}\r\n"))).await;
        }
    }
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
    use super::{origin_ok, refuse_unknown_args};
    use axum::http::{header::ORIGIN, HeaderMap, HeaderValue};

    #[test]
    fn a_cli_subcommand_typed_at_the_server_is_refused_not_swallowed() {
        let arg = |s: &str| vec![s.to_string()];
        // No args is the real invocation: boot the cockpit.
        assert_eq!(refuse_unknown_args(&[]), None);
        // A CLI subcommand must not silently start a server; it must name the binary that has it.
        let msg = refuse_unknown_args(&arg("doctor")).expect("must refuse");
        assert!(msg.contains("skein doctor"), "{msg}");
        assert!(refuse_unknown_args(&arg("attach"))
            .expect("must refuse")
            .contains("skein attach"));
        assert!(refuse_unknown_args(&arg("--help"))
            .expect("help")
            .contains("no subcommands"));
    }

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
