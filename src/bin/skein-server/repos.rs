//! The repositories skein manages: listing, adding, removing and pulling one, its settings and
//! module notes, and the runtimes a box can run.

use super::*;

/// A repo's own settings. Absent field = leave it alone; empty string = clear it. One request can
/// carry all of them, so the settings pane saves a card, not a keystroke.
#[derive(Deserialize)]
pub(super) struct RepoSettingsReq {
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
    /// the branch a new box of this repo starts from; empty = the remote's default
    base_branch: Option<String>,
}

pub(super) async fn api_set_repo_settings(
    Path(id): Path<String>,
    Json(req): Json<RepoSettingsReq>,
) -> Response {
    // First, so a branch that cannot be one refuses before anything else is written.
    if let Some(branch) = &req.base_branch {
        if let Err(error) = skein::repos::set_base_branch(&id, branch) {
            return (StatusCode::BAD_REQUEST, error).into_response();
        }
    }
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
pub(super) async fn api_modules(Path(id): Path<String>) -> Response {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    match tokio::task::spawn_blocking(move || skein::moduledocs::status(&repo)).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct WriteModuleReq {
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
pub(super) async fn api_write_module(
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

/// List the repos skein manages, each with the GitHub repository it maps to as `slug`.
///
/// `slug` is resolved here rather than in the browser because `source` is not always the answer: an
/// entry registered before a path stopped being registrable holds a path, and the remote it really
/// fetches lives in the mirror's `origin` — a `git` call only the host can make. A browser parsing
/// `source` alone disagreed with the host about the same repo. Empty string ⇒ no GitHub remote.
pub(super) async fn api_repos() -> Json<Vec<serde_json::Value>> {
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
pub(super) async fn api_runtimes() -> Json<Vec<skein::runtime::RuntimeInfo>> {
    Json(skein::runtime::supported_runtimes())
}

#[derive(Deserialize)]
pub(super) struct AddRepoReq {
    source: String,
    #[serde(default)]
    id: String,
    /// **Read only so that it can be refused** (SKEIN-535). Kept on the struct rather than deleted
    /// because serde ignores a field it does not know: dropping it would make a request that names
    /// a store succeed while quietly getting skein's own, which is the one outcome worse than the
    /// bug being fixed here.
    #[serde(default)]
    store: String,
}

/// Register a repo: clone its remote, provision its store + kit, record it. A path is a 400, from
/// `add_repo`. `git clone` can take a while, so run the blocking work off the async runtime.
///
/// **`store` is a CLI affordance and this route does not have it** (SKEIN-535). `add_repo` takes an
/// arbitrary host path and uses it as one — `ensure_store` (`src/kit.rs`) scaffolds a whole `.claude`
/// tree wherever it points, and its only guard is that the path is absolute. Over HTTP that made
/// `{"store":"/tmp/outside/evilstore"}` write that tree anywhere on the host, reproduced against a
/// running server on 2026-09-05. The request is authenticated, but the fleet API token is printed
/// into every cockpit URL, so this was a privilege question rather than an open door.
///
/// **Refused rather than ignored.** Silently substituting skein's managed store for the one the
/// caller named would be the server telling a caller its instruction was obeyed when it was not.
///
/// The CLI keeps it: `skein add --store ~/thing-shared/.claude` is a real, wanted use of an
/// absolute path, and it runs as the person on their own host rather than as a request carrying a
/// token that is on screen. `add_repo` cannot tell the two apart, so the caller is distinguished
/// here, at the route, which is the only place that knows.
pub(super) async fn api_add_repo(Json(r): Json<AddRepoReq>) -> Response {
    if r.source.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "missing source").into_response();
    }
    if !r.store.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "`store` is not accepted over HTTP: it is an arbitrary path on the host, and \
             registering a repo would scaffold a `.claude` store wherever it pointed.\n  \
             Adopt an existing store from the CLI instead — `skein add <git-url> --store <path>` \
             — which runs as you, on the host.\n  Leave `store` out and skein manages one under \
             its own home.",
        )
            .into_response();
    }
    let res = tokio::task::spawn_blocking(move || {
        let id = (!r.id.trim().is_empty()).then(|| r.id.trim().to_string());
        // `None`, always: the only way past the refusal above is not to have named a store.
        skein::repos::add_repo(r.source.trim(), id.as_deref(), None)
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
pub(super) async fn api_remove_repo(Path(id): Path<String>) -> Response {
    match skein::repos::remove_repo(&id) {
        Ok(repo) => Json(repo).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// Pull the latest code into a repo's working clone (fast-forward only). `git pull` hits the network,
/// so run the blocking work off the async runtime.
pub(super) async fn api_pull_repo(Path(id): Path<String>) -> Response {
    let res = tokio::task::spawn_blocking(move || skein::repos::pull_repo(&id)).await;
    match res {
        Ok(Ok(summary)) => Json(serde_json::json!({ "summary": summary })).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
