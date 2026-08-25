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
//! **What it deliberately does not do:** retry or paginate on its own. A queue that retried behind
//! your back would turn one slow answer into four, and the callers here want a partial answer they
//! can report ("this query failed, so those PRs are missing") far more than they want a complete
//! one that took a minute.
//!
//! **What it does refuse: spending calls it knows will fail.** Once GitHub answers a rate limit,
//! every call short-circuits with the resume time until the quota resets — the reset learned from
//! the free `/rate_limit` endpoint, which never counts against any quota and is therefore the one
//! path a hold lets through. This is not a retry: nothing is ever re-sent. Before the hold, a dead
//! quota still met ~45 doomed requests per refresh cycle, each one pure cost against the secondary
//! limit's patience.

use crate::util::output_with_timeout_why;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Per-process counter for request-body temp names, so two threads never pick the same one.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// Epoch seconds until which GitHub must not be called, when a rate limit is in force. One value
/// for the whole process, because the limit is per-token and every caller here shares the token:
/// once the quota is spent, the ~45 requests a refresh cycle fires would all fail the same way.
static RATE_HOLD: Mutex<Option<u64>> = Mutex::new(None);

/// The hold, poison-tolerant for the same reason as [`crate::testutil::env_lock`]: the value is a
/// timestamp, and there is no invariant a panicking test could have corrupted.
fn rate_hold() -> std::sync::MutexGuard<'static, Option<u64>> {
    RATE_HOLD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Test-only: set or clear the hold directly, so a test can expire one without waiting for it.
#[cfg(test)]
fn set_rate_hold(until: Option<u64>) {
    *rate_hold() = until;
}

/// Test-only guard: clears the hold on entry AND on drop. A test that engages the hold and then
/// panics must not leave the rest of the binary refusing to call its fake GitHubs. Every test that
/// can touch the hold takes one, right after the env lock — `pub(crate)` because the batched
/// review search's tests in [`crate::prq`] engage the hold too.
#[cfg(test)]
pub(crate) struct HoldClear;
#[cfg(test)]
impl HoldClear {
    pub(crate) fn new() -> Self {
        set_rate_hold(None);
        HoldClear
    }
}
#[cfg(test)]
impl Drop for HoldClear {
    fn drop(&mut self) {
        set_rate_hold(None);
    }
}

/// Now, in epoch seconds — the clock `/rate_limit`'s `reset` values are on.
fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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
    // The hold, checked before anything is spent. `/rate_limit` is exempt: it is free, and it is
    // the endpoint the hold itself is learned from, so gating it would leave no way back out.
    let exempt = url.ends_with("/rate_limit");
    if !exempt {
        let mut hold = rate_hold();
        if let Some(until) = *hold {
            let now = epoch_now();
            if now < until {
                let minutes = (until - now).div_ceil(60);
                return Err(format!(
                    "GitHub is rate limiting skein — resuming in about {minutes}m; until then \
                     skein is not calling GitHub at all, and what you see is the last answer it \
                     holds."
                ));
            }
            *hold = None;
        }
    }
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
        // Ask for gzip and undo it. The per-file diff listing for a big pull request is megabytes
        // of JSON that compresses about ten to one, and the transfer runs inside this call's
        // deadline — sending it uncompressed spends the budget on bytes.
        "--compressed".into(),
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
    // The pipes are drained WHILE waiting, and this line is load-bearing: a pipe holds about
    // 64KB, so a body any larger blocks curl mid-write if nobody reads. The old loop here waited
    // for curl to exit before reading — curl waiting on skein, skein waiting on curl — and the
    // deadline then killed a transfer that was going fine. Every diff over the pipe buffer
    // reported "GitHub did not answer", when the one not answering was skein.
    use std::io::Read as _;
    let (mut out_pipe, mut err_pipe) = match (child.stdout.take(), child.stderr.take()) {
        (Some(out), Some(err)) => (out, err),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(path) = &body_file {
                let _ = std::fs::remove_file(path);
            }
            return Err("curl started without pipes".into());
        }
    };
    // How much has arrived, visible to the timeout: "no answer at all" and "an answer too big to
    // finish in time" are different sentences, and the second was reported as the first.
    let arrived = std::sync::Arc::new(AtomicU64::new(0));
    let counting = arrived.clone();
    let out_thread = std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            match out_pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    all.extend_from_slice(&buf[..n]);
                    counting.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        all
    });
    let err_thread = std::thread::spawn(move || {
        let mut all = Vec::new();
        let _ = err_pipe.read_to_end(&mut all);
        all
    });
    // Bounded, because this is on the path a cockpit poll takes: an unreachable API must fail the
    // badge, not hold a blocking thread until something else notices.
    let status = {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) => break status,
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(path) = &body_file {
                        let _ = std::fs::remove_file(path);
                    }
                    let got = arrived.load(std::sync::atomic::Ordering::Relaxed);
                    return Err(match got {
                        0 => format!("GitHub did not answer within {}s", timeout.as_secs()),
                        _ => format!(
                            "GitHub was still answering after {}s — {}KB had arrived when \
                             skein stopped waiting",
                            timeout.as_secs(),
                            got / 1024
                        ),
                    });
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    if let Some(path) = &body_file {
        let _ = std::fs::remove_file(path);
    }
    if !status.success() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&stdout).into_owned();
    // The status is the last line; everything before it is the body. Split from the END, since a
    // diff contains newlines and a JSON body may too.
    let (body, status) = match text.rsplit_once('\n') {
        Some((body, status)) => (body.to_string(), status.trim().parse().unwrap_or(0)),
        None => (String::new(), text.trim().parse().unwrap_or(0)),
    };
    // A rate-limited answer engages the hold — but never from the `/rate_limit` call itself, which
    // is how engaging learns the reset without recursing.
    if !exempt && rate_limited(status, &body).is_some() {
        engage_hold(token);
    }
    Ok((status, body))
}

/// `GET`, as JSON. A non-2xx answers with GitHub's own `message` when it has one, because that is
/// the sentence worth showing ("Bad credentials", "Not Found") rather than a bare status.
pub(crate) fn get_json(path: &str, token: &str) -> Result<serde_json::Value, String> {
    get_json_within(path, token, Duration::from_secs(30))
}

/// [`get_json`], with the caller's own budget. For the endpoints whose answers are measured in
/// megabytes — the per-file diff listing of a pull request GitHub refuses to serve whole — where
/// 30s is a poll's budget, not a transfer's.
pub(crate) fn get_json_within(
    path: &str,
    token: &str,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", api_base());
    json(call(
        "GET",
        &url,
        token,
        None,
        "application/vnd.github+json",
        timeout,
    )?)
}

/// What this repository is called **now**, following a rename.
///
/// A repository name is not a stable identifier, and skein stored one as if it were. GitHub's two
/// APIs then disagree about what to do with a stale one, and skein depends on both: REST answers
/// `301 Moved Permanently` with the canonical URL in the body, so every REST caller that follows it
/// keeps working — while **search silently matches nothing**. Measured on a live fleet:
///
/// ```text
/// repo:acme/gadget-demo is:pr is:open review-requested:me  ->  issueCount 0, no errors
/// repo:acme/thing        is:pr is:open review-requested:me  ->  issueCount 23
/// ```
///
/// Twenty-three pull requests waiting on somebody, an empty queue, HTTP 200 and nothing to say why.
///
/// The redirect is read rather than followed with `curl -L`, and deliberately: following it would
/// paper over the rename, and the caller's job is to record the new name rather than to spend a
/// redirect on every request for ever. `-L` on the shared `call` would also make every POST follow
/// one, which is not a thing to switch on for this.
pub(crate) fn canonical_repo(slug: &str, token: &str) -> Result<String, String> {
    let (status, body) = call(
        "GET",
        &format!("{}/repos/{slug}", api_base()),
        token,
        None,
        "application/vnd.github+json",
        Duration::from_secs(30),
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| complaint(status, &body))?;
    if let Some(name) = value.get("full_name").and_then(|v| v.as_str()) {
        return Ok(name.to_string());
    }
    // A rename. The body carries `/repositories/<id>`, which is the identifier that does not move —
    // asking it is how the new name is learned rather than guessed.
    if let Some(url) = value.get("url").and_then(|v| v.as_str()) {
        let path = url.rsplit_once("/repositories/").map(|(_, id)| id);
        if let Some(id) = path.filter(|id| id.chars().all(|c| c.is_ascii_digit())) {
            let moved = get_json(&format!("/repositories/{id}"), token)?;
            if let Some(name) = moved.get("full_name").and_then(|v| v.as_str()) {
                return Ok(name.to_string());
            }
        }
    }
    Err(value
        .get("message")
        .and_then(|m| m.as_str())
        .map(|m| format!("GitHub said {status}: {m}"))
        .unwrap_or_else(|| complaint(status, &body)))
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
    let (status, text, value) = graphql_answer(query, variables, token)?;
    if let Some(errors) = value.get("errors").and_then(|e| e.as_array()) {
        if !errors.is_empty() {
            let said = error_messages(errors);
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

/// One GraphQL query, keeping what DID answer. Returns `(data, errors)`: `data` as GitHub sent it
/// — a failed alias inside it is `null` — and the top-level `errors` array (empty on a clean
/// answer), each entry carrying GitHub's `path` back to the alias it is about.
///
/// The sibling [`graphql`] fails the whole request on any error, which is right for its callers: a
/// single-field query with errors has no partial worth keeping. This one exists for the batched
/// review search, where five aliases travel in one request and four good answers must not be
/// discarded because the fifth failed — the caller maps each errored alias to its own blind spot
/// instead. Still `Err` when there is nothing to salvage: transport failures, an unreadable body,
/// a missing or null `data` — and a rate limit engages the hold exactly as everywhere else.
pub(crate) fn graphql_partial(
    query: &str,
    variables: serde_json::Value,
    token: &str,
) -> Result<(serde_json::Value, Vec<serde_json::Value>), String> {
    let (status, text, value) = graphql_answer(query, variables, token)?;
    let errors = value
        .get("errors")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    match value.get("data") {
        // `data: null` with errors is GraphQL's whole-request failure, not a partial one.
        Some(data) if !data.is_null() => Ok((data.clone(), errors)),
        _ => Err(match error_messages(&errors).as_str() {
            "" => complaint(status, &text),
            said => said.to_string(),
        }),
    }
}

/// The GraphQL wire both entry points share: POST, parse, and the rate-limit check — an HTTP 200
/// whose errors carry `"type": "RATE_LIMITED"` engages the hold here, so no caller can forget it.
/// The message text is accepted as a second signal in case the type ever changes spelling.
fn graphql_answer(
    query: &str,
    variables: serde_json::Value,
    token: &str,
) -> Result<(u16, String, serde_json::Value), String> {
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
        let said = error_messages(errors);
        let limited = errors.iter().any(|e| {
            e.get("type")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.eq_ignore_ascii_case("RATE_LIMITED"))
        }) || said.to_ascii_lowercase().contains("rate limit");
        if limited {
            engage_hold(token);
            return Err(rate_limit_sentence(match said.is_empty() {
                true => "the GraphQL rate limit is exceeded",
                false => &said,
            }));
        }
    }
    Ok((status, text, value))
}

/// GraphQL error entries' messages, joined — empty when none of them carry one.
fn error_messages(errors: &[serde_json::Value]) -> String {
    errors
        .iter()
        .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
        .collect::<Vec<_>>()
        .join("; ")
}

/// GitHub said the quota is spent: find out when it comes back, and stop calling until then.
///
/// The when comes from `GET /rate_limit`, which reports every resource's quota and **never counts
/// against any of them** — the one question that stays free when everything else is refused. The
/// hold ends at the soonest `reset` of a resource that is actually out (`remaining` 0), because
/// that is the earliest moment any call could succeed again. An unusable answer means a flat
/// fifteen minutes.
fn engage_hold(token: &str) {
    let now = epoch_now();
    let until = rate_reset_after(token, now).unwrap_or(now + 15 * 60);
    *rate_hold() = Some(until);
    let minutes = until.saturating_sub(now).div_ceil(60).max(1);
    eprintln!(
        "skein: GitHub rate limit hit — not calling GitHub for about {minutes}m, resuming around \
         {:02}:{:02} UTC",
        (until / 3600) % 24,
        (until / 60) % 60
    );
}

/// When the soonest spent resource resets, read from `/rate_limit`. `None` when the endpoint could
/// not answer or nothing qualifies — the caller then falls back to a flat wait rather than guess.
fn rate_reset_after(token: &str, now: u64) -> Option<u64> {
    let (status, body) = call(
        "GET",
        &format!("{}/rate_limit", api_base()),
        token,
        None,
        "application/vnd.github+json",
        Duration::from_secs(10),
    )
    .ok()?;
    if !(200..=299).contains(&status) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let resources = value.get("resources")?;
    ["core", "search", "graphql"]
        .into_iter()
        .filter_map(|name| resources.get(name))
        .filter(|r| r.get("remaining").and_then(|v| v.as_u64()) == Some(0))
        .filter_map(|r| r.get("reset").and_then(|v| v.as_u64()))
        .filter(|reset| *reset > now)
        .min()
}

/// The sentence for a rate limit, when the answer is one. GitHub's 403/429 bodies say "API rate
/// limit exceeded" or "secondary rate limit"; named plainly here because a person reading "GitHub
/// said 403" reasonably asks whether skein is being rate limited, and the answer was on hand.
fn rate_limited(status: u16, said: &str) -> Option<String> {
    (matches!(status, 403 | 429) && said.to_ascii_lowercase().contains("rate limit"))
        .then(|| rate_limit_sentence(said))
}

/// The one sentence for a rate limit, wherever it shows up — a 403/429 body or a GraphQL 200 —
/// so both doors report the same fact the same way.
fn rate_limit_sentence(said: &str) -> String {
    format!("GitHub is rate limiting skein — it said: {said}. Skein stops calling GitHub until the limit resets; nothing here needs fixing.")
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
            .map(|m| {
                rate_limited(status, m).unwrap_or_else(|| format!("GitHub said {status}: {m}"))
            })
            .unwrap_or_else(|| complaint(status, &body))),
    }
}

/// What to say when there is no `message` to quote. Clipped, because an HTML error page is not a
/// diagnosis and a whole one in a toast is worse than none.
fn complaint(status: u16, body: &str) -> String {
    let body = body.trim();
    if let Some(said) = rate_limited(status, body) {
        return said;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A GitHub that serves exactly one canned answer per connection, for the failure modes the
    /// fixture in `tests/review_queue.rs` cannot produce: an answer measured in megabytes, and an
    /// answer that arrives but never finishes.
    fn one_shot_github(status: u16, body: Vec<u8>, dribble: bool) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                // Read the request enough to not reset the connection under curl.
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                if dribble {
                    // A tenth of the promised body, then silence with the connection held open:
                    // an answer that is arriving and will not finish inside any sane budget.
                    let _ = stream.write_all(&body[..body.len() / 10]);
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_secs(30));
                }
                let _ = stream.write_all(&body);
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// The regression that reported every big diff as "GitHub did not answer within 30s". curl's
    /// stdout is a pipe holding ~64KB; a body larger than that blocks curl mid-write unless the
    /// pipe is drained while waiting, and the old loop only read it after curl exited — which it
    /// could then never do. One megabyte, well past any pipe buffer, on a short budget: with the
    /// drain in place it comes back in milliseconds.
    #[test]
    fn an_answer_bigger_than_a_pipe_is_read_whole_not_reported_as_silence() {
        let _g = crate::testutil::env_lock();
        let long = "a".repeat(1_000_000);
        let api = one_shot_github(200, format!("{{\"data\":\"{long}\"}}").into_bytes(), false);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let got = get_json_within("/big", "token", Duration::from_secs(10));
        std::env::remove_var("SKEIN_GITHUB_API");
        let got = got.expect("a 1MB answer must be read, not deadlocked on");
        assert_eq!(
            got.get("data").and_then(|v| v.as_str()).map(str::len),
            Some(1_000_000),
            "the whole body arrives, not the first pipe-buffer of it"
        );
    }

    /// "Did not answer" and "was still answering" are different diagnoses — the first sends a
    /// person looking at rate limits, the second at the size of what they asked for. An answer
    /// that is arriving too slowly must be reported as cut off, with how much had come.
    #[test]
    fn an_answer_still_arriving_when_time_runs_out_says_so() {
        let _g = crate::testutil::env_lock();
        let api = one_shot_github(200, vec![b'x'; 500_000], true);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let err = get_json_within("/slow", "token", Duration::from_secs(2));
        std::env::remove_var("SKEIN_GITHUB_API");
        let err = err.expect_err("an answer that never finishes must fail");
        assert!(
            err.contains("still answering"),
            "an arriving answer is cut off, not called silence: {err}"
        );
        assert!(
            !err.contains("did not answer"),
            "the one sentence this must not be: {err}"
        );
    }

    /// A rate limit answers 403 with the reason in the body. Shown as what it is, because "GitHub
    /// said 403" makes a person ask exactly the question the body already answered.
    #[test]
    fn a_rate_limit_is_named_rather_than_left_as_a_status_code() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let api = one_shot_github(
            403,
            br#"{"message":"API rate limit exceeded for installation ID 1."}"#.to_vec(),
            false,
        );
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let err = get_json("/user", "token");
        std::env::remove_var("SKEIN_GITHUB_API");
        let err = err.expect_err("a 403 is an error");
        assert!(
            err.contains("rate limiting skein"),
            "the diagnosis is in the sentence: {err}"
        );
    }

    /// A GitHub whose quota is spent: everything answers 403 "rate limit", except `/rate_limit`,
    /// which reports core out (resetting at `reset`) and graphql out an hour later. Counts what it
    /// is asked, per path, so a test can prove a call never arrived.
    fn spent_github(reset: u64) -> (String, std::sync::Arc<AtomicU64>, std::sync::Arc<AtomicU64>) {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let spent = std::sync::Arc::new(AtomicU64::new(0));
        let quota = std::sync::Arc::new(AtomicU64::new(0));
        let (spent_count, quota_count) = (spent.clone(), quota.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let (status, body) = if request.starts_with("GET /rate_limit") {
                    quota_count.fetch_add(1, Ordering::SeqCst);
                    (
                        200u16,
                        format!(
                            r#"{{"resources":{{"core":{{"remaining":0,"reset":{reset}}},"search":{{"remaining":30,"reset":{reset}}},"graphql":{{"remaining":0,"reset":{}}}}}}}"#,
                            reset + 3600
                        ),
                    )
                } else {
                    spent_count.fetch_add(1, Ordering::SeqCst);
                    (
                        403u16,
                        r#"{"message":"API rate limit exceeded for user ID 1."}"#.to_string(),
                    )
                };
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), spent, quota)
    }

    /// The regression SKEIN-208 exists for: once the quota was dead, every refresh still fired
    /// ~45 doomed requests. The first rate-limited answer must engage a hold read from
    /// `/rate_limit` — ending at the SOONEST spent reset (core here, not graphql's an hour later)
    /// — and the second call must die at home, never reaching the wire.
    #[test]
    fn a_spent_quota_stops_the_next_call_before_it_leaves_the_process() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let reset = epoch_now() + 600;
        let (api, spent, quota) = spent_github(reset);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let first = get_json("/user", "token");
        let second = get_json("/user", "token");
        std::env::remove_var("SKEIN_GITHUB_API");
        let first = first.expect_err("a spent quota is an error");
        assert!(
            first.contains("rate limiting skein"),
            "the first call reports the limit: {first}"
        );
        assert_eq!(
            *rate_hold(),
            Some(reset),
            "the hold ends at the SOONEST spent reset, not the latest"
        );
        let second = second.expect_err("a held call is an error");
        assert!(
            second.contains("resuming in about 10m"),
            "the refusal names the wait: {second}"
        );
        assert_eq!(
            spent.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the second call must never reach the server"
        );
        assert_eq!(
            quota.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "engaging asks /rate_limit exactly once"
        );
    }

    /// A hold is a timer, not a switch: once its moment passes, calls flow again without anyone
    /// resetting anything — and the spent hold is cleared on the way through.
    #[test]
    fn an_elapsed_hold_lets_calls_reach_github_again() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        set_rate_hold(Some(epoch_now() - 5));
        let api = one_shot_github(200, br#"{"fine":true}"#.to_vec(), false);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let got = get_json("/user", "token");
        std::env::remove_var("SKEIN_GITHUB_API");
        let got = got.expect("an elapsed hold must not block");
        assert_eq!(got.get("fine").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            *rate_hold(),
            None,
            "an elapsed hold is cleared once a call passes it"
        );
    }

    /// `/rate_limit` is the door a hold must leave open: it is free, and it is where the hold's
    /// end is learned, so a hold that gated it could never be re-measured.
    #[test]
    fn the_rate_limit_endpoint_passes_through_an_active_hold() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        set_rate_hold(Some(epoch_now() + 600));
        let api = one_shot_github(200, br#"{"resources":{}}"#.to_vec(), false);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let quota = get_json("/rate_limit", "token");
        let other = get_json("/user", "token");
        std::env::remove_var("SKEIN_GITHUB_API");
        quota.expect("/rate_limit must pass through a hold");
        let other = other.expect_err("everything else must not");
        assert!(
            other.contains("not calling GitHub"),
            "the refusal says what is happening: {other}"
        );
    }

    /// A GitHub whose GraphQL quota is spent the way it actually spends: `/graphql` answers HTTP
    /// 200 with a `RATE_LIMITED` errors array, and `/rate_limit` reports graphql out until
    /// `reset`. Counts per path, so a test can prove a call never arrived.
    fn graphql_spent_github(
        reset: u64,
    ) -> (String, std::sync::Arc<AtomicU64>, std::sync::Arc<AtomicU64>) {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let gql = std::sync::Arc::new(AtomicU64::new(0));
        let quota = std::sync::Arc::new(AtomicU64::new(0));
        let (gql_count, quota_count) = (gql.clone(), quota.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let body = if request.starts_with("GET /rate_limit") {
                    quota_count.fetch_add(1, Ordering::SeqCst);
                    format!(
                        r#"{{"resources":{{"core":{{"remaining":4000,"reset":{reset}}},"search":{{"remaining":30,"reset":{reset}}},"graphql":{{"remaining":0,"reset":{reset}}}}}}}"#
                    )
                } else {
                    gql_count.fetch_add(1, Ordering::SeqCst);
                    r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 123"}]}"#
                        .to_string()
                };
                let head = format!(
                    "HTTP/1.1 200 X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), gql, quota)
    }

    /// The shape the live failure took: GraphQL's primary rate limit is an HTTP 200 whose errors
    /// say `"type": "RATE_LIMITED"` — no 403 anywhere — and GraphQL search is where nearly all of
    /// skein's quota goes. It must engage the hold exactly as a 403 body does: the first call
    /// names the wait, the second dies at home.
    #[test]
    fn a_graphql_rate_limit_inside_a_200_engages_the_hold() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let reset = epoch_now() + 600;
        let (api, gql, quota) = graphql_spent_github(reset);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let first = graphql("query { x }", serde_json::json!({}), "token");
        let second = graphql("query { x }", serde_json::json!({}), "token");
        std::env::remove_var("SKEIN_GITHUB_API");
        let first = first.expect_err("a spent GraphQL quota is an error");
        assert!(
            first.contains("rate limiting skein") && first.contains("stops calling"),
            "the 200 is reported as the rate limit it is: {first}"
        );
        assert_eq!(
            *rate_hold(),
            Some(reset),
            "a 200-shaped rate limit engages the hold at graphql's reset"
        );
        let second = second.expect_err("a held call is an error");
        assert!(
            second.contains("resuming in about 10m"),
            "the refusal names the wait: {second}"
        );
        assert_eq!(
            gql.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the second call must never reach the server"
        );
        assert_eq!(
            quota.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "engaging asks /rate_limit exactly once"
        );
    }
}
