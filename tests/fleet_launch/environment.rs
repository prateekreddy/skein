//! What a box receives from the environment that started it or reached into it: the
//! session's allow-list, a crossing's, and the GitHub token a scoped box holds or does not.

use super::*;

/// **A box's session gets the launcher's allow-list of the environment, and nothing else from the
/// process that started it** (SKEIN-972).
///
/// The cockpit starts every box, so whatever the cockpit was started with used to be in every box:
/// `SKEIN_HOME`, and `SKEIN_LISTEN_INHERITED_ONLY`, which the doorway sets for the cockpit alone.
/// `src/box-session.sh` now keeps only the names on `inherited_env`, each with its reason.
///
/// Driven through the real mechanism: this process's environment stands for the cockpit's, because
/// `Place::exec` spawns the launcher exactly as `skein-server` does, and the answer is read from a
/// file the box's own agent wrote from inside its namespace. Asking the running box instead would
/// be wrong: a crossing enters through `nsenter` with an environment of its own and reports that.
///
/// What would make each assertion fail:
///   * the canary: putting `SKEIN_TEST_LEAK_CANARY` on the list, or deleting the filter. A name no
///     list would carry is what tells an allow-list from a deny-list of the names noticed so far.
///   * the cockpit's three: deleting the filter.
///   * `FOREIGN_TEST_LEAK_CANARY`: a filter that unsets only `SKEIN_*` names, which passes every
///     other canary here because they all carry that prefix.
///   * the exported function: deleting the launcher's `exec env -u …` that drops names bash cannot
///     unset.
///   * `SANDBOX_NAME`: a filter that removes everything, which would also take the proxy and the
///     credential placeholders every box needs.
///   * `SKEIN_BOX`: running the filter after the launcher's own exports rather than before them.
///   * `SKEIN_IN_BOX`: deleting the launcher's `export SKEIN_IN_BOX=1` (SKEIN-1086).
///
/// Only names and two chosen values leave the box: the fixture is kept when a test fails, and the
/// environment of a developer's box carries real credentials.
#[test]
fn a_box_session_inherits_only_its_allow_list() {
    let _env = env_lock();
    // The real crossing is the subject, as in the test above.
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so it cannot host a box",
        );
    }
    // Its own name, so its cgroup cannot be another test's, in this binary or in another run.
    let name = "envlist-main";
    let root = scratch_named("env");
    let sandbox_home = sandbox_home_with_agent(&root);

    let mut pins = env_pins();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        // What must not arrive: a name nobody would list, and the cockpit's own three.
        .set("SKEIN_TEST_LEAK_CANARY", "skein-test-canary")
        // And one with no `SKEIN_` prefix: a filter that strips only skein's own names would
        // pass every other canary here, and a cloud credential in the cockpit's environment is
        // exactly that shape.
        .set("FOREIGN_TEST_LEAK_CANARY", "foreign-test-canary")
        .set("SKEIN_LISTEN_INHERITED_ONLY", "1")
        .set("SKEIN_IN_FLEET", "1")
        .set("BASH_FUNC_skein_leak%%", "() {  echo leaked\n}")
        // A name that is not an identifier, which bash can neither hold nor unset.
        .set("SKEIN_TEST_ODD-NAME", "1")
        // What must: one of the sandbox's own, which is on the list.
        .set("SANDBOX_NAME", "skein-test-allowed");
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh");

    // Written to a temporary name and moved, so the poll below never reads half a report. The
    // canary's value is spelled in two pieces so this command line cannot be what matches it.
    let agent = "{ printf 'canary %s\\n' \"$(env | grep -c 'skein-test-cana''ry')\"; \
                 printf 'allowed %s\\n' \"${SANDBOX_NAME-}\"; \
                 printf 'inbox %s\\n' \"${SKEIN_IN_BOX-}\"; \
                 if type skein_leak >/dev/null 2>&1; then echo 'fn defined'; else echo 'fn absent'; fi; \
                 env | cut -d= -f1 | sed 's/^/name /'; } > /tmp/env.tmp && mv /tmp/env.tmp /tmp/env.report; \
                 exec sleep 300";
    let launched = own_sandbox(FLEET)
        .exec(
            &session_script(name, "skein-agent", agent),
            Duration::from_secs(60),
        )
        .expect("start the box");
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");

    let report_path = PathBuf::from(format!("{}/tmp/env.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !report_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let report = fs::read_to_string(&report_path).unwrap_or_default();
    // And the environment of the box's tmux server, which every window and pane in the box is made
    // from. Needed beside the pane's own report because this box is uncovered, so its pane starts
    // behind the launcher's `sh -c` banner wrapper — and dash drops a variable whose name is not an
    // identifier, which an exported function's `BASH_FUNC_<name>%%` is not. A covered box has no
    // wrapper, so the pane alone would be asking a question this fixture answers by accident.
    // Names only, for the reason the report is.
    let server_env: Vec<String> = Command::new("tmux")
        .args(["-S", &box_sock(name), "show-environment", "-g"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.split_once('=').map(|(n, _)| n.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let limits = fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
    // Ended before asserting, so a failure leaves no box running; `Scratch` would stop it too.
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

    assert!(
        !report.is_empty(),
        "the box's agent never wrote its environment, so nothing below would be about a box"
    );
    let field = |key: &str| {
        report
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or("")
            .to_string()
    };
    let names: Vec<&str> = report
        .lines()
        .filter_map(|l| l.strip_prefix("name "))
        .collect();
    assert!(
        !names.contains(&"SKEIN_TEST_LEAK_CANARY") && field("canary") == "0",
        "a variable on no list reached the box from the process that started it, so a box still \
         inherits the cockpit's environment rather than the launcher's allow-list: {names:?}"
    );
    assert!(
        !names.contains(&"FOREIGN_TEST_LEAK_CANARY"),
        "a variable with no SKEIN_ prefix and on no list reached the box, so the launcher's filter \
         strips skein's own names rather than everything off its list: {names:?}"
    );
    for cockpit in [
        "SKEIN_LISTEN_INHERITED_ONLY",
        "SKEIN_IN_FLEET",
        "SKEIN_HOME",
    ] {
        assert!(
            !names.contains(&cockpit),
            "{cockpit} is the cockpit's, and it reached the box: {names:?}"
        );
    }
    assert!(
        server_env.iter().any(|n| n == "HOME"),
        "the box's tmux server reported no environment, so the two checks below would be about \
         nothing: {server_env:?}"
    );
    assert!(
        !server_env.iter().any(|n| n == "SKEIN_TEST_LEAK_CANARY"
            || n == "FOREIGN_TEST_LEAK_CANARY"
            || n.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))),
        "the box's tmux server, which every window in the box is made from, carries the canary, \
         an exported function or another name that is not an identifier from the cockpit's \
         environment: {server_env:?}"
    );
    assert_eq!(
        field("fn"),
        "absent",
        "an exported shell function from the cockpit's environment is defined in the box"
    );
    assert_eq!(
        field("allowed"),
        "skein-test-allowed",
        "SANDBOX_NAME is on the allow-list and did not arrive, so the filter is removing what a box \
         needs rather than what it was not given: {names:?}"
    );
    assert_eq!(
        field("inbox"),
        "1",
        "the launcher did not mark the session as a box, so a skein-server started in it would \
         honour $SKEIN_NO_API_AUTH on the fleet's shared network namespace (SKEIN-1086): {names:?}"
    );
    assert!(
        names.contains(&"SKEIN_BOX"),
        "the launcher exports SKEIN_BOX on purpose and the box does not have it, so the filter ran \
         over the launcher's own exports: {names:?}"
    );
}

/// **A crossing into a box gets the same allow-list its session did, and is marked as in a box**
/// (SKEIN-1085).
///
/// The test above is about the session, which the launcher starts. This is the other way in: a
/// `Place` crossing, which `nsenter`s from skein-server with the server's environment — the path
/// provisioning, the attach shell, the pane observer and a model call all take. Driven the same
/// way: this process's environment stands for the cockpit's, a real box is started, placed, and
/// crossed into with `Place::exec`, and the crossing reports its own environment from inside the
/// namespace.
///
/// What would make each assertion fail:
///   * the canary and the cockpit's three: deleting the filter in `Place::enter`
///     (`keep_only_listed`), or putting the canary on the launcher's list.
///   * `FOREIGN_TEST_LEAK_CANARY`: a filter that unsets only `SKEIN_*` names.
///   * the exported function and the odd name: dropping `"${skein_odd[@]}"` from the `exec env`.
///   * `SANDBOX_NAME`: a filter that keeps nothing, or one reading an empty list.
///   * `TERM`: dropping it from `CROSSING_ALSO`, which leaves an attach with no terminal to draw on.
///   * `SKEIN_IN_BOX`: deleting `SKEIN_IN_BOX=1` from the crossing's `exec`.
///   * that the report is from inside the box at all: the mount namespace it read equals the
///     anchor's, which fails if the crossing ever stopped entering.
///
/// Names and three chosen values only leave the box, for the reason the test above gives.
#[test]
fn a_crossing_into_a_box_carries_only_its_allow_list() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so it cannot host a box",
        );
    }
    let name = "crossenv-main";
    let root = scratch_named("crossenv");
    let sandbox_home = sandbox_home_with_agent(&root);

    let mut pins = env_pins();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .set("SKEIN_TEST_LEAK_CANARY", "skein-test-canary")
        // And one with no `SKEIN_` prefix: a filter that strips only skein's own names would
        // pass every other canary here, and a cloud credential in the cockpit's environment is
        // exactly that shape.
        .set("FOREIGN_TEST_LEAK_CANARY", "foreign-test-canary")
        .set("SKEIN_LISTEN_INHERITED_ONLY", "1")
        .set("SKEIN_IN_FLEET", "1")
        .set("BASH_FUNC_skein_leak%%", "() {  echo leaked\n}")
        .set("SKEIN_TEST_ODD-NAME", "1")
        .set("SANDBOX_NAME", "skein-test-allowed")
        .set("TERM", "skein-test-term")
        // Set here so that it arriving says the crossing set it, not that it was inherited.
        .set("SKEIN_IN_BOX", "0");
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh");

    let launched = own_sandbox(FLEET)
        .exec(
            &session_script(name, "skein-agent", "exec sleep 300"),
            Duration::from_secs(60),
        )
        .expect("start the box");
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    let ns_start = sh(&format!(
        "sed -n 's/.*) //p' /proc/{anchor}/stat | cut -d' ' -f20"
    ))
    .trim()
    .parse::<u64>()
    .expect("the anchor's start time");
    record_place(
        name,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            home: sandbox_home.display().to_string(),
            tree: "/".into(),
            sock: box_sock(name),
            generation: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .expect("a boot id")
                .trim()
                .to_string(),
            ns_start,
            ..Default::default()
        },
    )
    .expect("place the box");

    // The canary's value is spelled in two pieces so this command line cannot be what matches it.
    let crossed = place_of(name).expect("placed").exec(
        "printf 'canary %s\\n' \"$(env | grep -c 'skein-test-cana''ry')\"; \
         printf 'allowed %s\\n' \"${SANDBOX_NAME-}\"; \
         printf 'term %s\\n' \"${TERM-}\"; \
         printf 'inbox %s\\n' \"${SKEIN_IN_BOX-}\"; \
         printf 'mnt %s\\n' \"$(readlink /proc/self/ns/mnt)\"; \
         if type skein_leak >/dev/null 2>&1; then echo 'fn defined'; else echo 'fn absent'; fi; \
         env | cut -d= -f1 | sed 's/^/name /'",
        Duration::from_secs(30),
    );
    let anchor_mnt = fs::read_link(format!("/proc/{anchor}/ns/mnt"))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let limits = fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
    forget_place(name);
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

    let report = crossed.expect("the crossing into the box did not run");
    let field = |key: &str| {
        report
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or("")
            .to_string()
    };
    let names: Vec<&str> = report
        .lines()
        .filter_map(|l| l.strip_prefix("name "))
        .collect();
    assert!(
        !anchor_mnt.is_empty() && field("mnt") == anchor_mnt,
        "the crossing did not run in the box's mount namespace, so nothing below is about a box: \
         {:?} against the anchor's {anchor_mnt:?}",
        field("mnt")
    );
    assert!(
        !names.contains(&"SKEIN_TEST_LEAK_CANARY") && field("canary") == "0",
        "a variable on no list crossed into the box, so a crossing still carries the cockpit's \
         whole environment rather than the launcher's allow-list: {names:?}"
    );
    assert!(
        !names.contains(&"FOREIGN_TEST_LEAK_CANARY"),
        "a variable with no SKEIN_ prefix and on no list crossed into the box, so the crossing's \
         filter strips skein's own names rather than everything off its list: {names:?}"
    );
    for cockpit in [
        "SKEIN_LISTEN_INHERITED_ONLY",
        "SKEIN_IN_FLEET",
        "SKEIN_HOME",
    ] {
        assert!(
            !names.contains(&cockpit),
            "{cockpit} is the cockpit's, and it crossed into the box: {names:?}"
        );
    }
    assert!(
        !names
            .iter()
            .any(|n| n.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))),
        "a name that is not an identifier — an exported function or an odd name — crossed into \
         the box: {names:?}"
    );
    assert_eq!(
        field("fn"),
        "absent",
        "an exported shell function from the cockpit's environment is defined in the crossing"
    );
    assert_eq!(
        field("allowed"),
        "skein-test-allowed",
        "SANDBOX_NAME is on the allow-list and did not cross, so the filter removes what a box \
         needs rather than what it was not given: {names:?}"
    );
    assert_eq!(
        field("term"),
        "skein-test-term",
        "TERM did not cross, so `tmux attach-session` in an attach crossing has no terminal to \
         draw on: {names:?}"
    );
    assert_eq!(
        field("inbox"),
        "1",
        "the crossing did not mark what it runs as in a box, so a skein-server started from it \
         would honour $SKEIN_NO_API_AUTH on the fleet's shared network namespace: {names:?}"
    );
}

/// `session_script`'s `KEY='value'` assignment for `key`, replaced by `KEY=value`.
///
/// Asserted to have found it, so a renamed or re-quoted assignment fails here, by name, rather than
/// leaving the launch on whatever the fixture's config chose and every assertion after it about a
/// box of the other kind.
fn with_launch_env(script: &str, key: &str, value: &str) -> String {
    let at = script
        .find(&format!("{key}="))
        .unwrap_or_else(|| panic!("session_script no longer sets {key}: {script}"));
    let end = at
        + script[at..]
            .find(' ')
            .expect("an assignment followed by more");
    format!("{}{key}={value}{}", &script[..at], &script[end..])
}

/// **A crossing into a scoped box carries that box's own `GH_TOKEN` or none, never the fleet's; a
/// `fleet`-scoped box's crossing still carries the fleet's** (SKEIN-1095).
///
/// The launcher replaces `GH_TOKEN` for a scoped box with its own-repo token, or removes it, and
/// drops `ANTHROPIC_API_KEY`/`OPENAI_API_KEY` for a box with a login of its own. A crossing is
/// spawned by skein-server, whose environment has the fleet's token; this is that crossing, for
/// real, into three real boxes started from this process's environment:
///
///   * `scoped`: `SKEIN_GIT_SCOPE=repo`, an own-repo token planted where the launcher reads it, and
///     a Claude login in the box's private home.
///   * `bare`: scoped, with no own-repo token placed.
///   * `fleet`: what the fixture's config gives, which is `fleet` (it can issue no write tokens).
///
/// What would make each assertion fail:
///   * the session's `gh`: the launcher no longer swapping the token. It is the baseline the
///     crossing's `gh` is held to, so without it that assertion proves nothing.
///   * the crossing's `gh` for `scoped` and `bare`: deleting `as_the_session_holds` from
///     `Place::enter`, which carries skein-server's `skein-test-fleet-token` in.
///   * the API keys against the session, and `scoped`'s `anthropic` being `none`: `GH_TOKEN`
///     alone taken from the session, the API keys left as skein-server's.
///   * `openai` kept: the API keys unset unconditionally rather than as the session holds them.
///   * the crossing's `gh` for `fleet`: the decided names unset and never taken back, or
///     `GH_TOKEN` dropped for every box, which is the change the owner would notice.
///
/// Only `skein-test-` values exist in this environment, so the reports carry values.
#[test]
fn a_crossing_into_a_scoped_box_carries_its_own_github_token_or_none() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so it cannot host a box",
        );
    }
    let root = scratch_named("ghscope");
    let sandbox_home = sandbox_home_with_agent(&root);
    let mut pins = env_pins();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .set("GH_TOKEN", "skein-test-fleet-token")
        .set("ANTHROPIC_API_KEY", "skein-test-proxy-key")
        .set("OPENAI_API_KEY", "skein-test-proxy-key");
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh");

    let report = "printf 'gh %s\\nanthropic %s\\nopenai %s\\n' \"${GH_TOKEN-none}\" \
                  \"${ANTHROPIC_API_KEY-none}\" \"${OPENAI_API_KEY-none}\"";
    let mut seen = Vec::new();
    for (case, name) in [
        ("scoped", "ghscope-main"),
        ("bare", "ghbare-main"),
        ("fleet", "ghfleet-main"),
    ] {
        let mut script = session_script(
            name,
            "skein-agent",
            &format!(
                "{{ {report}; }} > /tmp/s.tmp && mv /tmp/s.tmp /tmp/session.report; exec sleep 300"
            ),
        );
        assert!(
            script.contains("SKEIN_GIT_SCOPE='fleet'"),
            "this fixture was expected to give a box the fleet scope, so `fleet` below would not \
             be a fleet-scoped box: {script}"
        );
        if case != "fleet" {
            script = with_launch_env(&script, "SKEIN_GIT_SCOPE", "repo");
            script = with_launch_env(&script, "SKEIN_BOX_REPO", "example/thing");
        }
        if case == "scoped" {
            let tokens = PathBuf::from(box_state(name)).join("git-tokens");
            fs::create_dir_all(&tokens).unwrap();
            fs::write(tokens.join("example%2Fthing"), "skein-test-box-token").unwrap();
            let claude = PathBuf::from(box_root(name)).join("home/.claude");
            fs::create_dir_all(&claude).unwrap();
            fs::write(
                claude.join(".credentials.json"),
                r#"{"claudeAiOauth":{"accessToken":"skein-test-access","refreshToken":"skein-test-refresh"}}"#,
            )
            .unwrap();
        }
        let launched = own_sandbox(FLEET)
            .exec(&script, Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("start the {case} box: {e}"));
        let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
        let ns_start = sh(&format!(
            "sed -n 's/.*) //p' /proc/{anchor}/stat | cut -d' ' -f20"
        ))
        .parse::<u64>()
        .expect("the anchor's start time");
        record_place(
            name,
            &PlaceRecord {
                sandbox: FLEET.into(),
                ns_pid: anchor,
                home: sandbox_home.display().to_string(),
                tree: "/".into(),
                sock: box_sock(name),
                generation: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                    .expect("a boot id")
                    .trim()
                    .to_string(),
                ns_start,
                ..Default::default()
            },
        )
        .expect("place the box");
        let crossed = place_of(name)
            .expect("placed")
            .exec(report, Duration::from_secs(30));
        let session_path = PathBuf::from(format!("{}/tmp/session.report", box_root(name)));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !session_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let session = fs::read_to_string(&session_path).unwrap_or_default();
        let limits =
            fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
        forget_place(name);
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
        seen.push((case, session, crossed.map_err(|e| e.to_string())));
    }

    let field = |text: &str, key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or("")
            .to_string()
    };
    for (case, session, crossed) in &seen {
        let crossed = crossed
            .as_ref()
            .unwrap_or_else(|e| panic!("the crossing into the {case} box did not run: {e}"));
        // The GitHub token each kind of box is given, pinned: this is the property itself.
        let gh = match *case {
            "scoped" => "skein-test-box-token",
            "bare" => "none",
            _ => "skein-test-fleet-token",
        };
        assert_eq!(
            field(session, "gh"),
            gh,
            "the {case} box's own session does not hold the GH_TOKEN the launcher gives that kind \
             of box, so the crossing assertions below have no baseline: {session:?}"
        );
        assert_eq!(
            field(crossed, "gh"),
            gh,
            "a crossing into the {case} box carries a GH_TOKEN other than the one its session \
             holds: skein-test-fleet-token into a scoped box is SKEIN-1095, and none into the \
             fleet box is that box losing the token it is meant to keep: {crossed:?}"
        );
        // The API keys follow the session, whatever it was given. Which boxes have a login is
        // not this test's to pin: the launcher seeds a login from one box's home into another's,
        // so the `scoped` box's login can reach the two started after it.
        for (key, name) in [
            ("anthropic", "ANTHROPIC_API_KEY"),
            ("openai", "OPENAI_API_KEY"),
        ] {
            assert_eq!(
                field(crossed, key),
                field(session, key),
                "a crossing into the {case} box carries a {name} other than the one its session \
                 holds: crossing {crossed:?}, session {session:?}"
            );
        }
        // And one of each, pinned, so the comparison above cannot pass with both sides the same
        // for the wrong reason: dropped where the box has a login, kept where no box has one.
        if *case == "scoped" {
            assert_eq!(
                field(crossed, "anthropic"),
                "none",
                "the scoped box has a Claude login, and a crossing into it still carries the \
                 placeholder ANTHROPIC_API_KEY its session was not given: {crossed:?}"
            );
        }
        assert_eq!(
            field(crossed, "openai"),
            "skein-test-proxy-key",
            "no box here has a Codex login, and a crossing into the {case} box lost the \
             OPENAI_API_KEY its session was given: {crossed:?}"
        );
    }
}
