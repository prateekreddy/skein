//! **The board, the session API and the handoff brief say the same thing about a box's turn.**
//!
//! Turn state has two halves (`signals.rs`'s module note): the hook edge, which nothing clears when
//! a prompt is answered, and the level observation of the box's own screen, which clears itself.
//! `signals::fuse_status` corrects the first with the second, and until `signals::turn_state` was
//! the one reader only the board applied it. `digest::session_digest` read the edge alone, so
//! `/api/boxes/:name/session` (which serves that digest as it is) and the handoff brief (which
//! copies its `state` into the brief a replacement box starts from) could say `needs-input` while
//! the board's row for the same box said `working`.
//!
//! The fixture is exactly that case: an edge that still says `needs-input` from before the prompt
//! was answered, and a fresh screen, observed after it, of the agent busy again.

mod common;

use common::{env_pins, Scratch};
use std::fs;

const FLEET: &str = "agree-fleet";
const NAME: &str = "thing-agree";

/// A real capture of a box mid-turn, the same one `tests/pane_attribution.rs` uses, so what the
/// screen half says is a genuine observation rather than a string this test wrote to classify.
const BUSY: &str = include_str!(
    "fixtures/panes/claude-waiting.gadget-optimize-AI.working-series-a.2026-08-25.txt"
);

#[test]
fn the_board_the_session_digest_and_the_handoff_agree_about_a_box_whose_prompt_was_answered() {
    let root = Scratch::boxes("skein-agree");
    let status = root.join("status");
    fs::create_dir_all(&status).unwrap();

    let mut pins = env_pins();
    pins.set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .unset("SANDBOX_VM_ID")
        .unset("SKEIN_SELF");
    // `store_for_box` falls back to the registry's own directory for a box in no managed repo,
    // which is where the status files below go (the recipe in tests/board_cost.rs). The registry's
    // own `status` is empty so the only answer either reader can give comes from `status/`.
    let reg = root.join("sandboxes.json");
    fs::write(
        &reg,
        format!(r#"{{"{NAME}":{{"branch":"main","dir":"/nowhere","lastSeen":"1","status":""}}}}"#),
    )
    .unwrap();
    pins.set("SKEIN_REGISTRY", &reg);

    let mut config = skein::config::load_config();
    config.fleet_sandbox = FLEET.into();
    skein::config::save_config(&config).expect("turn the fleet on");
    skein::place::record_place(
        NAME,
        &skein::place::PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: 1,
            home: format!("/boxes/{NAME}/home"),
            tree: format!("/boxes/{NAME}/tree"),
            sock: format!("/boxes/{NAME}/session.sock"),
            ..Default::default()
        },
    )
    .expect("place a box");

    let now = chrono::Utc::now();
    // The edge: the Notification hook's `needs-input`, written ten minutes ago and never cleared,
    // because no runtime fires anything when a prompt is answered.
    fs::write(
        status.join(format!("{NAME}.json")),
        serde_json::json!({
            "status": "needs-input",
            "ts": (now - chrono::Duration::minutes(10)).to_rfc3339(),
            "box": NAME,
        })
        .to_string(),
    )
    .unwrap();
    // The level: the screen, sampled now, showing the agent working again.
    let secs = now.timestamp();
    fs::write(
        status.join(format!("{NAME}.pane.json")),
        serde_json::json!({
            "contract": 1,
            "ts": secs,
            "activity": secs,
            "age": 1,
            "moving": 1,
            "dead": 0,
            "session": "skein-agent",
            "box": NAME,
            "title": format!("_ {NAME}"),
            "title_age": 46923,
            "cmd": "node",
            "tail": BUSY.lines().map(|l| l.replace('\t', " ")).collect::<Vec<_>>(),
        })
        .to_string(),
    )
    .unwrap();

    let views = skein::board::load_views().expect("a board tick");
    let board = views
        .iter()
        .find(|v| v.name == NAME)
        .unwrap_or_else(|| panic!("{NAME} is not on the board"));
    // The control: the fused answer really is `working`, so the agreement below is agreement on
    // the corrected state and not on the stale edge.
    assert_eq!(
        board.state, "working",
        "the fixture must reach the board as an answered prompt, or nothing here is being tested"
    );

    // `/api/boxes/:name/session` is `session_digest` serialised as it is (skein-server's
    // `api_session`), so this is the value that route answers with.
    let digest = skein::digest::session_digest(NAME).expect("a digest for a known box");
    let served = serde_json::to_value(&digest).unwrap();
    assert_eq!(
        served["state"], board.state,
        "the session API said {:?} while the board said {:?} about the same box: the digest read \
         the hook edge alone instead of the fused turn state",
        served["state"], board.state
    );

    let brief =
        skein::handoff::prepare_handoff(NAME, Some("claude"), "codex").expect("a handoff brief");
    let text = fs::read_to_string(&brief).unwrap();
    let line = format!("- state: `{}`", board.state);
    assert!(
        text.contains(&line),
        "the handoff brief must carry the board's state ({line:?}), and a replacement box would \
         start from a prompt that was already answered otherwise:\n{text}"
    );
}
