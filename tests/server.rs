//! Black-box smoke test: launch the real `skein-server` binary and exercise the HTTP surface.
//! Catches route-wiring, the include_str! UI, vendored assets, and the :name path-traversal guard
//! — the layers a pure unit test can't see.

mod common;

use common::{fake_github, have, skip, Scratch};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
fn token_home(tag: &str) -> Scratch {
    let dir = doorway_stopped(Scratch::temp(&format!("skein-it-{tag}")));
    std::fs::write(dir.join("api-token"), API_TOKEN).unwrap();
    dir
}

/// **A spawned server starts a supervisor that is not its child, and `Kid` cannot take it with it**
/// (SKEIN-765).
///
/// `main` runs `fleet::heal_fleet`, which runs `ensure_fleet_door` → `start_server`, which is a
/// `tmux new-session` holding `while [ -f <root>/.skein/server-doorway.py ]; do python3 … ; sleep
/// 2; done`. Three processes per spawned server — the tmux server, the shell, and whichever python
/// the loop is on — none of them descended from the `skein-server` this file kills. Twelve tests
/// left better than thirty behind on a run where every one of them passed, and
/// `node tests/ui/harness/leaks.mjs` — the check `CLAUDE.md` tells every agent to trust after a
/// run — exited 1 naming `skein-it-`, `skein-settings-it` and `skein-repos-it`.
///
/// **Removing the directory is not what stops them, and relying on it is the SKEIN-645 shape.**
/// `Scratch` keeps the directory when the thread is panicking, and the doorway script inside it is
/// the loop's own exit condition — so on the one path where a leak costs the most, the loop
/// restarts a python every two seconds for as long as the evidence is kept. On the passing path it
/// does stop, about two seconds late, which is late enough for the gate to fail and for the next
/// run's count to be wrong.
///
/// So this rides on `Scratch::quiesce_with`, which runs on **every** way out, panic included, and
/// only the removal is conditional — the same argument `tests/fleet_move.rs`'s `scratch()` makes,
/// and the same one `quiesceOnExit` makes for the node tier.
///
/// It takes an already-built `Scratch` rather than building one, so the `Scratch::temp("…")` and
/// its literal prefix stay at the call site: `tests/ui/harness/leaks.mjs` reads the fixture names
/// it checks for out of exactly that shape (`rustPrefixes`), and a helper that swallowed the
/// literal would delete this file's three names from the gate that catches this defect.
fn doorway_stopped(dir: Scratch) -> Scratch {
    dir.quiesce_with(|home| stop_doorway(&fleet_root_in(home)))
}

/// Stop the supervisor under `root`, leaving the directory alone.
///
/// **The order is load-bearing**, and it is `tests/fleet_move.rs`'s: the doorway script first,
/// because it is the loop's own exit condition; then the tmux server; then a beat for it to go.
/// Killing tmux while the script is still on disk leaves the restart condition true for anything
/// that supervises the supervisor.
///
/// Not `tmux has-session` to decide whether to bother, and not as an assertion anywhere: the socket
/// lives *inside* `root`, so a missing socket answers "No such file or directory", which reads as
/// "already stopped" whether it is true or not — the exact hole
/// `a_supervisor_whose_fleet_is_gone_stops_rather_than_restarting_for_ever` fell into. The socket's
/// existence gates only the sleep, which costs nothing to skip when nothing was ever started.
fn stop_doorway(root: &Path) {
    let skein = root.join(".skein");
    let _ = std::fs::remove_file(skein.join("server-doorway.py"));
    let sock = skein.join("server.tmux");
    if !sock.exists() {
        return;
    }
    let _ = Command::new("tmux")
        .args(["-S", &sock.to_string_lossy(), "kill-server"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    std::thread::sleep(Duration::from_millis(250));
}

/// Every process whose command line names `root`, by pid.
///
/// Command lines and not environments, unlike `tests/ui/harness/leaks.mjs`, and the difference is
/// the subject: what SKEIN-687 could not see was the `skein-server` itself, which is exec'd as a
/// bare binary path and carries its fixture only in `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT`. The
/// supervisor is the opposite — its whole loop, doorway path included, *is* its argv — and the
/// server is this file's own child, taken by `Kid`. `ps` rather than `/proc` so this needs no
/// `#[cfg(target_os = "linux")]`, which would be a gate to declare in `tests/platform_gates.rs`
/// for a question both kernels answer.
fn naming(root: &Path) -> Vec<String> {
    let needle = root.to_string_lossy().into_owned();
    let out = Command::new("ps")
        .args(["-eo", "pid=,args="])
        .output()
        .expect("ps ran");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(&needle))
        .map(|l| l.split_whitespace().next().unwrap_or("?").to_string())
        .collect()
}

/// The fleet root a spawned `skein-server` is pinned at: a directory inside the fixture's own
/// scratch, made by the server itself if it wants one.
///
/// **Every spawn in this file needed it and none of them had it.** `main` calls
/// `probes::ensure_probe_all`, `fleet::ensure_fleet_kit` and `fleet::heal_fleet` before it binds a
/// port, and those resolve `util::fleet_root()` — which fell through to `"/boxes"`, the owner's
/// LIVE fleet on any machine running skein, and `heal_fleet` writes there rather than only reading
/// (SKEIN-685, an instance of SKEIN-530's class). `cargo test --all` runs this file every time.
///
/// `util::fleet_root` refuses an unpinned test process now (SKEIN-690), and the child inherits
/// `$SKEIN_TEST` from this binary — `.cargo/config.toml`'s `[env]` table puts it here and
/// `Command` passes the environment on — so a spawn that forgets this dies at the pin with a
/// message naming the variable, instead of quietly healing somebody's fleet. That is asserted
/// both ways in `a_server_heals_the_fleet_root_it_was_given_and_refuses_when_given_none`.
///
/// Under `$SKEIN_HOME` rather than beside it, because
/// `a_request_string_that_becomes_a_path_cannot_climb_out_of_skein_home` asserts that nothing is
/// created above the home, and a sibling fleet root would be a fixture breaking that test's own
/// premise.
/// `impl AsRef<Path>` rather than `&Scratch` so [`doorway_stopped`]'s callback, which is handed a
/// bare `&Path`, derives the root the same way every spawn does. Two spellings of "the fleet root
/// is `<home>/fleet`" is two places for it to stop agreeing, and the teardown would be the copy
/// that went wrong silently.
fn fleet_root_in(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join("fleet")
}

/// How many times a request is tried, and the pause before try n+1 (`BACKOFF * n`).
///
/// SKEIN-173: under a loaded machine one run died inside `http_post` — `read_to_end` hit a dropped
/// connection, the bare `unwrap` panicked, and the suite failed on a socket accident instead of an
/// assertion (707/707 green on re-run). A dropped connection is not what any test here tests, so
/// the helpers retry it a bounded number of times; what they must never do, on any path, is fail
/// without naming the request that died and the socket error that killed it — an anonymous panic
/// teaches people to re-run instead of read.
const TRIES: u32 = 3;
const BACKOFF: Duration = Duration::from_millis(200);

/// Send one raw request on a fresh socket and read to EOF. Every socket-level failure comes back
/// as `Err` — including a response so truncated it has no status line, which is the same dropped
/// connection wearing a different face — so the caller can retry all of them the same way.
fn send_once(addr: &str, raw: &[u8]) -> std::io::Result<(u16, String)> {
    let mut s = TcpStream::connect(addr)?;
    s.write_all(raw)?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| {
            std::io::Error::other(format!("no status line in a {}-byte response", buf.len()))
        })?;
    Ok((status, text))
}

/// The retry wrapper under `http_get` and `http_post`. `what` names the request being made
/// ("GET /api/boxes") and appears in the panic, with the last socket error, when every try failed.
fn send(addr: &str, what: &str, raw: &[u8]) -> (u16, String) {
    let mut last = None;
    for attempt in 1..=TRIES {
        match send_once(addr, raw) {
            Ok(got) => return got,
            Err(e) => last = Some(e),
        }
        if attempt < TRIES {
            std::thread::sleep(BACKOFF * attempt);
        }
    }
    panic!(
        "{what} to {addr} failed {TRIES} times; last error: {}",
        last.unwrap()
    );
}

/// One request over a fresh `Connection: close` socket → (status, full raw response incl. headers).
fn http_get(addr: &str, path: &str) -> (u16, String) {
    let raw = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\
         Connection: close\r\n\r\n"
    );
    send(addr, &format!("GET {path}"), raw.as_bytes())
}

/// One GET that reads for at most `patience` — for a stream, which never closes.
fn http_get_for(addr: &str, path: &str, patience: Duration) -> (u16, String) {
    let raw =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\r\n");
    let mut last = None;
    for attempt in 1..=TRIES {
        match stream_once(addr, raw.as_bytes(), patience) {
            Ok(got) => return got,
            Err(e) => last = Some(e),
        }
        if attempt < TRIES {
            std::thread::sleep(BACKOFF * attempt);
        }
    }
    panic!(
        "GET {path} (stream) to {addr} failed {TRIES} times; last error: {}",
        last.unwrap()
    );
}

/// One attempt at a stream read. `Err` only when the connection died before a single byte arrived
/// — the retryable shape. Once bytes are in hand they are returned whatever ends the read, because
/// partial evidence in an assertion message beats a retry that throws it away.
fn stream_once(addr: &str, raw: &[u8], patience: Duration) -> std::io::Result<(u16, String)> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(patience))?;
    s.write_all(raw)?;
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
            // Patience ran out: the normal end of reading a quiet stream, not a dead socket.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break
            }
            Err(e) if got == 0 => return Err(e),
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
    Ok((status, text))
}

/// One POST with a raw body + headers → (status, full raw response).
///
/// A retried POST is re-sent whole. In the sliver where the first attempt was applied and only its
/// response was lost, the second answer reports the collision and the caller's assertion prints it
/// — a named failure, which is still strictly better than the socket panic it replaces.
fn http_post(addr: &str, path: &str, headers: &str, body: &[u8]) -> (u16, String) {
    let mut raw = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\
         Connection: close\r\nContent-Length: {}\r\n{headers}\r\n",
        body.len()
    )
    .into_bytes();
    raw.extend_from_slice(body);
    send(addr, &format!("POST {path}"), &raw)
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

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
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
    let linkroot = Scratch::temp("skein-linkcheck");
    let linked = linkroot.join("points-at-tmp");
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
    let home = token_home("starve");
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("TOKIO_WORKER_THREADS", "1") // one async worker → starvation is deterministic
        .env("SKEIN_LS_CMD", "sleep 2; echo '[]'") // every load_views() now takes ~2s
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
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
    let dir = doorway_stopped(Scratch::temp("skein-settings-it"));
    std::fs::write(
        dir.join("config.json"),
        r#"{"fleet_sandbox":"skein-fleet","fleet_memory":"26g","base_branch":"trunk"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("api-token"), API_TOKEN).unwrap();

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", dir.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&dir))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
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

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", dir.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&dir))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
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

/// The doorstep's own account of itself: room, how many are on it, how many it has turned away,
/// and the grace it is actually using.
///
/// Every one of those is a number the server computed under its own lock, which is why the flood
/// test below asks for them instead of timing things. `turned_away` in particular is incremented
/// inside `admit`, in the same critical section as the arrival that caused the eviction, so reading
/// it says what the server *did* and not whether this thread got there in time to watch.
fn doorstep(addr: &str) -> serde_json::Value {
    let (st, seen) = http_get(addr, "/api/machine/doorstep");
    assert_eq!(st, 200, "the doorstep is not being served: {seen}");
    let body = seen.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body.trim()).expect("the doorstep is JSON")
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
    let home = token_home("flood");
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
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

    // What one authenticated request costs on this box, right now, with nothing in the way. The
    // one bound below that is genuinely about elapsed time is measured against this rather than
    // against a constant: a constant wide enough for a contended box is one that no longer fails
    // when the property breaks, and a constant tight enough to catch the property fails on a box
    // that is merely busy. Taken before the flood, so it is the same machine under the same load.
    let t_base = Instant::now();
    let (st, _) = http_get(&addr, "/api/boxes");
    assert_eq!(st, 200, "the fixture's own token does not open the api");
    let base = t_base.elapsed();

    // Sockets that connect and say nothing at all — no request line, no credential. This is the
    // whole of the attack: it needs no token, because §9.4's answer is that connecting is not
    // authenticating, and that answers reading rather than exhausting.
    let room = skein::knock::ROOM;
    let flood: Vec<TcpStream> = (0..room + 16)
        .map(|_| {
            let s = TcpStream::connect(&addr).expect("the port accepts");
            // A ceiling on liveness, not the assertion. This used to be 700ms, picked to sit under
            // the grace so that a patient read could not let the deadline masquerade as an
            // eviction — which quietly made the answer depend on whether the box got round to
            // accepting eighty connections inside 700ms, and it did not when five other things
            // were building on it (SKEIN-601). What tells an eviction from the deadline now is
            // `turned_away` and `knocking`, both read from the server below, so this number only
            // has to outlast a scheduler hiccup.
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            s
        })
        .collect();

    // **Wait for the server to say so, rather than for a clock.** `TcpStream::connect` returns as
    // soon as the kernel has the connection in the listen backlog, which says nothing whatever
    // about the server having accepted it; on a loaded box the accept loop is exactly what falls
    // behind. Polling the count the server keeps takes that out of the assertion: the loop below
    // ends on a fact, and its ceiling is only there so a server that evicts nobody fails in
    // seconds instead of hanging.
    let waited = Instant::now();
    let door = loop {
        let door = doorstep(&addr);
        if door["turned_away"].as_u64().unwrap_or(0) >= 16 {
            break door;
        }
        assert!(
            waited.elapsed() < Duration::from_secs(30),
            "the room turned nobody away in {:?} — a flood that is not bounded reaches the \
             file-descriptor limit and the cockpit stops answering: {door}",
            waited.elapsed()
        );
        std::thread::sleep(Duration::from_millis(10));
    };

    // And at that same instant the rest of the flood was **still standing**. This is what lets the
    // reads below be patient: on its own a patient read proves nothing about eviction, because the
    // grace deadline closes every stranger eventually and a room that evicted nobody would pass
    // it. `knocking` being near the room's size says the deadline has not started closing anyone,
    // so an end-of-stream here can only have come from the arrivals after them.
    let standing = door["knocking"].as_u64().unwrap_or(0);
    assert!(
        standing >= (room / 2) as u64,
        "only {standing} strangers were left on the doorstep when the evictions were counted, so \
         the grace deadline had already begun closing them and an eviction can no longer be told \
         from a timeout: {door}"
    );

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

    // The other end of the same control: the flood is still on the doorstep *after* those sixteen
    // end-of-streams were read. Had the grace deadline been what closed them, this would be near
    // zero — so this pair is the whole of "immediately, by the arrivals after them", and no part
    // of it is a duration.
    let after = doorstep(&addr);
    let still = after["knocking"].as_u64().unwrap_or(0);
    assert!(
        still >= (room / 2) as u64,
        "only {still} strangers were still standing once the sixteen evictions had been read \
         back, so those end-of-streams cannot be told apart from the grace deadline firing: \
         {after}"
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
    // Scaled from the baseline above, and it has to be: the flat five seconds this replaced was
    // **longer than the grace**, so a request that really had been queued behind the flood — which
    // comes back when the deadline releases them, one grace period later — passed it. Eight times
    // an idle request absorbs the scheduling noise of a busy box; half a grace period of headroom
    // keeps the bound underneath the wait it exists to catch.
    let grace = Duration::from_secs(door["grace_secs"].as_u64().unwrap_or(0));
    let bound = base * 8 + grace / 2;
    assert!(
        t0.elapsed() < bound,
        "an authenticated request took {:?} while the flood held the door, against {bound:?} — \
         the same request took {base:?} with the doorstep empty, and the grace is {grace:?}, which \
         is what a request queued behind the strangers would have waited",
        t0.elapsed()
    );

    // And the flood is **visible**, which is the other half of harmless. `knock` keeps the cockpit
    // answering, and that is exactly what would leave a flood showing up as "the board felt slow
    // once" with nothing to look at. Read back above rather than here, because the evictions are
    // now what this test waits on rather than something it checks at the end.
    assert!(
        door["turned_away"].as_u64().unwrap_or(0) >= 16,
        "the evictions are not reachable from outside the process: {door}"
    );
    assert_eq!(door["room"].as_u64(), Some(skein::knock::ROOM as u64));
    // The grace the server is really using, asserted rather than assumed. The five-second read
    // below used to stand in for this — badly, since five seconds is under the ten-second default
    // as well, so a server that ignored `SKEIN_DOORSTEP_GRACE` failed it for a reason the message
    // never named. Asserted here, the read below is free to be patient.
    assert_eq!(
        door["grace_secs"].as_u64(),
        Some(2),
        "the server is not using the grace this fixture set, so the wait below is not the \
         mechanism under test: {door}"
    );

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
    // A ceiling on liveness. That the deadline is the configured two seconds and not the
    // ten-second default is asserted from `grace_secs` above; what is left for this read to
    // establish is that the deadline closes the connection **at all**, and a server that never
    // closes it fails this however long the ceiling is.
    last.set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
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
        return skip("no python3 to stand in for the process manager");
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
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
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
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
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

/// The server says the warden is missing **at boot**, not at the first Launch.
///
/// Every other thing this server depends on is checked when it starts — the turn-state probes, the
/// kit, the fleet's launcher, the gh token, the ssh key — and each says so on stderr when it is not
/// there. The warden was the exception, so a host without one found out from a 500 after pressing a
/// button, which on an upgrade lands weeks after the change that caused it with nothing pointing
/// back.
///
/// **And it must not be fatal.** A fleet that already exists runs perfectly well without a warden;
/// what it cannot do is be created or resized. Refusing to start over a capability somebody may not
/// use today would be the wrong trade — so this asserts both halves: it complains, and it serves.
#[test]
fn the_server_says_at_boot_when_no_warden_is_answering() {
    let home = token_home("nowarden");
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    // A port with nothing on it, so "no warden" is a state this actually reaches rather than one it
    // inherits from whatever the machine happens to be running.
    let quiet = free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        .env("SKEIN_WARDEN", format!("127.0.0.1:{quiet}"))
        .env("SKEIN_REGISTRY", "")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the server");

    // It serves. The complaint is a complaint, not a refusal.
    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the server never bound, so 'it still serves' was never actually asked"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let (code, _) = http_get(&addr, "/");
    assert_eq!(code, 200, "the server did not come up without a warden");

    let _ = child.kill();
    let out = child
        .wait_with_output()
        .expect("collect the server's output");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains("warden"),
        "starting with no warden said nothing about it:\n{said}"
    );
    // The address it asked, because a wrong port is the likeliest cause and the reader cannot check
    // a number nothing printed.
    assert!(
        said.contains(&quiet.to_string()),
        "it did not say where it looked:\n{said}"
    );
}

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

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("SKEIN_GITHUB_API", &api)
        .env("GH_TOKEN", "test-token")
        .env("SKEIN_REGISTRY", "")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _kid = Kid(child);
    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "server never bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

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

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("SKEIN_REGISTRY", home.to_path_buf().join("registry.json"))
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
    let addr = format!("127.0.0.1:{}", free_port());
    let mut child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("SKEIN_REGISTRY", home.to_path_buf().join("registry.json"))
        .env_remove("SKEIN_NO_API_AUTH")
        .env_remove("SKEIN_SHARED")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut out = child.stdout.take().unwrap();
    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "server never bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
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

/// **The fleet root a spawned server was given is the one it writes into — and given none, it
/// refuses to start at all.**
///
/// This is SKEIN-685 as a test, and it needs both halves. `skein-server`'s `main` runs
/// `fleet::heal_fleet()` before it binds a port, and that writes: measured against a fixture root,
/// five files land in `<root>/.skein/` — `box-session.sh`, `server.tmux`, `server-doorway.py`,
/// `git-credential-skein`, `skein-startup.sh`. Every spawn in this file passed `$SKEIN_HOME` and
/// not `$SKEIN_FLEET_ROOT`, so `util::fleet_root()` fell through to `/boxes` and those five went
/// to the LIVE fleet's own `.skein` — the launcher every real box starts through — rewritten from
/// whatever was in the tree, on every `cargo test --all`.
///
/// The second half is a *process*, not a thread, and that is the point: the guard keys on
/// `$SKEIN_TEST`, which reaches an integration binary from `.cargo/config.toml`'s `[env]` table
/// and reaches the child because `Command` passes this process's environment on. A suite that only
/// ever exercises the pinned path proves nothing about what an unpinned one does.
///
/// **What makes it fail**, both run rather than reasoned about: deleting the `assert!` from
/// `util::fleet_root` lets the second child bind and reach the `assert!(!status.success())`;
/// dropping the `.env("SKEIN_FLEET_ROOT", …)` from the first half turns it into the second and the
/// launcher assertion fires.
#[test]
fn a_server_heals_the_fleet_root_it_was_given_and_refuses_when_given_none() {
    // ── given a root: what heal_fleet writes lands in it ──
    let home = token_home("fleet-root");
    let root = fleet_root_in(&home);
    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", &root)
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("SKEIN_REGISTRY", "")
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
    let launcher = root.join(".skein/box-session.sh");
    assert!(
        launcher.is_file(),
        "the server bound and wrote no launcher at {} — if `heal_fleet` no longer writes one, this \
         test is aimed at a mechanism that has moved, and the pin it justifies has to be argued \
         again rather than quietly dropped",
        launcher.display()
    );

    // ── given none: it refuses, before the port and before any write ──
    let bare = token_home("fleet-root-bare");
    let out = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", format!("127.0.0.1:{}", free_port()))
        .env("SKEIN_HOME", bare.path())
        .env_remove("SKEIN_FLEET_ROOT")
        .env_remove("SKEIN_SHARED")
        .output()
        .expect("the server binary ran");
    assert!(
        !out.status.success(),
        "a server started with no fleet root and lived — which means it resolved one, and the only \
         one it can resolve unasked is /boxes"
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("SKEIN_FLEET_ROOT"),
        "the refusal does not name the variable to set, so it tells whoever hit it nothing: {said}"
    );
    assert!(
        !bare.join("fleet").exists(),
        "the refusal came after something had already been written"
    );
}

/// **The teardown stops the supervisor; removing the directory is not what stops it** (SKEIN-765).
///
/// The distinction is the whole test, and it is the one a leak count taken after a *passing* run
/// cannot make. On the passing path `Scratch` removes the fixture, the doorway script goes with it,
/// and the loop notices within its two-second sleep — so "the processes are gone a moment later"
/// is true whether or not [`stop_doorway`] does anything at all. On the failing path the directory
/// is KEPT, deliberately, because it is the only evidence a failure leaves (`tests/common/mod.rs`)
/// — and then the loop's exit condition is kept with it and the supervisor runs for ever. That is
/// SKEIN-645: a kept fixture restarted a python every two seconds for nine hours.
///
/// So this asserts against a fixture that is still there — `.skein` is checked for on the line
/// before — which is exactly the shape of the panic path, without needing a panic to produce it.
///
/// **What makes each assertion fail**, run rather than reasoned about:
///
/// * presence, before: the supervisor starts inside `heal_fleet`, which runs before the bind, so a
///   bound server that has none means the mechanism this teardown is aimed at has moved.
/// * presence, after `Kid`: making `Kid::drop` also stop the doorway would empty it — and that is
///   the belief this whole item corrects, that killing the server is enough.
/// * absence, after `stop_doorway`: pointing its `kill-server` at `server.tmux.wrong` leaves the
///   loop mid-`sleep 2` and three processes alive. Removing the doorway script is *not* enough on
///   its own, which is why the socket is killed as well as the script removed.
#[test]
fn the_doorway_supervisor_stops_when_the_teardown_runs_and_not_when_the_fixture_is_removed() {
    if !have("tmux") {
        return skip("no tmux, so a spawned server starts no supervisor for this to stop");
    }
    let home = token_home("doorway");
    let root = fleet_root_in(&home);
    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", &root)
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("SKEIN_REGISTRY", "")
        .env_remove("SKEIN_SHARED")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let kid = Kid(child);
    let start = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "server never bound"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let running = naming(&root);
    assert!(
        !running.is_empty(),
        "the server bound and nothing anywhere names {} — `heal_fleet` reaches `start_server` \
         before the bind, so if it no longer starts a tmux supervisor this test is aimed at a \
         mechanism that has moved, and the teardown it justifies has to be argued again rather \
         than quietly dropped",
        root.display()
    );

    // The server goes, and the supervisor does not: it is tmux's child, not this process's.
    drop(kid);
    let orphaned = naming(&root);
    assert!(
        !orphaned.is_empty(),
        "killing the spawned server emptied {} of named processes, so the supervisor IS reachable \
         from `Kid` after all — which would make this whole teardown unnecessary, and is worth \
         knowing before it is deleted",
        root.display()
    );

    stop_doorway(&root);
    assert!(
        root.join(".skein").is_dir(),
        "{} is gone, so an empty count below would be the fixture's removal rather than the \
         teardown — the one thing this test exists to tell apart",
        root.join(".skein").display()
    );
    let left = naming(&root);
    assert!(
        left.is_empty(),
        "the teardown ran against a fixture that is still on disk and left {} process(es) — \
         pid(s) {} — so on the path where the directory is KEPT, which is every failing test, \
         this supervisor restarts a python every two seconds for ever (SKEIN-645)",
        left.len(),
        left.join(", ")
    );
}
