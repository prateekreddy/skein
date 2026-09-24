//! What a box runs from skein as it starts, rather than from a hook, and whether a sibling box can
//! change it.

use super::*;

/// The helpers a box runs from skein as it starts or is attached to — the kit's, the attach
/// shell's, the tracker's and the status line's — and the Codex hook wiring its installer reads.
/// Each is a file in the turn-state variant's `probe/` under `.skein` (SKEIN-1149).
/// `probes::every_start_helper_runs_from_the_read_only_plugin` holds every caller to that directory;
/// this list only says what box A tries to change.
const START_FILES: [&str; 8] = [
    "shared-home.sh",
    "agent-guide.sh",
    "install-codex-hooks.sh",
    "sync-install.sh",
    "sync-refresh.sh",
    "statusline-command.sh",
    "box-pane.sh",
    "codex-hooks.json",
];

/// **A box cannot change what its sibling runs from skein as it starts — Codex's hook wiring, the
/// kit's helpers or the status line — and the sibling's start still works** (SKEIN-1149).
///
/// Two boxes of one repository share its store, and every box can write it (docs/threat-model.md).
/// Until SKEIN-1149 the kit ran `shared-home.sh`, `agent-guide.sh`, `install-codex-hooks.sh` and
/// `sync-install.sh` from the store's `skein/bin/`, the installer merged the store's
/// `skein/codex-hooks.json` into the box's `~/.codex/hooks.json`, the attach shell and the tracker
/// ran their helpers from there too, and skein's default status line ran the store's renderer. So a
/// box could choose what its siblings ran the next time they started. They run skein's read-only
/// copies under the fleet root's `.skein` now.
///
/// Nothing here is hand-written in between: the plugin is `probes::plugin_install_under`'s bytes
/// and the kit is `src/kit/skein-startup.sh`, installed into this fixture fleet as
/// `fleet::install_launcher` installs them, mode 755 and owned by the uid every box runs as. What
/// box B runs is the kit, the way `fleet::provision_script` runs it, and then every string in
/// `probes::start_invocations` — Codex's setup, the guide refresh, the screen observer, the
/// tracker's install and refresh, and the status line — each the production string.
///
/// Box A (`web-main`) overwrites every one of those files in `.skein` with a script that leaves a
/// marker in the store (and Codex's wiring with a hook that would), then plants the same in the
/// store's `skein/bin/` and `skein/codex-hooks.json`, which it can: that write succeeding is what
/// makes the rest a statement about `.skein` rather than about a box that can write nothing. Box B
/// (`web-two`) then starts under the same cover.
///
/// **The workshop box is the control, and it is a full one**: it skips the isolation block, so A's
/// writes land, and B's start then runs every one of A's plants — each leaves its marker. So the
/// markers' absence in the covered box is an absence something could have filled.
///
/// **What would make this fail**:
/// - `--bind` in place of `--ro-bind` for `.skein` in `box-session.sh`'s isolation block: A's
///   writes land (the first assertion);
/// - any caller put back on the store — the kit's `$skein_probe` as `$store/skein/bin`, a runtime
///   or tracker string on `.claude/skein/bin/`, the status line on the store's renderer, or the
///   Codex installer reading `$store/skein/codex-hooks.json`: B runs A's plant (a marker appears,
///   or B's Codex hooks carry A's command);
/// - a helper the plugin does not install: B's start does not work (the boot report, the guide or
///   the Codex hooks come up missing).
#[test]
fn a_box_cannot_change_what_its_sibling_runs_as_it_starts() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so whether a box can change what its \
             siblings run from skein as they start was NOT exercised",
        );
    }
    for tool in ["jq", "tmux"] {
        if !common::have(tool) {
            return skip(&format!(
                "no {tool} here, and the kit refuses to provision a box without it, so whether a \
                 box can change what its siblings run as they start was NOT exercised"
            ));
        }
    }
    // A, trying to overwrite each file in `$1` (the plugin's probe/) and then planting the same in
    // the store at `$2`. Each plant leaves `$2/planted-<name>` behind if anything runs it. One line
    // per attempt.
    let attack = r##"
dir="$1"; store="$2"; shift 2
for name in "$@"; do
  case "$name" in
    *.json) body="{\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"touch $store/planted-$name\"}]}]}}" ;;
    *) body="#!/bin/sh
touch '$store/planted-$name'" ;;
  esac
  ( printf '%s\n' "$body" > "$dir/$name" ) 2>/dev/null && echo "overwrote $name" || echo "kept $name"
  case "$name" in
    *.json) plant="$store/skein/$name" ;;
    *) plant="$store/skein/bin/$name" ;;
  esac
  mkdir -p "$(dirname "$plant")" 2>/dev/null
  ( printf '%s\n' "$body" > "$plant" ) 2>/dev/null && echo "planted $name" || echo "unplanted $name"
done
"##;
    // B, starting: the kit as the fleet runs it, then each in-box string skein runs at attach, in
    // its checkout, with a private $HOME and /tmp of its own under it.
    let start = r#"
tree="$1"; store="$2"; root="$3"; shift 3
export HOME="$tree/.home-b"; m="$tree/.tmp-b"; mkdir -p "$HOME" "$m"
SKEIN_STARTUP_MARKERS="$m" SKEIN_PROVISION=1 SKEIN_BOX=web-two SKEIN_STORE="$store" \
  WORKSPACE_DIR="$tree" bash "$root/.skein/skein-startup.sh" </dev/null >"$m/kit.out" 2>&1
echo "kit $?"
cd "$tree" || exit 1
for invocation in "$@"; do
  printf '{}' | SKEIN_BOX=web-two CLAUDE_PROJECT_DIR="$tree" bash -c "$invocation" >/dev/null 2>&1
done
# The kit's tracker install and the screen observer are detached; give them the time a plant
# needs to leave its marker (the workshop run below proves they do within it).
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ "$(ls "$store" | grep -c '^planted-')" -ge 7 ] && break
  sleep 1
done
# The screen observer's single-instance lock, in whatever /tmp this box has.
rm -f /tmp/skein-pane.web-two.lock
"#;
    let names: Vec<String> = START_FILES.iter().map(|n| n.to_string()).collect();
    let invocations: Vec<String> = skein::probes::start_invocations()
        .into_iter()
        .map(|(_, shell)| shell)
        .collect();
    assert_eq!(
        invocations.len(),
        6,
        "the start invocations are not the set this test runs"
    );

    for born in [Born::Covered, Born::Workshop] {
        let fleet = Fleet::make("startscripts");
        let root = fleet.fleet_root.to_string_lossy().into_owned();
        let kit = fs::read_to_string(script("kit/skein-startup.sh")).unwrap();
        let mut install = skein::probes::plugin_install_under(&root);
        install.push((format!("{root}/.skein/skein-startup.sh"), kit));
        for (path, body) in install {
            let path = Path::new(&path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let probe = format!(
            "{}/probe",
            skein::runtime::turn_state_plugin_dir_under(&root)
        );
        let originals: Vec<Vec<u8>> = START_FILES
            .iter()
            .map(|n| {
                fs::read(format!("{probe}/{n}"))
                    .unwrap_or_else(|e| panic!("the plugin installs no {n}: {e}"))
            })
            .collect();

        // What the store holds for B's start that is data rather than a helper: the runtime
        // manifest and the guide agent-guide.sh checks for, and B's launch spec saying it is Codex.
        let store = fleet.store();
        fs::create_dir_all(store.join("skein/launch")).unwrap();
        fs::write(
            store.join("skein/runtimes.tsv"),
            "codex\tCodex\tcodex\t.codex/AGENTS.md\t.codex/AGENTS.override.md\n",
        )
        .unwrap();
        fs::write(store.join("skein/SHARED-HOME.md"), "the guide\n").unwrap();
        fs::write(
            store.join("skein/launch/web-two.json"),
            "{\"agent\":\"codex\",\"branch\":\"\"}\n",
        )
        .unwrap();
        let tree = fleet.fleet_root.join("web-main/tree");

        let mut args = vec![probe.clone(), store.to_string_lossy().into_owned()];
        args.extend(names.iter().cloned());
        let said: Vec<String> = String::from_utf8_lossy(&fleet.in_box(born, attack, &args))
            .lines()
            .map(str::to_string)
            .collect();
        let expect = |verb: &str| -> Vec<String> {
            START_FILES
                .iter()
                .flat_map(|n| [format!("{verb} {n}"), format!("planted {n}")])
                .collect()
        };
        let mut args = vec![
            tree.to_string_lossy().into_owned(),
            store.to_string_lossy().into_owned(),
            root.clone(),
        ];
        args.extend(invocations.iter().cloned());
        let started = String::from_utf8_lossy(&fleet.in_box(born, start, &args)).into_owned();
        let planted_ran: Vec<String> = fs::read_dir(&store)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("planted-"))
            .collect();

        if born == Born::Workshop {
            assert_eq!(
                said,
                expect("overwrote"),
                "the control could not write .skein either, so the covered box's refusals prove \
                 nothing"
            );
            // Every script plant ran when B started, so a marker's absence below is one that
            // something could have filled. Codex's wiring is data, not a script: its plant is
            // proven by B's hooks carrying it.
            for name in START_FILES.iter().filter(|n| n.ends_with(".sh")) {
                assert!(
                    planted_ran.contains(&format!("planted-{name}")),
                    "the control's start did not run A's {name}, so this test cannot see a \
                     sibling's start running a plant: ran {planted_ran:?}\n{started}"
                );
            }
            continue;
        }

        assert_eq!(
            said,
            expect("kept"),
            "a box can change what its sibling runs from skein as it starts, or cannot write the \
             store at all"
        );
        for (name, original) in START_FILES.iter().zip(&originals) {
            assert_eq!(
                &fs::read(format!("{probe}/{name}")).unwrap(),
                original,
                "{name}, which a sibling runs as it starts, is not the one skein installed"
            );
        }
        assert_eq!(started.trim(), "kit 0", "the sibling's start failed");
        assert!(
            planted_ran.is_empty(),
            "the sibling's start ran what another box planted in the store: {planted_ran:?}"
        );

        // And B's start worked, through skein's own copies.
        let boot: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(store.join("skein/boot/web-two.json"))
                .expect("the sibling's start wrote no boot report"),
        )
        .unwrap();
        assert_eq!(boot["shared_home"], "linked", "{boot}");
        assert_eq!(boot["agent_guide"], "installed", "{boot}");
        assert_eq!(boot["codex_hooks"], "installed", "{boot}");
        let home = tree.join(".home-b");
        let hooks = fs::read_to_string(home.join(".codex/hooks.json"))
            .expect("the sibling has no Codex hooks");
        assert!(
            !hooks.contains("planted-"),
            "the sibling's Codex hooks carry a command another box wrote: {hooks}"
        );
        let hooks: serde_json::Value = serde_json::from_str(&hooks).unwrap();
        let skeins: serde_json::Value =
            serde_json::from_slice(&originals[START_FILES.len() - 1]).unwrap();
        assert_eq!(
            hooks, skeins,
            "the sibling's Codex hooks are not the ones skein installed"
        );
        assert!(
            fs::read_to_string(home.join(".codex/AGENTS.md"))
                .expect("the sibling has no agent guide")
                .contains("skein:shared-home:start"),
            "the sibling's agent guide is not skein's"
        );
    }
}
