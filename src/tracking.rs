//! Work tracking: named connections to a `sync` gateway, and wiring one box to one of them.
//!
//! A gateway URL and the personal token that mints tokens at it are ONE thing — a token minted
//! with PAT `A` is only valid where `A` authenticates — so [`SyncConnection`] holds the pair and a
//! repo selects a whole connection rather than describing half of one.
//!
//! Nothing here runs on a tick. Provisioning is an explicit act: it spends a network round trip
//! and mints a real credential.

use crate::config::*;
use crate::util::*;
use crate::{
    box_liveness, guest_write, load_repos, repo_for_box, save_repos, sbx_guest_output, Liveness,
    Repo,
};
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
    let mut cfg = load_config();
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
    cfg.sync_gateway_url.clear();
    if let Err(e) = save_config(&cfg) {
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
    let mut list = load_connections();
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
    match list.iter_mut().find(|c| c.id == id) {
        Some(existing) => *existing = conn.clone(),
        None => list.push(conn.clone()),
    }
    // The token first: a connection listed as ready before its credential landed would send someone
    // to press Track work against a gateway that will refuse them.
    if let Some(t) = token {
        set_connection_token(&id, t)?;
    }
    save_connections(&list)?;
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
    let mut list = load_connections();
    let before = list.len();
    list.retain(|c| c.id != id);
    if list.len() == before {
        return Err(format!("no work-tracking connection called {id:?}"));
    }
    save_connections(&list)?;
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
