//! **The board believes the box's own engine while it is beating, and its own reading otherwise**
//! (SKEIN-1360).
//!
//! skein's mod writes `<store>/status/<box>.mod.json` from inside the agent's Claude Code session
//! (`src/plugin/hooks/attention.ts`): the state the engine is in, entered on the event itself, and
//! a `seen` heartbeat. `signals::turn_state` prefers it while `seen` is fresh and falls back to the
//! hook edge fused with the screen otherwise, which is everything a Codex box, an old CLI or an
//! account with mods off still has.
//!
//! The screen in every case below is a real capture of a box mid-turn, sampled now: the shape the
//! frozen-spinner bug left on screen while a box was in fact waiting on its person (SKEIN-321). So
//! "the board says working" is the inference, and anything else is the engine's word winning.

mod common;

use common::{env_pins, Scratch};
use std::fs;
use std::path::Path;

const FLEET: &str = "engine-fleet";
const NAME: &str = "thing-engine";

/// The same capture `tests/turn_state_agreement.rs` uses: what the screen half says is a genuine
/// observation rather than a string written to classify.
const BUSY: &str = include_str!(
    "fixtures/panes/claude-waiting.gadget-optimize-AI.working-series-a.2026-08-25.txt"
);

fn write_mod(status: &Path, signal: serde_json::Value) {
    fs::write(status.join(format!("{NAME}.mod.json")), signal.to_string()).unwrap();
}

/// What the mod writes, as `write` in attention.ts lays it out, `seen` that many seconds ago.
fn signal(state: &str, seen_ago: i64) -> serde_json::Value {
    let now = chrono::Utc::now();
    serde_json::json!({
        "box": NAME,
        "from": "mod",
        "shape": "attention-1",
        "version": "0.1.0",
        "session": "session-1",
        "state": state,
        "at": (now - chrono::Duration::seconds(seen_ago)).to_rfc3339(),
        "seen": (now - chrono::Duration::seconds(seen_ago)).to_rfc3339(),
        "detail": "Bash",
        "stop": "",
        "agent": "",
        "limit": null,
    })
}

fn row() -> skein::board::BoxView {
    skein::board::load_views()
        .into_iter()
        .find(|v| v.name == NAME)
        .unwrap_or_else(|| panic!("{NAME} is not on the board"))
}

#[test]
fn a_fresh_mod_signal_decides_the_board_and_a_stale_or_absent_one_falls_back() {
    let root = Scratch::boxes("skein-engine");
    let status = root.join("status");
    fs::create_dir_all(&status).unwrap();

    let mut pins = env_pins();
    pins.set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .unset("SANDBOX_VM_ID")
        .unset("SKEIN_SELF");
    // A box in no managed repo reports into the registry's own directory (the recipe in
    // tests/turn_state_agreement.rs), which is where `status/` is.
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
    // The hook edge: `working`, from the UserPromptSubmit that started the turn a minute ago.
    fs::write(
        status.join(format!("{NAME}.json")),
        serde_json::json!({
            "status": "working",
            "ts": (now - chrono::Duration::minutes(1)).to_rfc3339(),
            "box": NAME,
        })
        .to_string(),
    )
    .unwrap();
    // The level: the screen, sampled now, reading as a box at work.
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

    // The control: with no mod signal, the board is on its inference, and it says `working`.
    let absent = row();
    assert_eq!(
        (absent.state.as_str(), absent.status_from.as_str()),
        ("working", "screen"),
        "with no mod signal the fixture must read as the screen's `working`, or nothing below \
         tests a change"
    );

    // The engine asked the person about a tool call. The screen still says working and is newer
    // than nothing the hooks wrote, and the board flips on the first read after the write.
    write_mod(&status, signal("permission", 0));
    let asked = row();
    assert_eq!(
        (
            asked.state.as_str(),
            asked.blocked_kind.as_str(),
            asked.status_from.as_str()
        ),
        ("needs-input", "permission", "engine"),
        "a fresh mod signal of a permission wait must put the box on the needs-you tier at once"
    );
    assert_eq!(asked.tier, 0, "needs-you is tier 0");
    assert_eq!(
        asked.engine_health, "",
        "a beating mod is not reported as missing"
    );

    write_mod(&status, signal("question", 5));
    assert_eq!(row().blocked_kind, "question");

    // The turn ended with the answer: waiting on the person, where the screen still says working.
    write_mod(&status, signal("waiting", 30));
    let waiting = row();
    assert_eq!(
        (waiting.state.as_str(), waiting.status_from.as_str()),
        ("waiting", "engine")
    );

    // The model refused the turn: the person's move, and the row says why.
    let mut refused = signal("refused", 10);
    refused["stop"] = "refusal".into();
    write_mod(&status, refused);
    let row_refused = row();
    assert_eq!(
        (row_refused.state.as_str(), row_refused.status_from.as_str()),
        ("waiting", "engine")
    );
    assert_eq!(
        row_refused.headline.as_deref(),
        Some("The model refused the last turn. Reword it and try again."),
        "a refused turn must say so, or it reads like any answered one"
    );

    // The heartbeat stopped two minutes ago: the session may be gone, so its word is not believed
    // and the board is back on its own reading.
    write_mod(&status, signal("permission", 120));
    let stale = row();
    assert_eq!(
        (stale.state.as_str(), stale.status_from.as_str()),
        ("working", "screen"),
        "a mod signal past its heartbeat window must fall back to the inference"
    );

    // Fresh, but not this box's, or not a shape this build reads: no signal at all.
    let mut other = signal("permission", 0);
    other["box"] = "thing-other".into();
    write_mod(&status, other);
    assert_eq!(
        row().status_from,
        "screen",
        "another box's signal was believed"
    );
    let mut newer = signal("permission", 0);
    newer["shape"] = "attention-2".into();
    write_mod(&status, newer);
    assert_eq!(
        row().status_from,
        "screen",
        "a shape this build does not read was believed"
    );
    let mut unnamed = signal("permission", 0);
    unnamed["box"] = "".into();
    write_mod(&status, unnamed);
    assert_eq!(
        row().status_from,
        "screen",
        "a signal naming no box was believed"
    );

    // Loaded and nothing heard yet: the mod is there, and decides nothing.
    write_mod(&status, signal("loaded", 0));
    let loaded = row();
    assert_eq!(
        (loaded.state.as_str(), loaded.status_from.as_str()),
        ("working", "screen")
    );
    assert_eq!(loaded.engine_health, "");

    // A torn read of a file being rewritten is no signal for one tick, not an error.
    fs::write(
        status.join(format!("{NAME}.mod.json")),
        r#"{"box":"thing-en"#,
    )
    .unwrap();
    assert_eq!(row().status_from, "screen");
}

/// `engine_health` is what the cockpit shows as "mod not loaded": only for a running Claude box.
#[test]
fn mod_not_loaded_is_said_of_a_running_claude_box_only() {
    use skein::signals::{engine_health, EngineRead, EngineSignal};
    let some = EngineSignal::default();
    assert_eq!(engine_health("claude", &EngineRead::Absent, true), "absent");
    assert_eq!(
        engine_health("claude", &EngineRead::Stale(some.clone()), true),
        "stale"
    );
    assert_eq!(engine_health("claude", &EngineRead::Fresh(some), true), "");
    assert_eq!(
        engine_health("codex", &EngineRead::Absent, true),
        "",
        "Codex gets nothing from the mod, so its absence is not news"
    );
    assert_eq!(
        engine_health("claude", &EngineRead::Absent, false),
        "",
        "a box that is not running has no session to load a mod into"
    );
}
