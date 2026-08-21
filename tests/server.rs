//! Black-box smoke test: launch the real `skein-server` binary and exercise the HTTP surface.
//! Catches route-wiring, the include_str! UI, vendored assets, and the :name path-traversal guard
//! — the layers a pure unit test can't see.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The token every fixture writes into its `$SKEIN_HOME`, and that every request below carries.
///
/// Fixed rather than read back after startup: the server mints one on first use, and a test racing
/// that would fail for a reason unrelated to what it tests. The refusal path has its own coverage in
/// `tests/ui/smoke.mjs`, against the running server.
const API_TOKEN: &str = "tttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttt";

/// A `$SKEIN_HOME` holding nothing but the API token, so a spawned server authenticates the requests
/// below and never touches the developer's real `~/.skein`.
fn token_home(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("skein-it-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("api-token"), API_TOKEN).unwrap();
    dir
}

/// One request over a fresh `Connection: close` socket → (status, full raw response incl. headers).
fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(
        format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\
             Connection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap();
    (status, text)
}

/// One GET that reads for at most `patience` — for a stream, which never closes.
fn http_get_for(addr: &str, path: &str, patience: Duration) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(patience)).unwrap();
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\r\n")
            .as_bytes(),
    )
    .unwrap();
    // **Bounded, not read-to-end.** A stream does not end, and a producer that keeps sending keeps
    // `read_to_end` reading — which is a test that hangs rather than one that fails. Enough bytes
    // for the headers and the opening event is the whole question here.
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0;
    while got < buf.len() {
        match s.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => {
                got += n;
                // The opening event has arrived; anything after it is the next tick, and waiting for
                // one is waiting for the fleet to change.
                if String::from_utf8_lossy(&buf[..got]).contains("\n\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    buf.truncate(got);
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, text)
}

/// One POST with a raw body + headers → (status, full raw response).
fn http_post(addr: &str, path: &str, headers: &str, body: &[u8]) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(
        format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\
             Connection: close\r\nContent-Length: {}\r\n{headers}\r\n",
            body.len()
        )
        .as_bytes(),
    )
    .unwrap();
    s.write_all(body).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap();
    (status, text)
}

/// Kill the server when the test ends, however it ends.
struct Kid(Child);
impl Drop for Kid {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn server_serves_ui_vendor_and_guards_routes() {
    let dir = std::env::temp_dir().join(format!("skein-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reg = dir.join("sandboxes.json");
    std::fs::write(
        &reg,
        r#"{"thing-a":{"branch":"a","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":"done"}}"#,
    )
    .unwrap();

    // A placement, because that is what makes a box a box. `fleet_sandbox` defaults to
    // `skein-fleet`, so this host is in the fleet model — where the placement records are the
    // register and a registry entry alone is a box skein never placed. The board stopped asking
    // `sbx ls` on every tick, and this is the other side of that: it no longer needs to.
    let home = token_home("routes");
    let places = std::path::PathBuf::from(&home).join("places");
    std::fs::create_dir_all(&places).unwrap();
    std::fs::write(
        places.join("thing-a.json"),
        r#"{"sandbox":"skein-fleet","ns_pid":1,"home":"/boxes/thing-a/home","tree":"/boxes/thing-a/tree","sock":"/boxes/thing-a/session.sock"}"#,
    )
    .unwrap();

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_REGISTRY", &reg)
        .env("SKEIN_HOME", &home)
        .env_remove("SKEIN_SHARED")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _kid = Kid(child);

    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "server never bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let (st, body) = http_get(&addr, "/");
    assert_eq!(st, 200);
    assert!(
        body.to_ascii_lowercase()
            .contains("cache-control: no-store"),
        "embedded UI must not survive a binary upgrade in the browser cache"
    );
    assert!(
        body.contains("/vendor/xterm.js"),
        "UI must reference vendored xterm"
    );
    assert!(body.contains("</html>"), "UI must not be truncated");

    // The queue answers with the standing **and** the rows it was derived from. Two calls would be
    // two answers, and the pair that disagrees is the reassuring one: "nothing needs you" over a
    // list of things that do.
    let (st, queue) = http_get(&addr, "/api/queue");
    assert_eq!(st, 200);
    let body = queue.split("\r\n\r\n").nth(1).unwrap_or("");
    let queued: serde_json::Value = serde_json::from_str(body.trim()).expect("the queue is JSON");
    assert!(
        queued.get("waiting").map(|w| w.is_array()).unwrap_or(false),
        "the queue no longer sends its rows under `waiting`: {body}"
    );
    assert!(
        queued
            .get("standing")
            .and_then(|s| s.get("standing"))
            .and_then(|s| s.as_str())
            .is_some(),
        "the queue no longer sends the standing beside the rows: {body}"
    );

    // What replaces Browse (parity §7): a typed path, and an answer that says what was found. A
    // link is reported as a link rather than as whatever it points at — the whole job of the line
    // is to say what is actually there.
    let (st, found) = http_get(&addr, &format!("/api/path?p={}", "/tmp"));
    assert_eq!(st, 200);
    assert!(found.contains("\"kind\":\"folder\""), "{found}");
    // A link is a link. Reporting what it points at would be a screen saying a folder is there when
    // what is there is a pointer at one — and it is the same rule §9.5 R8 applies wherever skein
    // looks at a path somebody else can shape.
    let linked = std::env::temp_dir().join(format!("skein-linkcheck-{}", std::process::id()));
    let _ = std::fs::remove_file(&linked);
    std::os::unix::fs::symlink("/tmp", &linked).unwrap();
    let (st, through) = http_get(&addr, &format!("/api/path?p={}", linked.display()));
    assert_eq!(st, 200);
    assert!(
        through.contains("\"kind\":\"link\""),
        "a symbolic link was reported as what it points at: {through}"
    );
    let _ = std::fs::remove_file(&linked);

    let (st, missing) = http_get(&addr, "/api/path?p=/definitely/not/here");
    assert_eq!(st, 200);
    assert!(
        missing.contains("\"resolved\":false") && missing.contains("\"kind\":\"missing\""),
        "a path that is not there must say so rather than erroring: {missing}"
    );

    // The new board, beside the old one. `docs/delivery.md` names treating "ground-up surfaces" and
    // "new topology" as one project as the biggest avoidable risk in the plan, and this route is
    // what keeps them separate — so the test that matters is that BOTH answer.
    let (st, v2) = http_get(&addr, "/v2");
    assert_eq!(st, 200, "the new board is not served");
    assert!(
        v2.contains("/vendor/cockpit.js"),
        "the new board does not load the bundle"
    );
    assert!(v2.contains("</html>"), "the new board is truncated");
    assert!(
        v2.to_ascii_lowercase().contains("cache-control: no-store"),
        "a cached new board outlives the binary its API belongs to"
    );

    // `?t=` lands back on the page it was offered to. Redirecting to `/` would look exactly like
    // the new board silently not existing.
    let (st, exchanged) = http_get(&addr, &format!("/v2?t={API_TOKEN}"));
    assert_eq!(st, 303, "the token was not exchanged for a session");
    assert!(
        exchanged.to_ascii_lowercase().contains("location: /v2"),
        "the session exchange sent the visitor to a different board: {exchanged}"
    );
    assert!(
        exchanged.contains("HttpOnly"),
        "the session cookie is reachable from script"
    );

    // The four vendor URLs are unchanged — the code behind them moved to the generated table, and
    // "unchanged" is the whole claim of that move.
    for (path, ct) in [
        ("/vendor/xterm.js", "application/javascript"),
        ("/vendor/xterm.css", "text/css"),
        ("/vendor/addon-fit.js", "application/javascript"),
        ("/vendor/marked.js", "application/javascript"),
    ] {
        let (st, body) = http_get(&addr, path);
        assert_eq!(st, 200, "{path}");
        assert!(body.contains(ct), "{path} is served as the wrong type");
    }

    // One route for a directory of built files, with no code per file. Reached by the name it has
    // in the table, since there is no bundle yet.
    let (st, body) = http_get(&addr, "/assets/xterm.min.js");
    assert_eq!(st, 200);
    assert!(body.contains("application/javascript"));
    assert!(
        body.to_ascii_lowercase()
            .contains("cache-control: no-store"),
        "an asset whose name carries no hash must not be cached: {}",
        body.lines().take(8).collect::<Vec<_>>().join(" | ")
    );

    // And a path that climbs is a 404 rather than a file. The server never joins a caller's path
    // onto anything; this is the end-to-end proof of that.
    for climbing in [
        "/assets/../Cargo.toml",
        "/assets/a/../../Cargo.toml",
        "/assets/nothing-here.js",
    ] {
        let (st, _) = http_get(&addr, climbing);
        assert!(
            st == 404 || st == 301 || st == 400,
            "{climbing} answered {st}"
        );
    }

    let (st, body) = http_get(&addr, "/api/boxes");
    assert_eq!(st, 200);
    assert!(body.contains("thing-a"));

    // ---- one producer, and the stream opens with a snapshot ----
    // Every client used to build its own interval and run the whole fleet snapshot itself; five
    // tabs were five snapshots a tick. The wire proof is the opening event: a client is *given* the
    // picture rather than computing one, which is only possible when a producer already holds it.
    let (st, body) = http_get_for(&addr, "/api/events", Duration::from_secs(6));
    assert_eq!(st, 200);
    assert!(
        body.contains("event: snapshot"),
        "the stream must open with the picture, not with the next change: {}",
        body.lines().take(12).collect::<Vec<_>>().join(" | ")
    );
    assert!(
        body.contains("\"event\":\"snapshot\""),
        "the event name and the payload's tag must agree, or a client switches on two things: {body}"
    );

    // ---- creating a box is something a surface that is not a terminal can ask for ----
    // The whole point of the route: before it, creation was `?launch=` on the terminal WebSocket, so
    // porting the REST API alone would have lost it. `thing-a` belongs to no registered repo, so
    // the act starts and then fails saying so — which is exactly the case that has to stay readable
    // after the stream closes, and the reason this is an Act rather than a POST returning 201.
    let (st, body) = http_post(
        &addr,
        "/api/boxes/thing-a/create",
        "Content-Type: application/json\r\n",
        br#"{"branch":"feat/x"}"#,
    );
    assert_eq!(
        st, 202,
        "creating a box must be accepted, not answered: {body}"
    );
    assert!(body.contains("create-thing-a"), "{body}");

    // Readable afterwards, by a caller that never watched anything.
    let mut settled = String::new();
    for _ in 0..200 {
        let (st, body) = http_get(&addr, "/api/acts/create-thing-a");
        assert_eq!(st, 200);
        if body.contains("\"state\":\"ended\"") {
            settled = body;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        settled.contains("no registered repo"),
        "a failed create must still say why once nobody is watching: {settled}"
    );
    assert!(
        settled.contains("\"code\":1"),
        "the exit code is the answer: {settled}"
    );

    // A second ask while one runs is a conflict, and an act nobody started is a 404.
    let (st, _) = http_get(&addr, "/api/acts/never-started");
    assert_eq!(st, 404);

    let (st, _) = http_get(&addr, "/api/boxes/x..y/diff");
    assert_eq!(st, 400, "path-traversal name must be rejected");

    // Attachments: the route is wired for any content type (not just images) and rejects a bad box
    // name before it can stream a byte into a sandbox. `sbx` is never invoked here.
    let (st, body) = http_post(
        &addr,
        "/api/boxes/x..y/upload",
        "Content-Type: video/mp4\r\nX-Skein-Name: clip.mp4\r\nX-Skein-Drop: b1\r\n",
        b"\0\0not-really-a-video",
    );
    assert_eq!(st, 200);
    assert!(
        body.contains("invalid box name"),
        "upload must guard the name: {body}"
    );

    // The default 2 MB body cap must be off on that route — a screenshot, let alone a video, is
    // bigger. axum rejects an over-cap upload from Content-Length alone, before the handler runs, so
    // announcing 3 MB and sending nothing is enough: with the cap this answers 413 immediately;
    // without it the request is either still waiting for the body or already failed on the *box*.
    // (Announce-only, so the test never races a 3 MB write against an early error response.)
    let mut s = TcpStream::connect(&addr).unwrap();
    s.write_all(
        format!(
            // The token matters here and not only for consistency: without it this answers 401,
            // which satisfies "not 413" and leaves the assertion below passing while testing
            // nothing at all.
            "POST /api/boxes/thing-a/upload HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
             Authorization: Bearer {API_TOKEN}\r\n\
             Content-Type: application/octet-stream\r\nX-Skein-Name: big.bin\r\n\
             Content-Length: {}\r\n\r\n",
            3 * 1024 * 1024
        )
        .as_bytes(),
    )
    .unwrap();
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf); // times out when the server is (correctly) awaiting the body
    let reply = String::from_utf8_lossy(&buf).to_ascii_lowercase();
    assert!(
        !reply.contains(" 413 ") && !reply.contains("length limit exceeded"),
        "body cap must be disabled on the upload route: {reply}"
    );
}

/// Regression guard for the "typing lags only when the box is idle" freeze.
///
/// The live-fleet snapshot (`load_views` — subprocess `sbx ls` + a per-box `git` + journal/diff
/// reads, 1-2s for a busy fleet) must run on the blocking pool, never inline on an async worker.
/// Inline (the original `api_events` `.map` / `api_boxes` body) it froze the worker for the whole
/// computation every 2s SSE tick, starving any terminal websocket sharing that worker: mid-stream
/// the output flood hid the gap, but at rest a lone keystroke's echo waited out the stall.
///
/// Proven at the HTTP layer — a terminal WS bridge is just another task on the same runtime, so if a
/// cheap request isn't starved, neither is the socket. Pin the server to ONE async worker thread
/// (`TOKIO_WORKER_THREADS=1`) so the starvation is deterministic (with the default worker-per-core
/// pool an inline block on one worker wouldn't reliably starve a task on another), make every
/// `load_views` sleep ~2s, then race a cheap static asset against an in-flight `/api/boxes`. Inline,
/// the sole worker is blocked and the asset can't be served until the sleep ends (~2s) → fails;
/// offloaded via `spawn_blocking`, the worker stays free and it returns in milliseconds.
#[test]
fn slow_fleet_snapshot_does_not_starve_concurrent_requests() {
    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("TOKIO_WORKER_THREADS", "1") // one async worker → starvation is deterministic
        .env("SKEIN_LS_CMD", "sleep 2; echo '[]'") // every load_views() now takes ~2s
        .env("SKEIN_HOME", token_home("starve"))
        .env_remove("SKEIN_REGISTRY")
        .env_remove("SKEIN_SHARED")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _kid = Kid(child);

    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "server never bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Put a slow snapshot in-flight on the worker, then time a cheap static asset racing it.
    let slow_addr = addr.clone();
    let slow = std::thread::spawn(move || http_get(&slow_addr, "/api/boxes")); // ~2s
    std::thread::sleep(Duration::from_millis(200)); // let load_views reach its sleep
    let t0 = Instant::now();
    let (st, _) = http_get(&addr, "/vendor/xterm.js");
    let cheap = t0.elapsed();
    assert_eq!(st, 200);
    assert!(
        cheap < Duration::from_millis(1000),
        "a cheap request was blocked for {cheap:?} while the fleet snapshot ran — load_views is \
         blocking the sole async worker instead of the blocking pool (the idle-terminal freeze)"
    );
    let _ = slow.join();
}

/// Saving settings must never clear a setting the screen does not render.
///
/// Every `Config` field has a serde default, so a partial body deserialized straight into one turns
/// each absent field into its default and writes that back. `fleet_sandbox` is not on the settings
/// screen — so saving *anything* cleared it, skein forgot the fleet existed, every box read as
/// legacy and the board emptied. Measured on a live fleet of eight.
#[test]
fn saving_settings_leaves_untouched_fields_alone() {
    let dir = std::env::temp_dir().join(format!("skein-settings-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{"fleet_sandbox":"skein-fleet","fleet_memory":"26g","base_branch":"trunk"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("api-token"), API_TOKEN).unwrap();

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", &dir)
        .env_remove("SKEIN_SHARED")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _kid = Kid(child);
    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "server never bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Exactly what the settings screen sends: the fields it renders, and no others.
    let (st, _) = http_post(
        &addr,
        "/api/settings",
        "Content-Type: application/json\r\n",
        br#"{"fleet_memory":"32g"}"#,
    );
    assert_eq!(st, 200);

    let (_, body) = http_get(&addr, "/api/settings");
    let saved: serde_json::Value =
        serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap_or("{}"))
            .expect("settings are JSON");
    assert_eq!(
        saved["fleet_memory"], "32g",
        "the field sent must be applied"
    );
    assert_eq!(
        saved["fleet_sandbox"], "skein-fleet",
        "a field the screen never renders must survive a save — clearing this one unmakes the fleet"
    );
    assert_eq!(saved["base_branch"], "trunk", "and so must every other one");
    std::fs::remove_dir_all(&dir).ok();
}

/// The repo list must name the GitHub repository the host will mint a token for — including for a
/// repo adopted from a local path, where the answer is in the clone's `origin` and the browser has no
/// way to look. Parsing `source` in the page instead left every adopted repo reading "not a GitHub
/// remote" while its boxes needed a token to push at all.
#[test]
fn the_repo_list_names_the_repository_the_host_will_mint_for() {
    let dir = std::env::temp_dir().join(format!("skein-repos-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("api-token"), API_TOKEN).unwrap();

    // Adopted in place: `source` is a path, and only the clone knows it is a GitHub repo. This is
    // skein's own shape, not an exotic one.
    let adopted = dir.join("code/adopted");
    let plain = dir.join("code/plain");
    for (work, origin) in [
        (&adopted, "git@github.com:acme/adopted.git"),
        (&plain, ""),
    ] {
        std::fs::create_dir_all(work).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(work)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
        };
        git(&["init", "-q"]);
        if !origin.is_empty() {
            git(&["remote", "add", "origin", origin]);
        }
    }
    std::fs::write(
        dir.join("repos.json"),
        serde_json::json!([
            { "id": "adopted", "source": adopted.to_string_lossy(), "work": adopted.to_string_lossy(), "store": "" },
            { "id": "plain", "source": plain.to_string_lossy(), "work": plain.to_string_lossy(), "store": "" },
            { "id": "cloned", "source": "https://github.com/acme/cloned.git", "work": "", "store": "" },
        ])
        .to_string(),
    )
    .unwrap();

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", &dir)
        .env_remove("SKEIN_SHARED")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _kid = Kid(child);
    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "server never bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let (_, body) = http_get(&addr, "/api/repos");
    let repos: Vec<serde_json::Value> =
        serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap_or("[]"))
            .expect("repos are JSON");
    let slug_of = |id: &str| -> String {
        repos
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id} is in the list"))["slug"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(
        slug_of("adopted"),
        "acme/adopted",
        "an adopted clone's origin is what names it"
    );
    assert_eq!(
        slug_of("cloned"),
        "acme/cloned",
        "and a URL-added repo is named by the URL"
    );
    assert_eq!(
        slug_of("plain"),
        "",
        "a repo with no remote anywhere has nothing to name — and no token field to offer"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A box gets a free denial of the control plane if connections cost nothing until they
/// authenticate — architecture §9.4, "pre-auth connection exhaustion". The port is reachable from
/// every box (one network namespace) and both existing caps are inside handlers, so they count only
/// clients that already presented a credential.
///
/// Two properties, and the second is the one worth stating: a flood is **bounded**, so it cannot
/// climb to the file-descriptor limit; and it does not stop an authenticated client being served,
/// because the doorstep evicts the oldest stranger rather than refusing the newest arrival. A cap
/// that refused would satisfy the first and fail the second, which is why the test asserts both.
#[test]
fn a_flood_that_never_authenticates_cannot_hold_the_door() {
    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", token_home("flood"))
        // Two seconds instead of ten: the deadline is the same mechanism at either length, and the
        // default would make this test spend most of its life waiting for a clock.
        .env("SKEIN_DOORSTEP_GRACE", "2")
        .env_remove("SKEIN_REGISTRY")
        .env_remove("SKEIN_SHARED")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _kid = Kid(child);

    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "server never bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Sockets that connect and say nothing at all — no request line, no credential. This is the
    // whole of the attack: it needs no token, because §9.4's answer is that connecting is not
    // authenticating, and that answers reading rather than exhausting.
    let room = skein::knock::ROOM;
    let flood: Vec<TcpStream> = (0..room + 16)
        .map(|_| {
            let s = TcpStream::connect(&addr).expect("the port accepts");
            // Well under the two-second grace, deliberately. The deadline closes every stranger
            // eventually, so a patient read proves nothing about eviction: a room that evicts
            // nobody would pass it. What is being asserted here is that these were closed
            // **immediately, by the arrivals after them**.
            s.set_read_timeout(Some(Duration::from_millis(700)))
                .unwrap();
            s
        })
        .collect();

    // The oldest are gone: at the limit, an arrival takes the place of the stranger that has been
    // standing longest. Read returns end-of-stream on a socket the server closed.
    let mut closed = 0;
    for held in flood.iter().take(16) {
        let mut byte = [0u8; 1];
        if matches!((&mut &*held).read(&mut byte), Ok(0)) {
            closed += 1;
        }
    }
    assert_eq!(
        closed,
        16,
        "the first 16 of {} connections should have been evicted by the arrivals after them — a \
         flood that is not bounded reaches the file-descriptor limit and the cockpit stops \
         answering",
        flood.len()
    );

    // And the point of evicting rather than refusing: the client that *will* authenticate arrives
    // into a room that is full, and is served anyway.
    let t0 = Instant::now();
    let (st, _) = http_get(&addr, "/api/boxes");
    assert_eq!(
        st, 200,
        "an authenticated request was refused while a flood held the door — a cap that refuses \
         when full lets the flooder decide who gets in"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "an authenticated request waited {:?} behind the flood",
        t0.elapsed()
    );

    // And the flood is **visible**, which is the other half of harmless. `knock` keeps the cockpit
    // answering, and that is exactly what would leave a flood showing up as "the board felt slow
    // once" with nothing to look at.
    let (st, seen) = http_get(&addr, "/api/machine/doorstep");
    assert_eq!(st, 200);
    let body = seen.split("\r\n\r\n").nth(1).unwrap_or("");
    let door: serde_json::Value = serde_json::from_str(body.trim()).expect("the doorstep is JSON");
    assert!(
        door["turned_away"].as_u64().unwrap_or(0) >= 16,
        "the evictions are not reachable from outside the process: {body}"
    );
    assert_eq!(door["room"].as_u64(), Some(skein::knock::ROOM as u64));

    // Behind the token, like everything that is not a static asset — otherwise the flooder can
    // watch its own progress, and a defence that reports on itself to whoever is attacking it is
    // helping. Asked with no credential at all, which is what a box has.
    let mut bare = TcpStream::connect(&addr).unwrap();
    bare.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    bare.write_all(
        format!("GET /api/machine/doorstep HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .unwrap();
    let mut refused = Vec::new();
    let _ = bare.read_to_end(&mut refused);
    let refused = String::from_utf8_lossy(&refused).into_owned();
    assert!(
        !refused.contains("turned_away"),
        "an unauthenticated caller was told how the flood is going: {refused}"
    );

    // The second half: a stranger that survived the eviction still does not get to stand there for
    // free. Every one of them is closed once the grace period passes.
    let last = flood.last().expect("the flood is not empty");
    last.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut byte = [0u8; 1];
    let deadline = Instant::now();
    let ended = matches!((&mut &*last).read(&mut byte), Ok(0));
    assert!(
        ended,
        "a connection that never presented a credential was still open after {:?} — the grace \
         deadline is what makes a slot cost a reconnection instead of nothing",
        deadline.elapsed()
    );
    drop(flood);
}

/// The socket is opened before skein and handed to it, and skein serves on **that** one.
///
/// architecture §9.4: one network namespace and a port mapping that outlives skein means a box that
/// binds the cockpit's port before skein does becomes the cockpit, and the browser hands it the
/// fleet's token on the first request. The token cannot answer that — the only answer is that the
/// port is never free, which means skein takes a socket somebody else opened rather than racing for
/// one. This is the taking half; the in-fleet start that does the opening is 4c.
///
/// Driven through `python3` because the descriptor has to survive `exec` with `CLOEXEC` cleared and
/// land on fd 3, and the standard library exposes neither `dup2` nor a way to clear that flag. What
/// it stands in for is the process manager, which does exactly this and no more.
#[test]
fn the_server_serves_on_a_socket_it_was_handed_rather_than_one_it_bound() {
    if Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipping: no python3 to stand in for the process manager");
        return;
    }
    let home = token_home("handover");
    let where_port = home.join("port");
    // Binds, puts the listener on fd 3 with CLOEXEC cleared, writes the port it got, and execs the
    // server. `LISTEN_PID` is left out deliberately: it is the older half of the convention and is
    // accepted, and setting it would mean predicting a pid this side of the fork.
    let handover = r#"
import os, socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.bind(("127.0.0.1", 0))
s.listen(64)
open(sys.argv[1], "w").write(str(s.getsockname()[1]))
os.set_inheritable(s.fileno(), True)
if s.fileno() != 3:
    os.dup2(s.fileno(), 3, inheritable=True)
os.environ["LISTEN_FDS"] = "1"
os.execv(sys.argv[2], sys.argv[2:])
"#;
    let child = Command::new("python3")
        .args(["-c", handover])
        .arg(&where_port)
        .arg(env!("CARGO_BIN_EXE_skein-server"))
        // Somewhere it could never have bound by itself, so a pass cannot be a bind that happened to
        // work: the address served below is read back from the socket python opened.
        .env("SKEIN_ADDR", "127.0.0.1:1")
        .env("SKEIN_HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _kid = Kid(child);

    let start = Instant::now();
    let addr = loop {
        if let Ok(port) = std::fs::read_to_string(&where_port) {
            let addr = format!("127.0.0.1:{}", port.trim());
            if TcpStream::connect(&addr).is_ok() {
                break addr;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "the server never served on the socket it was handed"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    let (st, _) = http_get(&addr, "/api/health");
    assert_eq!(
        st, 200,
        "the handed-in socket accepted a connection but the server behind it did not answer"
    );
}

/// A start that was told the socket comes from outside, and got none, stops.
///
/// The two modes want opposite answers and the difference has to be *said*. Host-driven there is
/// nobody upstream to open a socket, so binding is the only way to start. In the fleet a missing
/// descriptor means the start sequence did not do its job — and binding anyway runs the very race
/// this closes, from the one process that was supposed to have closed it.
#[test]
fn told_the_socket_comes_from_outside_and_given_none_the_server_refuses_to_bind() {
    let home = token_home("inherited-only");
    let addr = format!("127.0.0.1:{}", free_port());
    let mut child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", &home)
        .env("SKEIN_LISTEN_INHERITED_ONLY", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Waited for rather than read to the end. A server that ignored the setting and bound is a
    // server that never exits, so `output()` here would *hang* instead of failing — which is a
    // failure a person has to interpret from a stuck run. Written the other way round the first
    // time, and the sabotage took two minutes to say what this says in five seconds.
    let start = Instant::now();
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None if start.elapsed() > Duration::from_secs(10) => {
                let _ = child.kill();
                panic!("the server stayed up, so it bound a port it was told not to bind");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    assert!(
        !status.success(),
        "the server exited cleanly rather than refusing"
    );
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let why = stderr;
    assert!(why.contains("LISTEN_FDS=1"), "{why}");
    assert!(why.contains("SKEIN_LISTEN_INHERITED_ONLY"), "{why}");
    // And it really did not bind — otherwise the refusal is a message printed over a live socket.
    assert!(
        TcpListener::bind(&addr).is_ok(),
        "the port is still held, so the refusal happened after the bind"
    );
}
