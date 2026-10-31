//! The fleet's shared Playwright browsers (SKEIN-1341): read-only in every box, linked into each
//! box's own browsers directory by the kit as the box starts, and filled by skein in a namespace
//! that sees no box. Every browser here is a fixture tree; nothing is downloaded
//! (docs/decisions/tests-never-reach-a-live-fleet.md).

use super::*;
use skein::fleet::browsers;

/// A fixture "browser": a directory shaped like the ones Playwright makes, with an executable in it.
fn fixture_browser(out: &Path, name: &str, says: &str) {
    use std::os::unix::fs::PermissionsExt;
    let dir = out.join(name).join("chrome-linux");
    fs::create_dir_all(&dir).unwrap();
    fs::write(out.join(name).join("INSTALLATION_COMPLETE"), "").unwrap();
    fs::write(dir.join("chrome"), format!("#!/bin/sh\necho {says}\n")).unwrap();
    fs::set_permissions(dir.join("chrome"), fs::Permissions::from_mode(0o755)).unwrap();
}

/// Publish fixture browsers into the fleet's store, the way a fill does after its check.
fn publish_fixture(root: &Path, names: &[&str]) {
    let out = root.join(format!("staged-{}", names.join("-")));
    for name in names {
        fixture_browser(&out, name, &format!("skein-test-{name}"));
    }
    let staged = browsers::verify(&out, browsers::DEFAULT_FILL_CAP)
        .expect("a fixture browser passes the check");
    let store = PathBuf::from(browsers::store_dir());
    fs::create_dir_all(&store).unwrap();
    browsers::publish(&out, &store, &staged).expect("published");
}

/// Take an entry out of the store from the sandbox's side, as a clean would.
fn unpublish(name: &str) {
    let entry = PathBuf::from(browsers::store_dir()).join(name);
    let _ = Command::new("chmod")
        .arg("-R")
        .arg("u+w")
        .arg(&entry)
        .status();
    fs::remove_dir_all(&entry).unwrap();
}

/// Pin a fixture fleet for one of these tests, with the launcher (and the kit beside it) installed.
fn browser_fleet(what: &str) -> (Scratch, PathBuf, common::EnvPins) {
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
    (root, sandbox_home, pins)
}

/// Start box `name`, let its agent write `/tmp/<report>.report`, read that, and end the box.
fn run_in_box(name: &str, workshop: bool, agent: &str, report: &str) -> String {
    let mut script = session_script(name, "skein-agent", agent);
    if workshop {
        script = with_launch_env(&script, "SKEIN_BOX_PRIVILEGED", "1");
    }
    let launched = own_sandbox(FLEET)
        .exec(&script, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("start {name}: {e}"));
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    let report_path = PathBuf::from(format!("{}/tmp/{report}.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(60);
    while !report_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let said = fs::read_to_string(&report_path).unwrap_or_default();
    let _ = fs::remove_file(&report_path);
    let limits = fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
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
    said
}

fn field(report: &str, key: &str) -> String {
    report
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key} ")))
        .unwrap_or("")
        .to_string()
}

/// **The fleet's browser store is read-only in an ordinary box and in the workshop box: a write, a
/// chmod, a rename and a hard link into or out of it are all refused, and each box is pointed at a
/// browsers directory of its own** (SKEIN-1341).
///
/// The marks are looked for on the HOST afterwards, so an attempt a box was told failed and that
/// landed anyway still fails the test. The workshop box is the case that matters: every other box
/// sees `.skein` read-only already, and the workshop box saw the fleet root as the sandbox does.
///
/// What would make each assertion fail:
///   * every refusal, in the workshop box: dropping the `--ro-bind` of the store from
///     `box-session.sh` (the workshop box then writes, chmods, renames and links).
///   * `path` and `gc`: dropping the session block's `PLAYWRIGHT_*` export.
#[test]
fn the_browser_store_is_read_only_in_every_box() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so whether a box can \
             change the fleet's browsers was NOT exercised",
        );
    }
    let (root, sandbox_home, _pins) = browser_fleet("pwstore");
    publish_fixture(&root, &["chromium-1"]);
    let store = PathBuf::from(browsers::store_dir());
    let chrome = store.join("chromium-1/chrome-linux/chrome");
    let before = fs::read(&chrome).unwrap();
    let mode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&chrome).unwrap().mode()
    };

    let s = store.display().to_string();
    let agent = format!(
        "s='{s}'; e=\"$s/chromium-1\"; {{ \
           printf 'path %s\\n' \"${{PLAYWRIGHT_BROWSERS_PATH-none}}\"; \
           printf 'gc %s\\n' \"${{PLAYWRIGHT_SKIP_BROWSER_GC-none}}\"; \
           echo x > \"$e/planted\" 2>/dev/null && echo 'write yes' || echo 'write no'; \
           echo x >> \"$e/chrome-linux/chrome\" 2>/dev/null && echo 'append yes' || echo 'append no'; \
           chmod u+w \"$e/chrome-linux/chrome\" 2>/dev/null && echo 'chmod yes' || echo 'chmod no'; \
           mv \"$e\" \"$s/chromium-9\" 2>/dev/null && echo 'rename yes' || echo 'rename no'; \
           ln \"$e/chrome-linux/chrome\" \"$s/hard\" 2>/dev/null && echo 'hardin yes' || echo 'hardin no'; \
           ln \"$e/chrome-linux/chrome\" \"$HOME/hard\" 2>/dev/null && echo 'hardout yes' || echo 'hardout no'; \
           mkdir \"$s/webkit-1\" 2>/dev/null && echo 'mkdir yes' || echo 'mkdir no'; \
         }} > /tmp/pw.tmp && mv /tmp/pw.tmp /tmp/pw.report; exec sleep 300"
    );
    for (case, name, workshop) in [
        ("ordinary", "pwstore-main", false),
        ("workshop", "pwshop-main", true),
    ] {
        let report = run_in_box(name, workshop, &agent, "pw");
        assert_eq!(
            field(&report, "path"),
            sandbox_home
                .join(".cache/ms-playwright")
                .display()
                .to_string(),
            "the {case} box is not pointed at a browsers directory of its own: {report:?}"
        );
        assert_eq!(field(&report, "gc"), "1", "{case} box: {report:?}");
        for what in [
            "write", "append", "chmod", "rename", "hardin", "hardout", "mkdir",
        ] {
            assert_eq!(
                field(&report, what),
                "no",
                "the {case} box could {what} the fleet's browser store: {report:?}"
            );
        }
        let marks: Vec<PathBuf> = [
            store.join("chromium-1/planted"),
            store.join("chromium-9"),
            store.join("hard"),
            store.join("webkit-1"),
            PathBuf::from(box_root(name)).join("home/hard"),
        ]
        .into_iter()
        .filter(|p| p.exists())
        .collect();
        assert!(
            marks.is_empty(),
            "the {case} box changed the fleet's browser store: {marks:?}"
        );
        assert_eq!(
            fs::read(&chrome).unwrap(),
            before,
            "{case} box changed a browser"
        );
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&chrome).unwrap().mode(),
            mode,
            "{case} box chmodded a browser"
        );
    }
}

/// **The kit links every browser in the store into a box's own browsers directory at each start; a
/// link a box plants there misleads that box alone; a published entry is linked at the box's next
/// start; and a link to an entry that is gone is removed** (SKEIN-1341).
///
/// Box A starts and runs the kit as a start does, then plants a link of its own at the name the
/// next entry will have, and tries to plant one in the store. The next entry is published and the
/// first taken away. Box B then starts and must link the new entry to the store, whatever A did.
/// Box A starts again and must link a third entry published since, drop its dangling link, and keep
/// its own planted link (a box's own entry is never replaced).
///
/// What would make each assertion fail:
///   * A's and B's links: the kit's link loop deleted, or moved after the kit's early exit (a start
///     that is not a provision would then link nothing).
///   * A's first links holding only `chromium-1`: either of the kit's two `case "$skein_name"`
///     name-shape filters removed. The store also holds `notabrowser` (no revision, which only the
///     first filter refuses), `chro mium-1` (a space, which only the second refuses) and `Chromium-1`
///     (upper case, which both refuse). None of them is a browser directory, and a name in the
///     store becomes a path in every box. Watched: each filter removed alone, and both together.
///   * B's `chromium-2`: a store a box could write (A's plant would land there), or a kit that
///     followed links rather than reading them.
///   * A's `chromium-1` gone at its next start: the kit's dangling-link pass deleted.
///   * A's `chromium-2` kept: a kit that replaces anything at an entry's name.
#[test]
fn every_start_links_the_fleets_browsers_and_a_boxs_plants_stay_its_own() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so the kit's browser \
             links were NOT exercised",
        );
    }
    let (root, _sandbox_home, _pins) = browser_fleet("pwlinks");
    publish_fixture(&root, &["chromium-1"]);
    // Directories in the store that are not `<name>-<revision>`: the store only ever receives what
    // `verify` passed, so these are put there by hand, as anything that wrote the store would.
    for odd in ["notabrowser", "Chromium-1", "chro mium-1"] {
        fs::create_dir_all(PathBuf::from(browsers::store_dir()).join(odd)).unwrap();
    }
    let store = PathBuf::from(browsers::store_dir());
    let kit = skein::fleet::box_provision_path();
    let pw = "\"$HOME/.cache/ms-playwright\"";
    let start = format!(
        "mkdir -p /tmp/m && SKEIN_STARTUP_MARKERS=/tmp/m bash '{kit}' >/tmp/kit.out 2>&1; \
         for l in {pw}/*; do printf 'link %s %s\\n' \"$(basename \"$l\")\" \"$(readlink \"$l\")\"; done"
    );
    let links = |report: &str| -> Vec<(String, String)> {
        report
            .lines()
            .filter_map(|l| l.strip_prefix("link "))
            .filter_map(|l| l.split_once(' '))
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    let to = |name: &str| store.join(name).display().to_string();

    let a_agent = format!(
        "{{ {start}; \
           ln -s /tmp/a-own {pw}/chromium-2 2>/dev/null; \
           ln -s /tmp/a-own '{s}/chromium-3' 2>/dev/null && echo 'store planted'; true; \
         }} > /tmp/k.tmp && mv /tmp/k.tmp /tmp/k.report; exec sleep 300",
        s = store.display()
    );
    let a_first = run_in_box("pwlinks-main", false, &a_agent, "k");
    assert_eq!(
        links(&a_first),
        [("chromium-1".to_string(), to("chromium-1"))],
        "box A's start did not link the store's one browser: {a_first:?}"
    );
    assert!(!a_first.contains("store planted"), "{a_first:?}");

    publish_fixture(&root, &["chromium-2"]);
    unpublish("chromium-1");

    let b_agent =
        format!("{{ {start}; }} > /tmp/k.tmp && mv /tmp/k.tmp /tmp/k.report; exec sleep 300");
    let b = run_in_box("pwlinks-two", false, &b_agent, "k");
    assert_eq!(
        links(&b),
        [("chromium-2".to_string(), to("chromium-2"))],
        "box B did not get the store's browser, or got what box A planted: {b:?}"
    );

    publish_fixture(&root, &["chromium-3"]);
    let a_next = run_in_box("pwlinks-main", false, &b_agent, "k");
    assert_eq!(
        links(&a_next),
        [
            ("chromium-2".to_string(), "/tmp/a-own".to_string()),
            ("chromium-3".to_string(), to("chromium-3")),
        ],
        "box A's next start did not link the new browser, kept a link to one that is gone, or \
         replaced the link it planted itself: {a_next:?}"
    );
}

/// The words of a fixture fetch: a shell, run inside the fill's namespace, that stages a browser
/// and writes down what it could see and reach from in there.
fn fixture_fetch(root: &Path, extra: &str) -> Vec<String> {
    let boxes = root.join("boxes");
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "o=\"$PLAYWRIGHT_BROWSERS_PATH/chromium-7\"; mkdir -p \"$o/chrome-linux\"; \
             : > \"$o/INSTALLATION_COMPLETE\"; r=\"$o/report\"; \
             [ -e '{b}/web-main' ] && echo 'box seen' >> \"$r\"; \
             [ -e '{b}/.skein' ] && echo 'fleet seen' >> \"$r\"; \
             [ -e /run/docker.sock ] && echo 'docker seen' >> \"$r\"; \
             touch '{b}/.skein/planted' 2>/dev/null && echo 'fleet written' >> \"$r\"; \
             env | cut -d= -f1 | grep -q '^SKEIN' && echo 'skein env' >> \"$r\"; \
             [ -n \"$GH_TOKEN\" ] && echo 'token' >> \"$r\"; \
             {extra} echo done >> \"$r\"",
            b = boxes.display()
        ),
    ]
}

/// **A fill runs in a namespace that sees no box, no fleet and no credential and writes only its
/// staging directory; what it staged is published read-only; and a fill that stages a link is
/// refused whole** (SKEIN-1341).
///
/// The fetch is a fixture standing where `npx playwright install` stands, run through the real
/// crossing and the real namespace, with a box directory, `.skein` and `GH_TOKEN` in this process
/// for it to find. Its report is published as part of the browser it staged, so it is read back from
/// the store.
///
/// What would make each assertion fail:
///   * the report: binding the fleet root, `/run` or the sandbox's home into the namespace, or
///     dropping `--clearenv` (the fetch then sees a box, the fleet, `SKEIN_*` or the token).
///   * `.skein/planted`: a writable bind of anything but staging.
///   * read-only: `publish` skipping `make_read_only`.
///   * the refused fill: `verify` reading through links; and the store keeping its staging
///     directory: `fill_with` not removing it.
#[test]
fn a_fill_sees_no_box_and_writes_only_its_staging() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() {
        return skip("this machine cannot make a bwrap namespace, so a fill was NOT exercised");
    }
    let (root, _sandbox_home, mut pins) = browser_fleet("pwfill");
    pins.set("GH_TOKEN", "skein-test-token");
    fs::create_dir_all(root.join("boxes/web-main/home")).unwrap();
    let store = PathBuf::from(browsers::store_dir());

    let said = browsers::fill_with(FLEET, "chromium@1.62.0", &fixture_fetch(&root, ""))
        .unwrap_or_else(|e| panic!("the fixture fill failed: {e}"));
    assert!(said.contains("chromium-7"), "{said}");
    assert_eq!(
        fs::read_to_string(store.join("chromium-7/report")).unwrap(),
        "done\n",
        "the fill's namespace could see or reach more than its staging"
    );
    assert!(!root.join("boxes/.skein/planted").exists());
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::metadata(store.join("chromium-7/report"))
            .unwrap()
            .mode()
            & 0o222,
        0
    );

    let why = browsers::fill_with(
        FLEET,
        "firefox@1.62.0",
        &fixture_fetch(&root, "ln -s /etc \"$o/chrome-linux/etc\";"),
    )
    .expect_err("a fill that staged a link was published");
    assert!(why.contains("is a link"), "{why}");
    assert_eq!(
        fs::read_dir(store.join(".staging")).unwrap().count(),
        0,
        "a fill left its staging behind"
    );
}

/// **A browser request is answered without the owner: a good one is fetched and installed, a bad
/// one is denied with the reason, and a box's second request waits for its first** (SKEIN-1341).
///
/// The queue is planted directly, as a box can write it. The fetch is the fixture, swapped in for
/// `npx` at the crossing; a fill whose fetch was not swapped runs `false` instead, so nothing here
/// can reach the network.
///
/// What would make each assertion fail:
///   * `installed`: `serve` not approving, or `install` running the apt script for a browser.
///   * `denied`: `problem` accepting any package for the `playwright` kind.
///   * the second request still pending: `serve` taking more than one request per box a pass.
#[test]
fn a_browser_request_is_answered_without_the_owner() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("jq") {
        return skip("no bwrap or no jq here, so the request pass was NOT exercised");
    }
    let (root, _sandbox_home, _pins) = browser_fleet("pwserve");
    let queue = root.join("boxes/.skein/substrate/requests");
    let plant = |b: &str, id: &str, want: &str| {
        fs::create_dir_all(queue.join(b)).unwrap();
        let body = serde_json::json!({
            "id": id, "box": b, "kind": "playwright", "packages": [want],
            "asked": format!("2026-10-29T10:00:0{}Z", id.len() % 10), "state": "pending",
        });
        fs::write(queue.join(b).join(format!("{id}.json")), body.to_string()).unwrap();
    };
    plant("web-main", "a1", "chromium@1.62.0");
    plant("web-main", "a22", "firefox@1.62.0");
    plant("api-main", "b1", "chrome@1.62.0");

    let fixture = fixture_fetch(&root, "")
        .iter()
        .map(|w| skein::util::sh_quote(w))
        .collect::<Vec<_>>()
        .join(" ");
    let _crossing = skein::place::seam::install(Box::new(move |argv: &[String]| {
        if !argv.iter().any(|a| a.contains("'npx' '-y'")) {
            return None;
        }
        let swapped: Vec<String> = argv
            .iter()
            .map(|a| {
                a.replace(
                    "'npx' '-y' 'playwright@1.62.0' 'install' 'chromium'",
                    &fixture,
                )
            })
            .collect();
        if swapped.iter().any(|a| a.contains("'npx' '-y'")) {
            return Some(vec!["false".into()]);
        }
        Some(swapped)
    }));
    let did = browsers::serve(FLEET);
    let state = |b: &str, id: &str| skein::substrate::decision(b, id).map(|r| (r.state, r.log));
    let (good, log) = state("web-main", "a1").expect("the good request was not decided");
    assert_eq!(good, "installed", "{log}\n{did:?}");
    assert!(PathBuf::from(browsers::store_dir())
        .join("chromium-7")
        .is_dir());
    let (bad, why) = state("api-main", "b1").expect("the bad request was not decided");
    assert_eq!(bad, "denied", "{did:?}");
    assert!(why.contains("not a package name"), "{why}");
    assert!(
        state("web-main", "a22").is_none(),
        "a box's second browser request was taken in the same pass: {did:?}"
    );
}

/// Start box `name` with `agent` as its agent, wait for it to write `/tmp/<report>.report`, and
/// place it the way a start does, so skein's crossing can find it. Returns the anchor, and leaves
/// the box running: [`end_box`] stops it.
fn start_running(name: &str, sandbox_home: &Path, agent: &str, report: &str) -> (u32, String) {
    let launched = own_sandbox(FLEET)
        .exec(
            &session_script(name, "skein-agent", agent),
            Duration::from_secs(60),
        )
        .unwrap_or_else(|e| panic!("start {name}: {e}"));
    let anchor = anchor_from_launch(&launched).expect("the launcher reports its anchor pid");
    let report_path = PathBuf::from(format!("{}/tmp/{report}.report", box_root(name)));
    let deadline = Instant::now() + Duration::from_secs(60);
    while !report_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let said = fs::read_to_string(&report_path).unwrap_or_default();
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
    (anchor, said)
}

fn end_box(name: &str, anchor: u32) {
    forget_place(name);
    let limits = fs::read_to_string(format!("{}/limits.state", box_root(name))).unwrap_or_default();
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
}

/// The links in a browsers directory, read from the sandbox's side without following any.
fn links_in(dir: &Path) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let to = fs::read_link(e.path()).ok()?;
            Some((
                e.file_name().to_string_lossy().into_owned(),
                to.display().to_string(),
            ))
        })
        .collect();
    found.sort();
    found
}

/// **A browser skein fetches reaches the boxes running now, without a restart: each links it
/// inside itself, as itself, by the kit's own function; a link a box planted stays its own; a box
/// whose browsers directory is a link gets the links where that link leads inside the box, never
/// where it leads from outside; and a box that is not running is left for its next start**
/// (SKEIN-1355).
///
/// Box A starts, runs the kit and plants its own link at `chromium-7`, the name the fill will
/// publish. Box B makes its browsers directory a link into its own `/tmp`, then runs the kit. Box C
/// is placed but not running. The fill is the real one, with a fixture standing where `npx` stands,
/// and it stages `chromium-7` and `ffmpeg-8`. Neither A nor B is restarted.
///
/// What would make each assertion fail:
///   * A's `ffmpeg-8` and B's two links: the post-fill pass skipped (`fill_with` not calling
///     `link_running_boxes`), or the kit's function no longer cut whole out of the kit.
///   * A's `chromium-7` still `/tmp/a-own`: a pass that replaces what is at an entry's name.
///   * nothing at B's link target as seen from the sandbox: the link run from the sandbox's side
///     with the box's home as `HOME` rather than inside the box, which follows B's link out to the
///     sandbox's own `/tmp`.
///   * A's anchor the same process: a pass that restarts boxes to link them.
///   * C among the later ones, and "Linked now in 2": a pass that counts a box that is not running.
#[test]
fn a_fetched_browser_reaches_running_boxes_without_a_restart() {
    let _env = env_lock();
    let _real = skein::place::seam::real_crossings();
    if !bwrap_works() || !have("tmux") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux, so linking a fetched \
             browser into running boxes was NOT exercised",
        );
    }
    let (root, sandbox_home, _pins) = browser_fleet("pwlive");
    publish_fixture(&root, &["chromium-1"]);
    let store = PathBuf::from(browsers::store_dir());
    let to = |name: &str| store.join(name).display().to_string();
    let kit = skein::fleet::box_provision_path();
    let pw = "\"$HOME/.cache/ms-playwright\"";
    let start =
        format!("mkdir -p /tmp/m && SKEIN_STARTUP_MARKERS=/tmp/m bash '{kit}' >/tmp/kit.out 2>&1");
    // A name of B's own, inside B's private `/tmp`, that would be the SANDBOX's `/tmp` if anything
    // followed B's link from out here. Made, empty, on the sandbox's side too, so a pass that
    // followed the link from here would find a directory to write into rather than stopping at a
    // dangling link, and is caught by what it wrote.
    let b_farm = format!("/tmp/skein-test-pwlive-{}", std::process::id());
    let _ = fs::remove_dir_all(&b_farm);
    fs::create_dir_all(&b_farm).unwrap();

    let (a, a_said) = start_running(
        "pwlive-main",
        &sandbox_home,
        &format!(
            "{start}; ln -s /tmp/a-own {pw}/chromium-7; echo ready > /tmp/r.tmp && \
             mv /tmp/r.tmp /tmp/r.report; exec sleep 300"
        ),
        "r",
    );
    let (b, b_said) = start_running(
        "pwlive-two",
        &sandbox_home,
        &format!(
            "rm -rf {pw}; mkdir -p \"$HOME/.cache\" '{b_farm}'; ln -s '{b_farm}' {pw}; {start}; \
             echo ready > /tmp/r.tmp && mv /tmp/r.tmp /tmp/r.report; exec sleep 300"
        ),
        "r",
    );
    record_place(
        "pwlive-gone",
        &PlaceRecord {
            sandbox: FLEET.into(),
            home: sandbox_home.display().to_string(),
            tree: "/".into(),
            sock: box_sock("pwlive-gone"),
            ..Default::default()
        },
    )
    .expect("place the stopped box");

    let a_farm = PathBuf::from(box_root("pwlive-main")).join("home/.cache/ms-playwright");
    let b_own = PathBuf::from(box_root("pwlive-two")).join(b_farm.trim_start_matches('/'));
    let a_before = links_in(&a_farm);
    let b_before = links_in(&b_own);

    let said = browsers::fill_with(
        FLEET,
        "chromium@1.62.0",
        &fixture_fetch(&root, "mkdir -p \"$PLAYWRIGHT_BROWSERS_PATH/ffmpeg-8\";"),
    );
    let a_after = links_in(&a_farm);
    let b_after = links_in(&b_own);
    let host_side = links_in(Path::new(&b_farm));
    let a_alive = Path::new(&format!("/proc/{a}")).exists();
    end_box("pwlive-main", a);
    end_box("pwlive-two", b);
    forget_place("pwlive-gone");
    let _ = fs::remove_dir_all(&b_farm);

    let said = said.unwrap_or_else(|e| panic!("the fixture fill failed: {e}"));
    assert_eq!(a_said.trim(), "ready", "box A did not come up");
    assert_eq!(b_said.trim(), "ready", "box B did not come up");
    assert_eq!(
        a_before,
        [
            ("chromium-1".to_string(), to("chromium-1")),
            ("chromium-7".to_string(), "/tmp/a-own".to_string()),
        ],
        "box A's start did not link the store, or its plant did not land"
    );
    assert_eq!(
        b_before,
        [("chromium-1".to_string(), to("chromium-1"))],
        "box B's start did not link the store through its own link"
    );
    assert_eq!(
        a_after,
        [
            ("chromium-1".to_string(), to("chromium-1")),
            ("chromium-7".to_string(), "/tmp/a-own".to_string()),
            ("ffmpeg-8".to_string(), to("ffmpeg-8")),
        ],
        "running box A did not gain the fetched browser, or lost its own link: {said}"
    );
    assert!(
        host_side.is_empty(),
        "something followed box B's link from the sandbox's side and wrote into {b_farm} there: \
         {host_side:?}"
    );
    assert_eq!(
        b_after,
        [
            ("chromium-1".to_string(), to("chromium-1")),
            ("chromium-7".to_string(), to("chromium-7")),
            ("ffmpeg-8".to_string(), to("ffmpeg-8")),
        ],
        "running box B did not gain the fetched browsers inside itself: {said}"
    );
    assert!(a_alive, "box A was restarted to link the browser");
    assert!(
        said.contains(
            "Linked now in 2 running box(es); 1 other box(es) link it at their next start."
        ),
        "{said}"
    );
}
