//! `move it aside`: the one act the review pane offers on a set-aside file it could not read
//! (SKEIN-552).
//!
//! Its own file rather than a route in `review.rs` because it is not a review act — it touches no
//! pull request and asks GitHub nothing. It renames one file on this machine and says where to.

use axum::extract::Path;
use axum::Json;

/// Rename a repo's unreadable `archived.json` or `snoozed.json` to `<file>.unreadable-<date>`.
///
/// `:file` is `archived` or `snoozed`, the word `Queue::unreadable_set_aside` carries. The answer's
/// `moved_to` is the path the toast names — the file is kept, so "nothing was deleted" is true.
pub(super) async fn api_move_set_aside_aside(
    Path((id, file)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    // Resolved first, like every route on `/api/repos/:id` — `:id` reaches `prq::review_dir` as a
    // path component.
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    let id = repo.id;
    let res = tokio::task::spawn_blocking(move || {
        let r = skein::prq::move_set_aside_aside(&id, &file);
        // The next ask must re-read the directory rather than serve the line from the cache.
        skein::prq::invalidate(&id);
        r
    })
    .await;
    Json(match res {
        Ok(Ok(to)) => serde_json::json!({ "ok": true, "moved_to": to.display().to_string() }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    })
}
