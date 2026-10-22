//! The one way a test runs a script skein ships into a box (SKEIN-1229).
//!
//! Plain `std`, because it is compiled twice: as `testutil::box_script` inside the library's tests,
//! and `#[path]`-included by `tests/common/mod.rs` for the integration binaries, which cannot reach
//! `testutil`. One file rather than two copies, so the two crates cannot drift apart on what is
//! isolated. `tools/box-script-check.py` refuses a test that starts one of these scripts any other
//! way.
//!
//! **Why a helper and not a few `.env` calls at each site.** A script under `src/store/` or
//! `src/probe/` finds the store it writes into by itself, and the first place it looks is not the
//! environment: `src/probe/box-self.sh`'s `skein_box_store` reads `<git --git-common-dir>/skein-store`
//! for the checkout it starts in — the project directory, else the working directory. The launcher
//! writes that file into the clone a box runs in, and every lane worktree of that clone shares its
//! common git directory. So a test that left the working directory as `cargo test` set it — the
//! worktree — filed its fixture notes into the owner's LIVE mailbox, and pinning `$SKEIN_HOME` or
//! `$SKEIN_FLEET_ROOT` did nothing about it. The same inherited environment let `mailbox.sh inbox`
//! read the owner's inbox under the real `$SKEIN_STATE` and record those ids in the real
//! `$HOME/.skein-mail-seen`: messages marked delivered that nobody was shown. Each of those is a
//! variable or a directory a site has to remember, and the sites that forgot were the leak.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// What a box sets that a test must never inherit: who the box is, where its store and tmux socket
/// are, and the plugin's own directory. A test that means one of them sets it after this.
const BOX_IDENTITY: [&str; 6] = [
    "SKEIN_BOX",
    "SANDBOX_VM_ID",
    "SKEIN_STORE",
    "SKEIN_TMUX_SOCK",
    "WORKSPACE_DIR",
    "CLAUDE_PLUGIN_ROOT",
];

/// Git's own pointers, set when a test runs under a hook: with any of them inherited, every `git`
/// the script runs answers about this checkout whatever directory it is started in.
const GIT_POINTERS: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
];

/// `bash`, ready to be handed a box script (`.arg(script)`) or a hook's command (`.arg("-c")`),
/// confined to `dir` — the test's own temp directory.
///
/// * the working directory and `$CLAUDE_PROJECT_DIR` are an empty `dir/.box-script/project`, so
///   the checkout a script resolves is never the one `cargo test` runs in. Not `dir` itself:
///   `box-self.sh` refuses a store at or under the checkout, and a test's store is under `dir`;
/// * `$GIT_CEILING_DIRECTORIES` is `dir`'s parent, so a `git` started under `dir` cannot climb out
///   of it into an enclosing checkout — a `$TMPDIR` inside a worktree would otherwise put one there;
/// * `$HOME`, `$SKEIN_STATE` and `$SKEIN_FLEET_ROOT` are empty directories beside it,
///   so the seen-file, the owner's inbox and the fleet root are this test's;
/// * the box's identity and git's pointers ([`BOX_IDENTITY`], [`GIT_POINTERS`]) are removed.
///
/// A test that needs one of these to be something else — its own fixture tree as the project, its
/// own home — sets it on the returned command, and that wins: what this guarantees is that nothing
/// is ever inherited from whoever ran the suite.
pub fn box_shell(dir: &Path) -> Command {
    let isolated = dir.join(".box-script");
    let home = isolated.join("home");
    let state = isolated.join("state");
    let fleet = isolated.join("fleet");
    let project = isolated.join("project");
    for d in [&home, &state, &fleet, &project] {
        std::fs::create_dir_all(d).unwrap_or_else(|e| panic!("{}: {e}", d.display()));
    }
    let mut shell = Command::new("bash");
    shell
        .current_dir(&project)
        .env("CLAUDE_PROJECT_DIR", &project)
        .env("HOME", &home)
        .env("SKEIN_STATE", &state)
        .env("SKEIN_FLEET_ROOT", &fleet);
    if let Some(parent) = dir.parent() {
        shell.env("GIT_CEILING_DIRECTORIES", parent);
    }
    for name in BOX_IDENTITY.iter().chain(GIT_POINTERS.iter()) {
        shell.env_remove(name);
    }
    shell
}

/// The checkout a lane runs the suite from, rebuilt where a test can watch it: a clone whose git
/// directory carries a `skein-store` pointer, a worktree of that clone, and behind the pointer a
/// store standing in for the owner's live one, with an owner's inbox and seen-file beside it.
///
/// Built so that [`PointedWorktree::rerun`] can run real tests of this binary from inside it —
/// what a lane's `cargo test` did — and [`PointedWorktree::snapshot`] can say whether anything
/// behind the pointer moved. Nothing here is the real store: every path is under the `dir` given.
pub struct PointedWorktree {
    /// The worktree the suite is run from.
    pub worktree: PathBuf,
    /// The store the clone's pointer names: the stand-in for the live one.
    pub store: PathBuf,
    /// A box's `$HOME`, holding a seen-file with one id already in it.
    pub home: PathBuf,
    /// A box's `$SKEIN_STATE`, holding one unread message from the owner.
    pub state: PathBuf,
}

impl PointedWorktree {
    pub fn new(dir: &Path) -> PointedWorktree {
        let clone = dir.join("clone");
        let worktree = dir.join("worktree");
        let store = dir.join("live-store/.claude");
        let home = dir.join("live-home");
        let state = dir.join("live-state");
        // A store is a directory holding `skein/` (`_skein_is_store` in box-self.sh), and this one
        // already has mail in it: a broadcast nobody has read, which an `inbox` run against it
        // would mark seen.
        std::fs::create_dir_all(store.join("skein/bin")).unwrap();
        std::fs::create_dir_all(store.join("mailbox")).unwrap();
        std::fs::write(
            store.join("mailbox/1-owner-box.json"),
            r#"{"from":"another-box","to":"broadcast","kind":"note","branch":"main","body":"a real note","ts":"2026-01-01T00:00:00Z","seenBy":[],"relayedTo":[],"originProject":""}"#,
        )
        .unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".skein-mail-seen"), "0-already-shown.json\n").unwrap();
        std::fs::create_dir_all(state.join("inbox")).unwrap();
        std::fs::write(
            state.join("inbox/2-from-the-owner.json"),
            r#"{"from":"skein","to":"broadcast","kind":"note","branch":"","body":"the owner's unread message","ts":"2026-01-01T00:00:00Z","seenBy":[]}"#,
        )
        .unwrap();

        std::fs::create_dir_all(&clone).unwrap();
        git(dir, &["init", "-q", "clone"]);
        git(&clone, &["commit", "-q", "--allow-empty", "-m", "fixture"]);
        git(
            &clone,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
            ],
        );
        // What the launcher writes into a box's clone, and what every worktree of it then reads
        // through `git rev-parse --git-common-dir`.
        std::fs::write(
            clone.join(".git/skein-store"),
            format!("{}\n", store.display()),
        )
        .unwrap();
        PointedWorktree {
            worktree,
            store,
            home,
            state,
        }
    }

    /// The store `box-self.sh` resolves for a script started in the worktree with nothing pinned —
    /// the control that makes an untouched store mean something: the pointer is live, so a test
    /// that inherited the worktree would have found it.
    pub fn resolved_from_the_worktree(&self, box_self: &Path) -> String {
        let out = Command::new("bash")
            .arg("-c")
            .arg(r#". "$1" && skein_box_store "$2""#)
            .arg("box-self")
            .arg(box_self)
            .arg(&self.worktree)
            .current_dir(&self.worktree)
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .output()
            .expect("bash");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Run `tests` — exact names in this test binary — from the worktree, with a box's `$HOME`,
    /// `$SKEIN_STATE`, `$CLAUDE_PROJECT_DIR` and `$SKEIN_BOX` all pointing at the stand-ins: the
    /// environment a lane inherits. One at a time, so a failure reads in order.
    pub fn rerun(&self, tests: &[&str]) -> Output {
        let me = std::env::current_exe().expect("this test binary");
        Command::new(me)
            .arg("--exact")
            .args(tests)
            .arg("--test-threads=1")
            .current_dir(&self.worktree)
            .env("CLAUDE_PROJECT_DIR", &self.worktree)
            .env("HOME", &self.home)
            .env("SKEIN_STATE", &self.state)
            .env("SKEIN_BOX", "the-live-box")
            .output()
            .expect("re-run this test binary")
    }

    /// Every file behind the pointer, the seen-file and the owner's inbox, with its bytes.
    pub fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        for root in [&self.store, &self.home, &self.state] {
            walk(root, &mut out);
        }
        out
    }
}

fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// What changed between two [`PointedWorktree::snapshot`]s, one line per path; empty when nothing.
pub fn moved(
    before: &BTreeMap<PathBuf, Vec<u8>>,
    after: &BTreeMap<PathBuf, Vec<u8>>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (path, bytes) in after {
        match before.get(path) {
            None => out.push(format!(
                "written: {} — {}",
                path.display(),
                String::from_utf8_lossy(bytes)
            )),
            Some(was) if was != bytes => out.push(format!(
                "changed: {} — now {}",
                path.display(),
                String::from_utf8_lossy(bytes)
            )),
            _ => {}
        }
    }
    for path in before.keys().filter(|p| !after.contains_key(*p)) {
        out.push(format!("removed: {}", path.display()));
    }
    out
}
