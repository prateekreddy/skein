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

}
