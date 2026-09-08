//! **One box's screen, filed under another box's name, must not become that box's turn state.**
//!
//! `board::load_views` reads `<store>/status/<box>.pane.json` and hands what it finds to
//! `classify_pane` as that box's screen. The filename was the only thing asserting whose screen it
//! was, and a misfiled observation is the worst possible shape for that: well-formed, fresh, and
//! classifying perfectly, so it renders as the box's real state with nothing on the row to mark it.
//! A box that needs you reads as busy, or a busy box reads as needing you, and the board looks
//! entirely normal either way.
//!
//! The unit halves are tested where they live — `probes::tests` drives the shell probe through its
//! three identity branches, `signals::tests` covers the predicates. This is the whole path: a real
//! captured pane, written to disk under the wrong name, read back through the board a browser tab
//! actually renders. It is the one that would notice the filter being dropped from `load_views`
//! while both halves it is built from still pass.

mod common;

use common::{env_pins, Scratch};
use std::fs;
use std::path::Path;

const FLEET: &str = "attrib-fleet";
/// The box the capture is really of, and one it is not. The pair the bug was reported against.
const OWNER: &str = "gadget-demo-optimize-AI";
const OTHER: &str = "gadget-demo-refactoring";
/// A box whose observer predates the `box` field, which is every box in the fleet until it is
/// reattached.
const LEGACY: &str = "gadget-demo-directory-service";

/// A real capture: `gadget-demo-optimize-AI` mid-turn on 2026-08-25, elapsed time advancing and
/// the spinner cycling. Reused rather than hand-written because the point of the test is what a
/// genuine observation does when it is filed under the wrong name — a synthetic tail would only
/// prove that a string this test wrote classifies the way this test expected.
const BUSY: &str = include_str!(
    "fixtures/panes/claude-waiting.gadget-optimize-AI.working-series-a.2026-08-25.txt"
);

/// An observation exactly as `box-pane.sh` writes one, `box` included — or omitted, for the probe
/// that predates the field.
fn observation(status_dir: &Path, filed_as: &str, claims: Option<&str>, tail: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut obs = serde_json::json!({
        "contract": 1,
        "ts": now,
        "activity": now,
        "age": 1,
        "moving": 1,
        "dead": 0,
        "session": "skein-agent",
        // The title travels with the capture because the grammar reads it too, and it is the
        // second reason a misfiled observation looks normal: it names the box it really came from.
        "title": format!("_ {OWNER}"),
        "title_age": 46923,
        "cmd": "node",
        "tail": tail.lines().map(|l| l.replace('\t', " ")).collect::<Vec<_>>(),
    });
    if let Some(b) = claims {
        obs["box"] = serde_json::Value::String(b.to_string());
    }
    fs::write(
        status_dir.join(format!("{filed_as}.pane.json")),
        serde_json::to_string(&obs).unwrap(),
    )
    .unwrap();
}

#[test]
fn the_board_will_not_show_one_boxs_screen_as_another_boxs_turn_state() {
    let root = Scratch::boxes("skein-attrib");
    let status = root.join("status");
    fs::create_dir_all(&status).unwrap();

    // The fixture recipe from tests/board_cost.rs: the registry names the boxes, the placements
    // make them the fleet's, and `store_for_box` falls back to the registry's own directory for a
    // box that matches no managed repo — which is where the observations go.
    // Bound after `root`, so every variable stops naming it before the directory goes.
    let mut pins = env_pins();
    pins.set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        // This suite may itself be running inside a box, which the board promotes onto itself.
        .unset("SANDBOX_VM_ID")
        .unset("SKEIN_SELF");
    let reg = root.join("sandboxes.json");
    let rows: Vec<String> = [OWNER, OTHER, LEGACY]
        .iter()
        // A registry status of `waiting` is the fallback the row shows when the screen half
        // contributes nothing — so "the misfiling was refused" and "the misfiling was believed"
        // are two different visible answers, not one answer and a blank.
        .map(|n| {
            format!(
                r#""{n}":{{"branch":"main","dir":"/nowhere","lastSeen":"1","status":"waiting"}}"#
            )
        })
        .collect();
    fs::write(&reg, format!("{{{}}}", rows.join(","))).unwrap();
    pins.set("SKEIN_REGISTRY", &reg);

    let mut config = skein::config::load_config();
    config.fleet_sandbox = FLEET.into();
    skein::config::save_config(&config).expect("turn the fleet on");
    for name in [OWNER, OTHER, LEGACY] {
        skein::place::record_place(
            name,
            &skein::place::PlaceRecord {
                sandbox: FLEET.into(),
                ns_pid: 1,
                home: format!("/boxes/{name}/home"),
                tree: format!("/boxes/{name}/tree"),
                sock: format!("/boxes/{name}/session.sock"),
                generation: String::new(),
                ns_start: 0,
                launcher: String::new(),
                ceiling: String::new(),
                ..Default::default()
            },
        )
        .expect("place a box");
    }

    // One capture, written three ways.
    observation(&status, OWNER, Some(OWNER), BUSY); // correctly filed
    observation(&status, OTHER, Some(OWNER), BUSY); // OWNER's screen, under OTHER's name
    observation(&status, LEGACY, None, BUSY); // an observer that cannot say

    let views = skein::board::load_views().expect("a board tick");
    let row = |name: &str| {
        views
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("{name} is not on the board"))
    };

    // The control, and the thing that keeps the assertion below from being vacuous: this capture
    // really does read as a working box when it is the box's own.
    assert_eq!(
        row(OWNER).state,
        "working",
        "the fixture must reach the board as a live screen, or nothing here is being tested"
    );

    // The bug. Same bytes, one field different, and the row must fall back to the edge signal —
    // `waiting`, from the registry — instead of borrowing the other box's turn.
    assert_eq!(
        row(OTHER).state,
        "waiting",
        "a screen observation naming {OWNER} was rendered as {OTHER}'s turn state. The filename is \
         a claim about whose screen it is; `pane_is_ours` is what checks it, and `load_views` has \
         to apply it before `classify_pane` ever sees the observation."
    );
    assert!(
        row(OTHER).blocked_kind.is_empty(),
        "a refused observation must not carry its dialog onto the row either"
    );
    // `screen_health` is deliberately not asserted here: it returns "" for a box `box_liveness`
    // cannot confirm is Running, and these boxes are placed records without a live namespace. The
    // "misfiled" verdict itself is covered where it can be exercised directly, in
    // `probes::tests::a_screen_observation_is_filed_under_its_own_box_and_says_which`.

    // And the cost of refusing is bounded to what is actually wrong. An observation that names
    // NOBODY could not be checked — `health::Level::Unknown`, not a fault — and is still read,
    // because every box in the fleet is running such a probe until it is reattached and treating
    // "unknown" as "wrong" would take the screen half away from all of them at once.
    assert_eq!(
        row(LEGACY).state,
        "working",
        "an observation from a probe that predates the `box` field must still be read"
    );
}
