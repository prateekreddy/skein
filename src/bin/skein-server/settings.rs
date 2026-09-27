//! The settings panels: skein's own, one box's — tracking, identity, disk and what it declares —
//! and the sync connections a box is provisioned with.

use super::*;

/// The configured work-tracking connections. Never carries a Plane token — only whether one is
/// stored, which is the whole question the settings screen needs answered.
pub(super) async fn api_sync_status() -> Json<skein::tracking::SyncStatus> {
    Json(skein::tracking::sync_status())
}

/// Record which work tracker a box claims through — or that it claims through none.
///
/// Written *before* the box launches, so provisioning finds the answer already there rather than
/// minting a token against the repo's default and having it corrected afterwards. Absent
/// `connection` clears the override and returns the box to its repo's setting.
pub(super) async fn api_set_box_tracking(
    Path(name): Path<String>,
    Json(r): Json<TrackingReq>,
) -> Response {
    // Nineteen `:name` routes in this file open with this line and this one did not, which is how
    // `..%2F..%2Fx` reached `~/.skein/boxes/<name>/tracking`. `tracking::set_box_tracking` refuses
    // it too now — the library is where a guard cannot be skipped by the next caller — and this
    // line stays because the two answer different questions: the library's is 500-shaped ("skein
    // could not do that"), and a name a client sent is a 400.
    if !skein::util::valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid box name").into_response();
    }
    // The same split for the choice: the library refuses it too, and a value a client sent is a 400.
    if let Some(Err(why)) = r
        .connection
        .as_deref()
        .map(skein::tracking::check_box_tracking_choice)
    {
        return (StatusCode::BAD_REQUEST, why).into_response();
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
pub(super) async fn api_set_box_identity(
    Path(name): Path<String>,
    Json(r): Json<IdentityReq>,
) -> Response {
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
pub(super) async fn api_box_settings(Path(name): Path<String>) -> Response {
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
pub(super) async fn api_set_box_disk(Path(name): Path<String>, Json(r): Json<DiskReq>) -> Response {
    match skein::fleet::set_box_disk_limit(&name, r.limit.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct DiskReq {
    #[serde(default)]
    limit: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct IdentityReq {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

#[derive(serde::Deserialize)]
pub(super) struct TrackingReq {
    /// `Some("<id>")` to track there, `Some("")` for untracked, absent to inherit the repo.
    #[serde(default)]
    connection: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ConnectionReq {
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
pub(super) async fn api_save_connection(Json(req): Json<ConnectionReq>) -> Response {
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
pub(super) async fn api_remove_connection(Path(id): Path<String>) -> Response {
    match skein::tracking::remove_connection(&id) {
        Ok(()) => Json(skein::tracking::sync_status()).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Forget one connection's stored token, keeping the connection itself.
pub(super) async fn api_forget_connection_token(Path(id): Path<String>) -> Response {
    match skein::tracking::set_connection_token(&id, "") {
        Ok(()) => Json(skein::tracking::sync_status()).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

/// Mint this box a tracker token and register the `sync` MCP server inside it. Spends a network
/// round trip and creates a real credential, so — like verify — it only ever happens on a click.
pub(super) async fn api_sync_provision(Path(name): Path<String>) -> Response {
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
pub(super) async fn api_sync_refresh(
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

/// Read skein's app settings (the cockpit's toggles).
pub(super) async fn api_settings() -> Json<serde_json::Value> {
    Json(shown(&skein::config::load_config()))
}

/// The settings as the page reads them: the file, plus `held` — which of its fields an environment
/// variable is holding right now, and which variable ([`skein::config::held_by_env`]). The page
/// names the variable beside exactly those, and nothing beside the rest.
///
/// `held` is not a setting and is never written: the save merges into a `Config`, which has no
/// such field, so a page that posts it back loses nothing and keeps nothing.
fn shown(c: &skein::config::Config) -> serde_json::Value {
    let mut v = serde_json::to_value(c).unwrap_or_default();
    if let Some(map) = v.as_object_mut() {
        map.insert(
            "held".into(),
            serde_json::json!(skein::config::held_by_env()),
        );
    }
    v
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
pub(super) async fn api_set_settings(Json(patch): Json<serde_json::Value>) -> Response {
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
        Ok(()) => Json(shown(&c)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A box's tracking choice that is not a connection id, or names none that is configured, is
    /// refused with the value in the answer, and clearing it with `""` still works** (SKEIN-538,
    /// SKEIN-1134).
    ///
    /// Both halves in one test because the guard is `is_empty() || valid_connection_id`, and the
    /// easy way to get it wrong is to drop the first half — which refuses the one legitimate
    /// non-id this route takes. Driven through the handler rather than the library, because the
    /// status is half of what is asserted: a 500 would mean the write was attempted.
    #[test]
    fn a_tracking_choice_that_is_no_connection_id_is_refused_and_an_empty_one_still_clears() {
        let _env = super::env_lock();
        let home = super::scratch_dir("538");
        // The review routes' `EnvPins`, which restores from `Drop`: this binary cannot reach the
        // library's, for the reason its doc gives, and a third copy is how copies drift.
        let mut env = super::review::review_routes::env_pins();
        env.set("SKEIN_HOME", &home);
        let file = home.join("boxes/probe-a/tracking");

        let post = |connection: &str| {
            let r = TrackingReq {
                connection: Some(connection.to_string()),
            };
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a tokio runtime for this test's body")
                .block_on(async {
                    let response = api_set_box_tracking(Path("probe-a".into()), Json(r)).await;
                    let status = response.status();
                    let body = axum::body::to_bytes(response.into_body(), 1 << 16)
                        .await
                        .unwrap();
                    (status, String::from_utf8_lossy(&body).to_string())
                })
        };

        let (status, why) = post("Not A Connection");
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a choice that is no connection id was not refused as a bad request: {why}"
        );
        assert!(
            why.contains("\"Not A Connection\""),
            "the refusal does not name the value it refused: {why}"
        );
        assert!(!file.exists(), "a refused choice was written anyway");

        // Well-formed but naming no configured connection: refused too, and the answer lists the
        // ones that exist (SKEIN-1134). Measured against a configured one that IS saved, so the
        // refusal cannot be passing because nothing is ever saved.
        std::fs::write(
            home.join("connections.json"),
            br#"[{"id":"plane","label":"plane","gateway_url":"https://plane.example"}]"#,
        )
        .unwrap();
        let (status, why) = post("plane2");
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a choice naming no configured connection was not refused as a bad request: {why}"
        );
        assert!(
            why.contains("\"plane2\"") && why.contains("(plane)"),
            "the refusal does not name the value and the connections that exist: {why}"
        );
        assert!(
            !file.exists(),
            "a choice naming no connection was written anyway"
        );
        let (status, why) = post("plane");
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "a configured connection was refused: {why}"
        );
        assert_eq!(
            skein::tracking::box_tracking("probe-a").as_deref(),
            Some("plane"),
            "a configured connection was not recorded as the box's choice"
        );

        let (status, why) = post("");
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "clearing a box's choice with \"\" was refused: {why}"
        );
        assert_eq!(
            skein::tracking::box_tracking("probe-a").as_deref(),
            Some(""),
            "an empty choice did not record \"this box claims no work\""
        );
        let _ = std::fs::remove_dir_all(&home);
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
        let me = server_source();
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
}
