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
/// - a helper dropped from what the plugin installs: the test refuses before anything runs ("the
///   plugin installs no …");
/// - a caller naming a file the plugin does not install (the kit's installer misspelt): B's start
///   does not work (the boot report says `codex_hooks` is `absent`).
#[test]
fn a_box_cannot_change_what_its_sibling_runs_as_it_starts() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so whether a box can change what its \
             siblings run from skein as they start was NOT exercised",
        );
    }
    // The kit refuses to provision a box without either (its `tools_ok`).
    if !common::have("jq") || !common::have("tmux") {
        return skip(
            "no jq or no tmux here, and the kit refuses to provision a box without both, so whether \
             a box can change what its siblings run as they start was NOT exercised",
        );
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
# The screen observer asks tmux whether the agent's session is up, and keeps watching while it is:
# pointed at a socket of this fixture's own, where there is none, so it cannot find a real
# `skein-agent` on the machine's default tmux server and go on observing it after the test.
export SKEIN_TMUX_SOCK="$m/no-server.sock" TMUX_TMPDIR="$m"
for invocation in "$@"; do
  printf '{}' | SKEIN_BOX=web-two CLAUDE_PROJECT_DIR="$tree" bash -c "$invocation" >/dev/null 2>&1
done
# The kit's tracker install and the screen observer are detached; give them the time a plant
# needs to leave its marker (the workshop run below proves they do within it).
for _ in 1 2 3 4 5 6 7 8 9 10; do
  [ "$(ls "$store" | grep -c '^planted-')" -ge 7 ] && break
  sleep 1
done
# Nothing this start detached outlives the test: wait for the observer and the kit's tracker install
# to exit, then take the observer's single-instance lock out of whatever /tmp this box has.
own="^(bash|timeout -k [0-9]+ [0-9]+ bash) $root/\.skein/[^ ]*/(box-pane|sync-install)\.sh"
for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -f "$own" >/dev/null || break; sleep 1; done
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

/// **A box cannot plant a hook or a status line that its sibling's Claude runs** (SKEIN-1153,
/// closed by SKEIN-1048 and SKEIN-1053).
///
/// Until SKEIN-1048 the kit's case 2 copied hooks, `tui` and `statusLine` from the store's
/// `settings.json` into the repo's own `.claude/settings.json` at every start. Every box of the
/// repo can write the store, so box A could put a command there and box B ran it after its next
/// start. The kit now writes only skein's defaults, read from the plugin's read-only
/// `settings-defaults.json`, and only into B's own `settings.local.json`.
///
/// Box A (`web-main`) tries to overwrite `settings-defaults.json` in the plugin, then plants a hook
/// and a status line in every settings file of the store's: `settings.json`,
/// `settings.local.json`, and a `skein/settings-defaults.json` beside the store's other copies,
/// which it can. That write succeeding is what makes the rest a statement about the plugin rather
/// than about a box that can write nothing. Box B (`web-two`) then starts under the same cover:
/// the kit, then every hook command and the status line of both settings files Claude Code reads
/// in B's checkout, each run through a shell the way Claude Code runs one. A plant that runs
/// leaves a marker in the store.
///
/// **The workshop box is the control**: A's write to the plugin lands, B's start carries A's
/// status line into its local settings, and running it leaves the marker. So the marker's absence
/// in the covered box is an absence something could have filled.
///
/// Both layouts (SKEIN-1053): a repo that ships `.claude/`, and one that tracks nothing there,
/// whose box B still has `.claude` as the store's own link — the layout every box had before —
/// and converts it as it starts, under the cover. After that, `.claude` is a directory of B's own
/// with the store's entries linked in and its settings files left out.
///
/// **What would make this fail**:
/// - `--bind` in place of `--ro-bind` for `.skein`: A's write to the plugin lands (the first
///   assertion);
/// - the kit reading its defaults from the store (`defaults="$store/skein/settings-defaults.json"`):
///   B runs A's status line (a marker appears);
/// - the kit as it was at bd85fd5, merging the store's `settings.json` into the repo's: B's
///   `settings.json` carries A's hook, which runs (a marker appears), and B's tree is dirty;
/// - the kit leaving `.claude` as the store's link where the repo ships none: B's
///   `.claude/settings.json` is the store's, and A's plant in it runs (a marker appears).
#[test]
fn a_box_cannot_plant_a_hook_or_status_line_its_siblings_claude_runs() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so whether a box can plant a setting its \
             siblings' Claude runs was NOT exercised",
        );
    }
    // The kit refuses to provision a box without either (its `tools_ok`).
    if !common::have("jq") || !common::have("tmux") {
        return skip(
            "no jq or no tmux here, and the kit refuses to provision a box without both, so whether \
             a box can plant a setting its siblings' Claude runs was NOT exercised",
        );
    }
    // A: the plugin's defaults at $1, then the store at $2. One line per attempt.
    let attack = r##"
defaults="$1"; store="$2"
plant() {
  printf '{"tui":"default","statusLine":{"type":"command","command":"touch %s/planted-%s-line"},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"touch %s/planted-%s-hook"}]}]}}\n' \
    "$store" "$1" "$store" "$1"
}
( printf '{"settings":%s,"storeEraStatusLine":""}\n' "$(plant plugin)" > "$defaults" ) 2>/dev/null \
  && echo "overwrote" || echo "kept"
for name in settings.json settings.local.json skein/settings-defaults.json; do
  tag="$(printf '%s' "$name" | tr -c 'a-z' '-')"
  case "$name" in
    skein/*) body="$(printf '{"settings":%s,"storeEraStatusLine":""}' "$(plant "$tag")")" ;;
    *) body="$(plant "$tag")" ;;
  esac
  ( printf '%s\n' "$body" > "$store/$name" ) 2>/dev/null && echo "planted $name" || echo "unplanted $name"
done
"##;
    // B, starting: the kit as the fleet runs it, then every command its Claude would run from the
    // settings in its checkout.
    let start = r#"
tree="$1"; store="$2"; root="$3"
export HOME="$tree/../home-b"; m="$tree/../tmp-b"; mkdir -p "$HOME" "$m"
SKEIN_STARTUP_MARKERS="$m" SKEIN_PROVISION=1 SKEIN_BOX=web-two SKEIN_STORE="$store" \
  WORKSPACE_DIR="$tree" bash "$root/.skein/skein-startup.sh" </dev/null >"$m/kit.out" 2>&1
echo "kit $?"
cd "$tree" || exit 1
for file in .claude/settings.json .claude/settings.local.json; do
  [ -f "$file" ] || continue
  jq -r '[.statusLine.command // empty] + [.hooks // {} | .[] | .[] | .hooks // [] | .[] | .command // empty] | .[]' "$file" \
    | while IFS= read -r command; do
        printf '{}' | CLAUDE_PROJECT_DIR="$tree" SKEIN_BOX=web-two bash -c "$command" >/dev/null 2>&1
      done
done
echo "status [$(git status --porcelain | tr '\n' ' ')]"
"#;

    for (born, ships) in [
        (Born::Covered, true),
        (Born::Covered, false),
        (Born::Workshop, true),
        (Born::Workshop, false),
    ] {
        let fleet = Fleet::make("startsettings");
        let root = fleet.fleet_root.to_string_lossy().into_owned();
        let kit = fs::read_to_string(script("kit/skein-startup.sh")).unwrap();
        // Everything the fleet installs but the tracker's installer, which the kit starts detached
        // and which has nothing to do with settings.
        let mut install = skein::probes::plugin_install_under(&root);
        install.retain(|(path, _)| !path.ends_with("/sync-install.sh"));
        install.push((format!("{root}/.skein/skein-startup.sh"), kit));
        for (path, body) in install {
            let path = Path::new(&path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let defaults = format!(
            "{}/probe/settings-defaults.json",
            skein::runtime::turn_state_plugin_dir_under(&root)
        );
        let original = fs::read(&defaults).expect("the plugin installs no settings-defaults.json");

        let store = fleet.store();
        fs::create_dir_all(store.join("skein/launch")).unwrap();
        fs::write(
            store.join("skein/runtimes.tsv"),
            "claude\tClaude\tclaude\t.claude/CLAUDE.md\t\n",
        )
        .unwrap();
        fs::write(store.join("skein/SHARED-HOME.md"), "the guide\n").unwrap();
        fs::write(
            store.join("skein/launch/web-two.json"),
            "{\"agent\":\"claude\",\"branch\":\"\"}\n",
        )
        .unwrap();
        // B's checkout. Either its repo tracks a `.claude/settings.json` of its own, or it tracks
        // nothing under `.claude` and B still has the layout every box had before SKEIN-1053:
        // `.claude` is the store's own link, which B's start converts under the cover.
        let tree = fleet.fleet_root.join("web-main/tree");
        let tracked = "{ \"model\": \"example-model\" }\n";
        let (file, message) = match ships {
            true => (".claude/settings.json", "the repo ships its own .claude"),
            false => ("README.md", "a repo"),
        };
        fs::create_dir_all(tree.join(file).parent().unwrap()).unwrap();
        fs::write(tree.join(file), tracked).unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", file],
            &["commit", "-qm", message],
        ] {
            let out = Command::new("git")
                .args([
                    "-c",
                    "user.name=example",
                    "-c",
                    "user.email=example@example.com",
                ])
                .arg("-C")
                .arg(&tree)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        }
        if !ships {
            std::os::unix::fs::symlink(&store, tree.join(".claude")).unwrap();
            fs::write(tree.join(".git/info/exclude"), "/.claude\n").unwrap();
        }

        let said: Vec<String> = String::from_utf8_lossy(&fleet.in_box(
            born,
            attack,
            &[defaults.clone(), store.to_string_lossy().into_owned()],
        ))
        .lines()
        .map(str::to_string)
        .collect();
        let started = String::from_utf8_lossy(&fleet.in_box(
            born,
            start,
            &[
                tree.to_string_lossy().into_owned(),
                store.to_string_lossy().into_owned(),
                root.clone(),
            ],
        ))
        .into_owned();
        let ran: Vec<String> = fs::read_dir(&store)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("planted-"))
            .collect();
        let planted: Vec<String> = [
            "settings.json",
            "settings.local.json",
            "skein/settings-defaults.json",
        ]
        .iter()
        .map(|n| format!("planted {n}"))
        .collect();

        if born == Born::Workshop {
            assert_eq!(
                said[0], "overwrote",
                "the control could not write the plugin either, so the covered box's refusal \
                 proves nothing"
            );
            assert_eq!(
                said[1..],
                planted[..],
                "the control could not write the store"
            );
            assert!(
                ran.contains(&"planted-plugin-line".to_string()),
                "the control's start did not run the status line A planted in the plugin, so this \
                 test cannot see a sibling running a plant: ran {ran:?}\n{started}"
            );
            continue;
        }

        assert_eq!(
            said,
            [vec!["kept".to_string()], planted].concat(),
            "a box can change the settings its sibling's start reads, or cannot write the store at \
             all"
        );
        assert_eq!(
            fs::read(&defaults).unwrap(),
            original,
            "settings-defaults.json, which a sibling's start reads, is not the one skein installed"
        );
        assert!(
            ran.is_empty(),
            "the sibling's Claude runs a setting another box planted (repo ships .claude: {ships}): \
             {ran:?}"
        );
        assert_eq!(
            started.trim(),
            "kit 0\nstatus []",
            "the sibling's start failed, or left its tree dirty"
        );
        // And B's start worked: skein's own defaults are in its local settings.
        let local: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(tree.join(".claude/settings.local.json"))
                .expect("the sibling's start wrote no local settings"),
        )
        .unwrap();
        let skeins: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(
            local["statusLine"], skeins["settings"]["statusLine"],
            "{local}"
        );
        if ships {
            assert_eq!(
                fs::read_to_string(tree.join(".claude/settings.json")).unwrap(),
                tracked,
                "the sibling's start changed the settings its repo tracks"
            );
        } else {
            assert!(
                !tree.join(".claude").is_symlink()
                    && tree.join(".claude/skein").is_symlink()
                    && !tree.join(".claude/settings.json").exists(),
                "the sibling's .claude is still the store's, or loads the store's settings"
            );
        }
    }
}

/// The workshop box restarting, the way the one that went dark did: `SKEIN_BOX_PRIVILEGED=1`, so
/// nothing binds its store back and the store is NOT a mount point; `$SKEIN_STORE` unset, because
/// only the provision passes it (`fleet::provision_script`); and a checkout whose repo tracks
/// `.claude/`, so `.claude` is a directory and not the store's old link.
///
/// `born` is [`Born::WorkshopUnmatched`] for the restart that went dark: its launcher was handed no
/// `SKEIN_BOX_STORE`, so it left no record of the store in the checkout (SKEIN-1174) and the kit
/// has to find it by itself. [`Born::Workshop`] is the same restart from a launcher that knows it.
///
/// `workspace` makes `Fleet::repos`, which holds both repos' stores, a mount of its own, as the
/// sandbox mounts `fleet::fleet_workspace`. `prepare` gets the checkout after it is committed and
/// before the kit runs. Returns the checkout and what the start said: `kit <status>`, the kit's
/// output, and `store is a mount` if the premise did not hold.
fn restart_workshop(
    fleet: &Fleet,
    born: Born,
    workspace: bool,
    prepare: impl FnOnce(&Path),
) -> Option<(PathBuf, String)> {
    if !bwrap_works() {
        skip(
            "bwrap cannot create a user namespace here, so how the workshop box finds its store \
             on a restart was NOT exercised",
        );
        return None;
    }
    if !common::have("jq") || !common::have("tmux") || !common::have("git") {
        skip(
            "no jq, tmux or git here, and the kit refuses to provision a box without the first \
             two, so how the workshop box finds its store on a restart was NOT exercised",
        );
        return None;
    }
    let root = fleet.fleet_root.to_string_lossy().into_owned();
    let mut install = skein::probes::plugin_install_under(&root);
    install.push((
        format!("{root}/.skein/skein-startup.sh"),
        fs::read_to_string(script("kit/skein-startup.sh")).unwrap(),
    ));
    for (path, body) in install {
        let path = Path::new(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let tree = fleet.fleet_root.join("web-main/tree");
    fs::create_dir_all(tree.join(".claude")).unwrap();
    fs::write(tree.join(".claude/settings.json"), "{}\n").unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", ".claude/settings.json"],
        &["commit", "-qm", "the repo ships its own .claude"],
    ] {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=example",
                "-c",
                "user.email=example@example.com",
            ])
            .arg("-C")
            .arg(&tree)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }
    prepare(&tree);
    let start = r#"
tree="$1"; home="$2"; root="$3"; store="$4"
unset SKEIN_STORE
export HOME="$home"; m="$home/.tmp"; mkdir -p "$HOME" "$m"
SKEIN_STARTUP_MARKERS="$m" SKEIN_PROVISION=1 SKEIN_BOX=web-main WORKSPACE_DIR="$tree" \
  bash "$root/.skein/skein-startup.sh" </dev/null >"$m/kit.out" 2>&1
echo "kit $?"
cat "$m/kit.out"
awk '{print $5}' /proc/self/mountinfo | grep -qxF "$store" && echo "store is a mount"
# Nothing the kit detached outlives the test.
own="^(bash|timeout -k [0-9]+ [0-9]+ bash) $root/\.skein/[^ ]*/sync-install\.sh"
for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -f "$own" >/dev/null || break; sleep 1; done
"#;
    let mounts = if workspace {
        vec![fleet.repos.clone()]
    } else {
        vec![]
    };
    let said = fleet.in_box_mounting(
        born,
        &mounts,
        start,
        &[
            tree.to_string_lossy().into_owned(),
            fleet.dir.join("home").to_string_lossy().into_owned(),
            root,
            fleet.store().to_string_lossy().into_owned(),
        ],
    );
    let said = String::from_utf8_lossy(&said).into_owned();
    assert!(
        !said.contains("store is a mount"),
        "the store is a mount point in this box, so this is not the restart that went dark: {said}"
    );
    Some((tree, said))
}

/// A launch spec for box `name` in the store of repo `repo`.
fn launch_spec(fleet: &Fleet, repo: &str, name: &str) {
    let dir = fleet
        .repos
        .join(format!("{repo}/store/.claude/skein/launch"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{name}.json")),
        "{\"agent\":\"claude\",\"branch\":\"\"}\n",
    )
    .unwrap();
}

/// The boot report box `web-main` left in repo `repo`'s store, if it left one.
fn boot_report(fleet: &Fleet, repo: &str) -> Option<serde_json::Value> {
    fs::read_to_string(
        fleet
            .repos
            .join(format!("{repo}/store/.claude/skein/boot/web-main.json")),
    )
    .ok()
    .map(|s| serde_json::from_str(&s).unwrap())
}

/// **The workshop box finds its store on a restart by the launch spec skein wrote for it**, with no
/// `$SKEIN_STORE`, no store mount and no `.claude/skein` link: the first start after git replaced
/// the old `.claude` link with the directory the repo tracks. The other repo's store, holding
/// another box's spec, is neither linked nor reported into.
///
/// **What would make this fail**:
/// - the scan of the workspace for `*/store/.claude/skein/launch/<box>.json` deleted: the box has no
///   store (`.claude/skein` is not linked, and no boot report is written);
/// - the match loosened to any launch spec (`launch/*.json`): both repos' stores match, the clone
///   names neither mirror, and none is adopted.
#[test]
fn the_workshop_box_finds_its_store_by_its_launch_spec_on_a_restart() {
    let fleet = Fleet::make("storefind-spec");
    launch_spec(&fleet, "web", "web-main");
    launch_spec(&fleet, "other", "other-main");
    let Some((tree, said)) = restart_workshop(&fleet, Born::WorkshopUnmatched, true, |_| {}) else {
        return;
    };
    assert_eq!(
        fs::read_link(tree.join(".claude/skein")).ok(),
        Some(fleet.store().join("skein")),
        "the workshop box did not link its own store: {said}"
    );
    let boot = boot_report(&fleet, "web").expect("the workshop box wrote no boot report");
    assert_eq!(boot["claude_link"], "merged", "{boot}");
    assert_eq!(boot["shared_home"], "linked", "{boot}");
    assert!(
        boot_report(&fleet, "other").is_none(),
        "the workshop box reported into another repo's store"
    );
    assert!(said.starts_with("kit 0\n"), "the start failed: {said}");
}

/// **A restart finds the store through the `.claude/skein` link an earlier start made**, with no
/// `$SKEIN_STORE` and no mount to scan at all, as sandbox-bootstrap.sh and box-status.sh do.
///
/// **What would make this fail**: the link step deleted — nothing else can find the store here,
/// so the box has no store and writes no boot report.
#[test]
fn the_workshop_box_finds_its_store_through_the_link_an_earlier_start_made() {
    let fleet = Fleet::make("storefind-link");
    launch_spec(&fleet, "web", "web-main");
    let store = fleet.store();
    let Some((tree, said)) = restart_workshop(&fleet, Born::WorkshopUnmatched, false, |tree| {
        std::os::unix::fs::symlink(store.join("skein"), tree.join(".claude/skein")).unwrap();
    }) else {
        return;
    };
    let boot = boot_report(&fleet, "web")
        .unwrap_or_else(|| panic!("the store was not found through the link: {said}"));
    assert_eq!(boot["claude_link"], "merged", "{boot}");
    assert_eq!(
        fs::read_link(tree.join(".claude/skein")).ok(),
        Some(fleet.store().join("skein")),
        "the start moved the link"
    );
}

/// **A box never adopts another repository's store.** Two stores carry a launch spec for this box
/// — the other one stale — and the clone was made from `web`'s mirror: `web`'s is adopted, and the
/// boot report names the one passed over. Then with only the other repo's store carrying a spec:
/// none is adopted, and the other store is left untouched.
///
/// **What would make this fail**:
/// - the tie-break replaced by the first spec found (`head -n 1`): `other` sorts first and is
///   adopted;
/// - a single spec adopted whatever the clone was made from: the second case adopts `other`.
#[test]
fn a_box_never_adopts_another_repositorys_store() {
    for stale_only in [false, true] {
        let fleet = Fleet::make("storefind-two");
        if !stale_only {
            launch_spec(&fleet, "web", "web-main");
        }
        launch_spec(&fleet, "other", "web-main");
        let mirror = fleet.repos.join("web/mirror");
        let Some((tree, said)) = restart_workshop(&fleet, Born::WorkshopUnmatched, true, |tree| {
            let out = Command::new("git")
                .arg("-C")
                .arg(tree)
                .args(["remote", "add", "local"])
                .arg(&mirror)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        }) else {
            return;
        };
        assert!(
            boot_report(&fleet, "other").is_none(),
            "the box reported into another repo's store (stale only: {stale_only}): {said}"
        );
        if stale_only {
            assert!(
                !tree.join(".claude/skein").exists() && !tree.join(".claude/skein").is_symlink(),
                "the box linked a store when its own had no launch spec: {said}"
            );
            assert!(said.contains("no store found"), "{said}");
            continue;
        }
        assert_eq!(
            fs::read_link(tree.join(".claude/skein")).ok(),
            Some(fleet.store().join("skein")),
            "the box did not link its own repo's store: {said}"
        );
        let boot = boot_report(&fleet, "web").expect("the box wrote no boot report");
        let note = boot["claude_note"].as_str().unwrap_or_default();
        let other = fleet.repos.join("other/store/.claude");
        assert!(
            note.contains(other.to_string_lossy().as_ref()),
            "the boot report does not name the store passed over: {boot}"
        );
    }
}

/// **A box with no findable store says so, and makes nothing up**: no `$SKEIN_STORE`, no mount,
/// no link, and no launch spec anywhere. The kit takes the no-store branch, fails the start, links
/// nothing into the checkout, and reports into no store.
///
/// **What would make this fail**: a fallback that takes a directory with no launch spec for this
/// box as its store — the tracked `.claude` itself, which is what sandbox-bootstrap.sh falls back
/// to — so the kit writes its boot report there and makes `.claude/skein` in the checkout.
#[test]
fn a_box_with_no_findable_store_says_no_store() {
    let fleet = Fleet::make("storefind-none");
    launch_spec(&fleet, "other", "other-main");
    let Some((tree, said)) = restart_workshop(&fleet, Born::WorkshopUnmatched, true, |_| {}) else {
        return;
    };
    assert!(
        said.contains("no store found"),
        "the kit did not say it found no store: {said}"
    );
    assert!(
        said.starts_with("kit 1\n"),
        "a start with no store did not fail: {said}"
    );
    assert!(
        !tree.join(".claude/skein").exists() && !tree.join(".claude/skein").is_symlink(),
        "the kit put a .claude/skein into the checkout without a store"
    );
    assert!(boot_report(&fleet, "web").is_none() && boot_report(&fleet, "other").is_none());
}
