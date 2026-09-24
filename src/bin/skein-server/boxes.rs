//! The board's boxes: what needs you and what happened while you were away, creating a box and
//! watching its act, and the per-box routes — its transcript, diff, files, session and mail, and
//! the presses that resume, restart, stop or destroy it.

use super::*;

/// Everything that stopped and is waiting on you, most urgent first.
///
/// **Not on the two-second tick.** The pull-request half comes from `prq::queue`'s own 60-second
/// cache, so a surface rendering this often still reaches GitHub once a minute per repo — but it is
/// a surface's call rather than the board's, which is what keeps `signal::board_tick` honest.
pub(super) async fn api_queue() -> Json<serde_json::Value> {
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
pub(super) async fn api_away() -> Json<serde_json::Value> {
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
pub(super) async fn api_seen() -> Response {
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
pub(super) async fn api_create_box(
    Path(name): Path<String>,
    Json(body): Json<CreateBox>,
) -> Response {
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
pub(super) struct CreateBox {
    branch: String,
    #[serde(default)]
    agent: Option<String>,
}

/// What an act is doing, and everything it has said. Readable after it has ended, which is the
/// whole point.
pub(super) async fn api_act(Path(id): Path<String>) -> Response {
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
pub(super) async fn act_stream(
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

/// Snapshot of the fleet, on request. `load_views` is blocking, so it runs on the blocking pool and
/// never inline on an async worker — see `start_producing` for why blocking a worker stalls every
/// terminal websocket scheduled on it.
///
/// Still here after the stream became one producer, and for two reasons: a surface that wants the
/// picture once should not have to open a stream to get it, and it is what a client re-syncs from
/// when it is told it has fallen behind.
pub(super) async fn api_boxes() -> Json<Vec<BoxView>> {
    let views = tokio::task::spawn_blocking(|| load_views().unwrap_or_default())
        .await
        .unwrap_or_default();
    Json(views)
}

/// Provider-neutral custom footer. Claude renders it natively from stdin; Codex maps its latest
/// token_count event (the `/status` data source) through the same renderer for the browser terminal.
pub(super) async fn api_statusline(Path(name): Path<String>) -> Response {
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
pub(super) struct TakeoverReq {
    target: String,
}

pub(super) async fn api_takeover(
    Path(name): Path<String>,
    Json(request): Json<TakeoverReq>,
) -> Response {
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

/// The box's conversation as its own record has it — survives a reboot, a server restart, a page
/// reload and the scrollback limit, none of which the rendered terminal does. `?bytes=` is how much
/// of the tail to read; the cockpit doubles it to page backwards.
pub(super) async fn api_transcript(
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
pub(super) async fn api_diff(Path(name): Path<String>) -> Response {
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
pub(super) async fn api_files(
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
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
pub(super) async fn api_file(
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
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
pub(super) async fn api_session(Path(name): Path<String>) -> Response {
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    match tokio::task::spawn_blocking(move || skein::digest::session_digest(&name)).await {
        Ok(Some(d)) => Json(d).into_response(),
        _ => (StatusCode::NOT_FOUND, "no such box").into_response(),
    }
}

/// All cross-box messages, newest first.
pub(super) async fn api_mailbox() -> Json<Vec<skein::mailbox::Message>> {
    Json(skein::mailbox::load_mailbox())
}

#[derive(Deserialize)]
pub(super) struct SendReq {
    to: String,
    body: String,
    #[serde(default)]
    kind: String,
}

/// Post a message into the shared mailbox (from skein). `to` is a vmid or "broadcast".
pub(super) async fn api_mailbox_send(Json(r): Json<SendReq>) -> Response {
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

#[derive(Deserialize)]
pub(super) struct RepinReq {
    branch: String,
}

/// Re-pin an existing box's launch spec to a different branch, without relaunching it — for a box
/// whose agent has moved off its recorded branch (e.g. branch-per-slice work) and keeps getting
/// checked back onto the stale one every reconnect. Takes effect on the box's next reconnect.
/// Returns {ok, error?}.
pub(super) async fn api_repin(
    Path(name): Path<String>,
    Json(r): Json<RepinReq>,
) -> Json<serde_json::Value> {
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
pub(super) struct ResumeReq {
    #[serde(default)]
    prompt: String,
}

/// Resume a paused box (the one-click "continue" — step 6). Fire-and-forget: kicks off the box's
/// agent headless and returns immediately; the inbox follows the box's own hooks. Returns {ok} or
/// {ok:false, error}. Only ever called from an explicit click; batch use is gated to `proceed` boxes.
pub(super) async fn api_resume(
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
pub(super) struct RestartAgentReq {
    #[serde(default)]
    runtime: String,
}

pub(super) async fn api_restart_agent(
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
pub(super) async fn api_narrate(Path(name): Path<String>) -> Json<serde_json::Value> {
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
pub(super) struct BatchReq {
    #[serde(default)]
    names: Vec<String>,
}

/// Batch-resume the boxes paused on a trivial "proceed?" (step 6). With AI on (step 7) each is first
/// run past the conservative safety gate; genuine decisions are held back. Returns
/// {ok, resumed, held, not_continued} and, when `ok` is false, `error`: the sentence the page shows.
pub(super) async fn api_resume_batch(Json(r): Json<BatchReq>) -> Json<serde_json::Value> {
    let names = r.names.clone();
    let outcome = tokio::task::spawn_blocking(move || skein::sandbox::resume_batch(&r.names))
        .await
        .map_err(|e| e.to_string());
    Json(batch_answer(&names, outcome))
}

/// What a batch Continue answers, given what was asked and what came back.
///
/// **`ok` is true only when every box asked for was continued or held** (SKEIN-1132). It used to be
/// true on every path: a panic inside the batch became `unwrap_or_default()`, an empty result, and
/// the page toasted "continuing 0" — a failure presented as a success, after the person had
/// confirmed a list of named boxes. A box that `resume_batch` neither resumed nor held is one it
/// skipped or failed to resume, and it is named, because "some of them did not" leaves the person
/// to work out which by opening every one.
///
/// **And each one is named with its reason** (SKEIN-1137), in `resume_box`'s own words: "not
/// running", or the resume log to read. Naming the box without the reason still left the person to
/// open it and find out, which is the half of the work the sentence exists to save.
fn batch_answer(
    names: &[String],
    outcome: Result<skein::sandbox::BatchOutcome, String>,
) -> serde_json::Value {
    let skein::sandbox::BatchOutcome {
        resumed,
        held,
        failed,
    } = match outcome {
        Ok(out) => out,
        Err(why) => {
            return serde_json::json!({
                "ok": false,
                "resumed": [],
                "held": [],
                "not_continued": names,
                "error": format!(
                    "continue stopped inside skein ({why}) — none of {} is known to have \
                     continued",
                    names.join(", ")
                ),
            })
        }
    };
    // Still derived from what was ASKED, not from `failed`: a box the batch dropped without a word
    // is exactly the case SKEIN-1132 was about, and it must not read as accounted for.
    let not_continued: Vec<&String> = names
        .iter()
        .filter(|n| !resumed.contains(n) && !held.contains(n))
        .collect();
    if not_continued.is_empty() {
        return serde_json::json!({
            "ok": true, "resumed": resumed, "held": held, "not_continued": [],
        });
    }
    let listed: Vec<String> = not_continued
        .iter()
        .map(|n| match failed.iter().find(|(name, _)| name == *n) {
            Some((_, why)) => format!("{n}: {why}"),
            None => n.to_string(),
        })
        .collect();
    let mut error = format!(
        "{} of {} did not continue — {}",
        not_continued.len(),
        names.len(),
        listed.join(" · ")
    );
    if !resumed.is_empty() {
        error.push_str(&format!(" · continuing {}", resumed.len()));
    }
    if !held.is_empty() {
        error.push_str(&format!(" · held {} for your eyes", held.len()));
    }
    serde_json::json!({
        "ok": false,
        "resumed": resumed,
        "held": held,
        "not_continued": not_continued,
        "error": error,
    })
}

/// Stop a box: halt the sandbox (frees compute; resume later via attach). Non-destructive — the box
/// stays listed and goes stale until resumed. Returns {ok} or {ok:false, error}.
pub(super) async fn api_stop(Path(name): Path<String>) -> Json<serde_json::Value> {
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
pub(super) async fn api_destroy(Path(name): Path<String>) -> Json<serde_json::Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// **A batch Continue that did not continue every box it was asked to says so, and names them**
    /// (SKEIN-1132).
    ///
    /// Three outcomes, because each is a way the old `ok: true` lied or could be made to: the batch
    /// failing inside the server (a `JoinError`, which became an empty result and "continuing 0"),
    /// a box it neither resumed nor held, and — the half that proves the other two are not simply
    /// "always false" — a batch where every box was accounted for.
    ///
    /// **What would make this fail:** answering `"ok": true` on the error path, which is the line
    /// this replaced; or dropping the not-continued check, so a partial batch reads as a success.
    #[test]
    fn a_batch_continue_that_did_not_continue_every_box_is_not_a_success() {
        let asked = names(&["alpha", "beta", "gamma"]);

        let failed = batch_answer(&asked, Err("task panicked".into()));
        assert_eq!(
            failed["ok"], false,
            "a batch that failed inside the server answered ok, and the page toasts it as success"
        );
        let why = failed["error"].as_str().unwrap_or_default();
        assert!(
            why.contains("task panicked") && why.contains("alpha, beta, gamma"),
            "the failure does not say what went wrong and which boxes it was: {why}"
        );
        assert_eq!(failed["not_continued"], serde_json::json!(asked));

        let partial = batch_answer(
            &asked,
            Ok(skein::sandbox::BatchOutcome {
                resumed: names(&["alpha"]),
                held: names(&["gamma"]),
                failed: vec![("beta".into(), "resume exited 1".into())],
            }),
        );
        assert_eq!(
            partial["ok"], false,
            "a box that was neither continued nor held read as a success"
        );
        assert_eq!(partial["not_continued"], serde_json::json!(["beta"]));
        let why = partial["error"].as_str().unwrap_or_default();
        assert!(
            why.contains("1 of 3") && why.contains("beta") && !why.contains("alpha,"),
            "the partial failure does not name exactly the box that did not continue: {why}"
        );
        assert_eq!(partial["held"], serde_json::json!(["gamma"]));

        let whole = batch_answer(
            &asked,
            Ok(skein::sandbox::BatchOutcome {
                resumed: names(&["alpha", "beta"]),
                held: names(&["gamma"]),
                failed: vec![],
            }),
        );
        assert_eq!(
            whole["ok"], true,
            "every box continued or held, and the batch was reported as a failure: {whole}"
        );
        assert!(
            whole.get("error").is_none(),
            "a success carried an error: {whole}"
        );
    }
    /// **Each box that did not continue is followed by its reason, in the owner's shape**
    /// (SKEIN-1137): `<n> of <m> did not continue — <box>: <reason> · … · continuing N`.
    ///
    /// The expected sentence is the owner-approved example, character for character, so a change
    /// to the separator, the dash or the order of the tails is a change to approved wording and
    /// fails here first.
    ///
    /// **What would make this fail:** formatting `not_continued` by name alone, as before this
    /// item — the sentence then reads `beta · gamma` and loses both reasons; or joining the
    /// entries with `, ` as the names used to be.
    #[test]
    fn a_box_that_did_not_continue_is_named_with_its_reason() {
        let asked = names(&["alpha", "beta", "gamma"]);
        let answer = batch_answer(
            &asked,
            Ok(skein::sandbox::BatchOutcome {
                resumed: names(&["alpha"]),
                held: vec![],
                failed: vec![
                    (
                        "beta".into(),
                        "box \"beta\" is not running; attach/start it before resuming".into(),
                    ),
                    ("gamma".into(), "not a box name".into()),
                ],
            }),
        );
        assert_eq!(
            answer["error"],
            "2 of 3 did not continue — beta: box \"beta\" is not running; attach/start it before \
             resuming · gamma: not a box name · continuing 1"
        );
        assert_eq!(
            answer["not_continued"],
            serde_json::json!(["beta", "gamma"])
        );
    }

    /// **A resume that fails inside the batch carries its own reason all the way to the answer the
    /// page toasts** (SKEIN-1137) — driven through the route, over the real `resume_batch` and
    /// `resume_box`, with one box's resume sabotaged to exit 3.
    ///
    /// Four boxes, one per way a name can end: `thing-up` continues; `thing-sab` is running but its
    /// resume command exits 3, so its reason is `resume_box`'s "resume exited 3 …; see <log>";
    /// `thing-down` is stopped, so its reason is "not running"; `../bad` is no box name at all.
    ///
    /// **What would make this fail:** `resume_batch` keeping only the `Ok` names again (as it did
    /// before this item) — `thing-sab` and `thing-down` then reach the sentence bare and the
    /// `contains` below names which reason went missing; or skipping an invalid name silently,
    /// which drops `../bad: not a box name`.
    #[test]
    fn a_sabotaged_resume_says_why_in_the_batch_answer() {
        let _env = super::super::env_lock();
        let home = super::super::scratch_dir("1137");
        let registry = home.join("sandboxes.json");
        std::fs::write(
            &registry,
            r#"{"thing-up":{"branch":"a","dir":"/d","lastSeen":"","status":"waiting"},
               "thing-sab":{"branch":"b","dir":"/d","lastSeen":"","status":"waiting"},
               "thing-down":{"branch":"c","dir":"/d","lastSeen":"","status":"waiting"}}"#,
        )
        .unwrap();
        let mut env = super::super::review::review_routes::env_pins();
        env.set("SKEIN_HOME", &home)
            .set("SKEIN_FLEET_ROOT", &home)
            .set("SKEIN_REGISTRY", &registry)
            .set("SKEIN_SHARED", "")
            .set("SKEIN_REPO", "")
            .set("SKEIN_AI", "off")
            .set(
                "SKEIN_LS_CMD",
                r#"printf '%s\n' '{"name":"thing-up","agent":"claude","status":"running"}' '{"name":"thing-sab","agent":"claude","status":"running"}' '{"name":"thing-down","agent":"claude","status":"stopped"}'"#,
            )
            // The sabotage: this one box's resume exits 3, the others succeed.
            .set("SKEIN_RESUME_CMD", "test {name} != thing-sab || exit 3");
        // The library's `sbx ls` gate remembers an answer for a moment outside its own tests, and
        // this binary is outside them; a sibling's fleet must not stand in for this one.
        skein::sbx::forget_fleet_boxes();

        let asked = names(&["thing-up", "thing-sab", "thing-down", "../bad"]);
        let answer = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a tokio runtime for this test's body")
            .block_on(api_resume_batch(Json(BatchReq {
                names: asked.clone(),
            })))
            .0;
        let _ = std::fs::remove_dir_all(&home);

        assert_eq!(answer["ok"], false, "{answer}");
        assert_eq!(
            answer["resumed"],
            serde_json::json!(["thing-up"]),
            "{answer}"
        );
        let why = answer["error"].as_str().unwrap_or_default();
        assert!(
            why.starts_with("3 of 4 did not continue — "),
            "the count or the dash is not the approved shape: {why}"
        );
        assert!(
            why.contains("thing-sab: resume exited 3") && why.contains("resume.log"),
            "the sabotaged resume's own reason, and the log it names, did not reach the answer: \
             {why}"
        );
        assert!(
            why.contains(
                "thing-down: box \"thing-down\" is not running; attach/start it before resuming"
            ),
            "the stopped box's reason did not reach the answer: {why}"
        );
        assert!(
            why.contains("../bad: not a box name"),
            "an invalid name was dropped rather than named: {why}"
        );
        assert!(
            why.ends_with(" · continuing 1"),
            "the continuing tail is gone: {why}"
        );
    }
}
