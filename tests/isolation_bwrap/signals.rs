//! A box can read its own resource signal file and cannot write it.

use super::*;

/// **A box cannot write its own signal file** (SKEIN-1054, box-plugin §4).
///
/// `<box_state>/signals/resources.json` is where skein tells a box what is asked of it, and the
/// plugin's hook holds one of the agent's commands on what it says. A box that could write it could
/// clear its own asks, or invent a crossing that holds its own agent. It is covered by the
/// launcher's read-only bind of the whole state directory, the same bind that makes `inbox/` a
/// directory only skein writes; this runs that bind in a real namespace rather than reading it.
///
/// The probe tries three writes: overwrite the file, create a sibling in `signals/`, and replace
/// the directory. Each must be refused, and the file must still read back whole.
///
/// **The workshop box runs the same probe as the control.** It skips the isolation block, so its
/// writes land. That is what makes the covered box's refusals a property of the cover rather than
/// of a probe that could not have written anything.
///
/// **What would make this fail**: `--bind "$state" "$state"` in place of `--ro-bind` in the
/// isolation block of `box-session.sh`.
#[test]
fn a_box_cannot_write_its_own_signal_file() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so the signal file's read-only bind was \
             NOT exercised",
        );
    }
    let probe = r#"
f="$1"; d=$(dirname "$f")
cat "$f" >/dev/null 2>&1 && echo "read" || echo "unread"
( printf '{"asks":[]}' > "$f" ) 2>/dev/null && echo "overwrote" || echo "kept"
( printf x > "$d/forged.json" ) 2>/dev/null && echo "created" || echo "refused"
( mv "$d" "$d.gone" ) 2>/dev/null && echo "moved" || echo "stayed"
"#;
    let original = "{\"asks\":[{\"kind\":\"disk\",\"crossing_id\":\"disk-1\"}]}\n";
    for born in [Born::Covered, Born::Workshop] {
        let fleet = Fleet::make("signals");
        let signals = fleet.state_parent.join("web-main/signals");
        fs::create_dir_all(&signals).unwrap();
        let file = signals.join("resources.json");
        fs::write(&file, original).unwrap();

        let out = fleet.in_box(born, probe, &[file.to_string_lossy().into_owned()]);
        let said: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_string)
            .collect();
        match born {
            Born::Covered => {
                assert_eq!(
                    said,
                    vec!["read", "kept", "refused", "stayed"],
                    "a box can write the file skein tells it what is asked of it in"
                );
                assert_eq!(
                    fs::read_to_string(&file).unwrap(),
                    original,
                    "the box changed its own signal file"
                );
            }
            _ => assert_eq!(
                said,
                vec!["read", "overwrote", "created", "moved"],
                "the control could not write either, so the covered box's refusals prove nothing"
            ),
        }
    }
}
