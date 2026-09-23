//! skein-server — the web cockpit. A tiny axum server over `skein` (lib):
//!   GET /                          the dark web board (self-contained, no build)
//!   GET /api/boxes                 fleet snapshot (JSON)
//!   GET /api/events                live fleet stream (SSE)
//!   GET /api/boxes/:name/terminal  WebSocket ↔ PTY ↔ persistent in-box tmux session
//!   GET /api/login/:runtime/terminal  WebSocket ↔ PTY ↔ one-shot `skein login` in the fleet HOME
//!
//! The terminal reuses wheels: portable-pty (server PTY) + xterm.js (browser). We write only the
//! WS↔PTY bridge. Bind is loopback-only by default; for remote access either `tailscale serve`
//! proxies to this loopback port, or `$SKEIN_ADDR` binds off-loopback and the box is reached at its
//! tailnet address directly. The tailnet is the auth boundary either way. See README "Remote access".
//!
//! ## Layout
//!
//! `main` and the router are here, with the few helpers several route files share. Every handler
//! is in a sibling file named for the area it serves, and the `use` of each file below is what
//! lets the router name them as it always did. Tests sit beside the code they test;
//! `cockpit_routes`, which reads the router against the pages, is here because the router is.

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
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

mod boxes;
mod door;
mod events;
mod fleet;
mod git;
mod health;
mod repos;
mod review;
mod settings;
mod terminal;
mod update;
mod upload;
mod workflows;

use boxes::*;
use door::*;
use events::*;
use fleet::*;
use git::*;
use health::*;
use repos::*;
use review::*;
use settings::*;
use terminal::*;
use update::*;
use upload::*;
use workflows::*;
// Vendored, not CDN-loaded: the terminal must work in the firewalled sbx network the tool lives in.
// They are no longer four constants and four handlers — `skein::assets` generates the table from the
// directory, because a build step emits files whose names carry content hashes and neither the count
// nor the names are known here. The four URLs are unchanged; only the code behind them is.
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

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

/// Everything the server does, on the runtime [`main`] builds once the socket's variables are gone.
async fn serve(handed: Result<Option<std::os::fd::RawFd>, String>) {
    // `main` has already withheld the socket from children and cleared its variables.
    // **This is a real skein, whatever `$SKEIN_TEST` says.** `tests/server/` and
    // `tests/ui/harness/server.mjs` both spawn this binary, and it inherits the marker from cargo's
    // `[env]` table — correctly, because `config::skein_home` and `util::fleet_root` still have to
    // refuse it an unpinned path, which is the whole of SKEIN-685. What it cannot do is install a
    // stand-in for its own crossings: that is a Rust closure and the harness is in another process.
    // So it says which side of `Place::spawning`'s guard it is on (SKEIN-530), and what keeps it
    // inside the fixture stays the fleet root it was handed. Held for the whole run.
    let _real = skein::place::seam::real_crossings();
    // **No `util::forward_interrupts()` here, and that is a decision rather than an omission.**
    // `src/bin/skein.rs` installs a `SIGINT` handler that forwards the signal to every bounded
    // child's process group, because a person typing Ctrl-C at a CLI means "stop the command you
    // are running for me". None of that is true of this binary. It has no terminal; it runs bounded
    // children for several browser clients at once on threads of its own, so a `SIGINT` arriving
    // here is not addressed to any one of them and killing all of their children would be wrong;
    // and it is stopped by whatever supervises it, which sends `SIGTERM`. Leaving the default
    // disposition alone means a `SIGINT` ends the server the way it always has.
    // Argv check before the bind and before anything is written: a typo changes nothing on disk.
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
    // The fleet's own kit, refreshed here so an upgraded skein updates the startup hook its
    // sandbox will run — the file is read by sbx at the next start, not by this process.
    if let Err(e) = skein::fleet::ensure_fleet_kit() {
        eprintln!("skein: ensure_fleet_kit: {e}");
    }
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
    // **The Docker daemon, watched.** `dockerd` runs in this sandbox with pid 1 for a parent and
    // nothing supervising it, so a container that gets it killed costs a rebuild of the whole fleet
    // to recover one process (`src/dockerd.rs`).
    //
    // This is where it lives because `skein-server` is the long-lived in-sandbox process. It used
    // to be the agent's, on exactly that reasoning, and moved here rather than being deleted with
    // it (SKEIN-573): the agent existed to survive a host-to-guest hop, and watching a daemon on
    // the same machine never was that.
    //
    // Its own OS thread rather than a tokio task, because a pass BLOCKS for the grace period — up
    // to five minutes once the backoff has opened — and that is a worker the async runtime would
    // rather have back. The loop owns its own sleeping.
    std::thread::Builder::new()
        .name("docker-watchdog".into())
        .spawn(skein::dockerd::watch_forever)
        .map(|_| ())
        .unwrap_or_else(|e| {
            eprintln!(
                "skein: the docker watchdog could not be started ({e}); a dockerd that dies will \
                 stay dead and cost a fleet rebuild"
            )
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
    // switched on for it, and then only pull requests somebody asked you to review — your own
    // limits, and the reason there is no daily quota: the scope is the budget.
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
    // The fleet's disk, said out loud rather than waited to be asked about (SKEIN-728, SKEIN-734).
    //
    // **Its own loop, for the reason the relay above states about itself**: a warning that only
    // renders when somebody has the cockpit open is not a warning. `stream.rs` starts its producer
    // at the first client and stops at the last — "a server nobody is watching does no work at
    // all" — and the day this exists for is the day the fleet reached 88% with nobody watching.
    //
    // **The body of that loop is `announce::watch_fleet_disk` and only the spawn is left here**
    // (SKEIN-738). Its period — five minutes, because that is `fleet_disk_usage`'s own gate — its
    // `spawn_blocking`, and its error handling were all written in this file, so the only thing that
    // could assert them was a test reading this file as text. That is
    // `announce::tests::the_server_is_what_runs_the_announcement`, and its own doc comment says
    // what it cannot do: tell whether a running server ever delivers anything. In the library the
    // same loop is driven at a period a test chooses, against a fixture fleet, and what is left
    // here is the one claim a source read is the right tool for — that something in the server
    // starts it at all. The relay above, a few file reads every five seconds, needs none of that.
    tokio::spawn(skein::announce::watch_fleet_disk());
    tokio::spawn(skein::fleet::watch_runtime_updates());
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
        // What the fleet has spent. Beside the health report rather than under `/api/boxes`
        // because it is one reading of the whole fleet, not a per-box one (SKEIN-835).
        .route("/api/usage", get(api_usage))
        .route("/api/runtimes", get(api_runtimes))
        .route("/api/repos", get(api_repos).post(api_add_repo))
        .route("/api/repos/:id", axum::routing::delete(api_remove_repo))
        .route("/api/repos/:id/pull", post(api_pull_repo))
        .route("/api/repos/:id/settings", post(api_set_repo_settings))
        .route("/api/review", get(api_review_merged))
        .route("/api/review/counts", get(api_review_counts))
        .route("/api/review/reading", get(api_review_reading))
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
        // The same reading, started rather than awaited: the answer arrives on `/api/events`, so
        // ten of these cost one browser connection between them instead of ten (SKEIN-366).
        .route("/api/repos/:id/review/:number/read", post(api_review_read))
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
        .route(
            "/api/repos/:id/review/:number/triggers",
            post(api_set_pr_triggers),
        )
        .route("/api/repos/:id/review/:number/act", post(api_review_act))
        // The shape of a change: which modules moved and how. The same route shape for both
        // sources, because the answer is the same question — `?box=` for a box's branch.
        .route("/api/repos/:id/review/:number/shape", get(api_pr_shape))
        .route("/api/boxes/:name/shape", get(api_box_shape))
        .route("/api/settings", get(api_settings).post(api_set_settings))
        .route("/api/fleet/plan", get(api_fleet_plan))
        .route("/api/fleet/create", post(api_fleet_create))
        .route("/api/fleet/resize", post(api_fleet_resize))
        .route("/api/fleet/save", post(api_fleet_save))
        .route("/api/fleet/limits", post(api_fleet_limits))
        .route("/api/fleet/resources", get(api_fleet_resources))
        .route("/api/fleet/load", get(api_fleet_load))
        // What else is on this machine — a question about the MACHINE, asked by a person. Not
        // "foreign boxes": `docs/parity.md` §7 removes that, and what survives is somebody running
        // more than one fleet needing to see them.
        .route("/api/machine/sandboxes", get(api_machine_sandboxes))
        .route("/api/machine/doorstep", get(api_machine_doorstep))
        .route("/api/machine/pressure", get(api_machine_pressure))
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
        // The press behind the bar's "update" (SKEIN-405). Its own route rather than a flag on
        // something else: this installs software into the sandbox every box shares, which is not a
        // thing to reach by accident.
        .route("/api/update-agents", post(api_update_agents))
        // Settings -> Update (SKEIN-1037); `restart` POSTs SKEIN-1029's press, GETs the holder.
        .route("/api/update", get(api_update))
        .route("/api/update/start", post(api_update_start))
        .route("/api/update/log", get(api_update_log))
        .route("/api/update/cancel", post(api_update_cancel))
        .route("/api/update/restart", get(api_holder).post(api_restart))
        .route("/api/login/:runtime/terminal", get(login_terminal))
        .route("/api/boxes/:name/terminal", get(terminal));

    // Everything above is routed; this decides who may drive it.
    //
    // A layer over the whole router rather than a check per handler, because the failure mode of
    // per-handler auth is a route added later that nobody remembers to guard — and this API grows a
    // route most weeks. What it lets through is named in one place, in `open_to_all`, which is a
    // list a reader can check against the router above.
    let app = app.layer(axum::middleware::from_fn(gate));

    // The headers every answer carries, for the same reason `gate` is a layer: a route added next
    // week inherits them without anybody remembering.
    //
    // `nosniff` is the one that pairs with `api_file`'s content types. Naming a type is only half
    // the fix — a browser that is allowed to sniff will still decide for itself that something is
    // HTML — and between them a box's file cannot become a document in this origin.
    //
    // The CSP is deliberately partial, and it is worth saying which half is missing. `object-src`,
    // `base-uri`, `frame-ancestors` and `form-action` are absolute: nothing in the cockpit uses a
    // plugin, rewrites its own base, is meant to be framed, or posts a form. `script-src` cannot yet
    // drop `'unsafe-inline'`, because the page IS one 600 KB inline script — and until it can, CSP
    // does not block a `javascript:` URL, which is why `cockpit/src/links.mjs` closes that directly
    // rather than leaning on this. What `script-src 'self'` does buy today is real: an injected
    // `<script src="https://elsewhere/">` is refused, and so is every `connect-src` off this origin,
    // which is the exfiltration half of any XSS that does get in.
    //
    // Dropping `'unsafe-inline'` is the security half of the front-end extraction the audit
    // recommends: every function lifted into `cockpit/src/` is a line of inline script that stops
    // needing it.
    let app = app.layer(axum::middleware::from_fn(security_headers));

    // The socket, and who opened it. `skein::doorway` says why this is not simply a bind: one
    // network namespace plus a port mapping that outlives skein means a box that binds the
    // cockpit's port *first* becomes the cockpit, and the browser hands it the fleet's token on the
    // first request (architecture §9.4). A socket opened before any box exists and inherited across
    // restarts is the only thing that closes it, since the token cannot.
    let listener = match skein::doorway::inherited(handed) {
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
    // **architecture §9.4's "the switch is refused" happens here** (SKEIN-962). The document had
    // said it since it was written and no code did it: `apiauth::disabled` returned a bool, the
    // `true` arm below printed a warning, and the server then served the whole API — git grants
    // included — to every box in the namespace. `apiauth::off_switch_refused` says where the
    // refusal applies and why a box cannot put the cockpit into it.
    //
    // **The refusal stays up and answers, rather than exiting.** The doorway restarts whatever it
    // started, two seconds later, for ever (the `main` loop in `src/server-doorway.py`), so a
    // refusal that exited would be a restart storm writing this sentence into a tmux pane nobody is
    // reading — SKEIN-645's shape exactly. Exiting would not even free the port: the door belongs
    // to the doorway and stays open, so every arrival would be accepted into its backlog and never
    // answered, which is a browser that hangs instead of one that is told what is wrong.
    let app = if skein::apiauth::off_switch_refused() {
        println!(
            "skein-server → http://{addr}\n  \
             REFUSING TO SERVE — $SKEIN_NO_API_AUTH is set and this server is under the fleet's \
             doorway, where the switch is refused (§9.4). Every request is answered with the reason."
        );
        eprintln!("{}", skein::apiauth::off_switch_refusal());
        refusal_only()
    } else {
        match skein::apiauth::disabled() {
            true => println!(
                "skein-server → http://{addr}\n  \
                 API AUTH OFF ($SKEIN_NO_API_AUTH) — anything that can reach this port drives the \
                 fleet, boxes included"
            ),
            // The token is printed, not just stored: this URL is how a browser gets a session, and
            // a secret nobody is shown is a secret nobody can use.
            false => match skein::apiauth::token() {
                // `expose()` — see the note on `apiauth::token`. The URL is the delivery channel.
                Ok(t) => println!("skein-server → http://{addr}/?t={}", t.expose()),
                Err(e) => println!(
                    "skein-server → http://{addr}\n  \
                     no API token could be created ({e}) — every API call will be refused until \
                     ~/.skein is writable"
                ),
            },
        }
        app
    };
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

/// **A plain `fn`, and the runtime built by hand, so that two things happen while this process has
/// one thread and no children** (SKEIN-1089): the handed-in socket is withheld from children, and
/// the two variables that describe it are read and cleared. `#[tokio::main]` built the multi-thread
/// runtime before the first line of `main`, so by the time the variables were cleared its worker
/// threads existed, and `heal_fleet`'s tmux and the watchers had already started with both in their
/// environment. `doorway::from_environment` says why they are cleared at all.
fn main() {
    skein::doorway::keep_from_children();
    let handed = skein::doorway::from_environment();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("skein-server: could not start the async runtime: {e}");
            std::process::exit(1);
        });
    runtime.block_on(serve(handed));
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

/// Is a query flag on? Present, and spelled `1` or `true`.
///
/// One reading of the idiom rather than one per flag. There are five of them on the review routes
/// now — `force`, `asked`, `rows`, `held`, `redraft` — and they all default the same safe way
/// round: absent or misspelled means OFF, so a caller that fumbles a marker gets the cautious
/// behaviour (the budgeted path, the full payload, the drafted review left where it is) rather
/// than the permissive one. `redraft` is the sharpest case: fumbled, a reader's vetted review
/// survives; honoured by accident, it is gone.
fn flag(q: &HashMap<String, String>, key: &str) -> bool {
    q.get(key).is_some_and(|v| v == "1" || v == "true")
}

#[cfg(test)]
/// `SKEIN_HOME`, `SKEIN_GITHUB_API` and `GH_TOKEN` are PROCESS-wide, and `cargo` runs these
/// tests as threads in one process. Two of them point skein at different GitHubs at the same
/// time and one gets the other's — which is SKEIN-307, and the reason this is a lock rather
/// than a comment asking people to be careful.
///
/// **At file scope, not inside a test module.** This binary has TWO — `tests` and
/// `review_routes` — and they are threads of one process. A lock inside either serialises
/// that module against itself and nothing else, which is how `origin_guard_honours_allowlist`
/// came to write `$SKEIN_ALLOWED_ORIGINS` with three siblings in the other module holding a
/// lock it could not take (SKEIN-307).
///
/// Not `#[serial]`: this crate has no such dependency, and a `Mutex` held for the body of the
/// test is the same guarantee. Poisoning is recovered from rather than propagated — a test
/// that already failed must not turn every other test in this module red as well.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
/// A scratch directory for this binary's tests, named so that what looks after the library's test
/// scratch recognises it too (SKEIN-1007).
///
/// `testutil::tempdir()` itself is out of reach, for the reason `review_routes`' `EnvPins` gives:
/// `testutil` is `#[cfg(test)]` inside the LIBRARY, and this is a separate `[[bin]]`. So what is
/// shared is the NAME. `skein-test-` at the head is the prefix `tests/ui/harness/leaks.mjs` derives
/// from `tempdir()`, so a process carrying this path is a fixture process to it; and it is the one
/// `testutil::sweep_stale_runs` removes once it is over an hour old, so a directory a panicking test
/// here leaves behind is reaped like one of the library's own.
///
/// The fixed head comes FIRST. `home_for` used to spell its home `skein-{what}-{pid}`, whose
/// literal cut at the first `{` is the bare stem `skein-` — a derived prefix that would match this
/// repository's own checkout path on every process that names it.
///
/// Emptied first: the name is pid-unique, not run-unique, and a recycled pid finds a crashed run's
/// leftovers.
fn scratch_dir(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("skein-test-server-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory for this test");
    dir
}

#[cfg(test)]
/// This binary's source, EVERY file of it, for the tests that read the server as text.
///
/// It was one file, and they read it with `include_str!` of that file. Re-rooting each to the one
/// file its subject lives in now would read less than it did: `the_route_the_pane_opens_is_what_prunes`
/// counts the callers of `review::prune` across the whole server, and a second caller in a sibling
/// file is exactly what it exists to catch (SKEIN-1103). So the directory is read rather than a list
/// of it, and a file added later is read without anybody remembering to add it.
///
/// `main.rs` first and the rest by name. A read that finds `main.rs` alone refuses: a `!contains`
/// over too little text passes, and says nothing.
fn server_source() -> &'static str {
    static SOURCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SOURCE.get_or_init(|| server_files().into_iter().map(|(_, text)| text).collect())
}

#[cfg(test)]
/// [`server_source`] with each file cut at its first line opening `#[cfg(test)]` — the production
/// half, which is what a `take_while` to that line was when the server was one file.
fn server_production() -> &'static str {
    static SOURCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SOURCE.get_or_init(|| {
        server_files()
            .into_iter()
            .map(|(_, text)| {
                let shipped: Vec<&str> = text
                    .lines()
                    .take_while(|l| !l.starts_with("#[cfg(test)]"))
                    .collect();
                shipped.join("\n") + "\n"
            })
            .collect()
    })
}

#[cfg(test)]
/// `(file name, text)` for every `.rs` file of this binary, `main.rs` first.
fn server_files() -> Vec<(String, String)> {
    let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/bin/skein-server"));
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rs"))
        .collect();
    names.sort_by_key(|n| (n != "main.rs", n.clone()));
    assert!(
        names.len() > 1 && names[0] == "main.rs",
        "read {names:?} out of {} — that is not this binary's source, and every source assertion \
         made over it would be about nothing",
        dir.display()
    );
    names
        .into_iter()
        .map(|n| {
            let text = std::fs::read_to_string(dir.join(&n)).unwrap_or_else(|e| panic!("{n}: {e}"));
            (n, text)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A marker nobody spelled is OFF, and that direction is the whole safety of these routes.
    ///
    /// `force`, `asked`, `rows` and `held` all pass through one reader, and every one of them is
    /// written so that failing to send it costs a caller the permissive behaviour, never grants
    /// it: no `asked` means the day's budget applies, no `held` means the request may compute, no
    /// `rows` means the full payload. A reader that treated "present" as true would turn
    /// `?asked=0` into an un-budgeted model call.
    #[test]
    fn a_review_marker_nobody_spelled_is_off() {
        let q: std::collections::HashMap<String, String> = [
            ("on", "1"),
            ("alsoOn", "true"),
            ("off", "0"),
            ("alsoOff", "false"),
            ("empty", ""),
            ("shouty", "TRUE"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert!(flag(&q, "on") && flag(&q, "alsoOn"));
        assert!(!flag(&q, "off") && !flag(&q, "alsoOff"));
        assert!(!flag(&q, "missing"), "an absent marker read as ON");
        assert!(!flag(&q, "empty"), "`?held=` read as ON");
        assert!(
            !flag(&q, "shouty"),
            "a spelling the routes do not document read as ON, so the set of ways to un-gate a \
             model call is larger than the set anybody wrote down"
        );
    }
}

/// Does the cockpit only ask for routes this server serves? (SKEIN-246)
///
/// This is the check that would have caught it, and the only one that would: `cockpit/src/change.mjs`
/// built `/api/pr/:repo/:n/shape`, the router registers `/api/repos/:id/review/:number/shape`, and
/// two tests asserted the wrong string — so the suite was green while clicking "change" on a pull
/// request in `/v2` got a 404, `answer.json()` threw on the HTML body, and the catch printed the
/// stand-in "the change could not be read".
///
/// **It only became that check when the bundle went into [`scanned`].** Until then the two HTML
/// files were compared against the router and the file that carried the broken URL was not, which is
/// the hole the two string assertions were sitting in. The URL is fixed and the bundle is scanned;
/// the assertions are gone, and what asserts the URL now is the router's own table.
///
/// It lives here because the route table lives here. A test beside the page can only assert the
/// string the page already has; a test beside the router can compare the two.
#[cfg(test)]
mod cockpit_routes {
    /// Every `/api/…` path the server registers, read from its own router entries.
    ///
    /// Read out of the source rather than out of a built `Router`, because axum's `Router` will not
    /// enumerate its paths — and a list maintained by hand beside the real one is the second
    /// opinion this whole check exists to prevent.
    ///
    /// A candidate carrying `{` or `$` is not a route and is dropped. This file's own tests quote
    /// URLs, and one of them quotes the URL that 404s: without this the table below would contain
    /// the broken path, the gate would find it served, and the check would pass by having read its
    /// own fixture as a router entry. It did, once.
    fn registered(server: &str) -> Vec<&str> {
        entries(server).into_iter().map(|(path, _)| path).collect()
    }

    /// The same entries, each with the HTTP methods it registers — `("/api/x", ["get", "post"])`.
    ///
    /// **Paths alone cannot see a handler whose callers all use another verb** (SKEIN-308).
    /// `POST /api/repos/:id/review/:number/critique` is the standalone drafter, a model call. The
    /// page fetches that exact path twice and both are bare `fetch(url)` — GET, answered by
    /// `api_critique_get`, a free disk read. A path-only scan sees a served path with a caller and
    /// says nothing, so the POST handler sat reachable only by typing the URL.
    ///
    /// Read by walking each router registration to its matching paren, rather than by finding the
    /// next quote: the router spells its longer entries across several lines, and the method sits
    /// below the path.
    ///
    /// **The token is assembled rather than written**, and that is not fussiness: `docs/parity.md`
    /// counts this binary's routes by grepping its source for the router's own call, so spelling it
    /// here — in code OR in a comment — adds to a number meant to count the router. It did, in the
    /// first draft of this function: three occurrences, and the count went 93 → 96.
    fn entries(server: &str) -> Vec<(&str, Vec<&str>)> {
        const VERBS: [&str; 5] = ["get", "post", "put", "delete", "patch"];
        const CALL: &str = concat!(".", "route", "(");
        let mut out = Vec::new();
        let mut at = 0usize;
        while let Some(found) = server[at..].find(CALL) {
            let open = at + found + CALL.len();
            // The whole call, by paren balance. `end` falls back to the end of the source, so a
            // call that never closes still advances the scan rather than spinning on itself.
            let mut depth = 1usize;
            let mut end = server.len();
            for (i, c) in server[open..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            at = end;
            let call = &server[open..end];
            // The path is the call's first string literal; the handlers are everything after it.
            let Some(quote) = call.find('"') else {
                continue;
            };
            let Some(shut) = call[quote + 1..].find('"') else {
                continue;
            };
            let path = &call[quote + 1..quote + 1 + shut];
            // A candidate carrying `{` or `$` is not a route and is dropped. This file's own tests
            // quote URLs, and one of them quotes the URL that 404s: without this the table would
            // contain the broken path, the gate would find it served, and the check would pass by
            // having read its own fixture as a router entry. It did, once.
            if !path.starts_with("/api/") || path.contains('{') || path.contains('$') {
                continue;
            }
            let handlers = &call[quote + 1 + shut..];
            let methods = VERBS
                .into_iter()
                .filter(|verb| {
                    handlers.match_indices(verb).any(|(i, _)| {
                        // A verb, not the tail of a handler's name: `get(api_critique_get)` holds
                        // the word twice and only one of them is the method.
                        handlers[i + verb.len()..].starts_with('(')
                            && !handlers[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_')
                    })
                })
                .collect();
            out.push((path, methods));
        }
        out
    }

    /// The verb a `fetch` of the URL just scanned will actually send.
    ///
    /// `after` is everything following the URL literal — the rest of its line, and the lines below
    /// it, because the options object is written under the call. Two narrowings keep this from
    /// reading somebody else's request: the window is bounded, and it stops at the next `fetch(`,
    /// so two calls close together cannot lend each other a method.
    ///
    /// **Absent means GET**, which is `fetch`'s own rule rather than a guess — so the failure mode
    /// of a missed `method:` is this reporting a POST as a GET. That direction costs a miss; the
    /// other would cost a false accusation, and a false accusation is what gets a gate deleted.
    fn method_at(after: &str) -> &'static str {
        let rest = after.trim_start_matches(['"', '\'', '`']);
        if rest.trim_start().starts_with(')') {
            return "GET";
        }
        // Bounded by CHARS, not bytes: these files are full of em dashes and arrows, and slicing
        // one in half panics.
        let mut window: String = rest.chars().take(400).collect();
        if let Some(next) = window.find("fetch(") {
            window.truncate(next);
        }
        for verb in ["POST", "PUT", "DELETE", "PATCH", "GET"] {
            if window.contains(&format!("method: \"{verb}\""))
                || window.contains(&format!("method:\"{verb}\""))
            {
                return verb;
            }
        }
        "GET"
    }

    /// `page` with every HTML comment blanked out — same lines, same line numbers, and the code
    /// that shares a line with a comment left standing.
    ///
    /// **What makes a span safe to blank at all is that the opener is anchored where the per-line
    /// filter anchored it**: a `<!--` opens a comment only when it begins its line (after leading
    /// whitespace), which is the exact predicate SKEIN-987 shipped. The swallow SKEIN-991 was left
    /// open for — `page.innerHTML = '<!-- ' + x`, a `<!--` inside a string putting the scanner in a
    /// comment it never leaves and silently dropping every request after it — cannot open one here,
    /// because that line begins with `page`. That direction is worse than the hole being closed: one
    /// excused route against a gate gone quiet over a whole file. So the anchor is what this is
    /// built on rather than something hoped about, and the test plants that exact line into the real
    /// pages and counts.
    ///
    /// **An opener with no `-->` after it anywhere blanks its own line and nothing else** — the
    /// per-line behaviour, exactly. Running to the end of the file is the one thing this must not
    /// do, for the same reason: an unclosed `<!--` is likelier to be this scanner failing to find
    /// the closer than a page with 6,000 commented-out lines. So no input makes this see LESS than
    /// the per-line filter saw, and none makes it blank past a `-->`.
    ///
    /// Blanked rather than deleted, because the gates report `index.html:1895` and a deleted line
    /// would move every number under it. Measured over [`scanned`]: 17 openers, all of them
    /// multi-line and all of them closed, changing 104 lines of 13,922.
    fn without_html_comments(page: &str) -> String {
        let lines: Vec<&str> = page.lines().collect();
        let mut out: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
        let mut i = 0;
        while i < lines.len() {
            let Some(open) = opens_html_comment(lines[i]) else {
                i += 1;
                continue;
            };
            // The first `-->` at or after the opener, on its own line or a later one.
            let closed = lines.iter().enumerate().skip(i).find_map(|(j, line)| {
                let from = if j == i { open + 4 } else { 0 };
                line[from..].find("-->").map(|k| (j, from + k + 3))
            });
            match closed {
                Some((j, end)) => {
                    for (m, text) in out.iter_mut().enumerate().take(j + 1).skip(i) {
                        let from = if m == i { open } else { 0 };
                        let to = if m == j { end } else { text.len() };
                        *text = blanked(text, from, to);
                    }
                    i = j + 1;
                }
                None => {
                    out[i] = blanked(&out[i], open, out[i].len());
                    i += 1;
                }
            }
        }
        out.join("\n")
    }

    /// Where a line's HTML comment opens, if it opens one at all.
    fn opens_html_comment(line: &str) -> Option<usize> {
        let t = line.trim_start();
        t.starts_with("<!--").then(|| line.len() - t.len())
    }

    /// `line` with `from..to` replaced by as many spaces as it held characters.
    fn blanked(line: &str, from: usize, to: usize) -> String {
        format!(
            "{}{}{}",
            &line[..from],
            " ".repeat(line[from..to].chars().count()),
            &line[to..]
        )
    }

    /// Every `/api/…` path a page can BUILD, as `(1-based line, path)` with `${…}` left standing.
    ///
    /// Two narrowings, both to keep this from reporting prose as a request. The `/api/` must open a
    /// string literal — a quote or a backtick immediately before it — and a line that starts a
    /// comment is skipped, because these files discuss routes in comments as often as they call
    /// them (`index.html` mentions `/api/repos/undefined/…` in a note about a bug that is fixed).
    ///
    /// **`<!--` is in that list, and leaving it out cost the gate both ways** (SKEIN-987). Two of
    /// the three scanned files are HTML and their comments open `<!--`, which is none of the three
    /// Rust/JS forms: `src/web/index.html:1905` says the usage pane is "Rendered from
    /// `/api/usage`", in backticks, and that sentence read as a request to that route. The visible
    /// direction is a false accusation — with the route deleted, the missing-path gate reported
    /// three asks where the page makes two, and named a line nobody fetches from. The costly
    /// direction is the other one: `every_method_this_router_registers_has_a_caller_or_a_declared_reason`
    /// counts asks through here too, so **a prose mention was enough to make a route nothing calls
    /// read as called** — the defect SKEIN-308 wrote that gate for, inverted.
    ///
    /// **`<!--` has since left that per-line list and become a SPAN** (SKEIN-991), because the
    /// per-line form saw the line a comment opens on and not the interior of one that runs on:
    /// `src/web/index.html:1895` named `/api/fleet/plan` in backticks, three lines inside a comment
    /// that opened at 1880, and counted as a request to it. [`without_html_comments`] blanks the
    /// whole span instead, and carries the argument about the direction that would be worse.
    /// Measured across [`scanned`]: 100 asks before, 97 after — the three it drops are prose
    /// (`index.html:1881` `/api/health`, `:1895` `/api/fleet/plan`, `:1931` `/api/update`), and all
    /// three routes are fetched for real elsewhere in the same page, so nothing became uncalled.
    ///
    /// The limit that is left, since it is the kind that reads as covered: `//`, `*` and `/*` are
    /// still per LINE. A JavaScript block comment whose interior lines do not begin with `*` is
    /// still read as code. `cockpit.js` indents every one of its interiors with `*` and
    /// `index.html` has three `*` lines in all, so there is nothing in the tree that costs today.
    fn asked_for(page: &str) -> Vec<(usize, String, &'static str)> {
        let page = without_html_comments(page);
        let lines: Vec<&str> = page.lines().collect();
        let mut out = Vec::new();
        for (n, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with('*') || t.starts_with("/*") {
                continue;
            }
            let b = line.as_bytes();
            for i in 0..b.len() {
                if !matches!(b[i], b'"' | b'\'' | b'`') {
                    continue;
                }
                if line[i + 1..].starts_with("/api/") {
                    let (path, used) = url_at(&line[i + 1..]);
                    // Everything after the URL: the rest of its line, then the lines the options
                    // object is written on. Twelve is comfortably past the longest here.
                    let after = std::iter::once(&line[i + 1 + used..])
                        .chain(lines[n + 1..].iter().take(12).copied())
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push((n + 1, path, method_at(&after)));
                }
            }
        }
        out
    }

    /// One URL literal, from its first character to whatever ends it.
    ///
    /// `${…}` is copied through as a placeholder — with brace counting, or
    /// `${encodeURIComponent(name)}` ends the path at its own closing paren and every box route in
    /// the cockpit reads as unserved.
    fn url_at(rest: &str) -> (String, usize) {
        let b = rest.as_bytes();
        let mut out = String::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'$' && b.get(i + 1) == Some(&b'{') {
                let mut depth = 0usize;
                let mut j = i + 1;
                while j < b.len() {
                    match b[j] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                out.push_str("${}");
                i = j + 1;
                continue;
            }
            // A query string, a fragment, the closing quote, or anything that cannot be in a path.
            if !b[i].is_ascii()
                || matches!(
                    b[i],
                    b'"' | b'\'' | b'`' | b'?' | b'#' | b' ' | b'\t' | b',' | b')'
                )
            {
                break;
            }
            out.push(b[i] as char);
            i += 1;
        }
        // The offset it stopped at, so the caller can read what follows the URL — which is where
        // `method:` lives, and the whole of what SKEIN-308's gate needs.
        (out, i)
    }

    /// Would this route answer that ask?
    ///
    /// Segment by segment. A `:param` in the route matches anything; a `${…}` in the ask matches a
    /// `:param`, and also matches a literal — `/api/boxes/${name}/${path}` is how the settings panel
    /// posts five different box routes, and it genuinely can be any of them.
    fn serves(route: &str, asked: &str) -> bool {
        let r: Vec<&str> = route.split('/').skip(1).collect();
        let a: Vec<&str> = asked.split('/').skip(1).collect();
        if r.len() != a.len() {
            return false;
        }
        r.iter()
            .zip(a.iter())
            .all(|(rs, as_)| match as_.find("${") {
                Some(0) => true,
                Some(k) => rs.starts_with(':') || *rs == &as_[..k],
                None => rs.starts_with(':') || rs == as_,
            })
    }

    #[test]
    fn the_cockpit_never_asks_for_a_route_this_server_does_not_serve() {
        let routes = registered(super::server_source());
        assert!(
            routes.len() > 60,
            "the route scan found {} routes — it stopped reading the router, so what follows \
             proves nothing",
            routes.len()
        );
        let mut asks = 0;
        let mut missing = Vec::new();
        for (name, page) in scanned() {
            for (line, path, _) in asked_for(page) {
                asks += 1;
                if !routes.iter().any(|r| serves(r, &path)) {
                    missing.push(format!("{name}:{line} asks {path}"));
                }
            }
        }
        assert!(
            asks > 80,
            "the page scan found {asks} requests — it stopped reading the pages"
        );
        assert!(
            missing.is_empty(),
            "the cockpit asks for {} path(s) no route answers, so each is a 404 the page reports \
             as its own generic failure:\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// Everything the browser runs, all of it scanned by both gates below.
    ///
    /// **The bundle is in this list, and that inclusion is the whole of SKEIN-246's fix.** Each page
    /// is one classic script plus `cockpit/src`, which `cockpit/build.mjs` concatenates into
    /// `src/web/vendor/cockpit.js` — so a URL built in a `cockpit/src` module is a URL the browser
    /// asks for, and one of them (`change.mjs`'s `shapeUrl`) asked `/api/pr/:repo/:n/shape`, which no
    /// router has ever registered, from the day the view shipped (`c5ddc46`, 2026-08-21) until this
    /// change. It survived because this list named only the two HTML files, and two tests asserted
    /// the broken string rather than the route table. One list, used by both gates, so the bundle
    /// cannot fall out of one of them.
    fn scanned() -> [(&'static str, &'static str); 3] {
        [
            ("src/web/index.html", include_str!("../../web/index.html")),
            ("src/web/v2.html", include_str!("../../web/v2.html")),
            (
                "src/web/vendor/cockpit.js",
                include_str!("../../web/vendor/cockpit.js"),
            ),
        ]
    }

    /// Every `/api/…` request the pages make — `(file, line, path, the verb it will send)`.
    fn every_ask() -> Vec<(&'static str, usize, String, &'static str)> {
        scanned()
            .into_iter()
            .flat_map(|(name, page)| {
                asked_for(page)
                    .into_iter()
                    .map(move |(line, path, method)| (name, line, path, method))
            })
            .collect()
    }

    /// Every `(path, method)` in `routes` that nothing in `asks` requests.
    ///
    /// Lifted out of `every_method_this_router_registers_has_a_caller_or_a_declared_reason` so a
    /// test can drive the gate's own composition over fixtures instead of a second copy of it
    /// (SKEIN-987). The thing that went wrong there was not in either scanner on its own — it was
    /// what this function does with them, and a test asserting on `asked_for` alone would not have
    /// seen a route go from unasked to asked.
    fn unasked<'a>(
        routes: &'a [(&'a str, Vec<&'a str>)],
        asks: &[(&str, usize, String, &'static str)],
    ) -> Vec<(&'a str, &'a str)> {
        routes
            .iter()
            .flat_map(|(path, methods)| methods.iter().map(move |m| (*path, *m)))
            .filter(|(path, method)| {
                !asks.iter().any(|(_, _, ask, asked_method)| {
                    asked_method.eq_ignore_ascii_case(method) && serves(path, ask)
                })
            })
            .collect()
    }

    /// **The page must not ask a verb this router does not register on that path.**
    ///
    /// The sibling of `the_cockpit_never_asks_for_a_route_this_server_does_not_serve`, one level
    /// finer. A path both sides agree on still 405s if the page POSTs where the router only took a
    /// GET, and axum answers that with a bare `Method Not Allowed` the page reports as its own
    /// generic failure — exactly the shape of SKEIN-246, which cost a working tab and a green suite.
    #[test]
    fn the_cockpit_never_asks_a_method_this_server_does_not_register() {
        let routes = entries(super::server_source());
        assert!(
            routes.len() > 60,
            "the route scan found {} entries — it stopped reading the router",
            routes.len()
        );
        let mut wrong = Vec::new();
        for (name, line, path, method) in every_ask() {
            // **A literal ending in `/` is half a URL, and this declines to judge it.**
            // `revokeGitq` builds its path by concatenation —
            // `"/api/fleet/git-grants/" + encodeURIComponent(box) + "/" + …` — so the scanner sees
            // the prefix and cannot know two segments follow. The path-only gate passes it by
            // accident, because a `:param` route matches the empty last segment; judging its
            // METHOD against that route would be a false accusation, and a false accusation is
            // what gets a gate deleted rather than fixed.
            if path.ends_with('/') {
                continue;
            }
            // Only where the PATH is served: a path nothing serves is the sibling test's failure,
            // and reporting it twice makes one bug look like two.
            if !routes.iter().any(|(route, _)| serves(route, &path)) {
                continue;
            }
            let served = routes
                .iter()
                .filter(|(route, _)| serves(route, &path))
                .any(|(_, methods)| methods.iter().any(|m| m.eq_ignore_ascii_case(method)));
            if !served {
                wrong.push(format!("{name}:{line} sends {method} to {path}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "the cockpit sends {} request(s) with a method the matching route does not register, \
             so each is a 405 the page reports as its own generic failure:\n  {}",
            wrong.len(),
            wrong.join("\n  ")
        );
    }

    /// **A handler nobody calls says who is expected to call it** (SKEIN-308).
    ///
    /// The bug this exists for: `POST /api/repos/:id/review/:number/critique` is the standalone
    /// drafter and costs a model call. The page fetches that exact path twice and both are bare
    /// `fetch(url)` — GET, answered by the free disk read beside it — because the page function
    /// that was its one POST caller was deleted in `b1d2b3a`, when the summary and the review
    /// became one visit (SKEIN-263). A path-only scan sees a served path with a caller and says
    /// nothing.
    ///
    /// So: every registered `(path, method)` the pages never ask for is listed HERE, with why.
    /// Exact both ways — a new unasked surface has to be declared, and a declaration that stops
    /// being true has to be deleted, which is the half that keeps the list from becoming folklore.
    ///
    /// **This list is not a to-do.** Several of these are right: an `EventSource`, a WebSocket
    /// upgrade and an `<a href>` are not `fetch` calls and never will be. What the list buys is
    /// that each one had to be written down by somebody who knew which kind it was.
    ///
    /// A URL the page builds by CONCATENATION reads here as its literal prefix, so it counts as a
    /// caller for any route that prefix matches and its longer siblings look unasked. That is the
    /// safe direction — a spurious line in this list costs one sentence, where the other way round
    /// is a gate accusing working code — and the sibling test declines to judge those asks at all.
    #[test]
    fn every_method_this_router_registers_has_a_caller_or_a_declared_reason() {
        // (path, method, why it has no `fetch` in the pages)
        let declared: &[(&str, &str, &str)] = &[
            (
                "/api/repos/:id/review/:number/triggers",
                "POST",
                "§10's per-pull-request trigger override, deliberately with no page caller yet. \
                 The MECHANISM is the part that was missing — the repo's set was the only one a \
                 pull request could be governed by — and what it should look like on a row is the \
                 owner's call rather than this build's: a picker over the trigger words is a \
                 surface, and skein has a rule about inventing those. Reachable by API and by the \
                 workflow file until then",
            ),
            (
                "/api/repos/:id/review",
                "GET",
                "no caller at all; the pane opens on the merged /api/review. SKEIN-327 is the \
                 owner's keep-or-delete call, and SKEIN-252 no longer depends on the answer",
            ),
            (
                "/api/machine/doorstep",
                "GET",
                "asked by `skein doctor` and by the host, not by a page",
            ),
            (
                "/api/machine/pressure",
                "GET",
                "asked by `skein doctor` and by the host, not by a page",
            ),
            (
                "/api/fleet/git-grants/:name/:repo",
                "DELETE",
                "`revokeGitq` DOES call it. The scanner cannot see that: the page builds this one \
                 by concatenation — `\"/api/fleet/git-grants/\" + encodeURIComponent(box) + \"/\" \
                 + …` — so the literal is a prefix with no idea two segments follow. A miss, not \
                 an unasked handler",
            ),
            (
                "/api/acts/:id/stream",
                "GET",
                "an EventSource, not a fetch — this scanner reads fetch calls",
            ),
            (
                "/api/away",
                "GET",
                "the away digest is opened as a page, not fetched",
            ),
            (
                "/api/away/seen",
                "POST",
                "posted by the away digest's own inline script, which is served from src/ and not \
                 scanned here",
            ),
            (
                "/api/boxes/:name/terminal",
                "GET",
                "a WebSocket upgrade — xterm opens it, no fetch is involved",
            ),
            (
                "/api/fleet/resize",
                "POST",
                "nothing asks it and nothing may: a resize is a destroy followed by a create, and \
                 skein is inside the sandbox it would destroy (architecture §7.5, SKEIN-467). The \
                 button that used to post here is gone from the page and does not come back. The \
                 route stays because the post can still ARRIVE — a tab left open on an older \
                 build, a script, somebody's curl — and what should meet it is the refusal with \
                 the host lines to run, not a 404 that reads as a broken server",
            ),
            (
                "/api/fleet/create",
                "POST",
                "the create-fleet dialog was its only caller and the dialog is deleted \
                 (SKEIN-627): it opened on `exists === false`, and an in-fleet skein answers \
                 `Some(true)` about the fleet it is standing in and `None` about every other name, \
                 so that state could not arise. What went is the SIZING SURFACE, not the route — \
                 creating a differently-named sandbox is still coherent, the warden is still on \
                 the host with the capability, and `fleet::request_fleet_create` still carries the \
                 attempt lease that stops two presses becoming two fleets. The owner's decision on \
                 SKEIN-627 declined that reading without refuting it, so this stays rather than \
                 being rebuilt the day something wants it",
            ),
        ];

        let routes = entries(super::server_source());
        let asks = every_ask();
        let uncalled = unasked(&routes, &asks);

        let undeclared: Vec<String> = uncalled
            .iter()
            .filter(|(path, method)| {
                !declared
                    .iter()
                    .any(|(p, m, _)| p == path && m.eq_ignore_ascii_case(method))
            })
            .map(|(path, method)| format!("{} {path}", method.to_uppercase()))
            .collect();
        assert!(
            undeclared.is_empty(),
            "this router registers {} handler(s) no page asks for, and nothing says why:\n  {}\n\n             Either give it a caller, or add it to the list above with the reason. A handler \
             reachable only by typing its URL is not dead code in the compiler's sense, and no \
             failure will ever mention it.",
            undeclared.len(),
            undeclared.join("\n  ")
        );

        let outlived: Vec<String> = declared
            .iter()
            .filter(|(path, method, _)| {
                !uncalled
                    .iter()
                    .any(|(p, m)| p == path && method.eq_ignore_ascii_case(m))
            })
            .map(|(path, method, _)| format!("{method} {path}"))
            .collect();
        assert!(
            outlived.is_empty(),
            "these are declared as having no caller, and the pages call them now:\n  {}\n\n\
             Delete the lines. The declaration did its job.",
            outlived.join("\n  ")
        );
    }

    /// The method scan, held to the same standard as the path scan beside it: it has to see the
    /// shape the bug came in, and it must not read one request's options as another's.
    #[test]
    fn the_method_scan_tells_a_posting_fetch_from_a_plain_one() {
        // SKEIN-308 itself: two bare fetches of the drafter's path.
        let plain = "  fetch(`/api/repos/${id}/review/${n}/critique`)\n    .then(r => r.json());";
        assert_eq!(asked_for(plain)[0].2, "GET");

        let posting = "  fetch(`/api/repos/${id}/review/${n}/act`, {\n                       \x20   method: \"POST\",\n    body: JSON.stringify(x),\n  });";
        assert_eq!(asked_for(posting)[0].2, "POST");
        // Both spellings the pages actually use.
        assert_eq!(
            asked_for("  fetch(`/api/x`, {method:\"DELETE\"})")[0].2,
            "DELETE"
        );

        // A request must not borrow the POST written under it. This is the false accusation the
        // window exists to prevent, and it is the one that would get this gate deleted.
        //
        // The fixture carries a second ARGUMENT with no method in it — `{ headers: h }` — on
        // purpose. The shape with no argument at all is answered by the early `)` check and never
        // reaches the window, so a neighbour test written THAT way passes whether the guard is
        // there or not; this one fails the moment it goes.
        let neighbours = "  fetch(`/api/a`, { headers: h })\n    .then(r => r.json());\n  fetch(`/api/b`, { method: \"POST\" });";
        let seen = asked_for(neighbours);
        assert_eq!(seen[0].1, "/api/a");
        assert_eq!(
            seen[0].2, "GET",
            "a request with no method of its own borrowed the one written below it"
        );
        assert_eq!(seen[1].2, "POST");
        // And the no-argument shape still reads as a GET, which is the SKEIN-308 case itself.
        assert_eq!(asked_for("  fetch(`/api/c`);")[0].2, "GET");

        // And the router side: one entry, both verbs, read across the lines it is written on.
        let router = format!(
            " .{}(\n \"/api/repos/:id/review/:number/critique\",\n {}(api_critique_get).{}(api_critique_draft),\n ) ",
            "route", "get", "post"
        );
        assert_eq!(
            entries(&router),
            vec![(
                "/api/repos/:id/review/:number/critique",
                vec!["get", "post"]
            )],
            "the router scan cannot see a method written under its path"
        );
    }

    /// The gate above is worth nothing if it cannot see the shape the bug came in, or cannot say
    /// where — the same discipline `tests/page_scripts.rs` holds its two scanners to.
    ///
    /// Written against the real one: `/api/pr/:repo/:n/shape` against a router that registers
    /// `/api/repos/:id/review/:number/shape`. A matcher that treated every `${…}` as a wildcard
    /// across segment boundaries would call these equal and pass the whole suite while the tab 404s.
    #[test]
    fn the_route_scan_reports_the_path_that_404d_and_names_its_line() {
        // Assembled rather than written out, and the reason is a gate: `docs/parity.md` counts
        // this binary's routes by grepping its source for the router's own call, so a fixture
        // that spells that token — or a comment that quotes it, as this one first did — adds to a
        // number meant to count the router. See `tests/parity_numbers.rs` for the command.
        let router = format!(
            " .{}(\"/api/repos/:id/review/:number/shape\", get(api_pr_shape)) ",
            "route"
        );
        let router = router.as_str();
        let routes = registered(router);
        assert_eq!(routes, vec!["/api/repos/:id/review/:number/shape"]);

        let wrong =
            "  return `/api/pr/${encodeURIComponent(r.repo)}/${encodeURIComponent(n)}/shape`;";
        let found = asked_for(wrong);
        assert_eq!(found.len(), 1, "the scanner did not see the request at all");
        assert_eq!(found[0].1, "/api/pr/${}/${}/shape");
        assert!(
            !routes.iter().any(|r| serves(r, &found[0].1)),
            "the matcher accepted the URL that 404s"
        );

        let right = "  return `/api/repos/${encodeURIComponent(r.repo)}/review/${n}/shape`;";
        let found = asked_for(right);
        assert!(
            routes.iter().any(|r| serves(r, &found[0].1)),
            "the matcher rejected the URL that works, which would make this gate the thing people \
             delete"
        );

        // Prose is not a request: these files name routes in comments more often than they call
        // them, and a scanner that read those would report the bugs they describe as live.
        assert!(asked_for("  // used to send the pump to `/api/repos/undefined/x`").is_empty());
    }

    /// **A route named only in an HTML comment has no caller, and the gate has to say so**
    /// (SKEIN-987).
    ///
    /// Two of the three scanned files are HTML. `asked_for` skipped `//`, `*` and `/*` and not
    /// `<!--`, so a sentence in `src/web/index.html` explaining where a pane's data comes from —
    /// with the path in backticks, which is one of the three quotes this scanner looks for —
    /// counted as a request to that route.
    ///
    /// The false accusation is the half you can see: the missing-path gate names a line nobody
    /// fetches from. This asserts the half you cannot, and it is the worse one. A handler nothing
    /// calls is what `every_method_this_router_registers_has_a_caller_or_a_declared_reason` exists
    /// to name (SKEIN-308), and a comment mentioning its path was enough to make it read as called
    /// — so that gate went green over exactly the thing it was written to catch. Measured on
    /// master before the fix, with a route planted beside `/api/usage` and one comment line added
    /// to `index.html`: all five tests in this module passed.
    ///
    /// Driven through `unasked`, the gate's own composition, rather than through `asked_for`
    /// alone: neither scanner was wrong by itself, and an assertion on the scanner would not have
    /// watched a route go from unasked to asked.
    #[test]
    fn a_route_named_only_in_an_html_comment_still_has_no_caller() {
        // Assembled, like every other router fixture here: `docs/parity.md` counts this binary's
        // routes by grepping its source for the router's own call.
        let router = format!(" .{}(\"/api/only-in-a-comment\", {}(h)) ", "route", "get");
        let routes = entries(&router);
        assert_eq!(
            routes,
            vec![("/api/only-in-a-comment", vec!["get"])],
            "the fixture router did not parse, so what follows proves nothing"
        );

        // The shape `src/web/index.html:1905` is in: prose, the path in backticks, no fetch.
        let page = "          <!-- The pane is rendered from `/api/only-in-a-comment` rather than \
                    written here. -->";
        assert!(
            asked_for(page).is_empty(),
            "an HTML comment that only NAMES a route was read as a request to it: {:?}",
            asked_for(page)
        );

        let asks: Vec<(&str, usize, String, &'static str)> = asked_for(page)
            .into_iter()
            .map(|(line, path, method)| ("fixture.html", line, path, method))
            .collect();
        assert_eq!(
            unasked(&routes, &asks),
            vec![("/api/only-in-a-comment", "get")],
            "a route nothing calls read as called, because a comment names it — the defect \
             SKEIN-308's gate exists to catch, inverted: the gate goes green over a handler \
             reachable only by typing its URL"
        );
    }

    /// **And the same for a route named on a comment's INTERIOR line** (SKEIN-991).
    ///
    /// The half SKEIN-987 left: its filter was per line, so it saw the line a comment opens on and
    /// not the lines under it. `src/web/index.html:1895` names `/api/fleet/plan` in backticks
    /// inside a comment that opened at 1880, and it counted as a request — so the sentence above
    /// about a handler reachable only by typing its URL was still true, one line further down.
    ///
    /// The four shapes are asserted together because three of them are the reason this was left
    /// alone rather than fixed, and a fix that closed the first and opened any of the others would
    /// be worse than what it replaced: code after a comment that closes on its own line, code under
    /// a comment that is never closed, and code under a `<!--` that is inside a string. The last is
    /// asserted twice — once on a fixture, and once by planting that line into the real pages and
    /// counting, because the fixture is small enough to have no `-->` after it and the danger only
    /// exists where there is one.
    #[test]
    fn a_route_named_inside_a_multi_line_html_comment_still_has_no_caller() {
        // Assembled, like every other router fixture here: `docs/parity.md` counts this binary's
        // routes by grepping its source for the router's own call.
        let router = format!(
            " .{}(\"/api/only-in-a-comments-interior\", {}(h)) ",
            "route", "get"
        );
        let routes = entries(&router);
        assert_eq!(
            routes,
            vec![("/api/only-in-a-comments-interior", vec!["get"])],
            "the fixture router did not parse, so what follows proves nothing"
        );

        // The shape `src/web/index.html:1895` is in: the route named in backticks, two lines under
        // the `<!--` that opened the comment.
        let page = [
            "        <!-- Where the fleet pane's two sentences come from:",
            "             the line to run on the host is `/api/only-in-a-comments-interior`,",
            "             rendered verbatim rather than rebuilt here. -->",
        ]
        .join("\n");
        let page = page.as_str();
        assert!(
            asked_for(page).is_empty(),
            "a route named on a line INSIDE an HTML comment was read as a request to it: {:?}",
            asked_for(page)
        );
        let asks: Vec<(&str, usize, String, &'static str)> = asked_for(page)
            .into_iter()
            .map(|(line, path, method)| ("fixture.html", line, path, method))
            .collect();
        assert_eq!(
            unasked(&routes, &asks),
            vec![("/api/only-in-a-comments-interior", "get")],
            "a route nothing calls read as called, because a comment's interior line names it — \
             SKEIN-987's defect one line further down"
        );

        // And the three directions that must NOT change, each with what it costs if it does.
        let seen = |page: &str| -> Vec<String> {
            asked_for(page).into_iter().map(|(_, p, _)| p).collect()
        };
        assert_eq!(
            seen("  <!-- the old pane is gone --> fetch(\"/api/after-a-closed-comment\");"),
            vec!["/api/after-a-closed-comment"],
            "a comment that opens and closes on one line hid the code written after it, which the \
             per-line filter this replaces did too — the whole line went"
        );
        assert_eq!(
            seen("  <!-- this comment is never closed\n  fetch(\"/api/after-an-unclosed-comment\");"),
            vec!["/api/after-an-unclosed-comment"],
            "an unclosed `<!--` swallowed the rest of the page: every request after it stops being \
             counted, this gate goes green over anything, and \
             every_method_this_router_registers_has_a_caller_or_a_declared_reason accuses routes \
             the page does call"
        );
        assert_eq!(
            seen("  page.innerHTML = '<!-- ' + x;\n  fetch(\"/api/after-a-string\");"),
            vec!["/api/after-a-string"],
            "a `<!--` inside a JavaScript string opened a comment — the swallow above, reachable \
             from a line of ordinary code"
        );

        // The same string, planted into the real pages, and measured on the MASK rather than on
        // the request count — which is not a preference.
        //
        // **A stray opener cannot cost this gate a request on today's pages, so a count would
        // prove nothing.** Measured: index.html's 16 closers all sit between 1648 and 1956 and its
        // first request is at 2109; v2.html's one closer is at 144 and its first request at 239;
        // cockpit.js holds no `-->` at all. There is no position in any of the three where a
        // comment a string opened could reach a `fetch`. Written as a request count this passed
        // against a scanner that treated `<!--` anywhere on a line as an opener, which is the
        // exact defect it is here for. So the property itself is asserted, one level down: a
        // `<!--` inside a string must not blank a line that was not already blank. 104 lines
        // blanked across the three pages — index.html 97, v2.html 7, cockpit.js 0 — where under
        // that scanner index.html alone goes to 1,721.
        let blanked_lines = |text: &str| -> usize {
            let masked = without_html_comments(text);
            text.lines()
                .zip(masked.lines())
                .filter(|(raw, cooked)| raw != cooked)
                .count()
        };
        let mut plantable = 0;
        for (name, page) in scanned() {
            // `cockpit.js` holds no `-->` at all, so nothing in it can be swallowed.
            if !page.contains("-->") {
                continue;
            }
            plantable += 1;
            assert_eq!(
                blanked_lines(&format!("  page.innerHTML = '<!-- ' + x;\n{page}")),
                blanked_lines(page),
                "{name}: one `<!--` inside a string blanked lines that are not comments, and every \
                 request among them stops being counted"
            );
        }
        assert!(
            plantable > 0,
            "no page holds a `-->` at all, so there is nowhere a stray opener could run to and the \
             counts above would be equal whatever this scanner did"
        );
    }
}
