//! What stands between a request and the cockpit: the token gate every route sits behind and
//! the short list of paths open to all, the security headers on every answer, the origin check
//! a WebSocket must pass, and the pages and vendored assets anyone may load.

use super::*;

/// Paths served without the fleet's token.
///
/// Deliberately short, and deliberately a list of *escapes* rather than a list of what is guarded:
/// a new route is protected the moment it is added, and opening one up has to be written down here
/// where it can be read and argued with.
///
/// Each of these is a static asset compiled into this binary — the same bytes for every fleet, no
/// state read, nothing mutated. `/` is here so an unauthenticated visitor gets a page that can
/// explain itself instead of a bare 401, and so `?t=` has somewhere to land.
fn open_to_all(path: &str) -> bool {
    // `/v2` for the same reason as `/`: it is the same bytes for every fleet, it reads no state, and
    // it is where `?t=` lands. An unauthenticated visitor gets a page that can explain itself.
    path == "/" || path == "/v2" || path.starts_with("/vendor/")
}

/// The whole cockpit, when the auth-off switch is set where it is refused (SKEIN-962).
///
/// A fallback and nothing else: every path, every method, the same 503 and the same sentence —
/// including `/` and `/vendor/`, which [`open_to_all`] would otherwise serve. A page that loads and
/// then fails every call it makes is a worse answer than one that says what is wrong, and this is
/// read by a person in a browser at least as often as by `curl`. No [`gate`] layer, because there
/// is nothing here to authenticate: the refusal is the same for the owner and for a box, and it
/// names no secret. It names the variable that is set, which is a fact a box could as easily read
/// off its own environment. The security headers stay on: this is still an origin a browser renders.
pub(super) fn refusal_only() -> Router {
    Router::new()
        .fallback(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                skein::apiauth::off_switch_refusal(),
            )
        })
        .layer(axum::middleware::from_fn(security_headers))
}

/// What the cockpit sends on every answer. See the layer's own note for what the CSP does and does
/// not close today.
///
/// `img-src` admits `data:` because the page draws inline SVG icons that way, and `blob:` because
/// the terminal and the attachment previews create object URLs. `connect-src` admits `ws:`/`wss:`
/// for the terminal WebSocket, which is same-origin but a different scheme.
const CSP: &str = "default-src 'self'; \
                   script-src 'self' 'unsafe-inline'; \
                   style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data: blob:; \
                   font-src 'self' data:; \
                   connect-src 'self' ws: wss:; \
                   object-src 'none'; \
                   base-uri 'self'; \
                   form-action 'self'; \
                   frame-ancestors 'none'";

pub(super) async fn security_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        ("content-security-policy", CSP),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
    ] {
        // Set rather than appended, and only when the handler did not say otherwise — a route that
        // needs its own policy stays in charge of it.
        if !headers.contains_key(name) {
            if let (Ok(name), Ok(value)) = (
                axum::http::HeaderName::from_bytes(name.as_bytes()),
                axum::http::HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }
    }
    response
}

/// Refuse anything that does not carry the fleet's token.
///
/// This is the answer to a box reaching `host.docker.internal:7878` — see [`skein::apiauth`] for
/// what that allowed and why a secret rather than a peer-address rule.
pub(super) async fn gate(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if skein::apiauth::authorised(request.headers()) {
        // The one place a connection stops being a stranger. Deliberately keyed on the credential
        // and not on being served: `open_to_all` would otherwise promote anything that can spell
        // `GET /`, which is every flooder, and the doorstep would bound nothing.
        let _ = KNOCK.try_with(|knock| knock.prove());
        return next.run(request).await;
    }
    if open_to_all(request.uri().path()) {
        return next.run(request).await;
    }
    skein::apiauth::refusal().into_response()
}

/// The cockpit page, and the one place the fleet's API token becomes a browser session.
///
/// `?t=<token>` is exchanged for an `HttpOnly` cookie and then **redirected away**, so the secret
/// does not stay in the address bar, in history, or in the `Referer` of anything the page later
/// links to. Every `fetch` in the document and the terminal WebSocket then carry it automatically,
/// which is why authenticating the API changed no calling code.
///
/// The document itself is served to anyone who asks. It holds no secrets — it is the same HTML
/// compiled into this binary — and gating it would only mean an unauthenticated visitor got a blank
/// page instead of one that can say what is wrong.
pub(super) async fn index(Query(q): Query<HashMap<String, String>>) -> Response {
    page(&q, "/", INDEX)
}

/// The new board, beside the old one rather than instead of it.
///
/// `docs/delivery.md` names treating "ground-up surfaces" and "new topology" as one project as the
/// single biggest avoidable risk in the plan, and shipping beside is what keeps them separate: `/`
/// keeps working, unchanged, until `docs/parity.md` §7 has been walked against this page item by
/// item. A surface that is 90% ported and cut over is worse than one that is 60% ported and not.
pub(super) async fn board_v2(Query(q): Query<HashMap<String, String>>) -> Response {
    page(&q, "/v2", V2)
}

/// One page, one session exchange.
///
/// `?t=<token>` is exchanged for an `HttpOnly` cookie and redirected **back to the page that was
/// asked for**, which is the only part of this that is per-page: a `/v2` link that landed you on `/`
/// would look like the new board silently not existing.
fn page(q: &HashMap<String, String>, self_path: &str, body: &'static str) -> Response {
    let headers = [
        (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
        // The UI is embedded in and version-coupled to this binary. Reusing an older document after
        // a server restart mixes stale JS/CSS with new API behaviour, so the browser must revalidate.
        (axum::http::header::CACHE_CONTROL, "no-store"),
    ];
    let offered = q.get("t").map(String::as_str).unwrap_or_default();
    // `apiauth::same`, not `==`. This is the one place the fleet's token is compared with a plain
    // string equality, and it is the place that mints the browser session — every other comparison
    // already goes through the constant-time helper written for exactly this.
    if !offered.is_empty() && skein::apiauth::matches(offered) {
        {
            // `SameSite=Strict` is what closes cross-site POSTs to this API. `Path=/` covers the
            // WebSocket as well as `/api`. No `Secure`, because the ordinary case is plain http on
            // loopback and a Secure cookie would simply never be stored there.
            return (
                StatusCode::SEE_OTHER,
                [
                    (axum::http::header::LOCATION, self_path.to_string()),
                    (
                        axum::http::header::SET_COOKIE,
                        format!(
                            "{}={offered}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000",
                            skein::apiauth::COOKIE
                        ),
                    ),
                ],
                "",
            )
                .into_response();
        }
        // A wrong token gets the page and no cookie, rather than a hint that it was wrong.
    }
    (headers, body).into_response()
}

/// One asset by name, or a 404 that says nothing about why.
///
/// "No such asset" and "that path may not name one" are the same answer on purpose: a caller probing
/// for the difference learns nothing, and there is nothing a person can do with the distinction that
/// a 404 does not already tell them.
pub(super) async fn asset(name: &str) -> Response {
    match skein::assets::get(name) {
        Some(a) => (
            [
                (axum::http::header::CONTENT_TYPE, a.content_type),
                (axum::http::header::CACHE_CONTROL, a.cache),
            ],
            a.bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The route a built bundle is served by. The path is a name relative to the asset root and never a
/// path this process joins onto anything a caller chose — see `skein::assets`.
pub(super) async fn any_asset(Path(path): Path<String>) -> Response {
    asset(&path).await
}

/// Origin guard for the terminal upgrade. Browsers always send `Origin` on a WebSocket handshake
/// and page JS cannot forge it, so rejecting unexpected origins blocks drive-by cross-origin
/// connections (WS is exempt from same-origin policy) and DNS-rebinding against the terminal.
/// Allowed: loopback (local use); any `*.ts.net` host and any Tailscale IP (the tailnet is the auth
/// boundary — reached either via `tailscale serve`, whose origin is the `.ts.net` name, or by hitting
/// an off-loopback bind at the box's raw tailnet address); and any host in `$SKEIN_ALLOWED_ORIGINS`
/// (comma-separated) for other reverse proxies. Non-browser clients send no `Origin` and are allowed.
pub(super) fn origin_ok(headers: &axum::http::HeaderMap) -> bool {
    let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        return true;
    };
    let authority = origin.split("://").nth(1).unwrap_or("");
    let authority = authority.split('/').next().unwrap_or(authority);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest) // [::1]:port -> ::1
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    if matches!(host, "localhost" | "127.0.0.1" | "::1")
        || host.ends_with(".ts.net")
        || is_tailnet_ip(host)
    {
        return true;
    }
    std::env::var("SKEIN_ALLOWED_ORIGINS").is_ok_and(|list| {
        list.split(',')
            .map(str::trim)
            .any(|h| !h.is_empty() && h == host)
    })
}

/// Is `host` an IP literal inside Tailscale's assigned ranges — CGNAT `100.64.0.0/10` (IPv4) or the
/// `fd7a:115c:a1e0::/48` ULA prefix (IPv6)? Lets a raw tailnet address be a valid terminal origin
/// when the operator binds off-loopback, without listing each box's IP in `$SKEIN_ALLOWED_ORIGINS`.
/// A non-IP host (e.g. `evil.com`) never parses, so this only ever widens access to the tailnet.
fn is_tailnet_ip(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            let o = v4.octets();
            o[0] == 100 && (64..=127).contains(&o[1]) // 100.64.0.0/10
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0 // fd7a:115c:a1e0::/48
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::ORIGIN, HeaderMap, HeaderValue};

    fn with_origin(o: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(o) = o {
            h.insert(ORIGIN, HeaderValue::from_str(o).unwrap());
        }
        h
    }

    #[test]
    fn origin_guard_allows_local_and_tailnet_blocks_others() {
        assert!(origin_ok(&with_origin(None))); // non-browser client
        assert!(origin_ok(&with_origin(Some("http://127.0.0.1:7878"))));
        assert!(origin_ok(&with_origin(Some("http://localhost:7878"))));
        assert!(origin_ok(&with_origin(Some("https://box.my-tnet.ts.net"))));
        assert!(!origin_ok(&with_origin(Some("https://evil.com"))));
        // a look-alike that only *contains* ts.net must not pass
        assert!(!origin_ok(&with_origin(Some(
            "https://box.ts.net.evil.com"
        ))));
    }

    #[test]
    fn origin_guard_allows_raw_tailnet_ip_but_not_public_ip() {
        // A box reached by its raw Tailscale address (off-loopback bind) — CGNAT 100.64.0.0/10 v4
        // and the fd7a:115c:a1e0::/48 v6 prefix — is a valid origin without any per-IP allowlisting.
        assert!(origin_ok(&with_origin(Some("http://100.64.0.30:7878"))));
        assert!(origin_ok(&with_origin(Some("http://100.64.0.1:7878"))));
        assert!(origin_ok(&with_origin(Some(
            "http://[fd7a:115c:a1e0::1]:7878"
        ))));
        // 100.x outside the /10, ordinary LAN/public IPs, and non-tailnet v6 must still be blocked.
        assert!(!origin_ok(&with_origin(Some("http://100.128.0.1:7878"))));
        assert!(!origin_ok(&with_origin(Some("http://192.168.1.10:7878"))));
        assert!(!origin_ok(&with_origin(Some("http://8.8.8.8"))));
        assert!(!origin_ok(&with_origin(Some("http://[2001:db8::1]:7878"))));
    }

    #[test]
    fn origin_guard_honours_allowlist() {
        // This binary's own lock, held by three sibling tests. Without it, the window between the
        // set and the remove below is one in which any other test in this binary reads an
        // allowlist it never asked for (SKEIN-307).
        let _env = super::env_lock();
        std::env::set_var("SKEIN_ALLOWED_ORIGINS", "proxy.local, other.host");
        assert!(origin_ok(&with_origin(Some("https://proxy.local"))));
        assert!(!origin_ok(&with_origin(Some("https://nope.local"))));
        std::env::remove_var("SKEIN_ALLOWED_ORIGINS");
    }
}
