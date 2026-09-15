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
// Vendored, not CDN-loaded: the terminal must work in the firewalled sbx network the tool lives in.
// They are no longer four constants and four handlers — `skein::assets` generates the table from the
// directory, because a build step emits files whose names carry content hashes and neither the count
// nor the names are known here. The four URLs are unchanged; only the code behind them is.
const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// How many embedded terminals may be open at once.
///
/// Named rather than spelled inside [`PTY_LIMIT`] because the refusal quotes it. A person told
/// "too many terminals open" and not told how many is being asked to guess at the rule they just
/// hit, and a number written twice is a number that stops agreeing with itself.
const PTY_MAX: usize = 24;

/// Cap concurrent embedded terminals so a flood of WS connections can't exhaust PTYs / file
/// descriptors on the host. Each live terminal holds one permit for its whole session.
static PTY_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(PTY_MAX);

/// One terminal's slot, held for the session — and **it says so when it is given back**.
///
/// A bare `SemaphorePermit` releases silently, which was fine while the only thing that could
/// happen next was somebody pressing a button. It is not fine now: a pane refused for the cap is
/// told to close another terminal, and it then watches the board for the slot rather than waiting
/// to be clicked (SKEIN-702). Only the release knows when that is, so the release is what publishes
/// it.
///
/// **`Option`, and taken before the announcement, because the order is the whole correctness of
/// this.** A field is dropped *after* the enclosing `Drop::drop` returns, so announcing first would
/// announce a slot that is not yet free — and the pane it wakes would race the very release that
/// woke it, be refused again, and go back to waiting for a permit that was already gone. `take()`
/// returns the permit to the semaphore inside this line; the send is on the line after it.
struct PtySlot(Option<tokio::sync::SemaphorePermit<'static>>);

impl Drop for PtySlot {
    fn drop(&mut self) {
        drop(self.0.take());
        skein::stream::pty_freed();
    }
}

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
    // **This is a real skein, whatever `$SKEIN_TEST` says.** `tests/server.rs` and
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
    // starts it at all. The relay above is a few file reads on a five-second timer and needs none
    // of that care, which is why the two do not look the same.
    tokio::spawn(skein::announce::watch_fleet_disk());
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
        // The press behind the bar's "update" (SKEIN-405). Its own route rather than a flag on
        // something else: this installs software into the sandbox every box shares, which is not a
        // thing to reach by accident.
        .route("/api/update-agents", post(api_update_agents))
        // Settings -> Update. Three routes because they are three different costs: a reading that
        // must never block, a press that starts minutes of work, and a log a page tails across the
        // restart that press causes.
        .route("/api/update", get(api_update))
        .route("/api/update/start", post(api_update_start))
        .route("/api/update/log", get(api_update_log))
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
            // `expose()` — see the note on `apiauth::token`. The URL is the delivery channel.
            Ok(t) => println!("skein-server → http://{addr}/?t={}", t.expose()),
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

/// What the cockpit sends on every answer. See the layer's own note for what the CSP does and does
/// not close today.
///
/// `img-src` admits `data:` because the page draws inline SVG icons that way, and `blob:` because
/// the terminal and the attachment previews create object URLs. `connect-src` admits `ws:`/`wss:`
/// for the terminal WebSocket, which is same-origin but a different scheme.
const CSP: &str = "default-src 'self'; \
                   script-src 'self' 'unsafe-inline'; \
                   style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data: blob:; \
                   font-src 'self' data:; \
                   connect-src 'self' ws: wss:; \
                   object-src 'none'; \
                   base-uri 'self'; \
                   form-action 'self'; \
                   frame-ancestors 'none'";

async fn security_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        ("content-security-policy", CSP),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
    ] {
        // Set rather than appended, and only when the handler did not say otherwise — a route that
        // needs its own policy stays in charge of it.
        if !headers.contains_key(name) {
            if let (Ok(name), Ok(value)) = (
                axum::http::HeaderName::from_bytes(name.as_bytes()),
                axum::http::HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
    }
    response
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
    // `apiauth::same`, not `==`. This is the one place the fleet's token is compared with a plain
    // string equality, and it is the place that mints the browser session — every other comparison
    // already goes through the constant-time helper written for exactly this.
    if !offered.is_empty() && skein::apiauth::matches(offered) {
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
    let boxed = name.clone();
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
        Ok(look) => {
            keep_a_launch_that_never_ran(id.clone(), boxed);
            (StatusCode::ACCEPTED, Json(look)).into_response()
        }
        // 409, because the thing that stops a second create is that one is already running — which
        // is a conflict rather than a bad request, and the message says which act to watch.
        Err(why) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": why })),
        )
            .into_response(),
    }
}

/// Follow a create act to its end, and keep the reason when its command never ran.
///
/// **The transcript is not the record.** An act holds what the command said for `act::RETENTION`
/// and then forgets it, and it is only ever in this server's memory — so a create that died because
/// `skein` could not be executed leaves nothing behind, and the next surface to ask about the box
/// reads `starts/<box>.err`, finds nothing, and says no start was attempted (SKEIN-589). The
/// terminal route keeps the same reason the same way; two launch surfaces that answer differently
/// about the same failure is what one of these is for.
fn keep_a_launch_that_never_ran(id: String, name: String) {
    tokio::spawn(async move {
        let Some((_, mut rest)) = skein::act::watch(&id) else {
            return;
        };
        // Drained to the end rather than polled: the empty chunk is the act's own "there will be no
        // more", and a closed channel is the only other thing that cannot be mistaken for a slow act.
        loop {
            match rest.recv().await {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
        let Some(look) = skein::act::look(&id) else {
            return;
        };
        // `-1` is act's word for "killed by a signal", which is not an exit code and not this
        // failure; `u32::try_from` is what refuses it rather than a second check that could disagree.
        let skein::act::State::Ended { code } = look.state else {
            return;
        };
        let Ok(code) = u32::try_from(code) else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || {
            skein::sandbox::remember_launch_never_ran(&name, code)
        })
        .await;
    });
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
/// carry all of them, so the settings pane saves a card, not a keystroke.
#[derive(Deserialize)]
struct RepoSettingsReq {
    /// a Plane project URL or bare uuid — what this repo's tracker tokens bind to
    plane_project: Option<String>,
    /// which work-tracking connection this repo claims through, by id; empty = not tracked
    sync_connection: Option<String>,
    /// whether this repo has a review queue the badge may poll
    review_queue: Option<bool>,
    /// may the reviewer engine act on this repo at all — `docs/pr-review.md` §10, layer 3
    auto_review: Option<bool>,
    /// how far it may go unattended: none | comment | changes | approve
    auto_review_ceiling: Option<String>,
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
        skein::repos::ReviewerSettings {
            auto_review: req.auto_review,
            ceiling: req.auto_review_ceiling,
        },
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
    /// The repo the caller believes this note is about — which is not necessarily the repo in the
    /// URL, and that is the entire point. See [`note_is_for_this_repo`].
    #[serde(default)]
    repo: String,
}

/// **Is this note about the repo it is being stored against?** (SKEIN-427)
///
/// `moduledocs::write` already refuses a path that is not one of the repo's modules, and that check
/// cannot see this: `src`, `docs` and `tests` are modules of half the repos in a fleet, so repo A's
/// `src` posted to repo B is a *valid* write of B's `src` — a minute of model time spent
/// overwriting a note nobody asked about, while the note the reader meant to refresh stays stale.
/// The cockpit's own defect was exactly that shape (the notes panel kept another repo's rows after
/// the repo filter moved), and a request carrying only a path gives this handler nothing to notice
/// it with.
///
/// So the note states the repo it is about and the two are compared. An empty claim is accepted:
/// it is not a wrong one, and a caller that says nothing about provenance — `curl`, a script — is
/// not the failure this exists for. What it refuses is a caller that names one repo and writes to
/// another, which is only ever a caller that has lost track of which repo it is showing.
fn note_is_for_this_repo(id: &str, claimed: &str) -> Result<(), String> {
    if claimed.is_empty() || claimed == id {
        return Ok(());
    }
    Err(format!(
        "that note is about {claimed} and this is {id} — nothing was written"
    ))
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
    // Asked of the REQUEST, before anything is looked up: a request that names two repos is
    // refused as that, and not as whatever the wrong one happens to make of the path.
    if let Err(why) = note_is_for_this_repo(&id, &req.repo) {
        return Json(serde_json::json!({ "ok": false, "error": why }));
    }
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

/// **One answer to "are summaries on?", in a payload that carries the question twice** (SKEIN-299).
///
/// `Queue::ai` is filled from `review::summaries_enabled()` when the queue is REFRESHED, and then
/// travels with the queue into the sixty-second micro-cache and onto disk. `MergedQueue::ai` is
/// computed when the payload is assembled, and that is the one the pane reads
/// (`src/web/index.html`: `ai: m.ai`, then `revQueue.ai`). Both serialise as `ai`. So toggling the
/// switch and then being served from cache or from `prq::remembered` put the same fact in one
/// response twice with different answers — and the stale one was stale by construction, not by
/// accident: nothing about a cached queue ever revisits it.
///
/// **The switch is a live fact about this machine, not a property of the queue that was fetched.**
/// So it is answered at the moment of serving, on every path a `Queue` leaves this binary by.
///
/// Written here rather than by deleting `Queue::ai` because removing a serialised field is `prq`'s
/// call, not this file's — and the payload has to stop contradicting itself either way. The day the
/// field goes, this function goes with it.
///
/// `on` is passed in rather than read here so the merged payload cannot disagree with ITSELF: its
/// queues are stamped with the very value its own `ai` carries, not with a second reading of the
/// switch taken a moment later.
fn settle_switch(queues: &mut [skein::prq::Queue], on: bool) {
    for queue in queues {
        queue.ai = on;
    }
}

/// What pruning needs from one repository's queue: its id, the slug GitHub knows it by, and every
/// open pull request in it as `(number, head sha)`.
///
/// Named because `Vec<(String, String, Vec<(u64, String)>)>` in a signature says nothing about
/// which `String` is the slug — `clippy::type_complexity` is right that nobody reads it twice.
type PrunableQueue = (String, String, Vec<(u64, String)>);

/// Which of these queues skein may tidy readings against, and what to hand [`skein::review::prune`]
/// for each — `(repo id, slug, every open PR and its head)`.
///
/// **Its own function because pruning had exactly one caller and that caller had none** (SKEIN-252).
/// `review::prune` was wired only into `GET /api/repos/:id/review`, and nothing asks for that route:
/// the pane opens on the merged answer instead (`src/web/index.html`), so
/// `summaries/<number>-<head_sha>.json` accumulated one file per PR per head commit for ever, and
/// merged pull requests kept all of theirs. `prune`'s own doc opens "Nothing used to", which had
/// become true again.
///
/// Two guards, and both are about not deleting something skein still wants:
///
/// * **`whole`** (SKEIN-231). `prune` reads a pull request's ABSENCE from this list as a reason to
///   go and ask whether it is closed. A membership search cut off at its page makes every pull
///   request past the hundredth absent for a reason that has nothing to do with it, and each of
///   their summaries would then pay a `pr_is_open` REST call, on every pane open, for ever.
/// * **`fresh`**. The superseded-head rule deletes a summary whose sha is not the one this queue
///   reports — which is only safe if the queue's idea of the head is current. A queue read back off
///   disk (`prq::remembered`, which stamps `fresh = false`) can be arbitrarily old, and pruning
///   against one could delete the reading of the commit the pull request is actually at now. The
///   dead route pruned only after a live `prq::queue` call; this keeps exactly that rule while
///   moving it to a route somebody calls.
///
/// The slug comes from the QUEUE rather than from `prq::repo_slug`, so a repository that has been
/// renamed is asked about under the name GitHub knows it by (`queue_within` follows the rename
/// before it fills this in).
fn prunable(queues: &[skein::prq::Queue]) -> Vec<PrunableQueue> {
    queues
        .iter()
        .filter(|q| q.whole && q.fresh && !q.slug.is_empty())
        .map(|q| {
            let open = q
                .prs
                .iter()
                .map(|pr| (pr.number, pr.head_sha.clone()))
                .collect();
            (q.repo_id.clone(), q.slug.clone(), open)
        })
        .collect()
}

/// The housekeeping a queue answer owes, run BEHIND it.
///
/// Detached, and after the answer is built: pruning may ask GitHub whether a pull request is closed,
/// and doing that on the way to the response would spend somebody's pane-open on tidying files they
/// cannot see. Nothing here has an answer the caller is waiting for.
fn prune_behind(queues: &[skein::prq::Queue]) {
    for (id, slug, open) in prunable(queues) {
        tokio::task::spawn_blocking(move || {
            skein::review::prune(&id, &slug, &open);
            // **And the boxes, not only the readings** (`docs/pr-review.md` §11). A review box
            // outlives its pull request otherwise, holding a checkout and a conversation nobody
            // will ask for again — and it holds them quietly, because a managed box is grouped as
            // skein's own and so does not look wrong on the board.
            //
            // Here rather than in its own tick because this is where the answer already is: the
            // one thing that may end a box is GitHub saying the pull request is closed, and
            // `prunable` has already established that this queue was read whole and fresh, which
            // is what makes an absence worth asking about at all.
            let numbers: Vec<u64> = open.iter().map(|(number, _)| *number).collect();
            // The question is asked HERE and the answer handed down: `reviewbox` may not reach
            // `prq` without joining the `{prq, review}` cycle, and it is the module that destroys
            // boxes. `prune` above asks the same question for the same pull requests, so this is
            // the second caller of it in one pass — worth knowing if either ever becomes slow.
            let ask = |number: u64| skein::prq::pr_is_open(&slug, number);
            for name in skein::reviewbox::close_finished(&id, &numbers, ask) {
                eprintln!("skein: {name}'s pull request is closed, so its review box is gone");
            }
        });
    }
}

/// Every repo's queue in one answer — what the pane opens on. Serves what the counts poll already
/// builds; `?force=1` re-reads GitHub.
///
/// **This is where readings are tidied** (SKEIN-252), because this is the route that runs.
async fn api_review_merged(Query(q): Query<HashMap<String, String>>) -> Response {
    let force = q.get("force").is_some_and(|v| v == "1" || v == "true");
    match tokio::task::spawn_blocking(move || skein::prq::merged(force)).await {
        Ok(mut m) => {
            let on = m.ai;
            settle_switch(&mut m.queues, on);
            prune_behind(&m.queues);
            Json(m).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// How many PRs need you, per repo — for the badge on the review button.
///
/// **The pull requests the count is OF travel with it, because the PAGE decides what needs you**
/// (SKEIN-323). `prq::counts` answers with `Lane::NeedsYou`, which is the REVIEWER's question; the
/// badge counts the pane's your-move list, which mixes both roles, so a pull request you opened
/// with changes requested on it belongs in the number and is not in that lane. That rule is
/// `cockpit/src/move.mjs`, one pure function, deliberately the page's and not the server's
/// (SKEIN-302) — so the fix is to send the rows and let the one rule count them, rather than to
/// write it a second time here in Rust and watch the two answers drift.
///
/// Polled on a slow timer, so it deliberately does NOT force a refresh — and how stale the badge
/// may be is decided in [`skein::prq::counts`], not here. That is the same per-repo cache the pane
/// reads, under a **ten-minute** budget where the pane insists on sixty seconds: a badge is a
/// number acted on within minutes, and every refresh behind it is a GitHub round trip per repo,
/// per open tab, every three minutes — the steady-state spend that got a live fleet rate-limited.
///
/// This comment used to say sixty seconds, on the strength of nothing but what the route did
/// before `e6c006e` moved the budget (SKEIN-235). It is one number in two places or it drifts
/// again, so `the_badge_route_documents_the_budget_prq_actually_uses` reads both.
///
/// Repos with the queue switched off, and repos with no GitHub remote, are never asked.
async fn api_review_counts() -> Response {
    // The merge train's stops are stapled on HERE, not inside `prq::counts` — the stops file is
    // `prwork`'s, and `prq` reading it would join the module cycle (`docs/modules.toml`). This
    // route already stands on both modules, and the stops are a disk read, so every branch of the
    // count — the failed and the switched-off included — can still say a machine waits on a person.
    match tokio::task::spawn_blocking(|| {
        let repos = skein::repos::load_repos();
        let mut counts = skein::prq::counts();
        for count in &mut counts {
            count.stopped = skein::prwork::stops(&count.repo_id);
        }
        counts
            .into_iter()
            .map(|count| {
                let prs = if count.error.is_empty() && count.skipped.is_empty() {
                    repos
                        .iter()
                        .find(|r| r.id == count.repo_id)
                        // **`Duration::MAX`, and it is what makes this free rather than what makes
                        // it stale.** `counts()` has just asked this very repo for a queue no older
                        // than ten minutes, so the cache holds that queue right now — anything this
                        // could read is the answer the count beside it was taken from. Asking for
                        // ten minutes again would be the same hit with one way to miss: the two
                        // numbers drifting apart would silently turn the badge poll into a SECOND
                        // GitHub round trip per repo per tab, which is the spend SKEIN-208 halved.
                        // A window nothing can fall outside cannot do that, and a repo `counts()`
                        // could not build has already been sent to the branch below.
                        .and_then(|r| skein::prq::queue_within(r, Duration::MAX).ok())
                        .map(|q| q.prs)
                        .unwrap_or_default()
                } else {
                    // Nothing was counted, so there is nothing to count again: `error` and
                    // `skipped` are the whole of what this repo has to say, and an empty list here
                    // is read by the page as "no rows", never as "no pull requests need you".
                    Vec::new()
                };
                BadgeCount { count, prs }
            })
            .collect::<Vec<_>>()
    })
    .await
    {
        Ok(counts) => Json(counts).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// One repo's badge entry: `prq`'s count, flattened, plus the pull requests it was taken over.
///
/// **The whole [`skein::prq::Pr`] rather than the fields today's rule happens to read.** A
/// projection would be a second statement of which facts decide whose move it is, kept in a
/// different language from the rule itself — and the bug this shape exists to end (SKEIN-323) is
/// exactly that: a fact the rule needs never reaching the thing that applies it. These are the same
/// rows `/api/review` already sends the pane, out of the same cache, so they cost no GitHub call.
#[derive(serde::Serialize)]
struct BadgeCount {
    #[serde(flatten)]
    count: skein::prq::Count,
    /// Empty for a repo that failed or was never asked — and empty for one with nothing open, which
    /// is the same list and the same number.
    prs: Vec<skein::prq::Pr>,
}

/// **What skein is reading right now.** In-memory, no disk, no GitHub — the page may ask often.
///
/// The page cannot know this on its own. A reading takes most of a minute (35s, measured on the
/// owner's fleet), it runs in a blocking task that outlives the request's browser, and skein starts
/// some of them itself. So a row that says "reading again… 22s" after a reload is reading it from
/// here (SKEIN-333).
///
/// **Deliberately not on the queue payload.** The queue is a GitHub round trip behind a 60s
/// micro-cache; in-flight state changes on the scale of a press and is dead within a minute. Riding
/// the queue would make the page choose between a stale spinner and refreshing GitHub every few
/// seconds — and the owner's own constraint on the timer is the opposite: "for the timer I hope you
/// are counting locally and doing github request once in a while only or on refresh." This route is
/// the cheap half; the elapsed seconds are counted by the page from `started_ms`.
async fn api_review_reading() -> Response {
    Json(skein::review::readings()).into_response()
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
    // Read once, so all three exits below answer the switch identically (SKEIN-299).
    let on_now = skein::review::summaries_enabled();
    // **Paint now, refresh behind.** Opening this tab used to block on three GraphQL searches per
    // repo plus the viewer lookup, so a cold cache showed nothing at all until every one of them
    // came back. What it was being compared against is a blank panel, and the last queue beats a
    // blank panel every time — as long as its age travels with it, which is what `as_of` and
    // `fresh` are for. Same rule as the board's staleness banner: stale is safe only when visible.
    //
    // `force` is the explicit refresh and always waits, because somebody who pressed it is asking
    // for the new answer rather than for a fast one.
    if !force {
        if let Some(mut fresh) = skein::prq::unexpired(&id) {
            settle_switch(std::slice::from_mut(&mut fresh), on_now);
            return Json(fresh).into_response();
        }
        if let Some(mut old) = skein::prq::remembered(&id) {
            // The refresh nobody is waiting for. Its result lands in the cache and on disk, so the
            // client's next ask — a few seconds later — is a cache hit rather than another wait.
            //
            // **Not forced** (SKEIN-235). It was `queue(&repo, true)`, which is the half of
            // SKEIN-206 this route never got: a client retries a stale answer at 4s/8s/16s/…, and
            // a forced refresh cannot be answered out of the cache a sibling refresh has just
            // filled, so every retry bought another round of GraphQL searches for the same repo —
            // "we aren't bombarding github right?". Unforced, a retry arriving after a sibling
            // landed is served by the `unexpired` check above and never reaches this line at all.
            //
            // `force` still means a forced read: it is handled below, where the caller waits for
            // it, because somebody who pressed refresh asked for the new answer rather than a fast
            // one. What is gone is forcing on a path where nobody asked for anything.
            //
            // Still one refresh short of `prq::merged`, which also holds `RefreshRunning` for the
            // repo so two cannot run at once. That guard is `prq`-private and belongs with the
            // cache it protects; exposing it is noted for that module's owner.
            tokio::task::spawn_blocking(move || {
                let _ = skein::prq::queue(&repo, false);
            });
            settle_switch(std::slice::from_mut(&mut old), on_now);
            return Json(old).into_response();
        }
    }
    match tokio::task::spawn_blocking(move || skein::prq::queue(&repo, force)).await {
        Ok(Ok(mut queue)) => {
            // The same rule as the merged route, spelled once (SKEIN-252). It used to be written
            // out here, and here alone — which is how the pruning came to have no caller at all.
            prune_behind(std::slice::from_ref(&queue));
            settle_switch(std::slice::from_mut(&mut queue), on_now);
            Json(queue).into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The queue a route needs in order to answer **about** the queue — from what skein already knows,
/// never from a GitHub round trip the reader waits on (SKEIN-291).
///
/// **The wait was never the payload.** Measured 2026-08-25 against a local server with a stubbed
/// GitHub and thirty-nine stored readings: with the micro-cache warm the full bulk-summaries answer
/// serialises in 7.4 ms and the thin one in 3.3 ms; with the micro-cache COLD and GitHub answering
/// in three seconds, *both* shapes take 3.11 s, and on a live fleet the same route was timed at
/// 10.42 s. The bytes cost about four milliseconds. Everything else is
/// `prq::queue(&repo, false)` refreshing past its sixty-second micro-cache (`queue` in
/// `src/prq/refresh.rs`, whose `force` match spends `Duration::from_secs(60)` on the `false` arm),
/// inline, before a byte is written — and a reader sees it as the cockpit hanging, because it
/// holds one of the browser's per-origin connections for the whole of it.
///
/// **These routes take the refresh off the reader's path, and deliberately do not start one of
/// their own.** Two things already refresh this cache: `GET /review` paints what is remembered and
/// kicks the refresh behind it (`api_review_queue` above), and the badge poll re-reads every repo
/// on a ten-minute budget (`prq::counts`, in `src/prq/refresh.rs`). The pane opens `/review`,
/// `/review/summaries` and `/workflows` for the same repo in one go, so a refresh started here as
/// well would be three GraphQL round trips per repo where one does — the duplicate-refresh spend
/// SKEIN-206's guard exists to prevent, rebuilt outside the guard, where it cannot see it.
///
/// It also makes these answers *agree*. Every one of them is keyed on the head shas the queue
/// reports, and taking them from the copy the pane is drawing is what stops a row and the reading
/// underneath it describing two different commits.
///
/// A machine with nothing remembered still waits: there is no older answer to hand over, and a
/// blank pane is not a faster one. What comes back then is a genuine read, marked fresh.
fn queue_as_known(repo: &skein::repos::Repo) -> Result<skein::prq::Queue, String> {
    if let Some(fresh) = skein::prq::unexpired(&repo.id) {
        return Ok(fresh);
    }
    if let Some(old) = skein::prq::remembered(&repo.id) {
        return Ok(old);
    }
    skein::prq::queue(repo, false)
}

/// Which queue an answer was built from, said on the answer itself.
///
/// skein already distinguishes a confident answer from a blind one — `Queue::fresh` and
/// `Queue::as_of` are that distinction, and SKEIN-239 is the item that exists to name the failure
/// of showing an old queue while claiming a current one. [`queue_as_known`] makes these routes able
/// to answer blind, so they have to be able to say so.
///
/// A header rather than a field, because these routes do not return a `Queue` and one of them
/// (`/review/summaries`) returns a bare map keyed by pull-request number, with nowhere to put a
/// field without changing a shape every caller destructures. One fact, one spelling, on every route
/// that can now answer from a remembered queue: `x-skein-queue: fresh | remembered`, and
/// `x-skein-queue-as-of` carrying the same RFC 3339 stamp `Queue::as_of` does.
fn answered_from<T: serde::Serialize>(queue: &skein::prq::Queue, body: T) -> Response {
    (
        [
            (
                "x-skein-queue",
                match queue.fresh {
                    true => "fresh",
                    false => "remembered",
                },
            ),
            ("x-skein-queue-as-of", queue.as_of.as_str()),
        ],
        Json(body),
    )
        .into_response()
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
    // Resolved before it is used, like the fifteen sibling routes on `/api/repos/:id` — this one
    // and `snooze` were the two that were not, and `:id` reaches `prq::review_dir` as a path
    // component. `..%2F..%2Fx` arrives here as `../../x`.
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    let id = repo.id;
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
    // Resolved first, for the reason written on `archive` above.
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    let id = repo.id;
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

/// One pull request's reading — computed if it is not held, or handed over as it stands.
///
/// Four markers. Three are about **who is asking and what may be spent**: `force=1` reads past the
/// cache, `asked=1` says a person asked so the day's ceiling does not apply, and `held=1` says read
/// nothing at all — answer with what is on disk, which is what an expanded queue row asks for once
/// the queue payload is thin.
///
/// `redraft=1` is a different question — **what must come back**. It maps to
/// `review::Review::Always`, so the pull request is reviewed even where skein would not review it
/// unasked: `review::Review::IfYours` asks whether the review is yours to give, and this says a
/// person is asking, which is its own authority. **There is nothing here to replace** — the
/// reading session posts its comment review to GitHub from inside its own checkout and skein keeps
/// no copy (`src/web/index.html` says the same thing from the other end) — so this marker is about
/// whether a review is drafted at all, where `force` is only about the cache. That is what is left
/// of SKEIN-293. The intent travels from the surface rather than being decided here, because
/// whether a row wants a review of its own is the pane's question and not this route's.
///
/// A redraft is always a forced read — `review::re_read_replacing_the_review` spells that itself,
/// because a cached reading returns from `visit` before anything is drafted and a redraft that
/// honoured the cache would be a press that does nothing. It is folded into `force` here as well,
/// for one reason: PRECEDENCE. `held=1` asks this route to read nothing at all, and the marker
/// that asks for work must not be silently downgraded into a disk read, so it wins.
///
/// **Absent, nothing changes.** The default is `Review::IfYours`, exactly what this route did
/// before, so a server that lands ahead of the page is invisible.
///
/// **What the page still asks of this route is `held=1`, and only that** (SKEIN-366). Verified:
/// `grep -n 'number}/summary' src/web/index.html` finds every `fetch` of this path — two of them,
/// at `:3231` and `:4142` — and both spell `?held=1`: the row opening, and the poll's landed
/// transition. The COMPUTING arm is still
/// served and is no longer what the cockpit presses: a reading held a browser connection open for
/// the length of a model call, and ten at once (`REV_ASKED_PARALLEL`) took every connection the
/// browser has. The page starts one at [`api_review_read`] now and collects it from the live
/// stream.
///
/// It is kept rather than deleted, and this paragraph is why: it is the one door that answers a
/// reading ON the request, which is what makes it usable by hand, by a script and by anything that
/// is not holding an `EventSource` — and [`read_a_pull_request`] is the same reading either way, so
/// there is no second behaviour here to drift.
async fn api_review_summary(
    Path((id, number)): Path<(String, u64)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    // What must come back, rather than who is asking — see the note above. Read first because
    // `force` follows from it.
    let redraft = flag(&q, "redraft");
    let force = redraft || flag(&q, "force");
    // The owner's boundary (see `review::Trigger`): the daily budget limits only what skein does
    // on its own initiative. A request a person made — the read button (`asked=1`) or a forced
    // re-read — is never budget-checked and never counted. Absent both markers the request is
    // treated as UNASKED, which is the safe default: a route that forgets the marker gates a
    // button instead of un-gating the pump.
    let asked = force || flag(&q, "asked");
    // **`held=1`: hand over what is on disk and read nothing.** The row that opened is asking for
    // the prose the thin queue payload left behind — a request to REMEMBER, not to analyse — and
    // it must never become a model call, on any head, at any hour of the budget. `force` wins if
    // both are given, because a person pressing "re-read" has asked for the opposite of this.
    let held = !force && flag(&q, "held");
    let trigger = if asked {
        skein::review::Trigger::Asked
    } else {
        skein::review::Trigger::Unasked
    };
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    let out = tokio::task::spawn_blocking(move || {
        // Two different questions, so two different queues (SKEIN-291).
        //
        // `held=1` is a row opening: it reads the prose already on disk for the head the row is
        // showing, and that head comes from the queue the pane painted — which may be the
        // remembered one. Going to GitHub first would put a refresh in front of a 4 ms disk read
        // (SKEIN-286), on the one path defined as "read nothing".
        //
        // Everything else on this route COMPUTES — a person pressed read, or re-read, or the pump
        // asked — and a reading is worth only the commit it was taken of. Spending a model call
        // against a head that has since moved is worse than waiting for the refresh that says so,
        // so those arms still ask for the current queue.
        if !held {
            return read_a_pull_request(&repo, number, redraft, force, trigger);
        }
        let queue = queue_as_known(&repo)?;
        let pr = queue
            .prs
            .iter()
            .find(|p| p.number == number)
            .ok_or("that PR is not in your queue")?;
        Ok((
            queue.clone(),
            skein::review::held(&repo.id, pr.number, &pr.head_sha),
        ))
    })
    .await;
    match out {
        Ok(Ok((queue, summary))) => answered_from(&queue, summary),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The blocking half of a reading: the queue it was taken against, and the answer.
///
/// **One function, two doors.** [`api_review_summary`] answers it on the request that asked, and
/// [`api_review_read`] answers it on the live stream. Two copies of this would be two producers of
/// one artefact, drifting in what they spend, what they store and which head they read — the
/// SKEIN-243 mistake by another name, and the one this file already carries a paragraph about on
/// [`api_critique_draft`].
fn read_a_pull_request(
    repo: &skein::repos::Repo,
    number: u64,
    redraft: bool,
    force: bool,
    trigger: skein::review::Trigger,
) -> Result<(skein::prq::Queue, skein::review::Known), String> {
    // A reading is worth only the commit it was taken of, so this arm asks for the current queue —
    // spending a model call against a head that has since moved is worse than waiting for the
    // refresh that says so.
    let queue = skein::prq::queue(repo, false)?;
    let pr = queue
        .prs
        .iter()
        .find(|p| p.number == number)
        .ok_or("that PR is not in your queue")?;
    let identities = std::iter::once(queue.viewer.clone()).collect::<Vec<_>>();
    let summary = if redraft {
        skein::review::re_read_replacing_the_review(repo, &queue.slug, pr, &identities)
    } else {
        skein::review::summarise(repo, &queue.slug, pr, &identities, force, trigger)
    };
    // The same shape the bulk route answers, built by `review` rather than assembled here: one
    // visit produces the summary AND the review in one model call now, and a route that answered
    // only half of that made the page wait for a refresh to learn the other half.
    let known = skein::review::known_at(&repo.id, summary, &pr.head_sha);
    Ok((queue.clone(), known))
}

/// Start a reading, and answer at once. The reading itself comes back on `/api/events`.
///
/// **This route exists to cost a connection for milliseconds instead of minutes** (SKEIN-366). The
/// cockpit is HTTP/1.1 — verified: `curl --http2 …/api/health` still answers `HTTP/1.1 200 OK` —
/// and browsers cap that at six connections per origin. A reading is a model call taking tens of
/// seconds, and the page reads ten at a time when somebody presses a stack read
/// (`REV_ASKED_PARALLEL`), so on the request-shaped route those ten hold every connection the
/// browser has. Measured in a real browser: an unrelated `GET /api/health` from the same page took
/// 12 ms with three readings in flight, 12,814 ms with six, and 34,438 ms with ten.
///
/// **Not a smaller width.** The instruction, given twice, is that a read somebody asks for is not
/// rationed; lowering the parallelism would move the cliff rather than remove it. What changes is
/// where the answer travels: this returns immediately, and [`skein::review::ReadingDone`] carries
/// the whole reading down the `EventSource` the page already holds. Ten readings then cost one
/// connection between them.
///
/// **The reading is not cancelled by the client going away**, and that is deliberate — it was
/// already true. The work runs on the blocking pool and writes to disk whichever way it ends; a
/// reload used to lose the answer's delivery and now loses nothing, because the next board to open
/// hears it or reads it off disk.
///
/// Duplicate presses are the caller's business, exactly as they were on the request-shaped route:
/// this spends what it is asked to spend.
async fn api_review_read(
    Path((id, number)): Path<(String, u64)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    // Read exactly as [`api_review_summary`] reads them, including the safe default: absent both
    // markers the request is UNASKED, so a caller that forgets one gates a button rather than
    // un-gating a sweep.
    let redraft = flag(&q, "redraft");
    let force = redraft || flag(&q, "force");
    let asked = force || flag(&q, "asked");
    let trigger = match asked {
        true => skein::review::Trigger::Asked,
        false => skein::review::Trigger::Unasked,
    };
    // Refused HERE rather than announced as a failed reading: "no such repo" is a fault in the
    // request, and a caller that gets `ok` and then a failure on the stream cannot tell a bad URL
    // from a model that would not answer.
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    tokio::task::spawn_blocking(move || {
        let done = match read_a_pull_request(&repo, number, redraft, force, trigger) {
            Ok((queue, known)) => skein::review::ReadingDone {
                repo_id: repo.id.clone(),
                number,
                summary: serde_json::to_value(&known).ok(),
                error: String::new(),
                // The `x-skein-queue` distinction, in the payload: a reading delivered on a stream
                // has no headers to carry it, and "this answer was built from a remembered queue"
                // is exactly the fact SKEIN-239 exists to stop being dropped.
                queue: match queue.fresh {
                    true => "fresh".into(),
                    false => "remembered".into(),
                },
                as_of: queue.as_of.clone(),
            },
            // A failure is announced, never swallowed. The page turned a failed request into a
            // visible `transient` row carrying the reason, and it must go on being able to: a
            // reading that simply never arrives is a row that says "reading…" for ever.
            Err(e) => skein::review::ReadingDone {
                repo_id: repo.id.clone(),
                number,
                summary: None,
                error: e,
                queue: String::new(),
                as_of: String::new(),
            },
        };
        skein::review::announce_reading(done);
    });
    Json(serde_json::json!({ "ok": true, "reading": true })).into_response()
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
///
/// **`?rows=1` asks for the row shape** — the same readings with the prose taken out
/// ([`skein::review::Known::thin`]). Measured on a live fleet, 2026-08-25, thirty-nine stored
/// readings: 153,381 bytes for the full answer, of which a collapsed row draws the line, the
/// flags and whether a review is drafted. Reproduced locally at 155,167 B against 12,055 B, and
/// 7.4 ms of server time against 3.3 ms
/// (`tests/server.rs::the_review_queue_payload_can_be_asked_for_rows_instead_of_prose`, which
/// prints both). The prose comes back per row when a row is opened, from
/// `/review/:n/summary?held=1`.
///
/// **The ten seconds in that measurement was not this payload**, and saying so here is the point:
/// with the queue's micro-cache warm the full answer is written in single-digit milliseconds, and
/// with it cold both shapes waited the same however long `prq::queue` took to hear back from
/// GitHub. That wait was SKEIN-291, and it is gone from this route — it goes through
/// [`queue_as_known`], which reads what skein already holds and never blocks on a refresh. This is
/// the bytes, and the connection those bytes occupy.
///
/// Which queue the answer was built from travels on the response, `x-skein-queue: fresh` or
/// `remembered` — see [`answered_from`]. A map keyed by pull-request number has nowhere to put the
/// `fresh`/`as_of` pair a `Queue` carries in its own payload, and an answer that cannot say it is
/// blind is the thing SKEIN-239 exists to refuse.
///
/// A query parameter rather than a second route, for the reason the shape itself is a `thin()` and
/// not a `Row` struct: one handler, one `known()` call, one serialisation. A second route is a
/// second place to assemble a payload, and the last time this record had two of those the drafted
/// review and the summary stopped agreeing (SKEIN-243). The default is unchanged and stays the
/// full reading, so no caller is affected by this existing.
async fn api_review_summaries(
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let rows = flag(&q, "rows");
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    let out = tokio::task::spawn_blocking(move || {
        // `queue_as_known`, not `queue(&repo, false)`: this route reads disk, and it used to do it
        // behind a GitHub refresh that took 10.42 s on a live fleet (SKEIN-291). The queue is
        // wanted here only for the list of (number, head) pairs to look up, and the pairs the pane
        // is drawing are exactly the remembered ones.
        let queue = queue_as_known(&repo)?;
        let want: Vec<(u64, String)> = queue
            .prs
            .iter()
            .map(|pr| (pr.number, pr.head_sha.clone()))
            .collect();
        Ok::<_, String>((queue, skein::review::known(&repo.id, &want)))
    })
    .await;
    match out {
        Ok(Ok((queue, known))) => answered_from(
            &queue,
            known
                .into_iter()
                .map(|(number, k)| (number.to_string(), if rows { k.thin() } else { k }))
                .collect::<std::collections::BTreeMap<_, _>>(),
        ),
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
///
/// Delegated to `workflow::editor_shape` — the same code `to_bytes` writes the file with. The
/// hand-built copy this replaces dropped `serial` the day it was added, which under-reported a
/// running train AND meant an editor save would strip it from the file (see `editor_shape`).
fn written(f: &skein::workflow::Workflow) -> serde_json::Value {
    skein::workflow::editor_shape(f)
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
        // The queue as skein already knows it (SKEIN-291). This route's own note above says it is
        // "a separate read from the queue" and the page's call site calls it "cheap: it reads the
        // cached queue" — but `queue(&repo, false)` refreshes the moment that cache is a minute
        // old, and the pane fetches this per repo alongside `/review` and `/review/summaries`, so
        // a cold minute made three routes wait on three refreshes of one queue.
        let queue = queue_as_known(&repo)?;
        // One read for every PR's history — `journal()` per PR would re-read the same file
        // per row.
        let mut journals = skein::prwork::journals(&repo.id);
        // What the train view is computed FROM: the same carrying set the sweep uses — archived
        // PRs excluded, because a PR set aside is one you said "not now" about and the
        // sweep honours that; a panel that showed it in the line would promise an act the tick
        // will never take.
        let mut carrying: Vec<(u64, String)> = Vec::new();
        let mut prs = serde_json::Map::new();
        for pr in &queue.prs {
            // `facts_of_in`, not `facts_of`: the tick answers the reviewer's reading facts from
            // the repo's own cache, and a panel that answered them from nothing would show an
            // approval as unreachable while the tick reached it. That is the disagreement the
            // comment above forbids, one field further down.
            let facts = skein::prwork::facts_of_in(&repo.id, pr, &queue.viewer, &queue.trunk);
            let standing = skein::prwork::standing(&repo.id, pr.number, &facts, &flows);
            // **`holding` too, not just `workflow`** (SKEIN-326). A holding pull request — assigned
            // by hand, its workflow's own `matches` not met (SKEIN-279) — carries a non-empty
            // `standing.workflow` ON PURPOSE, so the row can still show which workflow somebody
            // chose. But `prwork::sweep` acts on `Carries::acting`, not `Carries::name`, and skips
            // it. Without this condition the panel drew it as a car, and as the FRONT if it had the
            // lowest number, while the tick's front was somebody else — which is precisely what the
            // comment above says must not happen.
            if !standing.workflow.is_empty()
                && standing.holding.is_empty()
                && !matches!(pr.lane, skein::prq::Lane::Archived)
            {
                carrying.push((pr.number, standing.workflow.clone()));
            }
            let mut entry = serde_json::to_value(standing).unwrap_or_default();
            // The history rides beside the standing: "which step is it on" and "what has it
            // already done" are one question to the person automating this.
            entry["journal"] =
                serde_json::to_value(journals.remove(&pr.number).unwrap_or_default())
                    .unwrap_or_default();
            prs.insert(pr.number.to_string(), entry);
        }
        let trains = skein::prwork::trains(&repo.id, &carrying, &flows);
        Ok::<_, String>((
            queue,
            serde_json::json!({
                "enabled": skein::prwork::enabled(),
                "read_prs": repo.read_prs,
                // `editor_shape`, NOT a hand-built copy: this route had the second of the two hand
                // serializers that silently dropped `serial` — see workflow::editor_shape.
                "defined": flows.iter().map(skein::workflow::editor_shape).collect::<Vec<_>>(),
                "trains": trains,
                "prs": prs,
            }),
        ))
    })
    .await;
    match out {
        Ok(Ok((queue, value))) => answered_from(&queue, value),
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
#[derive(Deserialize)]
struct TriggersReq {
    /// The words this pull request wakes on. Absent forgets the override and lets the repo's set
    /// speak again; an EMPTY list is the deliberate "wake on nothing", which is a third state and
    /// not the same as absent.
    #[serde(default)]
    on: Option<Vec<String>>,
}

/// Give one pull request its own trigger set, or take it back off — §10's "overridable per pull
/// request", which until now only the workflow assignment was.
///
/// The three states live in `repos::set_pr_triggers` where they are tested, for the reason the
/// route above records: the last time a three-state meaning was written in a route it grew a bug
/// within the hour.
async fn api_set_pr_triggers(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<TriggersReq>,
) -> Json<serde_json::Value> {
    if !skein::repos::load_repos().iter().any(|r| r.id == id) {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    }
    match skein::repos::set_pr_triggers(&id, number, req.on) {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}

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
    /// Line comments, posted WITH the verdict — GitHub's own review semantics — so a verdict kind
    /// with comments goes through the review-with-comments call, and a non-verdict kind refuses
    /// them rather than dropping them silently. The cockpit sends none: the surface that drafted
    /// them on a line of a diff was skein's own reading view, and the change is read on GitHub now.
    #[serde(default)]
    comments: Vec<skein::prq::ReviewComment>,
    /// The head sha the comments were drafted against — what the reader was actually looking at.
    /// Empty means "assume current". When it trails the live head, the comments are re-anchored
    /// against the new diff rather than refused (SKEIN-214): a moving PR must not make a finished
    /// review unpostable.
    #[serde(default)]
    drafted_at: String,
}

async fn api_review_act(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<ActReq>,
) -> Json<serde_json::Value> {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    let out = tokio::task::spawn_blocking(move || {
        // **A write derives what it addresses without a queue refresh** (SKEIN-272), through the
        // two functions written for exactly that — `slug_for_write` here for the repository, and
        // `head_to_post_against` below for the commit. So a GitHub READ failing can never make a
        // verdict impossible and then report it in the refresh's words — five membership searches,
        // about a repository nobody asked after. This route used to open with
        // `prq::queue(&repo, false)?` for `queue.slug` and `pr.head_sha`, which put every verdict
        // the cockpit can post behind a full refresh.
        let slug = skein::prq::slug_for_write(&repo)?;
        let verdict = match req.kind.as_str() {
            "approve" => Some(skein::prq::Verdict::Approve),
            "request-changes" => Some(skein::prq::Verdict::RequestChanges),
            "comment" => Some(skein::prq::Verdict::Comment),
            _ => None,
        };
        // `ask` and `draft` need the whole `Pr`, and neither writes to GitHub. They read the queue
        // in their own arms, where a refresh that fails is honestly about what was asked for — and
        // where "that PR is not in your queue" is a true and useful thing to say, which it was not
        // in front of a verdict on a PR you authored and were never asked to review.
        let queued = || -> Result<skein::prq::Pr, String> {
            skein::prq::queue(&repo, false)?
                .prs
                .into_iter()
                .find(|p| p.number == number)
                .ok_or_else(|| "that PR is not in your queue".to_string())
        };
        let text = match (verdict, req.kind.as_str()) {
            (Some(v), _) if !req.comments.is_empty() => {
                // What `commit_id` must name. `head_to_post_against` reads the LIVE head and is
                // the one place that says what to do when it cannot — one function, so no write
                // path can answer it differently again (SKEIN-230) — and its fallback is what this
                // machine already remembers rather than the sha the draft was read at, which would
                // compare equal to itself.
                let seen_at = skein::prq::remembered_head(&id, number);
                let head = skein::prq::head_to_post_against(
                    &slug,
                    number,
                    seen_at.as_deref().unwrap_or(&req.drafted_at),
                );
                skein::prq::submit_review_with_comments(skein::prq::ReviewPost {
                    slug: &slug,
                    number,
                    head_sha: &head,
                    verdict: v,
                    body: &req.body,
                    comments: &req.comments,
                    drafted_at: &req.drafted_at,
                    // The person's own credential, which is what a review is posted as. Sourced
                    // here rather than inside, so the one rule this route has to honour is written
                    // where somebody reading the route can see it.
                    token: &skein::prq::host_token()?,
                })?
            }
            (Some(v), _) => skein::prq::submit_review(&slug, number, v, &req.body)?,
            (None, _) if !req.comments.is_empty() => {
                return Err(format!(
                    "line comments post with a verdict — approve, request-changes or comment — \
                     not with {}",
                    req.kind
                ))
            }
            (None, "merge") => {
                // **The revision the person actually looked at** (SKEIN-338). Until this line the
                // merge chip called `prq::merge(&slug, number)`, which sent `merge_method` and
                // nothing else — no expected head, no base check — while the merge train beside it
                // sent `sha` and refused to merge off the trunk. The unguarded one was the only
                // merge a person could reach, and on a fleet with `$SKEIN_PR_WORKFLOWS` off it was
                // the only merge skein performed at all.
                //
                // Two sources, best first. `drafted_at` is what the CLIENT says is on screen — the
                // same field the verdict path above uses for the same question, so there is one
                // wire name for "the head I was reading". `remembered_head` is what THIS machine
                // last saw for the row the chip was drawn on, from the cache or the copy on disk,
                // and it reads nothing over the network — the SKEIN-272 rule, so a GitHub read
                // failing can never be what stops a merge.
                //
                // Neither is asked of GitHub, deliberately: the live head is what the merge is
                // being checked AGAINST, and deriving the expectation from the same place would
                // make it agree with itself and guard nothing. When both are empty the merge is
                // refused rather than defaulted — `prwork::merge_by_hand` says so in words.
                let seen = match req.drafted_at.trim().is_empty() {
                    false => req.drafted_at.clone(),
                    true => skein::prq::remembered_head(&id, number).unwrap_or_default(),
                };
                skein::prwork::merge_by_hand(&slug, number, &seen)?
            }
            (None, "ask") => skein::review::ask(&repo, &slug, &queued()?, &req.body)?,
            (None, "draft") => skein::review::draft_comment(&repo, &slug, &queued()?, &req.body)?,
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
    // Nineteen `:name` routes in this file open with this line and this one did not, which is how
    // `..%2F..%2Fx` reached `~/.skein/boxes/<name>/tracking`. `tracking::set_box_tracking` refuses
    // it too now — the library is where a guard cannot be skipped by the next caller — and this
    // line stays because the two answer different questions: the library's is 500-shaped ("skein
    // could not do that"), and a name a client sent is a 400.
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
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
        Some(_) => skein::fleet::box_identity(&name),
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
        // SURFACE 3 of SKEIN-846 — the one field this lane added to the box record, and it is the
        // whole of what the cockpit needs to render the state.
        //
        // `"covered"`, `"workshop"` or `"uncovered"`. The first is the ordinary box. The second is
        // this panel's `privileged` above, said as a cover rather than as a switch. The third is a
        // box skein cannot match to a repository: uncovered with nobody having chosen it, which
        // until SKEIN-836 was indistinguishable from `"covered"` from every surface skein had — and
        // an uncovered box reads and writes every other repository's store and work tree.
        //
        // Derived, so it is the cover this box gets at its NEXT start; that and `privileged` above
        // have the same grammar for the same reason (a namespace is built when a box comes up).
        // `fleet::box_exposure` is the single definition, shared with the refusal and with the
        // manifest the launcher is actually handed, so this cannot say "covered" over a box the
        // launcher is about to hand an empty manifest.
        "exposure": skein::fleet::box_exposure(&name).spelled(),
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
    // **An SVG is a document, not a picture**, and this route serves whatever is in a box's tree.
    //
    // It used to answer `image/svg+xml`, which means a `.svg` a box wrote — or one that arrived in a
    // cloned repo — rendered as markup in the cockpit's own origin as soon as anybody navigated to
    // this URL, with the session cookie attached to everything it then did. The in-page path was
    // never the problem: the file viewer draws images with `<img src=…>`, which does not run script
    // in an SVG. Direct navigation was, and a markdown link reaches it.
    //
    // So the inert types are named and everything else is a download. `octet-stream` plus an
    // attachment disposition is the pair that matters — the type alone still lets a browser sniff,
    // which is what `nosniff` on every response now also refuses.
    let ct = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ if bytes.contains(&0) => "application/octet-stream", // NUL byte ⇒ not text
        _ => "text/plain; charset=utf-8",
    };
    // `text/plain` is safe to render inline and is most of what this route serves, so only the
    // genuinely opaque answers are pushed to a download.
    let inline = ct != "application/octet-stream";
    let mut response = (
        [
            (axum::http::header::CONTENT_TYPE, ct),
            (
                axum::http::HeaderName::from_static("x-truncated"),
                if truncated { "1" } else { "0" },
            ),
        ],
        bytes,
    )
        .into_response();
    if !inline {
        response.headers_mut().insert(
            axum::http::header::CONTENT_DISPOSITION,
            axum::http::HeaderValue::from_static("attachment"),
        );
    }
    response
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
/// `slug` is resolved here rather than in the browser because `source` is not always the answer: an
/// entry registered before a path stopped being registrable holds a path, and the remote it really
/// fetches lives in the mirror's `origin` — a `git` call only the host can make. A browser parsing
/// `source` alone disagreed with the host about the same repo. Empty string ⇒ no GitHub remote.
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

/// What is true of this machine right now.
///
/// **Where skein is running used to ride here too** (SKEIN-467), beside the report rather than on
/// `/api/settings`, because it was not a setting: it was declared in the environment, nothing could
/// write it, and `/api/settings` is a document the page reads back and POSTs. There is one
/// deployment now (SKEIN-576) and nothing to report — see the note at the merge site below, and
/// `docs/parity.md` §7 for what a person stops being told.
///
/// Boxes cannot see this either way — the route is behind the same token as the rest.
async fn api_health() -> Json<serde_json::Value> {
    let report = {
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
                disk: skein::health::HealthCheck::unknown("the health check itself failed"),
                gitgate: skein::health::HealthCheck::unknown("the health check itself failed"),
                warden: skein::health::HealthCheck::unknown("the health check itself failed"),
                cover: skein::health::HealthCheck::unknown("the health check itself failed"),
                logins: Vec::new(),
                expired_logins: Vec::new(),
                runtime_updates: Vec::new(),
                models: Vec::new(),
                dark_boxes: Vec::new(),
                stale_boxes: Vec::new(),
                uncovered_boxes: Vec::new(),
                uncapped_boxes: Vec::new(),
                runtimes: skein::runtime::supported_runtimes(),
                // Empty rather than guessed: this is the report for a health task that *failed*, and
                // the checklist reads this field as "boxes can push". Naming a credential here would
                // tick that step off on the strength of a crash.
                git_credential: String::new(),
            })
    };
    // `ok: false` and nothing else, for the reason the arm above gives: a report the page cannot
    // read is a broken report, not a healthy fleet, and the banner is how it says so.
    let body = serde_json::to_value(&report).unwrap_or_else(|_| serde_json::json!({"ok": false}));
    // A `deployment` object used to be merged on here — `label`, `implies`, and the `in_fleet`
    // boolean the page branched on to decide whether to offer the rebuild button. There is one
    // deployment (SKEIN-576), so there is nothing to report and nothing to branch on: the page
    // hides that button permanently, because `docs/architecture.md` §7.5 puts fleet lifecycle
    // outside the fleet, not because a flag says so. What a person loses with `implies` — the
    // sentence saying where their skein runs and what is reachable from there — is in
    // `docs/parity.md` §7.
    Json(body)
}

/// How old a cached reading may be before a plain page load pays to take a new one.
///
/// The owner's rule, in his words: "cache, no reread on every page load, re-reads have to be
/// intentional by user and once in a while (say 1 hr)". Both halves of it are in [`api_usage`] —
/// `?refresh=1` is the intentional one, and this is the ceiling on everything else.
///
/// **`USAGE_STALE_SECS` in `cockpit/src/usage.mjs` is the same hour and is deliberately not derived
/// from this one.** They answer different questions: this decides whether skein re-reads 2.72 GB,
/// that decides whether the panel calls a reading old on screen. A cockpit that said "over an hour
/// old" at forty minutes would still be telling the truth about the reading it is showing, so the
/// two are free to differ and neither is a copy of the other.
const USAGE_MAX_AGE: Duration = Duration::from_secs(3600);

/// One fleet scan at a time, across every caller.
///
/// [`skein::usage::refresh`] walks every box's transcripts — 2.72 GB and 5.2 s from cold on the
/// fleet its cost table was measured on. Two tabs opening Settings at the same moment, or one
/// person clicking Refresh twice, would otherwise run that walk twice over the same bytes and race
/// to write the same cache. Serialised, the second caller waits and then finds the first one's
/// answer already stored: a plain load costs it the 20 kB read, and a refresh it asked for costs
/// the 0.11 s re-stat rather than the whole scan again.
///
/// **Held across the cached path too, rather than only around a scan.** Checking the cache first
/// and taking this only when a scan looks necessary would keep a plain load from ever queueing
/// behind somebody else's refresh — but it means writing the "is this reading young enough" rule a
/// second time, here, beside the one inside [`skein::usage::report`] that actually decides. Two
/// copies of that comparison is how a route ends up serving on one rule while reporting under
/// another, which is the whole defect this panel's freshness fields exist to make visible. So there
/// is one rule in one place, and the price is that a plain load arriving during a scan waits for it
/// and is then answered from the newer reading.
static USAGE_SCAN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What the fleet has spent, from the cached reading — re-read only when asked, or when the hour is up.
///
/// **A page load must never walk the fleet**, which is the entire reason this route defers to
/// [`skein::usage::report`] rather than [`skein::usage::refresh`]: `report` serves the stored
/// tally when it is inside the window and scans only when it is not. The cost table in
/// `src/usage.rs` is the argument — under 1 ms against 5.2 s — and it is measured rather than
/// assumed.
///
/// # The first request on a host with no cache blocks, and that is a choice
///
/// There is no reading to serve before the first scan, so the 5.2 s has to happen somewhere. The
/// alternative was to answer "nothing read yet" at once and scan in the background. It was rejected
/// because of what the page then does with that answer: `loadUsage` in `src/web/index.html` fetches
/// **once** when the pane is first opened and never polls, so a person opening Settings → Usage for
/// the first time would be told there is no reading, and left looking at it — the background scan
/// would finish into a page with no way to hear about it, and the only way forward would be to
/// press Refresh, which on a scan already in flight either queues behind it or starts a second one.
///
/// Blocking instead costs that person one wait, once per host, and the pane says what it is doing
/// while it waits: `usageHtml` renders "not read yet" with **no figure** — never a zero — and the
/// page disables the Refresh button and relabels it "reading…" for exactly as long as this request
/// is outstanding. Every load after it is the 20 kB read.
///
/// # A scan that fails still answers with the reading there is
///
/// `report` returns `Err` when a scan could not be completed or its cache could not be written. If
/// a previous reading is stored, that is served instead of a 500, and [`skein::usage::cached`]
/// stamps it `fresh: false` — which the panel renders as "read 3 hours ago — and skein has marked
/// it out of date". An old reading that says how old it is beats an empty pane, and this is the
/// field that says so. Only a failure with nothing stored behind it is a 500, which the page shows
/// as "skein could not read the fleet's usage".
async fn api_usage(Query(q): Query<HashMap<String, String>>) -> Response {
    // Presence means the caller meant it, and only an explicit negative is read as "no". The page
    // sends `?refresh=1` and nothing else, so this generosity is for a person typing the URL: on a
    // stricter rule `?refresh` or `?refresh=true` would quietly serve the cache while the caller
    // believed they had asked for a re-read — a re-read that silently did not happen is the same
    // shape of lie as an age that does not belong to the body it is attached to.
    let asked = !matches!(
        q.get("refresh").map(|v| v.trim()),
        None | Some("") | Some("0") | Some("false")
    );
    let _one_at_a_time = USAGE_SCAN.lock().await;
    // `spawn_blocking` because the scan is filesystem-bound and can run for seconds. On the async
    // worker it would hold up every other request on that thread — the freeze
    // `slow_fleet_snapshot_does_not_starve_concurrent_requests` exists to catch.
    //
    // **`refresh` directly rather than `report(Duration::ZERO)`**, which since SKEIN-847 would do
    // the same thing: the window is strict, so one of zero admits nothing and zero rescans. It
    // stays `refresh` because that is what this branch means — a reading of the fleet, not a stored
    // one no older than no time at all. The distinction was load-bearing when this route was
    // written: `age_secs <= max_age` served a reading taken inside the same whole second back to a
    // caller who had asked for a fresh one, so the second press of Refresh did nothing on exactly
    // the occasion it was doubted. `no_window_admits_a_reading_as_old_as_itself` holds the boundary
    // now, and `tests/ui/usage.mjs` watches the press after the press from outside the process.
    let taken = if asked {
        tokio::task::spawn_blocking(skein::usage::refresh).await
    } else {
        tokio::task::spawn_blocking(|| skein::usage::report(USAGE_MAX_AGE)).await
    };
    match taken {
        Ok(Ok(report)) => Json(report).into_response(),
        Ok(Err(why)) => match tokio::task::spawn_blocking(skein::usage::cached).await {
            Ok(Some(stale)) => Json(stale).into_response(),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("skein could not read the fleet's usage: {why}"),
            )
                .into_response(),
        },
        // The scan task itself died — a panic in the reader, or a runtime shutting down. Distinct
        // from `Err(why)` above, which is the reader reporting that it could not finish: this one
        // has no verdict to report, and saying which of the two happened is the difference between
        // a bug report somebody can act on and "usage is broken".
        Err(joined) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the fleet usage reader did not finish: {joined}"),
        )
            .into_response(),
    }
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

/// Register a repo: clone its remote, provision its store + kit, record it. A path is a 400, from
/// `add_repo`. `git clone` can take a while, so run the blocking work off the async runtime.
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
            // The repository this maps to, answered once the repo is registered rather than left to
            // the browser to parse out of what was typed — the host and the page agreeing on one
            // slug is what lets a write token offered in the dialog be stored against the right repo.
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

/// What boxes have asked the fleet to install.
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
/// The counters are files under `/sys/fs/cgroup` and `/proc` in the sandbox this process runs in,
/// so this is a handful of reads and no subprocess. Asked when somebody wants it, never on a tick.
/// It used to be an HTTP call to the in-sandbox agent, which could fail to answer; it cannot now
/// (SKEIN-521), so the 503 arm that meant "no agent" is gone with it.
///
/// A rate rather than a total, because everything the kernel keeps here is monotonic since boot: a
/// raw `98305` says the same enormous thing for ever and never says whether it is happening now.
async fn api_machine_pressure() -> Response {
    match tokio::task::spawn_blocking(skein::fleet::pressure).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// What the fleet's VM is using right now: memory, disk, load.
///
/// `spawn_blocking` for the same reason `api_events` uses it — behind this is an `sbx exec`, and
/// running one on an async worker stalls every terminal websocket that worker is pumping.
///
/// 204 rather than an error when there is no fleet: a board with each box in its own sandbox has no
/// single machine to gauge, and that is a normal configuration rather than something to warn about.
async fn api_fleet_resources() -> Response {
    match tokio::task::spawn_blocking(skein::fleet::fleet_resources).await {
        Ok(Some(r)) => Json(r).into_response(),
        _ => StatusCode::NO_CONTENT.into_response(),
    }
}

/// What the fleet sandbox takes, and from what.
///
/// The fleet sandbox is the largest thing skein builds on someone's machine, it is sized by
/// `fleet_memory` and its neighbours, and it cannot be changed afterwards without a rebuild: sbx
/// fixes memory, CPUs and disk at creation. So the numbers are worth showing beside what the machine
/// has, even where nothing on this page can set them.
///
/// So this is what the fleet pane has to say about the sandbox in one call: what the host has, what
/// skein would take of it, why sbx could not be asked, and the lines to run on the host in place of
/// a rebuild.
///
/// **It no longer says whether the sandbox is there, and there is no longer a question to ask**
/// (SKEIN-627). `exists` was a tri-state here, and the create-fleet dialog was its only reader:
/// `exists === false` was the one state that meant "there is none", and it opened the dialog. From
/// inside the sandbox `fleet::fleet_exists` answers `Some(true)` for the fleet this process is
/// standing in and `None` for every other name — `Some(false)` cannot arise — so the field carried
/// one value, nothing branched on it, and the dialog it existed for is deleted.
async fn api_fleet_plan() -> Json<serde_json::Value> {
    let (host, sandbox, refusal) = tokio::task::spawn_blocking(|| {
        let sandbox = skein::place::fleet_sandbox();
        // What the rebuild route would refuse with, verbatim — null on a host, where it refuses
        // nothing. It is here because the page HIDES that button in-fleet, and a hidden control
        // with no replacement is a dead end: this is the `sbx` lines to run on the host instead.
        // Rendering the refusal itself rather than the page composing its own means what somebody
        // is told here and what pressing would have said cannot drift.
        let refusal = skein::fleet::fleet_lifecycle_refusal("rebuild", true);
        (skein::fleet::host_capacity(), sandbox, refusal)
    })
    .await
    .unwrap_or_else(|_| (skein::fleet::host_capacity(), String::new(), None));
    let proposed = skein::fleet::proposed_fleet_size(&host);
    Json(serde_json::json!({
        "sandbox": sandbox,
        "why": skein::sbx::fleet_failure(),
        "lifecycle_refusal": refusal,
        "host": host,
        "proposed": proposed,
    }))
}

/// Create a fleet sandbox, at the size in the request.
///
/// **No cockpit surface calls this any more, and it is kept deliberately** (SKEIN-627). The
/// create-fleet dialog was its only caller, and the dialog is deleted because the state that opened
/// it — `exists === false` — cannot arise for a skein running inside its own fleet. What is deleted
/// with it is creating *this* fleet, which was always the impossible one. Creating a
/// **differently-named** sandbox is still a coherent act, the warden is still on the host with the
/// capability, and `fleet::request_fleet_create` still carries the attempt lease it needs — so the
/// route stays rather than being rebuilt the day something wants to reach it.
///
/// The numbers are saved before the create, not after: `create_argv` and `create_env` read the
/// config, so a size that was only passed here would be ignored by the very command it is for. It
/// also means the sandbox and the settings agree afterwards, which is what a later resize starts
/// from.
///
/// **In-fleet this asks the warden rather than refusing** (SKEIN-576). It used to refuse before the
/// config write, on the reasoning that fleet lifecycle lives on the host — but §7.5's argument is
/// about where the *doer* runs, and it never said who may ask. §2.3 already has `http` reaching
/// "GitHub, and the warden", and the warden is on the host with the capability. So this is the
/// explicit act a person initiates, and `fleet::request_fleet_create` is what puts it.
///
/// What is NOT restored is a fallback: with no warden reachable this refuses and prints the line,
/// exactly as `docs/delivery.md:143` requires. Nothing here runs `sbx`, in either deployment.
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
        // **The SERVING mount set, which is what `create_line` hands the same act** (SKEIN-678).
        // The two differ by one entry — the volume root — and it is the entry the install cannot
        // start without: `bootstrap.sh` finds the volume by scanning mountinfo for a mount point
        // ending in `/.skein`, and refuses rather than guessing when it finds none. `fleet_mounts`
        // contributes only the two directories *beneath* the volume, so a fleet created from here
        // came up with nothing for that scan to find and stopped at the refusal. sbx fixes mounts
        // at create and no verb adds one afterwards, so the sandbox had to be destroyed and remade.
        skein::fleet::request_fleet_create(&sandbox, &skein::fleet::fleet_serve_mounts())
            .map(|said| (sandbox, said))
    })
    .await;
    match out {
        Ok(Ok((sandbox, said))) => {
            Json(serde_json::json!({ "sandbox": sandbox, "said": said })).into_response()
        }
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
/// It used to answer 200 with the boxes that failed to come back. There is no such list any more:
/// nothing brings a box back from here, because the destroy takes this process with it
/// (SKEIN-679), so the phases that would have produced one are gone from
/// [`skein::fleet::resize_fleet`].
///
/// Refused in-fleet, and it is the sharpest case of the two: a rebuild is a destroy followed by a
/// create, the destroy is the half that cannot be undone, and it is the half that would succeed.
async fn api_fleet_resize(Json(r): Json<ResizeReq>) -> Response {
    if let Some(why) = skein::fleet::fleet_lifecycle_refusal("rebuild", true) {
        return (StatusCode::CONFLICT, why).into_response();
    }
    match skein::fleet::resize_fleet(&r.memory, &r.cpus, &r.disk, r.drop_docker) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Copy every box's work out of the sandbox and onto the host, and say where each one went.
///
/// **The one route on this pane that does something rather than refusing it** (SKEIN-680). Its
/// neighbours — create and resize — are lifecycle, which §7.5 puts outside the fleet permanently. A
/// save is not lifecycle: it reads the boxes and writes the host, destroys nothing, stops nothing,
/// and needs no box to be idle, so it is exactly the act this deployment *can* offer, and the
/// button for it is what makes the refusal beside it something other than a dead end.
///
/// **A partial save answers 200, and that is deliberate.** The body carries one entry per box with
/// its own error, because the box that failed is the one whose work is still only inside the
/// sandbox — a 500 would throw away the report naming it, along with the paths of every box that
/// did make it out. What a 500 means here is the whole act refusing before it wrote anything: no
/// census, no room on the host, a name that is not a box.
///
/// Blocking rather than an Act (§2.5), like the resize it replaces the first third of. A save is
/// minutes of `tar` and its transcript is four lines, not a build log; what a person waits for is
/// the report, which is the response.
async fn api_fleet_save() -> Response {
    match tokio::task::spawn_blocking(|| skein::fleet::save_boxes(&[])).await {
        Ok(Ok(boxes)) => Json(serde_json::json!({ "boxes": boxes })).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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
/// It answers with its **own clock** beside the path, and that is not telemetry — it is the one
/// thing that tells the two halves of a slow attach apart (SKEIN-269). The browser can measure only
/// click-to-answer, which counts the time a request spent queued in the browser's own connection
/// pool *before* it was ever sent; `ms.total` counts from this handler starting. A big wait with a
/// small `total` happened before the request reached skein; a `total` that fills the wait is the
/// box. Guessing between those two, from five drop directories and no numbers, is exactly what this
/// endpoint left the reader to do.
///
/// The phases are named rather than summed because they fail differently: `chose` is the round trip
/// that picks the channel, `body` is the transfer, `verdict` is waiting for the box to say the file
/// is written.
async fn api_upload(
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> Json<serde_json::Value> {
    let mut clock = UploadClock::start();
    match stream_upload(&name, &headers, body, &mut clock).await {
        Ok(path) => serde_json::json!({ "ok": true, "path": path, "ms": clock.ms() }).into(),
        // The timings ride the failure too: a refusal after eight minutes and one after eight
        // milliseconds are different bugs, and an `error` alone reports them identically.
        Err(e) => serde_json::json!({ "ok": false, "error": e, "ms": clock.ms() }).into(),
    }
}

/// What an upload spent, phase by phase, on the host's clock.
///
/// Kept by the caller and filled in as the phases end, so a failure still carries the phases that
/// completed — a struct built at the end would have nothing to say about the upload that did not
/// reach one.
struct UploadClock {
    began: std::time::Instant,
    at: std::time::Instant,
    chose: u128,
    body: u128,
    verdict: u128,
}

impl UploadClock {
    fn start() -> Self {
        let now = std::time::Instant::now();
        Self {
            began: now,
            at: now,
            chose: 0,
            body: 0,
            verdict: 0,
        }
    }
    /// End the phase that was running and start the next. Returns the millis it took.
    fn lap(&mut self) -> u128 {
        let now = std::time::Instant::now();
        let ms = now.duration_since(self.at).as_millis();
        self.at = now;
        ms
    }
    fn ms(&self) -> serde_json::Value {
        serde_json::json!({
            "chose": self.chose,
            "body": self.body,
            "verdict": self.verdict,
            "total": self.began.elapsed().as_millis(),
        })
    }
}

/// Per-attachment ceiling. Streaming means the *host* never buffers the upload, but the box's /tmp is
/// finite — this keeps a runaway (or fat-fingered) upload from filling the sandbox's disk.
const UPLOAD_CAP: u64 = 2 * 1024 * 1024 * 1024;

/// The stall budget as the reader would say it. Seconds read better and are what the deadline is
/// set in — but a test shortens it to milliseconds, and "nothing moved for 0s" is a sentence that
/// says the deadline is broken rather than that it fired.
fn stall_word() -> String {
    let d = upload_stall();
    match d.as_secs() {
        0 => format!("{}ms", d.as_millis()),
        n => format!("{n}s"),
    }
}

/// How long any one step of an upload may make **no progress** before it is a stall and says so.
///
/// It bounds **silence**, not the transfer: a `cat` that has stopped consuming, or a child that
/// will not exit. There used to be a whole-transfer budget beside it — an hour, because a 900 MB
/// video over a slow link legitimately takes most of one — and it went with the agent, which was
/// the only path that could be waiting on a socket rather than on a pipe. The two were one number
/// once, and under one number those are the same picture:
/// SKEIN-269's five uploads sat for minutes and the reader was told nothing, because nothing on
/// either side of the wire distinguished "still coming" from "never coming".
///
/// A minute rather than seconds, because the thing on the other end may be legitimately busy, and
/// waiting is only wrong when nothing is moving.
///
/// A function and not a `const` so `$SKEIN_UPLOAD_STALL_MS` can shorten it, which is what lets a
/// test drive a real stall against a real box in under a second instead of waiting a minute for the
/// deadline it is checking. Same shape as `knock::grace`; a value that does not parse, or is zero,
/// is the default rather than an error, because a mistyped knob must not disable a deadline.
fn upload_stall() -> Duration {
    let asked = std::env::var("SKEIN_UPLOAD_STALL_MS").ok();
    match asked.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => Duration::from_secs(60),
    }
}

/// Where an upload's bytes go: a streamed write into the box.
///
/// **One channel, where there used to be two.** The other was the in-sandbox agent's connection,
/// chosen for a declared length under a cap; it existed because the spawned path crossed a
/// host-to-guest hop that could stall, and the agent was the thing built to survive that. The hop
/// and the agent are gone (SKEIN-521), and this is the path that never had a ceiling: it streams
/// from a pipe, so neither this process nor the box holds the whole file.
struct Sink {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
}

impl Sink {
    /// Start the write, and hold the pipe into it.
    ///
    /// **A function rather than four lines inside [`stream_upload`]** so that a test can drive a
    /// real [`Self::abandon`] against the spawn production uses. The alternative is a test that
    /// builds its own `Command`, which would be asserting on its own stdio and its own process
    /// group — green whatever this function does.
    fn open(argv: &[String]) -> Result<Sink, String> {
        let mut child = tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            // So that giving up on it is giving up on it. `Sink::finish` stops waiting after
            // `upload_stall()`, and without this the abandoned child would go on running — still
            // holding the box's end of a file nobody is going to be told about, and still costing a
            // process per attempt, which the reader's five retries would have made five
            // (SKEIN-269).
            .kill_on_drop(true)
            // Its own process group, which is what makes [`Sink::abandon`] end the WRITE rather
            // than the process this side holds a handle on. Nothing here is a leaf program: the
            // argv is a crossing into the box, and the shell it lands in is `dash`, which FORKS a
            // `-c` command rather than exec'ing it — so the `cat` taking the upload is already a
            // grandchild. `kill_on_drop` and `Child::kill` both reach the pid and only the pid, so
            // an abandoned upload used to leave that `cat` holding the box's end of a half-written
            // file, with `ppid` 1 and nothing that would reap it (SKEIN-912, SKEIN-916).
            //
            // **The trade-off `skein::util::run_bounded` states does not arrive here**, and that
            // is a property of this process rather than a claim about groups: a `skein-server` is
            // not attached to a terminal and `main` says why it installs no `SIGINT` handler, so
            // there is no Ctrl-C for this child to have stopped receiving. What ends it is the
            // deadline, and now the deadline ends all of it.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("the write into the box could not be started: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin pipe")?;
        Ok(Sink { child, stdin })
    }

    async fn push(&mut self, chunk: &[u8]) -> Result<(), String> {
        use tokio::io::AsyncWriteExt as _;
        // Bounded, where it used to be unbounded: stdin is a pipe into a process that may have
        // stopped reading, and an `await` on that with no deadline parks this request for as long
        // as the process lives. The agent path had an hour; this one had nothing at all, which is
        // the worse half of SKEIN-269's host side.
        match tokio::time::timeout(upload_stall(), self.stdin.write_all(chunk)).await {
            Ok(r) => r.map_err(|e| format!("writing file to box: {e}")),
            Err(_) => Err(format!(
                "the box stopped taking the file — nothing moved for {}",
                stall_word()
            )),
        }
    }

    async fn finish(self) -> Result<(), String> {
        use tokio::io::AsyncWriteExt as _;
        // The verdict below is a moment away or is never coming: the body is already through and
        // `cat` exits on EOF. So it waits the stall budget rather than a whole-transfer one — a
        // write that will not exit used to hold the request with no deadline at all, and the reader
        // saw "uploading…" for as long as that lasted (SKEIN-269).
        let Sink { child, stdin } = self;
        let mut stdin = stdin;
        stdin.shutdown().await.ok();
        drop(stdin); // EOF for `cat`
        let out = match tokio::time::timeout(upload_stall(), child.wait_with_output()).await {
            Ok(r) => r.map_err(|e| format!("the write into the box failed: {e}"))?,
            Err(_) => {
                return Err(format!(
                    "the box never confirmed the file — the write did not finish within {}",
                    stall_word()
                ))
            }
        };
        if out.status.success() {
            return Ok(());
        }
        Err(format!(
            "the write into the box failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }

    /// Give up, leaving nothing running. The partial file is removed by the caller either way.
    ///
    /// **The group, not the pid.** `Child::kill` signals the process this side recorded, which is
    /// the crossing and not the `cat` under it — see the `process_group(0)` on the spawn. The
    /// negative `kill` here is the same move `skein::util::end_group` makes for the blocking
    /// sites, written out because that one takes a `std::process::Child` and this is a tokio one;
    /// the `kill().await` after it is what reaps the leader, so an abandoned upload leaves no
    /// zombie either.
    async fn abandon(self) {
        let Sink { mut child, stdin } = self;
        drop(stdin);
        // `id()` is `Some` only while the child is unreaped, and an unreaped pid cannot have been
        // handed to anybody else — so the group named here is this child's own and can be no
        // stranger's. That is the same invariant `skein::util::end_group`'s SAFETY note states.
        if let Some(pid) = child.id() {
            // SAFETY: `kill` has no memory effects, and `-pid` names the group led by a child this
            // process spawned with `process_group(0)` and has not reaped. A failure means the group
            // is already empty, which is the outcome being asked for.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        }
        let _ = child.kill().await;
    }
}

async fn stream_upload(
    name: &str,
    headers: &axum::http::HeaderMap,
    body: axum::body::Body,
    clock: &mut UploadClock,
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

    // There is no channel to choose any more: one write, streamed. The choice used to be made here
    // — before a single byte was read, on the DECLARED length rather than the real one, because an
    // upload is read off a network socket exactly once and by the time the truth is known there is
    // no second copy to fall back with. With one channel that ordering has nothing left to decide.
    //
    // The argv carries its own program: where the box lives decides that too.
    let argv = skein::sandbox::box_write_argv(name, &dir, &path)?;
    let mut sink = Sink::open(&argv)?;
    // The channel is chosen; everything above is `chose`. It is its own phase because it is the one
    // that happens before a byte of the body is read, and therefore the one a reader watching an
    // upload bar would see as nothing happening at all.
    clock.chose = clock.lap();
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
    clock.body = clock.lap();
    if let Some(e) = failed {
        sink.abandon().await;
        discard_partial(name, &path).await;
        return Err(e);
    }
    let said = sink.finish().await;
    clock.verdict = clock.lap();
    said?;
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
    // **Readings ride the same connection** (SKEIN-366). They are not fleet ticks and are not a
    // `Tick` — a reading is a fact about a pull request, and the fleet producer stops when nobody
    // is watching the board, which a reading must not depend on. So they are their own channel,
    // merged onto this one stream: the page holds ONE `EventSource`, and the number of connections
    // it spends does not grow with the number of readings it has in flight.
    let readings =
        tokio_stream::wrappers::BroadcastStream::new(skein::review::subscribe_readings())
            .filter_map(|item| match item {
                Ok(done) => Some(Ok(Event::default()
                    .event("reading")
                    .data(serde_json::to_string(&done).unwrap_or_else(|_| "{}".into())))),
                // Dropped rather than reported as a hole. A board that fell behind on readings cannot
                // repair itself from a count the way it can from a snapshot, and the page has its own
                // recovery for a reading that never arrived: the in-flight poll finds the read no
                // longer running and picks the answer up off disk.
                Err(_) => None,
            });
    Sse::new(tokio_stream::once(sse(&snapshot)).chain(following.merge(readings))).into_response()
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
        // Not a fleet fact and not on the producer's clock: a terminal slot was released, and a pane
        // that was refused one is watching this stream for exactly that (SKEIN-702).
        skein::stream::Tick::PtyFreed => "pty-freed",
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
    // hung browser can't silently stall new terminals. Dropped on every return → released, and the
    // release says so on the board's stream ([`PtySlot`]) so a pane refused here can come back
    // without being clicked.
    let _permit = match PTY_LIMIT.try_acquire() {
        Ok(p) => PtySlot(Some(p)),
        Err(_) => {
            refuse(&mut socket, pty_limit_reached(), AFTER_WAIT_PTY).await;
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
            // `why` is the best recovery sentence skein has — it names the command to run and the
            // command NOT to run. What it could not say is that nobody has to come back here
            // afterwards, so that is added rather than left to be guessed at (SKEIN-702).
            refuse(
                &mut socket,
                format!("skein: {why}{}", watching_for_box(&name)),
                AFTER_WAIT_BOX,
            )
            .await;
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

    let launching = launch.is_some();
    let code = pump_pty(&mut socket, cmd).await;
    // The one failure a box start cannot record for itself: `skein` never ran, so nothing inside it
    // wrote `starts/<box>.err`, and the reconnect that follows this PTY closing was told "There is
    // no record of a start having been attempted" — about a launch someone had just pressed a button
    // for (SKEIN-589). Kept here, and said here, because the shell's own `not found` scrolls away
    // with the terminal that carried it.
    if launching {
        if let Some(code) = code {
            let boxed = name.clone();
            if let Ok(Some(why)) = tokio::task::spawn_blocking(move || {
                skein::sandbox::remember_launch_never_ran(&boxed, code)
            })
            .await
            {
                let _ = socket
                    .send(Message::Text(format!("\r\nskein: {why}\r\n")))
                    .await;
            }
        }
    }
    // Say which of the two ways this session could have ended actually ended it, because the browser
    // cannot tell and the answer decides what it draws over the last thing on screen (SKEIN-672).
    // [`pump_pty`] returns a code only for a child that exited under a live socket, so this is
    // exactly "the command is over"; every other way out leaves without it and reads as the
    // connection having gone away.
    if let Some(code) = code {
        close_saying(
            &mut socket,
            AFTER_CHILD_ENDED,
            &format!("the command exited {code}"),
        )
        .await;
    }
}

/// What a person sees when every terminal slot is taken, in both places it can happen.
///
/// **It quotes [`PTY_MAX`] and it promises the pane comes back on its own**, and both halves are the
/// point (SKEIN-702). The old sentence — "too many terminals open — close one and retry" — named
/// neither the rule that had been hit nor who was retrying, so somebody who closed a pane was left
/// to work out that they now had to find and press something. They do not: the release publishes
/// `Tick::PtyFreed` and the pane reconnects itself. Saying so is what stops them waiting for nothing
/// or clicking for no reason.
fn pty_limit_reached() -> String {
    format!(
        "skein: too many terminals open — {PTY_MAX} at once is the limit. Close another terminal \
         and this one reopens on its own; there is nothing else to do.\r\n"
    )
}

/// The line added under `absent_box_reason`'s refusal: skein is watching, so nobody need come back.
fn watching_for_box(name: &str) -> String {
    format!(
        "skein is watching the board for {name}, and reopens this terminal on its own once the box \
         is there.\r\n"
    )
}

/// What a person sees when the bridge itself could not be built: **skein's own failure, said as
/// one**, and the one class of refusal with nothing to watch for.
///
/// Three parts, and each is load-bearing (SKEIN-702). `what` is what broke, in skein's words rather
/// than only the OS error, because "pty reader: Too many open files" tells somebody nothing about
/// whose fault it is. **"not anything you did" is part of the next step, not sympathy** — without it
/// the reader goes looking through their own box for the cause, which is the most expensive way
/// there is to make no progress. And `likely` names the thing that can actually be checked.
///
/// It also says that nothing is coming: these four have no condition anywhere that clears them, so
/// the pane offers "Try again" rather than a spinner, and the sentence agrees with the button.
fn skeins_own_fault(what: &str, error: &str, likely: &str) -> String {
    format!(
        "skein: {what}: {error}\r\nThis is skein's own failure, not anything you did — nothing in \
         your box is wrong. There is no condition here for skein to wait on, so nothing will reopen \
         this terminal by itself: use Try again below. If it keeps failing, {likely}.\r\n"
    )
}

/// Wait to be told the close was read, instead of hanging up on the sentence.
///
/// **A close frame is not delivered by having been written.** Returning from here drops the socket,
/// and a socket dropped while bytes it never read are still queued on it is closed by the kernel
/// with RST rather than FIN — which discards whatever the PEER had queued and not yet read. So the
/// close code is destroyed by the same reset that ends the connection, and the browser reports 1006:
/// a launch that ran to completion, read as a connection that went away, with the reconnect panel
/// back over the one line saying what happened (SKEIN-746, the defect SKEIN-672 removed).
///
/// The cockpit supplies both halves by itself. It writes on that socket without being asked —
/// `sendResize` fires off `requestAnimationFrame` and off xterm's own `onResize`, neither of which
/// is timed by anything here — while [`pump_pty`] stops reading the instant the child's PTY closes,
/// so anything arriving between that instant and this one is never read. And a page whose renderer
/// is short of CPU is exactly the page that has not yet read what it was sent. Measured with a
/// client that stops reading and keeps writing resizes: 30 of 30 sessions lost the close code, the
/// shell's error and skein's recorded reason, all three, and reported ECONNRESET instead.
///
/// Reading until the peer's own close empties that queue, and is the closing handshake RFC 6455
/// §7.1.4 describes. Its arrival is also the only proof the code was read, which is why this waits
/// for that rather than for a fixed moment. Bounded, because a peer that answers nothing must not
/// hold its PTY permit for ever, and generous, because the browser this is for is a slow one.
const CLOSE_ACK_WAIT: Duration = Duration::from_secs(5);

async fn hang_up(socket: &mut WebSocket) {
    let _ = tokio::time::timeout(CLOSE_ACK_WAIT, async {
        while let Some(Ok(msg)) = socket.recv().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// The close code skein's own end puts on a terminal socket when **there is nothing here to
/// reconnect to** — as against every other way a socket ends, which is the connection going away.
///
/// The cockpit draws a "session not connected · click to reconnect" panel over a terminal whose
/// socket has closed. Over a launch that ran to completion that panel covers the one line saying
/// what happened, and offers to reconnect to something that no longer exists — which is the whole of
/// SKEIN-672. `ws.onclose` cannot draw the distinction on its own: a child that exited and a
/// connection that dropped arrive at the browser identically. So the side that knows says which.
///
/// **It was `CLOSE_CHILD_ENDED`, and the rename is the point rather than tidying** (SKEIN-702). The
/// same panel went up over every *refusal* too — "too many terminals open", "box … does not exist"
/// — because those closed with no code at all and the browser read 1006. Sending them this code is
/// what uncovers their sentences, and the moment a refusal carries it "the child ended" is false:
/// a refusal has no child, and nothing ended. The page's question is not *what happened* but
/// **whether to offer a reconnect**, and a finished child and a refused start are the same answer to
/// it. One code, saying that one thing. Leaving the old name while widening what it covers is
/// SKEIN-748's defect exactly: a sentence true when it was written and quietly false afterwards.
///
/// **4000-4999 is the only range an application may define**, per RFC 6455 §7.4.2: 0-999 is unused,
/// 1000-2999 belongs to the protocol and to IANA, and 3000-3999 is for libraries registered with
/// IANA. A code from any of those would be either a lie about a protocol condition or a claim on
/// somebody else's registration. A browser reports 1006 for a connection that simply died and never
/// a 4xxx, so the ABSENCE of this code is what "the connection went away" is read from — which is
/// the direction that matters, because a close nobody wrote is the common one.
///
/// The reason beside it is one of the `AFTER_*` words below — the first word says what the pane
/// should wait for, and anything after it is for a person reading a trace.
const CLOSE_NOTHING_TO_RECONNECT: u16 = 4001;

/// What a pane should do next, written as the FIRST WORD of the close reason.
///
/// **The code and this answer two different questions, and both had to be answered.**
/// [`CLOSE_NOTHING_TO_RECONNECT`] answers "offer a reconnect?", and one code covers every way in
/// because the answer is always no. What is left is what the pane should do *instead*, and there
/// the paths genuinely differ (SKEIN-702): a terminal refused for the cap is waiting for a slot, one
/// refused for a missing box is waiting for the box, and one that fell over inside `pump_pty` is
/// waiting for nothing at all and must offer the control rather than pretend. **A pane that is
/// watching says what for; a pane that is watching nothing offers a button. A spinner that waits for
/// nothing is worse than a button**, so this is the field that stops the page inventing either one.
///
/// **A first word rather than the whole reason**, so the trace text a person reads in devtools can
/// keep following it — `child-ended the command exited 127`. The page splits on the first space; a
/// close reason is capped at 123 bytes, which none of these approaches.
///
/// This is not "branching on free text", which the old doc comment rightly refused. The token is
/// written here, read in `ws.onclose`, and is as much of the contract as the number above it — the
/// alternative was one close code per condition, which is three numbers agreeing about the thing the
/// number was deliberately not made to carry.
/// The command ran and is over. Nothing to wait for and nothing to retry — it finished.
const AFTER_CHILD_ENDED: &str = "child-ended";

/// Refused for the terminal cap. Wait for `skein::stream::Tick::PtyFreed` on the board's stream.
const AFTER_WAIT_PTY: &str = "wait-pty";

/// Refused because skein has no placement for the box. Wait for the box on the board's stream.
const AFTER_WAIT_BOX: &str = "wait-box";

/// Skein's own failure, with no condition anywhere that clears it. Offer the control.
const AFTER_NO_WATCH: &str = "no-watch";

/// Close a terminal socket the way [`CLOSE_NOTHING_TO_RECONNECT`] describes, and wait to be told the
/// close was read.
///
/// One helper rather than the same four lines at nine sites, because the four lines are the part
/// that was missing: **every one of these paths used to write its sentence and return**, and
/// returning drops the socket, which destroys both the sentence and the code (see [`hang_up`]).
async fn close_saying(socket: &mut WebSocket, after: &str, trace: &str) {
    let reason = match trace.is_empty() {
        true => after.to_string(),
        false => format!("{after} {trace}"),
    };
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: CLOSE_NOTHING_TO_RECONNECT,
            reason: reason.into(),
        })))
        .await;
    hang_up(socket).await;
}

/// Refuse a terminal: say what happened and what to do about it, say there is nothing to reconnect
/// to, say what the pane should wait for, and stay until that has been read.
async fn refuse(socket: &mut WebSocket, sentence: String, after: &str) {
    let _ = socket.send(Message::Text(sentence)).await;
    close_saying(socket, after, "").await;
}

/// The WS↔PTY byte pump shared by the box terminal ([`terminal_session`]) and the login terminal
/// ([`login_session`]): open a fresh PTY, spawn `cmd` on it, pipe bytes both ways, honour
/// `{"resize":…}` frames, ping every 30s, and reap the child on the way out.
///
/// Returns the child's exit code when the CHILD ended the session — PTY EOF with the socket still
/// up — and `None` when the socket went first or the bridge never got started. **In the
/// never-started case this has already refused and closed the socket itself** ([`refuse`], with
/// [`AFTER_NO_WATCH`]): its four failures are skein's own, they are the same four wherever the pump
/// is used, and the caller cannot say anything more useful about them than the pump can. So a caller
/// seeing `None` has nothing left to write and no socket to write it on. A socket that drops mid-session kills the child, which is exactly
/// what the login flow wants: a login is a one-shot flow, not a tmux-backed session to resume, so
/// an abandoned OAuth prompt dies with its browser tab instead of waiting forever for input nobody
/// can give it.
async fn pump_pty(socket: &mut WebSocket, cmd: CommandBuilder) -> Option<u32> {
    let pair = match native_pty_system().openpty(PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            refuse(
                socket,
                skeins_own_fault(
                    "could not open a terminal device",
                    &e.to_string(),
                    "the machine skein is running on has run out of pseudo-terminals",
                ),
                AFTER_NO_WATCH,
            )
            .await;
            return None;
        }
    };

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            refuse(
                socket,
                skeins_own_fault(
                    "could not start the program behind this terminal",
                    &e.to_string(),
                    "the program named in the error is missing from where skein is running",
                ),
                AFTER_NO_WATCH,
            )
            .await;
            return None;
        }
    };
    drop(pair.slave); // release the slave fd in the parent so EOF propagates on child exit

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            refuse(
                socket,
                skeins_own_fault(
                    "opened this terminal and then could not read from it",
                    &e.to_string(),
                    "skein is out of file descriptors, and restarting the server clears that",
                ),
                AFTER_NO_WATCH,
            )
            .await;
            return None;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            refuse(
                socket,
                skeins_own_fault(
                    "opened this terminal and then could not write to it",
                    &e.to_string(),
                    "skein is out of file descriptors, and restarting the server clears that",
                ),
                AFTER_NO_WATCH,
            )
            .await;
            return None;
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

    // Channel → PTY input (blocking write on a thread), and the channel is UNBOUNDED on purpose.
    //
    // **A bounded one puts the child's stdin in charge of the whole bridge** (SKEIN-750). Writing to
    // a PTY master whose child is not reading blocks after a few kilobytes — 20480 bytes in raw
    // mode, 8960 in canonical mode with a newline in the input, on one measurement of each, which is
    // an agent mid-turn, a full-screen TUI, or a paste into a `sleep`. (Canonical mode with no
    // newline never blocks at all: the discipline discards past its buffer instead. The probe those
    // three readings came from is in `tests/ui/ptystall.mjs`, which drives the reachable one through
    // this pump.) A blocked write stops `in_rx` draining, a bounded channel then fills, and the forward
    // below used to be `in_tx.send(b).await` *inside* a `tokio::select!` branch. A `select!` polls
    // nothing while a branch's handler is awaiting, so PTY output stopped reaching the browser, the
    // keepalive stopped and resize frames stopped being honoured — all three at once, for as long as
    // the child ignored its stdin, and nothing on screen could say why.
    //
    // The other repair was `try_send` and a sentence when the queue is full, and dropping is worse
    // here than dropping usually is: this is a **byte stream into a shell**, so half of
    // `rm -rf /tmp/scratch` is still a command that runs, and a notice afterwards does not unrun it.
    // Order and completeness are the contract; the pane going quiet is the symptom, not the trade.
    //
    // What bounds it in practice is the sender. These bytes reached us over a socket the browser had
    // to hold them in memory to write — `flushAttach` sends one array it has already built — so the
    // queue can only mirror what the page was already carrying, and it drains the instant the child
    // reads. A queue whose producer is bounded is not the same thing as an unbounded queue.
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
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

    // Who ended the bridge decides what the caller may say afterwards: only a child that exited
    // under a still-open socket has an exit code worth reporting to anyone.
    let mut child_ended = false;
    loop {
        tokio::select! {
            out = out_rx.recv() => match out {
                Some(bytes) => {
                    if socket.send(Message::Binary(bytes)).await.is_err() { break; }
                }
                None => { child_ended = true; break; } // PTY closed (child exited)
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    // Never `.await` here. This is the branch handler of the `select!` above, and
                    // an await in it is an await with nothing else being polled (SKEIN-750).
                    let _ = in_tx.send(b);
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

    // Reap the child so it doesn't linger as a zombie. Kill (a no-op for one that already exited),
    // then wait off the async runtime (Child::wait blocks); dropping `master`/the reader closes the
    // PTY so descendants get SIGHUP.
    let _ = child.kill();
    let status = tokio::task::spawn_blocking(move || child.wait()).await;
    match (child_ended, status) {
        (true, Ok(Ok(st))) => Some(st.exit_code()),
        _ => None,
    }
}

/// Update the agent CLIs every box in this fleet shares.
///
/// **Blocking on purpose, unlike the check behind it.** `fleet::runtime_updates` must never make
/// the board wait; this is somebody pressing a button and watching for the answer, so it says what
/// moved rather than returning immediately and leaving them to guess. It is an npm install, so it
/// is slow — the page says so before it starts.
async fn api_update_agents() -> Json<serde_json::Value> {
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
async fn api_update() -> Json<serde_json::Value> {
    let found = tokio::task::spawn_blocking(|| {
        // The token is looked up here rather than inside `update`, so that module needs no opinion
        // about credentials. Absent is fine and common: the repository is public, and an update
        // check that refused without a login would be a check nobody on a fresh fleet ever gets.
        let token = skein::prq::host_token().ok();
        (
            skein::update::available(token),
            skein::fleet::runtime_updates(),
            skein::update::running(&skein::place::fleet_sandbox()),
        )
    })
    .await;
    Json(match found {
        Ok((skein, runtimes, running)) => serde_json::json!({
            "skein": skein,
            "runtimes": runtimes,
            "running": running,
        }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    })
}

/// Start the update, and answer at once. What it is doing comes back on `/api/update/log`.
///
/// It ends by replacing this process, so there is nothing useful to await: a handler that held the
/// request would be a handler whose reply is written by a binary that no longer exists.
async fn api_update_start() -> Json<serde_json::Value> {
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
async fn api_update_log(
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
struct UpdateLogQuery {
    from: Option<u64>,
}

/// Upgrade to a WebSocket that runs the interactive runtime login — the same flow `skein login`
/// attaches to a terminal, on a PTY the cockpit owns. The UI half opens this when the fleet's
/// credential expires (`/api/health` → `expired_logins`), so repair is a click rather than a shell.
async fn login_terminal(
    ws: WebSocketUpgrade,
    Path(runtime): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin terminal blocked").into_response();
    }
    if !skein::runtime::valid_runtime(&runtime) {
        return (StatusCode::BAD_REQUEST, "unsupported runtime").into_response();
    }
    ws.on_upgrade(move |socket| login_session(socket, runtime))
}

/// The login half of the WS↔PTY bridge; [`pump_pty`] is shared with [`terminal_session`].
///
/// Deliberately NOT tmux-backed, unlike the box terminal: a login is a one-shot flow, and a socket
/// that drops mid-login should kill it — resuming a half-finished OAuth prompt in a session nobody
/// is attached to helps no one, and the next click simply starts a fresh one.
///
/// On exit 0 the post-login tail (`fleet::after_login`) runs HERE, in the server process — the CLI
/// path clears the refusal memory of the CLI process, which the long-running server never sees.
/// The sentences it returns go down the socket, and the socket closing is the UI's completion
/// signal either way.
async fn login_session(mut socket: WebSocket, runtime: String) {
    // Same cap as the box terminals: a login PTY is a PTY.
    //
    // **`AFTER_NO_WATCH` here, and `AFTER_WAIT_PTY` for a box terminal, for the same condition.**
    // The token says what the PANE should do, not what the server knows, and this pane cannot wait:
    // the login surface is a modal that closes with its socket, and one that reopened itself over
    // whatever somebody had moved on to would be worse than the walk back. So it names the control
    // that is still on screen behind it, which is the "log in" button they just pressed.
    let _permit = match PTY_LIMIT.try_acquire() {
        Ok(p) => PtySlot(Some(p)),
        Err(_) => {
            refuse(
                &mut socket,
                format!(
                    "skein: too many terminals open — {PTY_MAX} at once is the limit. Close a \
                     terminal and press log in again.\r\n"
                ),
                AFTER_NO_WATCH,
            )
            .await;
            return;
        }
    };
    // Infallible, and it used to be a `match` with a refusal arm (SKEIN-774). The only `Err`
    // `login_spawn_argv` could ever return was "no fleet sandbox configured", and `load_config`
    // repairs a blank `fleet_sandbox` before anybody reads it, so no value could reach that arm —
    // its sentence, its close code and its "press log in again" line were work spent on a pane
    // nobody can be shown. `docs/recovery-survey.md` §5 records it as GONE rather than fixed.
    let (program, argv) = skein::fleet::login_spawn_argv(&runtime);
    if runtime == "claude" {
        // The same coaching `skein login` prints: claude has no login subcommand, so the flow is
        // the TUI plus a slash command, and nothing on screen says so.
        let _ = socket
            .send(Message::Text(
                "skein: type /login once it starts, then /exit — `setup-token` returns a token to \
                 export and leaves no credential to seed boxes with\r\n"
                    .into(),
            ))
            .await;
    }
    let mut cmd = CommandBuilder::new(program);
    for a in &argv {
        cmd.arg(a);
    }
    // Propagate env so `sbx`/`bash` resolve on PATH, exactly as the box terminal does.
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    match pump_pty(&mut socket, cmd).await {
        Some(0) => {
            let rt = runtime.clone();
            let said = tokio::task::spawn_blocking(move || skein::fleet::after_login(&rt))
                .await
                .unwrap_or_else(|e| {
                    vec![format!("logged in, but the post-login share failed: {e}")]
                });
            for line in said {
                let _ = socket
                    .send(Message::Text(format!("skein: {line}\r\n")))
                    .await;
            }
        }
        Some(code) => {
            let _ = socket
                .send(Message::Text(format!(
                    "skein: login exited {code} — nothing changed\r\n"
                )))
                .await;
        }
        // The browser went first, or the pump refused and closed the socket saying so. Either way
        // there is nobody to tell and nothing left to tell them on.
        None => return,
    }
    // **The close is written and waited for, rather than left to the drop** (SKEIN-746). Every
    // sentence above is the last thing this flow says — `after_login`'s account of what was shared
    // with which boxes, or the exit code of an abandoned login — and the overlay toasts the last of
    // them. Returning here would drop the socket with the browser's own resizes still unread on it,
    // and a socket dropped on unread bytes is reset rather than closed, which discards the peer's
    // queue: the toast then says "nothing changed" about a login that worked.
    close_saying(&mut socket, AFTER_CHILD_ENDED, "the login flow is over").await;
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
mod tests {
    use super::{flag, note_is_for_this_repo, origin_ok, refuse_unknown_args, slow_down};
    use axum::http::{header::ORIGIN, HeaderMap, HeaderValue};
    use std::time::Duration;

    /// **Neither lifecycle route can do its work by a path that skips the check**, and this keeps
    /// it so — but the two checks are no longer the same check (SKEIN-576).
    ///
    /// `api_fleet_resize` still asks the deployment first, and that guard is the sharpest in the
    /// file: a rebuild is a destroy followed by a create, the destroy is the half that cannot be
    /// undone, and in-fleet it is the half that would succeed — taking the machine this process is
    /// on with it (SKEIN-467).
    ///
    /// `api_fleet_create` no longer refuses in-fleet, because §7.5 is about where the *doer* runs
    /// and never about who may ask. What replaces the refusal is narrower and stronger: the route
    /// may reach a create **only** through `fleet::request_fleet_create`, which asks the warden and,
    /// with no warden, refuses and prints. So what is asserted here is that it calls that and does
    /// not call `ensure_fleet` — the caller it used to have, which would create as a side effect of
    /// making sure a fleet was ready.
    ///
    /// Read out of the source rather than by calling the handlers, and that is not laziness:
    /// `api_fleet_create` writes the settings and `api_fleet_resize` destroys the sandbox, so a
    /// test that drove the gate through them would — on the day the gate broke, which is the only
    /// day it matters — do the exact irreversible thing it exists to prevent. Same technique as
    /// `cockpit_routes` below, which reads the router out of this file for its own reason.
    #[test]
    fn neither_lifecycle_route_reaches_its_work_by_a_path_that_skips_the_check() {
        // Production code only. This test names both handlers in its own body, and a scan that
        // read itself would find the guard in its own assertion.
        let production: String = include_str!("skein-server.rs")
            .lines()
            .take_while(|l| !l.starts_with("#[cfg(test)]"))
            .collect::<Vec<_>>()
            .join("\n");
        let handler = |name: &str| -> &str {
            production
                .split(name)
                .nth(1)
                .unwrap_or_else(|| panic!("{name} is gone"))
        };

        // The destroy half, unchanged: the deployment is asked before anything is destroyed.
        let resize = handler("async fn api_fleet_resize");
        let guard = resize.find("fleet_lifecycle_refusal(").unwrap_or_else(|| {
            panic!(
                "api_fleet_resize no longer asks where skein is running — in-fleet what it calls \
                 next destroys the machine this process is on (docs/architecture.md §7.5)"
            )
        });
        let doing = resize
            .find("skein::fleet::resize_fleet(")
            .expect("api_fleet_resize no longer resizes");
        assert!(
            guard < doing,
            "api_fleet_resize does its work before it checks the deployment, so the refusal \
             arrives after the damage"
        );

        // The create half: one way in, and it is the one that asks the warden.
        let create = handler("async fn api_fleet_create");
        assert!(
            create.contains("skein::fleet::request_fleet_create("),
            "api_fleet_create reaches a create by some path other than the explicit act, which is \
             the only path that refuses when no warden answers"
        );
        assert!(
            !create.contains("skein::fleet::ensure_fleet("),
            "api_fleet_create is back to creating through `ensure_fleet`, which creates as a side \
             effect of making a fleet ready and has no warden-less refusal of its own"
        );
    }

    /// **A note meant for one repo is not written against another** (SKEIN-427).
    ///
    /// The cockpit is where this went wrong and the cockpit is where it is now stopped, so this
    /// guard is unreachable through the page today. It is here because the page knowing which repo
    /// it is drawing is a thing that can go wrong again — it already did — and a handler holding a
    /// bare path has no way to tell a re-write of `src` from a re-write of somebody else's `src`.
    #[test]
    fn a_note_meant_for_one_repo_is_not_written_against_another() {
        // The ordinary write: the page names the repo it is showing, and it is this one.
        assert!(note_is_for_this_repo("acme", "acme").is_ok());
        // No claim is not a wrong claim. A caller that says nothing about provenance is left alone
        // — `moduledocs::write` still refuses a path that is not one of this repo's modules.
        assert!(note_is_for_this_repo("acme", "").is_ok());
        // Two repos in one request. This is the shape the panel produced: the path came from the
        // repo the reader had been looking at, the URL from the repo they had just switched to.
        let why = note_is_for_this_repo("bravo", "acme").expect_err(
            "a note claiming one repo was accepted against another, which is what this guard is",
        );
        assert!(
            why.contains("acme") && why.contains("bravo"),
            "the refusal has to name BOTH repos — a reader who is told only where it went cannot \
             tell which of their repos the note was about: {why}"
        );
    }

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
        // This binary's own lock, held by three sibling tests. Without it, the window between the
        // set and the remove below is one in which any other test in this binary reads an
        // allowlist it never asked for (SKEIN-307).
        let _env = super::env_lock();
        std::env::set_var("SKEIN_ALLOWED_ORIGINS", "proxy.local, other.host");
        assert!(origin_ok(&with_origin(Some("https://proxy.local"))));
        assert!(!origin_ok(&with_origin(Some("https://nope.local"))));
        std::env::remove_var("SKEIN_ALLOWED_ORIGINS");
    }
}

/// The review pane's routes, driven directly.
///
/// Its own module because these need `$SKEIN_HOME` and a GitHub that is not there, and the asserts
/// above are pure — mixing them would make a pure test's failure depend on an env var somebody
/// else's test set.
#[cfg(test)]
mod review_routes {
    use super::*;

    /// Drives an async body to completion on a runtime of this test's own, from a SYNC test.
    ///
    /// Every test here holds `env_lock()` — a `std::sync::MutexGuard` — for its whole body, because
    /// `SKEIN_HOME` and `GH_TOKEN` are process-wide and `cargo` runs these as threads of one
    /// process (SKEIN-307, and `tools/env-lock-check.py` fails the build without it). Under
    /// `#[tokio::test]` that guard would be held across the body's await points: a blocking lock
    /// owned by a task the executor may park, which is `clippy::await_holding_lock` and a real
    /// deadlock shape once anything else on that runtime wants the same lock.
    ///
    /// Taking the lock in a sync frame and running the futures inside `block_on` keeps exactly the
    /// same guarantee — no other test touches the environment until this one returns — while the
    /// guard never crosses a suspension point: the thread that owns it is the thread driving the
    /// runtime, and it does not go anywhere until the body is done.
    ///
    /// Current-thread and `enable_all` reproduce what `#[tokio::test]` built: the same scheduler,
    /// plus the timer `the_pruning_actually_runs…` sleeps on and the blocking pool `prune_behind`
    /// spawns onto.
    fn on_a_runtime<F: std::future::Future>(body: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime for this test's body")
            .block_on(body)
    }

    /// One home per test function, named after it — `cargo` runs these as threads in one process,
    /// so two tests sharing a directory share `repos.json` and each other's failures.
    fn home_for(what: &str) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!("skein-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("repos.json"),
            format!(
                r#"[{{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"{t}","store":"{s}","agent":"claude","read_prs":false,"plane_project":"","sync_connection":""}}]"#,
                t = home.join("tree").display(),
                s = home.join("store").display()
            ),
        )
        .unwrap();
        home
    }

    /// A queue on disk, exactly where `prq::remembered` reads it — one pull request, at `sha7`.
    fn remember_a_queue(home: &std::path::Path) {
        let dir = home.join("review").join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("queue.json"),
            serde_json::to_vec(&serde_json::json!({
                "repo_id": "demo", "slug": "acme/thing", "viewer": "you", "ai": true,
                "blind_spots": [], "as_of": "2026-08-25T09:00:00Z", "fresh": true,
                "whole": true, "trunk": "main",
                "prs": [{
                    "number": 7, "title": "shorten the timeout", "author": "someone",
                    "url": "https://github.com/acme/thing/pull/7",
                    "head_ref": "timeout", "head_sha": "sha7", "base_ref": "main",
                    "draft": false, "updated_at": "2026-08-25T08:00:00Z",
                    "committed_at": "2026-08-25T08:00:00Z", "checks": "passing",
                    "my_review": "", "review_is_current": false,
                    "reasons": ["reviewer"], "lane": "needs-you", "box_name": "",
                }],
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// A reading already paid for, at the head the remembered queue reports.
    fn remember_a_reading(home: &std::path::Path) {
        let dir = home.join("review").join("demo").join("summaries");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("7-sha7.json"),
            serde_json::to_vec(&serde_json::json!({
                "number": 7, "head_sha": "sha7", "depth": "expanded",
                "line": "the request timeout default drops from 30s to 5s.",
                "detail": "", "flags": [], "yours": [], "others": 0, "signals": [],
                "unread_because": "", "computed": true,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// Point skein at a GitHub that is not there. Port 1 refuses instantly, so a route that goes
    /// looking fails in milliseconds and this test stays fast — what is asserted is WHETHER it
    /// goes, not how long it waits when it does.
    fn no_github(home: &std::path::Path) {
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_GITHUB_API", "http://127.0.0.1:1");
        std::env::set_var("GH_TOKEN", "not-a-real-token");
    }

    fn forget_github(home: &std::path::Path) {
        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "GH_TOKEN"] {
            std::env::remove_var(key);
        }
        let _ = std::fs::remove_dir_all(home);
    }

    async fn read(response: Response) -> (StatusCode, String, String) {
        let status = response.status();
        let queue = response
            .headers()
            .get("x-skein-queue")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = axum::body::to_bytes(response.into_body(), 8 << 20)
            .await
            .unwrap();
        (status, queue, String::from_utf8_lossy(&body).to_string())
    }

    /// **The review pane's answers come from what skein remembers, not from a GitHub refresh the
    /// reader waits on** (SKEIN-291).
    ///
    /// The wait reported as the cockpit hanging — 10.42 s on `/review/summaries` — was
    /// never the payload: warm, the full answer serialises in ~7 ms; cold, with GitHub answering in
    /// three seconds, every shape of it took 3.11 s. It was `prq::queue(&repo, false)` refreshing
    /// past its sixty-second micro-cache, inline, before a byte was written.
    ///
    /// So: a home with a remembered queue and a reading in it, and no GitHub at all. Every route
    /// the pane opens with must still answer, and must say the queue it answered from was a
    /// remembered one.
    #[test]
    fn the_review_pane_answers_with_no_github_to_ask() {
        let _env = super::env_lock();
        on_a_runtime(async {
            let home = home_for("291");
            remember_a_queue(&home);
            remember_a_reading(&home);
            no_github(&home);

            // The bulk payload — the one that was measured at 10.42 s.
            let (status, from, body) =
                read(api_review_summaries(Path("demo".into()), Query(HashMap::new())).await).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "the bulk summaries route went to GitHub for a payload it reads off disk: {body}"
            );
            assert!(
                body.contains("the request timeout default drops"),
                "the reading skein already holds did not come back: {body}"
            );
            assert_eq!(
                from, "remembered",
                "the answer did not say which queue it was built from — a page cannot tell a \
                 confident answer from a blind one (SKEIN-239)"
            );

            // The workflows payload, fetched per repo in the same pane open.
            let (status, from, body) = read(api_workflows(Path("demo".into())).await).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "the workflows route went to GitHub for skein's own answer about the queue: {body}"
            );
            assert!(
                body.contains("\"7\""),
                "the remembered queue's pull request is missing from the workflows payload: {body}"
            );
            assert_eq!(from, "remembered", "the workflows answer did not say so");

            // A row opening: `held=1` is defined as "hand over what is on disk and read nothing".
            let held = HashMap::from([("held".to_string(), "1".to_string())]);
            let (status, from, body) =
                read(api_review_summary(Path(("demo".into(), 7)), Query(held)).await).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "opening a row put a GitHub refresh in front of a disk read: {body}"
            );
            assert!(
                body.contains("the request timeout default drops"),
                "the row opened onto no prose: {body}"
            );
            assert_eq!(from, "remembered", "the row's answer did not say so");

            // The other half of the same route is the control: asking skein to READ this pull request
            // is a model call, and a reading is worth only the commit it was taken of — so that arm
            // still insists on a current queue, and with no GitHub it must fail rather than quietly
            // analyse a head it has not checked.
            let (status, _, _) =
                read(api_review_summary(Path(("demo".into(), 7)), Query(HashMap::new())).await)
                    .await;
            assert_eq!(
                status,
                StatusCode::BAD_GATEWAY,
                "the computing arm answered from a remembered queue — a model call spent against a \
                 head skein has not checked"
            );

            forget_github(&home);
        });
    }

    /// One stored reading, on disk exactly where `review::prune` looks for it.
    fn a_reading_at(
        home: &std::path::Path,
        repo: &str,
        number: u64,
        sha: &str,
    ) -> std::path::PathBuf {
        let dir = home.join("review").join(repo).join("summaries");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{number}-{sha}.json"));
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "number": number, "head_sha": sha, "depth": "line", "line": "it changes a thing",
                "detail": "", "flags": [], "yours": [], "others": 0, "signals": [],
                "unread_because": "", "computed": true,
            }))
            .unwrap(),
        )
        .unwrap();
        path
    }

    /// A queue value with only the fields the pruning rule reads set to anything meaningful.
    fn a_queue(repo_id: &str, whole: bool, fresh: bool, prs: &[(u64, &str)]) -> skein::prq::Queue {
        let mut q: skein::prq::Queue = serde_json::from_value(serde_json::json!({
            "repo_id": repo_id, "slug": "acme/thing", "viewer": "you", "ai": true,
            "blind_spots": [], "as_of": "2026-08-25T09:00:00Z", "fresh": fresh,
            "whole": whole, "trunk": "main", "prs": [],
        }))
        .expect("the queue shape moved under this fixture");
        q.prs = prs
            .iter()
            .map(|(number, sha)| {
                serde_json::from_value(serde_json::json!({
                    "number": number, "title": "t", "author": "someone",
                    "url": "https://github.com/acme/thing/pull/1",
                    "head_ref": "b", "head_sha": sha, "base_ref": "main", "draft": false,
                    "updated_at": "2026-08-25T08:00:00Z", "committed_at": "2026-08-25T08:00:00Z",
                    "checks": "passing", "my_review": "", "review_is_current": false,
                    "reasons": ["reviewer"], "lane": "needs-you", "box_name": "",
                }))
                .expect("the PR shape moved under this fixture")
            })
            .collect();
        q
    }

    /// **A held pull request is not a car, so it can never be drawn as the front** (SKEIN-326).
    ///
    /// SKEIN-279 changed what an assignment means: it says WHICH workflow is responsible, never
    /// that its conditions are met. A pull request whose conditions are unmet is *held* — no clock,
    /// nothing written down, and deliberately not carried for the purpose of acting, so
    /// `prwork::sweep` passes over it.
    ///
    /// `standing.workflow` stays non-empty on a held pull request ON PURPOSE — the row's chooser
    /// must still show which workflow somebody picked — so building the train line from that field
    /// alone put the held one in the line, and as the FRONT when it had the lowest number. The
    /// panel then promised an act the tick would never take, which is what the comment three lines
    /// above the fix says must not happen. This panel is read as a dry run with the train
    /// switched OFF; a wrong front there is what would stop somebody switching it on.
    ///
    /// Both pull requests carry the SAME workflow, assigned the same way, and differ only in
    /// whether its `matches` hold. That is the whole distinction, so it is the whole fixture.
    #[test]
    fn a_held_pull_request_is_kept_out_of_the_train_line_the_panel_draws() {
        let _env = super::env_lock();
        on_a_runtime(async {
            let home = home_for("326");
            no_github(&home);

            // One serial workflow that acts only on an approved pull request.
            std::fs::write(
                home.join("workflows.json"),
                br#"{"workflow":[{"name":"ship","matches":["approved"],"serial":true,
                     "steps":[{"when":[],"do":"merge:squash"}]}]}"#,
            )
            .unwrap();

            // #7 is NOT approved, #9 is. Both are assigned `ship` by hand.
            let dir = home.join("review").join("demo");
            std::fs::create_dir_all(&dir).unwrap();
            let pr = |number: u64, decision: &str| {
                serde_json::json!({
                    "number": number, "title": "t", "author": "someone",
                    "url": "https://github.com/acme/thing/pull/1",
                    "head_ref": "b", "head_sha": "sha", "base_ref": "main", "draft": false,
                    "updated_at": "2026-08-25T08:00:00Z", "committed_at": "2026-08-25T08:00:00Z",
                    "checks": "passing", "my_review": "", "review_is_current": false,
                    "review_decision": decision,
                    "reasons": ["reviewer"], "lane": "needs-you", "box_name": "",
                })
            };
            std::fs::write(
                dir.join("queue.json"),
                serde_json::to_vec(&serde_json::json!({
                    "repo_id": "demo", "slug": "acme/thing", "viewer": "you", "ai": true,
                    "blind_spots": [], "as_of": "2026-08-25T09:00:00Z", "fresh": true,
                    "whole": true, "trunk": "main",
                    "prs": [pr(7, "REVIEW_REQUIRED"), pr(9, "APPROVED")],
                }))
                .unwrap(),
            )
            .unwrap();
            skein::prwork::assign("demo", 7, "ship").unwrap();
            skein::prwork::assign("demo", 9, "ship").unwrap();

            let (status, _, body) = read(api_workflows(Path("demo".into())).await).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let payload: serde_json::Value = serde_json::from_str(&body).expect("a JSON payload");

            // The fixture has to actually produce a HELD standing, or this test asserts nothing.
            assert_eq!(
                payload["prs"]["7"]["workflow"], "ship",
                "the row must still show which workflow was chosen: {body}"
            );
            assert!(
                !payload["prs"]["7"]["holding"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "#7 is not held, so this test is not about SKEIN-326 at all: {body}"
            );
            assert_eq!(payload["prs"]["9"]["workflow"], "ship");

            let train = &payload["trains"][0];
            assert_eq!(train["flow"], "ship", "{body}");
            assert_eq!(
                train["line"],
                serde_json::json!([9]),
                "a held pull request was drawn as a car — the tick passes over it, so the panel \
                 promises an act that will never be taken"
            );
            assert_eq!(
                train["front"], 9,
                "the panel's front is not the tick's front: #7 is held and #9 is what acts"
            );

            forget_github(&home);
        });
    }

    /// **One payload, one answer to "are summaries on?"** (SKEIN-299).
    ///
    /// `Queue::ai` is stamped when the queue is REFRESHED and then rides into the micro-cache and
    /// onto disk; `MergedQueue::ai` is computed when the payload is assembled, and it is the one
    /// the pane reads. Both serialise as `ai`, so a queue served from `prq::remembered` after the
    /// switch was toggled put the same fact in one response twice, disagreeing — and the stale one
    /// was stale by construction, since nothing about a cached queue ever revisits it.
    ///
    /// The fixture is the disagreement itself: a remembered queue written with `"ai": true`, read
    /// back while the switch says OFF. Before the fix the response carried `queues[0].ai == true`
    /// beside `ai == false`.
    ///
    /// **`tests/queue_field_readers.rs` cannot catch this and is not meant to** — it matches by
    /// field NAME against the page, and both payloads spell it `ai`, so the page's single read
    /// vouches for both. That looseness is documented in `docs/queue-fields.md`; a name shared
    /// between two payloads is exactly where it goes blind, so the guard has to be here.
    #[test]
    fn the_summaries_switch_is_answered_once_per_payload_not_once_per_cache_vintage() {
        let _env = super::env_lock();
        on_a_runtime(async {
            let home = home_for("299");
            remember_a_queue(&home);
            no_github(&home);
            // The remembered queue on disk says summaries were on when it was fetched.
            std::env::set_var("SKEIN_REVIEW_AI", "off");

            let (status, _, body) = read(api_review_merged(Query(HashMap::new())).await).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let payload: serde_json::Value = serde_json::from_str(&body).expect("a JSON payload");

            assert_eq!(
                payload["ai"], false,
                "the merged answer did not read the switch at all: {body}"
            );
            let queues = payload["queues"].as_array().expect("queues");
            assert!(
                !queues.is_empty(),
                "the fixture did not reach the payload, so this test asserts nothing: {body}"
            );
            for queue in queues {
                assert_eq!(
                    queue["ai"], payload["ai"],
                    "one payload answered `are summaries on?` two ways — the pane reads the merged \
                     field, and a cached queue kept the answer from whenever it was last refreshed"
                );
            }

            std::env::remove_var("SKEIN_REVIEW_AI");
            forget_github(&home);
        });
    }

    /// **Readings of a commit that has been replaced are actually deleted now** (SKEIN-252).
    ///
    /// `review::prune` had exactly one caller — the handler for `GET /api/repos/:id/review` — and
    /// that route has none: the pane opens on the merged answer. So `summaries/<n>-<sha>.json`
    /// accumulated one file per pull request per head commit, for ever. The unit behaviour was
    /// already covered by `review::tests::pruning_drops_replaced_commits_and_keeps_what_it_cannot_
    /// ask_about`; what no test could show was that anything CALLED it.
    ///
    /// Driven through `prune_behind`, the spawner both queue routes now share, and awaited by
    /// polling because it is deliberately detached — the housekeeping runs behind the answer, not
    /// in front of it. No GitHub: every file here belongs to a pull request that IS in the queue,
    /// so only the superseded-head rule runs, and that one asks nobody.
    #[test]
    fn the_pruning_actually_runs_and_only_against_a_queue_it_can_trust() {
        let _env = super::env_lock();
        on_a_runtime(async {
            let home = home_for("252");
            std::env::set_var("SKEIN_HOME", &home);

            // Open at `now`, with two readings of commits it has moved past.
            let current = a_reading_at(&home, "live", 7, "now");
            let stale = [
                a_reading_at(&home, "live", 7, "before"),
                a_reading_at(&home, "live", 7, "earlier"),
            ];
            // The same shape under a repo whose queue did not see everything.
            let partial = [
                a_reading_at(&home, "partial", 7, "before"),
                a_reading_at(&home, "partial", 7, "earlier"),
            ];
            // And one whose queue came back off disk rather than from GitHub.
            let remembered = [
                a_reading_at(&home, "stale", 7, "before"),
                a_reading_at(&home, "stale", 7, "earlier"),
            ];

            prune_behind(&[
                a_queue("live", true, true, &[(7, "now")]),
                a_queue("partial", false, true, &[(7, "now")]),
                a_queue("stale", true, false, &[(7, "now")]),
            ]);

            // Detached, so wait for it rather than assuming it has run. Generous: what is being
            // asserted is that it happens at all, not how fast.
            let left = || stale.iter().filter(|p| p.exists()).count();
            for _ in 0..100 {
                if left() < 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }

            assert_eq!(
                left(),
                1,
                "nothing was pruned — `review::prune` is wired to a route again, but that route is not \
                 the one the pane opens, so summaries still accumulate one file per head for ever"
            );
            assert!(
                current.exists(),
                "the reading of the commit in front of the reader was deleted"
            );
            assert!(
                partial.iter().all(|p| p.exists()),
                "a queue that did NOT see everything was pruned against (SKEIN-231): absence from a \
                 search cut off at its page says nothing about a pull request"
            );
            assert!(
                remembered.iter().all(|p| p.exists()),
                "a queue read back off disk was pruned against — its idea of the head can be \
                 arbitrarily old, so this can delete the reading of the commit the PR is at NOW"
            );

            forget_github(&home);
        });
    }

    /// The route the pane actually opens is the one that owns the pruning, and it is the ONLY
    /// owner — because "two callers, one of them dead" is how this started.
    #[test]
    fn the_route_the_pane_opens_is_what_prunes() {
        let me = include_str!("skein-server.rs");
        assert!(
            near(me, "async fn api_review_merged(", 0, 12).contains("prune_behind(&m.queues)"),
            "the merged queue route — the one `src/web/index.html` opens on — does not prune"
        );
        // Assembled, so this assertion is not one of its own hits.
        let direct = format!("skein::review::{}(", "prune");
        assert_eq!(
            me.matches(direct.as_str()).count(),
            1,
            "`review::prune` is called from somewhere other than `prune_behind` — the guards that \
             decide when pruning is safe live there, and a second call site does not have them"
        );
    }

    /// The lines of `source` around `needle` — `before` lines above it and `after` below.
    ///
    /// By lines rather than by byte offset: these files are full of em dashes and arrows, and a
    /// byte window into them lands mid-character and panics on a slice boundary.
    fn near(source: &str, needle: &str, before: usize, after: usize) -> String {
        let at = source
            .lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("`{needle}` is not in this file any more"));
        source
            .lines()
            .skip(at.saturating_sub(before))
            .take(before + after)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **The badge route says how stale the badge may be, and says the number `prq` uses**
    /// (SKEIN-235).
    ///
    /// It said sixty seconds for as long as it took anyone to look: `e6c006e` moved the badge poll
    /// to a ten-minute budget inside `prq::counts` and left the route's doc describing what the
    /// route used to do. Nobody reading only one of the two files could tell — the route said 60s,
    /// the module said 600s, and both were written as statements of fact.
    ///
    /// So this reads both. It is the cheapest form of "derive, do not assert": the prose is checked
    /// against the call it describes, in the file that makes it.
    #[test]
    fn the_badge_route_documents_the_budget_prq_actually_uses() {
        let prq = include_str!("../prq/refresh.rs");
        assert!(
            prq.contains("queue_within(&repo, Duration::from_secs(600))"),
            "the badge poll no longer reads through a ten-minute budget, so the sentence this test \
             is defending has become the wrong one — fix the doc, then fix this"
        );
        let doc = near(
            include_str!("skein-server.rs"),
            "async fn api_review_counts",
            16,
            1,
        );
        assert!(
            doc.contains("ten-minute"),
            "the badge route stopped naming the budget it rides:\n{doc}"
        );
        assert!(
            !doc.contains("60s") && !doc.contains("sixty-second"),
            "the badge route is describing a sixty-second cache again, which is the pane's budget \
             and not this one:\n{doc}"
        );
    }

    /// **The refresh nobody is waiting for is not a forced one** (SKEIN-235).
    ///
    /// `api_review_queue` hands over the remembered queue and refreshes behind it — the same shape
    /// `prq::merged` has, and it was missing the same lesson SKEIN-206 taught there. A client
    /// retries a stale answer at 4s/8s/16s/…; a FORCED refresh skips the micro-cache, so a retry
    /// arriving after a sibling refresh had already landed fetched the whole queue again instead
    /// of being answered out of the cache that sibling had just filled. Unforced, those retries are
    /// served by the `unexpired` check above and never reach the spawn at all.
    ///
    /// A source assertion because what regresses is one boolean, and it regresses by looking
    /// obviously right: "this is the refresh, so force it".
    #[test]
    fn the_queue_routes_background_refresh_is_not_a_forced_one() {
        let block = near(
            include_str!("skein-server.rs"),
            "The refresh nobody is waiting for.",
            0,
            24,
        );
        let forced = format!("skein::prq::{}(&repo, true)", "queue");
        assert!(
            !block.contains(forced.as_str()),
            "the background refresh is forced again — every stale-answer retry buys another round \
             of GraphQL searches for a repo nobody is waiting on:\n{block}"
        );
        assert!(
            block.contains("let _ = skein::prq::queue(&repo, false);"),
            "the background refresh is gone, or no longer spelled the way this reads it:\n{block}"
        );
    }

    /// The routes that answer *about* the queue do not open with a refresh.
    ///
    /// A source assertion beside the behavioural one, for the reason `prq.rs`'s own `counts`
    /// assertion gives: what regresses here is a *call*, one line, and it regresses by somebody
    /// adding a route that copies the shape of the one above it.
    #[test]
    fn no_route_that_only_reads_the_queue_refreshes_it() {
        let me = include_str!("skein-server.rs");
        // Two call sites left, and both spend a model call — `read_a_pull_request` and the
        // ask/draft arm of `/review/:n/act` — so both have a reason to want the current head: a
        // reading is worth only the commit it was taken of.
        //
        // There was a third, and it wanted the head for a different reason. `/review/:n/diff`
        // downloaded the LIVE diff and stamped it with the queue's `head_sha`, which is why it
        // could not be served from a remembered queue — it would have labelled today's diff with
        // yesterday's sha, and every comment drafted on it would have re-anchored against a diff
        // that had not moved. It went with the surface that drew that diff: the cockpit's own
        // reading view, replaced by reading the change on GitHub.
        //
        // **One site serves TWO routes** (SKEIN-366). `/review/:n/summary` and `/review/:n/read`
        // are the same reading through different doors — one answers on the request, the other on
        // the live stream — and they share `read_a_pull_request` rather than each opening their
        // own refresh. That is why adding a route did not add a site; two producers of one reading
        // is the thing the shared function exists to prevent.
        //
        // Counted through a needle that does not care whether the repo arrives as `repo` or
        // `&repo`, because the shared helper takes a reference and the routes own a value — a
        // needle spelling one of the two silently stops seeing the other.
        //
        // The needle is assembled rather than written out, so this assertion is not one of its
        // own hits — a source assertion that counts a string it contains counts itself, and the
        // number it reports drifts by one every time somebody edits the test.
        let refresh = format!("skein::prq::{}(", "queue");
        let blocking = me
            .match_indices(refresh.as_str())
            .filter(|(at, _)| {
                me[at + refresh.len()..].starts_with("repo, false)?")
                    || me[at + refresh.len()..].starts_with("&repo, false)?")
            })
            .count();
        assert_eq!(
            blocking, 2,
            "the number of routes opening with a blocking GitHub refresh changed"
        );
        assert!(
            me.contains("fn queue_as_known("),
            "the read-only routes lost the thing that keeps GitHub off the reader's path"
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

    /// Every `/api/…` path a page can BUILD, as `(1-based line, path)` with `${…}` left standing.
    ///
    /// Two narrowings, both to keep this from reporting prose as a request. The `/api/` must open a
    /// string literal — a quote or a backtick immediately before it — and a line that starts a
    /// comment is skipped, because these files discuss routes in comments as often as they call
    /// them (`index.html` mentions `/api/repos/undefined/…` in a note about a bug that is fixed).
    fn asked_for(page: &str) -> Vec<(usize, String, &'static str)> {
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
        let routes = registered(include_str!("skein-server.rs"));
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
    /// router has ever registered, from the day the view shipped (`667e4a2`, 2026-08-21) until this
    /// change. It survived because this list named only the two HTML files, and two tests asserted
    /// the broken string rather than the route table. One list, used by both gates, so the bundle
    /// cannot fall out of one of them.
    fn scanned() -> [(&'static str, &'static str); 3] {
        [
            ("src/web/index.html", include_str!("../web/index.html")),
            ("src/web/v2.html", include_str!("../web/v2.html")),
            (
                "src/web/vendor/cockpit.js",
                include_str!("../web/vendor/cockpit.js"),
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

    /// **The page must not ask a verb this router does not register on that path.**
    ///
    /// The sibling of `the_cockpit_never_asks_for_a_route_this_server_does_not_serve`, one level
    /// finer. A path both sides agree on still 405s if the page POSTs where the router only took a
    /// GET, and axum answers that with a bare `Method Not Allowed` the page reports as its own
    /// generic failure — exactly the shape of SKEIN-246, which cost a working tab and a green suite.
    #[test]
    fn the_cockpit_never_asks_a_method_this_server_does_not_register() {
        let routes = entries(include_str!("skein-server.rs"));
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
    /// that was its one POST caller was deleted in `6578a74`, when the summary and the review
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

        let routes = entries(include_str!("skein-server.rs"));
        let asks = every_ask();
        let unasked: Vec<(&str, &str)> = routes
            .iter()
            .flat_map(|(path, methods)| methods.iter().map(move |m| (*path, *m)))
            .filter(|(path, method)| {
                !asks.iter().any(|(_, _, ask, asked_method)| {
                    asked_method.eq_ignore_ascii_case(method) && serves(path, ask)
                })
            })
            .collect();

        let undeclared: Vec<String> = unasked
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
                !unasked
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
}

/// **An abandoned upload ends the write, not only the process the server holds a handle on.**
///
/// The fourth of the four sites SKEIN-916 names. `Sink::abandon` ran `child.kill().await`, which
/// reaches the crossing and not the `cat` under it — and `kill_on_drop(true)`, which looks like the
/// belt to that braces, reaches exactly the same pid. So a reader whose upload failed over (five
/// retries is the shape SKEIN-269 measured) left five writes holding five half-written files inside
/// the box, each with `ppid` 1 and nothing that would reap it.
#[cfg(test)]
mod upload_deadline {
    use super::*;

    /// **This process's own copy of `place::grouptest`**, and the duplication is a process boundary
    /// rather than an oversight: that module is `#[cfg(test)]` inside the LIBRARY, so it exists in
    /// the library's test binary and in no other — a binary target linking `skein` cannot see it.
    /// The rule it encodes is written out there; what is repeated here is the mechanism.
    struct Escapee {
        token: String,
        pidfile: std::path::PathBuf,
    }

    impl Escapee {
        fn new(dir: &std::path::Path) -> Escapee {
            Escapee {
                // A `sleep` duration, and therefore a name in the grandchild's own argv. This
                // process's pid is in it, so the scan below can match nothing a neighbouring suite
                // started.
                token: format!("600.{}", std::process::id()),
                pidfile: dir.join("abandoned.grandchild"),
            }
        }

        /// The argv for a write that starts a grandchild and then does not finish. The background
        /// `sleep` is a child of the shell, which is the child the server spawned — so it is
        /// exactly the process a kill on the recorded pid does not reach.
        fn argv(&self) -> Vec<String> {
            vec![
                "/bin/sh".into(),
                "-c".into(),
                format!(
                    "sleep {} & echo $! > {}; sleep {}",
                    self.token,
                    self.pidfile.display(),
                    self.token
                ),
            ]
        }

        /// **It is THERE.** Without this half, a stand-in that started nothing would pass the
        /// "gone" assertion below and report the fix working (SKEIN-833).
        fn there(&self) -> u32 {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while std::time::Instant::now() < until {
                if let Some(pid) = std::fs::read_to_string(&self.pidfile)
                    .ok()
                    .and_then(|raw| raw.trim().parse::<u32>().ok())
                {
                    if self.naming().contains(&(pid as libc::pid_t)) {
                        return pid;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!(
                "no grandchild naming {} was running before the sink was abandoned, so its absence \
                 afterwards would prove nothing about the kill",
                self.token
            );
        }

        /// **It is GONE** — the whole set, because the defect leaves more than one process behind
        /// and a test that watched one of them would report the other as fixed.
        fn gone(&self, pid: u32) {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(3000);
            let mut left = Vec::new();
            while std::time::Instant::now() < until {
                left = self.naming();
                if left.is_empty() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!(
                "the sink was abandoned and {left:?} were still running — pid {pid} is the \
                 GRANDCHILD the script recorded, and every one of these names `sleep {}`, so the \
                 kill reached the handle this side recorded and not the work it started",
                self.token
            );
        }

        /// Every process whose argv carries this fixture's token, read from `/proc/<pid>/cmdline`
        /// — never a pattern over a program name (SKEIN-647).
        fn naming(&self) -> Vec<libc::pid_t> {
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for entry in entries.flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
                    continue;
                };
                if let Ok(raw) = std::fs::read(entry.path().join("cmdline")) {
                    if String::from_utf8_lossy(&raw).contains(&self.token) {
                        found.push(pid);
                    }
                }
            }
            found
        }
    }

    impl Drop for Escapee {
        /// Nothing this fixture started outlives it, on the panicking path as much as the returning
        /// one — which is the path that matters, because a failing test here is the one with
        /// something still running.
        fn drop(&mut self) {
            for pid in self.naming() {
                // SAFETY: `kill` has no memory effects, and `pid` names a process whose argv
                // carries a token minted by this process — one this fixture's own script started.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    /// The grandchild is running while the sink is open, and gone once it is abandoned.
    ///
    /// Through [`Sink::open`], which is production's spawn: a test that built its own `Command`
    /// would be asserting on its own process group and would stay green however
    /// [`stream_upload`] spawns.
    ///
    /// **What makes it fail:** removing `.process_group(0)` from [`Sink::open`] and dropping the
    /// negative `kill` from [`Sink::abandon`] — the two lines this test exists for. `kill_on_drop`
    /// and `Child::kill` then reach the shell alone, the backgrounded `sleep` is reparented to init
    /// and goes on running, and `gone` fires naming the pids it can still see.
    #[test]
    fn an_abandoned_upload_takes_its_grandchildren_with_it() {
        let dir = std::env::temp_dir().join(format!("skein-dl916-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory for the fixture");
        let escapee = Escapee::new(&dir);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime for this test's body");
        let sink = runtime.block_on(async { Sink::open(&escapee.argv()) });
        let sink = sink.expect("the write did not start");

        let pid = escapee.there();
        runtime.block_on(sink.abandon());
        escapee.gone(pid);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
