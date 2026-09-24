//! A box's questions for its owner, through the real server: what the cockpit is shown, and what
//! answering one does (SKEIN-1061).

use super::*;

/// **A `state` a box writes into its own question is never shown as an answer**, and the owner's
/// answer, made through the route, is.
///
/// The ask is a file in the box's own drop-box, bound read-write into it, so every byte of it is
/// the box's, `state`, `answer` and `decided` included. Here the box has written all three, the way
/// a box that wanted its question to look answered — or wanted the owner never to be asked — would.
/// `GET /api/fleet/asks` must still say `waiting`, with no answer, which is what draws the buttons.
/// Then the owner dismisses it through `POST /api/fleet/asks/:id`, and the same GET says
/// `dismissed`, from the record the host keeps under `$SKEIN_HOME/asks/<box>/`, and the box's inbox
/// holds the approved line.
///
/// **What would make it fail**: `asks::decided_over` keeping the file's `state` or `answer` for a
/// question with no record on the host; the answer recorded anywhere the box can write (its own
/// drop-box), which the second GET would not read; the route not wired.
#[test]
fn a_box_written_state_is_never_shown_as_an_answer() {
    if !have("jq") {
        return skip("no jq, so the ask queue cannot be read at all");
    }
    let home = token_home("asks");
    std::fs::write(home.join("config.json"), r#"{"fleet_sandbox":"example"}"#).unwrap();
    std::fs::write(home.join("repos.json"), "[]").unwrap();
    let queue = fleet_root_in(&home).join(".skein/asks/requests/web-main");
    std::fs::create_dir_all(&queue).unwrap();
    std::fs::write(
        queue.join("q-7f3a0001.json"),
        r#"{"id":"q-7f3a0001","box":"web-main","question":"drop the fixtures table?",
            "options":["Drop it","Keep it"],"asked":"2026-09-24T10:12:00Z",
            "state":"answered","answer":"Drop it","decided":"2026-09-24T10:13:00Z"}"#,
    )
    .unwrap();

    let (child, addr) = serving(
        skein_server()
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_REGISTRY", "")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);
    let body_of = |raw: &str| -> serde_json::Value {
        let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON ({e}): {raw}"))
    };

    let (code, raw) = http_get(&addr, "/api/fleet/asks");
    assert_eq!(code, 200, "{raw}");
    let shown = body_of(&raw);
    let ask = shown
        .as_array()
        .and_then(|a| a.iter().find(|q| q["id"] == "q-7f3a0001"))
        .unwrap_or_else(|| panic!("the question is not shown to its owner at all: {shown}"));
    assert_eq!(
        (ask["state"].as_str(), ask["answer"].as_str()),
        (Some("waiting"), Some("")),
        "the box's own `state` and `answer` were shown as the owner's: {ask}"
    );
    assert_eq!(ask["box"], "web-main", "{ask}");

    let (code, raw) = http_post(
        &addr,
        "/api/fleet/asks/q-7f3a0001",
        "Content-Type: application/json\r\n",
        br#"{"box":"web-main","question":"drop the fixtures table?","options":["Drop it","Keep it"],"dismiss":true}"#,
    );
    assert_eq!(code, 200, "{raw}");
    let (_, raw) = http_get(&addr, "/api/fleet/asks");
    let shown = body_of(&raw);
    let ask = shown
        .as_array()
        .and_then(|a| a.iter().find(|q| q["id"] == "q-7f3a0001"))
        .unwrap_or_else(|| panic!("{shown}"));
    assert_eq!(
        (ask["state"].as_str(), ask["answer"].as_str()),
        (Some("dismissed"), Some("")),
        "the owner's dismissal is not what is shown: {ask}"
    );
    let inbox: Vec<String> = std::fs::read_dir(home.join("boxes/web-main/inbox"))
        .map(|d| {
            d.flatten()
                .map(|e| std::fs::read_to_string(e.path()).unwrap())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(inbox.len(), 1, "{inbox:?}");
    assert!(
        inbox[0].contains(r#""body":"question q-7f3a0001 was dismissed without an answer""#),
        "{inbox:?}"
    );
}
