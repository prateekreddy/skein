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
}
