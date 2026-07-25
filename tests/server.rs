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

/// One request over a fresh `Connection: close` socket → (status, full raw response incl. headers).
fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
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

/// One POST with a raw body + headers → (status, full raw response).
fn http_post(addr: &str, path: &str, headers: &str, body: &[u8]) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(
        format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n",
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

    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_REGISTRY", &reg)
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

    let (st, body) = http_get(&addr, "/vendor/xterm.js");
    assert_eq!(st, 200);
    assert!(body.contains("application/javascript"));

    let (st, body) = http_get(&addr, "/api/boxes");
    assert_eq!(st, 200);
    assert!(body.contains("thing-a"));

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
            "POST /api/boxes/thing-a/upload HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
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
