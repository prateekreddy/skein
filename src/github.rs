//! The one way skein talks to GitHub: HTTP, with a token skein already holds.
//!
//! **Why this exists rather than `gh`.** The review queue used to be built out of the `gh` CLI, and
//! that made a third-party binary a hard requirement of a feature that is on by default — announced
//! nowhere, met as a bug. It also dragged in `gh`'s credential model: `gh` keeps its token in the
//! system keyring on a modern Linux, so every call was a keyring read, and a locked login keyring
//! answered each with an unlock dialog. A queue that polls every three minutes therefore asked for a
//! password every three minutes, on a fleet whose owner had already given skein a perfectly good
//! token for something else.
//!
//! So the queue speaks to the API directly, with the credential the user chose. Nothing to install,
//! nothing to authenticate, and one credential doing every job it is capable of.
//!
//! **Why curl rather than an HTTP crate.** [`crate::gitgate`] already talks to GitHub this way to
//! mint App tokens, and the reason is the token: curl reads its options from **stdin**, so the
//! `Authorization` header never appears in `ps` or in any shell history. An HTTP crate would be
//! tidier and would add a TLS stack, a dependency tree and a second way of doing what already works.
//! curl is present on every macOS and every ordinary Linux, and skein already required it.
//!
//! **What it deliberately does not do:** retry, rate-limit, or paginate on its own. A queue that
//! retried behind your back would turn one slow answer into four, and the callers here want a
//! partial answer they can report ("this query failed, so those PRs are missing") far more than they
//! want a complete one that took a minute.

use crate::util::output_with_timeout_why;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// Per-process counter for request-body temp names, so two threads never pick the same one.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// Where the API lives. `$SKEIN_GITHUB_API` points it at a stub — the seam the browser tests drive,
/// replacing the fake `gh` binary they used to put on `$PATH`.
///
/// A test seam and nothing else: GitHub Enterprise would need more than a base URL (a different
/// GraphQL path, different token rules), and pretending otherwise here would be a feature nobody
/// had tested.
pub(crate) fn api_base() -> String {
    std::env::var("SKEIN_GITHUB_API")
        .ok()
        .map(|v| v.trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://api.github.com".into())
}

/// The curl options that carry the credential, fed over stdin so they never reach `ps`.
fn config(token: &str, accept: &str) -> String {
    format!(
        "header = \"Authorization: Bearer {}\"\n\
         header = \"Accept: {accept}\"\n\
         header = \"X-GitHub-Api-Version: 2022-11-28\"\n\
         header = \"User-Agent: skein\"\n",
        // A PAT is alphanumeric and a JWT is base64url segments, so neither can hold a quote or a
        // newline — but this is the line that would become an injected curl option if that ever
        // stopped being true, so it is enforced rather than assumed.
        token.replace(['"', '\n', '\\'], "")
    )
}

/// One call. Returns the body as text, whatever it is — JSON, a diff, or an error page.
///
/// The status code comes back separately (curl writes it after the body) because the callers need
/// it: a 404 on a pull request and a 401 on the whole API are different sentences, and a body alone
/// cannot tell them apart.
fn call(
    method: &str,
    url: &str,
    token: &str,
    body: Option<&str>,
    accept: &str,
    timeout: Duration,
) -> Result<(u16, String), String> {
    use std::io::Write;
    // The body goes to a file and the token stays on stdin, because both cannot have stdin: curl
    // reads `--config -` and `--data-binary @-` from the same place, and whichever gets there first
    // consumes the other's input. Found the direct way — the stub API received a curl config as its
    // request body.
    //
    // A file is safe for this half and not for the other: the body is a query, while the token is
    // the thing that must never touch the filesystem or `ps`. 0600 and removed either way.
    let body_file = body.map(|body| {
        let path = std::env::temp_dir().join(format!(
            "skein-req-{}-{}",
            std::process::id(),
            REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let write = || -> std::io::Result<()> {
            let mut file = std::fs::File::create(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(body.as_bytes())
        };
        write().map(|()| path.clone()).map_err(|e| {
            let _ = std::fs::remove_file(&path);
            format!("writing the request body: {e}")
        })
    });
    let body_file = match body_file {
        Some(Ok(path)) => Some(path),
        Some(Err(e)) => return Err(e),
        None => None,
    };
    let mut args: Vec<String> = vec![
        "-sS".into(),
        "-X".into(),
        method.to_string(),
        // The status on its own line after the body, so one call answers both questions.
        "-w".into(),
        "\n%{http_code}".into(),
        url.to_string(),
    ];
    if let Some(path) = &body_file {
        args.push("-H".into());
        args.push("Content-Type: application/json".into());
        args.push("--data-binary".into());
        args.push(format!("@{}", path.display()));
    }
    args.push("--config".into());
    args.push("-".into());

    let child = Command::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "curl is not installed, and skein reads GitHub with it".to_string()
            } else {
                format!("curl: {e}")
            }
        });
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            if let Some(path) = &body_file {
                let _ = std::fs::remove_file(path);
            }
            return Err(e);
        }
    };
    {
        let mut pipe = child.stdin.take().ok_or("curl took no stdin")?;
        pipe.write_all(config(token, accept).as_bytes())
            .map_err(|e| format!("curl: {e}"))?;
    }
    // Bounded, because this is on the path a cockpit poll takes: an unreachable API must fail the
    // badge, not hold a blocking thread until something else notices.
    let out = {
        let mut waiting = child;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match waiting.try_wait().map_err(|e| e.to_string())? {
                Some(_) => break waiting.wait_with_output().map_err(|e| e.to_string())?,
                None if std::time::Instant::now() >= deadline => {
                    let _ = waiting.kill();
                    let _ = waiting.wait();
                    if let Some(path) = &body_file {
                        let _ = std::fs::remove_file(path);
                    }
                    return Err(format!(
                        "GitHub did not answer within {}s",
                        timeout.as_secs()
                    ));
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    };
    if let Some(path) = &body_file {
        let _ = std::fs::remove_file(path);
    }
    if !out.status.success() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // The status is the last line; everything before it is the body. Split from the END, since a
    // diff contains newlines and a JSON body may too.
    let (body, status) = match text.rsplit_once('\n') {
        Some((body, status)) => (body.to_string(), status.trim().parse().unwrap_or(0)),
        None => (String::new(), text.trim().parse().unwrap_or(0)),
    };
    Ok((status, body))
}

/// `GET`, as JSON. A non-2xx answers with GitHub's own `message` when it has one, because that is
/// the sentence worth showing ("Bad credentials", "Not Found") rather than a bare status.
pub(crate) fn get_json(path: &str, token: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", api_base());
    json(call(
        "GET",
        &url,
        token,
        None,
        "application/vnd.github+json",
        Duration::from_secs(30),
    )?)
}

/// `GET`, as text — for the media types that are not JSON at all, i.e. a diff.
pub(crate) fn get_text(path: &str, token: &str, accept: &str) -> Result<String, String> {
    let url = format!("{}{path}", api_base());
    let (status, body) = call("GET", &url, token, None, accept, Duration::from_secs(60))?;
    match status {
        200..=299 => Ok(body),
        _ => Err(complaint(status, &body)),
    }
}

/// `POST`/`PUT` with a JSON body.
pub(crate) fn send_json(
    method: &str,
    path: &str,
    token: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", api_base());
    json(call(
        method,
        &url,
        token,
        Some(&body.to_string()),
        "application/vnd.github+json",
        Duration::from_secs(30),
    )?)
}

/// One GraphQL query.
///
/// GraphQL rather than REST for the review queue, and it is not a preference: a pull request's
/// reviews, the commit each was left against, and its check rollup are three more REST calls *per
/// pull request*. One query returns all of it for a hundred PRs. It is also what `gh` did — its
/// `--json` field names are GraphQL's, which is why the shape this returns needs almost no
/// translation to be what the queue already parses.
///
/// A GraphQL error is a 200 with an `errors` array, so success has to be read from the body rather
/// than from the status. Reported whole: partial data with the reason discarded is how a queue
/// silently under-reports, which is the one thing it must not do.
pub(crate) fn graphql(
    query: &str,
    variables: serde_json::Value,
    token: &str,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "query": query, "variables": variables });
    let url = format!("{}/graphql", api_base());
    let (status, text) = call(
        "POST",
        &url,
        token,
        Some(&body.to_string()),
        "application/vnd.github+json",
        Duration::from_secs(30),
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| complaint(status, &text))?;
    if let Some(errors) = value.get("errors").and_then(|e| e.as_array()) {
        if !errors.is_empty() {
            let said = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(match said.is_empty() {
                true => complaint(status, &text),
                false => said,
            });
        }
    }
    value
        .get("data")
        .cloned()
        .ok_or_else(|| complaint(status, &text))
}

/// Turn a body into JSON, or into the best sentence available about why not.
fn json((status, body): (u16, String)) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| complaint(status, &body))?;
    match status {
        200..=299 => Ok(value),
        _ => Err(value
            .get("message")
            .and_then(|m| m.as_str())
            .map(|m| format!("GitHub said {status}: {m}"))
            .unwrap_or_else(|| complaint(status, &body))),
    }
}

/// What to say when there is no `message` to quote. Clipped, because an HTML error page is not a
/// diagnosis and a whole one in a toast is worse than none.
fn complaint(status: u16, body: &str) -> String {
    let body = body.trim();
    match (status, body.is_empty()) {
        (0, true) => "GitHub sent nothing at all".to_string(),
        (0, false) => format!(
            "GitHub sent something unreadable: {}",
            crate::util::clip(body, 200)
        ),
        (_, true) => format!("GitHub answered {status} with an empty body"),
        (_, false) => format!("GitHub answered {status}: {}", crate::util::clip(body, 200)),
    }
}

/// `curl` present? Named here so the health report can ask without knowing how this module works.
pub fn have_curl() -> bool {
    let mut probe = Command::new("curl");
    probe.arg("--version");
    output_with_timeout_why(&mut probe, Duration::from_secs(5)).is_ok()
}
