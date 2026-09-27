//! Which store a box's scripts report into, and under which name: box-self.sh's two answers, asked
//! by every script skein ships into a box (SKEIN-1174).
//!
//! Every script used to work the store out for itself, as `.claude/skein`'s target's parent, and
//! fall back to the checkout's own `.claude` when there was no link. A repo that tracks `.claude/`
//! has no link until the kit makes one, so on such a box the bootstrap pointed `$HOME/shared` into
//! the clone and wrote a boot report there, and every probe filed its signals where the host never
//! looks. These tests run the production scripts inside a box built by the launcher's own blocks,
//! and ask where what they wrote went.

use super::*;
use std::collections::BTreeMap;

/// The plugin and the kit, installed into the fixture fleet as `fleet::install_launcher` installs
/// them. Returns the turn-state variant's `probe/` directory, where every script below is run from.
fn install(fleet: &Fleet) -> String {
    let root = fleet.fleet_root.to_string_lossy().into_owned();
    let mut files = skein::probes::plugin_install_under(&root);
    files.push((
        format!("{root}/.skein/skein-startup.sh"),
        fs::read_to_string(script("kit/skein-startup.sh")).unwrap(),
    ));
    for (path, body) in files {
        let path = Path::new(&path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        "{}/probe",
        skein::runtime::turn_state_plugin_dir_under(&root)
    )
}

/// `web-main`'s checkout, the shape SKEIN-1174 was found in: a git repository that tracks
/// `.claude/` (its own settings file), with no `.claude/skein` link. Plus what three of the probes
/// need in order to have something to say: a journal, an uncommitted change for the diff, and a
/// pending hand-off brief in the store.
fn tracked_checkout(fleet: &Fleet) -> PathBuf {
    let tree = fleet.fleet_root.join("web-main/tree");
    fs::create_dir_all(tree.join(".claude")).unwrap();
    fs::write(tree.join(".claude/settings.json"), "{}\n").unwrap();
    fs::write(tree.join("README.md"), "one\n").unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", ".claude/settings.json", "README.md"],
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
    fs::write(tree.join("README.md"), "one\ntwo\n").unwrap();
    fs::create_dir_all(tree.join(".skein")).unwrap();
    fs::write(tree.join(".skein/journal.md"), "did x / next y\n").unwrap();
    let handoffs = fleet.store().join("handoffs");
    fs::create_dir_all(&handoffs).unwrap();
    fs::write(handoffs.join("web-main.codex.pending.md"), "a brief\n").unwrap();
    // What the bootstrap's shared-home helper and the guide refresh read: `ensure_store` puts it in
    // every store, and without it the guide refresh fails whichever store it found.
    fs::create_dir_all(fleet.store().join("skein")).unwrap();
    fs::write(fleet.store().join("skein/SHARED-HOME.md"), "the guide\n").unwrap();
    tree
}

/// Every file and link under `dir` except `.git/` (a `git diff` refreshes the index, which is not a
/// write anybody could mind), as path → contents or link target.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            if rel == Path::new(".git") {
                continue;
            }
            if p.is_symlink() {
                out.insert(rel, format!("-> {}", fs::read_link(&p).unwrap().display()));
            } else if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.insert(rel, String::from_utf8_lossy(&fs::read(&p).unwrap()).into());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Every script skein ships into a box that finds the store, run inside the box as the hooks, the
/// kit and the attach shell run them: the ten probes, the mailbox, the bootstrap, and — each by the
/// production string that runs it — the tracker's refresh, the attach shell's guide refresh and the
/// shared-home import. `$7` is `kit` to
/// run the kit first, the way `fleet::provision_script` runs it but with no `$SKEIN_STORE`, as a
/// restart does. Each line of output is `<script> <exit>`; the stderr of the three that speak is
/// kept in `$HOME/said`.
const HOPS: &str = r#"
tree="$1"; probe="$2"; home="$3"; guide="$4"; import="$5"; refresh="$6"; kit="$7"
export HOME="$home" SKEIN_BOX=web-main SANDBOX_VM_ID=skein-fleet CLAUDE_PROJECT_DIR="$tree"
# The sandbox's own, which the launcher does not let into a box (`inherited_env`); left set, the
# tracker's scripts would take it for the project.
unset SKEIN_STORE SKEIN_TMUX_SOCK SKEIN_STATE WORKSPACE_DIR
mkdir -p "$HOME/stub" "$HOME/.tmp"
printf '#!/bin/sh\nexit 1\n' > "$HOME/stub/tmux"; chmod +x "$HOME/stub/tmux"
printf '%s\n%s\n' '{"type":"user","message":{"role":"user","content":"hi"}}' \
  '{"type":"assistant","message":{"usage":{"input_tokens":9,"output_tokens":3},"content":[{"type":"text","text":"done"}]}}' \
  > "$HOME/transcript.jsonl"
printf 'something of mine\n' > "$HOME/imported-thing.md"
cd "$tree" || exit 1
if [ "$kit" = kit ]; then
  SKEIN_STARTUP_MARKERS="$HOME/.tmp" SKEIN_PROVISION=1 WORKSPACE_DIR="$tree" \
    bash "$(dirname "$(dirname "$probe")")/skein-startup.sh" </dev/null >"$HOME/kit.out" 2>&1
  echo "kit $?"
  own="^(bash|timeout -k [0-9]+ [0-9]+ bash) $(dirname "$probe")/[^ ]*sync-install\.sh"
  for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -f "$own" >/dev/null || break; sleep 1; done
fi
run() { name="$1"; input="$2"; shift 2; printf '%s' "$input" | bash "$probe/$name" "$@" >/dev/null 2>>"$HOME/said"; echo "$name $?"; }
run box-status.sh '' working
run box-session.sh '{"last_assistant_message":"the turn ended"}' stop
run box-diff.sh ''
run box-journal.sh ''
run box-task.sh '{"tool_input":{"todos":[{"status":"in_progress","activeForm":"Reading"}]}}'
run box-codex-task.sh '{"prompt":"look"}'
run box-token-usage.sh "{\"transcript_path\":\"$HOME/transcript.jsonl\"}"
run box-codex-telemetry.sh '{"tool_name":"Bash"}' tool
run box-handoff.sh '{}' codex
run mailbox.sh '' send --to somebody --body 'a note'
run sandbox-bootstrap.sh "{\"cwd\":\"$tree\"}"
PATH="$HOME/stub:$PATH" run box-pane.sh '' skein-agent
bash -c "$refresh" >/dev/null 2>>"$HOME/said"; echo "refresh $?"
bash -c "$guide" >/dev/null 2>>"$HOME/said"; echo "guide $?"
bash -c "$import" >/dev/null 2>>"$HOME/said"; echo "import $?"
"#;

/// What each script leaves in the store it found, relative to that store.
const PER_BOX: [&str; 12] = [
    "status/web-main.json",
    "sessions/web-main.json",
    "diffs/web-main.patch",
    "journals/web-main.md",
    "tasks/web-main.json",
    "telemetry/web-main.jsonl",
    "telemetry/.codex-turn/web-main.tools",
    "handoffs/web-main.codex.consumed.md",
    "status/web-main.pane.json",
    "sandboxes.json",
    "shared-home/imported-thing.md",
    "hook-log/web-main.jsonl",
];

/// Run [`HOPS`] in a box of `web-main` born `born`, the workspace a mount when `workspace`.
/// Returns what the box said: the lines of [`HOPS`], then `--- said ---` and every script's stderr.
fn run_hops(fleet: &Fleet, born: Born, workspace: bool, kit: bool) -> Option<String> {
    if !bwrap_works() {
        skip(
            "bwrap cannot create a user namespace here, so where a box's scripts find its store \
             was NOT exercised",
        );
        return None;
    }
    if !common::have("jq") || !common::have("git") || !common::have("tmux") {
        skip(
            "no jq, git or tmux here, and the scripts under test need all three, so where a box's \
             scripts find its store was NOT exercised",
        );
        return None;
    }
    let probe = install(fleet);
    let tree = fleet.fleet_root.join("web-main/tree");
    let home = fleet.dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let guide = skein::probes::start_invocations()
        .into_iter()
        .find(|(what, _)| *what == "the attach shell's guide refresh")
        .expect("the attach shell no longer refreshes the guide")
        .1;
    let import = skein::sharedhome::import_script(&["imported-thing.md".to_string()]);
    let refresh = skein::probes::start_invocations()
        .into_iter()
        .find(|(what, _)| *what == "the tracker's refresh")
        .expect("the tracker no longer refreshes from a box")
        .1;
    let mounts = if workspace {
        vec![fleet.repos.clone()]
    } else {
        vec![]
    };
    let out = fleet.in_box_mounting(
        born,
        &mounts,
        "s=\"$1\"; shift; exec bash -c \"$s\" hops \"$@\"",
        &[
            HOPS.to_string(),
            tree.to_string_lossy().into_owned(),
            probe,
            home.to_string_lossy().into_owned(),
            guide,
            import,
            refresh,
            if kit { "kit" } else { "" }.to_string(),
        ],
    );
    let said = fs::read_to_string(home.join("said")).unwrap_or_default();
    Some(format!(
        "{}--- said ---\n{said}",
        String::from_utf8_lossy(&out)
    ))
}

/// **A box whose repo tracks `.claude/` and has no `.claude/skein` link writes nothing into its
/// checkout, and reports into its store when the launcher recorded it** (SKEIN-1174).
///
/// Two starts of the same checkout, before the kit has linked anything:
/// - from a launcher that was handed the box's store ([`Born::Covered`]): it records the store in
///   the checkout's git directory, and every script finds it there — each leaves its signal in the
///   store, and the checkout is exactly as it was;
/// - from one that was not ([`Born::Unmatched`]): nothing says where the store is, so every script
///   refuses — the checkout is exactly as it was, the store holds none of this box's signals, and
///   the bootstrap and the mailbox say why.
///
/// **What would make this fail**:
/// - one script given back its own inline hop (`store="$root/.claude"; if [ -L "$store/skein" ] …;
///   [ -d "$store" ]`): with no link it takes the checkout's `.claude`, and writes there (the
///   checkout changed, in both starts);
/// - the launcher's record deleted from `box-session.sh`, or box-self.sh no longer reading it: the
///   first start's scripts find no store and write nothing (a signal is missing from the store);
/// - box-self.sh falling back to the checkout's `.claude` instead of refusing: the second start
///   writes into the checkout.
#[test]
fn a_box_whose_repo_tracks_claude_never_reports_into_its_checkout() {
    for born in [Born::Covered, Born::Unmatched] {
        let fleet = Fleet::make("storeone-tracked");
        let tree = tracked_checkout(&fleet);
        let before = snapshot(&tree);
        let store_before = snapshot(&fleet.store());
        let Some(said) = run_hops(&fleet, born, false, false) else {
            return;
        };
        assert_eq!(
            snapshot(&tree),
            before,
            "a script wrote into the checkout ({born:?}):\n{said}"
        );
        let store = fleet.store();
        if born == Born::Covered {
            assert_eq!(
                fs::read_to_string(tree.join(".git/skein-store")).ok(),
                Some(format!("{}\n", store.display())),
                "the launcher did not record the store"
            );
            for rel in PER_BOX {
                assert!(
                    store.join(rel).exists(),
                    "{rel} is not in the store the launcher recorded:\n{said}"
                );
            }
            assert!(
                fs::read_dir(store.join("mailbox")).is_ok_and(|mut d| d.next().is_some()),
                "the mailbox sent nothing into the store:\n{said}"
            );
            assert_eq!(
                fs::read_link(fleet.dir.join("home/shared")).ok(),
                Some(store.join("shared-home")),
                "$HOME/shared is not the store's shared home:\n{said}"
            );
            assert!(
                fleet.dir.join("home/.codex/AGENTS.md").is_file(),
                "the guide refresh found no store:\n{said}"
            );
            assert!(
                !said.contains("could not be found"),
                "a script said it found no store:\n{said}"
            );
        } else {
            assert!(
                !tree.join(".git/skein-store").exists(),
                "a launcher with no store recorded one"
            );
            let store_after = snapshot(&store);
            let new: Vec<_> = store_after
                .keys()
                .filter(|k| !store_before.contains_key(*k))
                .collect();
            assert!(
                new.is_empty(),
                "a script with no record and no link found a store anyway: {new:?}\n{said}"
            );
            assert!(
                !fleet.dir.join("home/shared").is_symlink(),
                "$HOME/shared was linked with no store found:\n{said}"
            );
            for who in ["[skein-bootstrap]", "[skein-mailbox]"] {
                assert!(
                    said.lines()
                        .any(|l| l.starts_with(who) && l.contains("could not be found")),
                    "{who} did not say it found no store:\n{said}"
                );
            }
        }
    }
}

/// **Every script finds the store the kit linked** — the kit, the ten probes, the mailbox, the
/// bootstrap, the tracker's refresh, the guide refresh and the shared-home import, on one checkout.
///
/// The workshop box restarting from a launcher that recorded nothing ([`Born::WorkshopUnmatched`]),
/// so the kit finds the store by its launch spec in the workspace mount and links `.claude/skein`,
/// and every other script has only that link to go on. The checkout also carries what the
/// bootstrap left before this fix: a real `.claude/skein` directory holding a boot report, which
/// would block the link. The kit moves it aside, says so, and links.
///
/// Every script's signal lands in the store the link names, and the checkout gains exactly what the
/// kit puts there: the link, the local settings file, and the moved-aside directory.
///
/// **What would make this fail**:
/// - the kit's move-aside deleted: the directory blocks the link, every other script refuses (a
///   signal is missing from the store) and the boot report says the link failed;
/// - box-self.sh reading the link's target itself rather than its parent, or resolving it anywhere
///   but where the kit pointed it: the scripts miss the store (a signal is missing);
/// - the kit's discovery and box-self.sh parting ways — say the kit linking a store the scripts
///   then refuse because it sits in the checkout: the same.
#[test]
fn every_script_reports_into_the_store_the_kit_linked() {
    let fleet = Fleet::make("storeone-agree");
    let tree = tracked_checkout(&fleet);
    // What the bootstrap used to leave in a checkout like this one.
    fs::create_dir_all(tree.join(".claude/skein/boot")).unwrap();
    fs::write(
        tree.join(".claude/skein/boot/web-main.json"),
        "{\"probe_revision\":\"\",\"jq\":true}\n",
    )
    .unwrap();
    let launch = fleet.store().join("skein/launch");
    fs::create_dir_all(&launch).unwrap();
    fs::write(
        launch.join("web-main.json"),
        "{\"agent\":\"claude\",\"branch\":\"\"}\n",
    )
    .unwrap();
    let before = snapshot(&tree);
    let Some(said) = run_hops(&fleet, Born::WorkshopUnmatched, true, true) else {
        return;
    };
    assert!(said.starts_with("kit 0\n"), "the kit failed:\n{said}");
    let linked = fs::read_link(tree.join(".claude/skein"))
        .unwrap_or_else(|_| panic!("the kit did not link .claude/skein:\n{said}"));
    let store = linked.parent().unwrap().to_path_buf();
    assert_eq!(store, fleet.store(), "the kit linked another store");
    for rel in PER_BOX {
        assert!(
            store.join(rel).exists(),
            "{rel} is not in the store the kit linked:\n{said}"
        );
    }
    assert!(
        fleet.dir.join("home/.codex/AGENTS.md").is_file(),
        "the guide refresh did not find the kit's store:\n{said}"
    );
    let boot: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(store.join("skein/boot/web-main.json")).unwrap())
            .unwrap();
    assert_eq!(boot["claude_link"], "merged", "{boot}");
    assert!(
        boot["claude_note"]
            .as_str()
            .is_some_and(|n| n.contains(".claude/skein.skein-old")),
        "the boot report does not say the old directory was moved: {boot}"
    );
    let after = snapshot(&tree);
    let gained: Vec<String> = after
        .keys()
        .filter(|k| !before.contains_key(*k))
        .map(|k| k.display().to_string())
        .collect();
    assert_eq!(
        gained,
        [
            ".claude/settings.local.json",
            ".claude/skein",
            ".claude/skein.skein-old/boot/web-main.json",
        ],
        "the checkout gained something other than what the kit puts there:\n{said}"
    );
}

/// **A kit that cannot say which box it is writes no boot report**, as no probe files a signal
/// under the sandbox's name: box-self.sh's rule, which the kit alone of every script did not apply
/// (it ran on from `SKEIN_BOX` to `SANDBOX_VM_ID` whatever sandbox it was in).
///
/// A shared sandbox — the launcher is installed in the fixture fleet — and a start with no
/// `SKEIN_BOX`, from a launcher that recorded the store, so the store is found and the only thing
/// between the kit and a boot report is the name.
///
/// **What would make this fail**: the kit's old chain put back (`SKEIN_BOX`, else
/// `SANDBOX_VM_ID`): it writes `skein/boot/skein-fleet.json`, a report for a box that does not exist.
#[test]
fn a_kit_that_cannot_name_its_box_reports_under_no_name() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so what the kit does without a box name \
             was NOT exercised",
        );
    }
    if !common::have("jq") || !common::have("git") || !common::have("tmux") {
        return skip(
            "no jq, git or tmux here, and the kit needs all three, so what it does without a box \
             name was NOT exercised",
        );
    }
    let fleet = Fleet::make("storeone-noname");
    let probe = install(&fleet);
    let tree = tracked_checkout(&fleet);
    let home = fleet.dir.join("home");
    let start = r#"
tree="$1"; home="$2"; kit="$3"
unset SKEIN_STORE SKEIN_BOX
export HOME="$home" SANDBOX_VM_ID=skein-fleet; mkdir -p "$HOME/.tmp"
SKEIN_STARTUP_MARKERS="$HOME/.tmp" SKEIN_PROVISION=1 WORKSPACE_DIR="$tree" \
  bash "$kit" </dev/null >"$HOME/kit.out" 2>&1
echo "kit $?"; cat "$HOME/kit.out"
own="^(bash|timeout -k [0-9]+ [0-9]+ bash) $(dirname "$kit")/[^ ]*sync-install\.sh"
for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -f "$own" >/dev/null || break; sleep 1; done
"#;
    let kit = Path::new(&probe)
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("skein-startup.sh");
    let said = fleet.in_box(
        Born::Workshop,
        start,
        &[
            tree.to_string_lossy().into_owned(),
            home.to_string_lossy().into_owned(),
            kit.to_string_lossy().into_owned(),
        ],
    );
    let said = String::from_utf8_lossy(&said).into_owned();
    assert!(
        fs::read_link(tree.join(".claude/skein")).ok() == Some(fleet.store().join("skein")),
        "the kit did not find the store the launcher recorded, so the name was never the \
         question:\n{said}"
    );
    let boot = fleet.store().join("skein/boot");
    let reports: Vec<String> = fs::read_dir(&boot)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        reports.is_empty(),
        "a kit with no box name wrote a boot report under some name: {reports:?}\n{said}"
    );
}
