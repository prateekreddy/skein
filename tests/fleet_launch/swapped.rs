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
        let launcher = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/box-session.sh");
        Command::new("bash")
            .arg(&launcher)
            .arg(&self.name)
            .arg(self.root())
            .arg(self.dir.join("pid").join(&self.name))
            .arg("skein-shell")
            .arg(self.dir.join("state").join(&self.name))
            .arg("")
            .args(["bash", "-lc", "exec sleep 300"])
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
            .env("SKEIN_BOX_STORE", self.dir.join("store"))
            .output()
            .expect("bash to run the launcher")
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
