//! A box's whole life inside the fleet sandbox: created, entered, stopped, destroyed, with
//! every step asserted against the real namespace and the real tmux server.

use super::*;

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
pub(super) fn sandbox_home_with_agent(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = root.join("sandbox-home");
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(bin.join("claude"), "#!/bin/sh\necho 'stub agent'\n").unwrap();
    fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    home
}

/// Block until `anchor` is gone from `/proc`, and say so loudly, naming the pid, if it never is.
///
/// **This is the real post-condition of ending a box.** `box-session.sh` reports the tmux SERVER's
/// pid as the anchor and says why in as many words — "box alive <=> server alive <=> namespace
/// joinable" — and `place::local_liveness` decides a box by asking `/proc/<anchor>/stat` for that
/// process's start time. So "the box is down" IS "that pid is gone", and every liveness assertion
/// in this file rests on it. It is not a proxy that happens to correlate; it is the fact the sweep
/// reads.
///
/// **And it is asynchronous, measured rather than assumed.** With a probe printed at the instant
/// `place.exec("tmux -S <sock> kill-server")` returned `Ok("")`, `/proc/<anchor>` still existed —
/// and the box's socket still accepted a connection — in **2 of 15 runs** on this box. The tmux
/// client's exit says the server took the command, not that it has finished ending its panes, its
/// cgroup and its namespace. On a bare tmux server with one `sleep` pane the same probe was clean
/// 200 times out of 200, which is why this looks synchronous until it is a box.
///
/// So the wait is on the POST-CONDITION and never on the subject. `fleet_liveness()` is still read
/// exactly once after this returns, so a sweep that reports a dead box as running still fails on
/// the first and only read, with nothing retried and no budget to run out. What this removes is
/// the other failure — the one where tmux had simply not finished — which is not a fact about
/// skein at all, and which an accidental `warden_client` round trip to the host used to hide by
/// costing a few milliseconds in between (SKEIN-739's lesson, at the sixth site).
///
/// It is also **faster to fail than what it replaces**. A box that genuinely stays up fails here,
/// naming the pid and what was expected of it, rather than at a `Some(true)` against `Some(false)`
/// sixty lines away that says nothing about why. The five seconds are the failure path only: the
/// spin is a millisecond and the loop is not entered at all once the pid is gone, so the ordinary
/// case costs one `Path::exists`.
pub(super) fn anchor_gone(anchor: u32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let proc = format!("/proc/{anchor}");
    while Path::new(&proc).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the box's tmux server (pid {anchor}) is still in /proc five seconds after it was told \
             to end, so the box is still up — `place::local_liveness` reads exactly this, and every \
             liveness assertion after this point would be about a live box rather than about the \
             sweep"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
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
    // **The real crossing is this suite's subject**, so it says so rather than being refused:
    // `Place::spawning` turns a fleet-scope command into a panic in a test process that has
    // installed no stand-in (SKEIN-530), and a stand-in here would delete what the module note
    // above promises — a real clone, a real bwrap namespace, a real tmux server, real `nsenter`
    // re-entry. What keeps all of that inside the fixture is the `$SKEIN_FLEET_ROOT` these tests
    // pin at their own scratch tree.
    let _real = skein::place::seam::real_crossings();
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
    //
    // **The fixture's `~/.local/bin` is deliberately NOT on this PATH** — only the fake `sbx` is.
    // It used to be, and that made the `command -v claude` assertion below satisfiable two ways:
    // by the box resolving its own home, or by the spawner's PATH riding through `nsenter` into the
    // box. The second is not the property, and while it was available the assertion could not tell
    // a crossing that lands on the box's PATH from one that lands on the caller's (SKEIN-832).
    // With it gone there is exactly one path by which `claude` can answer from the fixture: the
    // PATH `Place::wrap` builds from the box's OWN home.
    let mut pins = env_pins();
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
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

    // **This fixture's box matches no repository, so it is one of the uncovered ones** — the
    // module note says so in its own words, "a sandbox that has never seen this repo", and nothing
    // here calls `save_repos`. `fleet::refuse_if_uncovered` turns that away now unless somebody has
    // said to, and `ensure_box_session` further down is on that path. Declared here, once, rather
    // than registering a repo: registering one would hand the launcher a manifest and change the
    // mounts this test is measuring, which is a different test. The refusal itself is asserted in
    // `an_uncovered_box_is_refused_until_it_is_allowed_and_then_says_so_where_someone_is_looking`.
    skein::fleet::allow_uncovered(BOX, true).expect("this fixture's box is uncovered on purpose");

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
    //
    // **It now asks about the PATH as well as the bind, because the fixture's bin directory is off
    // the spawner's PATH** (see the pin above). Two independent things have to hold for this to
    // answer: the share survived the bind, AND the crossing landed on a PATH derived from the box's
    // own home rather than on whatever the caller had. It caught the second failing on its own —
    // `Place::path_pin` put `FLEET_PATH` in front of a crossing and nothing set it back past the
    // hop, so a box answered `/usr/local/bin/claude`: the substrate's copy, with none of the
    // fleet's. That is exactly the confusion the resolved-path spelling exists to make visible.
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
    // `.claude` is a directory of the box's own, with the store's entries linked into it and its
    // settings files left out (SKEIN-1053); `skein` is the link the probes find the store through.
    assert_eq!(
        boxed
            .exec(
                "[ -d .claude ] && [ ! -L .claude ] && [ ! -e .claude/settings.json ] \
                 && readlink .claude/skein",
                Duration::from_secs(30)
            )
            .unwrap()
            .trim(),
        store.join("skein").to_string_lossy(),
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
    // **Which refusal answered, and the other one this recognises** (SKEIN-433). `resize_fleet`
    // refuses in phase 1 for more than one real reason and the FIRST of them is space:
    // `room_to_copy_out` runs before the per-box census, because discovering the host is full
    // after the sandbox is gone would be the worst possible moment for it. So on a machine with
    // too little room the answer here is a correct refusal about the disk and not the ordering
    // this covers — and `err.contains("no registered repo")` reported that correct refusal as a
    // malformed one, sending the reader to the wording when the truth was 352 MiB free.
    // `common::pinned_refusal` carries the argument; this call only has to name the other answer.
    common::pinned_refusal(
        &err,
        &["no registered repo", "untouched"],
        &[(
            "copying the boxes out needs about",
            "this machine has too little free space to copy the boxes out, so the resize refused \
             on space before it reached the per-box census this covers",
        )],
        "the resize ordering assertions",
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
