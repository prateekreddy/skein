//! Saving every box's work to the host is its own act, and the properties that make it one.
//!
//! `fleet::archive_box` — the byte copy that is the whole of "your work is safe" — existed for a
//! long time as phase 1 of a resize, which then destroys the fleet. That is the only reason a
//! person could not simply save their work, and SKEIN-680 is the door: same copy, no destroy.
//!
//! **So these tests are about what a save does NOT do**, as much as what it does. The strong one
//! below asserts that the archive is really on the host, that it holds the box's work, and that the
//! box is *still running* afterwards — not that a message was printed. A save somebody presses
//! while agents are working must leave those agents working.
//!
//! Its own binary because it drives skein through process-wide environment and needs the fleet's
//! execution seam stood in for.
//!
//! **What a fleet-scope command runs is stood in for through `place::seam`, not through `$PATH`**
//! (SKEIN-592). There is no `sbx` hop in-fleet: the script runs on this machine, so a fake on
//! `$PATH` is bypassed and the real command runs against the real fleet. The stand-in here does not
//! fake the copy — it runs the actual `archive_script` against a fixture fleet root, with `sudo`
//! stood in for by a shim that execs its arguments, so what is asserted is a tar file that `tar`
//! itself wrote.

mod common;

use common::{env_lock, Scratch};
use std::path::{Path, PathBuf};

const FLEET: &str = "save-fleet";

/// A process standing in for a box's anchor — killed on every way out, including a panic.
///
/// A `sleep` is a faithful enough stand-in for the property under test: the question a save has to
/// answer is whether the thing holding the box is still there when the copy finishes, and the
/// cheapest honest way to ask it is to hold something and look afterwards. The `Drop` is not
/// tidiness — a test that panics between the spawn and the kill is exactly how this repository
/// accumulated processes that outlived their fixtures by hours (SKEIN-645).
struct Anchor(std::process::Child);

impl Anchor {
    fn spawn() -> Anchor {
        Anchor(
            std::process::Command::new("sleep")
                .arg("120")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("a stand-in anchor process"),
        )
    }
    /// Alive means **running**, not merely present, and the difference is what this check is for.
    ///
    /// It was `kill -0` first, and that could not fail: a child this process killed and has not
    /// reaped is a zombie, the pid still exists, and `kill -0` succeeds. Proved by writing the
    /// sabotage this test names — a save that kills the box's anchor before copying it — and
    /// watching the assertion pass. `/proc/<pid>/stat`'s state field is what tells the two apart.
    fn alive(&self) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", self.0.id())) else {
            return false;
        };
        // The comm field is parenthesised and may itself contain spaces and brackets, so the state
        // is the first field after the LAST `)`.
        stat.rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z")
    }
}

impl Drop for Anchor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// **The stand-in for every fleet-scope command, and it really runs them.**
///
/// `archive_script` is the thing under test, so faking its output would leave nothing to assert:
/// the exclusions, the `rm -f` before the create and the `tar -C <box root>` are the difference
/// between an archive that holds the box and one that holds nothing. What is substituted is only
/// what a fixture cannot have — root — by putting a `sudo` that execs its arguments ahead of the
/// fixed PATH the real shell would use.
///
/// Every script is logged, including ones this fixture did not expect, because a command that
/// slipped past the stand-in would run for real and be invisible.
fn stand_in_for_fleet_commands(bin: &Path, log: PathBuf) -> skein::place::seam::Installed {
    let sudo = bin.join("sudo");
    std::fs::create_dir_all(bin).unwrap();
    std::fs::write(&sudo, "#!/bin/sh\nexec \"$@\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let bin = bin.to_path_buf();
    skein::place::seam::install(Box::new(move |argv: &[String]| {
        let script = argv.last().cloned().unwrap_or_default();
        Some(vec![
            "sh".to_string(),
            "-c".into(),
            format!(
                "printf 'ran %s\\n' {quoted} >> {log}\nPATH={bin}:$PATH\n{script}",
                quoted = shell_quote(&script),
                log = log.display(),
                bin = shell_quote(&bin.to_string_lossy()),
            ),
        ])
    }))
}

/// Single-quoted for `sh`, with embedded quotes closed and reopened — the scripts carry them.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A fleet with `boxes` in it: a checkout each, a placement record each, and a configured sandbox.
fn stage(what: &str, boxes: &[&str]) -> (Scratch, PathBuf, skein::place::seam::Installed) {
    let root = Scratch::boxes(&format!("skein-save-it-{what}"));
    let log = root.join("ran.log");
    std::env::set_var("SKEIN_HOME", root.join("skein"));
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));
    // The state root has to exist before the save runs, or `room_to_copy_out`'s `df` has nothing to
    // measure and it takes its "could not measure; continuing" arm — which would quietly skip the
    // real space check in every test in this file.
    std::fs::create_dir_all(root.join("skein").join("boxes")).unwrap();
    let stood_in = stand_in_for_fleet_commands(&root.join("bin"), log.clone());
    let mut config = skein::config::load_config();
    config.fleet_sandbox = FLEET.into();
    skein::config::save_config(&config).expect("configure the fleet");
    for name in boxes {
        let box_root = root.join("boxes").join(name);
        std::fs::create_dir_all(box_root.join("tree")).unwrap();
        std::fs::write(box_root.join("tree").join("work.txt"), b"unpushed\n").unwrap();
        skein::place::record_place(
            name,
            &skein::place::PlaceRecord {
                sandbox: FLEET.into(),
                ns_pid: std::process::id(),
                home: box_root.join("home").to_string_lossy().into_owned(),
                tree: box_root.join("tree").to_string_lossy().into_owned(),
                sock: box_root.join("session.sock").to_string_lossy().into_owned(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .expect("place the box");
    }
    (root, log, stood_in)
}

fn unstage() {
    std::env::remove_var("SKEIN_HOME");
    std::env::remove_var("SKEIN_FLEET_ROOT");
}

/// What `tar` says is inside an archive.
fn members(archive: &str) -> String {
    let out = std::process::Command::new("tar")
        .args(["-tf", archive])
        .output()
        .expect("list the archive");
    assert!(
        out.status.success(),
        "tar could not read {archive}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// **The strong one: the work is on the host and the box is still running.**
///
/// Both halves are the act. A save that copies nothing is a lie about where somebody's work is; a
/// save that stops a box to copy it is a checkpoint inside a lifecycle operation, which is the
/// shape the owner declined twice (SKEIN-680). Asserted against the archive `tar` actually wrote
/// and against the process still holding the box, never against a message.
///
/// **What would make this fail**, and each was made and watched:
///   * making the save copy nothing — replacing the `archive_box` call with `Ok(box_archive(…))`,
///     so the report names a path with no file at it. Fired "the archive is not on the host".
///   * making the save stop the box — killing the anchor before the copy. Fired "the box was
///     stopped by a save".
#[test]
fn saving_every_box_puts_its_work_on_the_host_and_leaves_the_boxes_running() {
    let _env = env_lock();
    let (root, log, _stood_in) = stage("all", &["web-main", "api-worker"]);
    // A box is running: something holds it, and its pid is in the box root the way a live box's is.
    let anchor = Anchor::spawn();
    std::fs::write(
        root.join("boxes").join("web-main").join("anchor.pid"),
        format!("{}\n", anchor.0.id()),
    )
    .unwrap();

    let saved = skein::fleet::save_boxes(&[]).expect("a save with a readable census and room");
    let ran = std::fs::read_to_string(&log).unwrap_or_default();
    unstage();

    assert_eq!(
        saved.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["api-worker", "web-main"],
        "a save reports every box in the census, in the order the census gives them"
    );
    for one in &saved {
        assert!(
            one.error.is_empty(),
            "{} was not copied out: {}",
            one.name,
            one.error
        );
        assert!(
            Path::new(&one.archive).is_file(),
            "the archive is not on the host — {} names {} and there is no file there",
            one.name,
            one.archive
        );
        // Not merely a file: the box's uncommitted work has to be inside it, or the save has
        // produced something the size of nothing and reported a path.
        let inside = members(&one.archive);
        assert!(
            inside.contains("./tree/work.txt"),
            "{}'s archive does not hold its checkout:\n{inside}",
            one.name
        );
        // The one exclusion that matters: `anchor.pid` names a process in a sandbox that will not
        // exist, and a box restored claiming an anchor that was never there is worse than one that
        // knows it has none.
        assert!(
            !inside.contains("anchor.pid"),
            "{}'s archive carries the anchor pid:\n{inside}",
            one.name
        );
        // Where it went, and how to bring it back — the second is what makes the copy a two-way
        // street rather than a one-way one.
        assert!(
            one.restore.contains(&one.archive)
                && one.restore.contains(&format!("/boxes/{}", one.name)),
            "{}'s restore line names neither the archive nor the box root: {}",
            one.name,
            one.restore
        );
    }

    // **Nothing was stopped.** The box's anchor is still the process it was.
    assert!(
        anchor.alive(),
        "the box was stopped by a save — a save must be pressable while agents are working"
    );
    // **Nothing was destroyed.** The checkout is still in the sandbox, which is what makes this a
    // copy: the archive is a second place the work lives, not the only one.
    let still = root
        .join("boxes")
        .join("web-main")
        .join("tree")
        .join("work.txt");
    assert_eq!(
        std::fs::read_to_string(&still).unwrap_or_default(),
        "unpushed\n",
        "a save moved the work instead of copying it: {}",
        still.display()
    );
    // And it asked the sandbox for nothing destructive. The transcript is every fleet-scope script
    // that ran, including any this fixture did not expect.
    for forbidden in ["rm -rf", "sbx rm", "kill", "tmux kill"] {
        assert!(
            !ran.contains(forbidden),
            "a save ran something that stops or destroys ({forbidden}):\n{ran}"
        );
    }
}

/// **One box failing does not lose the report, and does not lose the other boxes** (SKEIN-680).
///
/// This is where a save differs from a resize. A resize aborts on the first box it cannot read,
/// because what follows is a destroy and a partial copy is lost work. Nothing is destroyed here, so
/// stopping would leave every remaining box uncopied for no gain — and the box that failed has to
/// be named, because it is the one whose work is still only inside the sandbox.
///
/// The failure is arranged the way a real one arrives: the archive's own state directory cannot be
/// made, so `archive_script` aborts at its first line under `set -e`.
///
/// **What would make this fail**: reporting in aggregate, or aborting the run at the first failure.
/// Proved by turning the per-box `Err` arm into an early `return Err(why)`, which fired "a save
/// that lost one box lost the report for all of them".
#[test]
fn a_save_that_loses_one_box_still_saves_the_rest_and_names_the_one_it_lost() {
    let _env = env_lock();
    let (root, _log, _stood_in) = stage("partial", &["web-main", "api-worker"]);
    // `box_state` for this box cannot be a directory, so `mkdir -p` fails and the copy stops there.
    std::fs::create_dir_all(root.join("skein").join("boxes")).unwrap();
    std::fs::write(root.join("skein").join("boxes").join("api-worker"), b"x").unwrap();

    let saved = skein::fleet::save_boxes(&[])
        .expect("a save that lost one box lost the report for all of them");
    unstage();

    let lost = saved
        .iter()
        .find(|s| s.name == "api-worker")
        .expect("the box that failed is still in the report");
    assert!(
        !lost.error.is_empty() && lost.archive.is_empty(),
        "a box that was not copied out is reported as though it was: {lost:?}"
    );
    assert!(
        lost.error.contains("api-worker"),
        "the failure does not name the box whose work is still in the sandbox: {}",
        lost.error
    );
    let kept = saved
        .iter()
        .find(|s| s.name == "web-main")
        .expect("the boxes that succeeded are still in the report");
    assert!(
        kept.error.is_empty() && Path::new(&kept.archive).is_file(),
        "one box's failure took a box that had already been copied out with it: {kept:?}"
    );
}

/// **A census that could not be taken is not a fleet with nothing to save** (SKEIN-347, again).
///
/// A checkout with no placement record is a box skein cannot address and would silently skip. A
/// save that skipped it would hand somebody a report that looks complete and is short by exactly
/// the box they are about to destroy — which is worse than refusing, because it is believed.
///
/// **What would make this fail**: taking the census with `placed_boxes`, which cannot fail and
/// answers "no boxes" to a read it could not do. Proved — the save then reported one box copied and
/// said nothing about the other, firing the first assertion.
#[test]
fn a_census_that_could_not_be_taken_refuses_rather_than_saving_what_it_could_see() {
    let _env = env_lock();
    let (root, _log, _stood_in) = stage("census", &["web-main"]);
    // A checkout nobody has a record for: a record never written, or removed by hand.
    std::fs::create_dir_all(root.join("boxes").join("stray").join("tree")).unwrap();

    let refused = skein::fleet::save_boxes(&[])
        .expect_err("a save with an unaccounted checkout in the fleet root must refuse");
    unstage();

    assert!(
        refused.contains("stray"),
        "the refusal does not name the checkout nothing could carry out: {refused}"
    );
    assert!(
        refused.contains("nothing was copied out"),
        "a refusal that does not say what it did leaves somebody guessing: {refused}"
    );
    // And it is a refusal somebody can get past without fixing the census first.
    assert!(
        refused.contains("skein save <box>"),
        "the refusal names no way through for somebody who knows which box they want: {refused}"
    );
}

/// **A name that is not a box refuses before a byte is written.**
///
/// The dangerous half is not the name that matches nothing — `tar` refuses a root that is not
/// there, loudly. It is the name that matches a **directory with no checkout in it**: `tar` writes
/// that one happily, and an empty archive comes back through the report as a save that worked. A
/// save reported about work it does not contain is the one failure a person cannot detect by
/// reading the report, because the report is a success.
///
/// **What would make this fail**: dropping the on-disk check and trusting the name. Proved by
/// removing it, which let the empty directory through and left the second assertion looking at a
/// `.tar` that existed. (The first version of this check asked only whether the directory was
/// there, which the same sabotage showed was weaker than it read: it is a *checkout* that makes a
/// box, which is `census_placed_boxes`'s own test.)
#[test]
fn a_box_that_is_not_there_is_refused_rather_than_archived_empty() {
    let _env = env_lock();
    let (root, _log, _stood_in) = stage("typo", &["web-main"]);
    // A directory under the fleet root with no `tree` in it: a box half-removed by hand, or a
    // mistyped name that happens to collide with one. Invisible to the census, which is why the
    // name has to be checked here as well.
    std::fs::create_dir_all(root.join("boxes").join("web-mian")).unwrap();

    let refused = skein::fleet::save_boxes(&["web-mian".to_string()])
        .expect_err("a save of a name with no checkout behind it must refuse");
    unstage();

    assert!(
        refused.contains("web-mian") && refused.contains("Nothing was copied out"),
        "the refusal does not say which name it could not find, or what it did: {refused}"
    );
    let state = root.join("skein").join("boxes");
    let wrote: Vec<String> = std::fs::read_dir(&state)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        wrote.is_empty(),
        "a refused save wrote something anyway: {wrote:?}"
    );
}
