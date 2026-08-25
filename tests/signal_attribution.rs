//! **One box's hook signals, filed under another box's name, must not become that box's row.**
//!
//! The sibling of `tests/pane_attribution.rs`, for the three signals the *hooks* write. The board
//! reads `<store>/status/<box>.json`, `<store>/sessions/<box>.json` and `<store>/tasks/<box>.json`
//! and renders each as that box's turn state, its headline and what it is doing. The filename was
//! the only thing asserting whose they were, and every probe that wrote them resolved that name
//! through `${SKEIN_BOX:-${SANDBOX_VM_ID:-$(hostname)}}` — a chain that falls through to the
//! SANDBOX's name, which in a shared sandbox is one string for every box in it.
//!
//! The probes cannot write the wrong name any more; `probes::tests::
//! no_probe_files_a_signal_under_the_sandboxs_name` drives all eleven of them through the three
//! identity worlds to show it. That half fixes what is written from now on and says nothing about
//! what is already on disk — a store copied, a store restored, a box whose probes predate the fix.
//! This is the other half, end to end: real signals written under the wrong name and read back
//! through the board a browser tab renders. It is the one that would notice `signal_is_ours` being
//! dropped from `status_edge` while `signals::tests` still passes.

use std::fs;
use std::path::{Path, PathBuf};

const FLEET: &str = "sigattrib-fleet";
/// The box the signals are really about, and one they are not.
const OWNER: &str = "sigattrib-owner";
const OTHER: &str = "sigattrib-other";
/// A box whose probes predate the `box` field, which is every box until it is reattached.
const LEGACY: &str = "sigattrib-legacy";

/// Write one signal file exactly as the probe writes it — `box` included, or omitted for a probe
/// that predates the field.
fn signal(dir: &Path, kind: &str, filed_as: &str, claims: Option<&str>, body: serde_json::Value) {
    let d = dir.join(kind);
    fs::create_dir_all(&d).unwrap();
    let mut v = body;
    if let Some(b) = claims {
        v["box"] = serde_json::Value::String(b.to_string());
    }
    fs::write(
        d.join(format!("{filed_as}.json")),
        serde_json::to_string(&v).unwrap(),
    )
    .unwrap();
}

#[test]
fn the_board_will_not_show_one_boxs_hook_signals_as_another_boxs_row() {
    let root = PathBuf::from("/var/tmp").join(format!("skein-sigattrib-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    // The fixture recipe from tests/pane_attribution.rs: the registry names the boxes, the
    // placements make them the fleet's, and `store_for_box` falls back to the registry's own
    // directory for a box that matches no managed repo — which is where the signals go.
    std::env::set_var("SKEIN_HOME", root.join("skein"));
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));
    // This suite may itself be running inside a box, which the board promotes onto itself.
    std::env::remove_var("SANDBOX_VM_ID");
    std::env::remove_var("SKEIN_SELF");
    let reg = root.join("sandboxes.json");
    // No `status` in the registry rows, so the ONLY turn state available is the one skein's own
    // probe wrote — which makes "the misfiling was refused" show up as a row with no state at all
    // rather than as a state that happens to match.
    let rows: Vec<String> = [OWNER, OTHER, LEGACY]
        .iter()
        .map(|n| format!(r#""{n}":{{"branch":"main","dir":"/nowhere","lastSeen":"1"}}"#))
        .collect();
    fs::write(&reg, format!("{{{}}}", rows.join(","))).unwrap();
    std::env::set_var("SKEIN_REGISTRY", &reg);

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
            },
        )
        .expect("place a box");
    }

    let now = chrono::Utc::now().to_rfc3339();
    let head = "the migration is finished and the tests are green";
    let doing = "Rewriting the identity chain";
    for (filed_as, claims) in [
        (OWNER, Some(OWNER)), // correctly filed
        (OTHER, Some(OWNER)), // OWNER's signals, under OTHER's name
        (LEGACY, None),       // a probe that cannot say
    ] {
        signal(
            &root,
            "status",
            filed_as,
            claims,
            serde_json::json!({"status": "working", "detail": "API error: rate limit", "ts": now}),
        );
        signal(
            &root,
            "sessions",
            filed_as,
            claims,
            serde_json::json!({"ts": now, "kind": "stop", "lastMessage": head, "prompt": ""}),
        );
        signal(
            &root,
            "tasks",
            filed_as,
            claims,
            serde_json::json!({"task": doing}),
        );
    }

    let views = skein::board::load_views().expect("a board tick");
    let row = |name: &str| {
        views
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("{name} is not on the board"))
    };

    // The control, and what keeps every assertion below from being vacuous: these signals really do
    // reach the board when they are the box's own.
    assert_eq!(
        row(OWNER).state,
        "working",
        "the fixture must reach the board, or nothing here is being tested"
    );
    assert_eq!(row(OWNER).task.as_deref(), Some(doing));
    // The narrative signal wins the headline over the status detail when both are present, which
    // is why the assertions below have to rule out BOTH strings rather than just this one.
    assert_eq!(row(OWNER).headline.as_deref(), Some(head));

    // The bug. The same bytes, one field different, and none of the three may be borrowed.
    assert_ne!(
        row(OTHER).state,
        "working",
        "a status naming {OWNER} was rendered as {OTHER}'s turn state. The filename is a claim \
         about whose signal it is; `signal_is_ours` is what checks it, and `status_edge` has to \
         apply it before the status is read at all."
    );
    assert_eq!(
        row(OTHER).task.as_deref(),
        None,
        "a task naming {OWNER} was rendered as what {OTHER} is doing right now"
    );
    assert_ne!(
        row(OTHER).headline.as_deref(),
        Some("API error: rate limit"),
        "a status detail naming {OWNER} became {OTHER}'s headline — the row says why a box is in \
         trouble, about a box that never was"
    );
    assert_ne!(
        row(OTHER).headline.as_deref(),
        Some(head),
        "a narrative signal naming {OWNER} became {OTHER}'s headline: this box reads as having \
         said something it never said"
    );

    // And the cost of refusing is bounded to what is actually wrong. A signal that names NOBODY
    // could not be checked — not a fault — and is still read, because every box in the fleet is
    // running such a probe until it is reattached, and treating "unknown" as "wrong" would take all
    // three signals away from all of them at once.
    assert_eq!(
        row(LEGACY).state,
        "working",
        "a status from a probe that predates the `box` field must still be read"
    );
    assert_eq!(row(LEGACY).task.as_deref(), Some(doing));

    let _ = fs::remove_dir_all(&root);
}
