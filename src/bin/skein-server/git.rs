//! Git access for the fleet: what boxes have asked to write and been granted, the write
//! credentials and read token, the probe, and a box's git scope and privilege.

use super::*;

/// What boxes have asked to write, and what has already been granted.
///
/// Both in one response, because the question the panel answers is "who can write where" and a
/// pending ask and a live grant are two states of one answer. Reading the queue execs into the
/// sandbox; the grants are a host file, so they survive a fleet that is down.
pub(super) async fn api_git_grants() -> Json<serde_json::Value> {
    let requests = tokio::task::spawn_blocking(skein::gitgate::fleet_requests)
        .await
        .unwrap_or_default();
    // **Your GitHub identity, per repository** (SKEIN-1179): which credential skein reads each
    // managed repository with and which it posts, merges and labels with — the same answer
    // `prq::token_for` gives the queue, the verdict and the tick, from the same resolver, so the
    // page cannot describe a credential the calls do not use. Blocking because the last source is
    // the host's `gh` login, which is a subprocess the first time it is asked.
    let identity = tokio::task::spawn_blocking(|| {
        let repos: Vec<serde_json::Value> = skein::repos::load_repos()
            .iter()
            .filter_map(|repo| {
                let slug = skein::gitgate::repo_slug(repo)?;
                Some(serde_json::json!({
                    "repo": repo.id,
                    "slug": slug,
                    "reads": skein::prq::repo_token_source(&slug, skein::prq::Need::Read).key(),
                    "writes": skein::prq::repo_token_source(&slug, skein::prq::Need::Write).key(),
                }))
            })
            .collect();
        serde_json::json!({
            // "Who am I" — the one call that names no repository, and so the one that may be
            // answered by a token stored for some other repository.
            "viewer": skein::prq::host_token_source().key(),
            "repos": repos,
        })
    })
    .await
    .unwrap_or_default();
    let grants = skein::gitgate::grants();
    let now = chrono::Utc::now();
    Json(serde_json::json!({
        "identity": identity,
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
pub(super) struct CredentialReq {
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
pub(super) async fn api_git_credential(Json(r): Json<CredentialReq>) -> Response {
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
pub(super) struct ReadTokenReq {
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
pub(super) async fn api_git_read_token(Json(r): Json<ReadTokenReq>) -> Response {
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

pub(super) async fn api_git_credential_remove(Path(id): Path<String>) -> Response {
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
pub(super) struct GrantReq {
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
pub(super) async fn api_git_grant_decide(
    Path(id): Path<String>,
    Json(r): Json<GrantReq>,
) -> Response {
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
pub(super) async fn api_git_probe() -> Json<Vec<skein::gitgate::ProbeResult>> {
    Json(
        tokio::task::spawn_blocking(skein::gitgate::probe_credentials)
            .await
            .unwrap_or_default(),
    )
}

/// Withdraw a grant. Effective immediately for the decision, and within a tick for the token.
pub(super) async fn api_git_grant_revoke(Path((name, repo)): Path<(String, String)>) -> Response {
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
pub(super) struct ScopeReq {
    /// `"repo"`, `"fleet"`, or absent to go back to following the fleet default.
    #[serde(default)]
    scope: Option<String>,
}

/// Flip one box's GitHub scope. Takes effect at the box's next start.
pub(super) async fn api_set_box_git_scope(
    Path(name): Path<String>,
    Json(r): Json<ScopeReq>,
) -> Response {
    match skein::gitgate::set_box_scope(&name, r.scope.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct PrivilegedReq {
    on: bool,
}

/// Make one box the workshop box, or return it to being ordinary. Next start.
pub(super) async fn api_set_box_privileged(
    Path(name): Path<String>,
    Json(r): Json<PrivilegedReq>,
) -> Response {
    match skein::fleet::set_box_privileged(&name, r.on) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}
