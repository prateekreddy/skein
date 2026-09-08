//! The whole shared-sandbox launch, end to end, against a fake `sbx`.
//!
//! Everything the fleet path needs from the host is `sbx create` and `sbx exec`. But a sandbox is a
//! Linux machine with `bwrap`, `tmux` and `git` — and so is the machine running this test — so
//! `sbx exec <fleet> …` can simply mean "run it here" and the rest is genuinely exercised: a real
//! clone from a real remote, a real bwrap namespace, a real tmux server, real `nsenter` re-entry.
//!
//! What this deliberately does NOT cover is sbx's own behaviour — whether the flags are spelled
//! right, and where a workspace mount lands. Both were verified by hand against a real sandbox
//! instead (see `fleet::create_argv` and `fleet::fleet_workspace`), because no fake can answer them.
//!
//! Skipped rather than failed where the substrate is absent: this suite is about skein's logic, and
//! a machine without `bwrap` cannot host a box at all.

mod common;

use common::{bwrap_works, env_lock, env_pins, have, skip, Scratch};
use skein::config::{load_config, save_config, Config};
use skein::fleet::{
    anchor_from_launch, box_root, box_session_path, box_sock, box_state, clone_script,
    ensure_box_session, fleet_liveness, forget_fleet_liveness, heal_fleet, install_launcher,
    provision_script, resize_fleet, session_script, snapshot_box, start_box,
};
use skein::kit::ensure_store;
use skein::place::{forget_place, own_sandbox, place_of, record_place, shared_record, PlaceRecord};
use skein::probes::ensure_probe_in;
use skein::repos::{branch_of, save_repos, Repo};
use skein::sandbox::{destroy_box, stop_box};
use skein::sbx::{fleet_boxes, Liveness};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const FLEET: &str = "test-fleet";
const BOX: &str = "web-main";

fn sh(script: &str) -> String {
    let out = Command::new("bash")
        .arg("-lc")
        .arg(script)
        .output()
        .expect("bash");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A stand-in for `sbx` that runs the guest command locally.
///
/// `exec` drops its flags and the sandbox name and execs the rest, so an `nsenter` hop reaches the
/// same namespace it would in a real sandbox. `create` only has to succeed — the sandbox in this
/// test is the machine itself.
fn write_fake_sbx(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    fs::write(
        &p,
        r#"#!/usr/bin/env bash
verb="$1"; shift
case "$verb" in
  create) exit 0 ;;
  # Destroying the sandbox is the one irreversible step, so the harness records that it happened
  # rather than trusting resize's own report of whether it got that far.
  rm) : > "$SBX_RM_MARKER"; exit 0 ;;
  exec)
    while [ $# -gt 0 ]; do case "$1" in -*) shift ;; *) break ;; esac; done
    shift          # the sandbox name
    exec "$@" ;;
  *) echo "fake sbx: unsupported verb $verb" >&2; exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A bare repo with one commit on `main`, standing in for the remote a box clones from.
fn write_remote(root: &Path) -> String {
    let remote = root.join("remote.git");
    let seed = root.join("seed");
    let git = "git -c user.email=t@example.com -c user.name=test -c init.defaultBranch=main";
    sh(&format!(
        "set -e; git init --bare -q -b main {r}; {git} init -q {s}; \
         cd {s}; echo hello > README.md; {git} add -A; {git} commit -qm seed; \
         {git} remote add origin {r}; {git} push -q origin main",
        r = remote.display(),
        s = seed.display(),
        git = git
    ));
    remote.to_string_lossy().into_owned()
}

/// Deliberately **not** under `/tmp` or `$HOME`: a box binds its own directories over both, so a
/// box root beneath either is unreadable from outside — and `box-session.sh` refuses it outright.
/// The first run of this test put the scratch in `/tmp` and was correctly turned away.
///
/// The prefix is unchanged on purpose: the leaked-process gate counts `ps` lines matching
/// `skein-fleet-it-`, and renaming it would turn that count into a zero that proves nothing.
fn scratch_named(what: &str) -> Scratch {
    Scratch::boxes(&format!("skein-fleet-it-{what}"))
}

/// A home for the fleet sandbox, with an agent CLI in it where the real one lives.
///
/// Two things this replaces, both of which made the launch test depend on the machine it ran on.
/// `$HOME` was the developer's own — `sbx exec` here means "run it on this machine", so the box was
/// placed over a real home directory. And `command -v claude` inside the box was read as "the agent
/// survived the launch" when what it actually asked was "is Claude Code installed here": the suite
/// failed outright on a machine without it, and passed for the wrong reason on this one, where
/// `claude` is at `/usr/local/share/npm-global/bin/claude` — outside `$HOME` entirely, so replacing
/// `$HOME` wholesale would not have moved it.
///
/// The stub goes at `~/.local/bin/claude`, which is where Claude Code installs itself and therefore
/// the only placement under which that assertion means what it says: the launcher binds the box's
/// private home over `$HOME`, so a launch that replaced the home rather than binding into it takes
/// this path with it and `command -v claude` stops answering.
fn sandbox_home_with_agent(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = root.join("sandbox-home");
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(bin.join("claude"), "#!/bin/sh\necho 'stub agent'\n").unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    home
}

/// One box, from nothing to running to gone.
///
/// A single test rather than several: each step consumes the previous one's real side effects (the
/// anchor pid only exists once the session starts, and the placement only means anything while that
/// pid lives), so splitting them would mean either re-running the launch per assertion or sharing
/// mutable state between tests through the environment — which is exactly what makes suites flaky.
#[test]
fn a_box_lives_and_dies_inside_the_fleet_sandbox() {
    let _env = env_lock();
    if !bwrap_works() || !have("tmux") || !have("git") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let root = scratch_named("box");
    write_fake_sbx(&root.join("bin"));
    let remote = write_remote(&root);
    // Stand in for the SANDBOX's home, exactly as the sibling test below does and for the same
    // reason: `sbx exec` here means "run it on this machine", so a box placed over the real `$HOME`
    // is a box driving the developer's own home directory — and this one writes a credential file
    // into `~/.claude` (below) and reads `$HOME` into three `PlaceRecord`s.
    let sandbox_home = sandbox_home_with_agent(&root);

    // Bound after `root`, so every name stops pointing into the scratch tree before the tree is
    // removed — and `$HOME` in particular goes back on the failing path, where the `set_var` on
    // this test's last line used to be unwound past. A test that leaves `$HOME` naming a deleted
    // scratch directory is the worst of these to debug: everything after it in the binary reads
    // the developer's home as gone.
    let mut pins = env_pins();
    pins.set(
        "PATH",
        format!(
            "{}:{}:{}",
            root.join("bin").display(),
            sandbox_home.join(".local/bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("HOME", &sandbox_home)
    .set("SKEIN_HOME", root.join("skein"))
    // /boxes needs root to create; the seam exists so this path is testable at all.
    .set("SKEIN_FLEET_ROOT", root.join("boxes"));
    // **Named, because every address below is checked against it.** `Place` refuses an address for
    // a sandbox that is not the one this process is standing in — there is no `sbx` hop left to
    // reach another with (SKEIN-576) — and "the one it is standing in" is the configured fleet. A
    // fixture that left this at the default would be asking about somebody else's sandbox and
    // getting told so, which is correct and not what this test is about.
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");

    // ---- the launcher reaches a sandbox that has never seen this repo ----
    install_launcher(FLEET).expect("install box-session.sh");
    let launcher = box_session_path();
    assert!(
        Path::new(&launcher).exists(),
        "the launcher is embedded and installed over stdin, not served from a repo's store"
    );

    // ---- a checkout, from the remote at the base branch ----
    let place = own_sandbox(FLEET);
    place
        .exec(
            &clone_script(BOX, &remote, "main", "feat/auth", ""),
            Duration::from_secs(120),
        )
        .expect("clone");
    let tree = format!("{}/tree", box_root(BOX));
    assert_eq!(
        sh(&format!("git -C {tree} rev-parse --abbrev-ref HEAD")),
        "feat/auth",
        "the box starts on its own branch, cut from the remote base"
    );
    // A second box of the same name must not inherit this tree — it may hold uncommitted work.
    assert!(
        place
            .exec(
                &clone_script(BOX, &remote, "main", "feat/auth", ""),
                Duration::from_secs(60)
            )
            .is_err(),
        "an existing checkout is refused, not reused"
    );

    // ---- the session, and the anchor that outlives its launcher ----
    let launched = place
        .exec(
            &session_script(
                BOX,
                "skein-agent",
                // The agent records the environment it was STARTED with, which is the only place
                // that answer exists: a later `nsenter` gets a fresh environment, so asking the
                // running box would answer a different question. See the scratch assertion below.
                "printf '%s\\n' \"${CLAUDE_CODE_TMPDIR:-the shared /tmp}\" > /tmp/scratch.env; \
                 mkdir -p \"${CLAUDE_CODE_TMPDIR:-/tmp/nowhere}\"; \
                 echo agent-started > /tmp/agent.log; exec sleep 400",
            ),
            Duration::from_secs(60),
        )
        .expect("start the box");
    // Read off the launcher's own stdout, never out of the box's tree. The pidfile there is bound
    // read-write, so a box can put a sibling's server pid in it — and skein entering that would be
    // executing in the sibling's namespace with the box's name on it.
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    assert!(
        Path::new(&format!("/proc/{anchor}")).exists(),
        "the launcher has exited by now; the anchor must be the tmux server, which has not"
    );
    // And it is the same process the box was told to write down — the file stays for the box's own
    // use, so the two must agree while nobody is lying.
    let claimed = fs::read_to_string(skein::fleet::box_pidfile(BOX))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    assert_eq!(
        claimed,
        anchor.to_string(),
        "the launcher reported one pid and wrote another"
    );

    // ---- the ceiling that keeps one box from taking the fleet down ----
    // The limit is applied to the LAUNCHER before it execs bwrap, so the tmux server and everything
    // the agent forks inherit it. Moving the anchor pid afterwards would move one process and leave
    // its children outside — a limit that looks applied and holds nothing. Checked on the anchor
    // precisely because it is a process the launcher spawned, not the launcher itself.
    //
    // Skipped where the substrate can't do it: box-session.sh warns and runs the box uncapped rather
    // than refusing to start it, so the absence of cgroup delegation is not a test failure.
    // Which case this machine is in is read from the record the LAUNCH wrote, and not from a `sudo`
    // of the test's own. The probe here was `sudo mkdir -p /sys/fs/cgroup/skein` — with no `-n`, so
    // on a machine whose sudo wants a password it blocked on a prompt with `cargo test`'s output
    // captured and nothing on screen to answer, and on a machine with passwordless sudo it made a
    // root-owned cgroup on the developer's host to re-ask a question the launch had already
    // answered. `box-session.sh:1122-1160` writes `limits.state` as `capped <…>` or
    // `uncapped no-cgroup-delegation` on every start.
    //
    // Both branches assert, from opposite sides of the same agreement: whichever the launch says,
    // the anchor's own cgroup line has to say the same. Recording "uncapped" while the box IS in
    // its cgroup, or "capped" while it is not, is the failure either way — and the second is what
    // "nothing caps them" looked like before this existed.
    let cgroup_of_anchor = sh(&format!("cat /proc/{anchor}/cgroup 2>/dev/null"));
    let state = fs::read_to_string(format!("{}/limits.state", box_root(BOX))).unwrap_or_default();
    let in_its_cgroup = cgroup_of_anchor.contains(&format!("/skein/{BOX}"));
    if state.starts_with("capped ") {
        assert!(
            in_its_cgroup,
            "the launch recorded {state:?}, but the box's processes are outside its cgroup, so \
             nothing caps them: {cgroup_of_anchor}"
        );
        let limit = fs::read_to_string(format!("/sys/fs/cgroup/skein/{BOX}/memory.max"))
            .unwrap_or_default()
            .trim()
            .to_string();
        assert!(
            limit.parse::<u64>().map(|b| b > 0).unwrap_or(false),
            "the cgroup exists but holds no memory ceiling: {limit:?}"
        );
    } else {
        // Recorded, not merely logged: skein keeps a command's stdout and drops its stderr on
        // success, so "this box has no ceiling" would vanish precisely when the box started fine.
        // The file is how anything later can still ask — which is why its absence is a failure
        // rather than a second way of skipping.
        assert!(
            state.starts_with("uncapped "),
            "the launch left no readable answer to whether this box got a ceiling: {state:?}"
        );
        assert!(
            !in_its_cgroup,
            "the launch recorded {state:?} while the box sits in its own cgroup — the one record \
             anything later can read is wrong: {cgroup_of_anchor}"
        );
        eprintln!(
            "SKIPPED the cgroup ceiling assertions: this machine gave the launch no cgroup \
             delegation ({state})"
        );
    }

    // The stamp that makes the anchor an identity rather than a number, read the way skein reads
    // it — from this machine, which is the sandbox for this test.
    let generation = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .expect("a boot id")
        .trim()
        .to_string();
    let ns_start = sh(&format!(
        "sed -n 's/.*) //p' /proc/{anchor}/stat | cut -d' ' -f20"
    ))
    .trim()
    .parse::<u64>()
    .expect("the anchor's start time");

    record_place(
        BOX,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            // The sandbox's own HOME: a box no longer gets an empty private one. `claude` lives at
            // ~/.local/bin and its credentials at ~/.claude, so replacing HOME wholesale left a box
            // with no agent to run. Privacy comes from binding the few paths that must differ.
            home: std::env::var("HOME").unwrap_or_default(),
            tree: tree.clone(),
            sock: box_sock(BOX),
            generation: generation.clone(),
            ns_start,
            launcher: String::new(),
            ceiling: String::new(),
            ..Default::default()
        },
    )
    .unwrap();

    // The guard is not decoration, and this is the assertion that says so: the same box, one field
    // of its recorded identity wrong, is refused rather than entered. Every way of being wrong
    // means the box is gone — the pid still exists, and it belongs to something else.
    record_place(
        BOX,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            home: std::env::var("HOME").unwrap_or_default(),
            tree: tree.clone(),
            sock: box_sock(BOX),
            generation,
            ns_start: ns_start + 1,
            launcher: String::new(),
            ceiling: String::new(),
            ..Default::default()
        },
    )
    .unwrap();
    let refused = place_of(BOX)
        .expect("placed")
        .exec("pwd", Duration::from_secs(30))
        .expect_err("a recycled pid must not be entered");
    assert!(
        refused.contains("is gone") && refused.contains(&anchor.to_string()),
        "the refusal says the box is gone and names the pid: {refused}"
    );

    record_place(
        BOX,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            home: std::env::var("HOME").unwrap_or_default(),
            tree: tree.clone(),
            sock: box_sock(BOX),
            generation: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .unwrap()
                .trim()
                .to_string(),
            ns_start,
            launcher: String::new(),
            ceiling: String::new(),
            ..Default::default()
        },
    )
    .unwrap();

    // ---- and now every ordinary skein call lands inside that box ----
    let boxed = place_of(BOX).expect("placed");
    assert_eq!(
        boxed.sandbox, FLEET,
        "the box is not its own sandbox any more"
    );
    assert_eq!(
        boxed.exec("pwd", Duration::from_secs(30)).unwrap().trim(),
        tree,
        "scripts start at the repo root, which nsenter does not inherit"
    );
    // The agent and its credentials survive, because HOME is not replaced any more.
    //
    // The agent here is the stub `sandbox_home_with_agent` put at `~/.local/bin/claude`, which is
    // where Claude Code installs itself, and **the resolved path is what is asserted** rather than
    // "some claude answered".
    //
    // `command -v claude >/dev/null && echo yes` was the old spelling, and it asked the machine, not
    // the box: it fails on any machine without Claude Code installed, and on this one it stays green
    // with `.local` cut out of `share_paths` entirely, because `/usr/local/share/npm-global/bin/
    // claude` is still on the inherited PATH inside the box. The launcher binds the box's private
    // home over `$HOME` (`box-session.sh:939`) and binds `share_paths` back on top of it, so the
    // agent's own path is the one thing that says the share survived the bind.
    assert_eq!(
        boxed
            .exec("command -v claude", Duration::from_secs(30))
            .unwrap()
            .trim(),
        sandbox_home.join(".local/bin/claude").display().to_string(),
        "a box with no agent CLI cannot start one — this is what binding all of HOME broke"
    );
    // ...while the state that must differ per box really does. Two boxes sharing this file claim
    // work as the SAME agent, which silently defeats the atomic claim the tracker exists for.
    boxed
        .exec(
            "mkdir -p ~/.config/sync && echo mine > ~/.config/sync/env",
            Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(format!("{}/home/.config/sync/env", box_root(BOX)))
            .unwrap()
            .trim(),
        "mine",
        "the box's tracker identity landed in its own copy"
    );
    assert_eq!(
        boxed
            .exec("cat /tmp/agent.log", Duration::from_secs(30))
            .unwrap()
            .trim(),
        "agent-started",
        "the agent really ran inside the namespace"
    );

    // ...and it was started with a scratch directory of its own, rather than being left to derive
    // one from the shared /tmp.
    //
    // Claude Code puts its temp directory at `${os.tmpdir()}/claude-<uid>` and REFUSES to start
    // when that path is somebody else's. In a fleet that path is the sandbox's shared /tmp, and on
    // the owner's fleet something running as root got there first: every model call, and a login
    // whose OAuth had otherwise completed, came back `Temp directory /tmp/claude-1000 is owned by
    // uid 0` (SKEIN-289).
    //
    // Read off the file the AGENT wrote, never asked of the running box: `boxed.exec` enters the
    // namespace fresh through nsenter, so it would report its own environment and pass whatever
    // the launcher did. And driven through `session_script` + the real launcher + real bwrap,
    // because a grep for the string in `box-session.sh` proves the string is present, not that the
    // environment a box starts with carries it.
    let scratch = boxed
        .exec("cat /tmp/scratch.env", Duration::from_secs(30))
        .unwrap()
        .trim()
        .to_string();
    let box_home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(
        scratch,
        skein::fleet::model_scratch_dir(Path::new(&box_home))
            .display()
            .to_string(),
        "the box's agent starts in the shared /tmp, where anything that got there first stops the \
         runtime from starting at all"
    );
    // And the path is the box's OWN, not one every box in the sandbox shares: the launcher binds
    // the box's private home over $HOME, so the directory the agent made inside the namespace has
    // to land under the box's root out here.
    assert!(
        Path::new(&format!(
            "{}/home/{}",
            box_root(BOX),
            skein::fleet::MODEL_SCRATCH
        ))
        .is_dir(),
        "the agent's scratch directory is not in this box's private home, so every box in the \
         sandbox shares one — which is the thing a per-box path exists to prevent"
    );

    // ---- provisioning: the same script the kit runs, inside the box ----
    // A box that never got this comes up looking entirely healthy and simply never reports — no
    // hooks, no probe, no tracker. It is the one gap that cannot be seen from the outside, so it is
    // asserted from the inside, on the artefacts the script actually leaves.
    let store = root.join("skein/repos/web/store/.claude");
    ensure_store(&store).expect("a store to provision against");
    // Written by the ordinary launch path (`write_launch_spec_for_agent`), which the fleet shares —
    // keyed on the box name, which is exactly the identity `SKEIN_BOX` supplies inside the box.
    fs::write(
        store.join(format!("skein/launch/{BOX}.json")),
        r#"{"branch":"feat/auth","agent":"claude"}"#,
    )
    .unwrap();
    boxed
        .exec(
            &provision_script(BOX, &store.to_string_lossy()),
            Duration::from_secs(120),
        )
        .expect("provision the box");
    assert_eq!(
        boxed
            .exec("readlink .claude", Duration::from_secs(30))
            .unwrap()
            .trim(),
        store.to_string_lossy(),
        "the store link is what makes hooks, skills and the probe resolve at all"
    );
    // `shared` is scoped to a REPO, not to a sandbox — the two were one object when a box WAS a
    // sandbox. So it must be this box's own symlink into its own repo's store, not the fleet
    // sandbox's directory bound through to every box regardless of which repo they are checkouts of.
    // Binding it also failed closed: shared-home.sh refuses to replace a real path, and it gates
    // startup, so every box in the fleet would have failed to provision.
    assert_eq!(
        boxed
            .exec("readlink ~/shared", Duration::from_secs(30))
            .unwrap()
            .trim(),
        store.join("shared-home").to_string_lossy(),
        "a box's shared workspace must resolve to its own repo's store"
    );
    // The boot report is per BOX, not per sandbox. Every box here reports the same SANDBOX_VM_ID,
    // so without an explicit identity they would overwrite each other — one box's diagnosis
    // standing in for all of them, which is worse than none.
    let boot = store.join(format!("skein/boot/{BOX}.json"));
    let report = fs::read_to_string(&boot).expect("boot report at the box's own name");
    assert!(
        report.contains("\"claude_link\":\"linked\"")
            && report.contains("\"shared_home\":\"linked\""),
        "the report is how a dark box is diagnosed without entering it: {report}"
    );
    // The store is infrastructure that must never show up as a worktree change.
    assert_eq!(
        boxed
            .exec("git status --porcelain", Duration::from_secs(30))
            .unwrap()
            .trim(),
        "",
        "the linked store leaked into the box's diff"
    );
    // The isolation, from the other side: the box's /tmp is invisible to everyone else.
    assert!(
        !Path::new("/tmp/agent.log").exists(),
        "the box's /tmp leaked into the sandbox's"
    );
    // Bytes, not text: this is the path the Files tab serves images and PDFs down.
    boxed
        .write("cat > blob", &[0u8, 159, 146, 150], Duration::from_secs(30))
        .unwrap();
    assert_eq!(
        boxed.bytes("cat blob", Duration::from_secs(30)).unwrap(),
        vec![0u8, 159, 146, 150],
        "a lossy UTF-8 hop here corrupts every binary the box serves"
    );

    // ---- the snapshot that has to survive a resize ----
    // Changing the fleet's memory or CPUs means destroying the sandbox, and every box's checkout is
    // VM-local — that is what makes builds fast and what makes this the one path where a bug costs
    // real work. So each kind of not-yet-pushed state is made distinct here and checked separately:
    // a commit that is on no remote, a staged change, an unstaged change, and an untracked file.
    boxed
        .exec(
            "git -c user.email=t@e.com -c user.name=t commit -qm local --allow-empty \
             && echo staged > s.txt && git add s.txt \
             && echo hello-unstaged >> README.md \
             && echo loose > u.txt",
            Duration::from_secs(60),
        )
        .unwrap();
    let head = boxed
        .exec("git rev-parse HEAD", Duration::from_secs(30))
        .unwrap()
        .trim()
        .to_string();
    // A transcript and a credential, side by side in the box's private HOME exactly as the real
    // agent leaves them — so the snapshot has to distinguish them rather than take the directory.
    boxed
        .exec(
            "mkdir -p ~/.claude/projects/-boxes-web-main-tree \
             && echo '{\"type\":\"user\"}' > ~/.claude/projects/-boxes-web-main-tree/sess.jsonl \
             && echo 'SECRET-TOKEN' > ~/.claude/.credentials.json",
            Duration::from_secs(30),
        )
        .unwrap();

    let relative = snapshot_box(BOX, &store.to_string_lossy(), "resize-run").expect("snapshot");
    assert!(
        relative.starts_with("skein/handoff-snapshots/"),
        "the provisioning script refuses any path outside that prefix: {relative}"
    );
    let snap = store.join(&relative);
    for artifact in [
        "repo.bundle",
        "index.patch",
        "worktree.patch",
        "untracked.tgz",
    ] {
        assert!(
            snap.join(artifact).metadata().map(|m| m.len()).unwrap_or(0) > 0,
            "{artifact} is empty — one whole class of unpushed work would be lost"
        );
    }
    // Restore into a fresh clone, which is exactly what a resized box comes up as. `--all` rather
    // than `HEAD`: a bundle of HEAD alone drops every other local branch the box was carrying.
    let restored = root.join("restored");
    sh(&format!(
        "set -e; git clone -q --branch main {remote} {r}; cd {r}; \
         git fetch -q {s}/repo.bundle 'refs/heads/*:refs/remotes/snap/*'; \
         git checkout -q -B feat/auth snap/feat/auth; \
         git apply --binary --index {s}/index.patch; \
         git apply --binary {s}/worktree.patch; \
         tar -xzf {s}/untracked.tgz",
        r = restored.display(),
        s = snap.display()
    ));
    assert_eq!(
        sh(&format!("git -C {} rev-parse HEAD", restored.display())),
        head,
        "the unpushed commit did not survive the bundle"
    );
    assert_eq!(
        sh(&format!(
            "git -C {} diff --cached --name-only",
            restored.display()
        )),
        "s.txt",
        "the staged change came back unstaged, which loses the index"
    );
    assert!(
        sh(&format!("cat {}/README.md", restored.display())).contains("hello-unstaged"),
        "the unstaged change was lost"
    );
    assert_eq!(
        sh(&format!("cat {}/u.txt", restored.display())),
        "loose",
        "the untracked file was lost — no patch covers these"
    );

    // ---- the conversation travels; the credential does not ----
    // A rebuilt box must resume the session rather than open a new one against a familiar tree, and
    // the transcript is addressed by the cwd slug, which survives because the box comes back at the
    // same path. The credential must NOT travel: the store is host-side shared data, and the box is
    // re-seeded with auth from the sandbox anyway. An allowlist is what makes the second half hold
    // for files that do not exist yet.
    // The transcript is not merely snapshot-able, it is already on the HOST — bound in from
    // box_state, so it survives the sandbox dying rather than only surviving a planned resize. An
    // OOM or a hand-run `sbx rm` never runs a snapshot; this is what covers those.
    let host_transcript =
        PathBuf::from(box_state(BOX)).join("claude-projects/-boxes-web-main-tree/sess.jsonl");
    assert_eq!(
        fs::read_to_string(&host_transcript)
            .expect("the conversation must be readable from the host")
            .trim(),
        "{\"type\":\"user\"}",
        "the box wrote its transcript into VM-local disk, where a crash would take it"
    );
    // And the credential did NOT follow it out: only the record directories are host-bound.
    assert!(
        !PathBuf::from(box_state(BOX))
            .join("claude-projects/.credentials.json")
            .exists()
            && !fs::read_dir(box_state(BOX))
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().contains("credential")),
        "host-binding the record must not drag the credentials out with it"
    );

    // The snapshot carries only what is genuinely VM-local. The transcript is host-bound already,
    // so copying it out to the store and straight back would traverse virtiofs twice to arrive at
    // the file that never moved.
    let members = sh(&format!("tar -tzf {}/agent-state.tgz", snap.display()));
    assert!(
        !members.contains(".claude/projects"),
        "the host-bound transcript was copied redundantly through the store: {members}"
    );
    assert!(
        !members.contains("credentials"),
        "a credential reached the shared store: {members}"
    );

    // ---- a resize that cannot save a box must not destroy the sandbox ----
    // The entire safety property of resize_fleet is its ordering: everything comes out first, and a
    // single failure leaves the sandbox standing with every box still in it. A partial snapshot is
    // not a partial resize, it is lost work — and the box that loses it is precisely the one whose
    // state could not be read. Asserted against the marker the fake sbx writes, not against the
    // error message, because the failure being guarded is "it destroyed things anyway".
    let rm_marker = root.join("sbx-rm-happened");
    pins.set("SBX_RM_MARKER", &rm_marker);
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..load_config()
    })
    .unwrap();
    // This box belongs to no registered repo, so its work has nowhere to be saved.
    //
    // `drop_docker` so the refusal under test is the one this asserts: the fake sbx has no Docker to
    // ask, and skein reads silence from Docker as a reason to stop — correctly, but that would make
    // this test pass for the wrong reason and stop covering the ordering it exists for.
    let err = resize_fleet("8g", "4", "", true).expect_err("resize must refuse");
    assert!(
        err.contains("no registered repo") && err.contains("untouched"),
        "the refusal must say the sandbox was left alone: {err}"
    );
    assert!(
        !rm_marker.exists(),
        "the sandbox was destroyed despite a box whose work could not be saved"
    );
    // The fleet's name is left set from here on. This used to blank it — harmless while an address
    // for another sandbox merely grew an `sbx exec` prefix — and blanking it now names a DIFFERENT
    // sandbox: `load_config` repairs an empty name to the default (SKEIN-484), and `Place` refuses
    // an address for any sandbox but the one this process is standing in, because there is no `sbx`
    // hop left to reach one with (SKEIN-576). Every call below addresses this fixture's fleet.

    // ---- liveness, without entering anything ----
    let sock = box_sock(BOX);
    assert!(
        sh(&format!(
            "tmux -S {sock} has-session -t skein-agent && echo yes"
        )) == "yes",
        "the socket sits outside the private mounts so the fleet can be listed from outside"
    );

    // ---- teardown through skein's own lifecycle, not a hand-rolled kill ----
    // stop_box used to run `sbx stop <box>`, which for a shared box either misses or stops an
    // unrelated sandbox carrying the same name. It must reach this box's server instead.
    stop_box(BOX).expect("stop the box");
    for _ in 0..40 {
        if !Path::new(&format!("/proc/{anchor}")).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !Path::new(&format!("/proc/{anchor}")).exists(),
        "killing the server must end the box"
    );
    forget_place(BOX);
    assert!(
        place_of(BOX).is_none(),
        "a forgotten box must resolve to nothing, not to a dead namespace — and not to a sandbox \
         named after it, which was the per-VM model and is gone"
    );

    // The cgroup outlives the box's filesystem — rmdir only succeeds once the server is gone, which
    // the wait above has already established. destroy_box does this for a real box; stop_box (used
    // here) deliberately does not, because a stopped box is meant to be startable again.
    //
    // `-n`, and only where the launch actually made a cgroup: a `sudo` that wants a password has
    // nothing to prompt on under `cargo test`, and there is nothing here worth blocking a suite to
    // tidy up.
    if state.starts_with("capped ") {
        let _ = Command::new("sudo")
            .args(["-n", "rmdir", &format!("/sys/fs/cgroup/skein/{BOX}")])
            .status();
    }
}

/// The whole of `start_box`, rather than its pieces called in the right order by hand.
///
/// The test above assembles the launch itself — install, clone, session — and that is precisely why
/// it kept passing while every real launch failed. Each bug lived in the *seams*: the placement
/// recorded an empty HOME, so provisioning resolved `$HOME/shared` to `/shared`; no launch spec was
/// written, so the box stayed on the clone's default branch; the fleet root was never created. None
/// of it is visible unless the entry point itself is the thing under test.
#[test]
fn start_box_leaves_a_box_that_is_actually_usable() {
    let _env = env_lock();
    if !bwrap_works() || !have("tmux") || !have("git") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let root = scratch_named("start");
    write_fake_sbx(&root.join("bin"));
    let remote = write_remote(&root);
    let name = "demo-smoke";

    // Bound after `root`, so every name stops pointing into the scratch tree before the tree is
    // removed — `$HOME` below included, which the `set_var` on this test's last line put back only
    // when the test passed.
    let mut pins = env_pins();
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("SKEIN_HOME", root.join("skein"))
    .set("SKEIN_FLEET_ROOT", root.join("boxes"))
    // No runtime installs: this harness's `sbx exec` runs on THIS machine, so the substrate step
    // would npm-install an agent runtime onto the developer's box. It did exactly that once.
    .set("SKEIN_RUNTIME_PACKAGES", "");
    // Stand in for the SANDBOX's home. Without this the fake `sbx` reports this machine's real
    // $HOME, and the launcher would seed a box from — and reconcile credentials back into — the
    // developer's own ~/.claude. A test must not be able to touch that; the first run of this test
    // read a real credential file, which is exactly how it was caught.
    let sandbox_home = root.join("sandbox-home");
    fs::create_dir_all(sandbox_home.join(".claude")).unwrap();
    // Shaped like the real thing, because the sync now reads it rather than moving it about: it
    // tells a login from the husk a logout leaves, and it carries only the login across. A
    // placeholder like `{"tok":"…"}` passed straight through the old copy and would say nothing
    // about either. The `mcpOAuth` block is what the sandbox must KEEP when a box's login arrives —
    // those grants are per-repo, and copying the file whole used to discard the receiver's.
    fs::write(
        sandbox_home.join(".claude/.credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"SEEDED","refreshToken":"r"},"mcpOAuth":{"sync|sandbox":{"accessToken":"GRANT-SHARED"}}}"#,
    )
    .unwrap();
    pins.set("HOME", &sandbox_home)
        // The fleet sandbox already exists, so `ensure_fleet` goes straight to substrate +
        // launcher.
        .set("SKEIN_LS_CMD", format!("echo '[{{\"name\":\"{FLEET}\"}}]'"));

    let store = root.join("store");
    fs::create_dir_all(&store).unwrap();
    ensure_store(&store).expect("seed the store");
    ensure_probe_in(&store).expect("seed the store's scripts");
    let repo = Repo {
        read_prs: false,
        id: "demo".into(),
        source: remote.clone(),
        store: store.to_string_lossy().into_owned(),
        agent: "claude".into(),
        plane_project: String::new(),
        sync_connection: String::new(),
        review_queue: true,
        sync_gateway_url: String::new(),
        ..Default::default()
    };
    save_repos(std::slice::from_ref(&repo)).expect("register the repo");
    let mut config = load_config();
    config.fleet_sandbox = FLEET.into();
    save_config(&config).expect("turn the fleet on");

    // ---- the gate, warmed before the act, so the act has something wrong to settle ----
    // Read once here and once after each of the three acts below, with no `forget_fleet_liveness()`
    // in between. That is the whole test: the gate serves its last good answer while it refreshes
    // behind the caller, so the only thing that can make the second read agree with the fleet is the
    // act having invalidated it. This is also the only place the check can live — `cfg!(test)` is
    // false for the library these tests link, so the gate is real here and disabled in unit tests.
    assert_eq!(
        fleet_liveness().get(name).copied(),
        None,
        "nothing is placed under this name yet, so the warm answer must not mention it"
    );

    start_box(
        name,
        &repo,
        "feat/smoke",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect("start the box");

    // Remove `start_box`'s settle and this reads back the map from before the launch — no entry at
    // all for a box that is up and whose row the person who pressed the button is looking at.
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(true),
        "starting a box must settle the liveness gate, or the board serves the pre-start picture"
    );

    // The placement must carry a real HOME: `Place::wrap` exports it, and an empty one sends every
    // `$HOME/…` path in provisioning to the filesystem root.
    let placed = shared_record(name).expect("the box is placed");
    assert!(
        placed.home.starts_with('/') && placed.home.len() > 1,
        "a placement with no HOME makes every box command write to /: {:?}",
        placed.home
    );

    // And which isolation it got. The whole chain, in one assertion, because every link is silent
    // on its own: `install_launcher` stamps the script it installs, the script reports the stamp it
    // was given, and `start_box` records what was reported. Break any of them and a box that IS
    // covered reads as uncovered forever — a restart that never clears the thing asking for it.
    //
    // A box keeps the mount namespace it was born with, so this is the only moment the answer
    // exists; there is nothing on the host to check it against afterwards, which is the whole
    // reason the value has to travel with the box.
    assert_eq!(
        placed.launcher,
        skein::fleet::launcher_revision(),
        "the box that this launcher just started does not know which launcher started it"
    );

    // And what bounds it. The value depends on the machine — a scratch fleet with no cgroup
    // delegation reports `uncapped no-cgroup-delegation` and that is the honest answer — so what is
    // asserted is that the box **said something**, which is the whole of the defect. Nothing on the
    // host can read `limits.state`; it is written inside the sandbox, so a box that reported nothing
    // is a box whose ceiling is unknowable, and it used to look exactly like a bounded one.
    assert!(
        !placed.ceiling.is_empty(),
        "the box did not say what bounds its memory, so an uncapped one is invisible again"
    );
    assert!(
        skein::fleet::is_capped(&placed.ceiling) || placed.ceiling.starts_with("uncapped "),
        "the ceiling state is neither capped nor a named reason: {:?}",
        placed.ceiling
    );

    // The launch spec is how the box, and skein, learn which branch this box is for. Asserting on
    // the checkout alone proves nothing: `clone_script` checks the branch out itself, so that stays
    // green with no spec at all — while the box's own restart falls back to the clone's default and
    // the board reports the wrong branch.
    let tree = format!("{}/tree", box_root(name));
    assert_eq!(
        sh(&format!("git -C {tree} rev-parse --abbrev-ref HEAD")),
        "feat/smoke",
        "the box is on its own branch, not the clone's default"
    );
    assert_eq!(
        branch_of(name).as_deref(),
        Some("feat/smoke"),
        "skein must be able to read the box's branch back from its launch spec"
    );

    // Provisioning ran *inside* the box: its shared home resolves under the store, not under /.
    let boxed = place_of(name).expect("placed");
    let link = boxed
        .exec("readlink \"$HOME/shared\" || true", Duration::from_secs(30))
        .expect("read the shared-home link");
    assert!(
        link.trim().starts_with(store.to_str().unwrap()),
        "shared home must point into the mounted store, got {link:?}"
    );

    // And the store is reachable from the checkout, which is what makes hooks and the probe work.
    let claude = boxed
        .exec(
            &format!("readlink -f {tree}/.claude || true"),
            Duration::from_secs(30),
        )
        .expect("resolve .claude");
    assert!(
        claude.trim().starts_with(store.to_str().unwrap()),
        "the box's .claude must resolve into the store, got {claude:?}"
    );

    // ---- the sandbox cycles: the tree survives, the session does not ----
    // Measured against a real sandbox, not imagined: after sbx restarted skein-fleet, the box's
    // checkout, private HOME and cgroup ceiling were all intact and its tmux server was gone. Every
    // such box was then unreachable, and what a user saw first was an nsenter error about a pid.
    let before = shared_record(name).unwrap().ns_pid;
    let place = own_sandbox(FLEET);
    // A re-login inside the box, made just before the session dies. It is newer than the sandbox's
    // copy and it STAYS HERE: the file a box writes is not evidence about itself, and a box that
    // could improve the fleet's copy could also poison it. See the launcher's direction rule.
    let box_cred = PathBuf::from(format!("{}/home/.claude/.credentials.json", box_root(name)));
    let seeded = fs::read_to_string(&box_cred).unwrap_or_default();
    assert!(
        seeded.contains("SEEDED"),
        "a new box inherits the sandbox's login rather than asking for its own: {seeded}"
    );
    std::thread::sleep(Duration::from_millis(1100)); // mtime granularity, not a race
                                                     // A re-login, and a grant of this box's own alongside it. Both are written here because the two
                                                     // must travel differently: the login belongs to the person and goes everywhere, the grant
                                                     // belongs to this box's repository and goes nowhere.
    fs::write(
        &box_cred,
        br#"{"claudeAiOauth":{"accessToken":"RELOGIN","refreshToken":"r"},"mcpOAuth":{"sync|box":{"accessToken":"GRANT-MINE"}}}"#,
    )
    .unwrap();
    place
        .exec(
            &format!("tmux -S {} kill-server", box_sock(name)),
            Duration::from_secs(30),
        )
        .ok();
    // The server was killed behind skein's back, which is the one thing the gate cannot know. A warm
    // gate hands back its last picture immediately and refreshes behind the caller — deliberately,
    // so the board never blanks on a slow tick — so without this the assertion reads whatever the
    // *previous* test left there. Skein invalidates at every point it changes the fleet itself; this
    // is that, for a change skein did not make.
    forget_fleet_liveness();
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(false),
        "a killed server is a stopped box, and the sweep must see it"
    );

    // A snapshot exists to rescue work, so it must not need the box's session. Taken here, with the
    // server dead — the state resize hit on the first real run, where it refused with an nsenter
    // error and left the work it was trying to save unreachable.
    let snap = snapshot_box(name, repo.store.as_str(), "test-run").expect("snapshot a dead box");
    let bundle = store.join(&snap).join("repo.bundle");
    assert!(
        bundle.exists() && bundle.metadata().map(|m| m.len()).unwrap_or(0) > 0,
        "the box's commits must be saved even with no session: {}",
        bundle.display()
    );
    assert!(
        store.join(&snap).join("agent-state.tgz").exists(),
        "and its private agent state with them"
    );
    ensure_box_session(name).expect("restart the session from the tree");
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(true),
        "the box is reachable again without a re-clone"
    );
    let after = shared_record(name).unwrap().ns_pid;
    assert_ne!(
        before, after,
        "the anchor is a new process, so the placement must name it — a stale pid addresses nothing"
    );

    // ---- one login, seeded down, and never written back up ----
    // The fleet's copy is what every box is seeded from, so a box that could write it could hand
    // every later box a credential of its choosing — and the expiry it would win on is a number
    // inside a file the box writes. So the flow is one-way here: the box keeps its re-login and the
    // fleet's copy is untouched, whatever the two files claim about themselves.
    let reconciled =
        fs::read_to_string(sandbox_home.join(".claude/.credentials.json")).unwrap_or_default();
    assert!(
        reconciled.contains("SEEDED") && !reconciled.contains("RELOGIN"),
        "a box wrote the copy every later box is seeded from: {reconciled}"
    );
    // Nothing else travelled either, and this half fails silently. Copying the file whole would
    // have handed the sandbox this box's per-repo grant and destroyed the sandbox's own —
    // surfacing much later as an MCP server asking to be authorised again.
    assert!(
        reconciled.contains("GRANT-SHARED"),
        "the sandbox's own MCP grant was destroyed by a login sync: {reconciled}"
    );
    assert!(
        !reconciled.contains("GRANT-MINE"),
        "one box's per-repo MCP grant escaped into the shared copy: {reconciled}"
    );
    // And the box was not handed the fleet's older copy over its own newer one either: it keeps
    // what it has. Losing a working login to a stale one is the failure this rule replaced, not
    // one it is allowed to reintroduce.
    let kept = fs::read_to_string(&box_cred).unwrap_or_default();
    assert!(
        kept.contains("RELOGIN"),
        "the box's own login was overwritten with the fleet's older one: {kept}"
    );

    // ---- stopping and destroying settle the gate too, and this is also the teardown ----
    // Waited out through `/proc` rather than by polling `fleet_liveness`: every extra read is a
    // chance for the refresh running behind an earlier one to land, which would hide exactly the
    // staleness under test. One act, one read.
    let anchor = shared_record(name).expect("the box is placed").ns_pid;
    stop_box(name).expect("stop the box");
    for _ in 0..40 {
        if !Path::new(&format!("/proc/{anchor}")).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !Path::new(&format!("/proc/{anchor}")).exists(),
        "killing the server must end the box"
    );
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(false),
        "stopping a box must settle the liveness gate, or the board keeps it running"
    );

    // The same rule with a worse failure: the box is not stopped but gone, and a gate serving its
    // last good answer leaves a destroyed box on the board for anyone to click.
    destroy_box(name).expect("destroy the box");
    assert_eq!(
        fleet_liveness().get(name).copied(),
        None,
        "destroying a box must settle the liveness gate, or the board keeps a box that is gone"
    );
    assert!(
        shared_record(name).is_none(),
        "a destroyed box is unplaced, so nothing can be sent into what used to be its namespace"
    );
}

/// A fleet outlives the skein that made it, so restarting the server has to repair one.
///
/// The sandbox keeps whichever `box-session.sh` it was last given. Upgrade the host and the two
/// disagree: this skein passes a spec the installed launcher cannot read, the launcher exits before
/// tmux, and every reconnect enters an anchor pid from the last boot — `nsenter: cannot open
/// /proc/<pid>/ns/user`, forever, because nothing on the reconnect path ever replaced the copy that
/// could not start. Nothing else in a run observes that mismatch, which is why the repair belongs to
/// the restart.
///
/// And it must not cost a VM boot. Starting the cockpit is not a request to run the fleet, so a
/// sleeping sandbox is asked about (`sbx ls`) rather than asked *of* (`sbx exec`) — the launcher it
/// carries is repaired by `ensure_box_session` on the path that wakes it instead.
#[test]
fn a_server_restart_repairs_a_fleet_that_predates_it() {
    let _env = env_lock();
    let root = scratch_named("box");
    write_fake_sbx(&root.join("bin"));
    // Bound after `root`, so every name stops pointing into the scratch tree before it is removed.
    let mut pins = env_pins();
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("SKEIN_HOME", root.join("skein"))
    .set("SKEIN_FLEET_ROOT", root.join("boxes"));
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        fleet_memory: "26g".into(),
        ..Config::default()
    })
    .expect("configure a fleet");

    // The launcher an older skein left behind, in the one place the sandbox looks for it.
    install_launcher(FLEET).expect("install box-session.sh");
    let launcher = box_session_path();
    let stale = "#!/usr/bin/env bash\nexit 9 # an older skein's copy\n";
    fs::write(&launcher, stale).unwrap();

    // **The sleeping half of this test is gone, and the property with it** (SKEIN-576). It asserted
    // that a fleet reported `stopped` by `sbx ls` was left alone — that starting the cockpit is not
    // a request to boot a VM — and it was a host's property: only a skein OUTSIDE the sandbox can
    // observe one that is not running. Skein runs inside the fleet now, so a fleet it can heal is
    // running by definition; there is no state in which this process exists and its sandbox does
    // not. What replaced the observation is that `sbx ls` is not asked at all from in here.
    //
    // ---- the copy out there becomes this binary's copy ----
    pins.set(
        "SKEIN_LS_CMD",
        format!(r#"echo '[{{"name":"{FLEET}","status":"running"}}]'"#),
    );
    await_ls(Some(Liveness::Running));
    heal_fleet().expect("heal a running fleet");
    let now = fs::read_to_string(&launcher).unwrap();
    assert_ne!(
        now, stale,
        "a running fleet keeps the launcher it was given"
    );
    assert!(
        now.contains("apply_fleet_ceilings"),
        "the launcher installed is the embedded one, whole: {now:.120}"
    );
}

/// Wait until `fleet_boxes` serves what `sbx ls` is now saying about the fleet sandbox.
///
/// Changing what sbx says is not the same as skein seeing it. The answer is gated (1.5s), and once
/// it ages out the *first* caller is handed the remembered one while the refresh runs behind — that
/// is deliberate, so a wedged daemon makes the cockpit stale instead of making it stop, and it is
/// exactly why a test cannot assume its next call reflects the change it just made. Bounded, so a
/// gate that never comes round fails the assertion it was called for rather than hanging the suite.
fn await_ls(want: Option<Liveness>) {
    for _ in 0..60 {
        if fleet_boxes()
            .unwrap_or_default()
            .iter()
            .any(|b| b.name == FLEET && b.live == want)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
