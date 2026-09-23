//! The review pane's routes: the merged and per-repo queues, the badge counts, a pull request's
//! reading and summaries, archive and snooze, the shape of a change, and the acts a reader
//! presses.

use super::*;

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
pub(super) async fn api_review_merged(Query(q): Query<HashMap<String, String>>) -> Response {
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
/// before `0410016` moved the budget (SKEIN-235). It is one number in two places or it drifts
/// again, so `the_badge_route_documents_the_budget_prq_actually_uses` reads both.
///
/// Repos with the queue switched off, and repos with no GitHub remote, are never asked.
pub(super) async fn api_review_counts() -> Response {
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
pub(super) async fn api_review_reading() -> Response {
    Json(skein::review::readings()).into_response()
}

/// A repo's review queue: every open PR that is yours, and which lane it sits in.
///
/// `?force=1` skips the 60s micro-cache — for the refresh button and for the moment after an act
/// that changed a PR's state. Blocking work (three `gh` round trips) goes to a blocking thread so a
/// slow GitHub cannot stall the cockpit's SSE tick.
pub(super) async fn api_review_queue(
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
pub(super) fn queue_as_known(repo: &skein::repos::Repo) -> Result<skein::prq::Queue, String> {
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
pub(super) fn answered_from<T: serde::Serialize>(queue: &skein::prq::Queue, body: T) -> Response {
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
pub(super) struct ArchiveReq {
    /// true = set aside, false = bring back. Explicit rather than a toggle so a double-tap or a
    /// retried request cannot flip a PR back into a lane you already moved it out of.
    on: bool,
}

/// Set aside (or restore) one PR in a repo's queue.
pub(super) async fn api_review_archive(
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
pub(super) struct SnoozeReq {
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
pub(super) async fn api_review_snooze(
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
pub(super) async fn api_pr_shape(Path((id, number)): Path<(String, u64)>) -> Response {
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
pub(super) async fn api_box_shape(Path(name): Path<String>) -> Response {
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
pub(super) async fn api_review_summary(
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
pub(super) async fn api_review_read(
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
pub(super) struct ReadingReq {
    on: bool,
}

/// Say whether skein may read this repo's pull requests with nobody watching.
///
/// Its own route, for the same reason it is its own setter: this is the switch that decides whether
/// skein spends model calls on its own, and it should not be reachable as a side effect of saving
/// something else.
pub(super) async fn api_set_reading(
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
pub(super) async fn api_review_summaries(
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
pub(super) struct ActReq {
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

pub(super) async fn api_review_act(
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
        let home = super::scratch_dir(what);
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

    /// A minimal, file-local copy of `src/testutil.rs::EnvPins` (SKEIN-711): this binary cannot
    /// reach that one, since `testutil` is `#[cfg(test)] mod testutil` inside the LIBRARY crate and
    /// `skein-server` is a separate `[[bin]]` that only sees the library's `pub` surface —
    /// `tests/common/mod.rs` carries the same copy for the same reason, for the integration
    /// binaries. Restores from `Drop`, so a test that panics still puts these back, which the
    /// `for key in [..] { remove_var(key) }` loop this replaces did not survive.
    struct EnvPins(Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>);

    fn env_pins() -> EnvPins {
        EnvPins(Vec::new())
    }

    impl EnvPins {
        fn set(&mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) -> &mut EnvPins {
            self.0.push((name.into(), std::env::var_os(name)));
            std::env::set_var(name, value);
            self
        }
    }

    impl Drop for EnvPins {
        fn drop(&mut self) {
            // Reverse, so the FIRST pin of a name is the last one undone — see the doc comment on
            // `src/testutil.rs::EnvPins::drop`, which this mirrors.
            for (name, prior) in self.0.drain(..).rev() {
                match prior {
                    Some(v) => std::env::set_var(&name, v),
                    None => std::env::remove_var(&name),
                }
            }
        }
    }

    /// Point skein at a GitHub that is not there. Port 1 refuses instantly, so a route that goes
    /// looking fails in milliseconds and this test stays fast — what is asserted is WHETHER it
    /// goes, not how long it waits when it does.
    fn no_github(home: &std::path::Path) -> EnvPins {
        let mut env = env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_GITHUB_API", "http://127.0.0.1:1");
        env.set("GH_TOKEN", "not-a-real-token");
        env
    }

    /// The directory `no_github`'s caller built. Its `EnvPins` guard restores the environment
    /// itself, from `Drop`, so this only ever has the one job now (SKEIN-711).
    fn forget_github(home: &std::path::Path) {
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
            let _gh_env = no_github(&home);

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
            let _gh_env = no_github(&home);

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
            let mut gh_env = no_github(&home);
            // The remembered queue on disk says summaries were on when it was fetched.
            gh_env.set("SKEIN_REVIEW_AI", "off");

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
            let mut gh_env = env_pins();
            gh_env.set("SKEIN_HOME", &home);

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
        let me = server_source();
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
    /// It said sixty seconds for as long as it took anyone to look: `0410016` moved the badge poll
    /// to a ten-minute budget inside `prq::counts` and left the route's doc describing what the
    /// route used to do. Nobody reading only one of the two files could tell — the route said 60s,
    /// the module said 600s, and both were written as statements of fact.
    ///
    /// So this reads both. It is the cheapest form of "derive, do not assert": the prose is checked
    /// against the call it describes, in the file that makes it.
    #[test]
    fn the_badge_route_documents_the_budget_prq_actually_uses() {
        let prq = include_str!("../../prq/refresh.rs");
        assert!(
            prq.contains("queue_within(&repo, Duration::from_secs(600))"),
            "the badge poll no longer reads through a ten-minute budget, so the sentence this test \
             is defending has become the wrong one — fix the doc, then fix this"
        );
        let doc = near(server_source(), "async fn api_review_counts", 16, 1);
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
        let block = near(server_source(), "The refresh nobody is waiting for.", 0, 24);
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
        let me = server_source();
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
