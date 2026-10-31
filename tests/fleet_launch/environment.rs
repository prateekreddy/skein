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
pub(super) fn with_launch_env(script: &str, key: &str, value: &str) -> String {
    let at = script
        .find(&format!("{key}="))
        .unwrap_or_else(|| panic!("session_script no longer sets {key}: {script}"));
    let end = at
        + script[at..]
            .find(' ')
            .expect("an assignment followed by more");
    format!("{}{key}={value}{}", &script[..at], &script[end..])
}

/// **A crossing into a scoped box carries no `GH_TOKEN`, never the fleet's; a `fleet`-scoped box's
/// crossing still carries the fleet's** (SKEIN-1095, SKEIN-1332).
///
/// The launcher removes `GH_TOKEN` for a scoped box, whether or not an own-repo token is placed
/// (git and `gh` read that file on every call instead), and drops
/// `ANTHROPIC_API_KEY`/`OPENAI_API_KEY` for a box with a login of its own. A crossing is
/// spawned by skein-server, whose environment has the fleet's token; this is that crossing, for
/// real, into three real boxes started from this process's environment:
///
///   * `scoped`: `SKEIN_GIT_SCOPE=repo`, an own-repo token planted where the launcher reads it, and
///     a Claude login in the box's private home.
///   * `bare`: scoped, with no own-repo token placed.
///   * `fleet`: what the fixture's config gives, which is `fleet` (it can issue no write tokens).
///
/// What would make each assertion fail:
///   * the session's `gh`: the launcher no longer dropping the fleet's token, or exporting the
///     placed own-repo token again (SKEIN-1332: a forgotten key would then go on working from
///     `$GH_TOKEN`). It is the baseline the crossing's `gh` is held to.
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
        // What the launcher says it decided, which the placement record keeps and the board
        // compares with the configuration (SKEIN-1331): the scope each case was given, the cover
        // an unmatched name gets, and no telemetry plugin. What would make these fail: the
        // launcher not printing them, or printing the switch's input rather than its decision.
        assert_eq!(
            (
                skein::fleet::scope_from_launch(&launched),
                skein::fleet::exposure_from_launch(&launched).as_deref(),
                skein::fleet::telemetry_from_launch(&launched),
            ),
            (Some(case != "fleet"), Some("uncovered"), Some(false)),
            "the {case} box's launcher did not report what it decided: {launched:?}"
        );
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
            "scoped" | "bare" => "none",
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

/// **`gh` in a running scoped box uses the token placed for it now, not the one the box started
/// with, and a forgotten token is nowhere in the box's environment** (SKEIN-1240, SKEIN-1332).
///
/// Replacing a key in Settings rewrites the box's placed token file at once, and forgetting one
/// removes it. An exported `GH_TOKEN` would be a snapshot taken when the launcher ran, held by
/// every process in the box (and every crossing, which copies it from the box's tmux server) until
/// the box is restarted, so the launcher exports none. The `gh` shim the launcher binds over the
/// real `gh` reads the file on every call instead. Driven for
/// real: a real box, the real `gh` behind the shim, and `gh auth token`, which prints the
/// credential `gh` resolved without reaching GitHub. Only `skein-test-` values are placed here.
///
/// The box's own session shell is the long-lived process the owner reported: it asks once, waits
/// while the test replaces the file, then asks again, all with one environment.
///
/// What would make each assertion fail:
///   * `before`'s `gh`: the shim not reading the placed own-repo token. It is the baseline.
///   * every `env`: the launcher exporting the placed token as `GH_TOKEN` again (SKEIN-1332), which
///     leaves `removed`'s environment holding a key that was forgotten.
///   * `after` in the session, and the crossing's `gh`: the shim gone, so `gh` has nothing, or a
///     shim that reads the file once — the SKEIN-1240 bug.
///   * the chosen token: a shim that replaces every `GH_TOKEN` unconditionally, which would take
///     the owner's own token away from a review crossing.
///   * `removed`: a shim that leaves the old exported value in place when no file is placed, so a
///     revoked key would go on being presented.
///
/// **Hermetic against the machine's own GitHub login** (SKEIN-1342). On a fleet box the `gh` the
/// launcher finds is that box's own shim, which supplies that box's real token whenever it is
/// handed none; the launcher wraps the binary behind it instead, and `removed` fails if it does
/// not. Every failure message goes through `redacted`, so a credential that is not one of this
/// test's own `skein-test-` values is never printed, and the comparisons are `assert!` rather than
/// `assert_eq!`, whose `left`/`right` would print the raw value.
#[test]
fn gh_in_a_running_box_uses_the_token_placed_now() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") || !have("gh") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux or gh, so it cannot host a \
             box with gh in it",
        );
    }
    let name = "ghlive-main";
    let root = scratch_named("ghlive");
    let sandbox_home = sandbox_home_with_agent(&root);
    let mut pins = env_pins();
    // Everything `gh` reads a credential from, pinned to nothing of the machine's (SKEIN-1342):
    // its config directory and the XDG one it falls back to, the session bus its keyring is
    // reached through, and every token variable it honours. `GH_TOKEN` is set rather than unset,
    // to the fleet's stand-in, because a scoped box dropping it is part of what is tested. The box
    // itself inherits only the launcher's allow-list, which carries none of the rest; pinning them
    // here keeps the host side of this test from depending on that.
    let gh_config = root.join("gh-config");
    fs::create_dir_all(&gh_config).unwrap();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .set("GH_TOKEN", "skein-test-fleet-token")
        .set("GH_CONFIG_DIR", &gh_config)
        .set("XDG_CONFIG_HOME", root.join("xdg-config"))
        .unset("GITHUB_TOKEN")
        .unset("GH_ENTERPRISE_TOKEN")
        .unset("GITHUB_ENTERPRISE_TOKEN")
        .unset("DBUS_SESSION_BUS_ADDRESS");
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh");

    // `gh auth token` exits non-zero with nothing on stdout when it has no credential.
    let report = "t=$(gh auth token 2>/dev/null); printf 'gh %s\\nenv %s\\n' \"${t:-none}\" \
                  \"${GH_TOKEN-none}\"";
    let mut script = session_script(
        name,
        "skein-agent",
        &format!(
            "{{ {report}; }} > /tmp/b.tmp && mv /tmp/b.tmp /tmp/before; \
             while [ ! -e /tmp/go ]; do sleep 0.1; done; \
             {{ {report}; }} > /tmp/a.tmp && mv /tmp/a.tmp /tmp/after; exec sleep 300"
        ),
    );
    script = with_launch_env(&script, "SKEIN_GIT_SCOPE", "repo");
    script = with_launch_env(&script, "SKEIN_BOX_REPO", "example/thing");
    let tokens = PathBuf::from(box_state(name)).join("git-tokens");
    fs::create_dir_all(&tokens).unwrap();
    let placed = tokens.join("example%2Fthing");
    fs::write(&placed, "skein-test-box-token-old").unwrap();

    let launched = own_sandbox(FLEET)
        .exec(&script, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("start the box: {e}"));
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

    let box_tmp = PathBuf::from(format!("{}/tmp", box_root(name)));
    let wait_for = |file: &str| {
        let path = box_tmp.join(file);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        fs::read_to_string(&path).unwrap_or_default()
    };
    let before = wait_for("before");
    // The key is replaced the way `gitgate::refresh_tokens` replaces it: the file is rewritten
    // under a running box. Then the session's shell, still running with its first environment,
    // is let go to ask again.
    fs::write(&placed, "skein-test-box-token-new").unwrap();
    fs::write(box_tmp.join("go"), "").unwrap();
    let after = wait_for("after");
    let place = place_of(name).expect("placed");
    let crossed = place.exec(report, Duration::from_secs(30));
    let chosen = place.exec(
        "t=$(GH_TOKEN=skein-test-chosen gh auth token 2>/dev/null); printf 'gh %s\\n' \"${t:-none}\"",
        Duration::from_secs(30),
    );
    fs::remove_file(&placed).unwrap();
    let removed = place.exec(report, Duration::from_secs(30));

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

    let field = |text: &str, key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or("")
            .to_string()
    };
    let crossed = crossed.unwrap_or_else(|e| panic!("the crossing did not run: {e}"));
    let chosen = chosen.unwrap_or_else(|e| panic!("the crossing did not run: {e}"));
    let removed = removed.unwrap_or_else(|e| panic!("the crossing did not run: {e}"));

    assert!(
        field(&before, "gh") == "skein-test-box-token-old",
        "gh in the box did not use its own placed token, so nothing below has a baseline: {}",
        redacted(&before)
    );
    for (when, report) in [
        ("at start", &before),
        ("after the key was replaced", &after),
        ("in a crossing", &crossed),
        ("after the key was forgotten", &removed),
    ] {
        assert!(
            field(report, "env") == "none",
            "the box's environment holds a GH_TOKEN {when}, which keeps working after the key is \
             forgotten or replaced (SKEIN-1332): {}",
            redacted(report)
        );
    }
    assert!(
        field(&after, "gh") == "skein-test-box-token-new",
        "gh in a session started before the key was replaced still uses the old one (SKEIN-1240): \
         {}",
        redacted(&after)
    );
    assert!(
        field(&crossed, "gh") == "skein-test-box-token-new",
        "gh in a crossing still uses the old key: {}",
        redacted(&crossed)
    );
    assert!(
        field(&chosen, "gh") == "skein-test-chosen",
        "a GH_TOKEN set on purpose for one call was replaced by the placed token, which would take \
         a review crossing's own credential away: {}",
        redacted(&chosen)
    );
    assert!(
        field(&removed, "gh") == "none",
        "with no token placed for this box any more, gh still presented one: {}",
        redacted(&removed)
    );
}

/// **Every box finds the fleet's shared Rust toolchain on its PATH, and no box can change it**
/// (SKEIN-1337).
///
/// A real box over a fixture toolchain under the fleet root, laid out as `bootstrap.sh` lays out
/// the real one: `cargo/bin` with a `cargo` and a `rustup` in it, and a `rustup` home. Two boxes:
/// an ordinary one, and the workshop box, which is the one that matters for the marks — every
/// other box sees `.skein` read-only already, and the workshop box saw the fleet root as the
/// sandbox does. The marks are looked for on the HOST afterwards, so a write that a box was told
/// failed and that landed anyway still fails the test.
///
/// What would make each assertion fail:
///   * `cargo`: dropping `$HOME/.cargo/bin` from `box_path`, or the bind over it.
///   * `rustup_home`: dropping the session block's `RUSTUP_HOME` export.
///   * `cargo_home`: pointing `CARGO_HOME` at the shared toolchain, whose registry would then be
///     one every box writes, or leaving it unset.
///   * the marks: dropping the `--ro-bind` of the toolchain (the workshop box writes them).
///   * `show`: a shim that refuses everything.
///   * `default`: dropping the shim, which hands a mutation to the real `rustup`.
#[test]
fn every_box_finds_the_shared_toolchain_and_none_can_change_it() {
    use std::os::unix::fs::PermissionsExt;
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so it cannot host a box",
        );
    }
    let root = scratch_named("rusttool");
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
    install_launcher(FLEET).expect("install box-session.sh");

    let toolchain = root.join("boxes/.skein/toolchain");
    let bin = toolchain.join("cargo/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(toolchain.join("rustup")).unwrap();
    fs::write(toolchain.join("rustup/settings.toml"), "version = \"12\"\n").unwrap();
    for (tool, says) in [
        ("cargo", "echo skein-test-cargo"),
        ("rustup", "echo skein-test-rustup \"$@\""),
    ] {
        fs::write(bin.join(tool), format!("#!/bin/sh\n{says}\n")).unwrap();
        fs::set_permissions(bin.join(tool), fs::Permissions::from_mode(0o755)).unwrap();
    }

    let shared_bin = bin.display().to_string();
    let agent = format!(
        "{{ printf 'cargo %s\\n' \"$(cargo 2>&1)\"; \
           printf 'rustup_home %s\\n' \"${{RUSTUP_HOME-none}}\"; \
           printf 'cargo_home %s\\n' \"${{CARGO_HOME-none}}\"; \
           touch \"${{RUSTUP_HOME:-/nowhere}}/skein-test-mark\" 2>/dev/null; \
           touch \"$HOME/.cargo/bin/skein-test-mark\" 2>/dev/null; \
           touch '{shared_bin}/skein-test-mark2' 2>/dev/null; \
           printf 'show %s\\n' \"$(rustup show 2>&1)\"; \
           printf 'default %s\\n' \"$(rustup default nightly 2>&1)\"; \
         }} > /tmp/tc.tmp && mv /tmp/tc.tmp /tmp/tc.report; exec sleep 300"
    );
    for (case, name) in [("ordinary", "rusttool-main"), ("workshop", "rustshop-main")] {
        let mut script = session_script(name, "skein-agent", &agent);
        if case == "workshop" {
            script = with_launch_env(&script, "SKEIN_BOX_PRIVILEGED", "1");
        }
        let launched = own_sandbox(FLEET)
            .exec(&script, Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("start the {case} box: {e}"));
        let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
        let report_path = PathBuf::from(format!("{}/tmp/tc.report", box_root(name)));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !report_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let report = fs::read_to_string(&report_path).unwrap_or_default();
        let limits =
            fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
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

        let field = |key: &str| {
            report
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{key} ")))
                .unwrap_or("")
                .to_string()
        };
        assert_eq!(
            field("cargo"),
            "skein-test-cargo",
            "a box does not find the fleet's cargo on its PATH: {case} box: {report:?}"
        );
        assert_eq!(
            field("rustup_home"),
            toolchain.join("rustup").display().to_string(),
            "a box's RUSTUP_HOME is not the fleet's shared one: {case} box: {report:?}"
        );
        assert_eq!(
            field("cargo_home"),
            sandbox_home.join(".cargo").display().to_string(),
            "a box's CARGO_HOME is not its own: {case} box: {report:?}"
        );
        let marks: Vec<PathBuf> = [
            toolchain.join("rustup/skein-test-mark"),
            bin.join("skein-test-mark"),
            bin.join("skein-test-mark2"),
        ]
        .into_iter()
        .filter(|p| p.exists())
        .collect();
        assert!(
        marks.is_empty(),
        "the {case} box wrote into the fleet's shared toolchain, which every box runs: {marks:?}"
    );
        assert_eq!(
            field("show"),
            "skein-test-rustup show",
            "`rustup show` in a box does not reach the real rustup: {case} box: {report:?}"
        );
        assert!(
        field("default").contains("read-only") && !field("default").contains("skein-test-rustup"),
        "`rustup default nightly` in a box was not refused in skein's words: {case} box: {report:?}"
    );
    }
}

/// **`cargo install` works in a box, and what it installs is on the box's PATH** (SKEIN-1349).
///
/// SKEIN-1337 bound the fleet's toolchain `bin` read-only over every box's `~/.cargo/bin`, which is
/// where `cargo install` puts a program by default, so every install failed. The session block now
/// points `CARGO_INSTALL_ROOT` at `~/.local`. This runs the real `cargo` that is running this test,
/// reached through a fixture toolchain laid out as `bootstrap.sh` lays out the fleet's, and installs
/// a one-file crate offline in an ordinary box.
///
/// What would make it fail: dropping `CARGO_INSTALL_ROOT` from the session block's export (cargo
/// then writes into the read-only `~/.cargo/bin` and the install fails), or pointing it anywhere
/// that is not on `box_path`. And the last assertion: an install that reached the SANDBOX's own
/// `~/.local/bin`, which every box runs, rather than the box's copy-on-write view of it.
#[test]
fn cargo_install_in_a_box_lands_on_its_path() {
    use std::os::unix::fs::PermissionsExt;
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    let real_cargo = PathBuf::from(env!("CARGO"));
    let real_rustc = real_cargo.with_file_name("rustc");
    if !bwrap_works() || !have("tmux") || !real_rustc.is_file() {
        return skip(
            "this machine cannot make a bwrap namespace, lacks tmux, or has no rustc beside the \
             cargo running this test, so it cannot install a crate in a box",
        );
    }
    let root = scratch_named("cargoinst");
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
    install_launcher(FLEET).expect("install box-session.sh");

    let toolchain = root.join("boxes/.skein/toolchain");
    let bin = toolchain.join("cargo/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(toolchain.join("rustup")).unwrap();
    fs::write(
        bin.join("cargo"),
        format!(
            "#!/bin/sh\nRUSTC='{}' exec '{}' \"$@\"\n",
            real_rustc.display(),
            real_cargo.display()
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();

    let krate = root.join("hello");
    fs::create_dir_all(krate.join("src")).unwrap();
    fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"skein-test-hello\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [workspace]\n",
    )
    .unwrap();
    fs::write(
        krate.join("src/main.rs"),
        "fn main() { println!(\"skein-test-installed\"); }\n",
    )
    .unwrap();

    let name = "cargoinst-main";
    let agent = format!(
        "{{ cargo install --path '{}' --offline --target-dir /tmp/hello-target > /tmp/ci.log 2>&1; \
           printf 'rc %s\\n' \"$?\"; \
           printf 'found %s\\n' \"$(command -v skein-test-hello)\"; \
           printf 'runs %s\\n' \"$(skein-test-hello 2>&1)\"; \
           printf 'log %s\\n' \"$(tail -3 /tmp/ci.log | tr '\\n' ' ')\"; \
         }} > /tmp/ci.tmp && mv /tmp/ci.tmp /tmp/ci.report; exec sleep 300",
        krate.display()
    );
    let script = session_script(name, "skein-agent", &agent);
    let launched = own_sandbox(FLEET)
        .exec(&script, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("start the box: {e}"));
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    let report_path = PathBuf::from(format!("{}/tmp/ci.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(180);
    while !report_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let report = fs::read_to_string(&report_path).unwrap_or_default();
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

    let field = |key: &str| {
        report
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key} ")))
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(
        field("rc"),
        "0",
        "`cargo install` failed in a box: {report:?}"
    );
    assert_eq!(
        field("found"),
        sandbox_home
            .join(".local/bin/skein-test-hello")
            .display()
            .to_string(),
        "what `cargo install` put in a box is not on its PATH from ~/.local/bin: {report:?}"
    );
    assert_eq!(
        field("runs"),
        "skein-test-installed",
        "the installed program does not run in the box: {report:?}"
    );
    assert!(
        !sandbox_home.join(".local/bin/skein-test-hello").exists(),
        "a box's `cargo install` landed in the sandbox's own ~/.local/bin, which every box runs"
    );
}

/// What one box of a managed-store repo reported, through its session and through a crossing.
struct CacheCrossing {
    /// `var <NAME> <value or none>` for each name the session block points at the repo cache,
    /// written by the box's own agent from inside its session.
    session: String,
    /// The same lines from a `Place::exec` crossing into that box, then `cargo <rc>` and
    /// `npm <rc or absent>` for a cache write by each tool, run through the same crossing.
    crossed: String,
    /// The repo's tools area on this side of the box, `<fleet root>/.repo-tools/<id>`.
    cache: PathBuf,
    /// Where the box sees the area, `~/.repo-tools` of the sandbox's home.
    seen_at: String,
    /// Where `cache` links to when the case makes it a link, and otherwise nothing.
    elsewhere: PathBuf,
    /// The fixture both paths are in, kept until the assertions on them are done.
    _root: Scratch,
}

/// Start box `<tag>-main` of a repo whose store has the managed shape, `repos/<tag>/store/data`,
/// cross into it with `Place::exec`, and run a cargo and an npm cache write through that crossing.
/// With `linked`, the repo's tools area is a link to another directory, which the launcher
/// refuses, so the box has no area and no repo cache (SKEIN-1431).
///
/// This process's environment stands for skein-server's, as in the tests above, and it carries a
/// `CARGO_HOME`, `npm_config_cache` and `PIP_CACHE_DIR` of its own: skein-server runs with them on
/// a real fleet (`bootstrap.sh` exports `CARGO_HOME`), and none of them is this box's.
fn cross_into_a_box_of_a_managed_store(tag: &str, linked: bool) -> CacheCrossing {
    let real_cargo = PathBuf::from(env!("CARGO"));
    let real_rustc = real_cargo.with_file_name("rustc");
    let name = format!("{tag}-main");
    let root = scratch_named(tag);
    let sandbox_home = sandbox_home_with_agent(&root);
    let mut pins = env_pins();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .set("CARGO_HOME", root.join("skein-test-server-cargo"))
        .set("npm_config_cache", root.join("skein-test-server-npm"))
        .set("PIP_CACHE_DIR", root.join("skein-test-server-pip"));
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh");

    let store = root.join(format!("skein/repos/{tag}/store/data"));
    ensure_store(&store).expect("a managed store, with its repo cache");
    let cache = root.join(format!("boxes/.repo-tools/{tag}"));
    let elsewhere = root.join("elsewhere-cache");
    if linked {
        fs::create_dir_all(&elsewhere).unwrap();
        fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &cache).unwrap();
    }

    let report: String = skein::place::session_cache_names()
        .iter()
        .map(|n| format!("printf 'var {n} %s\\n' \"${{{n}-none}}\"; "))
        .collect();
    let script = with_launch_env(
        &session_script(
            &name,
            "skein-agent",
            &format!(
                "{{ {report} }} > /tmp/s.tmp && mv /tmp/s.tmp /tmp/session.report; exec sleep 300"
            ),
        ),
        "SKEIN_BOX_STORE",
        &skein::util::sh_quote(store.to_string_lossy().as_ref()),
    );
    let launched = own_sandbox(FLEET)
        .exec(&script, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("start the box: {e}"));
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    let ns_start = sh(&format!(
        "sed -n 's/.*) //p' /proc/{anchor}/stat | cut -d' ' -f20"
    ))
    .parse::<u64>()
    .expect("the anchor's start time");
    record_place(
        &name,
        &PlaceRecord {
            sandbox: FLEET.into(),
            ns_pid: anchor,
            home: sandbox_home.display().to_string(),
            tree: "/".into(),
            sock: box_sock(&name),
            generation: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .expect("a boot id")
                .trim()
                .to_string(),
            ns_start,
            ..Default::default()
        },
    )
    .expect("place the box");

    // A crate with no dependencies, in the box's private /tmp: resolving it offline still takes
    // cargo's download lock, the file `.package-cache` at the top of `CARGO_HOME`. `npm cache
    // verify` makes `_cacache` in npm's cache. Each is a write into the cache the tool was pointed
    // at, and neither needs the network.
    let krate = PathBuf::from(format!("{}/tmp/k", box_root(&name)));
    fs::create_dir_all(krate.join("src")).unwrap();
    fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"skein-test-k\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [workspace]\n",
    )
    .unwrap();
    fs::write(krate.join("src/main.rs"), "fn main() {}\n").unwrap();
    let crossed = place_of(&name).expect("placed").exec(
        &format!(
            "{report}\
             RUSTC={rustc} {cargo} generate-lockfile --offline --manifest-path /tmp/k/Cargo.toml \
               > /tmp/k/cargo.log 2>&1; printf 'cargo %s\\n' \"$?\"; \
             if command -v npm >/dev/null 2>&1; then npm cache verify > /tmp/k/npm.log 2>&1; \
               printf 'npm %s\\n' \"$?\"; else echo 'npm absent'; fi",
            rustc = skein::util::sh_quote(real_rustc.to_string_lossy().as_ref()),
            cargo = skein::util::sh_quote(real_cargo.to_string_lossy().as_ref()),
        ),
        Duration::from_secs(120),
    );
    let session_path = PathBuf::from(format!("{}/tmp/session.report", box_root(&name)));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !session_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let session = fs::read_to_string(&session_path).unwrap_or_default();
    let limits =
        fs::read_to_string(format!("{}/limits.state", box_root(&name))).unwrap_or_default();
    forget_place(&name);
    let _ = Command::new("tmux")
        .args(["-S", &box_sock(&name), "kill-server"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    anchor_gone(anchor);
    if limits.starts_with("capped ") {
        let _ = Command::new("sudo")
            .args(["-n", "rmdir", &format!("/sys/fs/cgroup/skein/{name}")])
            .status();
    }
    CacheCrossing {
        session,
        crossed: crossed.unwrap_or_else(|e| panic!("the crossing into the box did not run: {e}")),
        cache,
        seen_at: format!("{}/.repo-tools", sandbox_home.display()),
        elsewhere,
        _root: root,
    }
}

/// The value a `<key> <value>` report gives `key`, or the empty string.
fn field_of(report: &str, key: &str) -> String {
    report
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key} ")))
        .unwrap_or("")
        .to_string()
}

/// **A crossing into a box of a managed-store repo points cargo, npm, pip, uv and pnpm at the
/// repo's tools area, with its session's values, and a cache write through it lands there**
/// (SKEIN-1354, SKEIN-1431).
///
/// The cache was `repos/<id>/store/cache` on the host when this was written; it is the repo's
/// tools area on the fleet's disk now, which the box sees at `~/.repo-tools`, and pnpm's store is
/// the box's own beside its checkout. SKEIN-1350 pointed each tool at it in the box's session only. A command
/// skein runs into the box through a crossing (provisioning, a model call, the attach shell) fell
/// back to the box's own `~/.cargo` and `~/.npm`. This is that crossing, for real, into a real box:
/// every name the session block exports for the cache is compared between the box's own agent and
/// the crossing, and a real `cargo` and `npm` run through the crossing must write into the repo
/// cache.
///
/// What would make it fail: dropping `session_cache_names` from `as_the_session_holds` (the
/// crossing then carries none of the names, and cargo writes into the box's own home); a crossing
/// that kept the names from skein-server's environment rather than taking the session's, which
/// carries `skein-test-server-cargo`. Each was run and failed this test. The launcher no longer
/// binding the cache back or exporting the names fails the session line, before the crossing's.
#[test]
fn a_crossing_into_a_box_uses_its_repos_download_cache() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") || !PathBuf::from(env!("CARGO")).is_file() {
        return skip(
            "this machine cannot make a bwrap namespace, lacks tmux, or has no cargo binary at \
             the path that built this test, so it cannot run a cache write in a box",
        );
    }
    let got = cross_into_a_box_of_a_managed_store("crosscache", false);
    let area = &got.seen_at;
    let pnpm_store = "/crosscache-main/pnpm-store";
    for name in skein::place::session_cache_names() {
        let key = format!("var {name}");
        let session = field_of(&got.session, &key);
        assert!(
            session == "copy"
                || session == *area
                || session.starts_with(&format!("{area}/"))
                || session.ends_with(pnpm_store),
            "the box's session does not point {name} at its repo's tools area {area}, so the \
             crossing below has nothing to agree with: {:?}",
            got.session
        );
        assert_eq!(
            field_of(&got.crossed, &key),
            session,
            "a crossing into the box does not carry the session's {name}: {:?}",
            got.crossed
        );
    }
    assert_eq!(
        field_of(&got.crossed, "cargo"),
        "0",
        "cargo failed through the crossing: {:?}",
        got.crossed
    );
    assert!(
        got.cache.join("cache/cargo/.package-cache").is_file(),
        "cargo run through a crossing did not take its download lock in the repo's cache, so it \
         used a CARGO_HOME of its own: {:?}",
        got.crossed
    );
    if field_of(&got.crossed, "npm") != "absent" {
        assert!(
            got.cache.join("cache/npm/_cacache").is_dir(),
            "npm run through a crossing did not write into the repo's cache: {:?}",
            got.crossed
        );
    }
}

/// **A crossing into a box the launcher gave no repo cache carries none of the cache names, so the
/// box keeps its own `~/.cargo` and `~/.npm`** (SKEIN-1354).
///
/// The store has the managed shape and the repo's tools area is there, but as a link, which the
/// launcher refuses (it binds only a descriptor that leads to the area's own path, SKEIN-1431).
/// That stands for every box with no area: a store kept elsewhere with `skein add --store` names no
/// repo, and leaves `$repo_tools` empty by the same block, before anything is exported. skein-server's own `CARGO_HOME`, `npm_config_cache`
/// and `PIP_CACHE_DIR` are set in this environment, so a crossing that let them through is seen.
///
/// What would make it fail: a crossing that keeps the cache names from skein-server's environment
/// (on `keep_only_listed`'s list) rather than taking them from the session, which carries
/// `skein-test-server-cargo`; and the cache names put on the launcher's `inherited_env`, the
/// obvious way to carry them, which hands the session and every crossing skein-server's own
/// caches. Each was run and failed this test. (Dropping the launcher's link check fails it too:
/// the box is then given the area through the link, and its names point into `elsewhere`.)
#[test]
fn a_crossing_into_a_box_with_no_repo_cache_carries_none_of_its_names() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") || !PathBuf::from(env!("CARGO")).is_file() {
        return skip(
            "this machine cannot make a bwrap namespace, lacks tmux, or has no cargo binary at \
             the path that built this test, so it cannot run a cache write in a box",
        );
    }
    let got = cross_into_a_box_of_a_managed_store("crossnocache", true);
    for name in skein::place::session_cache_names() {
        let key = format!("var {name}");
        if name == "NPM_CONFIG_PREFIX" {
            // The sandbox's own, which every box inherits (`inherited_env`) and a box with no area
            // keeps: the prefix the agent CLIs are in. Never one in an area.
            let (session, crossed) = (field_of(&got.session, &key), field_of(&got.crossed, &key));
            assert!(
                session == crossed && !session.contains("/.repo-tools"),
                "a box with no area has npm's global prefix in an area, or its crossing disagrees: \
                 session {session:?}, crossing {crossed:?}"
            );
            continue;
        }
        assert_eq!(
            (
                field_of(&got.session, &key).as_str(),
                field_of(&got.crossed, &key).as_str()
            ),
            ("none", "none"),
            "a box with no repo cache has {name} set in its session or in a crossing into it: \
             session {:?}, crossing {:?}",
            got.session,
            got.crossed
        );
    }
    assert_eq!(
        field_of(&got.crossed, "cargo"),
        "0",
        "cargo failed through the crossing, so the empty cache below says nothing: {:?}",
        got.crossed
    );
    assert_eq!(
        fs::read_dir(&got.elsewhere).unwrap().count(),
        0,
        "a cache write through a crossing went through the refused link into {}",
        got.elsewhere.display()
    );
}

/// **A crossing into a box carries every tool variable its session holds, so its `cargo` reaches
/// the fleet's toolchain** (SKEIN-1371).
///
/// The session block exports `RUSTUP_HOME`, `CARGO_INSTALL_ROOT`, the Playwright and auto-update
/// names and the repo cache's, and a crossing used to carry only the cache's (SKEIN-1354). So a
/// crossing's `cargo` reached the rustup proxy in `~/.cargo/bin` with no `RUSTUP_HOME`, and a box
/// has no `~/.rustup`. This is a real box over a fixture toolchain laid out as `bootstrap.sh` lays
/// out the fleet's, its `cargo` a stand-in for rustup's proxy: it refuses without a `RUSTUP_HOME`
/// that is a directory, and otherwise runs the real `cargo` building this test. Every name
/// `place::session_exports` reads is compared between the box's own agent and a crossing made with
/// `Place::exec`. `PATH` the crossing builds itself rather than carrying, so it is compared with the
/// PATH the session block exported, which the agent's login profile may add to.
///
/// This process stands for skein-server and carries a `RUSTUP_HOME`, `CARGO_INSTALL_ROOT` and
/// `PLAYWRIGHT_BROWSERS_PATH` of its own, none of them this box's, so a crossing that kept the
/// server's is seen.
///
/// What would make it fail: dropping `session_exports` from `as_the_session_holds` (the crossing
/// then carries none of the tool names, and its `cargo` refuses); a `session_exports` that reads only
/// the repo-cache `if`, which is SKEIN-1354's; `place::box_path` drifting from the launcher's
/// `box_path` (the PATH line). The launcher no longer exporting a name fails the session line,
/// before the crossing's.
#[test]
fn a_crossing_into_a_box_carries_its_sessions_tool_variables() {
    use std::os::unix::fs::PermissionsExt;
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    let real_cargo = PathBuf::from(env!("CARGO"));
    let real_rustc = real_cargo.with_file_name("rustc");
    if !bwrap_works() || !have("tmux") || !real_rustc.is_file() {
        return skip(
            "this machine cannot make a bwrap namespace, lacks tmux, or has no rustc beside the \
             cargo running this test, so it cannot run cargo in a box",
        );
    }
    let name = "crosstool-main";
    let root = scratch_named("crosstool");
    let sandbox_home = sandbox_home_with_agent(&root);
    let mut pins = env_pins();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .set("RUSTUP_HOME", root.join("skein-test-server-rustup"))
        .set("CARGO_INSTALL_ROOT", root.join("skein-test-server-install"))
        .set(
            "PLAYWRIGHT_BROWSERS_PATH",
            root.join("skein-test-server-browsers"),
        );
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh");

    let toolchain = root.join("boxes/.skein/toolchain");
    let bin = toolchain.join("cargo/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(toolchain.join("rustup")).unwrap();
    fs::write(
        bin.join("cargo"),
        format!(
            "#!/bin/sh\n[ -d \"${{RUSTUP_HOME-}}\" ] || \
             {{ echo 'skein-test-proxy: no RUSTUP_HOME' >&2; exit 1; }}\n\
             RUSTC='{}' exec '{}' \"$@\"\n",
            real_rustc.display(),
            real_cargo.display()
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();

    let report: String = skein::place::session_exports()
        .iter()
        .map(|n| format!("printf 'var {n} %s\\n' \"${{{n}-none}}\"; "))
        .chain(["printf 'path %s\\n' \"$PATH\"; ".to_string()])
        .collect();
    let script = session_script(
        name,
        "skein-agent",
        &format!(
            "{{ {report} }} > /tmp/s.tmp && mv /tmp/s.tmp /tmp/session.report; exec sleep 300"
        ),
    );
    let launched = own_sandbox(FLEET)
        .exec(&script, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("start the box: {e}"));
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
    let crossed = place_of(name).expect("placed").exec(
        &format!(
            "{report}\
             v=$(cargo --version 2>&1); printf 'cargo %s\\n' \"$?\"; printf 'says %s\\n' \"$v\""
        ),
        Duration::from_secs(60),
    );
    let session_path = PathBuf::from(format!("{}/tmp/session.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !session_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let session = fs::read_to_string(&session_path).unwrap_or_default();
    // The PATH the launcher's session block exported, read from the tmux server it started: that
    // process inherits it before anything in the box sources a profile.
    let launched_path = fs::read(format!("/proc/{anchor}/environ"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .find_map(|kv| {
            kv.strip_prefix(b"PATH=")
                .map(|v| String::from_utf8_lossy(v).into_owned())
        })
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
    let crossed = crossed.unwrap_or_else(|e| panic!("the crossing into the box did not run: {e}"));

    assert_eq!(
        field_of(&session, "var RUSTUP_HOME"),
        toolchain.join("rustup").display().to_string(),
        "the box's session does not hold the fleet's RUSTUP_HOME, so the crossing below has \
         nothing to agree with: {session:?}"
    );
    // The ones this box's session must hold, so the comparison below is about values and not
    // only about `none` on both sides. `RUSTUP_AUTO_INSTALL` is the depot's toolchain's, and the
    // cache's names are a managed store's, and this box has neither.
    for tool in [
        "CARGO_INSTALL_ROOT",
        "DISABLE_AUTOUPDATER",
        "DISABLE_UPDATES",
        "PLAYWRIGHT_BROWSERS_PATH",
        "PLAYWRIGHT_SKIP_BROWSER_GC",
    ] {
        assert_ne!(
            field_of(&session, &format!("var {tool}")),
            "none",
            "the box's session does not hold {tool}: {session:?}"
        );
    }
    for tool in skein::place::session_exports() {
        let key = format!("var {tool}");
        assert_eq!(
            field_of(&crossed, &key),
            field_of(&session, &key),
            "a crossing into the box does not carry the session's {tool}: {crossed:?}"
        );
    }
    // Compared with the PATH the launcher exported, not the agent's: the agent's `bash -lc` sources
    // the profile again, and a substrate's profile may add to it (Ubuntu's appends `/snap/bin`). A
    // crossing never takes PATH from the session, so the launcher's export is what it must match.
    assert!(
        field_of(&session, "path").contains(&launched_path) && !launched_path.is_empty(),
        "the box's tmux server holds no PATH, or one its agent does not run on: \
         {launched_path:?} against {session:?}"
    );
    assert_eq!(
        field_of(&crossed, "path"),
        launched_path,
        "a crossing into the box runs on a PATH other than the one its session block exported"
    );
    assert_eq!(
        (
            field_of(&crossed, "cargo"),
            field_of(&crossed, "says").starts_with("cargo ")
        ),
        ("0".to_string(), true),
        "`cargo --version` failed through a crossing into the box: {crossed:?}"
    );
}

/// A report of `<key> <value>` lines fit to print in a failure message: a value that is one of
/// this file's own `skein-test-` sentinels, or `none`, is shown; anything else is a credential the
/// machine supplied, and only its length is (SKEIN-1342).
fn redacted(report: &str) -> String {
    report
        .lines()
        .map(|line| {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            if value.is_empty() || value == "none" || value.starts_with("skein-test-") {
                line.to_string()
            } else {
                format!("{key} <some other credential, {} chars>", value.len())
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}
