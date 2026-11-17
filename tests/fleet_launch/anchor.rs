//! The pid skein records as a box's address, and what the box can do about which one it is
//! (SKEIN-1492).

use super::*;
use skein::fleet::stamp_anchor;

/// End a box this file started, and wait for its anchor to be gone.
fn end_box(name: &str, anchor: Option<u32>) {
    let limits = fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
    let _ = Command::new("tmux")
        .args(["-S", &box_sock(name), "kill-server"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if let Some(anchor) = anchor {
        anchor_gone(anchor);
    }
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
}

/// The pid of the tmux server behind a box's socket, asked from outside the box.
fn server_pid(name: &str) -> Option<u32> {
    let out = Command::new("tmux")
        .args(["-S", &box_sock(name), "display", "-p", "#{pid}"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn wait_for(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    path.exists()
}

/// **Nothing a box's own login profile prints decides the pid skein records for it, and a pid
/// that is not the box's own is refused whoever printed it** (SKEIN-1492).
///
/// The launcher reports a box's anchor, the pid of its tmux server, on stdout from a shell inside
/// the box's namespace. That shell was `bash -lc`, so it sourced `~/.bash_profile` out of the
/// box's own home first, and `anchor_from_launch` takes the last marked line. Boxes share one PID
/// namespace, so a profile could print a sibling's tmux server pid, and skein then entered, wrote
/// credentials into and, on stop, killed the sibling as "this box".
///
/// Two real boxes: `victim`, started plainly, and `hostile`, whose profile prints the victim's
/// anchor under the launcher's own marker, first and, through a trap on `EXIT`, last. Then the
/// same with a profile that prints it and exits.
///
/// What would make each assertion fail:
///   * the profile's own marker file: nothing in skein. It shows the profile ran, in the pane's
///     login shell, so the absences below are about a profile that did run.
///   * the anchor read off the launch: spelling the launcher's session block `bash -lc` again.
///     The profile's trap then prints last, and the pid read is the victim's.
///   * the same after the profile that exits: the same change. That block is then ended by the
///     profile with status 0, and the profile's line is the only marked one.
///   * `stamp_anchor` on the victim's pid: dropping the verdict check from `stamp_anchor`, or the
///     `/tmp` comparison from `anchor_belongs_probe`.
///   * `stamp_anchor` on this process's pid, by its message: dropping the mount-namespace
///     comparison from `anchor_belongs_probe`, which leaves the `/tmp` one to refuse it as a
///     process of some other box.
///   * the same with a link at the hostile box's `tmp`: a `stat -L` on the box's side of the
///     comparison, with the `-L` test beside it gone. The link leads to the victim's `tmp`, and
///     the victim's server then compares equal.
///   * `stamp_anchor` on the box's own server: a check that refuses everything.
#[test]
fn a_box_cannot_choose_the_pid_skein_records_as_its_address() {
    let _env = env_lock();
    // The real crossing is the subject: `Place::exec` runs the launcher and the fleet-scope check
    // exactly as `skein-server` does.
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so it cannot host a box",
        );
    }
    // Their own names, so their cgroups cannot be another test's.
    let victim = "anchor-victim";
    let hostile = "anchor-hostile";
    let root = scratch_named("anchor");
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
    let fleet = own_sandbox(FLEET);

    let launched = fleet
        .exec(
            &session_script(victim, "skein-agent", "exec sleep 300"),
            Duration::from_secs(60),
        )
        .expect("start the victim box");
    let theirs = anchor_from_launch(&launched).expect("the victim's launcher reports its anchor");

    // ---- a profile that prints the victim's pid first and last ----
    let hostile_root = PathBuf::from(box_root(hostile));
    let profile = hostile_root.join("home/.bash_profile");
    fs::create_dir_all(profile.parent().unwrap()).unwrap();
    fs::write(
        &profile,
        format!(
            "printf 'SKEIN_ANCHOR {theirs}\\n'\n\
             trap 'printf \"SKEIN_ANCHOR {theirs}\\n\"' EXIT\n\
             : > /tmp/profile-one.ran\n"
        ),
    )
    .unwrap();
    let launched = fleet
        .exec(
            &session_script(hostile, "skein-agent", "sleep 300"),
            Duration::from_secs(60),
        )
        .expect("start the hostile box");
    let said = anchor_from_launch(&launched);
    let ran = wait_for(&hostile_root.join("tmp/profile-one.ran"));
    let own = server_pid(hostile);

    // What `stamp_anchor` makes of each pid the channel could have carried, while both boxes are
    // up. Collected first and asserted after the boxes are ended.
    let on_own = own.map(|pid| stamp_anchor(FLEET, hostile, pid));
    let on_theirs = stamp_anchor(FLEET, hostile, theirs);
    let on_skein = stamp_anchor(FLEET, hostile, std::process::id());
    // A link at the name skein compares against, leading to the victim's `tmp`. The box's own
    // `tmp` is bound by descriptor, so the box is undisturbed, and it is put back afterwards.
    let tmp = hostile_root.join("tmp");
    let aside = hostile_root.join("tmp.aside");
    fs::rename(&tmp, &aside).unwrap();
    std::os::unix::fs::symlink(format!("{}/tmp", box_root(victim)), &tmp).unwrap();
    let on_theirs_by_link = stamp_anchor(FLEET, hostile, theirs);
    fs::remove_file(&tmp).unwrap();
    fs::rename(&aside, &tmp).unwrap();
    end_box(hostile, own);

    // ---- a profile that prints the victim's pid and exits ----
    fs::write(
        &profile,
        format!("printf 'SKEIN_ANCHOR {theirs}\\n'\n: > /tmp/profile-two.ran\nexit 0\n"),
    )
    .unwrap();
    let relaunched = fleet.exec(
        &session_script(hostile, "skein-agent", "sleep 300"),
        Duration::from_secs(60),
    );
    let ran_again = wait_for(&hostile_root.join("tmp/profile-two.ran"));
    let said_again = relaunched.as_deref().ok().map(anchor_from_launch);
    end_box(hostile, server_pid(hostile));
    end_box(victim, Some(theirs));

    assert!(
        ran,
        "the hostile box's profile never ran, so nothing below is about a profile that prints"
    );
    assert!(
        ran_again,
        "the hostile box's second profile never ran, so nothing below is about it"
    );
    // Collected and reported together, so one run says which of them hold: the launcher's shell
    // and skein's check are two mechanisms, and a failure of the first must not hide the second.
    let mut wrong: Vec<String> = Vec::new();
    if said.as_ref().ok() == Some(&theirs) {
        wrong.push(format!(
            "PROFILE: a box's own `~/.bash_profile` printed a sibling's tmux server pid under \
             the launcher's marker and skein read it as this box's anchor. The launcher said: \
             {launched}"
        ));
    }
    if said.as_ref().ok().copied() != own || own.is_none() {
        wrong.push(format!(
            "OWN SERVER: the anchor read off the launch ({said:?}) is not the pid of the box's \
             own tmux server ({own:?}). The launcher said: {launched}"
        ));
    }
    // The profile ends the pane's login shell, so the box may not start at all, and then there is
    // nothing to record. What must not happen is a start that names the victim.
    if said_again.as_ref().and_then(|said| said.as_ref().ok()) == Some(&theirs) {
        wrong.push(format!(
            "PROFILE THAT EXITS: a profile that printed a sibling's pid and exited was read as \
             this box's anchor: {relaunched:?}"
        ));
    }
    if !matches!(on_own, Some(Ok(_))) {
        wrong.push(format!(
            "OWN STAMP: skein refused the box's own tmux server as its anchor, so no box could \
             be recorded at all: {on_own:?}"
        ));
    }
    match &on_theirs {
        Ok(_) => wrong.push(
            "SIBLING: skein stamped a sibling box's tmux server as this box's anchor: every \
             crossing into this box would enter the sibling, and stopping this box would kill it"
                .into(),
        ),
        Err(refused)
            if !(refused.contains("not in this box")
                && refused.contains("skein restart anchor-hostile")) =>
        {
            wrong.push(format!(
                "SIBLING: the refusal does not say what was wrong and what to do: {refused}"
            ))
        }
        Err(_) => {}
    }
    match &on_skein {
        Ok(_) => wrong.push(
            "FLEET SCOPE: skein stamped a process outside every box as a box's anchor: a \
             crossing into that box would run at fleet scope"
                .into(),
        ),
        Err(refused) if !refused.contains("outside every box") => wrong.push(format!(
            "FLEET SCOPE: a process in the fleet's own mount namespace was refused as something \
             else: {refused}"
        )),
        Err(_) => {}
    }
    if on_theirs_by_link.is_ok() {
        wrong.push(
            "LINK: a link the box left at its own `tmp`, leading to a sibling's, made the \
             sibling's tmux server read as this box's"
                .into(),
        );
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    drop(pins);
}
