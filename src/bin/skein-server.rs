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
use skein::board::load_views;
use skein::board::BoxView;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{Read, Write};
use std::time::Duration;
use tokio_stream::StreamExt;

use skein::cockpit::{INDEX, V2};
// Vendored, not CDN-loaded: the terminal must work in the firewalled sbx network the tool lives in.
// They are no longer four constants and four handlers — `skein::assets` generates the table from the
// directory, because a build step emits files whose names carry content hashes and neither the count
// nor the names are known here. The four URLs are unchanged; only the code behind them is.
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// Cap concurrent embedded terminals so a flood of WS connections can't exhaust PTYs / file
/// descriptors on the host. Each live terminal holds one permit for its whole session.
static PTY_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(24);

/// Cap live boards for the same reason as the PTY cap: a client that connects and never reads still
/// holds a channel, and a producer serving a hundred of them is a producer nobody is watching.
static EVENT_LIMIT: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));

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
        // The revision beside the package version, because the package version is 0.1.0 forever:
        // "which build is serving" was twice answerable only by grepping served HTML.
        Some("--version" | "-v") => Some(format!(
            "skein-server {} ({})",
            env!("CARGO_PKG_VERSION"),
            skein::health::BUILD_REVISION
        )),
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
    // still win; a malformed file is reported, not silently half-applied). See skein::util::load_dotenv.
    skein::util::load_dotenv();
    // Before the probe install, the kit and the port: those all write to the volume, and a volume
    // this binary does not understand must be refused rather than written to. `load_dotenv` comes
    // first because $SKEIN_HOME may be in the .env, and checking the wrong volume proves nothing.
    if let Err(e) = skein::volume::ensure_volume() {
        eprintln!("skein-server: {e}");
        std::process::exit(1);
    }
    // Install skein's turn-state probe into the shared store (idempotent), so every box reports
    // working/waiting/needs-input + task without the repo shipping hooks. Best-effort.
    if let Err(e) = skein::probes::ensure_probe_all() {
        eprintln!("skein: turn-state probe not installed ({e}); boxes will show live/stale only");
    }
    // Install skein's own sbx kit (idempotent) so launching a box needs no repo-side kit.
    if let Err(e) = skein::kit::ensure_kit() {
        eprintln!("skein: kit not installed ({e}); boxes will fail to provision");
    }
    // Bring an existing fleet sandbox into line with this binary: it keeps the launcher and the
    // ceilings it was last given, and an upgrade that changes what skein passes the launcher stops
    // every box in that fleet starting until the copy out there is replaced. A restart is the only
    // moment that mismatch is observable. Skips a sleeping fleet rather than booting a VM to fix it.
    if let Err(e) = skein::fleet::heal_fleet() {
        eprintln!("skein: could not heal the fleet sandbox ({e}); boxes may start with a stale launcher or stale ceilings");
    }
    // The transport, watched rather than decided once at startup.
    //
    // `heal_fleet` above is the only thing that installs the in-sandbox agent, and it is gated
    // behind a five-second `sbx ls`. A daemon that is cold at boot misses it — and a server usually
    // starts when everything else does — after which nothing tries again until someone starts a
    // box. That is how a fleet with the setting on stays on `sbx exec` for days: reported as "sbx
    // did not answer, so skein-fleet was not brought into line with this build", on a machine where
    // `sbx ls` in a terminal answered fine.
    //
    // A minute, because this is a repair and not a probe: when the agent is serving the tick costs
    // one loopback `/health`, and when it is not, the thing being waited for (a daemon coming up, a
    // sandbox starting) moves on the scale of minutes.
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            // Says something only when the answer *changed*, so a healthy fleet is silent and a
            // recovery is one line rather than a stream of them.
            if let Ok(Some(said)) = tokio::task::spawn_blocking(skein::fleet::heal_transport).await
            {
                eprintln!("skein: {said}");
            }
        }
    });
    // **One login anywhere, everywhere.**
    //
    // Claude invalidates sessions often, and every invalidation used to cost one interactive login
    // PER BOX — reported as six or seven a day on a fleet of a dozen. The launcher's rule only lets
    // a box's login heal the fleet's when the fleet "holds no login at all", and an invalidated
    // credential still counted as holding one, so nothing could ever heal anything.
    //
    // It also only ran at box session start, which is why this is a tick rather than a hook: a login
    // typed inside a running box has to reach the others without restarting them.
    //
    // A minute, for the same reason as the transport repair above: it is a repair, not a probe. The
    // common case is a single script in the sandbox that reads a dozen small files and writes none.
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            match tokio::task::spawn_blocking(skein::fleet::heal_logins).await {
                // Silent when there was nothing to do, which is nearly always. A line only when a
                // credential actually moved, because that is a thing somebody may want to know
                // happened without having asked for it.
                Ok(Ok(healed)) if !healed.is_empty() => eprintln!(
                    "skein: a working login reached {} place(s) that had none",
                    healed.len()
                ),
                _ => {}
            }
        }
    });
    // **Reading the queue you have to work, before you open it.** Nothing here until a repo is
    // switched on for it, and then only pull requests somebody asked you to review — the owner's
    // own limits, and the reason there is no daily quota: the scope is the budget.
    //
    // Ten minutes. This is the one tick that spends money, and what it waits for is a branch going
    // quiet for an HOUR, so a faster pass would only ask the same question sooner and get the same
    // answer. Three per pass keeps a queue that settles all at once from firing thirty model calls
    // in a minute.
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(600));
        loop {
            tick.tick().await;
            match tokio::task::spawn_blocking(skein::review::read_waiting).await {
                Ok(read) => {
                    for what in read {
                        eprintln!("skein: {what}");
                    }
                }
                Err(e) => eprintln!("skein: the reading pass did not finish ({e})"),
            }
        }
    });
    // **What makes a workflow automation rather than a button.** One pass over every repo skein
    // manages, one step per pull request, and the pass is the only thing that acts.
    //
    // Two minutes, and the number is chosen by what it is waiting for. The things a workflow reacts
    // to are minutes-scale — a review lands, CI finishes, somebody pushes — and the queue underneath
    // is cached for a minute, so a faster tick would mostly re-read its own cache. A slower one
    // would leave a green, approved pull request sitting unmerged for no reason a person could see.
    //
    // Does nothing at all until `pr_workflows` is switched on, including no GitHub reads: a feature
    // that is off should be invisible in every way somebody might notice, a rate limit included.
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(120));
        loop {
            tick.tick().await;
            match tokio::task::spawn_blocking(skein::prwork::sweep).await {
                // Silent when nothing happened, which is nearly always. A line per action, because
                // this is skein doing something outward-facing that nobody asked for just now —
                // the audit has it with its authority, and this is what a person watching sees.
                Ok(did) => {
                    for what in did {
                        eprintln!("skein: {what}");
                    }
                }
                Err(e) => eprintln!("skein: the workflow pass did not finish ({e})"),
            }
        }
    });
    // **The warden, said at boot rather than at the first Launch.**
    //
    // Fleet create and destroy go only through it and there is deliberately no fallback, so a host
    // without one has lost two lifecycle operations. Every other dependency this server needs is
    // checked here — probes, kit, the launcher, the gh token, the ssh key — and this one was not, so
    // the first anybody heard of it was a 500 from pressing a button. On an upgrade that lands weeks
    // after the change that caused it, with nothing left pointing back.
    //
    // Not fatal. The server runs a fleet that already exists perfectly well without a warden; what
    // it cannot do is make or resize one. Refusing to start over a capability somebody may not use
    // today would be the wrong trade — but so is silence, which is what this had.
    match skein::warden_client::sighting() {
        Some(_) => {}
        None => eprintln!(
            "skein: {}\n       {}",
            skein::warden_client::sighting_failure()
                .unwrap_or_else(|| "the host warden did not answer".into()),
            skein::health::warden_report().fix
        ),
    }
    // Seed the host gh token into sbx (global) so boxes can fetch/push/open PRs. Best-effort and
    // quiet — many setups rely on a proxy injecting credentials instead. Skip with $SKEIN_NO_GH_SECRET.
    if let Err(e) = skein::repos::ensure_gh_secret() {
        eprintln!("skein: gh token not seeded ({e}); boxes may not push without it");
    }
    // Load the configured SSH key into the host ssh-agent so sbx forwards it into boxes (SSH push).
    // No-op when none is configured. Best-effort.
    if let Err(e) = skein::config::ensure_ssh_key() {
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
            if let Err(e) = skein::mailbox::relay_cross_project_mail() {
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
                // Skein's boxes only. A sandbox skein did not place has none of this mounted, so a
                // token minted for it is written to a host directory nothing will ever read — and
                // re-minted every twenty minutes for as long as that sandbox exists.
                for view in load_views()
                    .unwrap_or_default()
                    .iter()
                    .filter(|v| !v.foreign)
                {
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
    // Mutable because a socket that was handed in is already bound, and to somewhere: the address
    // printed below has to be the one a browser can reach, not the one this process would have
    // chosen. `$SKEIN_ADDR` decides where to bind; an inherited descriptor decides nothing and
    // reports.
    let mut addr = std::env::var("SKEIN_ADDR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_ADDR.into());
    let app = Router::new()
        .route("/", get(index))
        // Beside `/`, not instead of it: the cutover is its own change, and a reversible one.
        .route("/v2", get(board_v2))
        .route("/vendor/xterm.js", get(|| asset("xterm.min.js")))
        .route("/vendor/xterm.css", get(|| asset("xterm.min.css")))
        .route("/vendor/addon-fit.js", get(|| asset("addon-fit.min.js")))
        .route("/vendor/marked.js", get(|| asset("marked.min.js")))
        // The cockpit's own pure functions, built from `cockpit/src`. Served the same way as the
        // vendored ones because it is the same kind of thing: bytes the page needs, in the binary.
        .route("/vendor/cockpit.js", get(|| asset("cockpit.js")))
        // One route, no code per file. This is what a built bundle is served by.
        .route("/assets/*path", get(any_asset))
        .route("/api/boxes/:name/files", get(api_files))
        .route("/api/boxes/:name/file", get(api_file))
        .route("/api/boxes", get(api_boxes))
        .route("/api/health", get(api_health))
        .route("/api/runtimes", get(api_runtimes))
        .route("/api/repos", get(api_repos).post(api_add_repo))
        .route("/api/repos/:id", axum::routing::delete(api_remove_repo))
        .route("/api/repos/:id/pull", post(api_pull_repo))
        .route("/api/repos/:id/settings", post(api_set_repo_settings))
        .route("/api/review", get(api_review_merged))
        .route("/api/review/counts", get(api_review_counts))
        .route("/api/repos/:id/modules", get(api_modules))
        .route("/api/repos/:id/modules/write", post(api_write_module))
        .route("/api/repos/:id/review", get(api_review_queue))
        .route(
            "/api/repos/:id/review/:number/archive",
            post(api_review_archive),
        )
        .route(
            "/api/repos/:id/review/:number/snooze",
            post(api_review_snooze),
        )
        .route(
            "/api/repos/:id/review/:number/summary",
            get(api_review_summary),
        )
        .route("/api/repos/:id/review/summaries", get(api_review_summaries))
        .route("/api/repos/:id/workflows", get(api_workflows))
        .route("/api/repos/:id/reading", post(api_set_reading))
        .route(
            "/api/workflows",
            get(api_workflow_file).put(api_save_workflows),
        )
        .route(
            "/api/repos/:id/review/:number/workflow",
            post(api_set_workflow),
        )
        .route("/api/repos/:id/review/:number/act", post(api_review_act))
        .route(
            "/api/repos/:id/review/:number/critique",
            get(api_critique_get).post(api_critique_draft),
        )
        .route(
            "/api/repos/:id/review/:number/critique/post",
            post(api_critique_post),
        )
        // The shape of a change: which modules moved and how. The same route shape for both
        // sources, because the answer is the same question — `?box=` for a box's branch.
        .route("/api/repos/:id/review/:number/diff", get(api_pr_reading))
        .route("/api/repos/:id/review/:number/shape", get(api_pr_shape))
        .route("/api/boxes/:name/shape", get(api_box_shape))
        .route("/api/settings", get(api_settings).post(api_set_settings))
        .route("/api/fleet/plan", get(api_fleet_plan))
        .route("/api/fleet/create", post(api_fleet_create))
        .route("/api/fleet/resize", post(api_fleet_resize))
        .route("/api/fleet/limits", post(api_fleet_limits))
        .route("/api/fleet/resources", get(api_fleet_resources))
        .route("/api/fleet/load", get(api_fleet_load))
        // What else is on this machine — a question about the MACHINE, asked by a person. Not
        // "foreign boxes": `docs/parity.md` §7 removes that, and what survives is somebody running
        // more than one fleet needing to see them.
        .route("/api/machine/sandboxes", get(api_machine_sandboxes))
        .route("/api/machine/doorstep", get(api_machine_doorstep))
        .route("/api/machine/pressure", get(api_machine_pressure))
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
        .route("/api/boxes/:name/privileged", post(api_set_box_privileged))
        .route("/api/fleet/git-probe", post(api_git_probe))
        .route("/api/fleet/git-credentials", post(api_git_credential))
        .route("/api/fleet/git-read-token", post(api_git_read_token))
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
        // What replaces Browse when there is no host display to open a picker on (parity §7).
        .route("/api/path", get(api_path))
        .route("/api/boxes/:name/diff", get(api_diff))
        .route("/api/boxes/:name/session", get(api_session))
        .route("/api/boxes/:name/statusline", get(api_statusline))
        .route("/api/mailbox", get(api_mailbox).post(api_mailbox_send))
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
        // Creating a box, for a surface that is not a terminal. The WebSocket path below still
        // works and still creates — this is beside it, not instead of it.
        .route("/api/boxes/:name/create", post(api_create_box))
        .route("/api/acts/:id", get(api_act))
        .route("/api/acts/:id/stream", get(act_stream))
        // Everything waiting on you, boxes and pull requests in one ordering.
        .route("/api/queue", get(api_queue))
        // What happened while you were out, and the acknowledgement that ends it.
        .route("/api/away", get(api_away))
        .route("/api/away/seen", post(api_seen))
        .route("/api/events", get(api_events))
        .route("/api/boxes/:name/terminal", get(terminal));

    // Everything above is routed; this decides who may drive it.
    //
    // A layer over the whole router rather than a check per handler, because the failure mode of
    // per-handler auth is a route added later that nobody remembers to guard — and this API grows a
    // route most weeks. What it lets through is named in one place, in `open_to_all`, which is a
    // list a reader can check against the router above.
    let app = app.layer(axum::middleware::from_fn(gate));

    // The socket, and who opened it. `skein::doorway` says why this is not simply a bind: one
    // network namespace plus a port mapping that outlives skein means a box that binds the
    // cockpit's port *first* becomes the cockpit, and the browser hands it the fleet's token on the
    // first request (architecture §9.4). A socket opened before any box exists and inherited across
    // restarts is the only thing that closes it, since the token cannot.
    let listener = match skein::doorway::inherited() {
        Ok(Some(handed)) => {
            let bound = handed
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| addr.clone());
            addr = bound;
            tokio::net::TcpListener::from_std(handed)
                .unwrap_or_else(|e| panic!("skein-server: the socket passed in is unusable: {e}"))
        }
        // Nobody passed one. Whether that is a normal start or a broken one is a deployment
        // question, not this line's — see `doorway::inherited_only`.
        Ok(None) if skein::doorway::inherited_only() => {
            eprintln!("{}", skein::doorway::missing());
            std::process::exit(1);
        }
        Ok(None) => tokio::net::TcpListener::bind(&addr)
            .await
            .unwrap_or_else(|e| panic!("skein-server: cannot bind {addr}: {e}")),
        // A caller that meant to pass a socket and got it wrong. Binding here would be the race
        // this exists to close, run by the one process that was supposed to have closed it — so it
        // is refused whichever mode this is, and the reason names the descriptor.
        Err(why) => {
            eprintln!("skein-server: {why}");
            std::process::exit(1);
        }
    };
    match skein::apiauth::disabled() {
        true => println!(
            "skein-server → http://{addr}\n  \
             API AUTH OFF ($SKEIN_NO_API_AUTH) — anything that can reach this port drives the \
             fleet, boxes included"
        ),
        // The token is printed, not just stored: this URL is how a browser gets a session, and a
        // secret nobody is shown is a secret nobody can use.
        false => match skein::apiauth::token() {
            Ok(t) => println!("skein-server → http://{addr}/?t={t}"),
            Err(e) => println!(
                "skein-server → http://{addr}\n  \
                 no API token could be created ({e}) — every API call will be refused until \
                 ~/.skein is writable"
            ),
        },
    }
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
    // The doorstep (`skein::knock`): the only place a cap can be **pre**-auth, because everything
    // else in this file has already accepted. See §9.4 — the port is reachable from every box, and
    // connecting is not authenticating, which answers reading and did not answer exhausting.
    let door = skein::knock::doorstep();
    let grace = skein::knock::grace();
    let mut refused = 0u32;
    // Whether this listener has ever worked. It is the one thing that tells a wrong descriptor
    // apart from a full one: `EMFILE` happens to a server that has been serving, and waiting it out
    // is right; a socket that is not a listening socket fails identically and forever, and waiting
    // that out is a process that is up, quiet, and will never answer. `doorway::adopt` catches the
    // two cases the standard library can see; this catches the rest without having to name them.
    let mut ever_served = false;
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(v) => {
                refused = 0;
                ever_served = true;
                v
            }
            // **Backed off, not retried immediately.** `continue` on an error that persists — the
            // ordinary one is `EMFILE`, the process out of descriptors — is a tight loop calling
            // `accept` and failing, which spends the control plane's CPU at exactly the moment it
            // has none to spare. The condition that causes it is also the condition a flood
            // produces, so the loop that exists to survive a flood was the loop that would burn on
            // one.
            Err(e) => {
                refused = refused.saturating_add(1);
                if refused == COMPLAIN_AFTER {
                    eprintln!(
                        "skein-server: cannot accept connections ({e}) — {refused} in a row. The \
                         usual cause is running out of file descriptors; the cockpit keeps trying."
                    );
                }
                if !ever_served && refused >= GIVE_UP {
                    eprintln!(
                        "skein-server: {refused} consecutive accept failures ({e}) and not one \
                         connection ever served, so this socket is not one anybody can arrive on. \
                         If it was passed in, whatever passed it passed the wrong descriptor; if it \
                         was bound here, the port was taken between the bind and now. Exiting \
                         rather than sitting up and quiet, which is indistinguishable from working."
                    );
                    std::process::exit(1);
                }
                tokio::time::sleep(slow_down(refused)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let svc = match make.call(()).await {
            Ok(s) => s,
            Err(e) => match e {}, // IntoMakeService is Infallible — this arm is unreachable
        };
        // Admitted, never refused: over the limit this evicts the oldest connection that still has
        // not authenticated, which is why a flood displaces itself instead of the next arrival.
        let knock = std::sync::Arc::new(door.admit());
        let mine = knock.clone();
        tokio::spawn(KNOCK.scope(knock, async move {
            let io = TokioIo::new(stream);
            // The builder is bound rather than chained: the connection borrows it, so a temporary
            // would be dropped at the end of the statement that created the future.
            let builder = ConnBuilder::new(TokioExecutor::new());
            let conn = builder.serve_connection_with_upgrades(io, TowerToHyperService::new(svc));
            tokio::pin!(conn);
            let deadline = tokio::time::sleep(grace);
            tokio::pin!(deadline);
            // Two ways this connection ends early, and one loop because the deadline has to be
            // *disarmed* rather than obeyed: a proven connection outlives the grace period by
            // definition — a terminal or an event stream is open for hours — so the timer firing on
            // one is a no-op and not a close.
            let mut watching = true;
            loop {
                tokio::select! {
                    _ = &mut conn => break,
                    _ = mine.ousted() => break,
                    _ = &mut deadline, if watching => {
                        watching = false;
                        if !mine.proven() {
                            break;
                        }
                    }
                }
            }
        }));
    }
}

/// After how many consecutive failures to accept a connection the operator is told.
///
/// Not the first: a single `ECONNABORTED` is ordinary — a client that hung up between the handshake
/// and the accept — and a line of log for it would be noise that teaches people to ignore the line.
const COMPLAIN_AFTER: u32 = 32;

/// After how many consecutive failures a server that has **never** served anything stops trying.
///
/// Twice [`COMPLAIN_AFTER`], so the line above is always printed before this one — a process that
/// exited without first saying why would be the same silence in a different shape. At
/// [`slow_down`]'s ceiling that is a handful of seconds, which is long enough for a transient
/// `EMFILE` at startup to pass and short enough that nobody is waiting on a page that will never
/// load.
const GIVE_UP: u32 = 64;

/// How long to wait after `accept` failed, given how many times in a row it has.
///
/// Doubling from a millisecond to a quarter of a second, and no further: the ceiling is what makes
/// this a **pause rather than a shutdown**. The condition is usually temporary — descriptors come
/// back when connections close — and a control plane that gave up would need somebody to notice and
/// restart it, at the moment they are least able to see anything.
fn slow_down(consecutive: u32) -> Duration {
    const CEILING: Duration = Duration::from_millis(250);
    let doubled = Duration::from_millis(1u64 << consecutive.min(8));
    doubled.min(CEILING)
}

tokio::task_local! {
    /// The connection the request being served arrived on.
    ///
    /// A task-local rather than a request extension because the service is built per connection by
    /// `IntoMakeService` and every request on it is polled inside this task — so the connection is
    /// already the scope, and threading a layer through the router to say so would add a wrapper
    /// per route to carry a fact the task already has.
    static KNOCK: std::sync::Arc<skein::knock::Knock>;
}

/// Paths served without the fleet's token.
///
/// Deliberately short, and deliberately a list of *escapes* rather than a list of what is guarded:
/// a new route is protected the moment it is added, and opening one up has to be written down here
/// where it can be read and argued with.
///
/// Each of these is a static asset compiled into this binary — the same bytes for every fleet, no
/// state read, nothing mutated. `/` is here so an unauthenticated visitor gets a page that can
/// explain itself instead of a bare 401, and so `?t=` has somewhere to land.
fn open_to_all(path: &str) -> bool {
    // `/v2` for the same reason as `/`: it is the same bytes for every fleet, it reads no state, and
    // it is where `?t=` lands. An unauthenticated visitor gets a page that can explain itself.
    path == "/" || path == "/v2" || path.starts_with("/vendor/")
}

/// Refuse anything that does not carry the fleet's token.
///
/// This is the answer to a box reaching `host.docker.internal:7878` — see [`skein::apiauth`] for
/// what that allowed and why a secret rather than a peer-address rule.
async fn gate(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    if skein::apiauth::authorised(request.headers()) {
        // The one place a connection stops being a stranger. Deliberately keyed on the credential
        // and not on being served: `open_to_all` would otherwise promote anything that can spell
        // `GET /`, which is every flooder, and the doorstep would bound nothing.
        let _ = KNOCK.try_with(|knock| knock.prove());
        return next.run(request).await;
    }
    if open_to_all(request.uri().path()) {
        return next.run(request).await;
    }
    skein::apiauth::refusal().into_response()
}

/// The cockpit page, and the one place the fleet's API token becomes a browser session.
///
/// `?t=<token>` is exchanged for an `HttpOnly` cookie and then **redirected away**, so the secret
/// does not stay in the address bar, in history, or in the `Referer` of anything the page later
/// links to. Every `fetch` in the document and the terminal WebSocket then carry it automatically,
/// which is why authenticating the API changed no calling code.
///
/// The document itself is served to anyone who asks. It holds no secrets — it is the same HTML
/// compiled into this binary — and gating it would only mean an unauthenticated visitor got a blank
/// page instead of one that can say what is wrong.
async fn index(Query(q): Query<HashMap<String, String>>) -> Response {
    page(&q, "/", INDEX)
}

/// The new board, beside the old one rather than instead of it.
///
/// `docs/delivery.md` names treating "ground-up surfaces" and "new topology" as one project as the
/// single biggest avoidable risk in the plan, and shipping beside is what keeps them separate: `/`
/// keeps working, unchanged, until `docs/parity.md` §7 has been walked against this page item by
/// item. A surface that is 90% ported and cut over is worse than one that is 60% ported and not.
async fn board_v2(Query(q): Query<HashMap<String, String>>) -> Response {
    page(&q, "/v2", V2)
}

/// One page, one session exchange.
///
/// `?t=<token>` is exchanged for an `HttpOnly` cookie and redirected **back to the page that was
/// asked for**, which is the only part of this that is per-page: a `/v2` link that landed you on `/`
/// would look like the new board silently not existing.
fn page(q: &HashMap<String, String>, self_path: &str, body: &'static str) -> Response {
    let headers = [
        (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
        // The UI is embedded in and version-coupled to this binary. Reusing an older document after
        // a server restart mixes stale JS/CSS with new API behaviour, so the browser must revalidate.
        (axum::http::header::CACHE_CONTROL, "no-store"),
    ];
    let offered = q.get("t").map(String::as_str).unwrap_or_default();
    if !offered.is_empty() && skein::apiauth::token().is_ok_and(|want| want == offered) {
        {
            // `SameSite=Strict` is what closes cross-site POSTs to this API. `Path=/` covers the
            // WebSocket as well as `/api`. No `Secure`, because the ordinary case is plain http on
            // loopback and a Secure cookie would simply never be stored there.
            return (
                StatusCode::SEE_OTHER,
                [
                    (axum::http::header::LOCATION, self_path.to_string()),
                    (
                        axum::http::header::SET_COOKIE,
                        format!(
                            "{}={offered}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000",
                            skein::apiauth::COOKIE
                        ),
                    ),
                ],
                "",
            )
                .into_response();
        }
        // A wrong token gets the page and no cookie, rather than a hint that it was wrong.
    }
    (headers, body).into_response()
}

/// Everything that stopped and is waiting on you, most urgent first.
///
/// **Not on the two-second tick.** The pull-request half comes from `prq::queue`'s own 60-second
/// cache, so a surface rendering this often still reaches GitHub once a minute per repo — but it is
/// a surface's call rather than the board's, which is what keeps `signal::board_tick` honest.
async fn api_queue() -> Json<serde_json::Value> {
    let rows = tokio::task::spawn_blocking(skein::queue::who_needs_you)
        .await
        .unwrap_or_default();
    // **The standing travels with the rows it was derived from.** `queue::standing` reads the same
    // list, so a client that asked for one and computed the other would be a second implementation
    // of the rule — and the two disagree the day the rule changes, in the direction of a board that
    // says "nothing needs you" over a list of things that do.
    //
    // `waiting` rather than `rows`, because `Standing::NeedsYou` serialises its own `rows` count and
    // two fields of that name in one document is a footgun for whoever reads it next.
    Json(serde_json::json!({
        "standing": skein::queue::standing(&rows),
        "waiting": rows,
    }))
}

/// What happened since the board was last acknowledged, and the standing that goes with it.
///
/// Answered by the **server**, which is the whole design: a client-side delta computed on tab focus
/// cannot survive a reload, cannot tell a box that finished while you were away from one that
/// finished before the tab was opened, and is wrong for every second tab. Three failures that all
/// look like the feature working.
async fn api_away() -> Json<serde_json::Value> {
    let (since, moments, rows) = tokio::task::spawn_blocking(|| {
        let since = skein::stream::last_seen();
        let moments = skein::stream::since(&since);
        (since, moments, skein::queue::who_needs_you())
    })
    .await
    .unwrap_or_default();
    Json(serde_json::json!({
        "since": since,
        "moments": moments,
        "standing": skein::queue::standing(&rows),
    }))
}

/// Acknowledge everything up to now.
///
/// The timestamp is the server's. A client that supplied its own would be choosing which moments it
/// is never shown, and a clock a minute fast would silently swallow a minute of them.
async fn api_seen() -> Response {
    match tokio::task::spawn_blocking(skein::stream::acknowledge).await {
        Ok(Ok(at)) => Json(serde_json::json!({ "at": at })).into_response(),
        Ok(Err(why)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
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

/// Start creating a box, and hand back the act to watch.
///
/// **Not a POST that returns 201.** Creating a box is minutes of clone, substrate and provisioning,
/// and §2.5 is explicit that an Act is streaming and unacknowledged — so what a caller gets is the
/// identity of something running, which it can watch, poll, or come back to after its stream has
/// closed. That last one is the property a plain POST would lose and the reason
/// `fleet::remember_start_failure` had to exist.
async fn api_create_box(Path(name): Path<String>, Json(body): Json<CreateBox>) -> Response {
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let branch = body.branch.trim().to_string();
    if branch.is_empty() {
        return (StatusCode::BAD_REQUEST, "a box is created on a branch").into_response();
    }
    let agent = body.agent.filter(|a| skein::runtime::valid_runtime(a));
    let id = skein::act::creating(&name);
    // `Attach::No`: the terminal path ends by attaching because a person is already looking at it.
    // A surface that is not a terminal wants the box made and will attach separately, or not at all
    // — and an attach with nobody on the other end is a tmux session talking to a closed pipe.
    let command = tokio::task::spawn_blocking(move || {
        skein::sandbox::launch_command_as(
            &name,
            &branch,
            agent.as_deref(),
            skein::sandbox::Attach::No,
        )
    })
    .await
    .unwrap_or_default();
    match skein::act::begin(&id, &command) {
        Ok(look) => (StatusCode::ACCEPTED, Json(look)).into_response(),
        // 409, because the thing that stops a second create is that one is already running — which
        // is a conflict rather than a bad request, and the message says which act to watch.
        Err(why) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": why })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct CreateBox {
    branch: String,
    #[serde(default)]
    agent: Option<String>,
}

/// What an act is doing, and everything it has said. Readable after it has ended, which is the
/// whole point.
async fn api_act(Path(id): Path<String>) -> Response {
    match skein::act::look(&id) {
        Some(look) => Json(look).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no such act — it may have finished longer ago than the warden keeps them"
            })),
        )
            .into_response(),
    }
}

/// Follow an act: what it has already said, then the rest as it arrives.
///
/// Both from one call, because taking them separately loses whatever the act said in between — for
/// a create, the line that mattered.
async fn act_stream(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin stream blocked").into_response();
    }
    let Some((so_far, rest)) = skein::act::watch(&id) else {
        return (StatusCode::NOT_FOUND, "no such act").into_response();
    };
    ws.on_upgrade(move |mut socket| async move {
        if !so_far.is_empty() && socket.send(Message::Text(so_far)).await.is_err() {
            return;
        }
        let mut rest = rest;
        loop {
            match rest.recv().await {
                // The empty chunk the act sends when it ends. A closed socket is the only signal a
                // watcher cannot mistake for a slow act.
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => {
                    if socket.send(Message::Text(chunk)).await.is_err() {
                        return;
                    }
                }
                // Lagged: this watcher fell behind the buffer. Told, never silently skipped —
                // §10.1's rule, and the alternative is a transcript with a hole nobody can see.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    let _ = socket
                        .send(Message::Text(format!(
                            "\r\n… {missed} lines were dropped because this window fell behind …\r\n"
                        )))
                        .await;
                }
                Err(_) => break,
            }
        }
        let _ = socket.close().await;
    })
}

/// One asset by name, or a 404 that says nothing about why.
///
/// "No such asset" and "that path may not name one" are the same answer on purpose: a caller probing
/// for the difference learns nothing, and there is nothing a person can do with the distinction that
/// a 404 does not already tell them.
async fn asset(name: &str) -> Response {
    match skein::assets::get(name) {
        Some(a) => (
            [
                (axum::http::header::CONTENT_TYPE, a.content_type),
                (axum::http::header::CACHE_CONTROL, a.cache),
            ],
            a.bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The route a built bundle is served by. The path is a name relative to the asset root and never a
/// path this process joins onto anything a caller chose — see `skein::assets`.
async fn any_asset(Path(path): Path<String>) -> Response {
    asset(&path).await
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

/// Snapshot of the fleet, on request. `load_views` is blocking, so it runs on the blocking pool and
/// never inline on an async worker — see `start_producing` for why blocking a worker stalls every
/// terminal websocket scheduled on it.
///
/// Still here after the stream became one producer, and for two reasons: a surface that wants the
/// picture once should not have to open a stream to get it, and it is what a client re-syncs from
/// when it is told it has fallen behind.
async fn api_boxes() -> Json<Vec<BoxView>> {
    let views = tokio::task::spawn_blocking(|| load_views().unwrap_or_default())
        .await
        .unwrap_or_default();
    Json(views)
}

/// Provider-neutral custom footer. Claude renders it natively from stdin; Codex maps its latest
/// token_count event (the `/status` data source) through the same renderer for the browser terminal.
async fn api_statusline(Path(name): Path<String>) -> Response {
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::probes::agent_statusline(&name)).await {
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
    if !skein::util::valid_name(&name) || !skein::runtime::valid_runtime(&request.target) {
        return (StatusCode::BAD_REQUEST, "invalid box or target runtime").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::takeover::replace_box(&name, &request.target))
        .await
    {
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
    /// whether this repo has a review queue the badge may poll
    review_queue: Option<bool>,
}

async fn api_set_repo_settings(
    Path(id): Path<String>,
    Json(req): Json<RepoSettingsReq>,
) -> Response {
    match skein::repos::set_repo_settings(
        &id,
        req.plane_project.as_deref(),
        req.sync_connection.as_deref(),
        req.review_queue,
    ) {
        Ok(repo) => Json(repo).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// A repo's modules and whether skein holds a current note on each.
async fn api_modules(Path(id): Path<String>) -> Response {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    match tokio::task::spawn_blocking(move || skein::moduledocs::status(&repo)).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct WriteModuleReq {
    path: String,
}

/// Write (or rewrite) the standing note for one module.
///
/// One module per request, never "write them all": each is a minute of model time, and a single
/// request that took twenty of them would look like a hang and could not report progress. The
/// cockpit walks the list itself, so it can show which one is being written and stop partway.
async fn api_write_module(
    Path(id): Path<String>,
    Json(req): Json<WriteModuleReq>,
) -> Json<serde_json::Value> {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    let out = tokio::task::spawn_blocking(move || skein::moduledocs::write(&repo, &req.path)).await;
    Json(match out {
        Ok(Ok(doc)) => serde_json::json!({ "ok": true, "path": doc.path, "written": doc.written }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// Every repo's queue in one answer — what the pane opens on. Serves what the counts poll already
/// builds; `?force=1` re-reads GitHub.
async fn api_review_merged(Query(q): Query<HashMap<String, String>>) -> Response {
    let force = q.get("force").is_some_and(|v| v == "1" || v == "true");
    match tokio::task::spawn_blocking(move || skein::prq::merged(force)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// How many PRs need you, per repo — for the badge on the review button.
///
/// Polled on a slow timer, so it deliberately does NOT force a refresh: it rides the same 60s
/// per-repo cache as the pane. Repos with the queue switched off, and repos with no GitHub remote,
/// are never asked.
async fn api_review_counts() -> Response {
    // The merge train's stops are stapled on HERE, not inside `prq::counts` — the stops file is
    // `prwork`'s, and `prq` reading it would join the module cycle (`docs/modules.toml`). This
    // route already stands on both modules, and the stops are a disk read, so every branch of the
    // count — the failed and the switched-off included — can still say a machine waits on a person.
    match tokio::task::spawn_blocking(|| {
        let mut counts = skein::prq::counts();
        for count in &mut counts {
            count.stopped = skein::prwork::stops(&count.repo_id);
        }
        counts
    })
    .await
    {
        Ok(counts) => Json(counts).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// A repo's review queue: every open PR that is yours, and which lane it sits in.
///
/// `?force=1` skips the 60s micro-cache — for the refresh button and for the moment after an act
/// that changed a PR's state. Blocking work (three `gh` round trips) goes to a blocking thread so a
/// slow GitHub cannot stall the cockpit's SSE tick.
async fn api_review_queue(
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let force = q.get("force").is_some_and(|v| v == "1" || v == "true");
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    // **Paint now, refresh behind.** Opening this tab used to block on three GraphQL searches per
    // repo plus the viewer lookup, so a cold cache showed nothing at all until every one of them
    // came back. What it was being compared against is a blank panel, and the last queue beats a
    // blank panel every time — as long as its age travels with it, which is what `as_of` and
    // `fresh` are for. Same rule as the board's staleness banner: stale is safe only when visible.
    //
    // `force` is the explicit refresh and always waits, because somebody who pressed it is asking
    // for the new answer rather than for a fast one.
    if !force {
        if let Some(fresh) = skein::prq::unexpired(&id) {
            return Json(fresh).into_response();
        }
        if let Some(old) = skein::prq::remembered(&id) {
            // The refresh nobody is waiting for. Its result lands in the cache and on disk, so the
            // client's next ask — a few seconds later — is a cache hit rather than another wait.
            tokio::task::spawn_blocking(move || {
                let _ = skein::prq::queue(&repo, true);
            });
            return Json(old).into_response();
        }
    }
    let slug = skein::prq::repo_slug(&repo);
    match tokio::task::spawn_blocking(move || skein::prq::queue(&repo, force)).await {
        Ok(Ok(queue)) => {
            // Housekeeping AFTER the answer, never before it. Pruning asks GitHub about summaries
            // whose PR is no longer in your lane, and doing that on the way to the response would
            // spend somebody's tab-open on tidying up files they cannot see. Detached: the queue is
            // already on its way out, and nothing here has an answer the caller is waiting for.
            if let Some(slug) = slug {
                let open: Vec<(u64, String)> = queue
                    .prs
                    .iter()
                    .map(|pr| (pr.number, pr.head_sha.clone()))
                    .collect();
                let id = queue.repo_id.clone();
                tokio::task::spawn_blocking(move || skein::review::prune(&id, &slug, &open));
            }
            Json(queue).into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct ArchiveReq {
    /// true = set aside, false = bring back. Explicit rather than a toggle so a double-tap or a
    /// retried request cannot flip a PR back into a lane you already moved it out of.
    on: bool,
}

/// Set aside (or restore) one PR in a repo's queue.
async fn api_review_archive(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<ArchiveReq>,
) -> Json<serde_json::Value> {
    let res = tokio::task::spawn_blocking(move || {
        let r = skein::prq::set_archived(&id, number, req.on);
        skein::prq::invalidate(&id);
        r
    })
    .await;
    Json(match res {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

#[derive(Deserialize)]
struct SnoozeReq {
    /// The head the row was showing when it was set aside. Sent by the client rather than read
    /// server-side so the hold is on what the REVIEWER saw: a push that lands between the row
    /// rendering and the click makes the shas disagree, and the row stays visible — the safe
    /// direction. Empty brings the PR back by hand; the ordinary ending is nobody calling that
    /// at all, because the author's next push stops the sha matching on its own.
    head_sha: String,
}

/// Set one PR aside *until its head moves* (SKEIN-144). The other instrument beside `archive`:
/// an archive holds until a human undoes it, a snooze holds until the AUTHOR acts — which is
/// what "not until CI is green" actually means on a fleet where red waits on somebody's push.
///
/// One PR per call, deliberately. "Clear every red row" is a queue-level act, but it is the
/// page's to compose from rows it is already holding (each carries `head_sha` and `checks`) —
/// a server-side sweep would have to re-answer "which rows are red" and could disagree with the
/// screen the click was aimed at.
async fn api_review_snooze(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<SnoozeReq>,
) -> Json<serde_json::Value> {
    let res = tokio::task::spawn_blocking(move || {
        let sha = (!req.head_sha.is_empty()).then_some(req.head_sha.as_str());
        let r = skein::prq::set_snoozed(&id, number, sha);
        skein::prq::invalidate(&id);
        r
    })
    .await;
    Json(match res {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// What one PR means, at the depth it earns. `?force=1` re-reads instead of using the cached one.
///
/// The PR is taken from the queue rather than from the request, so the summary is always keyed to
/// the head commit GitHub reports right now — a client that remembered a stale SHA cannot make
/// skein write a summary against it.
///
/// This never fails: a PR that could not be read comes back as an `unread` summary carrying the
/// reason, because the only sane response to a failure here is to show you the PR anyway.
/// What a pull request did, by module.
///
/// Not a diff renderer, deliberately: §11.1 and the owner both say the value is in *which modules
/// changed and how the system decomposes*, and if the text is wanted GitHub has it. This is the
/// first three of four levels — module, its standing note, what this change did to it — with the
/// files listed so the fourth is a click away.
async fn api_pr_shape(Path((id, number)): Path<(String, u64)>) -> Response {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    let shaped = tokio::task::spawn_blocking(move || {
        let slug = skein::prq::repo_slug(&repo).ok_or("this repo has no GitHub remote")?;
        let diff = skein::prq::pr_diff_text(&slug, number)?;
        Ok::<_, String>(skein::shape::of_diff(&repo, &diff))
    })
    .await;
    shape_response(shaped)
}

/// The same, for a box's own branch.
///
/// One function short of identical to the pull-request one, and that is the point: the shape of a
/// change does not depend on whether it arrived as a PR or as a branch somebody is still working on.
/// A reviewer looking at their own box's work asks exactly the question a reviewer of a PR asks.
async fn api_box_shape(Path(name): Path<String>) -> Response {
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let shaped = tokio::task::spawn_blocking(move || {
        let repo =
            skein::repos::repo_for_box(&name).ok_or("this box belongs to no registered repo")?;
        let diff = skein::diff::box_diff(&name).ok_or("this box has no diff to shape")?;
        Ok::<_, String>(skein::shape::of_diff(&repo, &diff.value.patch))
    })
    .await;
    shape_response(shaped)
}

fn shape_response(
    shaped: Result<Result<Vec<skein::shape::ModuleChange>, String>, tokio::task::JoinError>,
) -> Response {
    match shaped {
        Ok(Ok(modules)) => Json(modules).into_response(),
        // A shape that could not be read is an error with the reason, never an empty list: "this
        // change touched no module skein knows about" and "the diff could not be fetched" send a
        // person to different places.
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

async fn api_review_summary(
    Path((id, number)): Path<(String, u64)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let force = q.get("force").is_some_and(|v| v == "1" || v == "true");
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    let out = tokio::task::spawn_blocking(move || {
        let queue = skein::prq::queue(&repo, false)?;
        let pr = queue
            .prs
            .iter()
            .find(|p| p.number == number)
            .ok_or("that PR is not in your queue")?;
        let identities = std::iter::once(queue.viewer.clone()).collect::<Vec<_>>();
        Ok::<_, String>(skein::review::summarise(
            &repo,
            &queue.slug,
            pr,
            &identities,
            force,
        ))
    })
    .await;
    match out {
        Ok(Ok(summary)) => Json(summary).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The draft skein already holds for this PR, off disk, costing nothing. The pane compares its
/// `head_sha` with the queue's to mark a draft of an earlier commit as such.
async fn api_critique_get(Path((id, number)): Path<(String, u64)>) -> Response {
    match tokio::task::spawn_blocking(move || skein::review::critiqued(&id, number)).await {
        Ok(c) => Json(serde_json::json!({ "ok": true, "critique": c })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Draft an actual review of the PR. A model call — only ever reached by a person pressing the
/// button, never from a background pass.
async fn api_critique_draft(Path((id, number)): Path<(String, u64)>) -> Response {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    let out = tokio::task::spawn_blocking(move || {
        let queue = skein::prq::queue(&repo, false)?;
        let pr = queue
            .prs
            .iter()
            .find(|p| p.number == number)
            .ok_or("that PR is not in your queue")?;
        skein::review::critique(&repo, &queue.slug, pr)
    })
    .await;
    match out {
        Ok(Ok(c)) => Json(serde_json::json!({ "ok": true, "critique": c })).into_response(),
        Ok(Err(e)) => Json(serde_json::json!({ "ok": false, "error": e })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// What the person kept, and nothing else. The body carries the vetted comments themselves —
/// the server deliberately does not post what it stored, so an edit or a dropped comment in the
/// pane is exactly what reaches GitHub.
#[derive(serde::Deserialize)]
struct CritiquePostReq {
    head_sha: String,
    #[serde(default)]
    overall: String,
    #[serde(default)]
    comments: Vec<skein::review::Draft>,
}

async fn api_critique_post(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<CritiquePostReq>,
) -> Json<serde_json::Value> {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    // The rules — moved head refused, vetted comments only — live in `review::post_critique`,
    // where they are proven against a stubbed GitHub.
    let out = tokio::task::spawn_blocking(move || {
        skein::review::post_critique(&repo, number, &req.head_sha, &req.overall, &req.comments)
    })
    .await;
    Json(match out {
        Ok(Ok(text)) => serde_json::json!({ "ok": true, "text": text }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

#[derive(serde::Deserialize)]
struct ReadingReq {
    on: bool,
}

/// Say whether skein may read this repo's pull requests with nobody watching.
///
/// Its own route, for the same reason it is its own setter: this is the switch that decides whether
/// skein spends model calls on its own, and it should not be reachable as a side effect of saving
/// something else.
async fn api_set_reading(
    Path(id): Path<String>,
    Json(r): Json<ReadingReq>,
) -> Json<serde_json::Value> {
    match skein::repos::set_read_prs(&id, r.on) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "on": r.on })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}

/// Every reading skein already holds for this repo's queue, in one request.
///
/// **Reading from disk is not spending.** The pane used to learn what skein knew only by asking for
/// one pull request at a time, down the same path that COMPUTES a reading — so every limit meant to
/// bound money also bounded memory, and a reading already paid for stayed hidden behind a draft
/// flag, an unsettled branch, or the sixth row. Reported as "I can only see 2 PRs with summaries
/// while before there were a bunch".
///
/// So this costs nothing and refuses nothing: no model calls, no rules about drafts or settling.
/// What the pane then asks to have COMPUTED is a separate question, and that one keeps its limits.
async fn api_review_summaries(Path(id): Path<String>) -> Response {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    let out = tokio::task::spawn_blocking(move || {
        let queue = skein::prq::queue(&repo, false)?;
        let want: Vec<(u64, String)> = queue
            .prs
            .iter()
            .map(|pr| (pr.number, pr.head_sha.clone()))
            .collect();
        Ok::<_, String>(skein::review::known(&repo.id, &want))
    })
    .await;
    match out {
        Ok(Ok(known)) => Json(
            known
                .into_iter()
                .map(|(number, k)| (number.to_string(), k))
                .collect::<std::collections::BTreeMap<_, _>>(),
        )
        .into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The fleet's workflows, and every word one can be written with.
///
/// The vocabulary is served rather than hard-coded in the page for the same reason the tables exist
/// at all: a picker offering a word the parser refuses is a workflow somebody builds and cannot
/// save, and it is found by a person in the one moment they were trusting the tool.
async fn api_workflow_file() -> Response {
    let flows = skein::workflow::load();
    Json(serde_json::json!({
        "enabled": skein::prwork::enabled(),
        // A file that will not parse is reported as itself. The editor refuses to save over it,
        // because a person who has not seen what is there cannot mean to replace it.
        "error": flows.as_ref().err().cloned().unwrap_or_default(),
        "workflow": flows
            .as_ref()
            .map(|f| f.iter().map(written).collect::<Vec<_>>())
            .unwrap_or_default(),
        "conditions": skein::workflow::conditions(),
        "actions": skein::workflow::actions(),
    }))
    .into_response()
}

/// A workflow in the shape the file has and the editor edits.
fn written(f: &skein::workflow::Workflow) -> serde_json::Value {
    serde_json::json!({
        "name": f.name,
        "matches": f.matches.iter().map(skein::workflow::spell_cond).collect::<Vec<_>>(),
        "steps": f.steps.iter().map(|s| serde_json::json!({
            "when": s.when.iter().map(skein::workflow::spell_cond).collect::<Vec<_>>(),
            "do": skein::workflow::spell_act(&s.act),
        })).collect::<Vec<_>>(),
    })
}

/// Replace the fleet's workflows with what the editor sends.
///
/// The whole file at once, because a workflow is only meaningful as an ordered whole and a
/// step-by-step API would let a half-written one run on the next tick.
async fn api_save_workflows(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let raw = body.to_string();
    match skein::workflow::save(raw.as_bytes()) {
        Ok(flows) => Json(serde_json::json!({
            "ok": true,
            "workflow": flows.iter().map(written).collect::<Vec<_>>(),
        })),
        // The refusal names the workflow, the step and the word — see `workflow::from_bytes`. It is
        // shown as it is: a message that says "invalid" teaches nobody the vocabulary.
        Err(error) => Json(serde_json::json!({ "ok": false, "error": error })),
    }
}

/// What every pull request in this repo's queue would have happen to it, and what could.
///
/// A separate read from the queue on purpose. The queue is what GitHub says and is cached for a
/// minute; this is skein's own answer about it, and it changes the moment somebody assigns a
/// workflow — folding the two together would mean a page that cannot show a choice taking effect
/// without re-reading GitHub.
async fn api_workflows(Path(id): Path<String>) -> Response {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    let out = tokio::task::spawn_blocking(move || {
        // A file skein cannot read is reported as itself, not as "no workflows": a fleet whose
        // workflows all stopped working because of a typo must say so, or the automation simply
        // appears to have been forgotten.
        let flows = skein::workflow::load()?;
        let queue = skein::prq::queue(&repo, false)?;
        let mut prs = serde_json::Map::new();
        for pr in &queue.prs {
            let facts = skein::prwork::facts_of(pr, &queue.viewer, &queue.trunk);
            let standing = skein::prwork::standing(&repo.id, pr.number, &facts, &flows);
            prs.insert(
                pr.number.to_string(),
                serde_json::to_value(standing).unwrap_or_default(),
            );
        }
        Ok::<_, String>(serde_json::json!({
            "enabled": skein::prwork::enabled(),
            "read_prs": repo.read_prs,
            "defined": flows.iter().map(|f| serde_json::json!({
                "name": f.name,
                "matches": f.matches.iter().map(skein::workflow::spell_cond).collect::<Vec<_>>(),
                "steps": f.steps.iter().map(|s| serde_json::json!({
                    "when": s.when.iter().map(skein::workflow::spell_cond).collect::<Vec<_>>(),
                    "do": skein::workflow::spell_act(&s.act),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "prs": prs,
        }))
    })
    .await;
    match out {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct WorkflowReq {
    /// The workflow to put on this pull request. The empty string means "no workflow, and no rule
    /// either" — the exclusion, which is a choice and not an absence.
    ///
    /// Absent means **leave the choice alone**. It used to mean "forget it", which made
    /// `clear_stop` on its own silently take the workflow off the pull request as well: one button
    /// doing a second thing nobody asked it to.
    #[serde(default)]
    name: Option<String>,
    /// Forget the choice entirely, and let the rules speak for this pull request again.
    #[serde(default)]
    unassign: bool,
    /// Let a stopped workflow run again. What a person presses after fixing whatever stopped it.
    #[serde(default)]
    clear_stop: bool,
}

/// Choose what governs one pull request, or let it run again.
async fn api_set_workflow(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<WorkflowReq>,
) -> Json<serde_json::Value> {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    // The meaning of the three answers lives in `prwork`, where it is tested. It grew a bug in an
    // hour when it lived here: clearing a stop also un-assigned the workflow.
    let done = skein::prwork::apply(
        &repo.id,
        number,
        req.name.as_deref(),
        req.unassign,
        req.clear_stop,
    );
    match done {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}

/// Everything you can do to a PR from its row, behind one route.
///
/// One route rather than five because they share the whole of their setup — find the repo, fetch the
/// queue, locate the PR — and they differ only in the last line. Splitting them would be four more
/// copies of the same lookup, and four more places for the "is this PR actually yours" check to be
/// forgotten.
///
/// The `kind` values split along a line worth keeping visible: `ask` and `draft` produce text for
/// you and reach GitHub not at all, while `approve`, `request-changes`, `comment` and `merge` act
/// under your name. Drafting and posting are deliberately two calls.
#[derive(Deserialize)]
struct ActReq {
    /// approve | request-changes | comment | merge | ask | draft
    kind: String,
    /// The review body, the question, or the rough notes — depending on `kind`.
    #[serde(default)]
    body: String,
    /// Line comments written in the reading view. They post WITH the verdict — GitHub's own review
    /// semantics — so a verdict kind with comments goes through the review-with-comments call, and
    /// a non-verdict kind refuses them rather than dropping them silently.
    #[serde(default)]
    comments: Vec<skein::prq::ReviewComment>,
}

/// The change itself, for the reading view — the diff the reader already had a right to, at a
/// display budget, with an honest `cut` flag. No model call on this path: reading code needs no
/// summary, so this answers for unread PRs exactly as it does for read ones.
async fn api_pr_reading(Path((id, number)): Path<(String, u64)>) -> Json<serde_json::Value> {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "error": "no such repo" }));
    };
    let out = tokio::task::spawn_blocking(move || {
        let queue = skein::prq::queue(&repo, false)?;
        let pr = queue
            .prs
            .iter()
            .find(|p| p.number == number)
            .ok_or("that PR is not in your queue")?;
        skein::review::reading(&queue.slug, pr)
    })
    .await;
    Json(match out {
        Ok(Ok(r)) => serde_json::to_value(&r).unwrap_or_default(),
        Ok(Err(e)) => serde_json::json!({ "error": e }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    })
}

async fn api_review_act(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<ActReq>,
) -> Json<serde_json::Value> {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    let out = tokio::task::spawn_blocking(move || {
        let queue = skein::prq::queue(&repo, false)?;
        let pr = queue
            .prs
            .iter()
            .find(|p| p.number == number)
            .ok_or("that PR is not in your queue")?;
        let verdict = match req.kind.as_str() {
            "approve" => Some(skein::prq::Verdict::Approve),
            "request-changes" => Some(skein::prq::Verdict::RequestChanges),
            "comment" => Some(skein::prq::Verdict::Comment),
            _ => None,
        };
        let text = match (verdict, req.kind.as_str()) {
            (Some(v), _) if !req.comments.is_empty() => skein::prq::submit_review_with_comments(
                &queue.slug,
                number,
                &pr.head_sha,
                v,
                &req.body,
                &req.comments,
            )?,
            (Some(v), _) => skein::prq::submit_review(&queue.slug, number, v, &req.body)?,
            (None, _) if !req.comments.is_empty() => {
                return Err(format!(
                    "line comments post with a verdict — approve, request-changes or comment — \
                     not with {}",
                    req.kind
                ))
            }
            (None, "merge") => skein::prq::merge(&queue.slug, number)?,
            (None, "ask") => skein::review::ask(&repo, &queue.slug, pr, &req.body)?,
            (None, "draft") => skein::review::draft_comment(&repo, &queue.slug, pr, &req.body)?,
            (None, other) => return Err(format!("unknown action: {other}")),
        };
        // Anything that touched GitHub changed the lane this PR belongs in, and the queue is cached
        // for 60s — without this the row would sit in Needs you until the cache aged out.
        if matches!(
            req.kind.as_str(),
            "approve" | "request-changes" | "comment" | "merge"
        ) {
            skein::prq::invalidate(&id);
        }
        Ok::<_, String>(text)
    })
    .await;
    Json(match out {
        Ok(Ok(text)) => serde_json::json!({ "ok": true, "text": text }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// The configured work-tracking connections. Never carries a Plane token — only whether one is
/// stored, which is the whole question the settings screen needs answered.
async fn api_sync_status() -> Json<skein::tracking::SyncStatus> {
    Json(skein::tracking::sync_status())
}

/// Record which work tracker a box claims through — or that it claims through none.
///
/// Written *before* the box launches, so provisioning finds the answer already there rather than
/// minting a token against the repo's default and having it corrected afterwards. Absent
/// `connection` clears the override and returns the box to its repo's setting.
async fn api_set_box_tracking(Path(name): Path<String>, Json(r): Json<TrackingReq>) -> Response {
    match skein::tracking::set_box_tracking(&name, r.connection.as_deref()) {
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
    match skein::fleet::set_box_identity(&name, who) {
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
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let config = skein::config::load_config();
    let repo = skein::repos::repo_for_box(&name);
    let (git_name, git_email) = match &repo {
        Some(repo) => skein::fleet::box_identity(&name, repo),
        None => (config.git_name.clone(), config.git_email.clone()),
    };
    let own_identity = skein::fleet::box_identity_override(&name);
    Json(serde_json::json!({
        "name": name,
        "repo": repo.as_ref().map(|r| r.id.clone()).unwrap_or_default(),
        // the box's own choice, empty when it inherits
        "connection": skein::tracking::box_tracking(&name).unwrap_or_default(),
        // "" is a real answer (claims nowhere); absent is inheritance. A bare string cannot say which.
        "has_tracking_override": skein::tracking::box_tracking(&name).is_some(),
        "own_git_name": own_identity.clone().map(|(n, _)| n).unwrap_or_default(),
        "own_git_email": own_identity.map(|(_, e)| e).unwrap_or_default(),
        // The box's own answer, "" when it inherits — same grammar as tracking above. `effective`
        // is what it will actually come up with, which is not derivable in the page: it depends on
        // the fleet default *and* on whether a write token can be issued at all.
        // **Through `declared_read`, not out of the box's own directory** (§9.5 R8). These two were
        // read straight from `box_state`, which a box writes — so a box could not change what was
        // *enforced* (the effective values below come from `declared/`) but could change what this
        // panel told you its setting was. The next thing a person does with a settings panel is
        // press Save, which would have made the box's answer the declared one.
        "git_scope": skein::fleet::declared_read(&name, "git-scope").unwrap_or_default().trim().to_string(),
        "effective_git_scope": if skein::gitgate::box_is_scoped(&name) { "repo" } else { "fleet" },
        "git_scope_available": skein::gitgate::can_issue_write_tokens(),
        // The workshop box sees every box's files and can act at fleet scope. Reported per box so
        // the cockpit can say which one carries it without anyone opening a settings pane to check.
        "privileged": skein::fleet::box_is_privileged(&name),
        "own_disk": skein::fleet::declared_read(&name, "disk").unwrap_or_default().trim().to_string(),
        // and what is actually in force
        "effective_connection": skein::tracking::connection_for_box(&name).map(|c| c.label).unwrap_or_default(),
        "effective_git_name": git_name,
        "effective_git_email": git_email,
        "effective_disk_mb": skein::fleet::box_disk_limit(&name),
        // Asked of the box, because nothing host-side records it: the token lands in the box's own
        // `~/.config/sync/env`. One round trip, and only when someone opens this panel — the board
        // refreshes every 2s and could never pay for this per box.
        "wired": skein::sandbox::sbx_guest_output(
            &name,
            "test -s \"$HOME/.config/sync/env\" && echo wired",
            std::time::Duration::from_secs(15),
        )
        .unwrap_or_default()
        .contains("wired"),
        "agent": skein::repos::agent_for_box(&name),
        "repo_connection": repo
            .and_then(|r| skein::tracking::connection_for_repo(&r))
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
    match skein::fleet::set_box_disk_limit(&name, r.limit.as_deref()) {
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
    match skein::tracking::upsert_connection(
        req.id.as_deref(),
        &req.label,
        &req.gateway_url,
        req.token.as_deref().filter(|t| !t.trim().is_empty()),
    ) {
        Ok(_) => Json(skein::tracking::sync_status()).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Forget a connection entirely. Refused while a repo still selects it — see `remove_connection`.
async fn api_remove_connection(Path(id): Path<String>) -> Response {
    match skein::tracking::remove_connection(&id) {
        Ok(()) => Json(skein::tracking::sync_status()).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Forget one connection's stored token, keeping the connection itself.
async fn api_forget_connection_token(Path(id): Path<String>) -> Response {
    match skein::tracking::set_connection_token(&id, "") {
        Ok(()) => Json(skein::tracking::sync_status()).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Mint this box a tracker token and register the `sync` MCP server inside it. Spends a network
/// round trip and creates a real credential, so — like verify — it only ever happens on a click.
async fn api_sync_provision(Path(name): Path<String>) -> Response {
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::tracking::sync_provision_box(&name)).await {
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
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let force = q.get("replace").is_some_and(|v| v == "1" || v == "true");
    match tokio::task::spawn_blocking(move || skein::tracking::sync_refresh_box(&name, force)).await
    {
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
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let bytes = q
        .get("bytes")
        .and_then(|b| b.parse::<u64>().ok())
        .unwrap_or(256 * 1024);
    match tokio::task::spawn_blocking(move || skein::transcript::read_transcript(&name, bytes))
        .await
    {
        Ok(Ok(view)) => Json(view).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

/// The branch-vs-base patch a box last reported (plain text; empty when none yet).
async fn api_diff(Path(name): Path<String>) -> Response {
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    // Computed inside the box, so it forks a git in a sandbox — off the async runtime, like every
    // other blocking box call.
    let view = tokio::task::spawn_blocking(move || skein::diff::box_diff(&name))
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
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let rel = q.get("path").cloned().unwrap_or_default();
    let res = tokio::task::spawn_blocking(move || skein::files::list_box_files(&name, &rel))
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
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let rel = q.get("path").cloned().unwrap_or_default();
    let ext = rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let res = tokio::task::spawn_blocking(move || skein::files::read_box_file(&name, &rel))
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
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::digest::session_digest(&name)).await {
        Ok(Some(d)) => Json(d).into_response(),
        _ => (StatusCode::NOT_FOUND, "no such box").into_response(),
    }
}

/// All cross-box messages, newest first.
async fn api_mailbox() -> Json<Vec<skein::mailbox::Message>> {
    Json(skein::mailbox::load_mailbox())
}

#[derive(Deserialize)]
struct SendReq {
    to: String,
    body: String,
    #[serde(default)]
    kind: String,
}

/// List the repos skein manages, each with the GitHub repository it maps to as `slug`.
///
/// `slug` is resolved here rather than in the browser because for a repo adopted from a local path
/// the answer lives in the clone's `origin` — a `git` call only the host can make. The browser parsed
/// `source` on its own and therefore called every adopted-in-place repo "not a GitHub remote",
/// disagreeing with the host about the same repo. Empty string ⇒ no GitHub remote anywhere.
async fn api_repos() -> Json<Vec<serde_json::Value>> {
    Json(
        skein::repos::load_repos()
            .into_iter()
            .map(|r| {
                let slug = skein::gitgate::repo_slug(&r).unwrap_or_default();
                let mut value = serde_json::to_value(&r).unwrap_or_else(|_| serde_json::json!({}));
                if let Some(fields) = value.as_object_mut() {
                    fields.insert("slug".into(), serde_json::Value::String(slug));
                }
                value
            })
            .collect(),
    )
}

/// Runtime choices come from the core adapter registry so every client stays in sync when a new
/// provider is added.
async fn api_runtimes() -> Json<Vec<skein::runtime::RuntimeInfo>> {
    Json(skein::runtime::supported_runtimes())
}

async fn api_health() -> Json<skein::health::HealthReport> {
    Json(
        tokio::task::spawn_blocking(skein::health::health_report)
            .await
            // Every check UNKNOWN, and none of them a fault. The health task falling over says
            // nothing about whether sbx is installed or whether scoping is configured — a red line
            // for each would blame seven subsystems for one panic somewhere else entirely, which is
            // what this used to do with `ok: false`. `ok: false` on the report itself stays, so the
            // cockpit still says something is wrong; it is now the report that is broken rather
            // than everything it was asked about.
            .unwrap_or_else(|error| skein::health::HealthReport {
                ok: false,
                // The one field a crashed health task can still answer: it is about the binary,
                // not about anything the task had to go and ask.
                build: skein::health::BUILD_REVISION,
                registry: skein::health::HealthCheck::unknown(format!(
                    "the health check itself failed: {error}"
                )),
                sbx: skein::health::HealthCheck::unknown("the health check itself failed"),
                git: skein::health::HealthCheck::unknown("the health check itself failed"),
                gh: skein::health::HealthCheck::unknown("the health check itself failed"),
                ai: skein::health::HealthCheck::unknown("the health check itself failed"),
                probes: skein::health::HealthCheck::unknown("the health check itself failed"),
                mailbox: skein::health::HealthCheck::unknown("the health check itself failed"),
                memory: skein::health::HealthCheck::unknown("the health check itself failed"),
                gitgate: skein::health::HealthCheck::unknown("the health check itself failed"),
                warden: skein::health::HealthCheck::unknown("the health check itself failed"),
                cover: skein::health::HealthCheck::unknown("the health check itself failed"),
                logins: Vec::new(),
                expired_logins: Vec::new(),
                dark_boxes: Vec::new(),
                stale_boxes: Vec::new(),
                uncovered_boxes: Vec::new(),
                uncapped_boxes: Vec::new(),
                runtimes: skein::runtime::supported_runtimes(),
                // Empty rather than guessed: this is the report for a health task that *failed*, and
                // the checklist reads this field as "boxes can push". Naming a credential here would
                // tick that step off on the strength of a crash.
                git_credential: String::new(),
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
        skein::repos::add_repo(
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
            let warning = skein::repos::remote_warning(&repo);
            // The repository this maps to, now that there is a clone to ask. The dialog cannot know
            // it while you are still typing a *path* — only adopting it reveals the origin — so this
            // is what lets a write token offered in the dialog be stored against the right repo.
            let slug = skein::gitgate::repo_slug(&repo).unwrap_or_default();
            Json(serde_json::json!({ "repo": repo, "warning": warning, "slug": slug }))
                .into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

/// Unregister a repo (files left on disk).
async fn api_remove_repo(Path(id): Path<String>) -> Response {
    match skein::repos::remove_repo(&id) {
        Ok(repo) => Json(repo).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// Pull the latest code into a repo's working clone (fast-forward only). `git pull` hits the network,
/// so run the blocking work off the async runtime.
async fn api_pull_repo(Path(id): Path<String>) -> Response {
    let res = tokio::task::spawn_blocking(move || skein::repos::pull_repo(&id)).await;
    match res {
        Ok(Ok(summary)) => Json(serde_json::json!({ "summary": summary })).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

/// Read skein's app settings (the cockpit's toggles).
async fn api_settings() -> Json<skein::config::Config> {
    Json(skein::config::load_config())
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
    // The merge happens INSIDE the lock, against settings read inside it. Reading the base outside
    // is what made two tabs lose each other: each merged its patch onto a snapshot taken before the
    // other saved, so the second write put the first one's field back to what it had been.
    let saved = skein::config::update_config(|c| {
        let mut merged = match serde_json::to_value(&*c) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => return Err("unreadable config".to_string()),
        };
        for (key, value) in patch {
            merged.insert(key.clone(), value.clone());
        }
        *c =
            serde_json::from_value(serde_json::Value::Object(merged)).map_err(|e| e.to_string())?;
        Ok(c.clone())
    });
    let c = match &saved {
        Ok(c) => c.clone(),
        Err(_) => skein::config::Config::default(),
    };
    match saved.map(|_| ()) {
        Ok(()) => {
            // Apply a newly-set SSH key immediately (load into the agent) so the user needn't restart.
            if let Err(e) = skein::config::ensure_ssh_key() {
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
    match skein::fleet::apply_box_limits() {
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
        // Not a credential — the id is public, and the settings screen names it so "scoped" can say
        // *what by*. The key it pairs with is a path that never leaves the host.
        "app_id": skein::config::load_config().github_app_id,
        "app_problem": skein::gitgate::app_credentials().err().unwrap_or_default(),
        // Whether a write token can be issued *at all* — by App or by a stored PAT. This is what
        // scoping is gated on, so it is the honest "is this switched on" answer; `app_ready` alone
        // would read as off for someone using nothing but their own tokens.
        "ready": skein::gitgate::can_issue_write_tokens(),
        // Whether, never what. The optional read PAT is write-only like every token here, so the
        // settings screen can offer "replace" instead of "add" without the token crossing the wire.
        "read_pat_set": skein::gitgate::read_pat().is_some(),
        // The third credential path, so the status line can tell "boxes hold the account token" from
        // "boxes hold nothing". Those used to be the same sentence, because the account token was
        // seeded by default and therefore always the answer; now that it is chosen, a fleet with
        // nothing configured genuinely has no way to push and the pane has to say so.
        "account_token": skein::config::load_config().seed_gh_secret,
        "account_seeded": skein::repos::gh_secret_seeded().is_some(),
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
        for view in load_views()
            .unwrap_or_default()
            .iter()
            .filter(|v| !v.foreign)
        {
            let _ = skein::gitgate::refresh_tokens(&view.name);
        }
    });
    StatusCode::NO_CONTENT.into_response()
}

#[derive(serde::Deserialize)]
struct ReadTokenReq {
    /// The PAT itself. An empty string forgets the stored one — the only way to clear it, since
    /// nothing reads it back to compare against.
    #[serde(default)]
    token: String,
}

/// Store (or forget) the optional read-only PAT.
///
/// Its own route rather than a field on the settings form, and for the same reason the write tokens
/// have one: `config.json` round-trips through the browser on every save, so a token in it would be
/// handed to every tab that opens Settings. This one is write-only — no route serves it back, and
/// the page only ever learns whether one is set.
async fn api_git_read_token(Json(r): Json<ReadTokenReq>) -> Response {
    if let Err(e) = skein::gitgate::set_read_pat(&r.token) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    // Reads are placed by the same sweep that places writes, so a token stored now reaches the
    // boxes without waiting for the next tick.
    tokio::task::spawn_blocking(|| {
        for view in load_views()
            .unwrap_or_default()
            .iter()
            .filter(|v| !v.foreign)
        {
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
                for view in load_views()
                    .unwrap_or_default()
                    .iter()
                    .filter(|v| !v.foreign)
                {
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
    /// **What the page actually showed.** The grant is built from these, not from a re-read.
    ///
    /// The box matters as much as the repo, and that is easy to miss: `refresh_tokens` writes the
    /// minted installation token into the box the grant names, so a request swapped between render
    /// and click is not "a different repository" — it is a live write token landing in a box of the
    /// requester's choosing.
    #[serde(rename = "box", default)]
    box_name: String,
    #[serde(default)]
    repo: String,
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
        tokio::task::spawn_blocking(move || {
            let rendered = skein::gitgate::Request {
                id,
                box_name: r.box_name,
                repo: r.repo,
                state: "pending".into(),
                ..Default::default()
            };
            skein::gitgate::fleet_decide(&rendered, r.approve, hours)
        })
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

#[derive(serde::Deserialize)]
struct PrivilegedReq {
    on: bool,
}

/// Make one box the workshop box, or return it to being ordinary. Next start.
async fn api_set_box_privileged(
    Path(name): Path<String>,
    Json(r): Json<PrivilegedReq>,
) -> Response {
    match skein::fleet::set_box_privileged(&name, r.on) {
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
async fn api_substrate_decide(Path(id): Path<String>, Json(r): Json<DecideReq>) -> Response {
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

async fn api_fleet_transport() -> Json<skein::fleet::Transport> {
    // Blocking: it opens a socket to the agent. Cheap, but not on an async worker.
    Json(
        tokio::task::spawn_blocking(skein::fleet::transport_state)
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
async fn api_fleet_load() -> Json<Vec<skein::fleet::BoxLoad>> {
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
async fn api_machine_sandboxes() -> Response {
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
async fn api_machine_doorstep() -> Json<serde_json::Value> {
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
/// The counters live in the sandbox and are read by the agent, so this is one HTTP call and no
/// subprocess on either side. Asked when somebody wants it, never on a tick.
///
/// A rate rather than a total, because everything the kernel keeps here is monotonic since boot: a
/// raw `98305` says the same enormous thing for ever and never says whether it is happening now.
async fn api_machine_pressure() -> Response {
    match tokio::task::spawn_blocking(skein::fleet::pressure).await {
        Ok(Some(p)) => Json(p).into_response(),
        // No agent to ask is not a fleet under pressure, and answering zero would be inventing
        // news. 503 with the reason, so a surface can say "not known" rather than "fine".
        Ok(None) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "no fleet agent answered, so the kernel's pressure counters could not be read"
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn api_fleet_resources() -> Response {
    match tokio::task::spawn_blocking(skein::fleet::fleet_resources).await {
        Ok(Some(r)) => Json(r).into_response(),
        _ => StatusCode::NO_CONTENT.into_response(),
    }
}

/// What creating the fleet sandbox would take, and from what.
///
/// The fleet sandbox is the largest thing skein builds on someone's machine, and it used to appear
/// as a side effect of launching a first box — sized by whatever `fleet_memory` said, which on a new
/// install is a number chosen for somebody else's laptop. Nobody was ever shown it, and it cannot be
/// changed afterwards without a rebuild: sbx fixes memory, CPUs and disk at creation.
///
/// So this is the dialog's whole content in one call: what the host has, what skein proposes to take
/// of it, and whether there is a sandbox already. `exists` is a tri-state on purpose — `null` means
/// sbx could not be asked, which must not be shown as "no sandbox yet" or the answer would be to
/// create a second one.
async fn api_fleet_plan() -> Json<serde_json::Value> {
    let (host, exists, sandbox) = tokio::task::spawn_blocking(|| {
        let sandbox = skein::place::fleet_sandbox();
        let exists = (!sandbox.is_empty())
            .then(|| skein::fleet::fleet_exists(&sandbox))
            .flatten();
        (skein::fleet::host_capacity(), exists, sandbox)
    })
    .await
    .unwrap_or_else(|_| (skein::fleet::host_capacity(), None, String::new()));
    let proposed = skein::fleet::proposed_fleet_size(&host);
    Json(serde_json::json!({
        "sandbox": sandbox,
        "exists": exists,
        "why": skein::sbx::fleet_failure(),
        "host": host,
        "proposed": proposed,
    }))
}

/// Create the fleet sandbox at the size the person just confirmed.
///
/// The numbers are saved before the create, not after: `create_argv` and `create_env` read the
/// config, so a size that was only passed here would be ignored by the very command it is for. It
/// also means the sandbox and the settings agree afterwards, which is what a later resize starts
/// from.
async fn api_fleet_create(Json(r): Json<ResizeReq>) -> Response {
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
        skein::fleet::ensure_fleet(&sandbox, &skein::fleet::fleet_mounts()).map(|()| sandbox)
    })
    .await;
    match out {
        Ok(Ok(sandbox)) => Json(serde_json::json!({ "sandbox": sandbox })).into_response(),
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
/// 200 with the boxes that failed to come back: their work is already snapshotted on the host, so a
/// partial return is a retry (`skein start <box>`), not a failure of the resize.
async fn api_fleet_resize(Json(r): Json<ResizeReq>) -> Response {
    match skein::fleet::resize_fleet(&r.memory, &r.cpus, &r.disk, r.drop_docker) {
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
    match skein::mailbox::send_message(to, &r.kind, &r.body) {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Does this typed path resolve, and to what?
///
/// **The replacement for Browse, and not a smaller version of it.** The native picker needs a host
/// with display access, which in-fleet skein cannot have, and its main job — choosing a local
/// repository path — went away when repositories became remotes (`docs/parity.md` §7). What is left
/// is two fields where somebody types a path, and law 1 says a surface must tell you what it found
/// rather than accept it silently.
///
/// `symlink_metadata`, so a link is reported as a link rather than as whatever it points at. That is
/// not pedantry on a screen whose whole job is to say what is actually there — and it is the same
/// rule §9.5 R8 applies everywhere else skein looks at a path somebody else can shape.
///
/// It says nothing a caller could not learn by asking the fleet to use the path, and it is behind
/// the token like everything else.
async fn api_path(Query(q): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
    let asked = q.get("p").cloned().unwrap_or_default();
    let path = skein::util::expand_tilde(asked.trim());
    if path.is_empty() {
        return Json(serde_json::json!({ "resolved": false, "kind": "empty", "why": "" }));
    }
    let found = tokio::task::spawn_blocking(move || {
        let p = std::path::PathBuf::from(&path);
        match std::fs::symlink_metadata(&p) {
            Err(e) => serde_json::json!({
                "resolved": false,
                "kind": "missing",
                "path": path,
                "why": format!("{e}"),
            }),
            Ok(how) => {
                let kind = if how.file_type().is_symlink() {
                    "link"
                } else if how.is_dir() {
                    "folder"
                } else {
                    "file"
                };
                serde_json::json!({ "resolved": true, "kind": kind, "path": path, "why": "" })
            }
        }
    })
    .await;
    Json(found.unwrap_or_else(
        |e| serde_json::json!({ "resolved": false, "kind": "unknown", "why": e.to_string() }),
    ))
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
    if !skein::util::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let branch = r.branch;
    let res = tokio::task::spawn_blocking(move || skein::repos::repin_branch(&name, &branch)).await;
    Json(match res {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
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
    if !skein::util::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let prompt = body.map(|Json(b)| b.prompt).unwrap_or_default();
    let r = tokio::task::spawn_blocking(move || skein::sandbox::resume_box(&name, &prompt)).await;
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
    if !skein::util::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let runtime = body
        .map(|Json(value)| value.runtime)
        .filter(|value| !value.trim().is_empty());
    let result = tokio::task::spawn_blocking(move || {
        skein::sandbox::restart_agent_session(&name, runtime.as_deref())
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
    if !skein::util::valid_name(&name) {
        return Json(serde_json::json!({ "summary": null }));
    }
    let s = tokio::task::spawn_blocking(move || skein::ai::narrate(&name))
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
    let (resumed, held) =
        tokio::task::spawn_blocking(move || skein::sandbox::resume_batch(&r.names))
            .await
            .unwrap_or_default();
    Json(serde_json::json!({ "ok": true, "resumed": resumed, "held": held }))
}

/// Stop a box: halt the sandbox (frees compute; resume later via attach). Non-destructive — the box
/// stays listed and goes stale until resumed. Returns {ok} or {ok:false, error}.
async fn api_stop(Path(name): Path<String>) -> Json<serde_json::Value> {
    if !skein::util::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let r = tokio::task::spawn_blocking(move || skein::sandbox::stop_box(&name)).await;
    Json(match r {
        Ok(Ok(())) => serde_json::json!({ "ok": true }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}

/// Destroy a box: tear the sandbox down (sbx rm — reclaims its resources) then delist it.
/// Destructive. Returns {ok} or {ok:false, error}.
async fn api_destroy(Path(name): Path<String>) -> Json<serde_json::Value> {
    if !skein::util::valid_name(&name) {
        return Json(serde_json::json!({ "ok": false, "error": "invalid box name" }));
    }
    let r = tokio::task::spawn_blocking(move || skein::sandbox::destroy_box(&name)).await;
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
    /// A thread and a channel rather than a direct call because [`skein::place::AgentWrite`] is
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
    fn over(write: skein::place::AgentWrite) -> Sink {
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
    let mut rel = skein::util::pct_decode(&hdr("x-skein-name"));
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
    let (dir, path) = skein::sandbox::drop_dest(&batch, &rel)?;

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
        Some(n) if n <= skein::place::AGENT_WRITE_CAP => {
            let (box_name, dir, path) = (name.to_string(), dir.clone(), path.clone());
            tokio::task::spawn_blocking(move || {
                skein::sandbox::begin_box_write(&box_name, &dir, &path, UPLOAD_TIMEOUT)
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
            let argv = skein::sandbox::box_write_argv(name, &dir, &path)?;
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
    let inner = format!("rm -f {}", skein::util::sh_quote(path));
    // Through the placement, like the write it is undoing — `sbx exec <box>` names no sandbox in the
    // fleet, so the cleanup would fail exactly when the upload it is cleaning up did. And through
    // the agent when there is one, for the same reason: the moment a half-written file most needs
    // removing is the moment a fresh `sbx exec` is least likely to come back.
    let name = name.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        skein::place::place_of(&name).map(|place| place.exec(&inner, Duration::from_secs(10)))
    })
    .await;
}

/// Live fleet stream: an opening snapshot, then only what moved.
///
/// One producer feeds every client — see `start_producing` — and the transitions come from
/// `skein::stream`. The roadmap note that used to sit here ("replace polling with a subscription")
/// is done, and left as a stale comment it would describe the shape this no longer has.
async fn api_events() -> Response {
    // Post-accept and post-auth, like the PTY cap beside it. **Not the whole story**: the auth gate
    // runs after accept, so a cap here bounds authenticated clients and leaves connection exhaustion
    // before auth to the accept loop. Written down rather than implied — a box getting a free denial
    // of the control plane, and therefore of the approval surface, with no credential at all is a
    // different hazard at a different layer.
    let Ok(permit) = EVENT_LIMIT.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "too many live boards open — close one and retry",
        )
            .into_response();
    };
    // **Subscribe first, then start.** The producer stops when nobody is listening, and a tokio
    // interval fires immediately — so starting before subscribing means the first tick counts zero
    // listeners and the producer exits, leaving this client with an opening snapshot and silence
    // for ever. Found by reading; a test with one client would not have shown it as a race.
    let (snapshot, rest) = skein::stream::subscribe();
    start_producing();
    let following = tokio_stream::wrappers::BroadcastStream::new(rest).map(move |item| {
        // Held for the life of the stream, so the cap counts open boards rather than requests.
        let _permit = &permit;
        match item {
            Ok(tick) => sse(&tick),
            // **Told, never silently skipped.** A hole in the stream is worse than a gap you can
            // see: the board would look current and be wrong. So the client is told how many it
            // missed, and asks for a fresh snapshot to re-sync from.
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(missed)) => {
                Ok(Event::default().event("behind").data(missed.to_string()))
            }
        }
    });
    Sse::new(tokio_stream::once(sse(&snapshot)).chain(following)).into_response()
}

/// One tick, as the wire carries it.
///
/// The event name is the tag inside the payload, so a client switches on one thing rather than two
/// that can disagree.
fn sse(tick: &skein::stream::Tick) -> Result<Event, Infallible> {
    let name = match tick {
        skein::stream::Tick::Snapshot { .. } => "snapshot",
        skein::stream::Tick::Changed { .. } => "changed",
        skein::stream::Tick::Alive => "alive",
    };
    Ok(Event::default()
        .event(name)
        .data(serde_json::to_string(tick).unwrap_or_else(|_| "{}".into())))
}

/// The one producer. Started by the first client, stopped when the last one leaves.
///
/// **Every client used to run this itself** — five tabs were five fleet snapshots a tick, each
/// shelling out. A gate cannot fix that: the work was per client by construction. And stopping when
/// nobody is listening is a property the old shape could not have at all, because there was nobody
/// to notice.
fn start_producing() {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        let mut every = tokio::time::interval(skein::stream::TICK);
        // A tick that is late does not become two ticks in a row. The default policy bursts to catch
        // up, which for a snapshot means running the most expensive thing skein computes twice with
        // no gap — at a client that was already slow.
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if skein::stream::listeners() == 0 {
                RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
                // Double-checked, because the gap between deciding to stop and saying so is a gap a
                // client can arrive in: it would subscribe, find `RUNNING` still true, start
                // nothing, and then be left with a producer that had already gone. So look again —
                // and if somebody else has taken the flag in the meantime, they are producing now
                // and this one may leave.
                if skein::stream::listeners() == 0 {
                    return;
                }
                if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
            }
            // On the blocking pool: `load_views` is 1-2s of synchronous work for a busy fleet, and
            // running it on an async worker starves every terminal socket scheduled there. That was
            // the "typing lags only when the box is idle" freeze.
            let views = tokio::task::spawn_blocking(|| load_views().unwrap_or_default())
                .await
                .unwrap_or_default();
            skein::stream::publish(views);
        }
    });
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
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    let launch = q.get("launch").filter(|s| !s.is_empty()).cloned();
    let shell = q
        .get("shell")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let agent = q
        .get("agent")
        .filter(|a| skein::runtime::valid_runtime(a))
        .cloned();
    let handoff = q
        .get("handoff")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let from = q
        .get("from")
        .filter(|a| skein::runtime::valid_runtime(a))
        .cloned();
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
    let target_agent = agent.unwrap_or_else(|| skein::repos::agent_for_box(&name));
    if handoff && !shell {
        let hn = name.clone();
        let ht = target_agent.clone();
        let hf = from.clone();
        match tokio::task::spawn_blocking(move || {
            skein::handoff::prepare_handoff(&hn, hf.as_deref(), &ht)
        })
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
            tokio::task::spawn_blocking(move || skein::fleet::absent_box_reason(&boxed)).await
        {
            let _ = socket.send(Message::Text(format!("skein: {why}"))).await;
            return;
        }
        let boxed = name.clone();
        if let Ok(Err(e)) =
            tokio::task::spawn_blocking(move || skein::fleet::ensure_box_session(&boxed)).await
        {
            let _ = socket.send(Message::Text(format!("skein: {e}\r\n"))).await;
        }
    }
    let dir = skein::sbx::lookup_dir(&name).unwrap_or_default();
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
        b.arg(skein::sandbox::launch_command_with_agent(
            &name,
            branch,
            Some(&target_agent),
        ));
        b
    } else {
        match std::env::var(override_var) {
            Ok(c) if !c.is_empty() => {
                let c = c
                    .replace("{name}", &skein::util::sh_quote(&name))
                    .replace("{dir}", &skein::util::sh_quote(&dir));
                let mut b = CommandBuilder::new("sh");
                b.arg("-c");
                b.arg(c);
                b
            }
            _ => {
                // The program comes from the argv rather than being spelled here. It is `sbx` on a
                // host and something else in the fleet, and a spawner that names it cannot be told
                // otherwise — which is how a terminal ends up attached to the wrong machine.
                let argv = if shell {
                    skein::sandbox::shell_argv(&name)
                } else {
                    skein::sandbox::attach_argv_as(&name, &dir, &target_agent)
                };
                let mut b = CommandBuilder::new(argv.first().map(String::as_str).unwrap_or("sh"));
                for a in argv.iter().skip(1) {
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
    use super::{origin_ok, refuse_unknown_args, slow_down};
    use axum::http::{header::ORIGIN, HeaderMap, HeaderValue};
    use std::time::Duration;

    /// No security-deciding setting is read out of the directory a box writes (§9.5 R8).
    ///
    /// A source assertion, because that is what this is about: the failure is not a wrong value, it
    /// is a *path*, and a path is a string somebody types. Two of these were reading `git-scope` and
    /// `disk` straight from `box_state` — the enforced values were always read from `declared/`, so
    /// a box could not promote itself, but it could tell this panel that its own setting was
    /// something else. The next thing anybody does with a settings panel is press Save.
    ///
    /// `declared_read` is the only way in, and it also warns about a value left at the old path
    /// rather than believing it.
    #[test]
    fn a_boxs_own_directory_is_not_where_its_settings_are_read_from() {
        let me = include_str!("skein-server.rs");
        for flag in ["privileged", "git-scope", "disk", "identity"] {
            let out_of_the_box = format!("box_state(&name)).join(\"{flag}\")");
            assert!(
                !me.contains(&out_of_the_box),
                "`{flag}` is read out of the box's own directory, which the box writes"
            );
        }
        assert!(
            me.contains("declared_read(&name, \"git-scope\")")
                && me.contains("declared_read(&name, \"disk\")"),
            "the settings panel stopped reading these through `declared_read`"
        );
    }

    /// A control plane that cannot accept must wait, and must not give up.
    ///
    /// The failure this replaces is a tight loop: `accept` failing and being retried immediately
    /// spends the CPU of the process that has just run out of descriptors, and the condition that
    /// causes it — many connections at once — is the condition the accept loop exists to survive.
    #[test]
    fn a_failing_accept_pauses_and_keeps_trying() {
        // The first failure is nearly free: one aborted connection must not cost a real client a
        // measurable wait.
        assert!(slow_down(1) <= Duration::from_millis(2));
        // It grows.
        assert!(slow_down(4) > slow_down(2));
        // And it stops growing, which is what makes this a pause rather than a shutdown: the
        // descriptors come back when connections close, and nobody has to restart anything.
        let ceiling = slow_down(8);
        assert_eq!(slow_down(1_000), ceiling);
        assert_eq!(slow_down(u32::MAX), ceiling);
        assert!(
            ceiling <= Duration::from_millis(250),
            "a cockpit that waited longer than this would read as hung rather than busy"
        );
    }

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
