//! The isolation cover, proved by running bwrap rather than by reading its arguments.
//!
//! Every other test of `box-session.sh`'s isolation block inspects the argv it builds: does
//! `--tmpfs <path>` appear, does `--bind <store>` appear, does this tmpfs land after that bind.
//! That checks *skein asked for the right mounts*. What matters is *a box cannot reach the other
//! things*, and the gap between the two is exactly where a wrong flag, a wrong order, or a bwrap
//! behaviour nobody predicted lives. The ordering rule especially: it is a hand-written comparison
//! of positions in a list, and bwrap's own resolution is the authority on what that list means.
//!
//! So this one builds a fleet-shaped directory tree, generates the binds the way the launcher does,
//! runs **bwrap** with them, and asks the process inside what it can see.
//!
//! It **skips** rather than fails where bwrap cannot make a user namespace — an unprivileged
//! container without `CAP_SYS_ADMIN`, a kernel with `unprivileged_userns_clone` off. A test that
//! cannot run must not be a red build, and it must not be a silently green one either: the skip
//! says which check did not happen.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(name)
}

/// Is bwrap usable here at all? The cheapest possible namespace, and if that fails nothing below
/// can run.
fn bwrap_works() -> bool {
    Command::new("bwrap")
        .args(["--dev-bind", "/", "/", "--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The launcher's isolation block, lifted out of the script it lives in.
///
/// Read from `box-session.sh` rather than copied, so a change to the launcher is a change to what
/// this test runs — a copy would keep passing against the version it was written from.
fn isolation_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.starts_with(r#"if [ "${SKEIN_BOX_PRIVILEGED-}" != "1" ]; then"#))
        .expect("the isolation block moved");
    let to = lines[from..]
        .iter()
        .position(|l| *l == "fi")
        .map(|i| from + i)
        .expect("the isolation block has no end");
    lines[from..=to].join("\n")
}

/// A fleet-shaped tree: two repos, two boxes, and one repo's store kept outside the workspace.
struct Fleet {
    dir: PathBuf,
    /// `$SKEIN_FLEET_ROOT` — the sandbox-local directory holding every box's checkout.
    fleet_root: PathBuf,
    /// The host directory holding every box's durable state.
    state_parent: PathBuf,
    /// The workspace mount, holding both repos' stores.
    repos: PathBuf,
    /// A second mount, where somebody keeps a repo skein did not choose the location of.
    elsewhere: PathBuf,
}

impl Drop for Fleet {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Fleet {
    fn make(tag: &str) -> Fleet {
        let dir = std::env::temp_dir().join(format!("skein-bwrap-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let f = Fleet {
            fleet_root: dir.join("boxes-vm"),
            state_parent: dir.join("state"),
            repos: dir.join("repos"),
            elsewhere: dir.join("home-code-thing"),
            dir,
        };
        for p in [
            f.fleet_root.join(".skein"),
            f.fleet_root.join("web-main/tree"),
            f.fleet_root.join("other-main/tree"),
            f.state_parent.join("web-main"),
            f.state_parent.join("other-main"),
            f.repos.join("web/store/.claude/memory"),
            f.repos.join("other/store/.claude/memory"),
            f.elsewhere.join("store/.claude"),
        ] {
            fs::create_dir_all(&p).unwrap();
        }
        // Something identifiable in each, so "can see it" means "read its contents", not "the
        // directory entry exists".
        fs::write(f.fleet_root.join(".skein/box-session.sh"), "launcher\n").unwrap();
        fs::write(f.repos.join("web/store/.claude/memory/mine.md"), "mine\n").unwrap();
        fs::write(
            f.repos.join("other/store/.claude/memory/theirs.md"),
            "theirs\n",
        )
        .unwrap();
        fs::write(
            f.elsewhere.join("store/.claude/notes.md"),
            "somebody else\n",
        )
        .unwrap();
        fs::write(f.state_parent.join("web-main/conversation.jsonl"), "{}\n").unwrap();
        fs::write(f.state_parent.join("other-main/conversation.jsonl"), "{}\n").unwrap();
        f
    }

    fn mounts(&self) -> String {
        format!("{}\n{}\n", self.repos.display(), self.elsewhere.display())
    }

    fn store(&self) -> PathBuf {
        self.repos.join("web/store/.claude")
    }

    /// What a box of `web-main` can actually reach, once bwrap has applied the launcher's binds.
    ///
    /// One line per path: `see`, `write`, `blind` (there, unreadable) or `gone`.
    fn seen_by_box(&self, privileged: bool) -> String {
        let block = isolation_block();
        // The probe runs INSIDE the namespace. `see` means the contents came back, not that the
        // name resolved: a tmpfs leaves an empty directory where a full one was, and "it exists"
        // would call that a pass.
        let probe = r#"
for p in "$@"; do
  if [ ! -e "$p" ]; then echo "gone $p"; continue; fi
  if [ -d "$p" ]; then
    ls -A "$p" >/dev/null 2>&1 || { echo "blind $p"; continue; }
    [ -z "$(ls -A "$p" 2>/dev/null)" ] && { echo "empty $p"; continue; }
  else
    cat "$p" >/dev/null 2>&1 || { echo "blind $p"; continue; }
  fi
  if [ -d "$p" ] && touch "$p/.skein-write-probe" 2>/dev/null; then
    rm -f "$p/.skein-write-probe"; echo "write $p"
  else
    echo "see $p"
  fi
done
"#;
        let paths = [
            self.store(),
            self.repos.join("other/store/.claude"),
            self.elsewhere.join("store/.claude"),
            self.fleet_root.join("web-main"),
            self.fleet_root.join("other-main"),
            self.fleet_root.join(".skein"),
            self.state_parent.join("web-main"),
            self.state_parent.join("other-main"),
        ];
        let quoted: Vec<String> = paths
            .iter()
            .map(|p| skein::util::sh_quote(p.to_string_lossy().as_ref()))
            .collect();

        let runner = format!(
            "set -uo pipefail\n\
             binds=()\n\
             root={root}\n\
             state={state}\n\
             export SKEIN_FLEET_ROOT={fleet} SKEIN_BOX_PRIVILEGED={priv} \
             SKEIN_FLEET_MOUNTS={mounts} SKEIN_BOX_STORE={store}\n\
             {block}\n\
             exec bwrap --dev-bind / / ${{binds[@]+\"${{binds[@]}}\"}} -- /bin/sh -c {probe} skein-probe {paths}\n",
            root = skein::util::sh_quote(self.fleet_root.join("web-main").to_string_lossy().as_ref()),
            state = skein::util::sh_quote(self.state_parent.join("web-main").to_string_lossy().as_ref()),
            fleet = skein::util::sh_quote(self.fleet_root.to_string_lossy().as_ref()),
            priv = if privileged { "1" } else { "0" },
            mounts = skein::util::sh_quote(&self.mounts()),
            store = skein::util::sh_quote(self.store().to_string_lossy().as_ref()),
            probe = skein::util::sh_quote(probe),
            paths = quoted.join(" "),
        );

        let out = Command::new("bash")
            .arg("-c")
            .arg(&runner)
            .output()
            .expect("bash");
        assert!(
            out.status.success(),
            "the namespace could not be built: {}\n--- script ---\n{runner}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

/// What the report says about one path.
fn verdict<'a>(report: &'a str, path: &Path) -> &'a str {
    let want = path.to_string_lossy();
    report
        .lines()
        .find(|l| l.split_once(' ').map(|(_, p)| p) == Some(want.as_ref()))
        .and_then(|l| l.split_once(' '))
        .map(|(v, _)| v)
        .unwrap_or_else(|| panic!("the probe said nothing about {}:\n{report}", path.display()))
}

/// An ordinary box reaches its own repo and its own state, and nothing else the sandbox mounts.
#[test]
fn a_box_run_under_bwrap_can_reach_its_own_repo_and_no_one_elses() {
    if !bwrap_works() {
        eprintln!(
            "SKIPPED a_box_run_under_bwrap_can_reach_its_own_repo_and_no_one_elses: bwrap cannot \
             create a user namespace here, so the isolation cover was NOT exercised against a real \
             namespace on this machine"
        );
        return;
    }
    let fleet = Fleet::make("ordinary");
    let report = fleet.seen_by_box(false);

    // Its own, and read-write: the store is where its memory and mailbox are written.
    assert_eq!(
        verdict(&report, &fleet.store()),
        "write",
        "a box lost its own repo's store:\n{report}"
    );
    assert_eq!(
        verdict(&report, &fleet.state_parent.join("web-main")),
        "write",
        "a box lost the state its conversation lives in:\n{report}"
    );
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join("web-main")),
        "write",
        "a box lost its own checkout:\n{report}"
    );

    // The launcher and the credential helper, readable and not writable — a box that could rewrite
    // box-session.sh would choose its own next namespace.
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join(".skein")),
        "see",
        "the fleet's own scripts are writable by a box, or unreachable:\n{report}"
    );

    // And everything that is somebody else's.
    for (path, what) in [
        (
            fleet.repos.join("other/store/.claude"),
            "another repo's store",
        ),
        (
            fleet.elsewhere.join("store/.claude"),
            "a store outside the workspace",
        ),
        (
            fleet.fleet_root.join("other-main"),
            "another box's checkout",
        ),
        (
            fleet.state_parent.join("other-main"),
            "another box's conversation",
        ),
    ] {
        assert!(
            matches!(verdict(&report, &path), "gone" | "empty"),
            "{what} is reachable from a box:\n{report}"
        );
    }
}

/// The workshop box sees the fleet, which is the whole of what the flag is for.
///
/// Worth a test of its own: a cover that never lifts and a cover that never applies both pass a
/// one-sided check, and they are opposite bugs.
#[test]
fn the_workshop_box_sees_what_an_ordinary_box_cannot() {
    if !bwrap_works() {
        eprintln!(
            "SKIPPED the_workshop_box_sees_what_an_ordinary_box_cannot: bwrap cannot create a user \
             namespace here"
        );
        return;
    }
    let fleet = Fleet::make("workshop");
    let report = fleet.seen_by_box(true);
    for path in [
        fleet.repos.join("other/store/.claude"),
        fleet.elsewhere.join("store/.claude"),
        fleet.fleet_root.join("other-main"),
        fleet.state_parent.join("other-main"),
    ] {
        assert_eq!(
            verdict(&report, &path),
            "write",
            "the workshop box cannot reach {}, so the flag does nothing:\n{report}",
            path.display()
        );
    }
}
