//! What skein tells the host log, and what happens when nothing is listening (§9.5 R6).
//!
//! The warden's own half — appending to a fsynced jsonl, never rewriting a line — is tested in the
//! warden crate against the real `Log`. What is tested here is the half skein owns and the half that
//! is easy to get quietly wrong: **the entry's shape on the wire**, and that a sink which is not
//! there cannot stop the thing it exists to record.
//!
//! A hand-rolled listener rather than the real warden, deliberately: building a second binary for
//! this would spend a minute of every test run to check a join that is four strings and a header.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{mpsc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// One at a time. All three of these point `$SKEIN_WARDEN` somewhere different, and cargo runs the
/// tests in one process on several threads — so without this they would take each other's warden,
/// which reads as "nothing reached the sink" and is a lie about the code.
fn alone() -> MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A warden that only listens: takes one request, hands it back, answers `code`.
fn fake_warden(code: u16) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            // One read is enough: the request is one small write from the client, and this is not
            // trying to be an HTTP server.
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let said = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = r#"{"recorded":true}"#;
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {code} Status\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = tx.send(said);
        }
    });
    (port, rx)
}

/// A `$SKEIN_WARDEN_HOME` holding a secret, which is where the client reads one from.
fn home_with_secret(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("skein-audit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("secret"), "0123456789abcdef0123456789abcdef").unwrap();
    dir
}

#[test]
fn an_act_skein_takes_on_its_own_reaches_the_log_it_does_not_own() {
    let _alone = alone();
    let (port, heard) = fake_warden(200);
    std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
    std::env::set_var("SKEIN_WARDEN_HOME", home_with_secret("said"));

    skein::warden_client::reported(
        "destroy-box-web-main",
        "destroyed a box",
        "web-main is gone",
    );

    let said = heard
        .recv_timeout(Duration::from_secs(10))
        .expect("nothing reached the sink");
    assert!(said.starts_with("POST /v1/audit "), "{said}");
    // Behind the secret like every other call: after 4c the port is not the boundary, and an audit
    // entry is a thing a box would very much like to be able to write.
    assert!(
        said.contains("x-skein-warden: 0123456789abcdef"),
        "the entry was reported without the secret: {said}"
    );
    let body = said.split("\r\n\r\n").nth(1).unwrap_or_default();
    let entry: serde_json::Value = serde_json::from_str(body).expect(body);
    assert_eq!(entry["what"], "destroyed a box");
    assert_eq!(entry["detail"], "web-main is gone");
    assert_eq!(entry["operation"], "destroy-box-web-main");
    // A claim, and recorded as one — the warden has no way to check it, and says so in its own code.
    assert_eq!(entry["reported_by"], "skein");
    // **No timestamp.** A reporter that supplies its own time supplies the order of the log, and the
    // warden stamps arrival instead.
    assert!(
        entry.get("at").is_none(),
        "the reporter chose the log's ordering: {body}"
    );
}

#[test]
fn a_sink_that_is_not_there_cannot_stop_what_it_would_have_recorded() {
    let _alone = alone();
    // A port nothing is on. This is the ordinary case for anybody running skein without a warden —
    // and the pathological one is a warden that accepts and never answers, which the read timeout
    // covers and this cannot portably arrange.
    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
    std::env::set_var("SKEIN_WARDEN_HOME", home_with_secret("dead"));

    let began = Instant::now();
    skein::warden_client::reported(
        "destroy-box-web-main",
        "destroyed a box",
        "web-main is gone",
    );
    let waited = began.elapsed();
    // It returns, and it returns quickly: an audit sink that could hold a box destroy open would be
    // a reason to stop auditing, which is the opposite of what this requirement is for.
    assert!(
        waited < Duration::from_secs(10),
        "reporting an act took {waited:?} with nothing listening"
    );
}

#[test]
fn a_sink_that_accepts_and_never_answers_is_given_up_on() {
    let _alone = alone();
    // The pathological case, and the one a refused connection does not cover: something is
    // listening, so `connect` succeeds instantly and the wait is entirely on the reply. On the
    // doer's timeout — half an hour, because a person has to read a prompt and type — this would
    // hold a box destroy open for thirty minutes to record that it happened.
    let deaf = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = deaf.local_addr().unwrap().port();
    std::thread::spawn(move || {
        // Accept and hold. Never read, never answer, never close.
        let mut held = Vec::new();
        for stream in deaf.incoming().flatten() {
            held.push(stream);
        }
    });
    std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
    std::env::set_var("SKEIN_WARDEN_HOME", home_with_secret("deaf"));

    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        skein::warden_client::reported(
            "destroy-box-web-main",
            "destroyed a box",
            "web-main is gone",
        );
        let _ = done.send(());
    });
    assert!(
        finished.recv_timeout(Duration::from_secs(30)).is_ok(),
        "reporting an act never returned against a sink that accepts and says nothing — on the \
         doer's timeout this is half an hour of a destroy held open by its own audit entry"
    );
}

#[test]
fn a_refusal_is_not_read_as_a_recorded_entry() {
    let _alone = alone();
    // `{"recorded":true}` under a 401 is a well-formed reply that says the opposite of what it
    // parses as. The client reads the code, so the caller is told — on stderr, since nothing may
    // fail — rather than believing the entry landed.
    let (port, heard) = fake_warden(401);
    std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
    std::env::set_var("SKEIN_WARDEN_HOME", home_with_secret("refused"));
    let warden = skein::warden_client::Warden::at("127.0.0.1", port);
    let answer = warden.record("op", "did a thing", "detail");
    assert!(
        answer.is_err(),
        "a refused entry was reported as recorded: {answer:?}"
    );
    let _ = heard.recv_timeout(Duration::from_secs(5));
}
