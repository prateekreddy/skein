//! The review pane's routes: the merged and per-repo queues, the badge counts, archive and snooze,
//! the shape of a change, and the acts a reader presses.

use super::*;

/// Every repo's queue in one answer — what the pane opens on. Serves what the counts poll already
/// builds; `?force=1` re-reads GitHub.
pub(super) async fn api_review_merged(Query(q): Query<HashMap<String, String>>) -> Response {
    let force = q.get("force").is_some_and(|v| v == "1" || v == "true");
    match tokio::task::spawn_blocking(move || skein::prq::merged(force)).await {
        Ok(m) => {
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
    match tokio::task::spawn_blocking(|| {
        let repos = skein::repos::load_repos();
        skein::prq::counts()
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
            return Json(old).into_response();
        }
    }
    match tokio::task::spawn_blocking(move || skein::prq::queue(&repo, force)).await {
        Ok(Ok(queue)) => Json(queue).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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

/// Everything you can do to a PR from its row, behind one route.
///
/// One route rather than five because they share the whole of their setup — find the repo, fetch the
/// queue, locate the PR — and they differ only in the last line. Splitting them would be four more
/// copies of the same lookup, and four more places for the "is this PR actually yours" check to be
/// forgotten.
///
/// The `kind` values are `approve`, `request-changes`, `comment` and `merge`, and each acts under
/// your name.
#[derive(Deserialize)]
pub(super) struct ActReq {
    /// approve | request-changes | comment | merge
    kind: String,
    /// The review body.
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
                    token: &skein::prq::token_for(&slug, skein::prq::Need::Write)?,
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
                // nothing else — no expected head, no base check.
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
                // refused rather than defaulted — `prq::merge_by_hand` says so in words.
                let seen = match req.drafted_at.trim().is_empty() {
                    false => req.drafted_at.clone(),
                    true => skein::prq::remembered_head(&id, number).unwrap_or_default(),
                };
                skein::prq::merge_by_hand(&slug, number, &seen)?
            }
            (None, other) => return Err(format!("unknown action: {other}")),
        };
        // Anything that touched GitHub changed the lane this PR belongs in, and the queue is cached
        // for 60s — without this the row would sit in Needs you until the cache aged out.
        skein::prq::invalidate(&id);
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
pub(super) mod review_routes {
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
    /// `for key in [..] { remove_var(key) }` loop this replaces did not survive. `pub(crate)` so the
    /// settings routes' tests pin through it too rather than growing a third copy.
    pub(crate) struct EnvPins(Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>);

    pub(crate) fn env_pins() -> EnvPins {
        EnvPins(Vec::new())
    }

    impl EnvPins {
        pub(crate) fn set(
            &mut self,
            name: &str,
            value: impl AsRef<std::ffi::OsStr>,
        ) -> &mut EnvPins {
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
        env.set("GH_TOKEN", "skein-test-not-a-real-token");
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

    /// **An ask or a draft that ran outside its box says so on the answer** (SKEIN-819), and every
    /// other act answers exactly what it did before.
    ///
    /// The owner's decision puts `"outside": "<outside_box_because>"` on the route's answer so the
    /// composer can draw its line above the text. **What makes it fail:** `acted` dropping the key
    /// (the first assertion), or writing it whether or not there is a reason — the second and third,
    /// which is the direction that would put "Answered outside its box — ." over every ordinary
    /// answer.
    #[test]
    fn an_answer_from_outside_the_box_carries_why_and_an_ordinary_one_carries_nothing() {
        let why = "skein has no record of where that box is, so it was never started or it is gone";
        let said = acted("the answer", Some(why));
        assert_eq!(said["ok"], true);
        assert_eq!(said["text"], "the answer");
        assert_eq!(
            said["outside"], why,
            "the reason the answer ran outside its box was dropped on the way to the composer: {said}"
        );
        for ordinary in [acted("the answer", None), acted("the answer", Some(""))] {
            assert!(
                ordinary.get("outside").is_none(),
                "an answer from inside its box was marked as from outside it: {ordinary}"
            );
        }
    }
}
