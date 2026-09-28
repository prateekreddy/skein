//! What a box and a crossing into it RUN: the fixed PATH a fleet-scope script and a crossing
//! resolve against, a planted binary that is not what either runs, and the box's own PATH that
//! its session and a crossing agree on.

use super::*;

/// A binary a box plants in the shared `~/.local/bin` is not what a fleet-scope script runs
/// (ISO-1).
///
/// Every fleet-scope script skein sends went through `bash -lc`, and a login shell on this
/// substrate builds a PATH beginning `~/.local/bin:/usr/local/share/npm-global/bin:…`. `.local` is
/// bound read-WRITE into every box — deliberately, since it holds 547 MB of toolchain that eleven
/// boxes share rather than each paying for — and every box is uid 1000. So a box that copied a
/// file to `~/.local/bin/sudo` had it executed OUTSIDE its own namespace, where the real `sudo`
/// works and the fleet's credentials are readable. No exploit: a file copy.
///
/// Asserted against `place::Place::exec_argv` for a fleet-scope address, because that is the one
/// place the shape of every fleet-scope command is decided.
///
/// **It used to be asserted against the in-sandbox agent's `_argv`**, which was the only fleet-scope
/// path that closed this — the spawned path beside it still used `-lc`. The agent is deleted
/// (SKEIN-573), so the property moved into the surviving builder and this moved with it. Deleting a
/// transport must not delete what it was carrying, and this is the test that says so.
///
/// **Presence before absence.** The old argv is run first against the same planted binary, and it
/// must execute it. Without that half, a fixture whose plant never worked — a `$PATH` that does not
/// include it, a file that is not executable, a shell that reads no profile — would report the
/// marker absent and pass while proving nothing. This is the shape `tests/isolation_bwrap/` was
/// written to avoid twice over.
#[test]
fn a_planted_binary_is_not_what_a_fleet_scope_script_runs() {
    let dir = Scratch::temp("skein-path");
    let bin = dir.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = dir.join("planted-ran");
    // The plant. `id` because it is a real command a fleet-scope script would run and a box cannot
    // be stopped from naming; the file records that it was chosen and then answers plausibly.
    fs::write(
        bin.join("id"),
        format!(
            "#!/bin/sh\nprintf planted > {}\nexec /usr/bin/id \"$@\"\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(
        bin.join("id"),
        <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();
    // Debian's own `~/.profile`, which is what puts the shared directory at the head of PATH and
    // is seeded into every box by the launcher. Reproduced rather than assumed: the plant is only
    // reachable through a profile, so a test without one would be measuring nothing.
    fs::write(
        dir.join(".profile"),
        "PATH=\"$HOME/.local/bin:$PATH\"\nexport PATH\n",
    )
    .unwrap();

    // Built by the real thing, not copied here: a copy of the argv would keep passing against
    // whatever this test was written from.
    //
    // This used to declare the in-fleet deployment first, because a host-driven skein prefixed
    // `sbx exec <sandbox>` and this machine has no `sbx` to run — the hop was never what the test
    // is about, the shell after it is, and that is the same argv either way. SKEIN-521 deleted the
    // hop along with the deployment that took it, so there is nothing left to declare.
    //
    // The lock stays for `$SKEIN_HOME`, which is process-global and read on every call: `exec_argv`
    // asks the config which sandbox this process stands in, and `config::skein_home` refuses an
    // unpinned test rather than answering with the real one (SKEIN-626).
    let _env = common::env_lock();
    let was_home = std::env::var_os("SKEIN_HOME");
    std::env::set_var("SKEIN_HOME", dir.join("skein-home"));
    let argv_of = |script: &str| -> Vec<String> {
        skein::place::own_sandbox(&skein::place::fleet_sandbox()).exec_argv(script)
    };

    let run = |argv: &[String]| {
        let _ = fs::remove_file(&marker);
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .env("HOME", dir.path())
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("the fleet-scope command to run");
        assert!(status.success(), "the script itself failed: {argv:?}");
        marker.exists()
    };

    // First the argv this replaces, against the same plant. It MUST run it.
    assert!(
        run(&[
            "bash".to_string(),
            "-lc".to_string(),
            "id -u >/dev/null".to_string(),
        ]),
        "the fixture's plant was never executed even by a login shell, so the assertion below \
         would pass whatever the agent does"
    );

    // And now skein's own fleet-scope argv.
    let argv = argv_of("id -u >/dev/null");
    assert!(
        !run(&argv),
        "a fleet-scope command ran a binary out of the shared `~/.local/bin`, outside every box's \
         namespace: {argv:?}"
    );
    // Named rather than inferred from the absence: an argv that failed to run at all would also
    // leave no marker.
    assert!(
        argv.iter().any(|a| a.starts_with("PATH=")),
        "the fleet-scope argv no longer fixes PATH, so nothing decides which binary runs: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "-lc"),
        "a fleet-scope command uses a LOGIN shell again, which re-reads the profile and puts the \
         shared directory back at the head of PATH whatever the argv sets: {argv:?}"
    );

    match was_home {
        Some(v) => std::env::set_var("SKEIN_HOME", v),
        None => std::env::remove_var("SKEIN_HOME"),
    }
}

/// A binary a box plants is not what a crossing **into a box** runs either (ISO-1, SKEIN-832).
///
/// The sibling above covers a script addressed to the sandbox. This covers the other arm, and the
/// hole it closes was open for longer: `Place::shell` pinned PATH for a fleet-scope script and
/// `Place::enter` pinned nothing, so the outer `bash`, the `nsenter`, and the `cat`/`sed`/`cut` the
/// anchor guard spends on `/proc/<ns_pid>/stat` — **all of which run at fleet scope, outside the
/// box, before any hop** — resolved from whatever PATH the spawning process inherited.
///
/// **The reachable case is a person's own shell, not a server's.** `skein attach <box>` builds this
/// argv and `run_attach` spawns it with `Command::new(program)` (`src/bin/skein.rs:1346`), so the
/// PATH is the one the person typing it has — `~/.local/bin` at the head on this substrate, bound
/// read-WRITE into every box, every box uid 1000. That is why this fixture puts the plant on the
/// **spawner's** PATH and needs no `.profile`: unlike the fleet-scope arm, nothing here goes
/// through a login shell to reach the plant. The crossing reads that PATH directly.
///
/// **Presence before absence**, the shape `tests/isolation_bwrap/` exists to keep: the same argv
/// with the pin taken off is run first against the same plant, and it MUST execute it. Without that
/// half, a plant that was never reachable — an anchor the guard refuses, a `$PATH` that does not
/// carry it, a file that is not executable — would report the marker absent and pass about nothing.
///
/// The un-pinned argv is **derived from the real one** rather than spelled here, so it cannot go on
/// passing against a shape this test was written from; the derivation asserts what it is stripping.
///
/// **The anchor is this test's own process.** The guard compares the boot id and
/// `/proc/<ns_pid>/stat`'s start time against the record, and refuses with `exit 78` before it ever
/// reaches `nsenter` if either differs — so an anchor that is not provably alive would leave the
/// plant unreached for a reason that has nothing to do with PATH. Our own pid is alive by
/// construction, and it leaves nothing behind to leak.
///
/// **What would make this fail**: taking `Place::path_pin()` off `Place::enter`'s two arms. The
/// absence assertion fires, naming the planted `nsenter` that ran outside every box's namespace.
#[test]
fn a_planted_nsenter_is_not_what_a_crossing_runs() {
    let dir = Scratch::temp("skein-crossingpath");
    let bin = dir.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = dir.join("planted-ran");

    // The plant. `nsenter` because it is the hop itself — the one program a crossing MUST run at
    // fleet scope, and one a box can name without being stopped. It records that it was chosen and
    // then exits quietly rather than running what follows: this test asks which file was picked,
    // and a stand-in that carried the crossing through would be answering a different question.
    fs::write(
        bin.join("nsenter"),
        format!("#!/bin/sh\nprintf planted > {}\nexit 0\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(
        bin.join("nsenter"),
        <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();

    let _env = common::env_lock();
    let was_home = std::env::var_os("SKEIN_HOME");
    let skein_home = dir.join("skein-home");
    fs::create_dir_all(skein_home.join("places")).unwrap();
    std::env::set_var("SKEIN_HOME", &skein_home);

    // A record the guard can prove, read back through `place_of` rather than built here: the
    // placement reader is part of what decides the argv, and a `Place` assembled in the test would
    // skip it.
    let anchor = std::process::id();
    let stat = fs::read_to_string(format!("/proc/{anchor}/stat")).unwrap_or_default();
    // Cut after the LAST `) `, for the reason `Place::guard` cuts there: `comm` is in parentheses
    // and may hold spaces of its own, so a whitespace field index is right until it is not.
    let ns_start: u64 = stat
        .rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|f| f.parse().ok())
        .unwrap_or(0);
    assert!(
        ns_start > 0,
        "this process's own start time could not be read, so the guard would refuse the crossing \
         and the plant would go unreached for a reason that is not PATH: {stat}"
    );
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    fs::write(
        skein_home.join("places/thing-cross.json"),
        serde_json::json!({
            "sandbox": skein::place::fleet_sandbox(),
            "ns_pid": anchor,
            "home": dir.path().display().to_string(),
            "tree": dir.path().display().to_string(),
            "sock": dir.join("box.sock").display().to_string(),
            "generation": boot.trim(),
            "ns_start": ns_start,
        })
        .to_string(),
    )
    .unwrap();

    let place = skein::place::place_of("thing-cross").expect("the placement record just written");
    let argv = place.exec_argv("id -u >/dev/null");

    // The plant at the HEAD of the spawner's PATH, which is the case that matters: a person's
    // shell, not a pinned one.
    let run = |argv: &[String]| -> bool {
        let _ = fs::remove_file(&marker);
        let _ = Command::new(&argv[0])
            .args(&argv[1..])
            .env("HOME", dir.path())
            .env(
                "PATH",
                format!(
                    "{}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    bin.display()
                ),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .expect("the crossing to run");
        marker.exists()
    };

    // The same crossing with whatever environment prefix it carries taken off — a leading `env`
    // and the `NAME=VALUE` assignments after it.
    //
    // **It tolerates finding none, deliberately.** Stripping a fixed two elements would make the
    // one regression that matters — the pin being dropped — fail on the strip instead of on the
    // assertion written for it, and a reader would be told the argv had the wrong shape rather
    // than that a planted binary ran. With no pin to remove, `unpinned` is the crossing itself,
    // the two halves below run the same argv, and the ABSENCE assertion is the one that fires.
    let assignment = |a: &String| {
        let name = a.split('=').next().unwrap_or_default();
        a.contains('=')
            && !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let unpinned: Vec<String> = if argv[0] == "env" {
        argv.iter()
            .skip(1)
            .skip_while(|a| assignment(a))
            .cloned()
            .collect()
    } else {
        argv.clone()
    };

    // First, the argv this replaces, against the same plant. It MUST run it.
    assert!(
        run(&unpinned),
        "the fixture's plant was never executed even by a crossing with no PATH pin on it, so the \
         assertion below would pass whatever `Place::enter` builds: {unpinned:?}"
    );

    // And now skein's own crossing.
    assert!(
        !run(&argv),
        "a crossing into a box ran a planted `nsenter` from the spawner's PATH — at fleet scope, \
         outside every box's namespace, before any hop: {argv:?}"
    );
    // Named rather than inferred from the absence: an argv that failed to start at all would also
    // leave no marker, and a pin that named the planted directory would decide nothing.
    assert_eq!(
        argv[0], "env",
        "a crossing no longer begins with a PATH pin: {argv:?}"
    );
    assert!(
        argv[1].starts_with("PATH=") && !argv[1].contains(&bin.display().to_string()),
        "a crossing's pin is not a PATH, or it carries the box-writable directory itself: {}",
        argv[1]
    );

    match was_home {
        Some(v) => std::env::set_var("SKEIN_HOME", v),
        None => std::env::remove_var("SKEIN_HOME"),
    }
}

// ---------------------------------------------------------------------------------------------
// The PATH a box's OWN agent session runs on (SKEIN-851)
// ---------------------------------------------------------------------------------------------

/// The three statements the launcher decides PATH with, lifted out of the script.
///
/// Read rather than copied, for the reason [`isolation_block`] is: a copy keeps passing against the
/// version the test was written from. Each is taken by the prefix of its assignment at column 0 —
/// `export PATH=` (the fixed, root-owned six every fleet-scope command resolves against),
/// `box_path=` (the box's own, which is the subject of these two tests) and `tmux_bin=` (resolved
/// against the first of those, by absolute path, so no box can choose the binary whose pid skein
/// then addresses it by).
///
/// **Exactly one of each, and `box_path=` after `export PATH=`**, because `box_path` is written as
/// `…:$PATH` — it derives its tail from the line above it rather than repeating the six, so the
/// order is load-bearing and not cosmetic. It **refuses to run** rather than returning a short
/// list: a landmark that moved and a launcher that stopped deciding PATH are indistinguishable by
/// a missing line alone, which is the failure mode `unsets_between_blocks` exists to avoid.
fn launcher_path_statements() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let wanted = ["export PATH=", "box_path=", "tmux_bin="];
    let mut picked: Vec<(usize, &str)> = Vec::new();
    for want in wanted {
        let found: Vec<(usize, &str)> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with(want))
            .map(|(i, l)| (i, *l))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "box-session.sh has {} statements beginning `{want}` at column 0 and this harness \
             needs exactly one: it splices them into a runner, so two would fight and none means \
             the launcher no longer decides that PATH here",
            found.len()
        );
        picked.push(found[0]);
    }
    assert!(
        picked[0].0 < picked[1].0,
        "`box_path=` (line {}) now comes before `export PATH=` (line {}), so its `$PATH` tail is \
         whatever the launcher inherited rather than the fixed six",
        picked[1].0 + 1,
        picked[0].0 + 1
    );
    picked.sort_by_key(|(i, _)| *i);
    picked
        .iter()
        .map(|(_, l)| *l)
        .collect::<Vec<&str>>()
        .join("\n")
}

/// The launcher's fixed PATH — the value of its one `export PATH=` at column 0, which
/// [`launcher_path_statements`] has already proved is there exactly once.
fn launcher_fixed_path() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let line = src
        .lines()
        .find(|l| l.starts_with("export PATH="))
        .expect("box-session.sh no longer exports a fixed PATH");
    let value = line["export PATH=".len()..].trim().trim_matches('"');
    assert!(
        value.starts_with('/') && !value.contains('$'),
        "the launcher's `export PATH=` is no longer a literal list of directories ({value}), so it \
         is not the fixed PATH this harness compares a session's tail against"
    );
    value.to_string()
}

/// The launcher's final `exec bwrap` — the whole of it, to end of file.
///
/// This is the one block in `box-session.sh` that *starts a box*: the namespace, the login shell
/// inside it, the tmux server that anchors it, and the pane the agent runs in. Lifted rather than
/// reproduced so that a change to any of those is a change to what these tests run.
fn session_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| *l == "exec bwrap \\")
        .expect("the launcher's final `exec bwrap` moved");
    let block = lines[from..].join("\n");
    for landmark in ["bash -lc", "new-session", "\"${pane_cmd[@]}\""] {
        assert!(
            block.contains(landmark),
            "the lifted session block no longer holds `{landmark}`, so this harness is running \
             something other than the thing that starts a box"
        );
    }
    block
}

/// What a box's own agent session resolved, and the PATH it resolved it against.
struct Resolved {
    path: String,
    resolved: String,
}

/// A box being started for real: the launcher's own `exec bwrap` block, with three identically
/// named executables planted where the three PATHs a session could plausibly get would find them.
struct Started {
    dir: Scratch,
    /// The box's private home on disk, bound over `$HOME` inside the namespace exactly as
    /// `binds=(--bind "$home" "$HOME")` does.
    box_home: PathBuf,
    /// The `$HOME` the launcher runs with, and therefore the path the box's home appears at inside.
    /// A fixture directory rather than the runner's real home, so nothing here reads or writes it.
    fixture_home: PathBuf,
    /// Bound over `/usr/local/sbin` — the FIRST entry of the launcher's fixed six, so this is as
    /// early on that PATH as a substrate binary can be.
    substrate_decoy: PathBuf,
    /// Bound over `/etc/profile.d`, holding a script that puts a fourth directory at the head of
    /// PATH. The substrate has no such script (`grep -rn PATH /etc/profile /etc/profile.d/` is
    /// empty here, which is half of why SKEIN-851 existed) — it is planted so that "a login shell
    /// rebuilds PATH from the profile" is a thing this test can make TRUE and then watch lose.
    profile_decoy: PathBuf,
    tmux_bin: String,
}

/// The name of the binary all three copies are called. `skein-test-` prefixed, so it cannot collide
/// with anything real and says what it is in a process listing.
const PROBE_BIN: &str = "skein-test-probe";

impl Started {
    fn make() -> Started {
        // `Scratch::boxes`, not `Scratch::temp`: the session block binds `$tmp` over `/tmp` and the
        // box's home over `$HOME`, so a fixture under either is unreadable from outside it — the
        // launcher refuses such a root outright, and the report this test reads back would vanish.
        let dir = Scratch::boxes("skein-sesspath");
        let root = dir.join("box");
        let box_home = root.join("home");
        let fixture_home = dir.join("fleet-home");
        let substrate_decoy = dir.join("substrate-bin");
        let profile_decoy = dir.join("profile-bin");
        for p in [
            &box_home.join(".local/bin"),
            &fixture_home,
            &root.join("tmp"),
            &root.join("tree"),
            &substrate_decoy,
            &profile_decoy,
            &dir.join("profile-d"),
            &dir.join("skein-home"),
        ] {
            fs::create_dir_all(p).unwrap();
        }
        // Three copies of one name, each saying which one it is. `command -v` alone would answer a
        // path; this also runs the file, so "the box's own wins" is a binary that executed and not
        // just a directory entry that sorted first.
        for (tag, at) in [
            ("box", box_home.join(".local/bin")),
            ("substrate", substrate_decoy.clone()),
            ("profile", profile_decoy.clone()),
        ] {
            let f = at.join(PROBE_BIN);
            fs::write(&f, format!("#!/bin/sh\nprintf %s {tag}\n")).unwrap();
            fs::set_permissions(
                &f,
                <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
            )
            .unwrap();
        }
        fs::write(
            dir.join("profile-d/zz-skein-test.sh"),
            format!("PATH=\"{}:$PATH\"\nexport PATH\n", profile_decoy.display()),
        )
        .unwrap();

        // Resolved the way the launcher resolves it — by running the launcher's own `tmux_bin=`
        // line — so it is the same file a real box start would pick.
        //
        // **This REQUIRES tmux rather than skipping without it, and that is the lesser of two
        // wrongs rather than a preference.** A skip is what this file does everywhere else, and a
        // skip here would be one `common::REQUIREMENTS` does not declare: that list names what each
        // test binary needs and says only `bwrap` and `python3` for this one, and it lives in
        // `tests/common/mod.rs`, which the lane that added this test did not hold.
        // `tests/platform_gates.rs` catches exactly that and fails the build, which is how this was
        // found — so the choice was an undeclared silent skip or a loud failure, and a loud failure
        // is the one a person can act on. SKEIN-866 moves it back to a declared skip.
        //
        // It costs little in practice: `box-session.sh` itself exits 3 when tmux is missing, so a
        // machine that cannot run this cannot run a box either.
        let probe = format!("{}\nprintf %s \"$tmux_bin\"\n", launcher_path_statements());
        let out = Command::new("bash").arg("-c").arg(&probe).output().unwrap();
        let tmux_bin = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(
            !tmux_bin.is_empty(),
            "tmux is not on the fixed PATH box-session.sh resolves against, so no box can be \
             started and the PATH a box's own agent session runs on cannot be checked. Install \
             tmux; `common::REQUIREMENTS` does not yet list it for this binary (SKEIN-866)"
        );

        let sock = root.join("session.sock");
        let killer = tmux_bin.clone();
        let dir = dir.quiesce_with(move |at| {
            // Whatever happened, the tmux server this fixture started goes. A kept directory is the
            // only evidence a failure leaves and is kept on purpose (see `Scratch`); a kept SERVER
            // is a process nobody owns holding a namespace open, which is SKEIN-645's shape.
            let _ = Command::new(&killer)
                .args(["-S"])
                .arg(at.join("box/session.sock"))
                .arg("kill-server")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
        let _ = sock;
        Started {
            dir,
            box_home,
            fixture_home,
            substrate_decoy,
            profile_decoy,
            tmux_bin,
        }
    }

    /// Start one box and ask its agent session what `skein-test-probe` resolves to.
    ///
    /// `block` is the launcher's session block, possibly with its PATH export stripped — see
    /// [`the_agent_session_a_box_starts_runs_on_the_boxs_own_path`] for why that derivation is what
    /// the "before" half is spelled as.
    fn run(&self, block: &str, with_profile: bool) -> Resolved {
        let root = self.dir.join("box");
        let report = root.join("tmp/session-path.report");
        let _ = fs::remove_file(&report);
        let _ = fs::remove_file(root.join("tmp/session-path.part"));
        // Written where the box can write and the test can read: `$tmp` is bound over `/tmp`, so
        // `/tmp/...` inside is `<root>/tmp/...` outside. `PATH <value>` and `resolved <value>`
        // rather than `k=v`, because `answer` reads that shape.
        //
        // **Written to `.part` and RENAMED**, and that is not tidiness. Polling for the report's
        // existence was the first spelling, and the redirect creates the file before the first
        // `printf` runs — so this read a report with `PATH` and `resolved` in it and no `ran` yet,
        // and `answer` panicked about a line the probe was about to write. It passed on the run
        // before and failed under `$SKEIN_TESTS_NO_SKIP` on the next, same code: a race, which is
        // the one kind of harness defect that looks like a flaky subject. A rename is atomic, so
        // the name this waits for cannot exist half-written.
        let pane = format!(
            "{{ printf 'PATH %s\\n' \"$PATH\"\n\
             printf 'resolved %s\\n' \"$(command -v {PROBE_BIN} || echo none)\"\n\
             printf 'ran %s\\n' \"$({PROBE_BIN} 2>/dev/null || echo none)\"\n\
             }} > /tmp/session-path.part\n\
             mv /tmp/session-path.part /tmp/session-path.report\n\
             exec sleep 20\n"
        );
        let mut binds = format!(
            "--bind {} \"$HOME\" --bind {} /usr/local/sbin",
            skein::util::sh_quote(self.box_home.to_string_lossy().as_ref()),
            skein::util::sh_quote(self.substrate_decoy.to_string_lossy().as_ref()),
        );
        if with_profile {
            binds.push_str(&format!(
                " --bind {} /etc/profile.d",
                skein::util::sh_quote(self.dir.join("profile-d").to_string_lossy().as_ref())
            ));
        }
        let runner = format!(
            "set -uo pipefail\n\
             export HOME={home}\n\
             export SKEIN_HOME={skein_home} SKEIN_FLEET_ROOT={fleet_root}\n\
             {paths}\n\
             tmp={tmp}\n\
             tree={tree}\n\
             sock={sock}\n\
             pidfile={pidfile}\n\
             session=skein-test-sesspath\n\
             binds=({binds})\n\
             pane_cmd=(/bin/sh -c {pane} skein-test-pane)\n\
             {block}\n",
            home = skein::util::sh_quote(self.fixture_home.to_string_lossy().as_ref()),
            // Both pinned, in the one place a box start could otherwise reach the live fleet:
            // `$SKEIN_FLEET_ROOT` defaults to `/boxes`, and this runner is a real box start.
            skein_home =
                skein::util::sh_quote(self.dir.join("skein-home").to_string_lossy().as_ref()),
            fleet_root =
                skein::util::sh_quote(self.dir.join("fleet-root").to_string_lossy().as_ref()),
            paths = launcher_path_statements(),
            tmp = skein::util::sh_quote(root.join("tmp").to_string_lossy().as_ref()),
            tree = skein::util::sh_quote(root.join("tree").to_string_lossy().as_ref()),
            sock = skein::util::sh_quote(root.join("session.sock").to_string_lossy().as_ref()),
            pidfile = skein::util::sh_quote(root.join("session.pid").to_string_lossy().as_ref()),
            binds = binds,
            pane = skein::util::sh_quote(&pane),
            block = block,
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(&runner)
            .output()
            .expect("bash");
        assert!(
            out.status.success(),
            "the box did not start: {}\n--- runner ---\n{runner}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The pane writes, renames and then sleeps, so the report is complete or absent. Waited
        // for rather than assumed: `new-session -d` returns as soon as the server has the session,
        // and the pane is a fork of it.
        for _ in 0..200 {
            if report.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let text = fs::read_to_string(&report).unwrap_or_else(|e| {
            panic!(
                "the box's agent session never reported its PATH ({e}); launcher said: {}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
        // Immediately, not at the end: the pane's `sleep` is what holds the session open for the
        // block's own `display -p`, and nothing else needs it.
        let _ = Command::new(&self.tmux_bin)
            .arg("-S")
            .arg(root.join("session.sock"))
            .arg("kill-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let path = answer(&text, "PATH").to_string();
        let resolved = answer(&text, "resolved").to_string();
        // The file that ran and the file that resolved are asked separately and compared, because
        // `command -v` answers about a name and says nothing about whether it is executable.
        let ran = answer(&text, "ran");
        let expected_tag = match () {
            _ if resolved.starts_with(self.box_home.to_string_lossy().as_ref())
                || resolved.starts_with(self.fixture_home.to_string_lossy().as_ref()) =>
            {
                "box"
            }
            _ if resolved.starts_with("/usr/local/sbin") => "substrate",
            _ if resolved.starts_with(self.profile_decoy.to_string_lossy().as_ref()) => "profile",
            _ => "none",
        };
        assert_eq!(
            ran, expected_tag,
            "the session resolved `{PROBE_BIN}` to {resolved} and running it printed {ran}, so the \
             file that answered is not the file that ran and neither half of this test means what \
             it says"
        );
        Resolved { path, resolved }
    }
}

/// **A box's own agent session runs on the BOX's PATH**, so a box runs the agent its fleet installs
/// and shares rather than whichever copy the substrate happens to put first (SKEIN-851).
///
/// This is the `box-session.sh` half of SKEIN-832. That item fixed a *crossing* — skein reaching
/// into a running box — and `Place::wrap` carries the box's own PATH across the hop for it. The
/// launcher was left believing the opposite about its own session: the paragraph above its
/// `export PATH=` said the `bash -lc` under `exec bwrap` "rebuilds PATH from the profile exactly as
/// before", and so a fixed PATH at fleet scope could not reach a box. Measured, every clause of it
/// was false — no `~/.profile` in a box's private home or in the sandbox's, `/etc/profile` and
/// `/etc/profile.d/*` setting no PATH at all, and `export` inherited by that child like any other
/// variable — which left every agent session on the fixed six, with no `~/.local/bin` in it.
///
/// **It was latent, and that is the whole reason it is tested here rather than noticed in
/// production.** The sandbox was carrying an older copy of the launcher with no `export PATH=` line
/// at all, so boxes inherited a working PATH by accident; the next `install_launcher` is what would
/// have exposed it, in a diff pointing at nothing.
///
/// # Three PATHs a session could get, and a presence for every absence
///
/// An absence that was never a presence proves nothing, so each of the two decoys this test says
/// must LOSE is first shown winning, in the same fixture, through the same real `bwrap` and the same
/// real tmux:
///
/// | run | block | `/etc/profile.d` planted | what answers |
/// |---|---|---|---|
/// | 1 | the PATH export **stripped** | no | `/usr/local/sbin` — the first entry of the fixed six |
/// | 2 | the PATH export **stripped** | yes | the profile's directory, ahead of all six |
/// | 3 | **as the launcher is** | yes | the box's own `~/.local/bin` |
///
/// Run 1 is not a contrivance: it is precisely the session a freshly installed launcher would have
/// started before this fix, and what it resolves is the defect. Run 2 makes "a login shell rebuilds
/// PATH from the profile" *true* — it is false on this substrate — so that run 3 proves the export
/// is placed where even a substrate that did have such a profile could not undo it. That is why the
/// export is inside the `bash -lc` and not before the `exec` and not a `--setenv`.
///
/// The stripped block is **derived from the real one** and tolerates finding nothing to strip, for
/// the reason [`a_planted_nsenter_is_not_what_a_crossing_runs`] gives: if the fix is ever removed,
/// runs 1 and 3 become the same command and it is the ABSENCE assertion that fires, naming the
/// substrate binary a box ran, rather than a harness complaining about a shape.
///
/// # What would make this fail — applied to the real files, one at a time
///
/// Every assertion below was broken deliberately and watched to fail before it was believed, and
/// the two entries that did NOT fail where they were predicted to are here because that is the
/// part worth knowing:
///
///   * **Deleting `export PATH="$box_path"`** from the session block — run 3's resolution
///     assertion, naming the **profile** decoy. Not the substrate one, which is what a first draft
///     of this comment claimed: run 3 is the run with the profile planted, and the profile sits
///     ahead of all six.
///   * **Dropping `"$box_path"`** from the positionals — the box does not start at all. `box_path`
///     becomes the pane command's first word, `shift 6` eats it, and tmux is handed a fragment. It
///     fails on the harness's own "the box did not start", not on a PATH assertion, and that is
///     the honest description of it.
///   * **Putting `$HOME/.local/bin` last** in `box_path` — run 3's resolution assertion again
///     (`/usr/local/sbin` wins), and the sibling test.
///   * **Putting `$HOME/.local/bin` second**, behind the npm prefix — this is what the
///     head-of-PATH assertion is for, and nothing else reaches it: with the box's directory merely
///     PRESENT the resolution assertion is satisfied, because no decoy sits in the npm prefix.
///   * **Dropping `/usr/local/share/npm-global/bin`** — the sibling test only; a box still
///     resolves its own copy, which is exactly why the sibling test exists.
///   * **Dropping the `$PATH` tail**, so a box gets no userland — the sibling test fires, but run 3
///     dies earlier and elsewhere: with no fixed six on the session's PATH the pane cannot find
///     `mv`, so no report is written at all.
///   * **Appending a directory after `$box_path`** in the session block's export — the `ends_with`
///     assertion, and only it (SKEIN-1216: `export PATH="$box_path:/skein-test-extra"`). It
///     compares against the launcher's own `export PATH=`, not against run 1, because run 1 reads
///     the host's `/etc/profile.d` and a host may append to PATH there.
///   * **Neutering the strip**, so run 1 is the fixed launcher — run 1's presence assertion, whose
///     message counts the lines it removed (`0`) and so says which of the two it is.
///   * **Taking the profile bind off run 2** — run 2's presence assertion.
///   * **Giving the box's copy an unusable shebang** — the `resolved`/`ran` cross-check in
///     [`Started::run`], which is the only thing standing between `command -v` answering about a
///     name and a binary having actually executed.
#[test]
fn the_agent_session_a_box_starts_runs_on_the_boxs_own_path() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so the PATH a box's own agent session \
             runs on was NOT checked",
        );
    }
    let started = Started::make();
    let block = session_block();
    let stripped: String = block
        .lines()
        .filter(|l| !l.trim_start().starts_with("export PATH="))
        .collect::<Vec<&str>>()
        .join("\n");
    let removed = block.lines().count() - stripped.lines().count();

    // Run 1 — the session as it was before this fix, and the defect itself.
    let before = started.run(&stripped, false);
    assert_eq!(
        before.resolved,
        format!("/usr/local/sbin/{PROBE_BIN}"),
        "with the session block's PATH export stripped ({removed} line(s) removed), a box's agent \
         session did not resolve the binary planted at the head of the launcher's fixed PATH — so \
         nothing below is a demonstration that the fix changed anything. It ran on {}",
        before.path
    );
    assert!(
        !before
            .path
            .contains(&format!("{}/.local/bin", started.fixture_home.display())),
        "the stripped block still put the box's own `~/.local/bin` on the session's PATH, so run 3 \
         would pass whatever the launcher does: {}",
        before.path
    );

    // Run 2 — the same, with a profile that really does rebuild PATH.
    let profiled = started.run(&stripped, true);
    assert_eq!(
        profiled.resolved,
        started
            .profile_decoy
            .join(PROBE_BIN)
            .to_string_lossy()
            .into_owned(),
        "the planted `/etc/profile.d` script was never read, so run 3 cannot show the export \
         beating a profile and the reason it is placed inside the `bash -lc` goes untested. The \
         session ran on {}",
        profiled.path
    );

    // Run 3 — the launcher as it is, against both decoys at once.
    let now = started.run(&block, true);
    let own = started.fixture_home.join(".local/bin").join(PROBE_BIN);
    assert_eq!(
        now.resolved,
        own.to_string_lossy().into_owned(),
        "a box's own agent session resolved `{PROBE_BIN}` to {} instead of the box's own copy — it \
         ran on {}",
        now.resolved,
        now.path
    );
    // Named rather than left to the equality above: a session whose PATH merely CONTAINS the box's
    // directory somewhere behind the substrate's would answer the same way only by luck.
    assert_eq!(
        now.path.split(':').next(),
        Some(
            started
                .fixture_home
                .join(".local/bin")
                .to_string_lossy()
                .as_ref()
        ),
        "the box's own `~/.local/bin` is not at the HEAD of its session's PATH: {}",
        now.path
    );
    // And the fixed six is still behind it, which is what makes a box able to run `sudo`, `git` and
    // `python3` at all. Read out of the launcher's own `export PATH=`, not from run 1: run 1 reads
    // the HOST's `/etc/profile.d`, and a login shell there may append to PATH — Ubuntu's snapd
    // appends `/snap/bin` — while run 3 reads the planted one, so `before.path` is the fixed six
    // plus whatever this host's profile adds, and `ends_with` it was red on the ubuntu-24.04
    // runner for a reason that has nothing to do with the box (SKEIN-1216). Fails if the session
    // block puts anything after `$box_path` in its export.
    let fixed = launcher_fixed_path();
    assert!(
        now.path.ends_with(&format!(":{fixed}")),
        "the box's session PATH no longer ends with the launcher's fixed PATH, so a box has the \
         agent and not the userland: {} does not end with {}",
        now.path,
        fixed
    );
}

/// **A box's own session and a crossing into that box resolve the same PATH** — two producers, one
/// value (SKEIN-851).
///
/// `box-session.sh` derives the session's PATH from `$HOME` and its own fixed six;
/// `Place::wrap` derives a crossing's from the placement record's `home`, which
/// `fleet::sandbox_home` reads out of that same sandbox's `$HOME`. The same three directories in
/// the same order, arrived at twice, in a shell script and in Rust — and nothing makes them agree
/// except this.
///
/// **Why they are not shared instead.** `box-session.sh` is installed into a sandbox as a
/// standalone file and runs with no skein binary in reach, so it cannot read a Rust `const`; and
/// being *handed* the value by whatever spawns it would put it in `fleet::session_script`, which
/// this lane does not hold. So both derive, and the drift is made loud here rather than left to be
/// discovered as a box that resolves one agent when skein enters it and another when it starts
/// itself.
///
/// **This is a string comparison, and it is the one place that is the right shape.** What a session
/// actually resolves is asserted by
/// [`the_agent_session_a_box_starts_runs_on_the_boxs_own_path`], which runs a real box; this asks
/// only whether the two producers still say the same thing, and both sides are *run* rather than
/// quoted — the launcher's statements are evaluated by `bash`, and the crossing's argv is built by
/// `Place`.
///
/// **What would make this fail**: changing either side's order or contents — dropping
/// `/usr/local/share/npm-global/bin` from one, or putting `~/.local/bin` behind the fixed six in
/// one. Both were done, one at a time.
#[test]
fn a_box_session_and_a_crossing_into_it_agree_on_the_boxs_path() {
    let dir = Scratch::temp("skein-sesspath-agree");
    // The sandbox's `$HOME` — the one fact both producers start from. A fixture path rather than
    // the runner's own home, so neither side can be right by reading something real.
    let home = dir.join("fleet-home");
    fs::create_dir_all(&home).unwrap();

    // Side one: the launcher's own statements, evaluated.
    let probe = format!("{}\nprintf %s \"$box_path\"\n", launcher_path_statements());
    // `/bin/bash` absolutely, because the PATH below names a directory that does not exist — a
    // `bash` looked up on it is not found and the test fails on the spawn rather than on what it
    // is about. Proved by writing it the other way first.
    let out = Command::new("/bin/bash")
        .arg("-c")
        .arg(&probe)
        .env("HOME", &home)
        // Deliberately hostile: what the launcher inherits must not reach the value it builds.
        .env("PATH", "/nowhere-inherited")
        .output()
        .expect("bash");
    let session_path = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !session_path.contains("/nowhere-inherited"),
        "the launcher's `box_path` carries the PATH it inherited, so what a box resolves depends on \
         who started it: {session_path}"
    );

    // Side two: the crossing, built by `Place` from a placement record naming the same home.
    let _env = common::env_lock();
    let was_home = std::env::var_os("SKEIN_HOME");
    // **And the fleet root, because pinning one of the pair and not the other is what
    // `fleet-pin-check` refuses** (SKEIN-685). This scope resolves no fleet path today — it
    // reads a placement record out of `$SKEIN_HOME` and builds an argv — but
    // `place::fleet_sandbox` is one call away from one, and an unpinned `$SKEIN_FLEET_ROOT`
    // means `/boxes`, which on this machine is the owner's live fleet. Pinned rather than
    // declared: it costs nothing here, and an exception is a thing someone has to re-judge.
    let was_root = std::env::var_os("SKEIN_FLEET_ROOT");
    let skein_home = dir.join("skein-home");
    fs::create_dir_all(skein_home.join("places")).unwrap();
    std::env::set_var("SKEIN_HOME", &skein_home);
    std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet-root"));
    fs::write(
        skein_home.join("places/thing-agree.json"),
        serde_json::json!({
            "sandbox": skein::place::fleet_sandbox(),
            "ns_pid": std::process::id(),
            "home": home.display().to_string(),
            "tree": dir.join("tree").display().to_string(),
            "sock": dir.join("box.sock").display().to_string(),
        })
        .to_string(),
    )
    .unwrap();
    let place = skein::place::place_of("thing-agree").expect("the placement record just written");
    let argv = place.exec_argv("true");
    match was_home {
        Some(v) => std::env::set_var("SKEIN_HOME", v),
        None => std::env::remove_var("SKEIN_HOME"),
    }
    match was_root {
        Some(v) => std::env::set_var("SKEIN_FLEET_ROOT", v),
        None => std::env::remove_var("SKEIN_FLEET_ROOT"),
    }

    // `Place::wrap` is the last element and spells the assignment `PATH='…' && cd …`. Cut out of
    // the real argv rather than predicted, and the cut is asserted so a changed shape reports
    // itself instead of comparing two empty strings.
    let wrapped = argv.last().expect("a crossing's argv").clone();
    let crossing_path = wrapped
        .split_once(" PATH='")
        .and_then(|(_, rest)| rest.split_once('\''))
        .map(|(v, _)| v.to_string())
        .unwrap_or_else(|| {
            panic!("a crossing's wrapped script no longer assigns PATH at all: {wrapped}")
        });

    assert_eq!(
        session_path, crossing_path,
        "box-session.sh starts a box's session on one PATH and a crossing into that same box lands \
         on another, so which agent a box runs depends on how it was reached"
    );
}
