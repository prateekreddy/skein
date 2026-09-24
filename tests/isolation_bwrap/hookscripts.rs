//! What a box's turn-state hooks execute, and whether a sibling box can change it.

use super::*;

/// **A box cannot rewrite the script its sibling's turn-state hook runs, and the sibling's hook
/// still reports turn state** (SKEIN-1144).
///
/// Two boxes of one repository share its store, and every box can write that store — it is how
/// they share memory and mail (docs/threat-model.md). Until SKEIN-1144 the scripts the turn-state
/// hooks run lived there too, in `skein/bin/`, so one box could put its own commands into every
/// hook its siblings fire. They are installed into skein's plugin under the fleet root's `.skein`
/// now, which the launcher binds read-only, and the hooks run them from there.
///
/// Nothing here is hand-written in between: the plugin is `probes::plugin_install_under`'s bytes,
/// installed into this fixture fleet as `fleet::install_launcher` installs them, mode 755 and owned
/// by the same uid every box runs as — so the only thing between a box and those files is the
/// cover. The hook is the plugin's own `UserPromptSubmit` command for `box-status.sh`, run the way
/// Claude Code runs one: through a shell, with `$CLAUDE_PROJECT_DIR` and `${CLAUDE_PLUGIN_ROOT}` set.
///
/// Box A (`web-main`) tries to overwrite, append to, delete and move that script, to add a file
/// beside it and to move the whole plugin away. Then it writes a script of its own into the
/// store's `skein/bin/box-status.sh`, which it can; that write succeeding is what makes the rest of
/// this a statement about the plugin rather than about a box that can write nothing. Box B
/// (`web-two`) then fires its hook under the same cover, and its state has to arrive in the store
/// while A's planted script never runs.
///
/// **The workshop box runs A's writes as the control**: it skips the isolation block, so they land.
///
/// **What would make this fail**:
/// - `--bind` in place of `--ro-bind` for `.skein` in `box-session.sh`'s isolation block: A's
///   writes to the plugin land (the first assertion);
/// - the turn-state commands put back on `$CLAUDE_PROJECT_DIR/.claude/skein/bin/`, the store's
///   copies (`probes::turn_state_entries` returning `probes::store_era_entries` unchanged): the
///   script B's hook runs is then one A can overwrite (the first assertion), and B's hook runs
///   A's plant, which leaves a marker and reports no state (the last three).
#[test]
fn a_box_cannot_rewrite_the_script_its_siblings_turn_state_hook_runs() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so whether a box can rewrite the scripts \
             its siblings' turn-state hooks run was NOT exercised",
        );
    }
    // A, trying every way it has to change what the script at $1 is, then planting its own
    // script in the store copy at $2. One line per attempt.
    let attack = r#"
t="$1"; plant="$2"; plugin="$3"; d=$(dirname "$t")
( printf '#!/bin/sh\ntouch /nonexistent\n' > "$t" ) 2>/dev/null && echo "overwrote" || echo "kept"
( printf '\necho appended\n' >> "$t" ) 2>/dev/null && echo "appended" || echo "unappended"
( rm -f "$t" ) 2>/dev/null && [ ! -e "$t" ] && echo "deleted" || echo "present"
( printf x > "$d/planted.sh" ) 2>/dev/null && echo "created" || echo "refused"
( mv "$d" "$d.gone" ) 2>/dev/null && echo "moved" || echo "stayed"
( mv "$plugin" "$plugin.gone" ) 2>/dev/null && echo "unloaded" || echo "loaded"
mkdir -p "$(dirname "$plant")" 2>/dev/null
( printf '#!/bin/sh\nstore="$(dirname "$(dirname "$(dirname "$0")")")"\ntouch "$store/a-planted-script-ran"\n' > "$plant" ) 2>/dev/null \
  && echo "planted" || echo "unplanted"
"#;
    for born in [Born::Covered, Born::Workshop] {
        let fleet = Fleet::make("hookscripts");
        let root = fleet.fleet_root.to_string_lossy().into_owned();
        for (path, body) in skein::probes::plugin_install_under(&root) {
            let path = Path::new(&path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The checkout every box of `web` works in links the repo's store at `.claude`, as the
        // kit links it; the probes find the store from there.
        let store = fleet.store();
        let tree = fleet.fleet_root.join("web-main/tree");
        std::os::unix::fs::symlink(&store, tree.join(".claude")).unwrap();
        // And the store holds its own copies, as `probes::ensure_probe_in` leaves every store.
        let plant = store.join("skein/bin/box-status.sh");
        fs::create_dir_all(plant.parent().unwrap()).unwrap();

        // The hook B fires, from the variant every box loads whichever way the switch is set.
        let plugin = skein::runtime::turn_state_plugin_dir_under(&root);
        let hooks: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(format!("{plugin}/hooks/hooks.json")).unwrap(),
        )
        .unwrap();
        let command = hooks["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            .filter_map(|h| h["command"].as_str())
            .find(|c| c.contains("box-status.sh") && c.ends_with(" working"))
            .expect("the plugin has no UserPromptSubmit hook for box-status.sh")
            .to_string();
        let script = command
            .split_once('"')
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(s, _)| {
                s.replace("${CLAUDE_PLUGIN_ROOT}", &plugin)
                    .replace("$CLAUDE_PROJECT_DIR", &tree.to_string_lossy())
            })
            .expect("the hook names its script in quotes");
        let original = fs::read(format!("{plugin}/probe/box-status.sh"))
            .expect("the plugin installs no box-status.sh");
        fs::write(&plant, &original).unwrap();

        let out = fleet.in_box(
            born,
            attack,
            &[
                script.clone(),
                plant.to_string_lossy().into_owned(),
                plugin.clone(),
            ],
        );
        let said: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_string)
            .collect();
        if born == Born::Workshop {
            assert_eq!(
                said,
                vec![
                    "overwrote",
                    "appended",
                    "deleted",
                    "created",
                    "moved",
                    "unloaded",
                    "planted"
                ],
                "the control could not write the plugin either, so the covered box's refusals \
                 prove nothing"
            );
            continue;
        }
        assert_eq!(
            said,
            vec![
                "kept",
                "unappended",
                "present",
                "refused",
                "stayed",
                "loaded",
                "planted"
            ],
            "a box can change the script its sibling's turn-state hook runs, or cannot write the \
             store at all"
        );
        assert_eq!(
            fs::read(&script).unwrap(),
            original,
            "the script a sibling's hook runs is not the one skein installed"
        );

        // B fires its hook, under the same cover: the plugin and the store are bound alike into
        // every box of the repo.
        let fire = r#"cd "$1" && CLAUDE_PROJECT_DIR="$1" CLAUDE_PLUGIN_ROOT="$2" SKEIN_BOX=web-two \
bash -c "$3" </dev/null; echo "exit $?""#;
        let out = fleet.in_box(
            born,
            fire,
            &[tree.to_string_lossy().into_owned(), plugin.clone(), command],
        );
        assert_eq!(
            String::from_utf8_lossy(&out).trim(),
            "exit 0",
            "the sibling's hook failed"
        );
        assert!(
            !store.join("a-planted-script-ran").exists(),
            "the sibling's hook ran the script another box planted in the store"
        );
        let status = fs::read_to_string(store.join("status/web-two.json"))
            .unwrap_or_else(|e| panic!("the sibling's hook reported no turn state: {e}"));
        let status: serde_json::Value = serde_json::from_str(&status).expect(&status);
        assert_eq!(status["status"], "working", "{status}");
        assert_eq!(status["box"], "web-two", "{status}");
    }
}
