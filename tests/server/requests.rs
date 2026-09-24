//! What a request can ask for and what it is refused: the review queue as rows, a path
//! that tries to climb out of `$SKEIN_HOME`, the token a printed cockpit URL carries, and a
//! store outside the home refused over HTTP.

use super::*;

/// A GitHub the size of what a queue refresh asks for: the viewer, its teams, and one GraphQL
/// search answering `n` open pull requests.
///
/// Its own stub rather than `tests/review_queue.rs`'s, because that one drives the LIBRARY through
/// process-global environment variables and this drives the real binary as a child. The seam is
/// the same either way (`SKEIN_GITHUB_API`, `src/github.rs:92-97`), which is what makes the child
/// reachable without a network at all — and it is why both stubs now share `common::fake_github`
/// for the connection/parsing loop while keeping their own, different, answers.
fn stub_github_for(prs: u64, reading: bool) -> String {
    let nodes = (1..=prs)
        .map(|n| {
            format!(
                r#"{{"number":{n},"title":"pull request {n}","author":{{"login":"dana"}},"url":"https://github.com/acme/thing/pull/{n}","headRefName":"feat-{n}","headRefOid":"sha{n}","baseRefName":"main","isDraft":false,"updatedAt":"2026-08-20T00:00:00Z","latestReviews":{{"nodes":[]}},"commits":{{"nodes":[{{"commit":{{"statusCheckRollup":null}}}}]}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fake_github(move |req| {
        let path = &req.path;
        let body = String::from_utf8_lossy(&req.body).into_owned();
        if path.starts_with("/user/teams") {
            (403, r#"{"message":"Requires read:org"}"#.to_string())
        } else if path == "/user" || path.starts_with("/user?") {
            (200, r#"{"login":"me"}"#.to_string())
        } else if path.starts_with("/graphql") {
            // One request carries every membership search of a refresh, aliased q0…qN, and
            // each alias answers under its own name. The review-requested one carries the
            // queue; everything else answers empty, so a PR appears once.
            let aliases: Vec<String> = body
                .match_indices("\"q")
                .filter_map(|(at, _)| body[at + 1..].split('"').next().map(str::to_string))
                .filter(|a| a.len() > 1 && a[1..].chars().all(|c| c.is_ascii_digit()))
                .collect();
            let answered = aliases
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    format!(
                        r#""{a}":{{"nodes":[{}]}}"#,
                        if i == 0 { nodes.as_str() } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            (200, format!(r#"{{"data":{{{answered}}}}}"#))
        } else if reading && path.contains("/files") {
            (200, r#"[{"filename":"src/parser.rs"}]"#.to_string())
        } else if reading && path.contains("/pulls/") {
            // The raw diff. Served only when a test is exercising a READING; the queue-shape
            // tests want a route that cannot compute, so that "it answered from disk" and "it
            // went and bought one" are different outcomes rather than the same one.
            (
                200,
                "diff --git a/src/parser.rs b/src/parser.rs\n--- a/src/parser.rs\n                     +++ b/src/parser.rs\n@@ -1 +1 @@\n-const TIMEOUT: u64 = 30;\n                     +const TIMEOUT: u64 = 5;\n"
                    .to_string(),
            )
        } else {
            (404, r#"{"message":"no stub"}"#.to_string())
        }
    })
}

/// The queue's bulk payload can be asked for ROWS instead of prose (SKEIN-287).
///
/// Measured on the owner's fleet, 2026-08-25: `GET /api/repos/gadget-demo/review/summaries`
/// answered 153,381 bytes in 10.42 s for thirty-nine stored readings — each carrying a brief of
/// several thousand characters, its signals and its whole drafted review, none of which a
/// collapsed row draws. Ten seconds of one of the browser's per-origin connections is what turns
/// that from slow into wrong: everything the reader presses in that window queues behind it.
///
/// Driven through the ROUTE, not through `review::known`, because the unit test cannot see a query
/// parameter that is read but never applied — the shape of SKEIN-273's three dead buttons, which
/// every unit test passed. The sizes are printed with the test so the before and after are
/// reproducible by running it.
#[test]
fn the_review_queue_payload_can_be_asked_for_rows_instead_of_prose() {
    const PRS: u64 = 39;
    let home = token_home("rows");
    let api = stub_github_for(PRS, false);

    std::fs::write(
        home.join("repos.json"),
        format!(
            r#"[{{"id":"demo","source":"https://github.com/acme/thing.git","source_tree":"{}","store":"{}","agent":"claude","read_prs":false,"plane_project":"","sync_connection":""}}]"#,
            home.join("tree").display(),
            home.join("store").display()
        ),
    )
    .unwrap();

    // Readings on disk, in the shape a real one has: a brief of a few thousand characters, the
    // signals found in the diff, the owned paths, and a drafted review beside it.
    let summaries = home.join("review").join("demo").join("summaries");
    let critiques = home.join("review").join("demo").join("critiques");
    std::fs::create_dir_all(&summaries).unwrap();
    std::fs::create_dir_all(&critiques).unwrap();
    let brief = "## What it does\n\nShortens how long a request waits before giving up, and \
                 accounts for every caller that relied on the old ceiling.\n\n"
        .repeat(24);
    for n in 1..=PRS {
        // Every reading is of the head the queue reports — except the LAST, whose branch has moved
        // since it was read. That row is why `held=1` exists: the queue keeps and marks such a
        // reading, and the computing route cannot hand it back, because its cache lookup is keyed
        // on the head that is there now.
        let read_at = if n == PRS {
            "older".to_string()
        } else {
            format!("sha{n}")
        };
        std::fs::write(
            summaries.join(format!("{n}-{read_at}.json")),
            serde_json::to_vec(&serde_json::json!({
                "number": n, "head_sha": read_at, "depth": "expanded",
                "line": "the request timeout default drops from 30s to 5s.",
                "detail": brief,
                "flags": ["default", "behaviour"],
                "yours": ["src/parser.rs", "src/timeout.rs"], "others": 3,
                "signals": [{"kind": "default", "what": "TIMEOUT moved from 30 to 5",
                             "file": "src/parser.rs", "symbol": "TIMEOUT"}],
                "unread_because": "", "computed": true,
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            critiques.join(format!("{n}-sha{n}.json")),
            serde_json::to_vec(&serde_json::json!({
                "number": n, "head_sha": format!("sha{n}"),
                "overall": "one real problem, and a second worth a look.",
                "comments": [{"path": "src/parser.rs", "line": 12, "anchored": true,
                              "text": "this drops the error rather than returning it",
                              "line_text": "    let _ = parse(input);"}],
                "truncated": false,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    let (child, addr) = serving(
        skein_server()
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_GITHUB_API", &api)
            .env("GH_TOKEN", "skein-test-token")
            .env("SKEIN_REGISTRY", "")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

    let body_of = |raw: &str| {
        raw.split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default()
    };
    let at = Instant::now();
    let (code, raw) = http_get(&addr, "/api/repos/demo/review/summaries");
    let full_took = at.elapsed();
    assert_eq!(code, 200, "{raw}");
    let full = body_of(&raw);
    let at = Instant::now();
    let (code, raw) = http_get(&addr, "/api/repos/demo/review/summaries?rows=1");
    let rows_took = at.elapsed();
    assert_eq!(code, 200, "{raw}");
    let rows = body_of(&raw);

    println!(
        "SKEIN-287  {PRS} stored readings\n  full  {:>8} B  {:?}\n  rows  {:>8} B  {:?}",
        full.len(),
        full_took,
        rows.len(),
        rows_took
    );

    let full: serde_json::Value = serde_json::from_str(&full).expect("the full payload is JSON");
    let rows: serde_json::Value = serde_json::from_str(&rows).expect("the row payload is JSON");
    assert_eq!(
        full.as_object().unwrap().len(),
        PRS as usize,
        "the fixture did not produce {PRS} readings, so nothing below is measuring what it says"
    );
    assert_eq!(
        rows.as_object().unwrap().len(),
        PRS as usize,
        "asking for rows lost pull requests — this is a thinner payload, never a shorter list"
    );

    // The default is untouched: every caller that asks the way the pane asks today gets exactly
    // what it got before, prose and all.
    assert!(
        full["1"]["detail"]
            .as_str()
            .unwrap()
            .contains("What it does"),
        "the default payload stopped carrying the brief"
    );
    assert_eq!(full["1"]["signals"][0]["symbol"], "TIMEOUT");

    // And the row payload carries the line and the flags — with none of the prose behind them.
    assert_eq!(
        rows["1"]["line"],
        "the request timeout default drops from 30s to 5s."
    );
    assert_eq!(
        rows["1"]["flags"],
        serde_json::json!(["default", "behaviour"])
    );
    assert_eq!(
        rows["1"]["detail"].as_str().unwrap_or("").len(),
        0,
        "the brief is still riding every queue row — `?rows=1` was read and not applied"
    );
    let (full_len, rows_len) = (full.to_string().len(), rows.to_string().len());
    assert!(
        rows_len * 4 < full_len,
        "the row payload is not materially smaller: {rows_len} B against {full_len} B"
    );

    // The prose the row stopped carrying is still reachable, one row at a time, off disk.
    let (code, raw) = http_get(&addr, "/api/repos/demo/review/1/summary?held=1");
    assert_eq!(code, 200, "{raw}");
    let one: serde_json::Value = serde_json::from_str(&body_of(&raw)).unwrap();
    assert!(
        one["detail"].as_str().unwrap().contains("What it does"),
        "opening a row found no brief behind it: {one}"
    );
    assert_eq!(one["signals"][0]["symbol"], "TIMEOUT");

    // And the row `held=1` exists for: one whose branch has moved since it was read. The queue
    // keeps that reading and says so (`stale`), so opening it must hand the prose over — while the
    // COMPUTING route, whose cache lookup is keyed on the head that is there now, misses and goes
    // off to buy a new reading. Both are asked here, because the difference between them IS the
    // behaviour: without it, `held=1` could be dropped and every assertion above would still pass.
    assert_eq!(
        rows[&PRS.to_string()]["stale"],
        true,
        "the fixture's moved row is not being reported as read before the latest commits"
    );
    let (code, raw) = http_get(
        &addr,
        &format!("/api/repos/demo/review/{PRS}/summary?held=1"),
    );
    assert_eq!(code, 200, "{raw}");
    let moved: serde_json::Value = serde_json::from_str(&body_of(&raw)).unwrap();
    assert!(
        moved["detail"]
            .as_str()
            .unwrap_or("")
            .contains("What it does"),
        "opening a row read before the latest commits found no brief behind it: {moved}"
    );
    assert_eq!(
        moved["stale"], true,
        "a reading of an earlier commit was handed over as current"
    );
    let (code, raw) = http_get(
        &addr,
        &format!("/api/repos/demo/review/{PRS}/summary?asked=1"),
    );
    assert_eq!(code, 200, "{raw}");
    let bought: serde_json::Value = serde_json::from_str(&body_of(&raw)).unwrap();
    assert_eq!(
        bought["depth"], "unread",
        "the computing route answered a moved row from disk, so `held=1` is measuring nothing: \
         {bought}"
    );
}

// The test that stood here — "the read route replaces a drafted review only when the caller asks"
// — guarded a distinction that no longer exists. `force=1` re-read and kept the review the reader
// had vetted; `redraft=1` replaced it. There is no vetted review to keep: the session posts its
// own to GitHub, and skein stores none.
//
// What `redraft=1` still MEANS is `review::Review::Always` — review it even on a pull request
// skein would not review unasked — and that is asserted where the decision is made,
// `src/review.rs`'s `visit` tests.

/// **A string from a request that becomes a host path is refused when it is not a name — and an
/// ordinary name still works.**
///
/// Both halves, in one test, because the repo has been bitten by the other shape: an assertion that
/// something is absent proves nothing unless the same test has shown it can be present. So every
/// case below writes its file once with a name skein would accept, then asks for the same write
/// with `..%2F..%2F<marker>` and asserts the marker directory was never made.
///
/// Over the wire and through the real binary rather than as a unit test, because the question is
/// partly about the routing layer: `matchit` matches on the raw path, so `..%2F` never looks like a
/// separator to the router, and `Path<String>` then percent-decodes it into `../../`. That is the
/// step this exercises and a call to the library function cannot.
///
/// Four routes, and they were not all wrong the same way. `archive` and `snooze` had no check of
/// any kind where fifteen sibling `/api/repos/:id` routes resolve the id through `load_repos()`.
/// `tracking` and `mailbox` reach library functions that build a path out of a box name that no
/// caller had validated. `POST /api/repos` minted a repo id straight into a directory name.
#[test]
fn a_request_string_that_becomes_a_path_cannot_climb_out_of_skein_home() {
    let home = token_home("traversal");
    // The marker sits one level above `$SKEIN_HOME`, which is exactly where `../../` from
    // `<home>/review/<id>` and `<home>/boxes/<name>` lands.
    let marker = home
        .to_path_buf()
        .parent()
        .unwrap()
        .join(format!("skein-it-traversal-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&marker);
    let climb = format!(
        "..%2F..%2F{}",
        marker.file_name().unwrap().to_string_lossy()
    );
    let raw_climb = format!("../../{}", marker.file_name().unwrap().to_string_lossy());

    // One registered repo, so the ordinary half of each pair has something real to act on.
    std::fs::write(
        home.to_path_buf().join("repos.json"),
        br#"[{"id":"probe","source":"https://github.com/acme/thing.git","store":"/nonexistent","agent":"claude"}]"#,
    )
    .unwrap();
    // One configured work-tracking connection, so the ordinary tracking write below names one that
    // exists: a choice naming no configured connection is refused on its own (SKEIN-1134), and
    // without this the ordinary half would be refused for that instead of succeeding.
    std::fs::write(
        home.to_path_buf().join("connections.json"),
        br#"[{"id":"plane","label":"plane","gateway_url":"https://plane.example"}]"#,
    )
    .unwrap();

    let (child, addr) = serving(
        skein_server()
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_REGISTRY", home.to_path_buf().join("registry.json"))
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

    let json = "Content-Type: application/json\r\n";

    // ── the review archive and snooze ────────────────────────────────────────────────────────
    let (_, ok) = http_post(
        &addr,
        "/api/repos/probe/review/7/archive",
        json,
        br#"{"on":true}"#,
    );
    assert!(
        ok.contains("\"ok\":true"),
        "a registered repo cannot be archived, so the refusal below proves nothing: {ok}"
    );
    assert!(
        home.to_path_buf()
            .join("review/probe/archived.json")
            .exists(),
        "the ordinary archive wrote nothing"
    );
    let (_, no) = http_post(
        &addr,
        &format!("/api/repos/{climb}/review/7/archive"),
        json,
        br#"{"on":true}"#,
    );
    assert!(
        no.contains("no such repo"),
        "a traversing repo id was not refused as an unknown repo: {no}"
    );
    let (_, no) = http_post(
        &addr,
        &format!("/api/repos/{climb}/review/7/snooze"),
        json,
        br#"{"head_sha":"abc"}"#,
    );
    assert!(
        no.contains("no such repo"),
        "a traversing repo id was not refused by snooze: {no}"
    );

    // ── a box's tracking choice ──────────────────────────────────────────────────────────────
    let (st, _) = http_post(
        &addr,
        "/api/boxes/probe-a/tracking",
        json,
        br#"{"connection":"plane"}"#,
    );
    assert_eq!(st, 204, "an ordinary box name could not record a choice");
    assert!(
        home.to_path_buf().join("boxes/probe-a/tracking").exists(),
        "the ordinary tracking write left no file"
    );
    let (st, why) = http_post(
        &addr,
        &format!("/api/boxes/{climb}/tracking"),
        json,
        br#"{"connection":"plane"}"#,
    );
    // 400 specifically, not merely "not 204": a 500 would also satisfy `!= 204` and would mean the
    // write was attempted and failed for some other reason, which is a different outcome.
    assert_eq!(
        st, 400,
        "a traversing box name was not refused as one: {why}"
    );

    // ── the mailbox, where the name is a body field and needs no encoding at all ─────────────
    let (st, _) = http_post(
        &addr,
        "/api/mailbox",
        json,
        br#"{"to":"probe-a","kind":"note","body":"hello"}"#,
    );
    assert_eq!(st, 200, "an ordinary box could not be sent a message");
    assert!(
        home.to_path_buf().join("boxes/probe-a/inbox").exists(),
        "the ordinary send left no inbox"
    );
    let (st, why) = http_post(
        &addr,
        "/api/mailbox",
        json,
        format!(r#"{{"to":"{raw_climb}","kind":"note","body":"hello"}}"#).as_bytes(),
    );
    assert_ne!(st, 200, "a traversing recipient was delivered to: {why}");

    // ── registering a repo, the one place an id is minted ────────────────────────────────────
    // The clone fails (there is no such repository, and no network here), so this asserts on WHICH
    // refusal comes back: an id that never reached the filesystem, not a clone that did.
    let (_, why) = http_post(
        &addr,
        "/api/repos",
        json,
        format!(r#"{{"source":"https://github.com/acme/thing.git","id":"{raw_climb}"}}"#)
            .as_bytes(),
    );
    assert!(
        why.contains("cannot be a repo id"),
        "a traversing repo id was not refused before it became a directory: {why}"
    );
    // And the non-vacuous half: a path source is refused for being a path, not for its id.
    let (_, why) = http_post(
        &addr,
        "/api/repos",
        json,
        br#"{"source":"/home/somebody/private.git","id":"local"}"#,
    );
    assert!(
        why.contains("registers repos by remote"),
        "a local path ending in .git was accepted as a remote: {why}"
    );

    // ── nothing at all, anywhere above `$SKEIN_HOME` ─────────────────────────────────────────
    assert!(
        !marker.exists(),
        "{} was created: something wrote outside SKEIN_HOME",
        marker.display()
    );
}

/// **The `?t=` the server prints is the real token, and it opens the API.**
///
/// This exists because of the shape of a near-miss rather than of a bug: `apiauth::token` returns a
/// `secret::Secret`, whose whole purpose is that `{t}` prints `<secret>`. Converting the function
/// without converting the two call sites that build this URL compiles clean, passes every type
/// check, and ships a cockpit link that cannot open the cockpit — a failure with no compiler and no
/// panic behind it, only a person pasting a URL and being refused.
///
/// So it asserts against the bytes on disk, not against a shape: a regex for "looks like a token"
/// would be satisfied by anything, and `!= "<secret>"` would be satisfied by the next placeholder.
/// Then it spends the token, because a token that is printed correctly and does not authenticate is
/// the same outcome for the person holding it.
#[test]
fn a_printed_cockpit_url_carries_a_token_that_opens_the_api() {
    let home = token_home("printed");
    // The URL is printed with the address the socket is actually on, which with a handed socket is
    // the one this test opened rather than one `$SKEIN_ADDR` asked for (`src/bin/skein-server/main.rs`,
    // "the address printed below has to be the one a browser can reach"). So the line read below is
    // checked against the port the requests below go to, and not against a number both sides took
    // on trust.
    let (mut child, addr) = serving(
        skein_server()
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_REGISTRY", home.to_path_buf().join("registry.json"))
            .env_remove("SKEIN_NO_API_AUTH")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    // Taken after `serving` has seen a response, so the line is already written: the server prints
    // it as soon as it has the listener, which is before the accept loop it answered on.
    let mut out = child.stdout.take().unwrap();
    // Read only what has been written; the process stays up, so `read_to_end` would block for ever.
    let mut buf = vec![0u8; 4096];
    let n = std::io::Read::read(&mut out, &mut buf).unwrap();
    let printed = String::from_utf8_lossy(&buf[..n]).into_owned();
    let _kid = Kid(child);

    let carried = printed
        .split("?t=")
        .nth(1)
        .unwrap_or_else(|| panic!("no `?t=` in what the server printed: {printed}"))
        .trim()
        .to_string();
    let on_disk = std::fs::read_to_string(home.to_path_buf().join("api-token"))
        .expect("the server minted no token file");
    assert_eq!(
        carried,
        on_disk.trim(),
        "the printed URL does not carry the fleet's token, so the cockpit link is dead: {printed}"
    );

    // And it is a credential, not just a matching string.
    let raw = format!(
        "GET /api/boxes HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {carried}\r\n\
         Connection: close\r\n\r\n"
    );
    let (st, _) = send(&addr, "GET /api/boxes", raw.as_bytes());
    assert_eq!(st, 200, "the token in the printed URL was refused");
}

/// **`store` is refused over HTTP and still accepted from the CLI** (SKEIN-535).
///
/// `add_repo` takes `store` as an arbitrary host path and uses it as one: `ensure_store`
/// (`src/kit.rs`) scaffolds a whole `.claude` tree — `README.md`, `mailbox/`, `skein/bin/`,
/// `telemetry/`, a dozen more — wherever it points, and its only guard is that the path is
/// absolute. Over HTTP that wrote that tree anywhere on the host for anyone holding the API token,
/// which is printed into every cockpit URL.
///
/// **Both halves in one test, because each one alone is satisfiable by a wrong fix.** Refusing the
/// store inside `add_repo` would close the route and break `skein add --store`, which is a real and
/// wanted use of an absolute path; leaving the route alone keeps the hole. Only the pair pins the
/// refusal to the route, which is the one place that knows which caller it is talking to.
///
/// Each half goes through its own real front door — the wire for the route, the actual `skein`
/// binary for the CLI — so neither is a call to a library function standing in for the thing.
///
/// **Asserted on the scaffold rather than on a message.** The bug is a directory tree appearing on
/// disk, so that is what is measured on both sides: the same path, refused into non-existence over
/// HTTP and created by the CLI.
#[test]
fn a_store_outside_skein_home_is_refused_over_http_and_still_accepted_from_the_cli() {
    let home = token_home("storeguard");
    // Beside `$SKEIN_HOME`, not under it — the shape the reproduction on 2026-09-05 used
    // (`/tmp/outside/evilstore`), and the shape `--store ~/thing-shared/.claude` has.
    let outside = home
        .to_path_buf()
        .parent()
        .unwrap()
        .join(format!("skein-it-storeguard-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);

    // Registrable (so `registrable_source` lets it past) and unconnectable (so the mirror clone
    // fails on a refused connection instead of a DNS lookup or a real network round trip). Both
    // halves use the same one, so what differs between them is the store and nothing else.
    //
    // The clone failing is expected and is not what either half measures: `add_repo` runs
    // `ensure_store` BEFORE `ensure_mirror` (`src/repos/add.rs`), so the store is on disk — or refused
    // — well before the remote is ever reached.
    const SOURCE: &str = "https://127.0.0.1:1/storeguard.git";

    let (child, addr) = serving(
        skein_server()
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_REGISTRY", home.to_path_buf().join("registry.json"))
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);
    let json = "Content-Type: application/json\r\n";

    // ── over HTTP: refused, and nothing written ──────────────────────────────────────────────
    let (code, why) = http_post(
        &addr,
        "/api/repos",
        json,
        format!(
            r#"{{"source":"{SOURCE}","id":"storeguard","store":"{}"}}"#,
            outside.display()
        )
        .as_bytes(),
    );
    assert_eq!(
        code, 400,
        "POST /api/repos did not refuse a `store` outside $SKEIN_HOME: {why}"
    );
    // The assertion the whole item is about. It is checked before the wording below because a
    // refusal that still scaffolds the tree is the bug, whatever it says.
    assert!(
        !outside.exists(),
        "the HTTP route scaffolded a store outside $SKEIN_HOME at {} — this is SKEIN-535 itself, \
         and the status code above says nothing about it",
        outside.display()
    );
    // Refused loudly, and pointing at the way through. A 400 that only says "no" leaves the person
    // who really does want to adopt a store with nowhere to go.
    assert!(
        why.contains("skein add") && why.contains("--store"),
        "the refusal does not name the CLI as the way to adopt a store, so it is a dead end: {why}"
    );

    // ── from the CLI: the same path, accepted ────────────────────────────────────────────────
    // `skein add <url> --store <path>` — the exact line `cmd_add` serves (`src/bin/skein.rs`).
    let out = Command::new(env!("CARGO_BIN_EXE_skein"))
        .args([
            "add",
            SOURCE,
            "--id",
            "storeguard",
            "--store",
            outside.to_str().unwrap(),
        ])
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        .env("SKEIN_REGISTRY", home.to_path_buf().join("registry.json"))
        .env_remove("SKEIN_SHARED")
        .output()
        .unwrap();
    // Two entries `ensure_store` itself writes, so this reads the scaffold rather than the exit
    // status — the command as a whole fails at the unreachable remote, by construction, and that
    // failure is downstream of everything being measured here.
    assert!(
        outside.join("README.md").is_file() && outside.join("mailbox").is_dir(),
        "`skein add --store {}` no longer provisions a store outside $SKEIN_HOME — the CLI half of \
         SKEIN-535 has been broken by fixing the HTTP half in `add_repo` instead of at the route.\n\
         stdout: {}\n  stderr: {}",
        outside.display(),
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim(),
    );

    let _ = std::fs::remove_dir_all(&outside);
}

/// **A server that accepts and never answers fails a request helper instead of hanging it**
/// (SKEIN-817).
///
/// Every `http_get` and `http_post` in this suite goes through [`send`], and its read had no
/// ceiling: a server wedged mid-test, after it had answered once, held the whole binary for ever,
/// and a hang names nothing. So this stands up exactly that server — a listener whose connections
/// are accepted and then left alone — and requires the panic the retry path already writes, naming
/// the request and the socket error, well inside what a hang would be.
///
/// **What makes it fail**: take `set_read_timeout` back out of `send_once`. The call below then
/// blocks in `read_to_end` for as long as the listener lives, and the watchdog panics with the
/// sentence below instead.
#[test]
fn a_server_that_accepts_and_never_answers_fails_the_request_instead_of_hanging_it() {
    let quiet = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let addr = quiet.local_addr().unwrap().to_string();
    // Accepted and held, so the connection is as established as a real server's would be: what is
    // missing is only the answer.
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in quiet.incoming().flatten() {
            held.push(s);
        }
    });
    let (done, said) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let got = std::panic::catch_unwind(|| {
            send_within(
                &addr,
                "GET /quiet",
                b"GET /quiet HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
                Duration::from_millis(200),
            )
        });
        let _ = done.send(got.map_err(|p| {
            p.downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "a panic with no message".into())
        }));
    });
    // Three tries of 200ms plus 600ms of backoff is under two seconds; thirty is only the point at
    // which "slow" has become "hung".
    let outcome = said.recv_timeout(Duration::from_secs(30)).expect(
        "the request to a server that never answers was still waiting after 30s — it hangs",
    );
    let message = match outcome {
        Ok(got) => panic!("a server that never wrote a byte produced a response: {got:?}"),
        Err(message) => message,
    };
    assert!(
        message.contains("GET /quiet") && message.contains("failed 3 times"),
        "the failure does not name the request it was making: {message}"
    );
}
