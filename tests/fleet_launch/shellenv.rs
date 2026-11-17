//! The file the sandbox has every bash source, and what a box, and skein at fleet scope, do with
//! it (SKEIN-1491).

use super::*;

/// **A box cannot write the file the sandbox has every bash source, does not run what is in it,
/// and neither does a shell skein starts at fleet scope** (SKEIN-1491).
///
/// In the fleet sandbox `BASH_ENV=/etc/sandbox-persistent.sh`, and `/etc/profile.d` sources the
/// same file for every login shell. It is owned by the uid every box runs as and nothing covered
/// it, so a line one box appended ran in every later bash of every box, and, because
/// `skein-server` inherits `BASH_ENV` too, in every `bash -c` skein started outside the boxes.
///
/// The file here is a stand-in the test owns, named to the launcher through
/// `$SKEIN_SHELL_ENV_FILE`: the real one is the live sandbox's and must not be written. It sits in
/// the box's own root, which is the one place under this fixture a box can see and write, so
/// without the cover the write below lands. The line planted in it exports
/// `SKEIN_TEST_PLANTED=ran`.
///
/// What would make each assertion fail:
///   * the plain bash: nothing. It is the presence the three absences below are measured against,
///     and fails only if a bash with `BASH_ENV` set no longer sources the file.
///   * fleet scope: taking `-u BASH_ENV` out of `Place::path_pin`.
///   * `sourced`: taking the `--ro-bind-data` cover of `shell_env_file` out of the launcher. A
///     bash in the box that names the file itself then sources the planted line.
///   * `write`, and the file read back from outside: the same.
///   * the box's tmux server: putting `BASH_ENV` (or `ENV`) back on `inherited_env`.
#[test]
fn a_box_can_neither_write_nor_run_the_sandboxs_shell_environment_file() {
    let _env = env_lock();
    // The real crossing is the subject: `Place::exec` spawns the fleet-scope shell and the
    // launcher exactly as `skein-server` does.
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so it cannot host a box",
        );
    }
    // Its own name, so its cgroup cannot be another test's, in this binary or in another run.
    let name = "shellenv-main";
    let root = scratch_named("shellenv");
    let sandbox_home = sandbox_home_with_agent(&root);

    let mut pins = env_pins();
    pins.set("HOME", &sandbox_home)
        .set("SKEIN_HOME", root.join("skein"))
        .set("SKEIN_FLEET_ROOT", root.join("boxes"))
        .set("SANDBOX_NAME", "skein-test-allowed");
    // Only now that the fleet root is pinned does `box_root` name a path under the fixture.
    let planted = PathBuf::from(box_root(name)).join("sandbox-persistent.sh");
    fs::create_dir_all(planted.parent().unwrap()).unwrap();
    let line = "export SKEIN_TEST_PLANTED=ran\n";
    fs::write(&planted, line).unwrap();
    // The sandbox's own settings, as `skein-server` inherits them: the name of the file in
    // `BASH_ENV`, and in `ENV` for a shell in POSIX mode.
    pins.set("SKEIN_SHELL_ENV_FILE", &planted)
        .set("BASH_ENV", &planted)
        .set("ENV", &planted);
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        ..Config::default()
    })
    .expect("configure the fleet this test is standing in");
    install_launcher(FLEET).expect("install box-session.sh");

    let asks =
        "printf '%s|%s|%s' \"${SKEIN_TEST_PLANTED-unset}\" \"${BASH_ENV-unset}\" \"${ENV-unset}\"";

    // The presence: a bash started from this environment with nothing in between runs the line.
    let plain = Command::new("bash")
        .args(["-c", asks])
        .output()
        .expect("bash");
    let plain = String::from_utf8_lossy(&plain.stdout).to_string();
    assert!(
        plain.starts_with("ran|"),
        "a bash with BASH_ENV naming the planted file did not source it, so nothing below shows \
         that skein or the launcher kept it from running: {plain}"
    );

    // Fleet scope: the shell skein starts outside every box.
    let fleet_scope = own_sandbox(FLEET)
        .exec(asks, Duration::from_secs(30))
        .expect("a fleet-scope command");
    // Every absence is collected and reported together, so one run says which of them hold: they
    // are four mechanisms, and a failure of the first must not hide the state of the rest.
    let mut wrong: Vec<String> = Vec::new();
    if fleet_scope.trim() != "unset|unset|unset" {
        wrong.push(format!(
            "FLEET SCOPE: a shell skein started outside every box sourced the file BASH_ENV \
             names, or still carries BASH_ENV or ENV; any box can write that file: {fleet_scope}"
        ));
    }

    // The box. `BASH_ENV` is exported in front of the launcher, which is how the launcher met it
    // before `Place::path_pin` removed it, so the list is still what keeps it from the box.
    let file = skein::util::sh_quote(planted.to_string_lossy().as_ref());
    let agent = format!(
        "{{ printf 'sourced %s\\n' \"$(BASH_ENV={file} bash -c 'printf %s \"${{SKEIN_TEST_PLANTED-unset}}\"')\"; \
         if printf 'export SKEIN_TEST_PLANTED=by-the-box\\n' >> {file}; then echo 'write landed'; \
         else echo 'write refused'; fi 2>/dev/null; \
         printf 'size %s\\n' \"$(wc -c < {file})\"; }} > /tmp/shellenv.tmp \
         && mv /tmp/shellenv.tmp /tmp/shellenv.report; exec sleep 300"
    );
    let launched = own_sandbox(FLEET)
        .exec(
            &format!(
                "export BASH_ENV={file} ENV={file}; {}",
                session_script(name, "skein-agent", &agent)
            ),
            Duration::from_secs(60),
        )
        .expect("start the box");
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");

    let report_path = PathBuf::from(format!("{}/tmp/shellenv.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !report_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let report = fs::read_to_string(&report_path).unwrap_or_default();
    // The environment of the box's tmux server, which every window and pane in the box is made
    // from. Not the pane's own: the pane is a login shell, and on a sandbox whose
    // `/etc/profile.d` exports `BASH_ENV` it has the name again, now naming the covered file.
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
            .args([
                "-n",
                "rmdir",
                &format!("/sys/fs/cgroup/skein/{name}/box"),
                &format!("/sys/fs/cgroup/skein/{name}"),
            ])
            .status();
    }

    assert!(
        !report.is_empty(),
        "the box's agent never wrote its report, so nothing below would be about a box"
    );
    let said = |key: &str| report.lines().any(|l| l == key);
    if !said("sourced unset") {
        wrong.push(format!(
            "SOURCED: a bash in a box that names the sandbox's shell-environment file ran what \
             is in it, so a line any box or the sandbox put there runs in this one: {report}"
        ));
    }
    if !said("write refused") {
        wrong.push(format!(
            "WRITE: a box wrote the sandbox's shell-environment file, which every bash in every \
             box sources: {report}"
        ));
    }
    let now = fs::read_to_string(&planted).unwrap_or_default();
    if now != line {
        wrong.push(format!(
            "CONTENT: the file is not what was planted, so the box's write reached it: {now:?}"
        ));
    }
    if !said("size 0") {
        wrong.push(format!(
            "EMPTY: what a box has at that path is not the empty file the launcher puts there: \
             {report}"
        ));
    }
    assert!(
        server_env.iter().any(|n| n == "SANDBOX_NAME"),
        "the box's tmux server environment could not be read, so the absence below proves \
         nothing: {server_env:?}"
    );
    for name in ["BASH_ENV", "ENV"] {
        if server_env.iter().any(|n| n == name) {
            wrong.push(format!(
                "INHERITED: a box's session inherited {name} from the sandbox, so every \
                 non-interactive shell in it sources a file the launcher did not choose"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    drop(pins);
}
