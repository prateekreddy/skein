//! The HTTP surface as a person's browser meets it: the UI and vendored assets served, routes
//! guarded, a slow fleet not starving other requests, settings saved field by field, and the
//! repo list naming the repository the host will mint for.

use super::*;

#[test]
fn server_serves_ui_vendor_and_guards_routes() {
    let dir = Scratch::temp("skein-it-registry");
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
    let places = home.to_path_buf().join("places");
    std::fs::create_dir_all(&places).unwrap();
    std::fs::write(
        places.join("thing-a.json"),
        r#"{"sandbox":"skein-fleet","ns_pid":1,"home":"/boxes/thing-a/home","tree":"/boxes/thing-a/tree","sock":"/boxes/thing-a/session.sock"}"#,
    )
    .unwrap();

    // **No `$SKEIN_ADDR`, and no port chosen in advance.** `serving` hands the server a socket it
    // already holds — see [`handed`] for why every spawn in this file is written this way now.
    let (child, addr) = serving(
        skein_server()
            .env("SKEIN_REGISTRY", &reg)
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // **And the warden, at an address where nothing listens.** This is a real
            // `skein-server`, so it asks one at boot — and it inherits `$SKEIN_TEST` from
            // cargo's `[env]` table, so `warden_client` refuses it the default rather than
            // letting it ask whatever warden the machine running the suite can reach
            // (SKEIN-762). Port 1 on loopback is refused by the kernel, which is also the
            // answer `the_server_says_at_boot_when_no_warden_is_answering` is about.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

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

    // **`/api/path` is gone, and not moved** (SKEIN-947). It answered "is there a file, folder or
    // link here" for any path on the filesystem skein-server stands on, which in the fleet is the
    // sandbox: the fleet root, `.skein/private/`, `/etc`. Its one caller was a field naming a key
    // on the host, a machine it could not see. Both went.
    //
    // A 404 alone could come from a server that is not answering at all, so the check compares it
    // with a path that never existed: the same status and no answer about the path. A route left in
    // place, or a fallback that swallows it, would answer 200 or answer something else. The
    // settings read beside it proves the same server does answer a real route.
    let (st, gone) = http_get(&addr, "/api/path?p=/tmp");
    let (never_st, _) = http_get(&addr, "/api/never-a-route-skein-947?p=/tmp");
    assert_eq!(
        st, 404,
        "/api/path answers again — it resolves paths on the sandbox's filesystem: {gone}"
    );
    assert_eq!(
        st, never_st,
        "/api/path is answered differently from a path that never existed: {gone}"
    );
    assert!(
        !gone.contains("\"kind\"") && !gone.contains("\"resolved\""),
        "something still says what is at a path: {gone}"
    );
    let (st, _) = http_get(&addr, "/api/settings");
    assert_eq!(
        st, 200,
        "the server beside the 404 is not answering real routes either"
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
    let home = token_home("starve");
    // `serving` and not a connect loop, and here it is load-bearing beyond the port: what this test
    // measures starts the moment it returns, and a connection is answered by the kernel long before
    // the server is serving. Waiting for an actual response means the ~2s that follows is the
    // starvation under test rather than the tail of a start-up.
    let (child, addr) = serving(
        skein_server()
            .env("TOKIO_WORKER_THREADS", "1") // one async worker → starvation is deterministic
            .env("SKEIN_LS_CMD", "sleep 2; echo '[]'") // every load_views() now takes ~2s
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env_remove("SKEIN_REGISTRY")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

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
    let dir = doorway_stopped(Scratch::temp("skein-settings-it"));
    std::fs::write(
        dir.join("config.json"),
        r#"{"fleet_sandbox":"skein-fleet","fleet_memory":"26g","git_name":"Example Person"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("api-token"), API_TOKEN).unwrap();

    let (child, addr) = serving(
        skein_server()
            .env("SKEIN_HOME", dir.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&dir))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

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
    assert_eq!(
        saved["git_name"], "Example Person",
        "and so must every other one"
    );
}

/// The repo list must name the GitHub repository the host will mint a token for — including for a
/// repo whose `source` is not a URL, where the answer is on the MIRROR and the browser has no way
/// to look. Parsing `source` in the page instead left such a repo reading "not a GitHub remote"
/// while its boxes needed a token to push at all.
///
/// `source` is a URL for every repo registered now, but a `repos.json` written before that still
/// carries a path — seen live on 2026-08-30, where a repo's `source` was a dead local path while
/// its mirror fetched from GitHub perfectly well. The mirror is the answer in that case.
#[test]
fn the_repo_list_names_the_repository_the_host_will_mint_for() {
    let dir = doorway_stopped(Scratch::temp("skein-repos-it"));
    std::fs::write(dir.join("api-token"), API_TOKEN).unwrap();

    // `source` is a path, and only the MIRROR knows it is a GitHub repo.
    let adopted = dir.join("code/adopted");
    let plain = dir.join("code/plain");
    for (id, origin) in [
        ("adopted", "git@github.com:acme/adopted.git"),
        ("plain", ""),
    ] {
        let mirror = dir.join("repos").join(id).join("mirror");
        std::fs::create_dir_all(&mirror).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&mirror)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
        };
        git(&["init", "-q", "--bare"]);
        if !origin.is_empty() {
            git(&["remote", "add", "origin", origin]);
        }
    }
    // **Each repo names a store of its own, under this test's scratch.** All three carried
    // `"store": ""` until SKEIN-551: the server scaffolds every store `registry::all_stores`
    // yields, an empty one resolved against the server's own working directory — which is this
    // crate's root, since nothing below sets `current_dir` — and this single test put
    // `settings.json`, `skein/` and fourteen empty directories at the checkout root on every run.
    // `kit::ensure_store` refuses a path like that now, so the fixture is no longer load-bearing
    // for the defect; it is realistic instead, which is what a repo record looks like in the field.
    let store_of = |id: &str| {
        dir.join("repos")
            .join(id)
            .join("store/.claude")
            .to_string_lossy()
            .into_owned()
    };
    std::fs::write(
        dir.join("repos.json"),
        serde_json::json!([
            { "id": "adopted", "source": adopted.to_string_lossy(), "store": store_of("adopted") },
            { "id": "plain", "source": plain.to_string_lossy(), "store": store_of("plain") },
            { "id": "cloned", "source": "https://github.com/acme/cloned.git", "store": store_of("cloned") },
        ])
        .to_string(),
    )
    .unwrap();

    let (child, addr) = serving(
        skein_server()
            .env("SKEIN_HOME", dir.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&dir))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

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
        "the mirror's origin is what names it"
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
}
