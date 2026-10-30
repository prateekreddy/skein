//! The agent CLIs and Rust toolchains a box runs: known versions from the fleet's depot, read-only
//! in every box, recorded at start, and moved onto a rollout only when the box is idle
//! (SKEIN-1372). Every CLI and toolchain here is a fixture tree; nothing is downloaded
//! (docs/decisions/tests-never-reach-a-live-fleet.md).

use super::*;
use skein::fleet::depot;
use skein::fleet::versions::{self, AGENT_CLI, RUST_TOOLCHAIN};
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

/// Publish a fixture claude version into the depot the way a fill does after its check, test it
/// through the real check namespace, and return its entry name.
fn publish_claude(root: &Path, version: &str) -> String {
    let entry = format!("claude@{version}");
    let out = root.join(format!("staged-{entry}"));
    npm_package(
        &out.join(&entry),
        "@anthropic-ai/claude-code",
        "bin/claude.exe",
        "claude",
        &format!("echo '{version} (Claude Code)'"),
    );
    let staged = depot::verify(&out, &AGENT_CLI).expect("a fixture prefix passes the check");
    depot::publish(&out, &depot::kind_dir(&AGENT_CLI), &staged).expect("published");
    let tested = versions::test_cli(FLEET, &entry).expect("the entry is tested");
    assert!(
        tested.ok,
        "the fixture {entry} failed its own test: {tested:?}"
    );
    entry
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
    let entry = publish_claude(&root, "1.0.0");
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
            "1.0.0 (Claude Code)",
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

/// **A box records the versions it was started on; a rollout makes a running box "restart to
/// apply" without touching what it runs; an idle box is then restarted onto the new version, and a
/// working box is not** (the owner's decisions 1 and 2).
///
/// Two boxes start on `claude@1.0.0`. `1.0.1` is published, tested and rolled out. Both are then
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
    let first = publish_claude(&root, "1.0.0");
    versions::rollout("claude", &first).expect("roll out 1.0.0");
    let p = prefix.display().to_string();
    let agent = format!(
        "{{ printf 'cli %s\\n' \"$('{p}/bin/claude' --version 2>&1)\"; }} > /tmp/v.tmp \
         && mv /tmp/v.tmp /tmp/v.report; exec sleep 300"
    );
    let mut anchors = std::collections::BTreeMap::new();
    for name in ["rollidle-main", "rollbusy-main"] {
        let (launched, anchor, report) = start(name, false, &prefix, &agent, "v");
        assert_eq!(
            field(&report, "cli"),
            "1.0.0 (Claude Code)",
            "{name}: {report:?}"
        );
        record_start(name, &launched, anchor);
        assert_eq!(
            shared_record(name).unwrap().versions,
            Some(std::collections::BTreeMap::from([(
                "claude".to_string(),
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

    let second = publish_claude(&root, "1.0.1");
    versions::rollout("claude", &second).expect("roll out 1.0.1");
    for name in ["rollidle-main", "rollbusy-main"] {
        assert!(
            stale(name).contains(&"agent-cli"),
            "{name} is behind a rollout and the board would not say so: {:?}",
            stale(name)
        );
        let still = own_sandbox(FLEET)
            .exec(
                &format!(
                    "nsenter -t {} -U -m --preserve-credentials '{p}/bin/claude' --version",
                    anchors[name]
                ),
                Duration::from_secs(30),
            )
            .unwrap_or_default();
        assert_eq!(
            still.trim(),
            "1.0.0 (Claude Code)",
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
        "1.0.1 (Claude Code)",
        "the idle box was not moved onto the rollout"
    );
    assert_eq!(
        shared_record("rollidle-main").unwrap().versions,
        Some(std::collections::BTreeMap::from([(
            "claude".to_string(),
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
    let used = publish_claude(&root, "1.0.0");
    let previous = publish_claude(&root, "1.0.1");
    let current = publish_claude(&root, "1.0.2");
    let unused = publish_claude(&root, "1.0.3");
    versions::rollout("claude", &previous).unwrap();
    versions::rollout("claude", &current).unwrap();
    record_place(
        "cleanuse-main",
        &PlaceRecord {
            sandbox: FLEET.into(),
            versions: Some(std::collections::BTreeMap::from([(
                "claude".to_string(),
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
    let done = versions::clean("agent-cli", &[unused.clone()]).expect("unused is removed");
    assert_eq!(done.removed, [unused.clone()]);
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
