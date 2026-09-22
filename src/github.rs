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
//! **Why curl rather than an HTTP crate.** The reason is the token: curl reads its options from
//! **stdin**, so the `Authorization` header never appears in `ps` or in any shell history. An HTTP
//! crate would be tidier and would add a TLS stack, a dependency tree and a second way of doing what
//! already works. curl is present on every macOS and every ordinary Linux, and skein already
//! required it.
//!
//! **"The one way" is meant literally.** [`crate::gitgate`] used to mint App tokens through a curl
//! wrapper of its own, and a second client is not a second style — it is a second set of answers to
//! every question this module spent commits getting right. That one never asked for an HTTP status,
//! spent its request body on argv, and knew nothing of the hold. It is gone; `gitgate` calls
//! [`get_json`] and [`send_json`] with its JWT like everything else.
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
//!
//! **And a hold is not a blind timer.** GitHub has two kinds of limit and skein used to treat them
//! as one: a primary quota that is spent, which `/rate_limit` reports with a `reset` worth quoting,
//! and a secondary burst limit, which spends no quota, so `/rate_limit` has nothing to say about it
//! and any wait skein prints is skein's own. [`Because`] is that distinction, [`BLIND_HOLD`] is
//! what the second one costs, and [`refuse_while_held`] re-asks the free endpoint before refusing,
//! so a quota that has visibly come back ends the hold instead of running it out.

use crate::util::output_with_timeout_why;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Per-process counter for [`RequestScratch`] names, so two threads never pick the same one.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// How many taken names [`RequestScratch::new`] steps past before it gives up and says so.
const SCRATCH_ATTEMPTS: u32 = 64;

/// **The directory a GitHub call keeps its request body and header dump in, made for that call and
/// entered by nobody else** (SKEIN-1022).
///
/// Both files used to sit directly in the temp directory under a name anyone could predict —
/// `skein-req-<pid>-<n>`, with `n` counting up from nought — and both opens FOLLOWED whatever was
/// already there: `File::create` has no `O_EXCL`, and curl's `-D` is the same (measured with curl
/// 8.18.0: a symlink at the `-D` path had its target truncated and replaced by the header block). On
/// a host with a shared world-writable `/tmp` and `fs.protected_symlinks=0`, another local user could
/// plant a symlink at the next name and have skein overwrite any file skein can write.
///
/// **A directory, rather than `create_new` on each file, because skein does not open the second
/// file — curl does**, and nothing tells curl to refuse a symlink. Pre-creating the header file
/// exclusively would hold only where `/tmp` is sticky: without the sticky bit the other user may
/// unlink it and plant the symlink between this side's create and curl's open. `mkdir` is exclusive
/// by definition, fails on a symlink at the name rather than following it (`EEXIST`, measured), and
/// makes the directory 0700 at birth — so no path under it can be planted, whichever program opens
/// it. The request body is still written with [`crate::secret::create_private`] inside it.
///
/// **A name already taken is stepped past, never used and never removed.** Whatever is there —
/// somebody's planted symlink, or the directory of an earlier run that was SIGKILLed before its
/// guard could drop, which the pid and counter can repeat after a reboot — is not this call's, and a
/// GitHub call must not start failing over debris. So the next counter value is tried, up to
/// [`SCRATCH_ATTEMPTS`], and only a temp directory full of taken names is an error.
///
/// **The pid is the LAST field of the name**, `skein-gh-<n>-<pid>`, and that is deliberate:
/// `tests/common/mod.rs`'s `sweep_abandoned` reads the last `-` field of every `skein-*` entry
/// in the temp directory as the pid of the run that owns it, and removes the entry when no such
/// process is alive. Under the old `<pid>-<n>` shape it would have read the COUNTER as a pid — and
/// a directory, unlike the old loose files, is something `remove_dir_all` removes. Pid last, a live
/// call's directory is kept and a killed one's is swept.
struct RequestScratch {
    dir: std::path::PathBuf,
}

impl RequestScratch {
    fn new(base: &std::path::Path) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        for _ in 0..SCRATCH_ATTEMPTS {
            let dir = base.join(format!(
                "skein-gh-{}-{}",
                REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                std::process::id()
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Ok(RequestScratch { dir }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "{SCRATCH_ATTEMPTS} names in a row were already taken in {}",
                base.display()
            ),
        ))
    }
}

impl Drop for RequestScratch {
    /// `remove_dir_all` does not follow a symlink inside what it removes, and nothing but this call
    /// could have put one there.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How long a hold lasts when `/rate_limit` reports nothing spent, or cannot be read at all.
///
/// **Sixty seconds because that is GitHub's own number.** Its rate-limit guidance says: if a
/// `Retry-After` header is present, wait that long; else if `x-ratelimit-remaining` is 0, wait for
/// `x-ratelimit-reset`; **else wait at least one minute before retrying**. The middle case is
/// [`Because::QuotaSpent`] below and keeps GitHub's own reset. This constant is the last case —
/// the secondary (burst/concurrency) limits, which spend no primary quota and typically clear in
/// seconds.
///
/// It replaces a flat fifteen minutes, and the fifteen was a guess that cost real work. Measured
/// on 2026-08-27: the rig logged `not calling GitHub for about 15m` twice, and during both windows
/// `GET /rate_limit` reported `core 5000/5000` and `graphql 5000/5000` while `curl` against the
/// same token answered immediately. Every summary in the cockpit went blank for the duration.
///
/// **Guessing short is the cheap mistake here.** If the real cause was a spent quota that
/// `/rate_limit` could not be read to confirm, a minute later one request is spent finding that
/// out, and it self-corrects the moment `/rate_limit` answers — one doomed request a minute
/// against the ~45 a refresh cycle used to fire (SKEIN-208), which is what the hold exists for.
/// Guessing long is the expensive one, and it is the one that was measured.
const BLIND_HOLD: u64 = 60;

/// The least time between two `/rate_limit` re-measurements of a hold that is already in force.
///
/// Asking is free — the endpoint is exempt from every primary quota, which is why [`call`] lets it
/// through a hold at all (`url.ends_with("/rate_limit")` below) — but free of *quota* is not free
/// of *requests*, and a refresh cycle meets a hold ~45 times. Forty-five probes in a moment is the
/// burst shape that trips a secondary limit, i.e. the very thing being waited out.
///
/// One minute is also exactly [`BLIND_HOLD`], and that is the point rather than a coincidence: a
/// blind hold is over before its first re-measurement could fire, so the probe only ever costs
/// anything for a hold long enough to be worth ending early.
const RECHECK_EVERY: u64 = 60;

/// Why skein is not calling GitHub — and therefore what it can honestly say about when it will.
///
/// The two are not the same refusal and must not read as the same sentence. A spent quota comes
/// with GitHub's own `reset`: there is a minute count a reader can trust and plan around. A
/// secondary limit comes with nothing — it spends no quota, so `/rate_limit` has nothing to say
/// about it — and any minute count skein prints there is invented. Both used to print one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Because {
    /// `/rate_limit` named a resource with `remaining` 0 and a `reset` still ahead. The hold ends
    /// at GitHub's moment, not skein's.
    QuotaSpent,
    /// GitHub refused, and no primary quota was spent — `/rate_limit` reported every resource with
    /// quota left, or could not be read. [`BLIND_HOLD`] long, and no reset to quote.
    NoQuotaSpent,
}

/// A hold in force: until when, why, and when its reason was last measured against `/rate_limit`.
#[derive(Clone, Copy, Debug)]
struct Hold {
    /// Epoch seconds until which GitHub must not be called.
    until: u64,
    /// Which sentence this hold is entitled to say.
    because: Because,
    /// When `/rate_limit` last said so, so the re-measurement can be rationed by [`RECHECK_EVERY`].
    checked: u64,
}

/// The hold, when a rate limit is in force. One value for the whole process, because the limit is
/// per-token and every caller here shares the token: once the quota is spent, the ~45 requests a
/// refresh cycle fires would all fail the same way.
static RATE_HOLD: Mutex<Option<Hold>> = Mutex::new(None);

/// The hold, poison-tolerant for the same reason as [`crate::testutil::env_lock`]: the value is a
/// timestamp and the reason for it, and there is no invariant a panicking test could have
/// corrupted.
fn rate_hold() -> std::sync::MutexGuard<'static, Option<Hold>> {
    RATE_HOLD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Test-only: set or clear the hold directly, so a test can expire one without waiting for it.
/// Just-measured, so it is the hold as [`engage_hold`] leaves one — see [`set_stale_hold`] for the
/// other kind.
#[cfg(test)]
fn set_rate_hold(until: Option<u64>) {
    let now = epoch_now();
    *rate_hold() = until.map(|until| Hold {
        until,
        because: Because::QuotaSpent,
        checked: now,
    });
}

/// Test-only: the hold as it stands, copied out from under the lock so an assertion does not hold
/// it while it formats a failure message.
#[cfg(test)]
fn held() -> Option<Hold> {
    *rate_hold()
}

/// Test-only: a hold whose reason was last measured longer ago than [`RECHECK_EVERY`], so the next
/// call through [`refuse_while_held`] re-measures it instead of trusting it.
#[cfg(test)]
fn set_stale_hold(until: u64, because: Because) {
    let now = epoch_now();
    *rate_hold() = Some(Hold {
        until,
        because,
        checked: now.saturating_sub(RECHECK_EVERY + 1),
    });
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
///
/// **And a test that has not pinned it is refused rather than answered**, the same rule
/// [`crate::util::fleet_root`], [`crate::config::skein_home`] and
/// [`crate::warden_client::Warden::send_within`] already hold for the fleet, the home and the
/// warden.
///
/// The guard is here rather than at the six call sites because this is the single place the
/// default is chosen — and every one of those six hands what it builds straight to [`call`], so
/// there is no "compute the address without going there" reading to protect, as there is for
/// [`crate::warden_client::Warden::configured`]. `grep -rn 'api_base' src/` counts nine lines: this
/// definition, two doc mentions, and six `format!`s that are each an argument to a request.
///
/// **What it costs when it is missing is not this test's failure.** An unauthenticated request to
/// api.github.com is rate-limited per IP, so the budget a stray test spends is the whole box's, and
/// the suite that runs out of it fails somewhere else entirely, for a reason that is not in its
/// own diff. It has already happened here: `review::visit`'s requested-review test downloaded a
/// diff from the real api.github.com, unpinned, and the same run against a neighbour's leftover
/// stub read a whole diff and spent a model call — same assertions, two different halves of the
/// function (SKEIN-693, `src/review/visit.rs`).
pub(crate) fn api_base() -> String {
    if let Some(base) = std::env::var("SKEIN_GITHUB_API")
        .ok()
        .map(|v| v.trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
    {
        return base;
    }
    assert!(
        !crate::util::in_test(),
        "$SKEIN_GITHUB_API is unset in a test process (${marker}). Refusing to fall back to \
         https://api.github.com: every caller of this turns it straight into a request, an \
         unauthenticated one is rate-limited PER IP, and the budget it spends belongs to the whole \
         box — so the suite that runs out of it is not this one (SKEIN-693). Point this test at \
         its own listener (`prwork::testkit::github`, `review::testkit::stub_github`, or \
         `tests/common`'s `fake_github`), or at `http://127.0.0.1:1` where nothing listens if the \
         call is not what is being asserted.",
        marker = crate::util::TEST_MARKER,
    );
    "https://api.github.com".into()
}

/// One path segment of a GitHub URL, with everything a segment may not carry escaped.
///
/// **It lives here, beside [`api_base`], because this module is what every path builder in the
/// crate already calls.** It used to live inside `prwork::remove_label` as a `map` over bytes, and
/// the function ten lines below it built `/repos/{slug}/git/refs/heads/{head_ref}` raw — so the
/// tree knew the rule and applied it in one of two places. A helper in the private scope of the one
/// caller that remembered is not a rule; it is a coincidence.
///
/// The values that need it are the ones a person names rather than skein: a branch, a label, a
/// ref, and — via [`path_segments`] — the two halves of a repository slug. Git forbids
/// `~ ^ : ? * [ \` in a ref and allows `#`, `%`, `&`, `+` and `;` — and `#` is the one that does
/// damage silently, because curl never sends a fragment: `DELETE …/heads/release#2` leaves GitHub
/// reading `DELETE …/heads/release`, which is a *different branch that probably exists*. `%` is the
/// louder half of the same bug, a malformed escape and a 404 that stops a merge train.
///
/// Unreserved characters (RFC 3986 §2.3) pass through; every other byte becomes `%XX`. **`/` is
/// escaped too**, which is why this is a *segment* encoder and not a path one: a ref really can be
/// `feature/x`, and GitHub's refs endpoint accepts `heads/feature/x` — so a caller that wants the
/// slashes kept calls [`path_segments`], which splits on them and comes back here per part.
///
/// **A segment that is nothing but dots is escaped too**, and it is the one rule that is not about
/// a single byte: `.` is unreserved, so `..` would otherwise survive this function character by
/// character and then be removed by RFC 3986 §5.2.4 — `/repos/acme/../../pulls/41` leaving the
/// process as `GET /pulls/41`, a path with a different prefix asked with the caller's credential.
/// `foo.bar` and `v1.0` keep their dots; only an all-dot segment is escaped, which is exactly the
/// set of segments RFC 3986 gives a meaning to.
///
/// **This escape is half a defence and [`call`]'s `--path-as-is` is the other half.** Measured
/// against a loopback listener: curl decodes `%2E` back to `.` and *then* normalises, so
/// `/repos/acme/%2E%2E/%2E%2E/pulls/41` and the unescaped spelling left the process as the same
/// `GET /pulls/41`. `prwork::acts`'s `a_repository_name_made_of_dots_cannot_climb_out_of_the_repos_path`
/// fails if either half is removed.
pub(crate) fn path_segment(raw: &str) -> String {
    // `.` and `..` — and, so no clever spelling is left out, any run of dots, none of which names
    // anything a repository, ref or label could have been called.
    let dots = !raw.is_empty() && raw.bytes().all(|b| b == b'.');
    raw.bytes()
        .map(|b| match b {
            b'.' if dots => "%2E".to_string(),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// A run of path segments, `/` kept as the separator and everything inside each part escaped by
/// [`path_segment`].
///
/// For the two values that are legitimately more than one segment: a **ref** (`feat/x`, which
/// GitHub's refs endpoint takes as separators — `%2F` there 404s every topic branch anybody has
/// named) and a **slug** (`owner/name`, two segments and never one). Encoding either whole is the
/// mistake that turns a working call into a 404; interpolating either raw is the mistake this whole
/// function exists to stop, because `slug` is not skein's own string — [`crate::update::slug_of`]
/// and `gitgate::slug_from_url` cut it out of whatever URL a person registered, and neither one
/// forbids a `?` or a `#` inside the halves it hands back.
pub(crate) fn path_segments(raw: &str) -> String {
    raw.split('/')
        .map(path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// `/repos/{owner}/{name}`, encoded — the prefix that nearly every REST path in this crate is
/// built on.
///
/// It exists so that the prefix is spelled once rather than at each of its twenty call sites, and
/// so that the conversion has a check rather than a claim:
/// `grep -rnE 'format!\("/repos/\{[a-z_]' src/ warden/` answers nothing, and it could not while
/// the shape a caller reaches for was a `format!` with a raw `{slug}` in it (SKEIN-633).
pub(crate) fn repo_path(slug: &str) -> String {
    format!("/repos/{}", path_segments(slug))
}

/// The curl options that carry the credential, fed over stdin so they never reach `ps`.
///
/// A [`crate::secret::Secret`] and not a `&str`, so the `Authorization: Bearer` line in this crate
/// can only be built from one. [`call`] is where the conversion happens — see the note there for
/// why the public entry points are still `&str`.
fn config(token: &crate::secret::Secret, accept: &str) -> String {
    format!(
        "header = \"Authorization: Bearer {}\"\n\
         header = \"Accept: {accept}\"\n\
         header = \"X-GitHub-Api-Version: 2022-11-28\"\n\
         header = \"User-Agent: skein\"\n",
        // A PAT is alphanumeric and a JWT is base64url segments, so neither can hold a quote or a
        // newline — but this is the line that would become an injected curl option if that ever
        // stopped being true, so it is enforced rather than assumed.
        token.expose().replace(['"', '\n', '\\'], "")
    )
}

/// One call. Returns the body as text, whatever it is — JSON, a diff, or an error page.
///
/// The status code comes back separately (curl writes it after the body) because the callers need
/// it: a 404 on a pull request and a 401 on the whole API are different sentences, and a body alone
/// cannot tell them apart.
///
/// The credential arrives as a [`crate::secret::Secret`] and was one for the whole path that
/// reached here — `prq::host_token` mints it, and `get_json`, `send_json` and `graphql` carry it.
/// It used to be converted to one on the first line of this body instead (SKEIN-519, secrets
/// Rule 2), which shut the door at the last room rather than the front: everything upstream of
/// `call` still held the credential as a printable `String`, so a `{token}` anywhere in `prq`,
/// `prwork` or `update` was a leak that compiled.
fn call(
    method: &str,
    url: &str,
    token: &crate::secret::Secret,
    body: Option<&str>,
    accept: &str,
    timeout: Duration,
) -> Result<(u16, String), String> {
    call_reading_headers(method, url, token, body, accept, timeout, false)
        .map(|(status, body, _)| (status, body))
}

/// [`call`], keeping the response headers when the caller asks for them.
///
/// **Opt-in rather than always collected.** One caller wants a header and twenty want a body, and
/// `-D` costs a file write per request — so the twenty do not pay for what they throw away. The one
/// caller is [`token_expiry`], whose answer GitHub puts in no response body at all, so there is no
/// way to get it from the other twenty.
///
/// The dump goes to a FILE and never to stdout, for the same reason the request body does: stdout
/// already carries the body with the status written after it, and a header block mixed into that
/// would be split off as the status by the `rsplit_once('\n')` below. 0600, inside a directory
/// only this call can enter ([`RequestScratch`]), and removed on every path out including the
/// deadline and the transport failure.
fn call_reading_headers(
    method: &str,
    url: &str,
    token: &crate::secret::Secret,
    body: Option<&str>,
    accept: &str,
    timeout: Duration,
    keep_headers: bool,
) -> Result<(u16, String, String), String> {
    use std::io::Write;
    // The hold, checked before anything is spent. `/rate_limit` is exempt: it is free, and it is
    // the endpoint the hold itself is learned from, so gating it would leave no way back out.
    let exempt = url.ends_with("/rate_limit");
    if !exempt {
        if let Some(refusal) = refuse_while_held(token) {
            return Err(refusal);
        }
    }
    // The body goes to a file and the token stays on stdin, because both cannot have stdin: curl
    // reads `--config -` and `--data-binary @-` from the same place, and whichever gets there first
    // consumes the other's input. Found the direct way — the stub API received a curl config as its
    // request body.
    //
    // A file is safe for this half and not for the other: the body is a query, while the token is
    // the thing that must never touch the filesystem or `ps`.
    //
    // Both files live in one directory this call made for itself, and only when there is a file to
    // put in it — see [`RequestScratch`] for why a directory, and why that closes the header half
    // too. Dropped on every path out, including the `?`s below, which the closure it replaced
    // could not reach.
    let scratch = match body.is_some() || keep_headers {
        false => None,
        true => Some(
            RequestScratch::new(&std::env::temp_dir())
                .map_err(|e| format!("making a private directory for the request: {e}"))?,
        ),
    };
    let body_file = match (body, &scratch) {
        (Some(body), Some(scratch)) => {
            let path = scratch.dir.join("request");
            crate::secret::create_private(&path)
                .and_then(|mut file| file.write_all(body.as_bytes()))
                .map_err(|e| format!("writing the request body: {e}"))?;
            Some(path)
        }
        _ => None,
    };
    // Where curl is told to write the response headers, when anybody wants them. Created here,
    // 0600, so that what curl opens is a file this call already owns: measured with curl 8.18.0, a
    // `-D` path that exists is truncated and written in place (same inode, mode kept), and one that
    // is a symlink is FOLLOWED — which is why the directory, not this file, is the defence.
    let header_file = match (keep_headers, &scratch) {
        (true, Some(scratch)) => {
            let path = scratch.dir.join("headers");
            crate::secret::create_private(&path)
                .map_err(|e| format!("making the response-header file: {e}"))?;
            Some(path)
        }
        _ => None,
    };
    let mut args: Vec<String> = vec![
        "-sS".into(),
        // **The path skein built is the path that goes on the wire** (SKEIN-633). Without this,
        // curl decodes `%2E` back to `.` and then applies RFC 3986 §5.2.4 to what it gets, so
        // `/repos/acme/%2E%2E/%2E%2E/pulls/41` — which `path_segment` escaped precisely to stop
        // that — leaves the process as `GET /pulls/41`. Measured against a loopback listener, both
        // spellings squashed identically. So the escape is only half the defence and this is the
        // other half: no path this crate builds ever contains a dot segment it meant, so there is
        // nothing here for curl to be helpful about.
        "--path-as-is".into(),
        // **Reach GitHub DIRECT, so the token skein resolved is the one GitHub sees (SKEIN-548).**
        // The host acts as the person here — `prq::host_token` resolves an exported `GH_TOKEN`, the
        // read PAT in Settings, or a stored write PAT, and returns an error rather than falling
        // through to anything account-wide when none is set. But curl honours `$HTTPS_PROXY`, and
        // the sandbox proxy TERMINATES TLS for the GitHub hosts and replaces the `Authorization`
        // header with the fleet account's (measured: a garbage `Bearer` is 200 through the proxy,
        // 401 direct). Left on the proxy, "reads as you" is a lie — every host call authenticates as
        // the account whatever token was resolved, and with sbx v0.43.0 the resolved token is
        // dropped rather than forwarded. `--noproxy` names the GitHub hosts so this call bypasses
        // the proxy and presents skein's own credential; loopback is listed so a `$SKEIN_GITHUB_API`
        // stub on `127.0.0.1` keeps working (and in a test no proxy is set, so this is then inert).
        // A direct connection the host's egress policy blocks fails as a transport error, which
        // `crate::health` reports as an unreachable line rather than this silently retrying the
        // proxy. There is no account-path exception: an installation token is not resolved here
        // ([`crate::prq::host_credential`] excludes the App), so nothing legitimately wants the
        // proxy's injected account credential.
        "--noproxy".into(),
        "api.github.com,github.com,raw.githubusercontent.com,gist.github.com,localhost,127.0.0.1,::1"
            .into(),
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
    if let Some(path) = &header_file {
        args.push("-D".into());
        args.push(path.display().to_string());
    }
    args.push("--config".into());
    args.push("-".into());

    let child = Command::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own process group, so the deadline below ends the WORK rather than the process this
        // side holds a handle on. curl does not normally fork for a request, which makes this the
        // weakest of the four sites SKEIN-916 names — and the point is that the deadline stops
        // depending on that being true: whatever `curl` on this box turns out to be (a wrapper
        // script, a shell alias installed as a binary, a build that shells out for a proxy helper),
        // the kill reaches it. A NEW group and not skein's, which a negative kill would turn into
        // skein killing itself.
        //
        // **The cost, paid rather than taken**, as `util::run_bounded` states it: this child no
        // longer shares the terminal's foreground group, so Ctrl-C stops reaching it that way. The
        // guard below registers the group with `util::forward_interrupts`'s handler, which is where
        // a CLI Ctrl-C reaches it instead. In `skein-server`, which deliberately installs no
        // handler, the guard is an entry nothing ever signals — and the deadline is unchanged.
        .process_group(0)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "curl is not installed, and skein reads GitHub with it".to_string()
            } else {
                format!("curl: {e}")
            }
        });
    let mut child = child?;
    // Registered before this side blocks on anything, so a Ctrl-C arriving between the spawn and
    // the first `try_wait` finds the group rather than an empty table. Underscored because the
    // waiting loop below returns from inside: the guard goes when the scope does, which is the line
    // after the reap rather than somewhere a `drop` call could be written.
    let _forwarding = crate::util::forwarding(child.id() as libc::pid_t);
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
            // The GROUP, not the pid: see the spawn above. `end_group` reaps as well.
            crate::util::end_group(&mut child);
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
                    crate::util::end_group(&mut child);
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
    // Read before `scratch` goes, and empty when nobody asked for it or curl never got as far as a
    // response — which is a real state the caller must tell apart from "no such header".
    let headers = header_file
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    drop(scratch);
    if !status.success() {
        return Err(transport_failure(
            status.code(),
            &String::from_utf8_lossy(&stderr),
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
    Ok((status, body, headers))
}

/// `GET`, as JSON. A non-2xx answers with GitHub's own `message` when it has one, because that is
/// the sentence worth showing ("Bad credentials", "Not Found") rather than a bare status.
pub(crate) fn get_json(
    path: &str,
    token: &crate::secret::Secret,
) -> Result<serde_json::Value, String> {
    get_json_within(path, token, Duration::from_secs(30))
}

/// **When GitHub says this credential stops working** — the one question no response body answers.
///
/// GitHub returns `github-authentication-token-expiration` on an authenticated REST call made with
/// a personal access token that carries an expiry, and returns no such header when the token has
/// none. Measured on this fleet on 2026-09-15 (SKEIN-928): `2026-10-15 13:19:49 UTC`.
///
/// **A missing header is read as "no expiry" only when GitHub actually authenticated the call**,
/// and that is the whole of why this answers with a `Result` rather than an `Option`. Measured from
/// a box on 2026-09-20 with `Authorization: token skein-test-garbage`: `HTTP/2 401`, and no
/// expiration header — so an absent header is also exactly what a dead credential looks like. Every
/// non-2xx is an `Err` here, and the reporting layer renders an `Err` as architecture §2.2's
/// `unknown` rather than as a pass.
///
/// `/user` rather than the quota-free `/rate_limit`: it is the endpoint the header was actually
/// measured on. One request per gate interval is a cheaper price than a green check built on a
/// guess about which endpoints carry the header. Ten seconds is a headers-only read of a tiny
/// document, and it is also how long the cockpit's FIRST health poll waits on a cold gate.
pub(crate) fn token_expiry(token: &crate::secret::Secret) -> Result<Option<String>, String> {
    let url = format!("{}/user", api_base());
    let (status, body, headers) = call_reading_headers(
        "GET",
        &url,
        token,
        None,
        "application/vnd.github+json",
        Duration::from_secs(10),
        true,
    )?;
    match status {
        200..=299 => Ok(expiry_in_headers(&headers).map(str::to_string)),
        _ => Err(complaint(status, &body)),
    }
}

/// The expiry header's value out of a raw header block, or `None` when it is not there.
///
/// Pure, so the parse is proven without a network — including the two things a hand-rolled header
/// reader gets wrong: HTTP header names are case-insensitive, and every line ends `\r\n`, so a
/// value that is not trimmed carries a carriage return into whatever prints it.
pub(crate) fn expiry_in_headers(headers: &str) -> Option<&str> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("github-authentication-token-expiration")
            .then(|| value.trim())
    })
}

/// [`get_json`], with the caller's own budget. For the endpoints whose answers are measured in
/// megabytes — the per-file diff listing of a pull request GitHub refuses to serve whole — where
/// 30s is a poll's budget, not a transfer's.
pub(crate) fn get_json_within(
    path: &str,
    token: &crate::secret::Secret,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", api_base());
    // A GET is idempotent, so a connection that died is worth one more request (SKEIN-271).
    json(ask_twice(|| {
        call(
            "GET",
            &url,
            token,
            None,
            "application/vnd.github+json",
            timeout,
        )
    })?)
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
pub(crate) fn canonical_repo(slug: &str, token: &crate::secret::Secret) -> Result<String, String> {
    let (status, body) = call(
        "GET",
        &format!("{}{}", api_base(), repo_path(slug)),
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
            // Digits only by the filter above, so the encoder cannot change this one — it is
            // here so that no `format!` in this crate puts an unencoded value into a GitHub path,
            // which is the invariant a grep can check and "this one is safe" is not.
            let moved = get_json(&format!("/repositories/{}", path_segment(id)), token)?;
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
pub(crate) fn get_text(
    path: &str,
    token: &crate::secret::Secret,
    accept: &str,
) -> Result<String, String> {
    let url = format!("{}{path}", api_base());
    let (status, body) =
        ask_twice(|| call("GET", &url, token, None, accept, Duration::from_secs(60)))?;
    match status {
        200..=299 => Ok(body),
        _ => Err(complaint(status, &body)),
    }
}

/// `POST`/`PUT` with a JSON body.
pub(crate) fn send_json(
    method: &str,
    path: &str,
    token: &crate::secret::Secret,
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
///
/// **Nothing that goes out through here is ever sent a second time** (SKEIN-341). This is the half
/// that carries skein's mutations: `update_branch` sends `updatePullRequestBranch` and
/// `set_thread_resolved` sends `resolve`/`unresolveReviewThread`. A third non-test caller,
/// `pr_body`, is a QUERY and goes through here anyway — forgoing the retry can cost it one
/// avoidable failure and can never repeat an act, which is the only direction this wire may err
/// in. All three, and nothing else: `grep -rn "github::graphql(" src/`. Every shape that would be
/// worth asking again is *ambiguous* about whether the request was carried out — a dead
/// connection, an empty 200, a 502 of the edge's own HTML — so each is reported as what it is
/// instead. A rebase that landed and is sent again meets its own `expectedHeadOid`, which no
/// longer matches, and the refusal is then written down as the rebase having failed on a branch
/// that was rebased. The retries live in [`graphql_partial`], which reads.
pub(crate) fn graphql(
    query: &str,
    variables: serde_json::Value,
    token: &crate::secret::Secret,
) -> Result<serde_json::Value, String> {
    let (status, text) = graphql_answer(query, variables, token)?;
    let value = graphql_value(status, &text)?;
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
///
/// **A query, so an answer that is not one is asked again once** (SKEIN-271, SKEIN-258) — and
/// *both* retries live here rather than in the shared [`graphql_answer`] (SKEIN-341). A connection
/// that died ([`ask_twice`]) and an edge that shrugged ([`edge_shrug`]) are the two ways a request
/// comes back without GitHub having answered it, and both are ambiguous about whether it was
/// carried out. That is survivable here and nowhere else: the one non-test caller is the batched
/// membership search — `one_request`, found with `grep -rn "graphql_partial" src/` — five `search`
/// aliases that read and write nothing. The sibling [`graphql`] carries the mutations, and asking
/// one of those again is exactly what must not happen — which the shrug retry did for as long as it
/// sat in the wire the two share.
pub(crate) fn graphql_partial(
    query: &str,
    variables: serde_json::Value,
    token: &crate::secret::Secret,
) -> Result<(serde_json::Value, Vec<serde_json::Value>), String> {
    // **An empty 200 is not an answer, and it is asked again once** (SKEIN-258).
    //
    // GitHub answers a request it gave up on server-side with a 200 and no bytes at all — no
    // `errors` array, nothing to parse — and the heaviest thing skein sends is exactly the shape
    // that provokes it: since the searches were batched, one request carries five `search`
    // connections of up to a hundred nodes each. Reported live from a cold first load, where every
    // repo sends one at once, and gone by the next refresh. A 5xx of the edge's own HTML is the
    // same non-answer wearing a different status (SKEIN-266), and [`edge_shrug`] weighs both.
    //
    // One retry, not a loop: a second failure of the same shape is a real condition and the caller
    // must see it. Nothing extra is spent — an answer nobody could read cost the same quota point
    // whether or not it is asked for again.
    let (status, text) = ask_twice(|| {
        let (status, text) = graphql_answer(query, variables.clone(), token)?;
        match edge_shrug(status, &text) {
            true => graphql_answer(query, variables.clone(), token),
            false => Ok((status, text)),
        }
    })?;
    let value = graphql_value(status, &text)?;
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

/// The GraphQL wire both entry points share: POST, and the rate-limit check — an HTTP 200 whose
/// errors carry `"type": "RATE_LIMITED"` engages the hold here, so no caller can forget it. The
/// message text is accepted as a second signal in case the type ever changes spelling.
///
/// **It asks once, and it is the entry point that decides whether to ask again** (SKEIN-341). The
/// retry for an edge shrug used to be right here, where [`graphql`]'s mutations pass through it: a
/// 502 of the edge's own HTML after `updatePullRequestBranch` had already rebased the branch sent
/// the rebase a second time. The same rule [`ask_twice`] is written to — a retry belongs at an
/// entry point that knows whether its request is idempotent, never on the shared wire underneath —
/// so this one carries no retry at all and [`graphql_partial`] carries both.
///
/// The answer comes back UNPARSED for that reason too. Whether the body would parse is half of what
/// [`edge_shrug`] weighs, so turning an unreadable body into an error down here left the entry
/// points with nothing to weigh, which is how the retry ended up in the wire in the first place;
/// [`graphql_value`] is the parse, once the caller has decided. The rate-limit check reads the body
/// when it parses and lets it go when it does not — a body that is not JSON cannot be carrying an
/// `errors` array, so nothing is lost by not insisting.
fn graphql_answer(
    query: &str,
    variables: serde_json::Value,
    token: &crate::secret::Secret,
) -> Result<(u16, String), String> {
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
    if let Some(errors) = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| value.get("errors").and_then(|e| e.as_array()).cloned())
    {
        let said = error_messages(&errors);
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
    Ok((status, text))
}

/// The body of a GraphQL answer, parsed — or the best sentence available about why it could not be.
///
/// One wording for an unreadable answer, shared by both entry points, while each keeps its own
/// decision about whether such an answer may be asked for again. That split is the whole of
/// SKEIN-341: the sentence is common, the retry is not.
fn graphql_value(status: u16, text: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|_| complaint(status, text))
}

/// Did GitHub's EDGE shrug, rather than its API answering? (SKEIN-258, SKEIN-266)
///
/// The API answers JSON — including for its own failures, which arrive as a 200 carrying an
/// `errors` array. Two shapes are not that, and both were reported live within an hour of each
/// other on the same fleet:
///
/// * a **200 with no bytes at all** — the backend gave up and the edge sent the envelope anyway;
/// * a **5xx whose body is not JSON** — `502 Bad Gateway`, nginx's own HTML, which the GitHub API
///   never produces.
///
/// Neither is an answer to parse, and both mean "GitHub never said" — which is a reason to ask
/// again on a read and the reason a write must not (SKEIN-341), so this only says which shape came
/// back and [`graphql_partial`] is the one that acts on it. A 5xx that DOES carry JSON is left
/// alone: that is the API speaking, and papering over what it said is how a real refusal turns into
/// a silent empty queue.
fn edge_shrug(status: u16, text: &str) -> bool {
    let body = text.trim();
    if status == 200 {
        return body.is_empty();
    }
    (500..=504).contains(&status) && serde_json::from_str::<serde_json::Value>(body).is_err()
}

/// Was this failure GitHub refusing to TAKE the request, rather than answering it? (SKEIN-266)
///
/// The caller that batches — `prq::search_prs_all`, five membership searches in one request —
/// needs this to tell "GitHub is unavailable" from "GitHub would not take it all at once", because
/// the answers are opposite: the first must be reported once and believed, the second must be
/// asked again in halves. Splitting an outage would turn one honest sentence back into five.
///
/// Recognised from the sentences [`complaint`] itself writes, and it lives beside them for that
/// reason: one module owns both the wording and what the wording means, so the two cannot drift
/// into a caller sniffing strings it does not own. The test below is what fails if they do.
pub fn edge_refused(why: &str) -> bool {
    // Rate limiting is neither: skein stops calling entirely, and splitting would only spend more
    // of a budget that has already run out.
    if why.contains("rate limiting skein") {
        return false;
    }
    why.contains("with an empty body")
        || ["502", "503", "504"]
            .iter()
            .any(|code| why.contains(&format!("GitHub answered {code}")))
}

/// The sentence for a call that never became an answer, written from curl's EXIT STATUS. (SKEIN-271)
///
/// Reported live while posting a drafted review:
///
/// ```text
/// curl failed: curl: (92) HTTP/2 stream 1 was not closed cleanly: CANCEL (err 8)
/// ```
///
/// The peer cancelled the HTTP/2 stream after the headers, so there is **no status and no body** —
/// nothing for [`edge_shrug`] to weigh or [`edge_refused`] to read, both of which take an answer
/// apart. Every ladder above therefore let it fall through as a hard error, one attempt, no retry.
/// So the transport failure gets its own kind, named here and read by [`connection_died`], for the
/// same reason `complaint` and `edge_refused` live together: one module owns both the wording and
/// what the wording means.
///
/// The wording is the other half of the fix. "GitHub did not answer" is what this used to become,
/// and it reads as *GitHub* being at fault; "the connection died" and "GitHub refused" ask
/// different things of whoever reads them — the first says ask again, the second says something is
/// wrong with the request. The exit code is kept in the sentence because it is the one durable
/// handle on which failure this was (92 is a cancelled HTTP/2 stream, 18 a truncated body, 7 a
/// connection that was never made) and curl's own words move between versions.
fn transport_failure(code: Option<i32>, stderr: &str) -> String {
    let said = stderr.trim();
    // curl prints "curl: (92) …" with `-sS`; the code is already in the sentence, so quoting the
    // prefix as well would say it twice.
    let said = said
        .strip_prefix("curl:")
        .map(str::trim_start)
        .unwrap_or(said);
    let said = match said.split_once(')') {
        Some((head, rest)) if head.starts_with('(') => rest.trim(),
        _ => said,
    };
    let said = crate::util::clip(said, 200);
    match (code, said.is_empty()) {
        (Some(code), false) => {
            format!("the connection to GitHub died before it answered (curl exit {code}: {said})")
        }
        (Some(code), true) => {
            format!("the connection to GitHub died before it answered (curl exit {code})")
        }
        (None, false) => {
            format!("the connection to GitHub died before it answered ({said})")
        }
        (None, true) => "the connection to GitHub died before it answered".to_string(),
    }
}

/// Did the CONNECTION die, rather than GitHub answering? (SKEIN-271)
///
/// The one question that separates "ask again" from "this request is wrong". Read from the
/// sentence [`transport_failure`] writes, beside it, so the two cannot drift.
///
/// What this deliberately does **not** cover is the deadline: "GitHub did not answer within 30s"
/// and "GitHub was still answering after 30s" are skein's own choice to stop waiting, and asking
/// again buys a second wait of the same length — a caller that wants those retried has to say so
/// itself. Nor does it cover curl being missing, which no number of attempts will fix.
///
/// **Idempotence is the caller's to know.** A dead stream is *ambiguous* for anything that writes:
/// the peer cancelled after the headers, so the request may well have been carried out before the
/// answer was lost. Reads may ask again on this; writes must find out what happened first — see
/// [`crate::prq::submit_review_with_comments`].
pub fn connection_died(why: &str) -> bool {
    why.contains("the connection to GitHub died")
}

/// Ask an **idempotent** call again, once, when the connection died rather than GitHub answering.
///
/// The same shape as the empty-200 retry in [`graphql_partial`] and for the same reason: a second
/// failure of the same kind is a real condition the caller must see, and one retry costs one
/// request. It lives at the read entry points rather than inside [`call`] on purpose — `call` is
/// the wire under `POST /reviews` and `PUT /merge` too, and a retry there would re-send those
/// blind.
fn ask_twice<T>(mut ask: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    match ask() {
        Err(why) if connection_died(&why) => {
            eprintln!("skein: {why} — asking again once, because reading again costs a request");
            ask()
        }
        other => other,
    }
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
/// that is the earliest moment any call could succeed again.
///
/// **A refusal that spent no quota is not that, and used to be treated as if it were.** GitHub's
/// secondary limits — the burst and concurrency ones — are answered as a 403/429 saying "rate
/// limit", so they arrive here, but they cost no primary quota: [`quota`] finds every resource
/// full and has no `reset` to offer. That answer used to mean a flat fifteen minutes, which on
/// 2026-08-27 stopped a live cockpit twice while `/rate_limit` reported `core 5000/5000` and a
/// hand-run `curl` on the same token answered on the spot. It now means [`BLIND_HOLD`], and the
/// hold remembers WHICH refusal it is so it can stop quoting a reset it never had.
fn engage_hold(token: &crate::secret::Secret) {
    let now = epoch_now();
    // Measured here rather than after the fact, and stamped `checked: now`, because engaging IS
    // the first measurement: a hold born this second must not be re-measured the next one.
    let hold = match quota(token, now) {
        Quota::Spent(reset) => Hold {
            until: reset,
            because: Because::QuotaSpent,
            checked: now,
        },
        Quota::Free | Quota::Unknown => Hold {
            until: now + BLIND_HOLD,
            because: Because::NoQuotaSpent,
            checked: now,
        },
    };
    *rate_hold() = Some(hold);
    match hold.because {
        Because::QuotaSpent => eprintln!(
            "skein: GitHub rate limit hit — not calling GitHub for about {}m, resuming around \
             {:02}:{:02} UTC",
            hold.until.saturating_sub(now).div_ceil(60).max(1),
            (hold.until / 3600) % 24,
            (hold.until / 60) % 60
        ),
        // No clock in this one, deliberately: there is no reset behind it, and a time printed
        // here would be skein's guess wearing GitHub's authority.
        Because::NoQuotaSpent => eprintln!(
            "skein: GitHub asked skein to slow down — no quota is spent, so there is no reset to \
             wait for; pausing calls for {BLIND_HOLD}s"
        ),
    }
}

/// The hold, consulted before a call spends anything — and re-measured before it refuses.
///
/// `Some(sentence)` refuses the call with the sentence to show; `None` lets it through.
///
/// **Why a hold in force is re-measured at all.** It used to be a bare timer: [`call`] compared the
/// clock against `until` and nothing else, so once a hold was engaged nothing could end it early —
/// not a quota visibly back at 5000/5000, not a person pressing refresh. The one question that
/// would settle it is the one question that is still free while everything else is refused, so it
/// is now asked: rationed to [`RECHECK_EVERY`], and stamped before the lock is released so twenty
/// threads meeting the same stale hold send one probe between them rather than twenty.
fn refuse_while_held(token: &crate::secret::Secret) -> Option<String> {
    let now = epoch_now();
    let hold = {
        let mut guard = rate_hold();
        let mut hold = (*guard)?;
        if now >= hold.until {
            // A hold is a timer, not a switch: its moment passing is enough, and the spent hold is
            // cleared on the way through.
            *guard = None;
            return None;
        }
        if now.saturating_sub(hold.checked) < RECHECK_EVERY {
            return Some(refusal(&hold, now));
        }
        hold.checked = now;
        *guard = Some(hold);
        hold
    };
    // The guard is dropped above on purpose. The probe is an HTTP round trip with its own ten
    // second budget, and holding the process-wide lock across it would park every other caller on
    // a call they are about to be refused anyway.
    match quota(token, now) {
        // Every resource GitHub reports has quota left, so whatever it refused skein for is over
        // as far as the primary limits can see. Sitting out the rest of the hold would refuse
        // calls GitHub would now answer — which is the half of the defect no "try again" reached.
        Quota::Free => {
            *rate_hold() = None;
            eprintln!(
                "skein: GitHub reports every quota unspent — the rate-limit hold is lifted early"
            );
            None
        }
        // Still out, and GitHub's CURRENT reset rather than the one learned when the hold was
        // engaged: a second window can open while the first is being waited out.
        Quota::Spent(reset) => {
            let hold = Hold {
                until: reset,
                because: Because::QuotaSpent,
                checked: now,
            };
            *rate_hold() = Some(hold);
            Some(refusal(&hold, now))
        }
        // Nothing was learned, so nothing changes: the hold stands as it was engaged.
        Quota::Unknown => Some(refusal(&hold, now)),
    }
}

/// What skein says when it refuses a call itself, rather than sending it.
///
/// Two sentences, because there are two facts. A spent quota has GitHub's own `reset` behind it, so
/// the minutes are a number the reader can plan around. A secondary limit has nothing behind it —
/// it spends no quota, so there is no reset anywhere to read — and the seconds are skein's own
/// back-off, named as such. Both used to print the same "resuming in about 15m" — measured on
/// 2026-08-27, twice, with every quota reported full — and only one of them was ever entitled to
/// say when.
///
/// Both keep "not calling GitHub" in the words: it is what [`crate::prq`]'s tests read a refusal by,
/// and it is the fact common to the two.
fn refusal(hold: &Hold, now: u64) -> String {
    let left = hold.until.saturating_sub(now);
    match hold.because {
        Because::QuotaSpent => format!(
            "GitHub is rate limiting skein — resuming in about {}m; until then skein is not \
             calling GitHub at all, and what you see is the last answer it holds.",
            left.div_ceil(60).max(1)
        ),
        Because::NoQuotaSpent => format!(
            "GitHub asked skein to slow down — no quota is spent, so there is no reset time to \
             give you; skein is not calling GitHub for another {left}s or so, and what you see is \
             the last answer it holds."
        ),
    }
}

/// What `/rate_limit` says about skein's quotas right now.
///
/// Three answers rather than an `Option<u64>`, because the missing one is the whole bug: "no
/// resource is out" and "I could not find out" were both `None`, and both bought a fifteen minute
/// blackout. They are opposite facts.
enum Quota {
    /// At least one resource is out, and the soonest of them comes back at this epoch second —
    /// the earliest moment any call could succeed again.
    Spent(u64),
    /// Every resource skein uses reported quota left. Whatever GitHub refused, it was not a
    /// primary limit, so there is no reset to wait for.
    Free,
    /// `/rate_limit` could not be read — unreachable, non-2xx, unparseable, or an answer naming
    /// none of the resources skein spends. Nothing was learned.
    Unknown,
}

/// Ask `/rate_limit`, which is exempt from every quota it reports and therefore the one question
/// that stays free while everything else is refused.
fn quota(token: &crate::secret::Secret, now: u64) -> Quota {
    let Ok((status, body)) = call(
        "GET",
        &format!("{}/rate_limit", api_base()),
        token,
        None,
        "application/vnd.github+json",
        Duration::from_secs(10),
    ) else {
        return Quota::Unknown;
    };
    if !(200..=299).contains(&status) {
        return Quota::Unknown;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
        return Quota::Unknown;
    };
    let Some(resources) = value.get("resources") else {
        return Quota::Unknown;
    };
    // The three skein actually spends. A resource that is not reported is not evidence either way,
    // so an answer carrying none of them is `Unknown` rather than `Free`.
    let named: Vec<&serde_json::Value> = ["core", "search", "graphql"]
        .into_iter()
        .filter_map(|name| resources.get(name))
        .collect();
    if named.is_empty() {
        return Quota::Unknown;
    }
    let soonest = named
        .iter()
        .filter(|r| r.get("remaining").and_then(|v| v.as_u64()) == Some(0))
        .filter_map(|r| r.get("reset").and_then(|v| v.as_u64()))
        // A `reset` already behind us is a window that has closed: the quota is back, whatever the
        // `remaining` in the same snapshot says.
        .filter(|reset| *reset > now)
        .min();
    match soonest {
        Some(reset) => Quota::Spent(reset),
        None => Quota::Free,
    }
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
///
/// **A `204 No Content` has no body, and that is the answer** (SKEIN-346). The parse used to run
/// before the status was ever looked at, so `serde_json::from_str("")` failed and every 204 came
/// back as "GitHub answered 204 with an empty body" — the `200..=299` arm below was unreachable for
/// any 204 GitHub has ever sent. `DELETE /repos/{slug}/git/refs/heads/{ref}` is exactly that
/// answer, and it is the call `prwork`'s train makes on the step after it merges: what a person
/// read after every clean merge-and-delete was "merged #12, but topic is still there: GitHub
/// answered 204 with an empty body" — a lie about a branch that was gone.
///
/// **204 by name, rather than "any 2xx whose body is empty".** An empty **200** is the opposite
/// thing: the edge shrugging with nothing behind it (SKEIN-258) — the shape [`edge_shrug`] names,
/// [`graphql_partial`] asks again, and [`edge_refused`] reads out of the very sentence below.
/// Calling that success would turn a request GitHub never answered into a `null` a caller
/// believes. 204 is the one status whose empty body is what HTTP says it must be, so it is the one
/// status that can be read as one.
fn json((status, body): (u16, String)) -> Result<serde_json::Value, String> {
    // JSON's own "no value", rather than an invented `{}`: a caller that goes looking for a field
    // finds nothing, which is the truth, instead of an object that says the answer had none.
    if status == 204 {
        return Ok(serde_json::Value::Null);
    }
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

    /// **The expiry header is found however GitHub spells it, and not found when it is absent**
    /// (SKEIN-928).
    ///
    /// Two things a hand-rolled header reader gets wrong, and both of them are silent: HTTP header
    /// names are case-insensitive, and every line ends `\r\n`, so an untrimmed value carries a
    /// carriage return into whatever prints it. The third case is the one the whole check turns on
    /// — a header block with no expiry in it, which is what GitHub returns for a token that never
    /// expires AND for one it just refused.
    ///
    /// Counterfactuals: replacing `eq_ignore_ascii_case` with `==` makes `found however GitHub
    /// capitalises it` fail; dropping the `trim` on the value makes `no carriage return` fail;
    /// defaulting the `find_map` to the first header makes `absent means absent` fail. Proven by
    /// sabotage — swapping in `==` fired the first of those.
    #[test]
    fn the_expiry_header_is_read_however_it_is_spelled() {
        let block = "HTTP/2 200\r\n\
                     Github-Authentication-Token-Expiration: 2026-10-15 13:19:49 UTC\r\n\
                     x-ratelimit-limit: 5000\r\n\r\n";
        assert_eq!(
            expiry_in_headers(block),
            Some("2026-10-15 13:19:49 UTC"),
            "found however GitHub capitalises it, and with no carriage return left on the end"
        );
        assert_eq!(
            expiry_in_headers("HTTP/2 200\r\nGITHUB-AUTHENTICATION-TOKEN-EXPIRATION: x\r\n"),
            Some("x"),
            "header names are case-insensitive in both directions"
        );
        // The measured shape of a refusal, from a box on 2026-09-20 with
        // `Authorization: token skein-test-garbage`: a 401 and no expiration header at all. This
        // must answer None rather than something, because the caller reads None beside a 2xx as
        // "this token never expires" — and beside a 401 as nothing at all.
        assert_eq!(
            expiry_in_headers("HTTP/2 401\r\nx-github-request-id: DA06:6D792\r\n\r\n"),
            None,
            "absent means absent: there is no header here to mistake for one"
        );
    }

    /// The credential every stub GitHub below is called with.
    ///
    /// Prefixed `skein-test-` deliberately: a fixture that looked like a real token
    /// (`gho_…`, `ghp_…`) is indistinguishable from one in a grep, and this tree has already had
    /// to sweep a client's real strings out of its fixtures once.
    fn fixture_token() -> crate::secret::Secret {
        crate::secret::Secret::new("skein-test-github-token")
    }

    /// An unpinned `$SKEIN_GITHUB_API` is refused, not answered with the real one (SKEIN-764).
    ///
    /// The cost of the default is not paid by the test that takes it. api.github.com rate-limits an
    /// unauthenticated request per IP, so a stray call spends a budget the whole box shares, and the
    /// suite that runs out of it fails somewhere else entirely. It has already happened here:
    /// `review::visit::tests` downloaded a real diff, and the same test against a neighbour's
    /// leftover stub read a whole diff and spent a model call — same assertions, two different
    /// halves of the function (SKEIN-693, `src/review/visit.rs`).
    ///
    /// **Both directions in one test.** A pinned base has to be answered and an unpinned one
    /// refused: a check that only ever exercised the pinned path would still pass with the
    /// `assert!` deleted, which is the whole failure mode — the same argument
    /// `tests/harness.rs::an_unpinned_fleet_root_is_refused_rather_than_answered` makes.
    ///
    /// **What makes it fail:** deleting the `assert!` from [`api_base`]. The `catch_unwind` below
    /// then comes back `Ok("https://api.github.com")` and the `panic!` in the `Ok` arm fires.
    #[test]
    fn an_unpinned_github_api_is_refused_rather_than_answered() {
        let _g = crate::testutil::env_lock();
        let was = std::env::var_os("SKEIN_GITHUB_API");

        // Pinned: answered, and answered with what it was given — trailing slash trimmed, which is
        // the one transformation this function makes and therefore the one worth pinning down.
        std::env::set_var("SKEIN_GITHUB_API", "http://127.0.0.1:1/");
        assert_eq!(
            api_base(),
            "http://127.0.0.1:1",
            "a pinned base was not the one handed back, so the refusal below would be the only \
             behaviour this function had left"
        );

        // Unpinned: refused.
        std::env::remove_var("SKEIN_GITHUB_API");
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let answered = std::panic::catch_unwind(api_base);
        std::panic::set_hook(hook);
        if let Some(v) = was {
            std::env::set_var("SKEIN_GITHUB_API", v);
        }

        let said = match answered {
            Ok(base) => {
                panic!("an unpinned test was answered with {base} instead of being refused")
            }
            Err(e) => e
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "<not a string>".into()),
        };
        assert!(
            said.contains("SKEIN_GITHUB_API"),
            "the refusal has to name the variable to set, or it tells a contributor nothing: {said}"
        );
    }

    /// **A request that misses its deadline ends what `curl` started, not only `curl`.**
    ///
    /// The weakest of the four sites SKEIN-916 names, and deliberately tested anyway: curl does not
    /// normally fork for a request, so the old `child.kill()` was adequate *because of what curl
    /// happens to be* rather than because of anything this loop does. What is asserted here is the
    /// loop's own property — the deadline ends the process TREE — which stays true whatever the
    /// `curl` on a box turns out to be. The stand-in is a `curl` that forks, which is the case the
    /// old code could not have survived.
    ///
    /// **A `$PATH` stand-in, because the program name is not a seam.** `Command::new("curl")`
    /// resolves through the environment, so this is the only way in; `crate::sbx::tests` shortens
    /// `$PATH` the same way, and `env_lock` is what keeps it from being anybody else's problem.
    ///
    /// **Both halves, in that order**: the grandchild is asserted RUNNING while the request is
    /// still inside its deadline, and gone after it — an absence that was never a presence proves
    /// nothing (SKEIN-833).
    ///
    /// **What makes it fail:** putting back the two lines this replaced — `.process_group(0)` off
    /// the spawn and `let _ = child.kill(); let _ = child.wait();` in place of `end_group`. The
    /// stand-in's backgrounded `sleep` then outlives the deadline and `gone` fires naming it.
    #[test]
    fn a_request_that_misses_its_deadline_takes_its_grandchildren_with_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let stand_in = dir.join("bin");
        std::fs::create_dir_all(&stand_in).expect("a directory for the stand-in");

        let escapee = crate::place::grouptest::escapee(dir, "github-curl");
        let curl = stand_in.join("curl");
        std::fs::write(&curl, format!("#!/bin/sh\n{}\n", escapee.script()))
            .expect("writing the stand-in");
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755))
            .expect("making the stand-in runnable");
        // Pinned rather than set: `EnvPins` puts both back from `Drop`, so a failing assertion
        // below does not leave a `curl` that sleeps for ten minutes on the next test's `$PATH`.
        let mut pins = crate::testutil::env_pins();
        pins.set(
            "PATH",
            format!(
                "{}:{}",
                stand_in.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        // Nothing listens there, and nothing is meant to: the stand-in above never reaches the
        // network. Pinned because an unpinned base is refused in a test process (SKEIN-693).
        pins.set("SKEIN_GITHUB_API", "http://127.0.0.1:1");

        // `/rate_limit` is the one path a rate hold does not gate, so a hold left by a neighbouring
        // test cannot turn this into a refusal that never spawns anything.
        let asking = std::thread::spawn(|| {
            get_json_within("/rate_limit", &fixture_token(), Duration::from_secs(4))
        });

        let pid = escapee.there();
        let outcome = asking.join().expect("the requesting thread panicked");
        let said = outcome.expect_err("a `curl` that sleeps for 600s came back inside 4s");
        assert!(
            said.contains("did not answer") || said.contains("still answering"),
            "the deadline is not what ended this, so what follows is not about the deadline: {said}"
        );
        escapee.gone(pid);
    }

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

    /// A GitHub that KILLS its first `deaths` connections, the way the edge did on the owner's
    /// fleet: the answer starts, promises a length, and the socket closes before it arrives. curl
    /// exits non-zero with **no status and no body** — the shape every ladder in this module used
    /// to fall straight through, because all of them take an answer apart. Counts what it was
    /// asked, so a test can prove a second request was or was not made.
    fn dying_github(deaths: usize, then: &'static str) -> (String, std::sync::Arc<AtomicU64>) {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::sync::Arc::new(AtomicU64::new(0));
        let counting = asked.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 65536];
                let _ = stream.read(&mut buf);
                let nth = counting.fetch_add(1, Ordering::SeqCst) as usize;
                if nth < deaths {
                    // A length promised and not delivered, then the socket goes. curl has nothing
                    // to hand back: the status line never reached its `-w`.
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\nhalf an ans");
                    let _ = stream.flush();
                    continue;
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    then.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(then.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), asked)
    }

    /// What [`transport_failure`] writes and what [`connection_died`] reads are one decision, kept
    /// in one module — and the kind is read from curl's EXIT STATUS, because a cancelled stream
    /// carries no status and no body to read it from (SKEIN-271).
    #[test]
    fn a_dead_connection_is_a_kind_of_its_own_and_not_an_answer() {
        // The sentence the live failure produces, in the words this module gives it.
        let died = transport_failure(
            Some(92),
            "curl: (92) HTTP/2 stream 1 was not closed cleanly: CANCEL (err 8)",
        );
        assert!(connection_died(&died), "the kind is unreadable: {died}");
        assert!(
            died.contains("connection to GitHub died") && died.contains("curl exit 92"),
            "a reader must be told the connection died, and which failure it was: {died}"
        );
        assert!(
            !died.contains("curl: (92)"),
            "the exit code is named once, not twice: {died}"
        );
        // curl with nothing to say still has an exit code, and the code is the durable handle.
        assert!(connection_died(&transport_failure(Some(7), "")));

        // …and everything that IS GitHub answering, or skein's own choice, is not this. A reader
        // told "the connection died" goes and asks again; told "GitHub refused" they go and look
        // at the request. Confusing the two sends them to the wrong place.
        assert!(!connection_died(&complaint(
            502,
            "<html>502 Bad Gateway</html>"
        )));
        assert!(!connection_died(&complaint(200, "")));
        assert!(!connection_died(&complaint(
            404,
            r#"{"message":"Not Found"}"#
        )));
        assert!(!connection_died(&rate_limit_sentence(
            "API rate limit exceeded"
        )));
        // Skein's own deadline: asking again buys a second wait of the same length, so it is the
        // caller's decision and not this predicate's.
        assert!(!connection_died("GitHub did not answer within 30s"));
        assert!(!connection_died(
            "GitHub was still answering after 30s — 700KB had arrived when skein stopped waiting"
        ));
        // No number of attempts installs curl.
        assert!(!connection_died(
            "curl is not installed, and skein reads GitHub with it"
        ));

        // The two kinds must not overlap. `edge_refused` means "GitHub would not take it all at
        // once", and the batched search answers it by splitting the request in half — which is the
        // wrong answer here: measured, the five-search batch body is 1,416 bytes, so nothing about
        // it is too big to take, and halving would spend four requests to meet the same network.
        assert!(!edge_refused(&died));
    }

    /// What `complaint` writes and what `edge_refused` reads are one decision, kept in one module.
    #[test]
    fn a_refusal_to_take_the_request_is_told_apart_from_a_refusal_to_answer_it() {
        // The two shapes the edge produces, in the words this module gives them.
        assert!(edge_refused(&complaint(200, "")));
        assert!(edge_refused(&complaint(
            502,
            "<html><head><title>502 Bad Gateway</title></head></html>"
        )));
        assert!(edge_refused(&complaint(503, "<html>unavailable</html>")));

        // …and everything that is GitHub actually answering. Splitting any of these would spend
        // more requests to be told the same thing several times.
        assert!(!edge_refused(&complaint(
            500,
            r#"{"message":"Server Error"}"#
        )));
        assert!(!edge_refused(&complaint(404, r#"{"message":"Not Found"}"#)));
        assert!(!edge_refused(&rate_limit_sentence(
            "API rate limit exceeded"
        )));
        assert!(!edge_refused("the `author:me` query failed"));
    }

    /// A GitHub whose FIRST answer is the edge shrugging and whose second is real — the shape a
    /// cold first load meets, and the reason the retry exists.
    ///
    /// The first status and body are the caller's, because [`edge_shrug`] knows two shapes and a
    /// test about the retry has to be able to drive both: a 200 with no bytes at all, and a 5xx
    /// carrying the edge's own HTML. Counts what it was asked, so a test can prove a second request
    /// was — or was not — made.
    ///
    /// The `Content-Type` is the same on both answers and carries no meaning: [`call`] takes the
    /// status from curl's `-w %{http_code}` and the body as bytes, and reads no header at all.
    fn shrug_then_real_github(
        status: u16,
        first: &'static str,
        second: &'static str,
    ) -> (String, std::sync::Arc<AtomicU64>) {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::sync::Arc::new(AtomicU64::new(0));
        let counting = asked.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let (status, body) = match counting.fetch_add(1, Ordering::SeqCst) {
                    0 => (status, first),
                    _ => (200, second),
                };
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), asked)
    }

    /// An empty 200 is asked again once, and a second one is reported (SKEIN-258).
    ///
    /// GitHub answers a request it gave up on with a 200 and no bytes — and since the searches were
    /// batched, the heaviest request skein sends is the one that provokes it, so a cold first load
    /// printed five identical alarms for one non-answer and was fine on the next refresh.
    ///
    /// Through [`graphql_partial`], because that is where the retry lives (SKEIN-341): the batched
    /// search is the caller this was reported from, and it is the read half. This test used to ask
    /// through [`graphql`], which was the visible half of the retry sitting in the wire the two
    /// share — a query proving a retry that a mutation was getting as well.
    #[test]
    fn an_empty_answer_is_asked_again_once_and_a_second_one_is_told() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear;
        let (api, asked) = shrug_then_real_github(200, "", r#"{"data":{"q0":{"nodes":[]}}}"#);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let answered = graphql_partial("query { x }", serde_json::json!({}), &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let (data, _) =
            answered.expect("an empty first answer was reported instead of being asked again");
        assert!(
            data.get("q0").is_some(),
            "the retry did not carry the real answer back: {data}"
        );
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the retry is one more request, not a loop"
        );

        // Twice empty is a condition rather than a flap, and the caller must see it: a silent
        // second retry would turn "GitHub is not answering" into a queue that is quietly short.
        let always_empty = one_shot_github(200, Vec::new(), false);
        std::env::set_var("SKEIN_GITHUB_API", &always_empty);
        let refused = graphql_partial("query { x }", serde_json::json!({}), &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let why = refused.expect_err("an answer that is never there was reported as success");
        assert!(
            why.contains("empty body"),
            "the reason stopped naming what happened: {why}"
        );
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
        let got = get_json_within("/big", &fixture_token(), Duration::from_secs(10));
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
        let err = get_json_within("/slow", &fixture_token(), Duration::from_secs(2));
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

    /// **A request body reaches curl through a file, never through argv** — asked of the host's
    /// real process table, while a real request is in flight.
    ///
    /// This is the property that decided which of the two curl clients in this crate survived.
    /// `gitgate` had one of its own that spent a body as `-d <body>`, and a command line is
    /// readable by every process on the host — the same hazard the `--config -` document beside it
    /// existed to close for the credential, left open for everything else. The other member of that
    /// class was the fleet-wide seeding, which handed `sbx secret set -g` a token on argv; it is
    /// deleted (architecture §13a), so this is the last client that has to keep the property.
    ///
    /// **The control is the half that makes the absence mean something.** An assertion that a
    /// string is missing from `ps` passes just as well when `ps` is broken, when the marker never
    /// travelled, or when curl had already exited — an absence that was never a presence proves
    /// nothing. So the same marker is first put on a command line deliberately and *found*, with
    /// the same scan, in the same window.
    ///
    /// The concrete change that breaks it: pushing `-d` and the body into `args` in [`call`]
    /// instead of writing the temp file and passing `--data-binary @path`.
    #[test]
    fn a_request_body_never_reaches_the_process_table() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        // Dribbling, so curl is still running — and therefore still in `ps` — while it is scanned.
        let api = one_shot_github(200, vec![b'x'; 200_000], true);
        let marker = format!("skein-argv-probe-{}", std::process::id());

        // Does a body on a command line show up at all, here, now? If this half fails the other
        // half is worthless, so it is asserted rather than assumed.
        let mut control = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "6",
                "-d",
                &format!("{{\"probe\":\"{marker}\"}}"),
                &format!("{api}/control"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("curl is what this module talks to GitHub with");
        let control_saw = process_table_shows(&marker);
        let _ = control.kill();
        let _ = control.wait();
        assert!(
            control_saw,
            "the scan cannot see a body that IS on a command line, so its silence about the real \
             request would prove nothing"
        );

        // The real path, with the same marker, under the same scan.
        let body = serde_json::json!({ "probe": marker }).to_string();
        let (url, sending) = (format!("{api}/real"), marker.clone());
        let call = std::thread::spawn(move || {
            call(
                "POST",
                &url,
                &fixture_token(),
                Some(&body),
                "application/vnd.github+json",
                Duration::from_secs(5),
            )
        });
        let leaked = process_table_shows(&sending);
        let _ = call.join();
        assert!(
            !leaked,
            "the request body was on a command line — every process on this host could read it"
        );
    }

    /// Is `needle` anywhere in the host's process table right now? Polled, because "in flight" is
    /// a window and the scan has to land inside it: it answers as soon as it sees the needle, and
    /// gives up after two seconds of not seeing it.
    fn process_table_shows(needle: &str) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(out) = Command::new("ps").args(["-ewwo", "args="]).output() {
                if String::from_utf8_lossy(&out.stdout).contains(needle) {
                    return true;
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
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
        let err = get_json("/user", &fixture_token());
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
        let first = get_json("/user", &fixture_token());
        let second = get_json("/user", &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let first = first.expect_err("a spent quota is an error");
        assert!(
            first.contains("rate limiting skein"),
            "the first call reports the limit: {first}"
        );
        assert_eq!(
            held().map(|h| (h.until, h.because)),
            Some((reset, Because::QuotaSpent)),
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
        let got = get_json("/user", &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let got = got.expect("an elapsed hold must not block");
        assert_eq!(got.get("fine").and_then(|v| v.as_bool()), Some(true));
        assert!(
            held().is_none(),
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
        let quota = get_json("/rate_limit", &fixture_token());
        let other = get_json("/user", &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        quota.expect("/rate_limit must pass through a hold");
        let other = other.expect_err("everything else must not");
        assert!(
            other.contains("not calling GitHub"),
            "the refusal says what is happening: {other}"
        );
    }

    /// A GitHub whose quotas are all FULL — the shape a secondary rate limit takes, and the shape
    /// measured on 2026-08-27 while skein was refusing to call anything for fifteen minutes.
    ///
    /// `/rate_limit` reports core, search and graphql at 5000 of 5000, because a secondary limit is
    /// about burst and concurrency and spends no primary quota at all. Everything else answers
    /// GitHub's own secondary-limit 403 when `refusing`, and an ordinary 200 when not — the same
    /// stub serves "GitHub is still saying no" and "GitHub is answering again", which is the pair
    /// the hold has to tell apart. Counts per path, so a test can prove a call never arrived.
    fn unspent_github(
        refusing: bool,
    ) -> (String, std::sync::Arc<AtomicU64>, std::sync::Arc<AtomicU64>) {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let other = std::sync::Arc::new(AtomicU64::new(0));
        let quota = std::sync::Arc::new(AtomicU64::new(0));
        let (other_count, quota_count) = (other.clone(), quota.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let (status, body) = if request.starts_with("GET /rate_limit") {
                    quota_count.fetch_add(1, Ordering::SeqCst);
                    // No `reset` worth reading here on purpose: nothing is spent, so nothing is
                    // resetting, and a hold that quotes one of these numbers is quoting a lie.
                    (
                        200u16,
                        r#"{"resources":{"core":{"remaining":5000,"limit":5000,"reset":0},"search":{"remaining":30,"limit":30,"reset":0},"graphql":{"remaining":5000,"limit":5000,"reset":0}}}"#
                            .to_string(),
                    )
                } else {
                    other_count.fetch_add(1, Ordering::SeqCst);
                    match refusing {
                        true => (
                            403u16,
                            r#"{"message":"You have exceeded a secondary rate limit. Please wait a few minutes before you try again."}"#
                                .to_string(),
                        ),
                        false => (200u16, r#"{"fine":true}"#.to_string()),
                    }
                };
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), other, quota)
    }

    /// A refusal that spent no quota is a short pause, not a quarter-hour blackout.
    ///
    /// The defect this exists for, measured on 2026-08-27: GitHub's SECONDARY limits are answered
    /// as a 403 saying "rate limit", so they engage the hold, but they cost no primary quota — and
    /// the old code asked `/rate_limit` for a reset, was told nothing was spent, and read that as
    /// "unusable answer", i.e. fifteen minutes. `GET /rate_limit` said `core 5000/5000` throughout
    /// both windows and a hand-run `curl` on the same token answered immediately, while every row
    /// in the cockpit sat there without a summary.
    ///
    /// The hold still engages — the second call must not reach the wire, which is SKEIN-208's whole
    /// point — but it lasts [`BLIND_HOLD`], and it does not claim to know when GitHub will relent.
    #[test]
    fn a_refusal_that_spent_no_quota_pauses_briefly_instead_of_blacking_out() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let before = epoch_now();
        let (api, other, quota) = unspent_github(true);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let first = get_json("/user", &fixture_token());
        let second = get_json("/user", &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");

        let first = first.expect_err("a secondary rate limit is an error");
        assert!(
            first.contains("rate limiting skein"),
            "the first call reports the limit: {first}"
        );
        let hold = held().expect("a secondary limit still engages the hold — SKEIN-208 stands");
        assert_eq!(
            hold.because,
            Because::NoQuotaSpent,
            "a refusal with every quota full is not a spent quota"
        );
        assert!(
            hold.until <= before + BLIND_HOLD + 5 && hold.until > before,
            "the pause is about {BLIND_HOLD}s, not the old flat 15m: {} seconds",
            hold.until.saturating_sub(before)
        );

        let second = second.expect_err("a held call is an error");
        assert!(
            second.contains("not calling GitHub") && second.contains("slow down"),
            "the refusal says GitHub asked skein to slow down: {second}"
        );
        assert!(
            !second.contains("resuming in about"),
            "a burst limit has no reset, so the refusal must not quote one: {second}"
        );
        assert_eq!(
            other.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the second call must never reach the server"
        );
        assert_eq!(
            quota.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "engaging asks /rate_limit exactly once"
        );
    }

    /// A spent quota and a slow-down do not read the same, and only one of them names a time.
    ///
    /// The other half of the same defect: both refusals used to be the single sentence "resuming in
    /// about 15m", so the one number a reader could plan around and the one skein had invented
    /// were typographically identical. A `reset` GitHub gave is worth quoting; a back-off skein
    /// chose is not the same claim and must not wear the same words.
    #[test]
    fn only_a_refusal_with_a_real_reset_tells_the_reader_when_github_comes_back() {
        let now = 1_000_000u64;
        let spent = refusal(
            &Hold {
                until: now + 600,
                because: Because::QuotaSpent,
                checked: now,
            },
            now,
        );
        let slow = refusal(
            &Hold {
                until: now + BLIND_HOLD,
                because: Because::NoQuotaSpent,
                checked: now,
            },
            now,
        );
        assert!(
            spent.contains("resuming in about 10m"),
            "a quota with a reset behind it names the wait: {spent}"
        );
        assert!(
            !slow.contains("resuming in about") && slow.contains("slow down"),
            "a burst limit has no reset to name: {slow}"
        );
        assert_ne!(spent, slow, "the two refusals must not read the same");
        // The fact common to both, and the one `crate::prq`'s tests read a refusal by.
        for said in [&spent, &slow] {
            assert!(
                said.contains("not calling GitHub"),
                "every refusal says what is happening: {said}"
            );
        }
    }

    /// A hold ends the moment `/rate_limit` shows the quota back, rather than waiting out a guess.
    ///
    /// The second half of the defect: [`call`] only ever compared the clock against `until`, so
    /// nothing could end a hold early — not a quota visibly back at 5000/5000, not a person
    /// pressing refresh. Asking is free and exempt, so the answer that would settle it was on hand
    /// the whole time and never asked for.
    #[test]
    fn a_hold_ends_early_when_github_reports_every_quota_unspent() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        set_stale_hold(epoch_now() + 600, Because::QuotaSpent);
        let (api, other, quota) = unspent_github(false);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let got = get_json("/user", &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");

        let got = got.expect("a hold whose quotas have reset must not refuse");
        assert_eq!(got.get("fine").and_then(|v| v.as_bool()), Some(true));
        assert!(
            held().is_none(),
            "a hold GitHub no longer justifies is lifted, not merely stepped over"
        );
        assert_eq!(
            quota.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the hold is re-measured against /rate_limit before it refuses"
        );
        assert_eq!(
            other.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the call goes through once the hold is lifted"
        );
    }

    /// Re-measuring a hold that is STILL out of quota keeps refusing, at GitHub's current reset.
    ///
    /// The re-check is not a way out: it is a question, and "still spent" is one of its answers.
    /// The hold takes the reset `/rate_limit` reports now rather than the one it was engaged with,
    /// because a second window can open while the first is being waited out.
    #[test]
    fn a_re_measured_hold_that_is_still_spent_refuses_at_githubs_current_reset() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let reset = epoch_now() + 900;
        // Ending sooner than GitHub's reset, so a hold that merely ran its timer out would let the
        // call through and this test would see the request arrive.
        set_stale_hold(epoch_now() + 30, Because::QuotaSpent);
        let (api, spent, quota) = spent_github(reset);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let refused = get_json("/user", &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");

        let refused = refused.expect_err("a quota that is still spent must still refuse");
        assert!(
            refused.contains("resuming in about 15m"),
            "the refusal names GitHub's own wait: {refused}"
        );
        assert_eq!(
            held().map(|h| (h.until, h.because)),
            Some((reset, Because::QuotaSpent)),
            "the hold moves to the reset GitHub reports now"
        );
        assert_eq!(
            quota.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "re-measuring asks /rate_limit exactly once"
        );
        assert_eq!(
            spent.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a re-measured hold still refuses before the wire"
        );
    }

    /// A hold is re-measured at most once a minute, however many calls meet it.
    ///
    /// `/rate_limit` is free of QUOTA, not free of REQUESTS, and a refresh cycle meets the hold ~45
    /// times (SKEIN-208). Forty-five probes in a moment is the burst shape that provokes a
    /// secondary limit — the hold would become a way of causing the thing it exists to wait out.
    #[test]
    fn a_hold_is_re_measured_at_most_once_a_minute_however_many_calls_meet_it() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        set_rate_hold(Some(epoch_now() + 600));
        let (api, spent, quota) = spent_github(epoch_now() + 600);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let refusals: Vec<_> = (0..5)
            .map(|_| get_json("/user", &fixture_token()))
            .collect();
        std::env::remove_var("SKEIN_GITHUB_API");

        for refused in &refusals {
            assert!(
                refused
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.contains("not calling GitHub")),
                "every call under a hold is refused: {refused:?}"
            );
        }
        assert_eq!(
            quota.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a hold measured this second is not measured again five more times"
        );
        assert_eq!(
            spent.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "and nothing reached the wire either"
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
        let first = graphql("query { x }", serde_json::json!({}), &fixture_token());
        let second = graphql("query { x }", serde_json::json!({}), &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let first = first.expect_err("a spent GraphQL quota is an error");
        assert!(
            first.contains("rate limiting skein") && first.contains("stops calling"),
            "the 200 is reported as the rate limit it is: {first}"
        );
        assert_eq!(
            held().map(|h| (h.until, h.because)),
            Some((reset, Because::QuotaSpent)),
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

    /// The read half of SKEIN-271: a query whose connection dies is asked again, and the second
    /// answer is the one the caller gets. Idempotent, so a retry costs one request and nothing
    /// else — the same bargain the empty-200 retry above already makes.
    #[test]
    fn a_query_whose_connection_dies_is_asked_again_and_the_second_answer_is_kept() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let (api, asked) = dying_github(1, r#"{"data":{"q0":{"nodes":[]}}}"#);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let answered = graphql_partial("query { x }", serde_json::json!({}), &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let (data, _) = answered.expect("a dead connection must be asked again, not reported");
        assert!(
            data.get("q0").is_some(),
            "the retry did not carry the real answer back: {data}"
        );
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the retry is one more request, not a loop"
        );
    }

    /// Twice dead is a condition, and the caller must be told what kind. This is the sentence that
    /// used to read "GitHub did not answer for <slug>" — true, and pointing at the wrong thing.
    #[test]
    fn a_connection_that_keeps_dying_is_reported_as_the_connection_and_not_as_github() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let (api, asked) = dying_github(usize::MAX, "");
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let answered = graphql_partial("query { x }", serde_json::json!({}), &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let why = answered.expect_err("a connection that never survives is an error");
        assert!(
            connection_died(&why) && why.contains("connection to GitHub died"),
            "the reason stopped naming what happened: {why}"
        );
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one retry, then the caller is told"
        );
    }

    /// **The read's retry must not reach a write** — the dead-connection half. [`graphql`] carries
    /// `updatePullRequestBranch`, which rebases somebody's branch, and a dead stream is ambiguous:
    /// the mutation may have run before the answer was lost. This is why [`ask_twice`] is applied
    /// in [`graphql_partial`] and not in the [`graphql_answer`] both share. The edge-shrug half of
    /// the same invariant is the test below it, and for a long time only this half was true.
    #[test]
    fn a_mutation_whose_connection_dies_is_never_sent_again() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let (api, asked) = dying_github(1, r#"{"data":{"ok":true}}"#);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let answered = graphql(
            "mutation { rebase }",
            serde_json::json!({}),
            &fixture_token(),
        );
        std::env::remove_var("SKEIN_GITHUB_API");
        let why = answered.expect_err("a dead connection on a mutation is an error, not a retry");
        assert!(
            connection_died(&why),
            "the caller cannot tell it was the connection: {why}"
        );
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a mutation reached GitHub twice — a rebase can happen twice"
        );
    }

    /// **The read's retry must not reach a write** — the edge-shrug half (SKEIN-341).
    ///
    /// [`edge_shrug`] knows two shapes, an empty 200 and a 5xx that is not JSON, and both are as
    /// ambiguous as a dead connection: the edge is speaking, so nothing in the answer says whether
    /// the backend carried the request out. That retry sat in [`graphql_answer`], which [`graphql`]
    /// shares, so a mutation met it — an edge 502 arriving after `updatePullRequestBranch` had
    /// already rebased the branch sent the rebase a second time, GitHub refused that one because
    /// `expectedHeadOid` no longer matched what it had just written, and `prwork` wrote a permanent
    /// stop saying the rebase failed on a branch that had in fact been rebased.
    ///
    /// The fixture answers the shrug first and something perfectly good second, so a retry cannot
    /// hide: were the mutation asked again it would come back `Ok`. Each shape is then driven down
    /// the read path as well, because the invariant is about which entry point asks — not about
    /// which shape came back — and a "fix" that simply deleted the retry would satisfy half of this
    /// test and fail the other half.
    #[test]
    fn a_mutation_is_never_sent_again_when_the_edge_shrugs() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let shapes = [
            (200u16, "", "an empty 200"),
            (
                502u16,
                "<html><head><title>502 Bad Gateway</title></head></html>",
                "a 502 of the edge's own HTML",
            ),
        ];
        for (status, first, what) in shapes {
            assert!(
                edge_shrug(status, first),
                "{what} is not the shape this test is about"
            );

            let (api, asked) = shrug_then_real_github(status, first, r#"{"data":{"ok":true}}"#);
            std::env::set_var("SKEIN_GITHUB_API", &api);
            let answered = graphql(
                "mutation { rebase }",
                serde_json::json!({}),
                &fixture_token(),
            );
            std::env::remove_var("SKEIN_GITHUB_API");
            assert_eq!(
                asked.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "a mutation reached GitHub twice after {what} — a rebase can happen twice"
            );
            let why = match answered {
                Err(why) => why,
                Ok(data) => {
                    panic!("{what} on a mutation was asked again and called success: {data}")
                }
            };
            assert!(
                why.contains("GitHub answered"),
                "the caller is not told what came back after {what}: {why}"
            );

            // The same non-answer on the READ path is asked again, and the second answer is kept.
            let (api, asked) =
                shrug_then_real_github(status, first, r#"{"data":{"q0":{"nodes":[]}}}"#);
            std::env::set_var("SKEIN_GITHUB_API", &api);
            let read = graphql_partial("query { x }", serde_json::json!({}), &fixture_token());
            std::env::remove_var("SKEIN_GITHUB_API");
            let (data, _) =
                read.unwrap_or_else(|why| panic!("a read must be asked again after {what}: {why}"));
            assert!(
                data.get("q0").is_some(),
                "the retry did not carry the real answer back after {what}: {data}"
            );
            assert_eq!(
                asked.load(std::sync::atomic::Ordering::SeqCst),
                2,
                "the read stopped asking again after {what}"
            );
        }
    }

    /// **A 204 is GitHub saying it did the thing** (SKEIN-346).
    ///
    /// `DELETE /repos/{slug}/git/refs/heads/{ref}` is answered `204 No Content` with zero bytes.
    /// [`json`] parsed the body before it read the status, so `from_str("")` failed and every
    /// successful delete came back as "GitHub answered 204 with an empty body" — which `prwork`'s
    /// train, deleting on the step after it merges, printed as "merged #12, but topic is still
    /// there: …" about a branch that was already gone. The `200..=299` arm could not be reached by
    /// any 204 ever sent.
    ///
    /// The second half is the boundary the fix must not blur: an empty **200** is the edge
    /// shrugging with nothing behind it, and it stays an error, because [`edge_refused`] reads that
    /// exact sentence and a request GitHub never answered must not become an answer.
    #[test]
    fn a_204_with_no_body_is_the_call_having_worked_and_an_empty_200_is_not() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let api = one_shot_github(204, Vec::new(), false);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let deleted = send_json(
            "DELETE",
            "/repos/acme/thing/git/refs/heads/topic",
            &fixture_token(),
            &serde_json::json!({}),
        );
        std::env::remove_var("SKEIN_GITHUB_API");
        let value =
            deleted.unwrap_or_else(|why| panic!("a 204 is the delete having worked: {why}"));
        assert!(
            value.is_null(),
            "a body that is not there reads as JSON's own no-value, not as an object that had \
             nothing in it: {value}"
        );

        let api = one_shot_github(200, Vec::new(), false);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let nothing = get_json("/user", &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let why = nothing.expect_err("an empty 200 is a non-answer, not an answer of nothing");
        assert!(
            why.contains("empty body") && edge_refused(&why),
            "the empty-200 sentence stopped being the one the edge check reads: {why}"
        );
    }

    /// A GET is idempotent too, and the same retry covers it: the live 502-with-HTML that took a
    /// queue refresh down arrived on this path as well.
    #[test]
    fn a_get_whose_connection_dies_is_asked_again() {
        let _g = crate::testutil::env_lock();
        let _hold = HoldClear::new();
        let (api, asked) = dying_github(1, r#"{"login":"someone"}"#);
        std::env::set_var("SKEIN_GITHUB_API", &api);
        let got = get_json("/user", &fixture_token());
        std::env::remove_var("SKEIN_GITHUB_API");
        let got = got.expect("a dead connection on a GET must be asked again");
        assert_eq!(got.get("login").and_then(|v| v.as_str()), Some("someone"));
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// **A path planted where a GitHub call keeps its request body is refused, not written
    /// through, and debris at the name costs the call nothing** (SKEIN-1022).
    ///
    /// Planted at the exact names the next call will try, in the real temp directory, because
    /// that is what the attacker in [`RequestScratch`]'s doc does: a symlink at the first name to a
    /// directory this test owns, and at the second a real directory holding a stale `request`,
    /// the way a SIGKILLed run leaves one. The call must step past both into the third name.
    ///
    /// Counterfactuals, each planted and watched: `DirBuilder::create` swapped for
    /// `create_dir_all`, which accepts a directory that is already there and so follows the
    /// symlink, fails `nothing is written through the planted symlink`; `AlreadyExists` returning
    /// its error rather than stepping to the next name fails `a taken name is stepped past`; a
    /// `remove_dir_all` of a taken name before stepping past it fails `somebody else's directory
    /// is left as it was`; and a `RequestScratch` that is never removed fails `nothing of the call
    /// is left behind`. A stub on loopback answers, so nothing here reaches GitHub.
    #[test]
    fn a_path_planted_at_the_request_scratch_name_is_stepped_past_not_written_through() {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::Ordering;
        let _g = crate::testutil::env_lock();

        // A GitHub that records what it was sent and answers with a header only `-D` can see.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
        let recording = received.clone();
        std::thread::spawn(move || {
            if let Some(mut stream) = listener.incoming().flatten().next() {
                let mut buf = [0u8; 4096];
                let mut got = Vec::new();
                // Until the body has arrived: it is the one thing this test sends.
                while !String::from_utf8_lossy(&got).contains("\"example\"") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => got.extend_from_slice(&buf[..n]),
                    }
                }
                *recording.lock().unwrap() = got;
                let body = b"{\"ok\":true}";
                let head = format!(
                    "HTTP/1.1 200 X\r\nX-Skein-Example: thing\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            }
        });

        // Whatever is planted goes afterwards, however this test ends.
        struct Planted(Vec<std::path::PathBuf>);
        impl Drop for Planted {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = std::fs::remove_file(path);
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }
        let base = std::env::temp_dir();
        let name = |n: u64| base.join(format!("skein-gh-{n}-{}", std::process::id()));
        let first = REQUESTS.load(Ordering::SeqCst);
        let (symlink, debris, used) = (name(first), name(first + 1), name(first + 2));
        let _planted = Planted(vec![symlink.clone(), debris.clone()]);

        let victim = crate::testutil::tempdir();
        let victim: &std::path::Path = victim.as_ref();
        std::os::unix::fs::symlink(victim, &symlink).expect("planting the symlink");
        std::fs::create_dir(&debris).expect("planting the debris");
        std::fs::write(debris.join("request"), "a SIGKILLed run's body").unwrap();
        // Presence before absence: the symlink leads where the attack needs it to.
        assert!(
            std::fs::metadata(&symlink).is_ok_and(|m| m.is_dir()),
            "the planted symlink does not resolve to a directory, so nothing below tests anything"
        );

        let answered = call_reading_headers(
            "POST",
            // `/rate_limit` is the one path a rate hold does not gate, so a hold left by a
            // neighbouring test cannot turn this into a refusal that never makes a directory.
            &format!("http://127.0.0.1:{port}/rate_limit"),
            &fixture_token(),
            Some("{\"thing\":\"example\"}"),
            "application/json",
            Duration::from_secs(10),
            true,
        );

        let written: Vec<_> = std::fs::read_dir(victim).unwrap().flatten().collect();
        assert!(
            written.is_empty(),
            "nothing is written through the planted symlink, yet {:?} appeared in its target",
            written.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );
        assert_eq!(
            REQUESTS.load(Ordering::SeqCst),
            first + 3,
            "a taken name is stepped past: two names were taken, so the third is the one used \
             (another value here also means something else took a name meanwhile)"
        );
        let (status, body, headers) =
            answered.expect("a taken name is stepped past, not reported as a failed call");
        assert_eq!((status, body.as_str()), (200, "{\"ok\":true}"));
        assert!(
            headers.contains("X-Skein-Example: thing"),
            "the header dump did not come back: {headers:?}"
        );
        let sent = String::from_utf8_lossy(&received.lock().unwrap()).into_owned();
        assert!(
            sent.contains("{\"thing\":\"example\"}"),
            "the request body did not reach the stub: {sent}"
        );
        assert_eq!(
            std::fs::read_to_string(debris.join("request"))
                .ok()
                .as_deref(),
            Some("a SIGKILLed run's body"),
            "somebody else's directory is left as it was"
        );
        assert!(
            std::fs::symlink_metadata(&symlink).is_ok_and(|m| m.file_type().is_symlink()),
            "somebody else's directory is left as it was — the symlink is gone"
        );
        assert!(
            !used.exists(),
            "nothing of the call is left behind, yet {} is still there",
            used.display()
        );
    }
}
