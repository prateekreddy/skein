//! `/api/health` and `/api/usage`: the health report, and the usage scan behind the usage panel.

use super::*;

/// What is true of this machine right now.
///
/// **Where skein is running used to ride here too** (SKEIN-467), beside the report rather than on
/// `/api/settings`, because it was not a setting: it was declared in the environment, nothing could
/// write it, and `/api/settings` is a document the page reads back and POSTs. There is one
/// deployment now (SKEIN-576) and nothing to report — see the note at the merge site below, and
/// `docs/parity.md` §7 for what a person stops being told.
///
/// Boxes cannot see this either way — the route is behind the same token as the rest.
pub(super) async fn api_health() -> Json<serde_json::Value> {
    let report = {
        tokio::task::spawn_blocking(skein::health::health_report)
            .await
            // Every check UNKNOWN, and none of them a fault. The health task falling over says
            // nothing about whether sbx is installed or whether scoping is configured — a red line
            // for each would blame seven subsystems for one panic somewhere else entirely, which is
            // what this used to do with `ok: false`. `ok: false` on the report itself stays, so the
            // cockpit still says something is wrong; it is now the report that is broken rather
            // than everything it was asked about.
            .unwrap_or_else(|error| {
                let mut report = skein::health::HealthReport {
                    ok: false,
                    // Written below from the report itself, as `health_report` does: nothing
                    // here is a fault so it chooses no headline, but the page reads this as what
                    // `OnBanner` says and it must not say something else (SKEIN-1013).
                    counted: Vec::new(),
                    // The one field a crashed health task can still answer: it is about the binary,
                    // not about anything the task had to go and ask.
                    build: skein::health::BUILD_REVISION,
                    labels: &skein::health::CHECK_LABELS,
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
                    token_expiry: skein::health::HealthCheck::unknown(
                        "the health check itself failed",
                    ),
                    proxy_injection: skein::health::HealthCheck::unknown(
                        "the health check itself failed",
                    ),
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
                    unowned: None,
                    runtimes: skein::runtime::supported_runtimes(),
                    // Empty rather than guessed: this is the report for a health task that
                    // *failed*, and the checklist reads this field as "boxes can push". Naming a
                    // credential here would tick that step off on the strength of a crash.
                    git_credential: String::new(),
                };
                report.counted = report.counted_on_the_wire();
                report
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
pub(super) async fn api_usage(Query(q): Query<HashMap<String, String>>) -> Response {
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
