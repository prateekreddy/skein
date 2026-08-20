//! Work tracking: named connections to a `sync` gateway, and wiring one box to one of them.
//!
//! A gateway URL and the personal token that mints tokens at it are ONE thing — a token minted
//! with PAT `A` is only valid where `A` authenticates — so [`SyncConnection`] holds the pair and a
//! repo selects a whole connection rather than describing half of one.
//!
//! Nothing here runs on a tick. Provisioning is an explicit act: it spends a network round trip
//! and mints a real credential.

use crate::config::*;
use crate::repos::{load_repos, repo_for_box, save_repos, Repo};
use crate::sandbox::{guest_write, sbx_guest_output};
use crate::sbx::{box_liveness, Liveness};
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The `sync` gateway a box talks to — its connection's, or empty when it isn't tracked. Trimmed of
/// a trailing slash so `sync_mcp_url` can append `/mcp` without producing `//mcp`.
pub fn sync_gateway_for_box(name: &str) -> String {
    connection_for_box(name)
        .map(|c| c.gateway_url)
        .unwrap_or_default()
}

/// One work-tracking connection: a backlog, and the personal token that mints per-box agent tokens
/// at it.
///
/// The pair is the primitive. A token minted with PAT `A` is only valid at the gateway `A`
/// authenticates to, so a per-repo gateway URL paired with one host-wide PAT — which is what this
/// replaced — is a setting that can only be right when every repo happens to share one Plane. A
/// repo now *selects* a connection instead of describing half of one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConnection {
    /// The stable key repos reference. Never renamed once repos point at it — that would untrack
    /// them silently — which is exactly why there is a separate, freely editable `label`.
    pub id: String,
    pub label: String,
    /// Base URL, no trailing slash. May be empty only for a connection migrated from a host that
    /// had stored a token but never a URL; the cockpit shows that as the gap it is.
    pub gateway_url: String,
}

pub(crate) fn connections_json() -> PathBuf {
    skein_home().join("connections.json")
}

/// A connection's personal token, in its own 0600 file rather than `connections.json`.
///
/// That file is written 0644 and is the exact object the settings screen GETs, so a field there
/// would be handed to every browser tab that opens Settings. The cockpit only ever learns whether
/// a token is set.
pub(crate) fn connection_token_path(id: &str) -> PathBuf {
    skein_home().join("tokens").join(id)
}

/// Where the single host-wide PAT lived before connections existed.
pub(crate) fn legacy_token_path() -> PathBuf {
    skein_home().join("plane-token")
}

/// An id is a filename under `tokens/`, so it is checked like one — no separators, no dots, no
/// leading dash. A token written to a path a caller chose is a path traversal wearing a config
/// field's clothes.
pub(crate) fn valid_connection_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A connection's personal token, or `None` when unset.
pub fn connection_token(id: &str) -> Option<String> {
    if !valid_connection_id(id) {
        return None;
    }
    fs::read_to_string(connection_token_path(id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Store (or, with an empty value, forget) a connection's personal token.
///
/// The mode is set on the temp file *before* the rename, not after: chmod-after-rename leaves a
/// window in which the real path is world-readable, and the whole point of this function is that
/// the window does not exist.
pub fn set_connection_token(id: &str, token: &str) -> Result<(), String> {
    if !valid_connection_id(id) {
        return Err(format!("not a connection id: {id:?}"));
    }
    let path = connection_token_path(id);
    let token = token.trim();
    if token.is_empty() {
        return match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("clearing the token: {e}")),
        };
    }
    let dir = skein_home().join("tokens");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let tmp = dir.join(format!(".{id}.tmp.{}", std::process::id()));
    fs::write(&tmp, token.as_bytes()).map_err(|e| format!("writing the token: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("securing the token: {e}"))?;
    }
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("storing the token: {e}")
    })
}

/// Every configured connection, in the order they were added.
pub fn load_connections() -> Vec<SyncConnection> {
    if let Ok(text) = fs::read_to_string(connections_json()) {
        if let Ok(list) = serde_json::from_str::<Vec<SyncConnection>>(&text) {
            return list;
        }
    }
    migrate_legacy_sync_config()
}

/// Persist the connection list (pretty, atomic).
pub fn save_connections(list: &[SyncConnection]) -> Result<(), String> {
    let home = skein_home();
    fs::create_dir_all(&home).map_err(|e| format!("mkdir {}: {e}", home.display()))?;
    let bytes = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    write_atomic(&connections_json(), &home, &bytes)
}

/// A connection id from a gateway URL: its host, minus the `mcp.`/`api.` everyone's is called.
pub(crate) fn connection_slug(url: &str) -> String {
    let host = url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    let host = host
        .strip_prefix("mcp.")
        .or_else(|| host.strip_prefix("api."))
        .unwrap_or(host);
    let mapped: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug: String = mapped.trim_matches('-').chars().take(64).collect();
    if slug.is_empty() {
        "tracker".into()
    } else {
        slug
    }
}

/// `base`, or `base-2`, `base-3`… — whichever is free.
pub(crate) fn unique_connection_id(base: &str, taken: &[SyncConnection]) -> String {
    let free = |id: &str| !taken.iter().any(|c| c.id == id);
    if free(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| free(id))
        .unwrap_or_else(|| base.to_string())
}

/// One-time promotion of the old single-gateway/single-PAT layout to named connections.
///
/// Deliberately behaviour-preserving, *including the part that was wrong*: a repo pointed at its own
/// gateway was until now wired up with the host-wide PAT, so its migrated connection starts with a
/// copy of that same token. Starting it empty would be the more principled thing and would break a
/// fleet that works today; the cockpit flags every connection so a wrong one is one field away.
///
/// Ordering is the crash-safety story. Tokens, then `connections.json`, then `repos.json`, and only
/// then the legacy state — so an interrupted run leaves the old layout intact and simply migrates
/// again next time.
pub(crate) fn migrate_legacy_sync_config() -> Vec<SyncConnection> {
    let cfg = load_config();
    let legacy_url = cfg
        .sync_gateway_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    let legacy_token = fs::read_to_string(legacy_token_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut repos = load_repos();
    let repo_urls: Vec<String> = repos
        .iter()
        .map(|r| r.sync_gateway_url.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .collect();
    // A fresh install has nothing to migrate, and writing it a file it never asked for would be the
    // migration inventing state rather than moving it.
    if legacy_url.is_empty() && legacy_token.is_none() && repo_urls.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<SyncConnection> = Vec::new();
    let adopt = |out: &mut Vec<SyncConnection>, url: &str| -> String {
        if let Some(existing) = out.iter().find(|c| c.gateway_url == url) {
            return existing.id.clone();
        }
        let id = unique_connection_id(&connection_slug(url), out);
        out.push(SyncConnection {
            id: id.clone(),
            label: connection_slug(url),
            gateway_url: url.to_string(),
        });
        id
    };
    let default_id = if legacy_url.is_empty() {
        // A stored token with no URL anywhere: keep the half that exists rather than dropping it,
        // so the screen still says "a token is stored, it has nowhere to point".
        legacy_token.is_some().then(|| {
            out.push(SyncConnection {
                id: "default".into(),
                label: "default".into(),
                gateway_url: String::new(),
            });
            "default".to_string()
        })
    } else {
        Some(adopt(&mut out, &legacy_url))
    };
    for repo in repos.iter_mut() {
        let own = repo
            .sync_gateway_url
            .trim()
            .trim_end_matches('/')
            .to_string();
        repo.sync_connection = if own.is_empty() {
            default_id.clone().unwrap_or_default()
        } else {
            adopt(&mut out, &own)
        };
        repo.sync_gateway_url.clear();
    }

    if let Some(token) = &legacy_token {
        for c in &out {
            if let Err(e) = set_connection_token(&c.id, token) {
                eprintln!("skein: migrating the Plane token to {}: {e}", c.id);
                return Vec::new(); // leave the old layout alone; try again next call
            }
        }
    }
    if let Err(e) = save_connections(&out) {
        eprintln!("skein: writing connections.json: {e}");
        return Vec::new();
    }
    if let Err(e) = save_repos(&repos) {
        eprintln!("skein: recording which connection each repo uses: {e}");
    }
    // Under the lock, and re-read there: this runs on a migration path that may be racing an
    // ordinary settings save, and clearing one field by writing back a whole snapshot is how the
    // other save is lost.
    if let Err(e) = crate::config::update_config(|c| {
        c.sync_gateway_url.clear();
        Ok(())
    }) {
        eprintln!("skein: clearing the superseded gateway setting: {e}");
    }
    let _ = fs::remove_file(legacy_token_path());
    out
}

/// Create or update a connection. `id: None` mints one from the URL's host.
///
/// `token: None` leaves whatever is stored alone — a blank field on a settings screen means "I came
/// here to change something else", never "forget my credential". Forgetting is its own act.
pub fn upsert_connection(
    id: Option<&str>,
    label: &str,
    gateway_url: &str,
    token: Option<&str>,
) -> Result<SyncConnection, String> {
    let url = gateway_url.trim().trim_end_matches('/').to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("a gateway URL has to start with http:// or https://".into());
    }
    let list = load_connections();
    let id = match id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => {
            if !valid_connection_id(id) {
                return Err("a connection id is lowercase letters, digits and dashes".into());
            }
            id.to_string()
        }
        None => unique_connection_id(&connection_slug(&url), &list),
    };
    let label = {
        let l = label.trim();
        if l.is_empty() {
            connection_slug(&url)
        } else {
            l.chars().take(80).collect()
        }
    };
    let conn = SyncConnection {
        id: id.clone(),
        label,
        gateway_url: url,
    };
    // The token first: a connection listed as ready before its credential landed would send someone
    // to press Track work against a gateway that will refuse them.
    if let Some(t) = token {
        set_connection_token(&id, t)?;
    }
    // Read-modify-write under the lock, and the list is re-read there: `list` above was loaded to
    // work out the id and the label, and adding this connection onto that snapshot would drop any
    // connection added since.
    crate::util::update_json(&connections_json(), |all: &mut Vec<SyncConnection>| {
        match all.iter_mut().find(|c| c.id == id) {
            Some(existing) => *existing = conn.clone(),
            None => all.push(conn.clone()),
        }
        Ok(())
    })?;
    Ok(conn)
}

/// Forget a connection and its token.
///
/// Refused while any repo still selects it, naming them: silently untracking three repos to honour
/// one click is a bigger edit than the click asked for.
pub fn remove_connection(id: &str) -> Result<(), String> {
    let users: Vec<String> = load_repos()
        .into_iter()
        .filter(|r| r.sync_connection == id)
        .map(|r| r.id)
        .collect();
    if !users.is_empty() {
        return Err(format!(
            "{} still {} it — point {} at another connection first",
            users.join(", "),
            if users.len() == 1 { "uses" } else { "use" },
            if users.len() == 1 {
                "that repo"
            } else {
                "those repos"
            },
        ));
    }
    crate::util::update_json(&connections_json(), |all: &mut Vec<SyncConnection>| {
        let before = all.len();
        all.retain(|c| c.id != id);
        match all.len() == before {
            true => Err(format!("no work-tracking connection called {id:?}")),
            false => Ok(()),
        }
    })?;
    let _ = set_connection_token(id, "");
    Ok(())
}

/// The connection a repo claims work through, if it has one.
pub fn connection_for_repo(repo: &Repo) -> Option<SyncConnection> {
    let list = load_connections();
    let chosen = repo.sync_connection.trim();
    if !chosen.is_empty() {
        return list.into_iter().find(|c| c.id == chosen);
    }
    // Belt for a migration interrupted between `connections.json` and `repos.json`: the repo still
    // carries the URL it used to, and that URL is now a connection.
    let legacy = repo.sync_gateway_url.trim().trim_end_matches('/');
    (!legacy.is_empty())
        .then(|| list.into_iter().find(|c| c.gateway_url == legacy))
        .flatten()
}

/// The connection a box claims work through.
///
/// A box whose repo is registered gets that repo's answer, and "none selected" means not tracked —
/// an explicit setting, not a gap to fill in. A box belonging to *no* registered repo (skein's old
/// single-repo layout) falls back to the sole connection when there is exactly one, because then
/// there is nothing to guess. With two, guessing is how a token gets minted against the wrong
/// backlog.
/// A box's own answer, when it was given one at creation, overrides its repo's.
///
/// The repo-level setting is a *default*, and it was the only setting there was — so a box on a
/// tracked repo was tracked whether or not that made sense for the work, and the only way to say
/// otherwise was to change the setting for every box of that repo at once.
pub fn connection_for_box(name: &str) -> Option<SyncConnection> {
    if let Some(chosen) = box_tracking(name) {
        // An empty override is not a missing one: it is "this box does not claim work".
        let chosen = chosen.trim().to_string();
        return (!chosen.is_empty())
            .then(|| load_connections().into_iter().find(|c| c.id == chosen))
            .flatten();
    }
    match repo_for_box(name) {
        Some(repo) => connection_for_repo(&repo),
        None => {
            let mut list = load_connections();
            (list.len() == 1).then(|| list.remove(0))
        }
    }
}

/// Where a box's own tracking choice is recorded — beside its other durable host-side state, so it
/// survives the box being rebuilt, resized or migrated.
fn box_tracking_path(name: &str) -> PathBuf {
    skein_home().join("boxes").join(name).join("tracking")
}

/// The box's own choice: `Some(id)` to claim through that connection, `Some("")` to claim through
/// none, `None` when it never made one and inherits the repo's.
pub fn box_tracking(name: &str) -> Option<String> {
    fs::read_to_string(box_tracking_path(name))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Record (or clear) that choice. `None` returns the box to its repo's default.
pub fn set_box_tracking(name: &str, choice: Option<&str>) -> Result<(), String> {
    let path = box_tracking_path(name);
    let Some(choice) = choice else {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("clearing {}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    };
    let dir = path.parent().ok_or("no parent directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    write_atomic(&path, dir, choice.trim().as_bytes())
}

/// The gateway's MCP endpoint for a configured base URL. Accepts either spelling, so a pasted
/// `/mcp` endpoint does not silently become `/mcp/mcp`.
pub fn sync_mcp_url(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with("/mcp") {
        base.to_string()
    } else {
        format!("{base}/mcp")
    }
}

/// Pull the project uuid out of whatever the user pasted — a full Plane project URL
/// (`https://plane.host/<workspace>/projects/<uuid>/issues`) or the bare uuid.
///
/// Scanning for the shape rather than parsing the URL is deliberate: the uuid is the only part
/// skein needs, and Plane's URL layout is not skein's to depend on.
pub fn plane_project_id(raw: &str) -> Option<String> {
    let bytes: Vec<char> = raw.chars().collect();
    let is_hex = |c: char| c.is_ascii_hexdigit();
    // 8-4-4-4-12
    let groups = [8usize, 4, 4, 4, 12];
    'start: for start in 0..bytes.len() {
        let mut i = start;
        for (g, len) in groups.iter().enumerate() {
            if g > 0 {
                if bytes.get(i) != Some(&'-') {
                    continue 'start;
                }
                i += 1;
            }
            for _ in 0..*len {
                match bytes.get(i) {
                    Some(&c) if is_hex(c) => i += 1,
                    _ => continue 'start,
                }
            }
        }
        // Reject a longer hex run that merely contains a uuid-shaped prefix.
        if bytes.get(i).is_some_and(|&c| is_hex(c) || c == '-') {
            continue 'start;
        }
        if start > 0 && bytes.get(start - 1).is_some_and(|&c| is_hex(c) || c == '-') {
            continue 'start;
        }
        return Some(bytes[start..i].iter().collect::<String>().to_lowercase());
    }
    None
}

/// One connection as the cockpit sees it. No secret crosses this line — only whether one is
/// present, because "is it configured?" is the whole question a settings screen has to answer.
#[derive(Debug, Clone, Serialize)]
pub struct SyncConnectionView {
    pub id: String,
    pub label: String,
    pub gateway_url: String,
    pub token_set: bool,
    /// Both halves present, so a box on this connection can actually be wired up.
    pub ready: bool,
    /// The repos that select it — what a delete would untrack, said before the click rather than
    /// after.
    pub repos: Vec<String>,
}

/// What the cockpit is told about work tracking.
#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub connections: Vec<SyncConnectionView>,
    /// At least one connection can mint — "is work tracking usable at all", for the nav pill.
    pub ready: bool,
}

pub fn sync_status() -> SyncStatus {
    let repos = load_repos();
    let connections: Vec<SyncConnectionView> = load_connections()
        .into_iter()
        .map(|c| {
            let token_set = connection_token(&c.id).is_some();
            SyncConnectionView {
                ready: token_set && !c.gateway_url.is_empty(),
                token_set,
                repos: repos
                    .iter()
                    .filter(|r| r.sync_connection == c.id)
                    .map(|r| r.id.clone())
                    .collect(),
                id: c.id,
                label: c.label,
                gateway_url: c.gateway_url,
            }
        })
        .collect();
    SyncStatus {
        ready: connections.iter().any(|c| c.ready),
        connections,
    }
}

/// A minted per-agent gateway token. The token is returned once and never stored by skein — it goes
/// straight into the box that will use it.
#[derive(Debug, Clone)]
pub struct MintedToken {
    pub token: String,
    /// The namespaced name the gateway gave this agent (`<owner>/<box>`).
    pub agent: String,
}

/// Mint a gateway token for one box, from the operator's Plane personal token.
///
/// curl rather than an HTTP crate: skein already shells out to `gh` and `sbx`, and a work-tracking
/// setting is not worth a TLS stack in the dependency tree. The PAT travels in the child's
/// environment, not its argv — argv is world-readable in `ps`, and this is the one credential whose
/// leak would let someone bypass every lease in the fleet.
pub fn sync_mint_token(
    agent: &str,
    project_id: Option<&str>,
    conn: &SyncConnection,
) -> Result<MintedToken, String> {
    // The whole connection, not a URL: the PAT that authorises the mint and the gateway that
    // performs it are one credential in two halves, and pairing half of one with half of another is
    // how a box ends up holding a token for a backlog it is not talking to.
    let base = conn.gateway_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err(format!(
            "the {} connection has no gateway URL yet",
            conn.label
        ));
    }
    let token = connection_token(&conn.id)
        .ok_or_else(|| format!("no Plane token stored for {}", conn.label))?;
    let body = match project_id {
        Some(p) => format!(
            r#"{{"agent":{},"projectId":{}}}"#,
            json_str(agent),
            json_str(p)
        ),
        None => format!(r#"{{"agent":{}}}"#, json_str(agent)),
    };
    let script = format!(
        "curl -sS -m 30 -X POST {}/v1/agent-tokens \
         -H \"Authorization: Bearer $SKEIN_PLANE_TOKEN\" \
         -H 'Content-Type: application/json' -d {}",
        sh_quote(&base),
        sh_quote(&body)
    );
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&script)
        .env("SKEIN_PLANE_TOKEN", &token);
    let out = bounded_output(&mut command, "curl", Duration::from_secs(45))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("the gateway could not be reached ({})", out.status)
        } else {
            detail
        });
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|_| gateway_said(&stdout))?;
    match parsed.get("token").and_then(|v| v.as_str()) {
        Some(token) => Ok(MintedToken {
            token: token.to_string(),
            agent: parsed
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or(agent)
                .to_string(),
        }),
        // A refusal is JSON too, and its `message` is written for a human — surface that rather
        // than "unexpected response", which sends the reader to the wrong place entirely.
        None => Err(gateway_said(&stdout)),
    }
}

/// The gateway's own words when it refuses, trimmed to something a toast can hold.
pub(crate) fn gateway_said(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
            let recovery = v.get("recovery").and_then(|r| r.as_str()).unwrap_or("");
            return if recovery.is_empty() {
                m.to_string()
            } else {
                format!("{m} — {recovery}")
            };
        }
    }
    let one = body.trim().lines().next().unwrap_or("").trim();
    if one.is_empty() {
        "the gateway returned nothing".into()
    } else {
        one.chars().take(200).collect()
    }
}

/// Wire one box to the work tracker: mint its token, write it into the box, and register the MCP
/// server there.
///
/// The credential is written over the box's stdin, never as a command argument — `sbx exec`'s argv
/// is visible in `ps` on the host, and a token pasted into a shell history is a token that outlives
/// the box. Inside the box it lands in the box-private `~/.config/sync/env` at 0600, deliberately
/// not in the shared `.claude` store, which is mounted live into every other box for the repo.
pub fn sync_provision_box(name: &str) -> Result<String, String> {
    // Readiness is per BOX, not global: a repo on its own connection is ready even when the one
    // every other repo uses is not, which is exactly the "this product tracks somewhere else" case.
    let conn = connection_for_box(name).ok_or(
        "this repo isn't wired to a work tracker — pick a connection for it in Settings → Repositories",
    )?;
    let gateway = conn.gateway_url.clone();
    if gateway.is_empty() {
        return Err(format!(
            "the {} connection has no gateway URL yet",
            conn.label
        ));
    }
    if connection_token(&conn.id).is_none() {
        return Err(format!(
            "no Plane token stored for {} — add it in Settings → Work tracking",
            conn.label
        ));
    }
    if box_liveness(name) != Some(Liveness::Running) {
        return Err(format!("{name} is not running"));
    }
    let project = repo_for_box(name)
        .map(|r| r.plane_project)
        .and_then(|p| plane_project_id(&p));
    let minted = sync_mint_token(name, project.as_deref(), &conn)?;

    // One heredoc-free write: the shell reads the file body from its own stdin, so nothing about
    // the token appears in any argument list on either side of the boundary.
    let env_file = format!(
        "# written by skein — box-private, never the shared store\nexport SYNC_GATEWAY_URL={}\nexport SYNC_AGENT_TOKEN={}\n",
        sh_quote(&gateway),
        sh_quote(&minted.token)
    );
    let script = "umask 077; mkdir -p \"$HOME/.config/sync\"; cat > \"$HOME/.config/sync/env\"; \
                  chmod 600 \"$HOME/.config/sync/env\"";
    guest_write(name, script, &env_file, Duration::from_secs(30))?;

    // Registration itself is the store installer's job, not a second copy of that logic here: it is
    // the thing that knows both runtimes, and it is the same script the kit runs at startup, so a
    // box wired up by hand and one wired up on creation cannot end up differently configured. FORCE
    // is exactly the "a new token has arrived" case it exists for.
    //
    // Resolved through the box's own `.claude`, because that is the store however it got mounted —
    // a symlinked store, a repo that ships its own `.claude` with `skein/` linked inside it, or a
    // direct-mode checkout all land here.
    let install = "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; \
         store=\"$root/.claude\"; \
         if [ -L \"$store/skein\" ]; then store=\"$(dirname \"$(readlink \"$store/skein\")\")\"; \
         elif [ -L \"$store\" ]; then store=\"$(readlink -f \"$store\")\"; fi; \
         script=\"$store/skein/bin/sync-install.sh\"; \
         if [ -r \"$script\" ]; then SKEIN_SYNC_FORCE=1 bash \"$script\" 2>&1; \
         else echo 'skein: no sync installer in this box'\\''s store'; fi";
    let report = sbx_guest_output(name, install, Duration::from_secs(60)).unwrap_or_default();
    if report.contains("work tracking ready") {
        Ok(format!("{name} is tracking work as {}", minted.agent))
    } else {
        // The token is in place either way, so say what is true: the credential landed, the
        // registration did not confirm. Reporting success here would be the exact silent-wrong-
        // result this whole surface exists to avoid.
        let last = report.trim().lines().last().unwrap_or("").trim();
        Err(format!(
            "token written, but registration did not confirm{} — the box can still be registered by hand against {}",
            if last.is_empty() {
                String::new()
            } else {
                format!(": {last}")
            },
            sync_mcp_url(&gateway)
        ))
    }
}

/// Does this repo's store hold work-tracking documents newer than the ones installed from it?
///
/// Free, and host-side on purpose: the skill and the memory live in the store, so answering costs
/// two file reads and never touches a box. That matters because this runs on every fleet snapshot,
/// and a signal that woke every box to ask it a question would cost more than the correction it is
/// advertising.
///
/// It cannot see the CLAUDE.md block, which lives inside each box's own clone. So this is "a newer
/// version exists", not "this box is stale" — the box-level truth comes back from the refresh
/// itself, which is the only thing that can read it.
pub fn sync_docs_available(store: &Path) -> bool {
    let newer =
        |installed: PathBuf, reference: PathBuf| match (fs::read(&installed), fs::read(&reference))
        {
            // Absent means never installed here, which is Track work's job rather than a refresh's.
            (Ok(a), Ok(b)) => a != b,
            _ => false,
        };
    newer(
        store.join("skills/work-tracking/SKILL.md"),
        store.join("skein/sync/work-tracking.skill.md"),
    ) || newer(
        store.join("memory/work-tracking.md"),
        store.join("skein/sync/work-tracking.memory.md"),
    )
}

/// Re-apply the work-tracking documents to a box that already has them.
///
/// `sync_provision_box` installs once and hands off, which is what lets a box own its config — but
/// it left corrections undeliverable. This is the delivery, under one rule the box can rely on:
/// **skein never overwrites an edit it can see.** Only documents still byte-identical to what skein
/// installed are rewritten; anything the box changed is reported and kept.
///
/// `force` covers boxes wired up before skein recorded what it installed. For those, "stale" and
/// "edited" are genuinely indistinguishable, so the human pressing the button is the missing
/// evidence. It still refuses documents known to be edited.
pub fn sync_refresh_box(name: &str, force: bool) -> Result<String, String> {
    if box_liveness(name) != Some(Liveness::Running) {
        return Err(format!("{name} is not running"));
    }
    // Same store resolution as provisioning, for the same reason: whichever way this repo's `.claude`
    // is mounted, the box's own view of it is the one that is right.
    let script = format!(
        "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; \
         store=\"$root/.claude\"; \
         if [ -L \"$store/skein\" ]; then store=\"$(dirname \"$(readlink \"$store/skein\")\")\"; \
         elif [ -L \"$store\" ]; then store=\"$(readlink -f \"$store\")\"; fi; \
         script=\"$store/skein/bin/sync-refresh.sh\"; \
         if [ -r \"$script\" ]; then bash \"$script\"{}; \
         else echo 'skein: no refresh script in this box'\\''s store' >&2; fi",
        if force { " --force" } else { "" }
    );
    // stderr deliberately not merged: stdout is the machine-readable report and stderr is the prose.
    let report = sbx_guest_output(name, &script, Duration::from_secs(60))?;
    Ok(describe_refresh(&report, force))
}

/// Turn the script's `name<TAB>state` lines into one sentence for the cockpit.
///
/// Its own function so the wording is testable without a box. The states are the ones found *before
/// the run acted*, so what counts as refreshed depends on what this run was willing to write —
/// hence `force` here rather than a second pass in the script.
///
/// The `yours` count is never folded into the total. "3 refreshed" when one was declined would be a
/// lie in the direction that costs most, since the whole promise is that skein left the box's edits
/// alone.
pub(crate) fn describe_refresh(report: &str, force: bool) -> String {
    let (mut done, mut kept, mut unknown) = (0, 0, 0);
    for line in report.lines() {
        match line.trim().rsplit_once('\t').map(|(_, s)| s.trim()) {
            Some("stale") => done += 1,
            Some("yours") => kept += 1,
            Some("unknown") if force => done += 1,
            Some("unknown") => unknown += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if done > 0 {
        parts.push(format!("refreshed {done}"));
    }
    if unknown > 0 {
        parts.push(format!(
            "{unknown} predates skein recording what it installed — use Replace to take those"
        ));
    }
    if kept > 0 {
        parts.push(format!("kept {kept} the box had edited"));
    }
    if parts.is_empty() {
        "already up to date".into()
    } else {
        parts.join("; ")
    }
}

/// Retire a box's tracker token at the gateway.
///
/// A destroyed box takes its filesystem with it but not its credential: the token stays valid
/// wherever it was copied, and it is a bearer token — nothing about it is bound to the box. So
/// teardown revokes it, from the same PAT that minted it (the gateway only lets you revoke your
/// own).
///
/// Silent no-op when tracking is not configured, so a destroy stays quiet for anyone not using it.
/// Never fatal: `sbx rm` has already succeeded by the time this runs, and refusing to finish a
/// teardown over a failed revocation would leave a box on the board that no longer exists.
pub fn sync_revoke_token(agent: &str) -> Result<(), String> {
    // The box's own connection: a token is only revocable at the gateway that minted it, and the
    // PAT that can revoke it is that connection's. Reading a host-wide default here would send the
    // DELETE to a gateway that never issued the token, and report success for a live credential.
    let Some(conn) = connection_for_box(agent) else {
        return Ok(());
    };
    let base = conn.gateway_url.trim().trim_end_matches('/').to_string();
    let Some(token) = connection_token(&conn.id) else {
        return Ok(());
    };
    if base.is_empty() {
        return Ok(());
    }
    let script = format!(
        "curl -sS -m 20 -X DELETE {}/v1/agent-tokens/{} \
         -H \"Authorization: Bearer $SKEIN_PLANE_TOKEN\"",
        sh_quote(&base),
        sh_quote(agent)
    );
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&script)
        .env("SKEIN_PLANE_TOKEN", &token);
    let out = bounded_output(&mut command, "curl", Duration::from_secs(30))?;
    let body = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!("the gateway could not be reached ({})", out.status));
    }
    revocation_outcome(&body)
}

/// Did the gateway actually retire the token?
///
/// Its own function because curl exiting 0 is not the claim — the claim is that the *gateway* said
/// it revoked something. A 404 for a box that was never wired up, an HTML error page from a proxy,
/// or a refusal all arrive as a successful transfer, and reading any of them as "revoked" would let
/// a live credential quietly outlive the box it was minted for.
pub(crate) fn revocation_outcome(body: &str) -> Result<(), String> {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) if v.get("revoked").is_some() => Ok(()),
        _ => Err(gateway_said(body)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::ensure_store;
    use crate::repos::set_repo_settings;
    use crate::sandbox::destroy_box;
    use crate::testutil::*;
    use std::env;

    /// A box chooses at creation; the repo's setting is the default it starts from.
    ///
    /// Without this, "use sync for this box?" could only be answered for every box of a repo at
    /// once — so one box doing untracked exploratory work meant either untracking its repo or
    /// minting it a token against a backlog it will never claim from.
    #[test]
    fn a_box_can_claim_somewhere_other_than_its_repo_or_nowhere_at_all() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        upsert_connection(
            Some("team"),
            "team",
            "https://team.example",
            Some("pat_team"),
        )
        .unwrap();
        upsert_connection(
            Some("solo"),
            "solo",
            "https://solo.example",
            Some("pat_solo"),
        )
        .unwrap();
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: "team".into(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        assert_eq!(
            connection_for_box("web-main").map(|c| c.id),
            Some("team".to_string()),
            "no choice of its own means the repo's"
        );

        set_box_tracking("web-main", Some("solo")).unwrap();
        assert_eq!(
            connection_for_box("web-main").map(|c| c.id),
            Some("solo".to_string()),
            "the box's own choice wins over its repo's"
        );
        assert_eq!(
            connection_for_box("web-other").map(|c| c.id),
            Some("team".to_string()),
            "and it is one box's choice, not the repo's — its siblings are untouched"
        );

        // Empty is a decision, not an absence: this box claims nowhere.
        set_box_tracking("web-main", Some("")).unwrap();
        assert!(connection_for_box("web-main").is_none());
        assert_eq!(sync_gateway_for_box("web-main"), "");

        // Clearing hands the box back to its repo, rather than leaving it permanently untracked.
        set_box_tracking("web-main", None).unwrap();
        assert_eq!(
            connection_for_box("web-main").map(|c| c.id),
            Some("team".to_string())
        );
        // Idempotent: clearing a box that never chose is not an error.
        set_box_tracking("web-main", None).unwrap();
        // Restored, or the next test to take `env_lock` inherits a SKEIN_HOME naming a
        // directory this test's guard has already removed — and writes through it, which
        // recreates the tree as a leak nobody owns.
        std::env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn a_repo_claims_work_through_the_connection_it_picks() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        upsert_connection(
            Some("shared"),
            "shared",
            "https://shared.example",
            Some("pat_shared"),
        )
        .unwrap();
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: "shared".into(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        assert_eq!(sync_gateway_for_box("web-main"), "https://shared.example");
        // Two products tracked in different Plane instances can't share a claim namespace, and the
        // token a box carries is only valid at the gateway that minted it — so switching backlogs
        // switches the credential too, which is exactly what picking a whole connection buys.
        upsert_connection(Some("own"), "own", "https://own.example/", Some("pat_own")).unwrap();
        set_repo_settings("web", None, Some("own"), None).unwrap();
        assert_eq!(
            sync_gateway_for_box("web-main"),
            "https://own.example",
            "trailing slash trimmed so /mcp doesn't double up"
        );
        assert_eq!(
            connection_for_box("web-main").map(|c| connection_token(&c.id).unwrap()),
            Some("pat_own".to_string()),
            "the PAT that mints has to be the one that authenticates AT that gateway"
        );
        assert_eq!(
            sync_mcp_url(&sync_gateway_for_box("web-main")),
            "https://own.example/mcp"
        );
        // Clearing means not tracked — an explicit setting, not a gap to be filled by a default.
        set_repo_settings("web", None, Some(""), None).unwrap();
        assert!(connection_for_box("web-main").is_none());
        assert_eq!(sync_gateway_for_box("web-main"), "");
        // A selection naming nothing would read as "tracked" and behave as "not tracked".
        assert!(set_repo_settings("web", None, Some("nope"), None).is_err());
        // One call can carry every field, and the fields don't disturb each other.
        set_repo_settings("web", None, Some("own"), None).unwrap();
        let saved = load_repos().into_iter().find(|r| r.id == "web").unwrap();
        assert_eq!(saved.sync_connection, "own");
        assert_eq!(saved.plane_project, "", "a field left None is left alone");
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // A box that belongs to no registered repo is skein's old single-repo layout. One connection is
    // unambiguous; two is a guess, and the wrong guess mints a real credential against the wrong
    // backlog — so it declines rather than picking.
    #[test]
    fn an_unregistered_box_only_inherits_a_connection_when_there_is_no_choice() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        assert!(
            connection_for_box("stray-main").is_none(),
            "none configured"
        );
        upsert_connection(Some("one"), "one", "https://one.example", None).unwrap();
        assert_eq!(
            connection_for_box("stray-main").map(|c| c.id),
            Some("one".into())
        );
        upsert_connection(Some("two"), "two", "https://two.example", None).unwrap();
        assert!(
            connection_for_box("stray-main").is_none(),
            "two backlogs and no repo to say which — refuse rather than guess"
        );
        // A *registered* repo with nothing picked is not a gap: it is "not tracked", and no number
        // of connections may override that.
        save_repos(&[Repo {
            id: "stray".into(),
            source: "/s".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        remove_connection("two").unwrap();
        assert!(
            connection_for_box("stray-main").is_none(),
            "an explicit 'not tracked' outranks a sole connection"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // The upgrade path off the old layout, where the gateway was per-repo and the PAT was one file
    // for the whole host. Silently dropping either half would leave a fleet that tracked work
    // yesterday and quietly stopped today.
    #[test]
    fn the_old_single_token_layout_becomes_named_connections() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        fs::create_dir_all(&*dir).unwrap();
        save_config(&Config {
            sync_gateway_url: "https://mcp.shared.example".into(),
            ..Default::default()
        })
        .unwrap();
        fs::write(dir.join("plane-token"), "plane_api_secret\n").unwrap();
        let repo = |id: &str, gw: &str| Repo {
            id: id.into(),
            source: format!("/src/{id}"),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: gw.into(),
        };
        save_repos(&[
            repo("web", ""),
            repo("bridge", "https://mcp.other.example/"),
            repo("also", "https://mcp.other.example"),
        ])
        .unwrap();

        let conns = load_connections();
        assert_eq!(
            conns.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["shared-example", "other-example"],
            "one connection per distinct gateway, named after its host"
        );
        let by_repo = |id: &str| {
            load_repos()
                .into_iter()
                .find(|r| r.id == id)
                .unwrap()
                .sync_connection
        };
        assert_eq!(by_repo("web"), "shared-example", "inherited the default");
        assert_eq!(by_repo("bridge"), "other-example");
        assert_eq!(
            by_repo("also"),
            "other-example",
            "the same URL twice is one connection, not two"
        );
        // Behaviour-preserving, including the part that was wrong: a repo on its own gateway was
        // being wired up with the host-wide PAT, so its connection starts with that same token.
        for c in &conns {
            assert_eq!(connection_token(&c.id).as_deref(), Some("plane_api_secret"));
        }
        // The legacy state is gone, so this runs exactly once.
        assert!(!dir.join("plane-token").exists());
        assert_eq!(load_config().sync_gateway_url, "");
        assert_eq!(load_repos()[0].sync_gateway_url, "");
        let again = load_connections();
        assert_eq!(
            again.len(),
            2,
            "second call reads the file, migrates nothing"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    #[test]
    fn a_fresh_host_is_left_alone_by_the_migration() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        assert!(load_connections().is_empty());
        assert!(
            !dir.join("connections.json").exists(),
            "nothing to migrate ⇒ no file invented"
        );
        env::remove_var("SKEIN_HOME");
    }

    // Removing a connection is a bigger edit than it looks: every repo pointing at it silently
    // stops tracking work. So it is refused, by name, rather than performed.
    #[test]
    fn a_connection_in_use_is_not_removed_out_from_under_its_repos() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        upsert_connection(
            Some("shared"),
            "shared",
            "https://shared.example",
            Some("pat"),
        )
        .unwrap();
        save_repos(&[Repo {
            id: "web".into(),
            source: "/src/web".into(),
            work: "/w".into(),
            store: dir.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: "shared".into(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();
        let e = remove_connection("shared").unwrap_err();
        assert!(e.contains("web"), "say which repo would lose tracking: {e}");
        assert!(remove_connection("ghost").is_err());
        set_repo_settings("web", None, Some(""), None).unwrap();
        remove_connection("shared").unwrap();
        assert!(load_connections().is_empty());
        assert!(
            connection_token("shared").is_none(),
            "the credential goes with the connection — a token nothing points at is one nobody rotates"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // An id becomes a filename under `tokens/`, so it is checked like one.
    #[test]
    fn a_connection_id_can_never_be_a_path() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        for bad in ["../evil", "a/b", ".ssh", "-lead", "UPPER"] {
            assert!(
                upsert_connection(Some(bad), "x", "https://x.example", Some("pat")).is_err(),
                "accepted {bad:?}"
            );
            assert!(set_connection_token(bad, "pat").is_err(), "wrote {bad:?}");
        }
        assert!(set_connection_token("", "pat").is_err());
        assert!(upsert_connection(None, "x", "not-a-url", None).is_err());
        // A derived id is always safe, however hostile the URL.
        let c = upsert_connection(None, "", "https://plane.example.com/mcp/", None).unwrap();
        assert_eq!(c.id, "plane-example-com", "mcp. stripped, dots to dashes");
        assert_eq!(c.label, "plane-example-com", "blank label ⇒ the host");
        assert_eq!(c.gateway_url, "https://plane.example.com/mcp");
        let d = upsert_connection(None, "", "https://plane.example.com", None).unwrap();
        assert_eq!(
            d.id, "plane-example-com-2",
            "a taken id is suffixed, never reused"
        );
        env::remove_var("SKEIN_HOME");
    }

    // A project id is what an agent token binds to, and the only place a human ever sees one is
    // the Plane URL they are already looking at — so pasting that URL has to work.
    #[test]
    fn a_plane_project_is_read_out_of_whatever_was_pasted() {
        let id = "1e2a3b4c-5d6e-4f70-8912-abcdefabcdef";
        assert_eq!(plane_project_id(id).as_deref(), Some(id));
        assert_eq!(
            plane_project_id(&format!(
                "https://plane.example.net/acme/projects/{id}/issues"
            ))
            .as_deref(),
            Some(id)
        );
        assert_eq!(plane_project_id(&id.to_uppercase()).as_deref(), Some(id));
        assert_eq!(plane_project_id("  \n").as_deref(), None);
        assert_eq!(plane_project_id("my-project").as_deref(), None);
        // The dangerous near-miss: a longer hex run whose first 36 chars are uuid-shaped. Accepting
        // it would store a project that authenticates and then 403s inside a session hours later.
        assert_eq!(plane_project_id(&format!("{id}0")).as_deref(), None);
        assert_eq!(plane_project_id(&format!("0{id}")).as_deref(), None);
    }

    // The Plane token is the one credential whose leak would let someone bypass every lease in the
    // fleet, so where it lives and who can read it is a claim worth a test rather than a comment.
    #[test]
    fn a_connections_token_is_private_to_this_host_and_never_in_a_config_file() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        assert!(!sync_status().ready, "nothing configured ⇒ not ready");

        upsert_connection(
            Some("shared"),
            "shared",
            "https://plane.example.com",
            None,
        )
        .unwrap();
        assert!(!sync_status().ready, "a gateway alone cannot mint anything");
        set_connection_token("shared", "  plane_api_secret  ").unwrap();
        assert_eq!(
            connection_token("shared").as_deref(),
            Some("plane_api_secret"),
            "trimmed"
        );

        // Not in connections.json — the object the settings screen GETs.
        let listed = fs::read_to_string(dir.join("connections.json")).unwrap();
        assert!(
            !listed.contains("plane_api_secret"),
            "the token must never be written where the settings form can read it: {listed}"
        );
        // ...and not in what the cockpit is told either.
        let status = sync_status();
        assert!(status.ready && status.connections[0].token_set);
        let json = serde_json::to_string(&status).unwrap();
        assert!(
            !json.contains("plane_api_secret"),
            "leaked to the browser: {json}"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("tokens").join("shared"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the token file must be owner-only");
        }

        // A blank token on a save means "unchanged" — opening Settings to fix a URL must not
        // silently delete the credential that makes the connection work.
        upsert_connection(
            Some("shared"),
            "renamed",
            "https://plane.example.com",
            None,
        )
        .unwrap();
        assert!(
            connection_token("shared").is_some(),
            "a save is not a forget"
        );
        assert_eq!(sync_status().connections[0].label, "renamed");

        set_connection_token("shared", "").unwrap();
        assert!(connection_token("shared").is_none(), "empty forgets it");
        assert!(
            set_connection_token("shared", "").is_ok(),
            "forgetting twice is not an error"
        );
        assert!(!sync_status().ready);
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    #[test]
    fn the_gateway_endpoint_is_the_same_whichever_url_was_pasted() {
        let want = "https://plane.example.com/mcp";
        for pasted in [
            "https://plane.example.com",
            "https://plane.example.com/",
            "https://plane.example.com/mcp",
            "  https://plane.example.com/mcp  ",
        ] {
            assert_eq!(sync_mcp_url(pasted), want, "for {pasted:?}");
        }
    }

    // A refusal from the gateway is JSON written for a human. Showing "unexpected response" instead
    // sends the reader to the wrong place entirely — usually to the network, when the real problem
    // is that they pasted an agent token where a Plane one belongs.
    #[test]
    fn a_gateway_refusal_is_reported_in_the_gateways_own_words() {
        let body = r#"{"error":"UNAUTHENTICATED","message":"Plane rejected that personal token","recovery":"Create a new one under your profile"}"#;
        let said = gateway_said(body);
        assert!(
            said.contains("Plane rejected that personal token"),
            "{said}"
        );
        assert!(said.contains("Create a new one"), "{said}");
        // Not JSON at all — usually an HTML error page from something that is not the gateway.
        assert!(gateway_said("<html><body>404</body></html>").contains("html"));
        assert_eq!(gateway_said("   "), "the gateway returned nothing");
    }

    /// The same hash the scripts compute, so a fixture manifest says what a real install would have.
    /// Shelling out to `sha256sum` on purpose: a Rust implementation could agree with itself while
    /// disagreeing with the shell, which is the only thing that matters here.
    fn sha256_of(bytes: &[u8]) -> String {
        use std::io::Write;
        let mut child = Command::new("sha256sum")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    }

    /// Run sync-refresh.sh against a store + project laid out like a wired box, and return its
    /// report (stdout is machine-readable, stderr is prose).
    fn refresh_run(
        store: &Path,
        project: &Path,
        boxhome: &Path,
        args: &[&str],
    ) -> (String, String) {
        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-refresh.sh"))
            .args(args)
            .env("HOME", boxhome)
            .env("WORKSPACE_DIR", project)
            .output()
            .unwrap();
        assert!(out.status.success(), "refresh must never fail a box");
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The whole promise of the re-apply action in one test: it delivers a correction to what skein
    /// installed, and it does not touch what the box wrote.
    ///
    /// Worth doing end to end rather than unit-testing the classifier, because the failure that
    /// matters — silently overwriting a box's own rules — lives in the file handling, not the
    /// comparison. A box that finds its edits reverted has no reason to trust anything else here.
    #[test]
    fn a_refresh_replaces_what_skein_installed_and_keeps_what_the_box_wrote() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        let boxhome = home.join("boxhome");
        let state = boxhome.join(".local/state/skein");
        fs::create_dir_all(&state).unwrap();

        let slug = project.display().to_string().replace('/', "-");
        fs::write(state.join(format!("sync-{slug}.done")), "").unwrap();
        let src = store.join("skein").join("sync");

        // The skill: installed by skein, then upstream moved. The manifest carries what was written.
        fs::create_dir_all(store.join("skills/work-tracking")).unwrap();
        fs::write(store.join("skills/work-tracking/SKILL.md"), "OLD SKILL\n").unwrap();
        // The memory: the box rewrote it. No manifest entry can match, and it must survive.
        fs::create_dir_all(store.join("memory")).unwrap();
        fs::write(
            store.join("memory/work-tracking.md"),
            "the box's own words\n",
        )
        .unwrap();

        // The block: installed verbatim from the store, so it is skein's to correct. `block_of` in
        // the script reads the section without its trailing blank line, which is what is recorded.
        let block_now = fs::read_to_string(src.join("work-tracking.block.md")).unwrap();
        fs::write(
            project.join("CLAUDE.md"),
            format!("# proj\n\n---\n\n{block_now}\n## Later section\n\nkept\n"),
        )
        .unwrap();
        fs::write(
            state.join(format!("sync-{slug}.manifest")),
            format!(
                "skill\t{}\nmemory\t{}\nblock\t{}\n",
                sha256_of(b"OLD SKILL\n"),
                // A hash nothing can match: the box's memory is not what skein wrote.
                "0".repeat(64),
                sha256_of(block_now.trim_end().as_bytes()),
            ),
        )
        .unwrap();

        // Now move the reference on, exactly as a `git submodule update` + copy would.
        fs::write(src.join("work-tracking.skill.md"), "NEW SKILL\n").unwrap();
        fs::write(
            src.join("work-tracking.block.md"),
            "## Work tracking\n\nuse `decompose`, not capture per child\n",
        )
        .unwrap();

        let (report, _) = refresh_run(&store, &project, &boxhome, &[]);

        assert!(
            report.contains("skill\tstale"),
            "the skill skein installed, now superseded, must be offered: {report}"
        );
        assert!(
            report.contains("memory\tyours"),
            "a memory the box rewrote must be recognised as the box's: {report}"
        );
        assert_eq!(
            fs::read_to_string(store.join("skills/work-tracking/SKILL.md")).unwrap(),
            "NEW SKILL\n",
            "the correction was not delivered"
        );
        assert_eq!(
            fs::read_to_string(store.join("memory/work-tracking.md")).unwrap(),
            "the box's own words\n",
            "skein overwrote an edit it could see — the one thing a refresh must never do"
        );
        let claude = fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert!(
            claude.contains("use `decompose`, not capture per child"),
            "the block was not corrected: {claude}"
        );
        assert!(
            claude.contains("# proj")
                && claude.contains("## Later section")
                && claude.contains("kept"),
            "rewriting the section ate the rest of the file: {claude}"
        );
        env::remove_var("SKEIN_HOME");
    }

    /// A box wired up before the manifest existed. Neither state is knowable, so the refusal has to
    /// be explicit rather than silently sorted into "stale" (overwrites edits) or "yours" (delivers
    /// nothing, forever).
    #[test]
    fn without_a_record_of_what_was_installed_a_refresh_asks_rather_than_guesses() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        let boxhome = home.join("boxhome");
        let state = boxhome.join(".local/state/skein");
        fs::create_dir_all(&state).unwrap();
        let slug = project.display().to_string().replace('/', "-");
        fs::write(state.join(format!("sync-{slug}.done")), "").unwrap();
        fs::create_dir_all(store.join("skills/work-tracking")).unwrap();
        fs::write(
            store.join("skills/work-tracking/SKILL.md"),
            "PRE-MANIFEST\n",
        )
        .unwrap();

        let (report, _) = refresh_run(&store, &project, &boxhome, &[]);
        assert!(report.contains("skill\tunknown"), "{report}");
        assert_eq!(
            fs::read_to_string(store.join("skills/work-tracking/SKILL.md")).unwrap(),
            "PRE-MANIFEST\n",
            "an unknown document was rewritten without being asked"
        );
        let said = describe_refresh(&report, false);
        assert!(
            said.contains("Replace"),
            "the report has to name the way out, or an unknown document is a dead end: {said}"
        );

        // The human pressing Replace is the evidence that was missing.
        let (forced, _) = refresh_run(&store, &project, &boxhome, &["--force"]);
        assert!(forced.contains("skill\tunknown"), "{forced}");
        assert_eq!(
            fs::read_to_string(store.join("skills/work-tracking/SKILL.md")).unwrap(),
            fs::read_to_string(store.join("skein/sync/work-tracking.skill.md")).unwrap(),
            "Replace did not take it"
        );
        env::remove_var("SKEIN_HOME");
    }

    /// The button only appears when there is something to deliver, so the signal behind it has to be
    /// quiet by default — an indicator that is always lit is one nobody reads.
    #[test]
    fn the_cockpit_only_offers_an_update_when_the_store_has_a_newer_one() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        assert!(
            !sync_docs_available(&store),
            "nothing installed yet is Track work's job, not an update"
        );
        fs::create_dir_all(store.join("skills/work-tracking")).unwrap();
        fs::copy(
            store.join("skein/sync/work-tracking.skill.md"),
            store.join("skills/work-tracking/SKILL.md"),
        )
        .unwrap();
        assert!(
            !sync_docs_available(&store),
            "an up-to-date box must stay quiet"
        );
        fs::write(store.join("skills/work-tracking/SKILL.md"), "older\n").unwrap();
        assert!(sync_docs_available(&store));
        env::remove_var("SKEIN_HOME");
    }

    #[test]
    fn wiring_a_box_up_refuses_before_it_spends_anything() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        // Nothing configured: the error has to name which half is missing, because "not configured"
        // sends someone to re-check the field they already filled in.
        let e = sync_provision_box("web-main").unwrap_err();
        assert!(e.contains("connection"), "{e}");
        upsert_connection(
            Some("shared"),
            "shared",
            "https://plane.example.com",
            None,
        )
        .unwrap();
        let e = sync_provision_box("web-main").unwrap_err();
        assert!(e.contains("Plane token"), "{e}");
        // Configured, but the box is not running — refuse before minting a credential for a box
        // that cannot receive it.
        set_connection_token("shared", "plane_api_x").unwrap();
        let e = sync_provision_box("web-main").unwrap_err();
        assert!(e.contains("not running"), "{e}");
        assert!(
            sync_mint_token(
                "web-main",
                None,
                &SyncConnection {
                    id: "shared".into(),
                    label: "shared".into(),
                    gateway_url: "http://127.0.0.1:9".into(),
                }
            )
            .is_err(),
            "minting must not be attempted against an unreachable gateway in a test"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // A destroyed box takes its disk with it, not its credential — the token is a bearer token and
    // nothing about it is bound to the box. Teardown therefore revokes it, and must not depend on
    // that succeeding: `sbx rm` has already run by then.
    #[test]
    fn retiring_a_box_retires_its_token_but_never_blocks_on_it() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        env::set_var("SKEIN_LS_CMD", "false");
        // Not configured at all ⇒ a silent no-op, so a destroy stays quiet for anyone not tracking.
        assert!(sync_revoke_token("web-main").is_ok(), "nothing to revoke");
        upsert_connection(
            Some("shared"),
            "shared",
            "https://plane.example.com",
            None,
        )
        .unwrap();
        assert!(
            sync_revoke_token("web-main").is_ok(),
            "a gateway with no stored PAT still has nothing to revoke"
        );
        // Configured, but pointed at nothing that answers: an error the caller LOGS rather than
        // one that aborts the teardown. The distinction is the whole point of the test.
        upsert_connection(
            Some("shared"),
            "shared",
            "http://127.0.0.1:9",
            Some("plane_api_x"),
        )
        .unwrap();
        assert!(
            sync_revoke_token("web-main").is_err(),
            "an unreachable gateway must be reported, not silently treated as revoked"
        );
        env::remove_var("SKEIN_HOME");
        env::remove_var("SKEIN_LS_CMD");
    }

    // The installer is shell that runs inside a box, so reading it proves nothing. Run it against a
    // real store, a real project and a fake `claude`, and check what it actually did.
    #[test]
    fn the_store_installer_registers_the_box_then_writes_the_rules() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();

        // An ESTABLISHED box: a CLAUDE.md the team has evolved, memories they curated with their
        // own index, a skills dir, and a Codex config with hand-written entries. Wiring up work
        // tracking must add to all of it and replace none of it.
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n\nsome direction\n").unwrap();
        fs::write(
            store.join("memory").join("MEMORY.md"),
            "- [Our own note](ours.md) — hard-won\n",
        )
        .unwrap();
        fs::write(store.join("memory").join("ours.md"), "the note itself\n").unwrap();
        fs::create_dir_all(store.join("skills").join("ours")).unwrap();
        fs::write(store.join("skills").join("ours").join("SKILL.md"), "ours\n").unwrap();

        // A `claude` that records how it was called. The registration is an argv claim — the URL,
        // the bearer, the scope — and argv is the only place that claim is observable.
        let bin = home.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        let log = home.join("claude.log");
        fs::write(
            bin.join("claude"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();

        let boxhome = home.join("boxhome");
        fs::create_dir_all(boxhome.join(".codex")).unwrap();
        fs::write(
            boxhome.join(".codex").join("config.toml"),
            "[mcp_servers.something_else]\nurl = \"https://theirs.test\"\n",
        )
        .unwrap();
        let run = || {
            Command::new("bash")
                .arg(store.join("skein").join("bin").join("sync-install.sh"))
                .env("HOME", &boxhome)
                .env("WORKSPACE_DIR", &project)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("SYNC_GATEWAY_URL", "https://gw.test/")
                .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
                .output()
                .unwrap()
        };
        assert!(run().status.success());

        let calls = fs::read_to_string(&log).unwrap();
        // The plugin IS the registration for Claude now. Upstream ships the same `sync` server
        // inside it, so skein registering its own would shadow the plugin's — a hand-added entry
        // wins — and the box would end up with the tools and none of the monitor or hooks.
        assert!(
            calls.contains("plugin marketplace add prateekreddy/sync"),
            "{calls}"
        );
        assert!(calls.contains("plugin install sync@sync"), "{calls}");
        assert!(
            !calls.contains("mcp add"),
            "skein must not register `sync` for Claude any more — the plugin declares it: {calls}"
        );
        // And the old one is taken away, because every box wired before today still carries it.
        assert!(calls.contains("mcp remove sync"), "{calls}");

        // The gateway still comes from the same place; only where it lands has changed. The plugin
        // declares its url as `${SYNC_MCP_URL:-…}`, which is upstream's supported seam and the only
        // one that survives a plugin update.
        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(boxhome.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["env"]["SYNC_MCP_URL"], "https://gw.test/mcp");

        // Codex keeps the token route: plugins are a Claude Code feature, so for Codex this is not
        // a fallback, it is the only way it ever gets these tools.
        let codex = fs::read_to_string(boxhome.join(".codex/config.toml")).unwrap();
        assert!(codex.contains("[mcp_servers.sync]"), "{codex}");
        assert!(codex.contains("Bearer sync_agent_abc"), "{codex}");
        assert!(
            codex.contains("https://theirs.test"),
            "it appends, never rewrites: {codex}"
        );

        // The rules land only after registration, and they land in the STORE — this repo's
        // `.claude` — so every box of the repo sees them, not just the one that was wired up.
        let claude_md = fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("## Work tracking"), "{claude_md}");
        assert!(
            claude_md.contains("some direction"),
            "it appends, never replaces"
        );
        assert!(store.join("memory/work-tracking.md").is_file());
        // Still shipped here because this box has Codex, which cannot install a Claude Code plugin
        // and would otherwise be left with the tools and no playbook for them. The sibling test
        // covers the other half: with the plugin and no Codex, this copy is skipped.
        assert!(store.join("skills/work-tracking/SKILL.md").is_file());
        assert!(store.join("skills/work-tracking/organising.md").is_file());
        assert!(store
            .join("skills/work-tracking/troubleshooting.md")
            .is_file());
        let index = fs::read_to_string(store.join("memory/MEMORY.md")).unwrap();
        assert!(index.contains("(work-tracking.md)"));

        // Nothing the box already had is touched. This is the whole contract for an existing box:
        // every write is an append or a create, never a replace.
        assert!(
            index.contains("[Our own note](ours.md)"),
            "the index was rewritten: {index}"
        );
        assert_eq!(
            fs::read_to_string(store.join("memory/ours.md")).unwrap(),
            "the note itself\n"
        );
        assert_eq!(
            fs::read_to_string(store.join("skills/ours/SKILL.md")).unwrap(),
            "ours\n"
        );
        let codex = fs::read_to_string(boxhome.join(".codex/config.toml")).unwrap();
        assert!(
            codex.contains("[mcp_servers.something_else]") && codex.contains("https://theirs.test"),
            "a hand-written Codex entry was lost: {codex}"
        );
        assert_eq!(
            codex.matches("[mcp_servers.sync]").count(),
            1,
            "the Codex block was written more than once: {codex}"
        );

        // Once, then hands off: the box may delete what it does not want, and a later start must
        // not restore it. Re-running is also how a box start behaves, so this is the common path.
        fs::remove_file(store.join("skills/work-tracking/SKILL.md")).unwrap();
        assert!(run().status.success());
        assert_eq!(
            fs::read_to_string(project.join("CLAUDE.md"))
                .unwrap()
                .matches("## Work tracking")
                .count(),
            1,
            "a second run appended the section again"
        );
        assert!(
            !store.join("skills/work-tracking/SKILL.md").exists(),
            "a deleted skill came back — the box cannot make its own edits stick"
        );
        env::remove_var("SKEIN_HOME");
    }

    // The ordering claim, which until this test was only a comment: rules are written only AFTER a
    // runtime actually registered. An instruction to "call capture" in a box whose registration
    // failed is a rule the agent cannot follow and will learn to read past — and it would sit in
    // CLAUDE.md looking exactly like a working one.
    #[test]
    fn a_failed_registration_installs_no_rules_for_tools_that_are_not_there() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n").unwrap();

        // A `claude` that refuses — a bad URL, an unreachable gateway, a rejected token all land
        // here. No codex either, so nothing registers.
        let bin = home.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("claude"), "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
        let boxhome = home.join("boxhome");
        fs::create_dir_all(&boxhome).unwrap();

        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SYNC_GATEWAY_URL", "https://gw.test")
            .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
            .output()
            .unwrap();
        assert!(out.status.success(), "still must not gate startup");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(
            said.contains("no runtime registered"),
            "it has to say so: {said}"
        );
        assert!(
            !fs::read_to_string(project.join("CLAUDE.md"))
                .unwrap()
                .contains("Work tracking"),
            "rules were written for tools the box does not have"
        );
        assert!(!store.join("memory/work-tracking.md").exists());
        assert!(!store.join("skills/work-tracking/SKILL.md").exists());
        // And nothing was stamped, so fixing the cause and starting again still works.
        assert!(!boxhome.join(".local/state/skein").exists());
        env::remove_var("SKEIN_HOME");
    }

    /// Wiring a repo to a tracker points every box of it at that gateway, in one write.
    ///
    /// The store is project scope for every box of the repo, and `env` is honoured there — so this
    /// reaches boxes that do not exist yet, which is what makes it one action per repo instead of
    /// one per box. Without it the plugin falls back to the default gateway compiled into it, and a
    /// box would quietly claim work on somebody else's tracker.
    #[test]
    fn a_repos_store_points_its_boxes_at_the_gateway_its_connection_names() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        fs::create_dir_all(&store).unwrap();

        crate::tracking::save_connections(&[crate::tracking::SyncConnection {
            id: "c1".into(),
            label: "ours".into(),
            gateway_url: "https://gw.test".into(),
        }])
        .unwrap();
        let repo = Repo {
            id: "r1".into(),
            source: "s".into(),
            work: "/w".into(),
            store: store.display().to_string(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: "c1".into(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        save_repos(std::slice::from_ref(&repo)).unwrap();

        ensure_store(&store).unwrap();
        let gateway = store.join("skein/sync/gateway");
        // `/mcp` under the base, the same normalisation every other caller uses — a pasted endpoint
        // must not become `/mcp/mcp`.
        assert_eq!(
            fs::read_to_string(&gateway).unwrap().trim(),
            "https://gw.test/mcp"
        );

        // Unwired again: the file goes, rather than outliving the decision to disconnect. A stale
        // URL here is worse than none — it keeps pointing boxes at a tracker the repo has left.
        save_repos(&[Repo {
            sync_connection: String::new(),
            ..repo
        }])
        .unwrap();
        ensure_store(&store).unwrap();
        assert!(
            !gateway.exists(),
            "an unwired repo must stop pointing its boxes anywhere"
        );

        env::remove_var("SKEIN_HOME");
    }

    /// A box with no minted token is still wired up: the URL alone is enough now.
    ///
    /// The token stopped being what Claude authenticates with the moment the plugin took over the
    /// server — it signs in over OAuth. Gating the whole install on a token would mean a repo could
    /// not be pointed at a tracker without one being minted per box, which is the per-box work this
    /// change exists to remove.
    #[test]
    fn the_url_alone_wires_a_box_up_and_the_token_is_only_codexs() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n").unwrap();

        let bin = home.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        let log = home.join("claude.log");
        fs::write(
            bin.join("claude"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();

        // The URL arrives the way the host publishes it for the whole repo — a file in the store,
        // no token and no environment override anywhere. This is the chain end to end: the host
        // writes one file, and a box start turns it into that box's own user-scope setting, which
        // is the scope the plugin actually reads.
        fs::write(store.join("skein/sync/gateway"), "https://gw.test/mcp\n").unwrap();
        let boxhome = home.join("boxhome");
        fs::create_dir_all(&boxhome).unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env_remove("SYNC_MCP_URL")
            .env_remove("SYNC_GATEWAY_URL")
            .env_remove("SYNC_AGENT_TOKEN")
            .output()
            .unwrap();
        assert!(out.status.success());

        let calls = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.contains("plugin install sync@sync"),
            "no token is not a reason to skip the plugin: {} / {calls}",
            String::from_utf8_lossy(&out.stderr)
        );
        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(boxhome.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["env"]["SYNC_MCP_URL"], "https://gw.test/mcp");
        // And Codex gets nothing rather than a `Bearer ` that 401s on first use — a registration
        // that looks complete is a worse place to find out than here.
        assert!(!boxhome.join(".codex/config.toml").exists());

        env::remove_var("SKEIN_HOME");
    }

    /// A box wired up before the plugin existed still gets it.
    ///
    /// This is the whole reason the plugin sits ahead of the stamp gate. The stamp means "this box
    /// owns its CLAUDE.md, memory and skill now", and re-asserting over it is what that gate exists
    /// to prevent — but the plugin is not a re-assertion, it is something upstream started shipping
    /// after these boxes were wired. Behind the gate it would have reached only boxes created from
    /// here on, which for an established fleet is none of them.
    #[test]
    fn an_already_wired_box_still_picks_up_the_plugin() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        // The box's own CLAUDE.md, as it has evolved since — untouched by anything below.
        fs::write(project.join("CLAUDE.md"), "# proj\n\nours\n").unwrap();

        let bin = home.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        let log = home.join("claude.log");
        fs::write(
            bin.join("claude"),
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();

        // Wired up on some earlier day: the stamp is there, and so is the skill it installed.
        let boxhome = home.join("boxhome");
        let state = boxhome.join(".local/state/skein");
        fs::create_dir_all(&state).unwrap();
        let slug = project.display().to_string().replace('/', "-");
        fs::write(state.join(format!("sync-{slug}.done")), "").unwrap();

        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SYNC_GATEWAY_URL", "https://gw.test/")
            .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
            .output()
            .unwrap();
        assert!(out.status.success());

        let calls = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.contains("plugin install sync@sync"),
            "a stamped box must still get the plugin: {calls}"
        );
        assert!(
            state.join("sync-plugin.done").is_file(),
            "and record it, so the next start is a file test rather than a subprocess"
        );
        // Everything the stamp protects is still untouched: the gate did its job for the things it
        // was guarding, and only the plugin came through ahead of it.
        assert_eq!(
            fs::read_to_string(project.join("CLAUDE.md")).unwrap(),
            "# proj\n\nours\n"
        );

        // Second start: the marker is believed, and `claude` is not asked again.
        fs::write(&log, "").unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SYNC_GATEWAY_URL", "https://gw.test/")
            .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            !fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("plugin install"),
            "installed once, then the box owns it — removing it must stay removed"
        );

        env::remove_var("SKEIN_HOME");
    }

    /// With the plugin installed and no Codex on the box, skein must NOT also write its own copy of
    /// the skill.
    ///
    /// Two copies of one skill is a fork, not a redundancy. The vendored copy is pinned to whatever
    /// upstream commit skein last pulled, so the first time an argument name changes the box holds
    /// two contradictory descriptions of the same tool with nothing to say which is older — and the
    /// skill's own advice is to trust the tool list over anything written in it, which is advice a
    /// stale copy gives just as confidently.
    #[test]
    fn the_plugins_skill_is_not_shadowed_by_a_vendored_copy() {
        use std::os::unix::fs::PermissionsExt;
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n").unwrap();

        let bin = home.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("claude"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();

        // No `.codex/config.toml` and no `codex` on PATH — a Claude-only box, where the plugin is
        // the whole story.
        let boxhome = home.join("boxhome");
        fs::create_dir_all(&boxhome).unwrap();
        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("SYNC_GATEWAY_URL", "https://gw.test/")
            .env("SYNC_AGENT_TOKEN", "sync_agent_abc")
            .output()
            .unwrap();
        assert!(out.status.success());

        assert!(
            !store.join("skills/work-tracking/SKILL.md").exists(),
            "the plugin ships this skill and keeps it current; skein's pinned copy must not sit \
             beside it: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The two skein still owns, because the plugin cannot write either: the block is per-repo
        // and always in context, and the memory is in skein's own format.
        let claude_md = fs::read_to_string(project.join("CLAUDE.md")).unwrap();
        assert!(claude_md.contains("## Work tracking"), "{claude_md}");
        assert!(store.join("memory/work-tracking.md").is_file());

        env::remove_var("SKEIN_HOME");
    }

    // A box with no credentials is not a broken box: startup runs this on every box, so it has to
    // be silent and change nothing until there is something to register.
    #[test]
    fn the_store_installer_does_nothing_at_all_without_credentials() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let store = home.join("store").join(".claude");
        ensure_store(&store).unwrap();
        let project = home.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("CLAUDE.md"), "# proj\n").unwrap();
        let boxhome = home.join("boxhome");
        fs::create_dir_all(&boxhome).unwrap();

        let out = Command::new("bash")
            .arg(store.join("skein").join("bin").join("sync-install.sh"))
            .env("HOME", &boxhome)
            .env("WORKSPACE_DIR", &project)
            .env_remove("SYNC_GATEWAY_URL")
            .env_remove("SYNC_AGENT_TOKEN")
            // Cleared explicitly, because this one is inherited from the *developer's* environment
            // rather than set by the fixture: Claude Code injects `env` from settings into every
            // subprocess it spawns, so a machine that has this variable set at all would otherwise
            // make the installer wire a box up here and the test would fail describing a bug that
            // does not exist.
            .env_remove("SYNC_MCP_URL")
            .output()
            .unwrap();
        assert!(out.status.success(), "it must never gate a box's startup");
        assert!(
            out.stderr.is_empty(),
            "a box without a tracker should start silently: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!fs::read_to_string(project.join("CLAUDE.md"))
            .unwrap()
            .contains("Work tracking"));
        assert!(!store.join("memory/work-tracking.md").exists());
        env::remove_var("SKEIN_HOME");
    }

    // The wiring, not the helper: a correct `sync_revoke_token` that teardown never calls leaves
    // exactly the live credential this exists to retire. Proven against a real socket, so the whole
    // path — destroy → curl → method, URL and bearer — is what is asserted.
    #[test]
    fn destroying_a_box_actually_sends_the_revocation() {
        use std::io::{Read, Write};
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Deadlined, not blocking: a regression here is "teardown stopped calling revoke", and a
        // blocking accept() turns that into a hung suite instead of a red test — which is how a
        // guard stops being read at all.
        listener.set_nonblocking(true).unwrap();
        let seen = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut buf = [0u8; 2048];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 22\r\n\r\n{\"revoked\":\"pro/gone\"}",
                        );
                        return String::from_utf8_lossy(&buf[..n]).into_owned();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return String::new(); // nothing ever asked to revoke
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => return format!("accept failed: {e}"),
                }
            }
        });

        upsert_connection(
            Some("shared"),
            "shared",
            &format!("http://127.0.0.1:{port}"),
            Some("plane_api_secret"),
        )
        .unwrap();
        env::set_var("SKEIN_DESTROY_CMD", "true"); // stand in for `sbx rm`
        env::set_var("SKEIN_REGISTRY", dir.join("sandboxes.json"));
        fs::write(dir.join("sandboxes.json"), "{}").unwrap();

        destroy_box("gone").unwrap();
        let request = seen.join().unwrap();
        assert!(
            !request.is_empty(),
            "teardown never asked the gateway to revoke anything — the box is gone, its token is not"
        );
        assert!(
            request.starts_with("DELETE /v1/agent-tokens/gone "),
            "{request}"
        );
        assert!(
            request.contains("Authorization: Bearer plane_api_secret"),
            "the PAT is what authorises a revocation — the box's own token cannot: {request}"
        );

        env::remove_var("SKEIN_DESTROY_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
    }

    // The transfer succeeding is not the claim; the gateway saying it revoked something is. Every
    // case below arrives as a perfectly successful curl, and reading any of them as "done" would
    // leave a live bearer token behind a box that no longer exists.
    #[test]
    fn only_the_gateway_saying_revoked_counts_as_revoked() {
        assert!(revocation_outcome(r#"{"revoked":"pro/web-main"}"#).is_ok());
        let e =
            revocation_outcome(r#"{"error":"NOT_FOUND","message":"no such agent"}"#).unwrap_err();
        assert!(e.contains("no such agent"), "{e}");
        // A proxy or the wrong host answering 200 with a page.
        assert!(revocation_outcome("<html>not the gateway</html>").is_err());
        // The shape that would slip through a bare "is it JSON?" check.
        assert!(revocation_outcome(r#"{"ok":true}"#).is_err());
        assert!(revocation_outcome("").is_err());
    }
}
