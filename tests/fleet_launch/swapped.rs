//! A box that rearranges its own root between starts: the launcher refuses a swapped directory
//! (SKEIN-1254) and replaces, rather than writes through, a link left where it writes (SKEIN-1255).
//!
//! Everything under a box's root is writable from inside the box, and `box-session.sh` runs at
//! fleet scope, where the far end of a link can be anything this uid reaches. These tests run the
//! whole launcher, unmodified, under real bwrap and tmux, against a fixture fleet: they plant links
//! in a fixture box's root pointing at decoys elsewhere in the fixture, start the box, and read
//! back what happened to the decoys and to the links.

use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::Output;

/// What a planted link leads to: a file whose bytes and mode must come through a start unchanged.
const DECOY: &str = "skein-test-decoy: nothing may write here\n";

/// A fixture fleet with one box root in it, and the launcher run directly against it.
struct Swap {
    dir: Scratch,
    name: String,
}

impl Swap {
    fn new(dir: Scratch, name: &str) -> Swap {
        let s = Swap {
            dir,
            name: name.to_string(),
        };
        for d in [
            s.fleet().join(".skein"),
            s.sandbox_home().join(".claude"),
            s.dir.join("run/user"),
            s.dir.join("state").join(name),
            s.dir.join("pid"),
            s.dir.join("store"),
            s.dir.join("decoys"),
            s.root().join("tree"),
        ] {
            fs::create_dir_all(&d).unwrap();
        }
        // The credential helper the scoped block wires into the box's gitconfig. Without it the
        // `git config --file` writes are skipped entirely, and a test of them would test nothing.
        let helper = s.fleet().join(".skein/git-credential-skein");
        fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
        s
    }

    fn fleet(&self) -> PathBuf {
        self.dir.join("fleet")
    }

    fn sandbox_home(&self) -> PathBuf {
        self.dir.join("sandbox-home")
    }

    fn root(&self) -> PathBuf {
        self.fleet().join(&self.name)
    }

    fn sock(&self) -> PathBuf {
        self.root().join("session.sock")
    }

    /// A decoy file outside the box root, readable only by its owner, as a credential would be.
    fn decoy_file(&self, tag: &str) -> PathBuf {
        let p = self.dir.join("decoys").join(tag);
        fs::write(&p, DECOY).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        p
    }

    /// A decoy directory outside the box root, with one file in it.
    fn decoy_dir(&self, tag: &str) -> PathBuf {
        let p = self.dir.join("decoys").join(tag);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("sentinel"), DECOY).unwrap();
        p
    }

    /// Plant a link at `rel` (relative to the box root) leading to `to`.
    fn plant(&self, rel: &str, to: &Path) {
        let at = self.root().join(rel);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        symlink(to, &at).unwrap();
    }

    /// Run the launcher as skein's `session_script` would, with an environment holding only what
    /// this fixture names: every path it can reach at fleet scope is inside the scratch directory.
    fn start(&self) -> Output {
        self.launcher("exec sleep 300")
            .output()
            .expect("bash to run the launcher")
    }

    /// The launcher's command line for this box, with `pane` as the box's pane command.
    fn launcher(&self, pane: &str) -> Command {
        let launcher = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/box-session.sh");
        let mut cmd = Command::new("bash");
        cmd
            .arg(&launcher)
            .arg(&self.name)
            .arg(self.root())
            .arg(self.dir.join("pid").join(&self.name))
            .arg("skein-shell")
            .arg(self.dir.join("state").join(&self.name))
            .arg("")
            .args(["bash", "-lc", pane])
            .env_clear()
            .env("HOME", self.sandbox_home())
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("SKEIN_FLEET_ROOT", self.fleet())
            // Its companion, pinned too, though nothing here reads it: an unpinned one is the
            // real `~/.skein` for anything that ever does.
            .env("SKEIN_HOME", self.dir.join("skein"))
            .env("SKEIN_RUNTIME_DIR", self.dir.join("run/user"))
            .env("SKEIN_GIT_SCOPE", "repo")
            .env("SKEIN_BOX_REPO", "skein-test/repo")
            .env("SKEIN_BOX_PEERS", "0")
            .env("SKEIN_BOX_PRIVILEGED", "0")
            .env("SKEIN_BOX_STORE", self.dir.join("store"));
        cmd
    }

    /// End a box this fixture started, and wait for its namespace to go.
    fn stop(&self, started: &Output) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(self.sock())
            .arg("kill-server")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Ok(anchor) = anchor_from_launch(&String::from_utf8_lossy(&started.stdout)) {
            anchor_gone(anchor);
        }
    }

    /// Whether this box's tmux server answers, which is whether a box is running.
    fn running(&self) -> bool {
        Command::new("tmux")
            .arg("-S")
            .arg(self.sock())
            .arg("ls")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn can_host_a_box() -> bool {
    bwrap_works() && have("tmux") && have("git")
}

/// The bytes and mode of a decoy file, for comparing before and after.
fn decoy_state(p: &Path) -> (Vec<u8>, u32) {
    let meta = fs::symlink_metadata(p).expect("the decoy is still there");
    (fs::read(p).unwrap(), meta.permissions().mode() & 0o7777)
}

/// The names in a decoy directory, sorted.
fn listing(p: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(p)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// **A box that swapped one of the directories the launcher binds or writes for a link is not
/// started, and the launcher says which path and what to do** (SKEIN-1254).
///
/// Each directory gets its own box, so one refusal cannot stand in for another. For each, the link
/// leads to a decoy directory outside the box's root, which stands for another box's home or the
/// fleet's own state.
///
/// What makes each case fail, named before it was written and then done: deleting that path's
/// `own_dir` call from `box-session.sh`. The start then goes ahead: for `home` the box is started
/// with the decoy bound as its HOME and the launcher writes `.claude.json` and the gitconfig into
/// it; for `tmp` the start id lands in the decoy; for `.local/state` and `bin` the launcher creates
/// or writes into the decoy. The exit status, the absence of a running box, and the decoy's
/// listing each fail.
#[test]
fn a_box_whose_own_directory_was_swapped_for_a_link_is_refused() {
    if !can_host_a_box() {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    for (i, rel) in [
        "home",
        "tmp",
        "home/.local",
        "home/.local/state",
        "bin",
        "home/.claude",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("swapdir-{i}");
        let sub = Swap::new(scratch_named(&name), &name);
        let decoy = sub.decoy_dir(&format!("dir-{i}"));
        let before = listing(&decoy);
        // The parents of a nested path are the box's own, made the way a first start makes them.
        if let Some(parent) = Path::new(rel).parent() {
            fs::create_dir_all(sub.root().join(parent)).unwrap();
        }
        sub.plant(rel, &decoy);

        let out = sub.start();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let up = sub.running();
        sub.stop(&out);

        assert!(
            !out.status.success() && !up,
            "{rel} was a link to a directory outside the box, and the box started anyway (exit \
             {:?}, running: {up}). stderr:\n{stderr}",
            out.status.code()
        );
        let named = sub.root().join(rel);
        assert!(
            stderr.contains(&format!("{} is a symbolic link", named.display()))
                && stderr.contains("cannot start"),
            "the refusal for {rel} does not name the path and what is wrong with it:\n{stderr}"
        );
        assert_eq!(
            listing(&decoy),
            before,
            "the start was refused but something was still written into the decoy behind {rel}"
        );
        assert_eq!(
            fs::read_to_string(decoy.join("sentinel")).unwrap(),
            DECOY,
            "the decoy's own file behind {rel} was changed"
        );
    }
}

/// **A link left where the launcher writes a file is replaced by the file, and the thing it led to
/// is untouched** (SKEIN-1255).
///
/// Every write the launcher makes into the box's root before `exec bwrap` is planted at once: the
/// start id, the four shims and the three empty mount points beside them, the ssh-agent cover, the
/// ceiling record, the gitconfig, and the store record in the checkout's git directory. Each link
/// leads to its own decoy, a 0600 file outside the root, as a credential would be. The start id's
/// link leads to a DIRECTORY, which is the case `mv -T` exists for.
///
/// What makes it fail, named before it was written and then done, one write at a time: reverting
/// that write to `> "$target"` (or `: >`, or `git config --file "$home/.gitconfig"`). The decoy's
/// bytes then change and the path is still a link. Dropping `-T` from the start id's `mv` puts the
/// file inside the decoy directory instead. Dropping the `chmod` on a shim leaves it 0600, which
/// the mode assertion catches.
///
/// The shims are planted only for the tools this machine has: the launcher writes no `gh` shim
/// where there is no `gh`, and a link it never had reason to touch proves nothing either way.
#[test]
fn a_link_where_the_launcher_writes_is_replaced_and_what_it_led_to_is_untouched() {
    if !can_host_a_box() {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let swap = Swap::new(scratch_named("swapfile"), "swapfile-main");
    fs::create_dir_all(swap.root().join("tree/.git")).unwrap();
    fs::create_dir_all(swap.root().join("bin")).unwrap();
    fs::create_dir_all(swap.root().join("tmp")).unwrap();

    let mut files: Vec<(&str, Option<u32>)> = vec![
        ("no-ssh-agent", None),
        ("limits.state", None),
        ("home/.gitconfig", None),
        ("tree/.git/skein-store", None),
    ];
    for (tool, shim) in [
        ("sudo", "bin/sudo"),
        ("git", "bin/git"),
        ("gh", "bin/gh"),
        ("docker", "bin/docker"),
    ] {
        if have(tool) {
            files.push((shim, Some(0o755)));
        }
    }
    // `sudo` has no `.real`: its shim is bound over the real one and never execs it.
    let reals: Vec<String> = files
        .iter()
        .filter(|(p, m)| m.is_some() && *p != "bin/sudo")
        .map(|(p, _)| format!("{p}.real"))
        .collect();
    for real in &reals {
        files.push((real.as_str(), None));
    }
    assert!(
        files.iter().any(|(p, _)| *p == "bin/git"),
        "git is required above, so the git shim must be among the planted writes"
    );

    let mut decoys = Vec::new();
    for (rel, _) in &files {
        let decoy = swap.decoy_file(&rel.replace('/', "_"));
        swap.plant(rel, &decoy);
        decoys.push((rel.to_string(), decoy.clone(), decoy_state(&decoy)));
    }
    let start_id_decoy = swap.decoy_dir("start-id-dir");
    swap.plant("tmp/skein-start-id", &start_id_decoy);
    let start_id_before = listing(&start_id_decoy);

    let out = swap.start();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    swap.stop(&out);
    assert!(
        out.status.success(),
        "the box did not start, so nothing below would be about the launcher's writes: {stderr}"
    );

    for (rel, decoy, before) in &decoys {
        assert_eq!(
            &decoy_state(decoy),
            before,
            "the launcher wrote through the link at {rel}: the file it led to changed"
        );
        let at = swap.root().join(rel);
        let meta = fs::symlink_metadata(&at).unwrap();
        assert!(
            meta.file_type().is_file(),
            "{rel} is still a link (or not a file) after the start, so the launcher did not \
             replace it: {:?}",
            meta.file_type()
        );
    }
    for (rel, mode) in &files {
        if let Some(want) = mode {
            let got = fs::metadata(swap.root().join(rel))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                got, *want,
                "the shim at {rel} is mode {got:o}, not {want:o}"
            );
        }
    }
    assert_eq!(
        listing(&start_id_decoy),
        start_id_before,
        "the start id was moved INTO the directory its link led to"
    );
    let start_id = swap.root().join("tmp/skein-start-id");
    assert!(
        fs::symlink_metadata(&start_id)
            .unwrap()
            .file_type()
            .is_file()
            && !fs::read_to_string(&start_id).unwrap().trim().is_empty(),
        "the start id is not a file of the box's own after the start"
    );
    let gitconfig = fs::read_to_string(swap.root().join("home/.gitconfig")).unwrap();
    assert!(
        gitconfig.contains("useHttpPath = true") && gitconfig.contains("git-credential-skein"),
        "the replaced gitconfig does not carry the helper the launcher wires in:\n{gitconfig}"
    );
    assert_eq!(
        fs::read_to_string(swap.root().join("tree/.git/skein-store"))
            .unwrap()
            .trim(),
        swap.dir.join("store").to_string_lossy(),
        "the store record was not written as a file of the checkout's own"
    );
}

/// **A gitconfig linked within the box's own home is edited where it leads, and a checkout whose
/// git directory is a link outside the box gets no store record** (SKEIN-1255).
///
/// The first is the one link the launcher follows on purpose: a dotfiles setup keeps the real file
/// elsewhere in the home, and replacing the link would cut it off. The second is the store record's
/// directory check.
///
/// What makes it fail: for the dotfile, treating every link as foreign (dropping the `case` that
/// accepts a target under the home) replaces the link, and the first assertion fails; for the git
/// directory, dropping its resolution check writes `skein-store` into the decoy directory.
#[test]
fn a_dotfile_link_inside_the_home_is_followed_and_a_foreign_git_directory_is_not_written() {
    if !can_host_a_box() {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let swap = Swap::new(scratch_named("swapdot"), "swapdot-main");
    let dotfiles = swap.root().join("home/dotfiles");
    fs::create_dir_all(&dotfiles).unwrap();
    fs::write(dotfiles.join("gitconfig"), "[user]\n\tname = skein-test\n").unwrap();
    symlink("dotfiles/gitconfig", swap.root().join("home/.gitconfig")).unwrap();
    let foreign_git = swap.decoy_dir("foreign-git");
    swap.plant("tree/.git", &foreign_git);
    let foreign_before = listing(&foreign_git);

    let out = swap.start();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    swap.stop(&out);
    assert!(out.status.success(), "the box did not start: {stderr}");

    let link = swap.root().join("home/.gitconfig");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "a gitconfig linked to a file inside the box's own home was replaced rather than followed"
    );
    let edited = fs::read_to_string(dotfiles.join("gitconfig")).unwrap();
    assert!(
        edited.contains("name = skein-test") && edited.contains("git-credential-skein"),
        "the file the dotfile link leads to was not edited in place, keeping what it had:\n{edited}"
    );
    assert_eq!(
        listing(&foreign_git),
        foreign_before,
        "the store record was written into a git directory outside the box"
    );
    assert!(
        stderr.contains("not a directory of its own"),
        "the launcher skipped the store record without saying why:\n{stderr}"
    );
}

/// **A box process that outlives a restart cannot swap a directory between the launcher's check
/// and its use** (SKEIN-1287).
///
/// Nothing unshares a box's pid namespace, so a `setsid` from inside a box is still running while
/// the box restarts, and it can rename the box's `home`, `tmp`, `~/.local/state` and `bin` and put
/// links to elsewhere in their place after `own_dir` has passed them. Here the test plays that
/// process: it makes the swaps the moment the launcher's start id appears, which is after the
/// checks and before any bind, and the launcher still has python and the shims to run. Each link
/// leads to a decoy directory outside the root, standing for another box's home or the fleet's
/// state, and every decoy must come through untouched.
///
/// Two starts. In the first `home`, `tmp` and `~/.local/state` are swapped and the box must still
/// start on the directories it was checked with: the pane writes a marker into `$HOME`, `/tmp` and
/// `~/.local/state`, and they land in the moved-aside originals. In the second `bin` is swapped,
/// and the shims must land in the moved-aside `bin`. That start need not succeed — the shims' mount
/// points are under `bin` inside the box's own namespace too, and a box that breaks its own start
/// is refused nothing it was owed.
///
/// What makes it fail, named before it was written and then done: binding `home` by name again
/// (`--bind "$home" "$HOME"` in place of `--bind-fd`) mounts the decoy as HOME, so the pane's
/// marker lands in it; writing the shims into `$root/bin` rather than `$bin_at` puts them in the
/// bin decoy.
#[test]
fn a_directory_swapped_after_it_was_checked_is_not_what_the_box_gets() {
    if !can_host_a_box() {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let pane = "touch \"$HOME/skein-test-was-home\" /tmp/skein-test-was-tmp \
                \"$HOME/.local/state/skein-test-was-state\"; exec sleep 300";
    for (i, (swaps, must_start, landed)) in [
        (
            &["home", "tmp", "home.moved/.local/state"][..],
            true,
            &[
                "home.moved/skein-test-was-home",
                "tmp.moved/skein-test-was-tmp",
                "home.moved/.local/state.moved/skein-test-was-state",
            ][..],
        ),
        (&["bin"][..], false, &["bin.moved/git", "bin.moved/sudo"][..]),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("swaprace-{i}");
        let swap = Swap::new(scratch_named(&name), &name);
        let root = swap.root();
        for d in ["home/.local/state", "tmp", "bin"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        // The sandbox's own `~/.local`, as every real fleet has: the box's `~/.local` is an overlay
        // of it, so the mount point for the box's state is in that overlay rather than in the box's
        // home, where the swap leaves a link.
        fs::create_dir_all(swap.sandbox_home().join(".local/state")).unwrap();
        let decoys: Vec<(String, PathBuf)> = swaps
            .iter()
            .map(|rel| (rel.to_string(), swap.decoy_dir(&format!("race-{}", rel.replace('/', "_")))))
            .collect();
        let before: Vec<Vec<String>> = decoys.iter().map(|(_, d)| listing(d)).collect();

        let child = swap
            .launcher(pane)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("bash to run the launcher");
        let start_id = root.join("tmp/skein-start-id");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !start_id.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let swapped = start_id.exists();
        if swapped {
            for (rel, decoy) in &decoys {
                let at = root.join(rel);
                fs::rename(&at, root.join(format!("{rel}.moved"))).unwrap();
                symlink(decoy, &at).unwrap();
            }
        }
        let out = child.wait_with_output().expect("the launcher to finish");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        // The pane's markers are written once tmux has started it; give it a moment.
        if must_start {
            for _ in 0..50 {
                if landed.iter().all(|l| root.join(l).exists()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        let up = swap.running();
        swap.stop(&out);

        assert!(swapped, "the start id never appeared, so nothing was swapped:\n{stderr}");
        if must_start {
            assert!(
                out.status.success() && up,
                "with {swaps:?} swapped, the box did not start on the directories it was checked \
                 with (exit {:?}, running: {up}):\n{stderr}",
                out.status.code()
            );
        }
        for ((rel, d), was) in decoys.iter().zip(&before) {
            assert_eq!(
                &listing(d),
                was,
                "something reached the decoy behind {rel} through the link swapped in after the \
                 check:\n{stderr}"
            );
        }
        for l in landed.iter().filter(|l| must_start || have(l.rsplit('/').next().unwrap())) {
            assert!(
                root.join(l).is_file(),
                "with {swaps:?} swapped, {l} is missing: the launcher did not keep to the \
                 directory it had checked\n{stderr}"
            );
        }
    }
}

/// **A box's credentials file that is a link is not read as the box's login** (SKEIN-1303).
///
/// The login merge is the one place a box's file can reach the fleet's canonical copy: when the
/// fleet holds no login, a box's login heals it. A box that links its `.credentials.json` to some
/// other login file would, through a reader that follows links, choose the credential every box
/// started afterwards is seeded from. Here the link leads to a decoy holding a valid-looking login
/// and the fleet holds none, which is exactly the case where the box's copy would be promoted.
///
/// What makes it fail, named before it was written and then done: opening the file without
/// `O_NOFOLLOW` in both `login_life` and `merge_login` (either one alone stops it, which the test
/// below checks one at a time). The decoy's login then counts as the box's, the vacuum clause
/// copies it up, and the sandbox's `.credentials.json` appears holding the decoy's token.
#[test]
fn a_box_credentials_file_that_is_a_link_is_not_its_login() {
    if !can_host_a_box() {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let swap = Swap::new(scratch_named("swaplogin"), "swaplogin-main");
    let decoy = swap.decoy_file("login");
    let login = r#"{"claudeAiOauth":{"accessToken":"skein-test-decoy-access","refreshToken":"skein-test-decoy-refresh","expiresAt":9999999999999}}"#;
    fs::write(&decoy, login).unwrap();
    let before = decoy_state(&decoy);
    swap.plant("home/.claude/.credentials.json", &decoy);
    let canon = swap.sandbox_home().join(".claude/.credentials.json");
    assert!(!canon.exists(), "the fixture fleet must hold no login of its own");

    let out = swap.start();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    swap.stop(&out);
    assert!(out.status.success(), "the box did not start: {stderr}");

    assert!(
        !fs::read_to_string(&canon)
            .unwrap_or_default()
            .contains("skein-test-decoy"),
        "the login a box's link led to was promoted to the fleet's copy"
    );
    assert_eq!(decoy_state(&decoy), before, "the file the link led to was changed");
}

/// **Each of the two readers in the login merge refuses a link on its own** (SKEIN-1303).
///
/// The whole-launcher test above needs both to be broken before it fails, because either one alone
/// stops the promotion. This runs each by itself, lifted out of the launcher:
///
/// * `login_life` on a link to a valid login answers "no login". Fails if its open loses
///   `O_NOFOLLOW`: the decoy's login is then judged the box's.
/// * `merge_login` into a box's copy that is a link replaces the link with a file holding the
///   login, and nothing else from what the link led to. Fails if its read of the destination
///   loses `O_NOFOLLOW`: the decoy's other keys are merged in and copied into the box.
#[test]
fn the_login_merge_reads_neither_side_through_a_link() {
    use super::onboarding::launcher_block;
    let dir = scratch_named("swapmerge");
    let decoy = dir.join("decoy.json");
    fs::write(
        &decoy,
        r#"{"claudeAiOauth":{"accessToken":"skein-test-decoy","expiresAt":9999999999999},"skein-test-other":"from-the-decoy"}"#,
    )
    .unwrap();
    let canon = dir.join("canon.json");
    fs::write(
        &canon,
        r#"{"claudeAiOauth":{"accessToken":"skein-test-canon","expiresAt":9999999999999}}"#,
    )
    .unwrap();
    let mine = dir.join("mine.json");
    symlink(&decoy, &mine).unwrap();
    let run = |call: String| {
        Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -uo pipefail\n{}\n{}\n{call}",
                launcher_block("login_life() {", "}"),
                launcher_block("merge_login() {", "}"),
            ))
            .output()
            .expect("bash runs the launcher's login functions")
    };
    let q = |p: &Path| skein::util::sh_quote(p.to_string_lossy().as_ref());

    let life = run(format!("login_life {}", q(&mine)));
    assert!(
        !life.status.success(),
        "login_life read a login through a link: {}",
        String::from_utf8_lossy(&life.stdout)
    );
    assert!(
        run(format!("login_life {}", q(&decoy))).status.success(),
        "the control: the decoy read by its own name is a login"
    );

    let merged = run(format!("merge_login {} {}", q(&canon), q(&mine)));
    assert!(merged.status.success(), "merge_login failed");
    assert!(
        fs::symlink_metadata(&mine).unwrap().file_type().is_file(),
        "the box's copy is still a link after the merge"
    );
    let now = fs::read_to_string(&mine).unwrap();
    assert!(
        now.contains("skein-test-canon") && !now.contains("from-the-decoy"),
        "the merge read the far end of the box's link into the box's copy: {now}"
    );
    assert!(
        fs::read_to_string(&decoy)
            .unwrap()
            .contains("skein-test-decoy"),
        "the merge wrote through the link"
    );
}

/// **A link another box puts in the repo's shared store is not followed by this box** (SKEIN-1308).
///
/// The store is writable by every box of the repo, and its `memory/` and `shared-rw/.codex/memory`
/// are where the agents keep their memory. Here a link stands in each, leading to a file in this
/// box's own private home, as a sibling could leave it at any time. The box's pane reads and
/// appends through both, the way an agent writing a memory note would, and says what happened.
///
/// What makes it fail, named before it was written and then done: starting bwrap without the
/// `nosymfollow` mount (`box_runner=()` left empty). The reads then return the private file and
/// the appends land in it.
#[test]
fn a_link_in_the_shared_store_is_not_followed_in_the_box() {
    if !can_host_a_box() {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git, so it cannot host a box",
        );
    }
    let swap = Swap::new(scratch_named("swapstore"), "swapstore-main");
    let store = swap.dir.join("store");
    let private = swap.root().join("home/skein-test-private");
    fs::create_dir_all(private.parent().unwrap()).unwrap();
    fs::write(&private, DECOY).unwrap();
    let planted = [
        store.join("memory/note.md"),
        store.join("shared-rw/.codex/memory/note.md"),
    ];
    for link in &planted {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        // Where the box will see it: its own home is bound over `$HOME`.
        symlink(swap.sandbox_home().join("skein-test-private"), link).unwrap();
    }
    fs::write(store.join("memory/plain.md"), "a note\n").unwrap();

    let report = swap.root().join("tmp/skein-test-links");
    let q = |p: &Path| skein::util::sh_quote(p.to_string_lossy().as_ref());
    let pane = format!(
        "{{ cat {plain} && echo plain-read; \
           for l in {a} {b}; do cat \"$l\" && echo followed; echo appended >> \"$l\" || echo refused; done; \
         }} > /tmp/skein-test-links.part 2>&1; mv /tmp/skein-test-links.part /tmp/skein-test-links; exec sleep 300",
        plain = q(&store.join("memory/plain.md")),
        a = q(&planted[0]),
        b = q(&planted[1]),
    );
    let out = swap
        .launcher(&pane)
        .output()
        .expect("bash to run the launcher");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    for _ in 0..50 {
        if report.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    swap.stop(&out);
    assert!(out.status.success(), "the box did not start: {stderr}");
    let said = fs::read_to_string(&report).unwrap_or_else(|_| panic!("the pane never reported:\n{stderr}"));

    assert!(
        said.contains("plain-read"),
        "the control: a regular file in the store could not be read from the box:\n{said}"
    );
    assert!(
        !said.contains("followed") && said.matches("refused").count() == 2,
        "a link in the shared store was followed from inside the box:\n{said}\n{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&private).unwrap(),
        DECOY,
        "the box's private file was written through a link in the shared store"
    );
}
