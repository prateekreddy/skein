//! The agent CLIs and Rust toolchains a box runs: known versions from the fleet's depot, read-only
//! in every box, recorded at start, and moved onto a rollout only when the box is idle
//! (SKEIN-1372). Every CLI and toolchain here is a fixture tree; nothing is downloaded
//! (docs/decisions/tests-never-reach-a-live-fleet.md).

use super::*;
use skein::fleet::depot;
use skein::fleet::versions::{self, AGENT_CLI, RUST_TOOLCHAIN, TYPESCRIPT};
use std::os::unix::fs::PermissionsExt;

/// Pin a fixture fleet with the launcher installed, and a fixture npm prefix standing for the
/// sandbox's own: one package of its own (`other`), and a `claude` of the sandbox's.
fn versions_fleet(what: &str) -> (Scratch, PathBuf, common::EnvPins) {
    let root = scratch_named(what);
    let sandbox_home = sandbox_home_with_agent(&root);
    let mut pins = env_pins();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"));
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh and the kit");
    plant_plugin_check(&root);
    let prefix = root.join("npm-prefix");
    npm_package(
        &prefix,
        "other",
        "run.sh",
        "other",
        "echo other-from-the-sandbox",
    );
    npm_package(
        &prefix,
        "@anthropic-ai/claude-code",
        "bin/claude.exe",
        "claude",
        "echo '0.0.1 (the sandbox own)'",
    );
    (root, prefix, pins)
}

/// One package in an npm prefix laid out as npm lays one out: the package under
/// `lib/node_modules`, and its command in `bin` as a relative link into it.
fn npm_package(prefix: &Path, package: &str, file: &str, command: &str, says: &str) {
    let dir = prefix.join("lib/node_modules").join(package);
    let at = dir.join(file);
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    fs::write(&at, format!("#!/bin/sh\n{says}\n")).unwrap();
    fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(prefix.join("bin")).unwrap();
    std::os::unix::fs::symlink(
        format!("../lib/node_modules/{package}/{file}"),
        prefix.join("bin").join(command),
    )
    .unwrap();
}

/// Publish a fixture CLI version into the depot the way a fill does after its check, test it
/// through the real check namespace, and return its entry name. Its binary answers `--version` the
/// way the real one does: `<version> (Claude Code)`, `codex-cli <version>`.
fn publish_cli(root: &Path, runtime: &str, version: &str) -> String {
    let entry = stage_cli(root, runtime, version);
    let tested = versions::test_cli(FLEET, &entry).expect("the entry is tested");
    assert!(
        tested.ok,
        "the fixture {entry} failed its own test: {tested:?}"
    );
    entry
}

/// Publish a fixture CLI version, as [`publish_cli`] does, without testing it.
fn stage_cli(root: &Path, runtime: &str, version: &str) -> String {
    let entry = format!("{runtime}@{version}");
    let out = root.join(format!("staged-{entry}"));
    let (package, file, says) = match runtime {
        "claude" => (
            "@anthropic-ai/claude-code",
            "bin/claude.exe",
            format!("{version} (Claude Code)"),
        ),
        _ => (
            "@openai/codex",
            "bin/codex.js",
            format!("codex-cli {version}"),
        ),
    };
    npm_package(
        &out.join(&entry),
        package,
        file,
        runtime,
        &format!("echo '{says}'"),
    );
    let staged = depot::verify(&out, &AGENT_CLI).expect("a fixture prefix passes the check");
    depot::publish(&out, &depot::kind_dir(&AGENT_CLI), &staged).expect("published");
    entry
}

/// Publish a fixture of the pinned TypeScript into the depot the way a fill does after its check:
/// an npm prefix whose `bin/tsc` answers `--version` as the real one does, `Version <pin>`.
fn publish_typescript(root: &Path) -> String {
    let entry = versions::typescript_entry();
    let pin = versions::TYPESCRIPT_PIN.trim();
    let out = root.join(format!("staged-{entry}"));
    npm_package(
        &out.join(&entry),
        "typescript",
        "bin/tsc",
        "tsc",
        &format!("echo 'Version {pin}'"),
    );
    let staged = depot::verify(&out, &TYPESCRIPT).expect("a fixture TypeScript passes the check");
    depot::publish(&out, &depot::kind_dir(&TYPESCRIPT), &staged).expect("published");
    entry
}

/// A stand-in for `tools/plugin-check.sh` in skein's source, as the mod's check (SKEIN-1359)
/// behaves at its door: it refuses a `claude` on `PATH` other than `$CLAUDE_CODE_VERSION`, exits 2
/// as the real one does when `$TSC` names no `tsc` it can run (SKEIN-1396), and says which `claude`
/// and which `tsc` it ran.
fn plant_plugin_check(root: &Path) {
    let tools = root.join("boxes/.skein/src/tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(
        tools.join("plugin-check.sh"),
        concat!(
            "#!/usr/bin/env bash\n",
            "have=$(claude --version | awk '{print $1}')\n",
            "[ -n \"$CLAUDE_CODE_VERSION\" ] && [ \"$have\" = \"$CLAUDE_CODE_VERSION\" ] || ",
            "{ echo \"claude is $have, asked for $CLAUDE_CODE_VERSION\"; exit 1; }\n",
            "ts=$(\"${TSC:-tsc}\" --version 2>/dev/null) || { echo \"no tsc at ${TSC:-tsc}\"; exit 2; }\n",
            "echo \"fixture plugin-check ran claude $have from $(command -v claude) with $ts\"\n",
        ),
    )
    .unwrap();
}

/// A fixture toolchain entry whose `rustc` says which one it is.
fn publish_toolchain(root: &Path, name: &str) {
    let out = root.join(format!("staged-{name}"));
    let bin = out.join(name).join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("rustc"),
        format!("#!/bin/sh\necho rustc-from-{name}\n"),
    )
    .unwrap();
    fs::set_permissions(bin.join("rustc"), fs::Permissions::from_mode(0o755)).unwrap();
    let staged = depot::verify(&out, &RUST_TOOLCHAIN).expect("a fixture toolchain passes");
    depot::publish(&out, &depot::kind_dir(&RUST_TOOLCHAIN), &staged).expect("published");
}

/// Start box `name` with `agent` as its agent, the npm prefix pointed at the fixture, and wait for
/// the agent's `/tmp/<report>.report`. Returns what the launcher said, the anchor, and the report.
fn start(
    name: &str,
    workshop: bool,
    prefix: &Path,
    agent: &str,
    report: &str,
) -> (String, u32, String) {
    let mut script = format!(
        "SKEIN_NPM_PREFIX={} {}",
        prefix.display(),
        session_script(name, "skein-agent", agent)
    );
    if workshop {
        script = with_launch_env(&script, "SKEIN_BOX_PRIVILEGED", "1");
    }
    let launched = own_sandbox(FLEET)
        .exec(&script, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("start {name}: {e}"));
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    let path = PathBuf::from(format!("{}/tmp/{report}.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let said = fs::read_to_string(&path).unwrap_or_default();
    let _ = fs::remove_file(&path);
    (launched, anchor, said)
}

/// End box `name`: its tmux server, then its cgroup if the launcher made one.
fn end(name: &str, anchor: u32) {
    let limits = fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
    let _ = Command::new("tmux")
        .args(["-S", &box_sock(name), "kill-server"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    anchor_gone(anchor);
    if limits.starts_with("capped ") {
        let _ = Command::new("sudo")
            .args(["-n", "rmdir", &format!("/sys/fs/cgroup/skein/{name}")])
            .status();
    }
}

fn field(report: &str, key: &str) -> String {
    report
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key} ")))
        .unwrap_or("")
        .to_string()
}

/// The record a start writes, with what the launcher said it bound.
fn record_start(name: &str, launched: &str, anchor: u32) {
    record_place(
        name,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            tree: format!("{}/tree", box_root(name)),
            sock: box_sock(name),
            launcher: skein::fleet::launcher_from_launch(launched),
            versions: versions::versions_from_launch(launched),
            ..Default::default()
        },
    )
    .expect("record the box");
}

/// **The rolled-out CLI is what a box runs, at the path its PATH names, and nothing in an ordinary
/// box or the workshop box can change it: not the command, not the package, not the depot entry,
/// not the prefix around them. The sandbox's other packages are still there, read-only.**
///
/// The marks are looked for on the host afterwards, so an attempt the box was told failed and that
/// landed anyway still fails the test.
///
/// What would make each assertion fail:
///   * `cli`: dropping the entry's package bind (the box runs the sandbox's own `0.0.1`), or the
///     command link (no `claude` at all).
///   * `other`: a projection that drops the sandbox's own packages.
///   * every refusal: dropping `--remount-ro "$npm_prefix"` (the tmpfs is writable: `planted`,
///     `rm` and `mkdir` succeed), or binding the entry read-write (`append` and `entry` succeed).
///   * the launch report: dropping the `SKEIN_VERSIONS` line, or reporting a CLI it did not bind.
#[test]
fn a_rolled_out_cli_is_read_only_in_every_box() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so whether a box can \
             change its agent CLI was NOT exercised",
        );
    }
    let (root, prefix, _pins) = versions_fleet("clirod");
    let pin = versions::pinned("claude").expect("claude is pinned");
    let ts = publish_typescript(&root);
    let entry = publish_cli(&root, "claude", pin);
    // The mod's check ran, against this entry's `claude` and the version under test, with the
    // pinned `tsc` from the depot. What would make it fail: `checks_for` without the plugin-check
    // row, or without `CLAUDE_CODE_VERSION`; `check_script` not emitting the check's binds, or
    // `TSC` not set (the planted check finds no `tsc` and exits 2).
    let tested = &versions::rollouts().tested[&entry];
    assert_eq!(tested.typescript.as_ref(), Some(&ts), "{tested:?}");
    let plugin = tested
        .checks
        .iter()
        .find(|c| c.name == "plugin-check")
        .expect("the mod's check was not run");
    assert!(
        plugin.ok
            && plugin.said.contains(&format!(
                "ran claude {pin} from /cli/bin/claude with Version {}",
                versions::TYPESCRIPT_PIN.trim()
            )),
        "{plugin:?}"
    );
    versions::rollout("claude", &entry).expect("roll out the fixture");
    let depot_entry = depot::kind_dir(&AGENT_CLI).join(&entry);
    let exe = depot_entry.join("lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe");
    let before = fs::read(&exe).unwrap();

    let p = prefix.display().to_string();
    let e = depot_entry.display().to_string();
    let agent = format!(
        "p='{p}'; e='{e}'; pkg=\"$p/lib/node_modules/@anthropic-ai/claude-code\"; {{ \
           printf 'cli %s\\n' \"$(\"$p/bin/claude\" --version 2>&1)\"; \
           printf 'other %s\\n' \"$(\"$p/bin/other\" 2>&1)\"; \
           echo x > \"$p/bin/planted\" 2>/dev/null && echo 'planted yes' || echo 'planted no'; \
           rm -f \"$p/bin/claude\" 2>/dev/null && echo 'rm yes' || echo 'rm no'; \
           mkdir \"$p/lib/node_modules/mine\" 2>/dev/null && echo 'mkdir yes' || echo 'mkdir no'; \
           echo x >> \"$pkg/bin/claude.exe\" 2>/dev/null && echo 'append yes' || echo 'append no'; \
           chmod u+w \"$pkg/bin/claude.exe\" 2>/dev/null && echo 'chmod yes' || echo 'chmod no'; \
           echo x > \"$e/planted\" 2>/dev/null && echo 'entry yes' || echo 'entry no'; \
         }} > /tmp/cli.tmp && mv /tmp/cli.tmp /tmp/cli.report; exec sleep 300"
    );
    for (case, name, workshop) in [
        ("ordinary", "clirod-main", false),
        ("workshop", "clishop-main", true),
    ] {
        let (launched, anchor, report) = start(name, workshop, &prefix, &agent, "cli");
        end(name, anchor);
        assert_eq!(
            field(&report, "cli"),
            format!("{pin} (Claude Code)"),
            "the {case} box does not run the rolled-out claude: {report:?}"
        );
        assert_eq!(
            field(&report, "other"),
            "other-from-the-sandbox",
            "the {case} box lost the sandbox's own packages: {report:?}"
        );
        for what in ["planted", "rm", "mkdir", "append", "chmod", "entry"] {
            assert_eq!(
                field(&report, what),
                "no",
                "the {case} box could {what} its agent CLI: {report:?}"
            );
        }
        assert_eq!(
            versions::versions_from_launch(&launched),
            Some(std::collections::BTreeMap::from([(
                "claude".to_string(),
                entry.clone()
            )])),
            "the {case} box's launcher did not report the CLI it bound: {launched}"
        );
    }
    let marks: Vec<PathBuf> = [
        prefix.join("bin/planted"),
        prefix.join("lib/node_modules/mine"),
        depot_entry.join("planted"),
    ]
    .into_iter()
    .filter(|p| p.exists())
    .collect();
    assert!(marks.is_empty(), "a box changed what boxes run: {marks:?}");
    assert!(
        prefix.join("bin/claude").exists(),
        "a box removed the sandbox's claude"
    );
    assert_eq!(
        fs::read(&exe).unwrap(),
        before,
        "a box changed the rolled-out claude"
    );
}

/// **A claude-code entry is not tested before the pinned TypeScript is in the depot, and a record
/// from before it, the stale failure an entry tested without `tsc` was left with, is tested again
/// once it is** (SKEIN-1396).
///
/// The entry is published with no TypeScript beside it, and its record is the one a test without
/// `tsc` left: failed, naming no TypeScript. Then the fixture TypeScript is published.
///
/// What would make each assertion fail:
///   * `waits`: dropping the guard in `test_cli` (the check runs, the planted check exits 2, and a
///     failed record is written over the old one).
///   * `retested`: keeping the plain `tested.contains_key` skip in `test_pass` (the stale failure is
///     never tested again, and the entry can never be rolled out).
///   * `once`: retesting a record that names the current TypeScript (a second pass tests again).
#[test]
fn a_claude_entry_waits_for_its_typescript_and_is_retested_when_it_arrives() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() {
        return skip(
            "this machine cannot make a bwrap namespace, so a claude entry's wait for its \
             TypeScript was NOT exercised",
        );
    }
    let (root, _prefix, _pins) = versions_fleet("tswait");
    let pin = versions::pinned("claude").expect("claude is pinned");
    let entry = stage_cli(&root, "claude", pin);
    let stale = serde_json::json!({
        "tested": { &entry: { "ok": false, "at": 1, "checks": [
            { "name": "plugin-check", "ok": false, "said": "plugin-check: tsc is not on PATH" }
        ] } }
    });
    let record = root.join("skein/versions.json");
    fs::create_dir_all(record.parent().unwrap()).unwrap();
    fs::write(&record, stale.to_string()).unwrap();

    let why = versions::test_cli(FLEET, &entry).expect_err("tested with no TypeScript");
    assert!(why.contains("waits for typescript@"), "{why}");
    let said = versions::test_pass(FLEET);
    assert!(
        said.iter().any(|l| l.contains("waits for typescript@")),
        "{said:?}"
    );
    let waits = &versions::rollouts().tested[&entry];
    assert_eq!(
        (waits.at, waits.typescript.clone()),
        (1, None),
        "a test with no TypeScript wrote a record: {waits:?}"
    );

    let ts = publish_typescript(&root);
    let said = versions::test_pass(FLEET);
    let retested = versions::rollouts().tested[&entry].clone();
    assert!(
        retested.ok && retested.typescript.as_ref() == Some(&ts),
        "the stale failure was not tested again once {ts} arrived: {retested:?} {said:?}"
    );
    assert!(versions::rollout("claude", &entry).is_ok(), "{retested:?}");

    let once = versions::test_pass(FLEET);
    assert!(
        once.is_empty(),
        "an entry tested with the current TypeScript was tested again: {once:?}"
    );
}

/// **A box records the versions it was started on; a rollout makes a running box "restart to
/// apply" without touching what it runs; an idle box is then restarted onto the new version, and a
/// working box is not** (the owner's decisions 1 and 2).
///
/// Two boxes start on `codex@1.0.0`. `1.0.1` is published, tested and rolled out. Both are then
/// stale, and both still run 1.0.0. Both ask to be restarted, through the drop-box file the box's
/// tool writes, and the restart pass runs with the board reading one `waiting` and the other
/// `working`. The
/// restart it is handed is the launcher's relaunch, so what the moved box runs afterwards is read
/// from inside it.
///
/// What would make each assertion fail:
///   * `versions` in the record: `versions_from_launch` not reading the launcher's line, or the
///     launcher not printing it.
///   * the stale key: dropping the versions comparison from `why_cover_is_stale`.
///   * `still 1.0.0` after the rollout: a projection that follows the rollout live (a bind of the
///     depot rather than of the entry the box started with).
///   * the idle box on 1.0.1: `restart_onto_rollouts` not restarting an idle box that asked.
///   * the working box untouched: dropping both idle checks, the pass's and the one at the moment
///     of the restart (it is restarted, its anchor dies, its ask is spent).
#[test]
fn an_idle_box_moves_onto_a_rollout_and_a_working_box_does_not() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so restart-to-apply was \
             NOT exercised",
        );
    }
    let (root, prefix, _pins) = versions_fleet("cliroll");
    let first = publish_cli(&root, "codex", "1.0.0");
    versions::rollout("codex", &first).expect("roll out 1.0.0");
    let p = prefix.display().to_string();
    let agent = format!(
        "{{ printf 'cli %s\\n' \"$('{p}/bin/codex' --version 2>&1)\"; }} > /tmp/v.tmp \
         && mv /tmp/v.tmp /tmp/v.report; exec sleep 300"
    );
    let mut anchors = std::collections::BTreeMap::new();
    for name in ["rollidle-main", "rollbusy-main"] {
        let (launched, anchor, report) = start(name, false, &prefix, &agent, "v");
        assert_eq!(
            field(&report, "cli"),
            "codex-cli 1.0.0",
            "{name}: {report:?}"
        );
        record_start(name, &launched, anchor);
        assert_eq!(
            shared_record(name).unwrap().versions,
            Some(std::collections::BTreeMap::from([(
                "codex".to_string(),
                first.clone()
            )])),
            "{name} did not record the version it started on"
        );
        anchors.insert(name, anchor);
    }
    let stale = |name: &str| skein::fleet::why_cover_is_stale(name, &shared_record(name).unwrap());
    assert!(
        !stale("rollidle-main").contains(&"agent-cli"),
        "a box on the current version reads stale"
    );

    let second = publish_cli(&root, "codex", "1.0.1");
    versions::rollout("codex", &second).expect("roll out 1.0.1");
    for name in ["rollidle-main", "rollbusy-main"] {
        assert!(
            stale(name).contains(&"agent-cli"),
            "{name} is behind a rollout and the board would not say so: {:?}",
            stale(name)
        );
        let still = own_sandbox(FLEET)
            .exec(
                &format!(
                    "nsenter -t {} -U -m --preserve-credentials '{p}/bin/codex' --version",
                    anchors[name]
                ),
                Duration::from_secs(30),
            )
            .unwrap_or_default();
        assert_eq!(
            still.trim(),
            "codex-cli 1.0.0",
            "a rollout changed what the running {name} runs"
        );
    }

    // Both asked, as a box does when it calls `skein_resources` with `restart: when-idle`: the
    // working one asked and then took up another task, which is the case the idle check is for.
    // Without its ask the ten-minute grace would hold it back on its own, and the idle check would
    // go untested.
    let ask = versions::restart_request_path("rollidle-main");
    let busy_ask = versions::restart_request_path("rollbusy-main");
    for a in [&ask, &busy_ask] {
        fs::create_dir_all(a.parent().unwrap()).unwrap();
        fs::write(a, "").unwrap();
    }
    let boxes = vec![
        versions::BoxNow {
            name: "rollidle-main".into(),
            state: "waiting".into(),
        },
        versions::BoxNow {
            name: "rollbusy-main".into(),
            state: "working".into(),
        },
    ];
    let memory = versions::IdleMemory::default();
    let moved = std::cell::RefCell::new(String::new());
    let said = versions::restart_onto_rollouts(
        &boxes,
        Instant::now(),
        &memory,
        stale,
        |name| {
            let state = boxes
                .iter()
                .find(|b| b.name == name)
                .map(|b| b.state.clone())
                .unwrap_or_default();
            skein::fleet::idle::idle(None, &state)
        },
        |name| {
            end(name, anchors[name]);
            let (launched, anchor, report) = start(name, false, &prefix, &agent, "v");
            record_start(name, &launched, anchor);
            *moved.borrow_mut() = report;
            Ok(())
        },
    );
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(
        field(&moved.borrow(), "cli"),
        "codex-cli 1.0.1",
        "the idle box was not moved onto the rollout"
    );
    assert_eq!(
        shared_record("rollidle-main").unwrap().versions,
        Some(std::collections::BTreeMap::from([(
            "codex".to_string(),
            second.clone()
        )]))
    );
    assert!(
        stale("rollidle-main").is_empty(),
        "{:?}",
        stale("rollidle-main")
    );
    assert!(
        !ask.exists(),
        "the box's ask outlived the restart it asked for"
    );
    assert!(
        busy_ask.exists(),
        "the working box's ask was spent without a restart"
    );
    assert!(
        Path::new(&format!("/proc/{}", anchors["rollbusy-main"])).exists(),
        "the working box was restarted"
    );
    assert!(stale("rollbusy-main").contains(&"agent-cli"));
    end("rollbusy-main", anchors["rollbusy-main"]);
    let idle_anchor = shared_record("rollidle-main").unwrap().ns_pid;
    end("rollidle-main", idle_anchor);
}

/// **A repo whose tree pins a toolchain in `rust-toolchain.toml` builds with that toolchain; a
/// repo that pins none gets the fleet's default; and neither can write the rustup home.** Through
/// the real rustup, with the fleet's `cargo/bin` laid out as bootstrap lays it out: every proxy a
/// relative link to `rustup`.
///
/// What would make each assertion fail:
///   * `pinned`: the launcher not binding the repo's toolchain, `session_script` not passing
///     `rust-repo`, or `repo_toolchain` not reading the file.
///   * `default` in both boxes: a `settings.toml` that names another default, or no rustup home
///     at all (the proxies then find no toolchain).
///   * `cargo`'s and `rustc`'s answers at all: the rustup shim exec'ing rustup by its own name
///     (SKEIN-1379), which turns every proxy into `rustup`.
///   * `write no`: dropping `--remount-ro` of the composed home.
///   * the reports: the launcher not reporting `rust` and `rust-repo`.
#[test]
fn a_repos_rust_toolchain_file_selects_its_toolchain() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so per-repo toolchains \
             were NOT exercised",
        );
    }
    let rustup = Command::new("sh")
        .args(["-c", "command -v rustup"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|p| p.starts_with('/'));
    let Some(rustup) = rustup else {
        return skip("no rustup on this machine's PATH, so per-repo toolchains were NOT exercised");
    };
    let (root, prefix, _pins) = versions_fleet("rustrepo");
    let bin = root.join("boxes/.skein/toolchain/cargo/bin");
    fs::create_dir_all(&bin).unwrap();
    // A real file, as bootstrap leaves it: the launcher binds its shim over this name.
    fs::copy(&rustup, bin.join("rustup")).expect("copy rustup into the fixture toolchain");
    for proxy in ["rustc", "cargo"] {
        std::os::unix::fs::symlink("rustup", bin.join(proxy)).unwrap();
    }
    let triple = versions::host_triple();
    let (default, pinned) = (format!("1.98.0-{triple}"), format!("1.97.0-{triple}"));
    publish_toolchain(&root, &default);
    publish_toolchain(&root, &pinned);
    versions::rollout("rust", &default).expect("roll out the default toolchain");
    let pinned_tree = PathBuf::from(box_root("rustpin-main")).join("tree");
    fs::create_dir_all(&pinned_tree).unwrap();
    fs::write(
        pinned_tree.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.97.0\"\n",
    )
    .unwrap();

    let agent = "{ printf 'here %s\\n' \"$(rustc --version 2>&1)\"; \
                   printf 'elsewhere %s\\n' \"$(cd /tmp && rustc --version 2>&1)\"; \
                   touch \"$RUSTUP_HOME/planted\" 2>/dev/null && echo 'write yes' || echo 'write no'; \
                 } > /tmp/rs.tmp && mv /tmp/rs.tmp /tmp/rs.report; exec sleep 300";
    for (name, here) in [("rustpin-main", &pinned), ("rustplain-main", &default)] {
        let (launched, anchor, report) = start(name, false, &prefix, agent, "rs");
        end(name, anchor);
        assert_eq!(
            field(&report, "here"),
            format!("rustc-from-{here}"),
            "{name} did not build with its repo's toolchain: {report:?}"
        );
        assert_eq!(
            field(&report, "elsewhere"),
            format!("rustc-from-{default}"),
            "{name}, outside its tree, is not on the fleet's default: {report:?}"
        );
        assert_eq!(
            field(&report, "write"),
            "no",
            "{name} wrote its rustup home: {report:?}"
        );
        let bound = versions::versions_from_launch(&launched).unwrap_or_default();
        assert_eq!(bound.get("rust"), Some(&default), "{launched}");
        assert_eq!(
            bound.get("rust-repo"),
            (name == "rustpin-main").then_some(&pinned),
            "{launched}"
        );
    }
}

/// **With no fleet default rolled out, a repo that pins a toolchain with a target builds with the
/// entry holding that target, and outside its tree keeps the bootstrap's toolchain; a repo that
/// pins one the fleet lacks is told its name by `cargo` and `rustc`** (SKEIN-1415). Through the
/// real rustup, with the fleet's `cargo/bin` laid out as bootstrap lays it out, and the bootstrap's
/// own rustup home holding a `stable-<triple>`.
///
/// What would make each assertion fail:
///   * `here` in the wasm box: `wanted_with` reading the repo's file only once a default is rolled
///     out (the box runs the bootstrap's toolchain), `repo_toolchain` dropping the targets (no entry
///     of that name is published, so nothing is bound), or the launcher binding the entry at its
///     own name rather than at the toolchain's (rustup finds no `1.98.1-<triple>`).
///   * `elsewhere` in the wasm box: a composed home that leaves out the bootstrap's toolchains or
///     names the repo's as the default.
///   * `write no`: dropping `--remount-ro` of the composed home.
///   * `here` and `deeper` in the lacking box: the shim not baking `rust-repo-missing`, not walking
///     up from a subdirectory, or its message not naming the entry; and `elsewhere` there: the shim
///     refusing outside the tree.
///   * `own` in both boxes: the shim speaking without asking whether the `RUSTUP_HOME` in force
///     holds the toolchain (a box whose own home holds it is refused a toolchain it has).
///   * the reports: the launcher not reporting `rust-repo` or `rust-repo-missing`, or reporting a
///     `rust` it never bound.
#[test]
fn a_repos_pinned_toolchain_is_bound_without_a_fleet_default_and_a_missing_one_is_named() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so a repo's pinned \
             toolchain without a default was NOT exercised",
        );
    }
    let rustup = Command::new("sh")
        .args(["-c", "command -v rustup"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|p| p.starts_with('/'));
    let Some(rustup) = rustup else {
        return skip(
            "no rustup on this machine's PATH, so a repo's pinned toolchain without a default \
             was NOT exercised",
        );
    };
    let (root, prefix, _pins) = versions_fleet("rustnodefault");
    let toolchain = root.join("boxes/.skein/toolchain");
    let bin = toolchain.join("cargo/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::copy(&rustup, bin.join("rustup")).expect("copy rustup into the fixture toolchain");
    for proxy in ["rustc", "cargo"] {
        std::os::unix::fs::symlink("rustup", bin.join(proxy)).unwrap();
    }
    let triple = versions::host_triple();
    let stable = toolchain.join(format!("rustup/toolchains/stable-{triple}/bin"));
    fs::create_dir_all(&stable).unwrap();
    fs::write(
        stable.join("rustc"),
        "#!/bin/sh\necho rustc-from-bootstrap\n",
    )
    .unwrap();
    fs::set_permissions(stable.join("rustc"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(toolchain.join("rustup/update-hashes")).unwrap();
    fs::write(
        toolchain.join("rustup/settings.toml"),
        format!("version = \"12\"\ndefault_toolchain = \"stable-{triple}\"\nprofile = \"minimal\"\n\n[overrides]\n"),
    )
    .unwrap();
    let wasm = format!("1.98.1-{triple}@wasm32-unknown-unknown");
    let lacking = format!("1.97.3-{triple}@wasm32-unknown-unknown");
    publish_toolchain(&root, &wasm);
    assert!(
        versions::rollouts().current.is_empty(),
        "this test is about a fleet with nothing rolled out"
    );
    for (name, channel) in [("rustwasm-main", "1.98.1"), ("rustlack-main", "1.97.3")] {
        let tree = PathBuf::from(box_root(name)).join("tree");
        fs::create_dir_all(tree.join("src/deep")).unwrap();
        fs::write(
            tree.join("rust-toolchain.toml"),
            format!(
                "[toolchain]\nchannel = \"{channel}\"\ncomponents = [\"clippy\", \"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n"
            ),
        )
        .unwrap();
    }

    // `own`: a rustup home of the box's own, holding the toolchain its repo pins, as a box that
    // installed it itself has. Made in the box's `/tmp`, run from the tree. `RUSTUP_AUTO_INSTALL=0`
    // because the fixture toolchain has no component records: with it on, rustup would go to the
    // network to add the file's wasm32 target, which a real home holding that target would not.
    let own = |channel: &str| {
        format!(
            "o=/tmp/own-rh/toolchains/{channel}-{triple}/bin; mkdir -p \"$o\" /tmp/own-rh/update-hashes; \
             printf '#!/bin/sh\\necho cargo-from-own\\n' > \"$o/cargo\"; chmod +x \"$o/cargo\"; \
             printf 'own %s\\n' \"$(RUSTUP_AUTO_INSTALL=0 RUSTUP_HOME=/tmp/own-rh cargo --version 2>&1)\"; "
        )
    };
    let agent = |channel: &str| {
        format!(
            "{{ printf 'here %s\\n' \"$(rustc --version 2>&1)\"; \
                   printf 'deeper %s\\n' \"$(cd src/deep && cargo --version 2>&1)\"; {} \
                   printf 'elsewhere %s\\n' \"$(cd /tmp && rustc --version 2>&1)\"; \
                   touch \"$RUSTUP_HOME/planted\" 2>/dev/null && echo 'write yes' || echo 'write no'; \
                 }} > /tmp/rs.tmp && mv /tmp/rs.tmp /tmp/rs.report; exec sleep 300",
            own(channel)
        )
    };

    let (launched, anchor, report) = start("rustwasm-main", false, &prefix, &agent("1.98.1"), "rs");
    end("rustwasm-main", anchor);
    assert_eq!(
        field(&report, "here"),
        format!("rustc-from-{wasm}"),
        "the wasm box did not build with its repo's entry: {report:?}"
    );
    assert_eq!(
        field(&report, "elsewhere"),
        "rustc-from-bootstrap",
        "the wasm box, outside its tree, is not on the bootstrap's toolchain: {report:?}"
    );
    assert_eq!(field(&report, "write"), "no", "{report:?}");
    assert_eq!(field(&report, "own"), "cargo-from-own", "{report:?}");
    let bound = versions::versions_from_launch(&launched).unwrap_or_default();
    assert_eq!(bound.get("rust-repo"), Some(&wasm), "{launched}");
    assert_eq!(bound.get("rust"), None, "{launched}");

    let (launched, anchor, report) = start("rustlack-main", false, &prefix, &agent("1.97.3"), "rs");
    end("rustlack-main", anchor);
    assert_eq!(
        field(&report, "own"),
        "cargo-from-own",
        "the lacking box was refused the toolchain its own rustup home holds: {report:?}"
    );
    for key in ["here", "deeper"] {
        let said = field(&report, key);
        assert!(
            said.contains(&format!(
                "pins the Rust toolchain {lacking}, which the fleet does not have yet"
            )),
            "{key}: the lacking box was not told the toolchain's name: {report:?}"
        );
    }
    assert_eq!(
        field(&report, "elsewhere"),
        "rustc-from-bootstrap",
        "the lacking box, outside its tree, was refused or moved: {report:?}"
    );
    let bound = versions::versions_from_launch(&launched).unwrap_or_default();
    assert_eq!(
        bound,
        std::collections::BTreeMap::from([("rust-repo-missing".to_string(), lacking.clone())]),
        "{launched}"
    );
}

/// **A version in use is never removed, nor the current one nor the one kept for rollback, and one
/// nothing keeps is removed whole.**
///
/// What would make it fail: `kept` not reading placement records (the in-use entry is removed),
/// not keeping `current` or `previous`, or `remove_unused` not re-checking at removal.
#[test]
fn the_clean_refuses_a_version_in_use() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    let (root, _prefix, _pins) = versions_fleet("cliclean");
    let used = publish_cli(&root, "codex", "1.0.0");
    let previous = publish_cli(&root, "codex", "1.0.1");
    let current = publish_cli(&root, "codex", "1.0.2");
    let unused = publish_cli(&root, "codex", "1.0.3");
    versions::rollout("codex", &previous).unwrap();
    versions::rollout("codex", &current).unwrap();
    record_place(
        "cleanuse-main",
        &PlaceRecord {
            sandbox: FLEET.into(),
            versions: Some(std::collections::BTreeMap::from([(
                "codex".to_string(),
                used.clone(),
            )])),
            ..Default::default()
        },
    )
    .unwrap();
    for keep in [&used, &previous, &current] {
        let why = versions::clean("agent-cli", &[keep.clone(), unused.clone()])
            .expect_err("a kept version was offered for removal");
        assert!(why.contains(keep.as_str()), "{why}");
        assert!(
            depot::is_published(&AGENT_CLI, &unused),
            "a refused clean removed something"
        );
    }
    let done =
        versions::clean("agent-cli", std::slice::from_ref(&unused)).expect("unused is removed");
    assert_eq!(done.removed, vec![unused.clone()]);
    assert!(!depot::is_published(&AGENT_CLI, &unused));
    for keep in [&used, &previous, &current] {
        assert!(depot::is_published(&AGENT_CLI, keep), "{keep} was removed");
    }
}

/// **The launcher's table of CLIs is `fleet::versions::CLIS`, row for row.**
///
/// What would make it fail: a CLI added to one and not the other, or a package or command spelled
/// differently in the two.
#[test]
fn the_launchers_cli_table_is_this_one() {
    let script = include_str!("../../src/box-session.sh");
    let line = script
        .lines()
        .find(|l| l.starts_with("skein_clis=("))
        .expect("the launcher's CLI table");
    let want: Vec<String> = versions::CLIS
        .iter()
        .map(|c| format!("\"{} {} {}\"", c.runtime, c.package, c.command))
        .collect();
    assert_eq!(line, format!("skein_clis=({})", want.join(" ")));
}

/// Start box `name` as [`start`] does, as a box of the repo whose managed store is `store`, so the
/// launcher gives it the repo's tools area (SKEIN-1431).
fn start_in_repo(
    name: &str,
    prefix: &Path,
    store: &Path,
    agent: &str,
    report: &str,
) -> (String, u32, String) {
    let script = with_launch_env(
        &format!(
            "SKEIN_NPM_PREFIX={} {}",
            prefix.display(),
            session_script(name, "skein-agent", agent)
        ),
        "SKEIN_BOX_STORE",
        &skein::util::sh_quote(store.to_string_lossy().as_ref()),
    );
    let launched = own_sandbox(FLEET)
        .exec(&script, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("start {name}: {e}"));
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    let path = PathBuf::from(format!("{}/tmp/{report}.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let said = fs::read_to_string(&path).unwrap_or_default();
    let _ = fs::remove_file(&path);
    (launched, anchor, said)
}

/// The bootstrap's toolchain in a fixture fleet, as `bootstrap.sh` lays it out: `rustup` in
/// `cargo/bin` (the file `rustup_bytes`), `cargo` and `rustc` as links to it, and a
/// `stable-<triple>` whose `rustc` says it is the bootstrap's.
fn bootstrap_toolchain(root: &Path, rustup_bytes: &[u8]) {
    let toolchain = root.join("boxes/.skein/toolchain");
    let bin = toolchain.join("cargo/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("rustup"), rustup_bytes).unwrap();
    fs::set_permissions(bin.join("rustup"), fs::Permissions::from_mode(0o755)).unwrap();
    for proxy in ["rustc", "cargo"] {
        std::os::unix::fs::symlink("rustup", bin.join(proxy)).unwrap();
    }
    let triple = versions::host_triple();
    let stable = toolchain.join(format!("rustup/toolchains/stable-{triple}/bin"));
    fs::create_dir_all(&stable).unwrap();
    for tool in ["rustc", "cargo"] {
        fs::write(
            stable.join(tool),
            format!("#!/bin/sh\necho {tool}-from-bootstrap\n"),
        )
        .unwrap();
        fs::set_permissions(stable.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// **In a repo with a tools area and no toolchain file, `cargo` and `rustc` run the fleet's own
/// toolchain, linked as the area's default with no download; once a default is rolled out, the
/// link moves to it** (SKEIN-1431). Through the real rustup, which is what reads the link.
///
/// What would make it fail: the shim not making the `fleet` link, or not writing `settings.toml`
/// (rustup has no default and says so); the session block leaving `RUSTUP_HOME` at the bootstrap's
/// home ("home"); a link made once and never remade (the second box still runs the bootstrap's);
/// the launcher not reporting the default it linked as bound.
#[test]
fn a_repo_with_no_toolchain_file_builds_with_the_linked_fleet_toolchain() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so the repo tools area's \
             default toolchain was NOT exercised",
        );
    }
    let rustup = Command::new("sh")
        .args(["-c", "command -v rustup"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|p| p.starts_with('/'));
    let Some(rustup) = rustup else {
        return skip(
            "no rustup on this machine's PATH, so the repo tools area's default toolchain was \
             NOT exercised",
        );
    };
    let (root, prefix, _pins) = versions_fleet("rustarea");
    bootstrap_toolchain(&root, &fs::read(&rustup).expect("read rustup"));
    let store = root.join("skein/repos/rustarea/store/data");
    ensure_store(&store).expect("a managed store");
    let area = root.join("boxes/.repo-tools/rustarea");
    let agent = "{ printf 'home %s\\n' \"$RUSTUP_HOME\"; \
                   printf 'here %s\\n' \"$(rustc --version 2>&1)\"; \
                   printf 'cargo %s\\n' \"$(cargo --version 2>&1)\"; \
                   printf 'link %s\\n' \"$(readlink \"$RUSTUP_HOME/toolchains/fleet\")\"; \
                 } > /tmp/rs.tmp && mv /tmp/rs.tmp /tmp/rs.report; exec sleep 300";

    let (launched, anchor, report) = start_in_repo("rustarea-main", &prefix, &store, agent, "rs");
    end("rustarea-main", anchor);
    assert!(
        field(&report, "home").ends_with("/.repo-tools/rustup"),
        "the box's RUSTUP_HOME is not its repo's area: {report:?}"
    );
    assert_eq!(
        (field(&report, "here"), field(&report, "cargo")),
        ("rustc-from-bootstrap".into(), "cargo-from-bootstrap".into()),
        "a repo with no toolchain file did not build with the fleet's toolchain: {report:?}"
    );
    let triple = versions::host_triple();
    assert!(
        fs::read_link(area.join("rustup/toolchains/fleet"))
            .is_ok_and(|t| t.ends_with(format!("toolchains/stable-{triple}"))),
        "the area's `fleet` toolchain is not a link to the bootstrap's: {report:?}"
    );
    let bound = versions::versions_from_launch(&launched).unwrap_or_default();
    assert_eq!(bound.get("rust"), None, "{launched}");

    let default = format!("1.98.0-{triple}");
    publish_toolchain(&root, &default);
    versions::rollout("rust", &default).expect("roll out the default toolchain");
    let (launched, anchor, report) = start_in_repo("rustarea-two", &prefix, &store, agent, "rs");
    end("rustarea-two", anchor);
    assert_eq!(
        field(&report, "here"),
        format!("rustc-from-{default}"),
        "after a rollout, the area's default is not the rolled-out toolchain: {report:?}"
    );
    let bound = versions::versions_from_launch(&launched).unwrap_or_default();
    assert_eq!(bound.get("rust"), Some(&default), "{launched}");
}

/// A stand-in for rustup that decides by its arguments, since a script run by `exec -a` cannot see
/// the name it was run as. The toolchain a call wants is its `+name`, else `RUSTUP_TOOLCHAIN`, else
/// the nearest `rust-toolchain.toml`'s channel, else `settings.toml`'s default; it is there when
/// `$RUSTUP_HOME/toolchains/<name>` is. `show active-toolchain` answers that; `toolchain install`
/// takes two seconds and logs its start and end beside `RUSTUP_HOME`; anything else is a proxy,
/// which says what it ran, or logs `unlocked` when its toolchain was missing (rustup's own
/// auto-install, which takes no lock).
const FAKE_RUSTUP: &str = r#"#!/bin/bash
log="$RUSTUP_HOME/../skein-test-installs.log"
plus=""
case "${1-}" in +?*) plus="${1#+}"; shift ;; esac
want() {
  [ -n "$plus" ] && { echo "$plus"; return; }
  [ -n "${RUSTUP_TOOLCHAIN-}" ] && { echo "$RUSTUP_TOOLCHAIN"; return; }
  d="$PWD"
  while [ -n "$d" ]; do
    [ -f "$d/rust-toolchain.toml" ] && { sed -n 's/^channel = "\(.*\)"$/\1/p' "$d/rust-toolchain.toml"; return; }
    d="${d%/*}"
  done
  sed -n 's/^default_toolchain = "\(.*\)"$/\1/p' "$RUSTUP_HOME/settings.toml" 2>/dev/null
}
tc="$(want)"
case "$*" in
  "show active-toolchain") [ -n "$tc" ] && [ -d "$RUSTUP_HOME/toolchains/$tc" ] ;;
  "toolchain install")
    echo "start $$" >>"$log"; sleep 2
    mkdir -p "$RUSTUP_HOME/toolchains/$tc"; echo "end $$" >>"$log" ;;
  *)
    if [ -d "$RUSTUP_HOME/toolchains/$tc" ]; then echo "ran $tc"
    else echo "unlocked $$" >>"$log"; echo "missing $tc"; exit 1; fi ;;
esac
"#;

/// **Two `cargo` runs at once in a repo whose pinned toolchain is missing install it once, one
/// after the other, under the repo's lock; a build whose toolchain is there takes no lock**
/// (SKEIN-1431). rustup is not safe for two processes on one home (rust-lang/rustup#988), and the
/// area is one home for every box of the repo. The two runs are in one box; the lock is a file in
/// the area every box of the repo has, so two boxes take it the same way.
///
/// What would make it fail: the shim installing without the lock (two `start`s before an `end`,
/// and two installs); taking the lock but not asking again once it holds it (a second install
/// after the first); not installing at all and leaving it to rustup (`unlocked` in the log). And a
/// build that waits on the lock when nothing is missing: the third run is made while another
/// process holds the lock, and must not wait for it.
#[test]
fn two_concurrent_auto_installs_are_serialised_by_the_shim() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") || !have("flock") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux or flock, so the repo's \
             install lock was NOT exercised",
        );
    }
    let (root, prefix, _pins) = versions_fleet("rustlock");
    bootstrap_toolchain(&root, FAKE_RUSTUP.as_bytes());
    let store = root.join("skein/repos/rustlock/store/data");
    ensure_store(&store).expect("a managed store");
    let tree = PathBuf::from(box_root("rustlock-main")).join("tree");
    fs::create_dir_all(&tree).unwrap();
    fs::write(
        tree.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.90.0\"\n",
    )
    .unwrap();
    let agent = "{ cargo build > /tmp/a.out 2>/tmp/a.err & cargo build > /tmp/b.out 2>/tmp/b.err; wait; \
                   printf 'a %s\\n' \"$(cat /tmp/a.out)\"; printf 'b %s\\n' \"$(cat /tmp/b.out)\"; \
                   exec 9>>\"$SKEIN_REPO_TOOLS/.locks/tools\"; flock 9; \
                   printf 'held %s\\n' \"$(timeout 20 cargo build 2>&1 || echo waited)\"; exec 9>&-; \
                 } > /tmp/rs.tmp && mv /tmp/rs.tmp /tmp/rs.report; exec sleep 300";
    let (_launched, anchor, report) = start_in_repo("rustlock-main", &prefix, &store, agent, "rs");
    end("rustlock-main", anchor);
    let log = fs::read_to_string(root.join("boxes/.repo-tools/rustlock/skein-test-installs.log"))
        .unwrap_or_default();
    let shape: Vec<&str> = log
        .lines()
        .map(|l| l.split(' ').next().unwrap_or(""))
        .collect();
    assert_eq!(
        shape,
        ["start", "end"],
        "two cargo runs at once did not install the missing toolchain exactly once, under the \
         lock:\n{log}\n{report:?}"
    );
    for run in ["a", "b"] {
        assert_eq!(
            field(&report, run),
            "ran 1.90.0",
            "run {run} did not build with the toolchain the other installed: {report:?}"
        );
    }
    assert_eq!(
        field(&report, "held"),
        "ran 1.90.0",
        "a build whose toolchain is there waited on the install lock: {report:?}"
    );
}
