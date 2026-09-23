//! The live board: `/api/events`, the SSE stream it answers with, the producer it starts, and
//! the cap on live boards.

use super::*;

/// Cap live boards for the same reason as the PTY cap: a client that connects and never reads still
/// holds a channel, and a producer serving a hundred of them is a producer nobody is watching.
static EVENT_LIMIT: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));

/// Live fleet stream: an opening snapshot, then only what moved.
///
/// One producer feeds every client — see `start_producing` — and the transitions come from
/// `skein::stream`. The roadmap note that used to sit here ("replace polling with a subscription")
/// is done, and left as a stale comment it would describe the shape this no longer has.
pub(super) async fn api_events() -> Response {
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
