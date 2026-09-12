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

mod common;

use common::{bwrap_works, skip, Scratch};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(name)
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
    dir: Scratch,
    /// `$SKEIN_FLEET_ROOT` — the sandbox-local directory holding every box's checkout.
    fleet_root: PathBuf,
    /// The host directory holding every box's durable state.
    state_parent: PathBuf,
    /// The workspace mount, holding both repos' stores.
    repos: PathBuf,
    /// A second mount, where somebody keeps a repo skein did not choose the location of.
    elsewhere: PathBuf,
    /// The volume the fleet's own state lives on, when this fleet is the 4c shape: a mount that
    /// CONTAINS what a box owns, rather than sitting beside it. `None` is the 4a shape.
    volume: Option<PathBuf>,
}

impl Fleet {
    fn make(tag: &str) -> Fleet {
        Fleet::build(tag, false)
    }

    /// The fleet skein-server runs inside (delivery §3 4c): box state lives on the VOLUME the
    /// server is given, beside the fleet's credentials — so the path the sandbox mounts is an
    /// ancestor of the two directories a box owns, which is the case the cover used to skip.
    fn make_on_volume(tag: &str) -> Fleet {
        Fleet::build(tag, true)
    }

    fn build(tag: &str, on_volume: bool) -> Fleet {
        let dir = Scratch::temp(&format!("skein-bwrap-{tag}"));
        let volume = on_volume.then(|| dir.join("volume"));
        let f = Fleet {
            fleet_root: dir.join("boxes-vm"),
            state_parent: match &volume {
                Some(v) => v.join("boxes"),
                None => dir.join("state"),
            },
            repos: dir.join("repos"),
            elsewhere: dir.join("home-code-thing"),
            volume,
            dir,
        };
        for p in [
            f.fleet_root.join(".skein"),
            // Where the sandbox keeps what it builds skein FROM (SKEIN-448). Under `.skein`
            // deliberately: the launcher binds that directory back read-only, so a box can read
            // the compiler and never replace it.
            f.fleet_root.join(".skein/src"),
            f.fleet_root.join(".skein/toolchain/cargo/bin"),
            f.fleet_root.join("web-main/tree"),
            f.fleet_root.join("other-main/tree"),
            f.state_parent.join("web-main"),
            f.state_parent.join("web-main/claude-projects"),
            f.state_parent.join("web-main/git-tokens"),
            f.state_parent.join("other-main"),
            f.dir.join("boxhome/.claude/projects"),
            f.repos.join("web/store/.claude/memory"),
            f.repos.join("other/store/.claude/memory"),
            f.elsewhere.join("store/.claude"),
        ] {
            fs::create_dir_all(&p).unwrap();
        }
        // Something identifiable in each, so "can see it" means "read its contents", not "the
        // directory entry exists".
        fs::write(f.fleet_root.join(".skein/box-session.sh"), "launcher\n").unwrap();
        fs::write(f.fleet_root.join(".skein/src/Cargo.toml"), "[package]\n").unwrap();
        // The compiler itself. If a box can write this file it can choose what the fleet's own
        // server is built from, which is architecture §9.2's rule with the stakes at their highest.
        fs::write(
            f.fleet_root.join(".skein/toolchain/cargo/bin/cargo"),
            "#!/bin/sh\n",
        )
        .unwrap();
        // Everything skein authenticates with inside the sandbox, in the one directory the
        // launcher covers (SKEIN-516 Rule 1). Real bytes in each, so "cannot read it" is a claim
        // about the cover and not about an empty fixture — and the workshop assertions below read
        // exactly these back to prove that.
        fs::create_dir_all(f.fleet_root.join(".skein/private")).unwrap();
        fs::write(
            f.fleet_root.join(".skein/private/fleet-agent.token"),
            "f".repeat(64),
        )
        .unwrap();
        fs::write(
            f.fleet_root.join(".skein/private/review-github.token"),
            "ghp_review\n",
        )
        .unwrap();
        // The two request queues, one drop-box per box. A box may write its OWN and no other's,
        // which is architecture §8.4's per-box request path — the step that was skipped when the
        // queues were unmasked, leaving every box a writable path to every other box's pending
        // requests and a way to file one in a neighbour's name (ISO-7). A file in each, because
        // the probe reports an empty directory as `empty` and never gets as far as writing to it.
        for queue in ["substrate", "gitgate"] {
            for owner in ["web-main", "other-main"] {
                let drop = f
                    .fleet_root
                    .join(format!(".skein/{queue}/requests/{owner}"));
                fs::create_dir_all(&drop).unwrap();
                fs::write(drop.join("20260101-000000-1.json"), "{}\n").unwrap();
            }
        }
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
        fs::write(
            f.state_parent
                .join("web-main/claude-projects/session.jsonl"),
            "{}\n",
        )
        .unwrap();
        // The artifact: a token the HOST minted and placed, which this box reads to push.
        fs::write(
            f.state_parent.join("web-main/git-tokens/owner%2Frepo"),
            "ghs_scoped\n",
        )
        .unwrap();
        fs::write(f.state_parent.join("other-main/conversation.jsonl"), "{}\n").unwrap();
        // What the volume holds beside the boxes: everything the fleet authenticates with. These
        // are the bytes the cover exists for — a box that can read `credentials/` is the fleet.
        if let Some(volume) = &f.volume {
            fs::create_dir_all(volume.join("credentials")).unwrap();
            fs::create_dir_all(volume.join("github-pats")).unwrap();
            fs::write(
                volume.join("credentials/claude.json"),
                "{\"token\":\"live\"}\n",
            )
            .unwrap();
            fs::write(volume.join("github-pats/acme"), "ghp_live\n").unwrap();
            fs::write(volume.join("api-token"), "t".repeat(64)).unwrap();
            // The warden's shared secret. `warden/secret.rs` says the bind may only widen because
            // "a file in a place no box's mount view reaches is a thing skein can have and a box
            // cannot" — so that claim is asserted here rather than inferred from the cover being
            // ordering-based. Whoever widens the bind is trusting this line.
            fs::create_dir_all(volume.join("warden")).unwrap();
            fs::write(volume.join("warden/secret"), "s".repeat(32)).unwrap();
        }
        f
    }

    fn mounts(&self) -> String {
        let mut out = format!("{}\n{}\n", self.repos.display(), self.elsewhere.display());
        if let Some(volume) = &self.volume {
            out.push_str(&format!("{}\n", volume.display()));
        }
        out
    }

    fn store(&self) -> PathBuf {
        self.repos.join("web/store/.claude")
    }

    /// Try to `connect()` to a unix socket from inside the namespace, and say what happened.
    ///
    /// **A `connect()` and not a `stat`, because they are different questions and only one of them
    /// is the boundary.** A read-only bind mount refuses a write to a regular file and refuses
    /// nothing at all to a socket: the kernel's `sb_permission` returns `EROFS` for regular files,
    /// directories and symlinks, and a socket is none of those. So a cover that made `.skein`
    /// read-only would leave every socket in it reachable from every box, which is exactly what
    /// `server.tmux` was until SKEIN-529 moved it under the tmpfs — see
    /// [`a_box_cannot_connect_to_the_fleets_tmux_socket`]. Asking the kernel to connect is the only
    /// check that tells the two apart.
    ///
    /// Returns `connected`, or `refused <errno name>`.
    fn connect_from_box(&self, privileged: bool, sock: &Path) -> String {
        // python3 rather than a shell: `sh` has no way to open a unix socket, and the whole point
        // is to make the syscall the kernel decides rather than to look at a directory listing.
        let probe = "import socket,sys\n\
                     s=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
                     try:\n\
                     \x20   s.connect(sys.argv[1]); print('connected')\n\
                     except OSError as e:\n\
                     \x20   print('refused', e.__class__.__name__)\n";
        let out = self.in_box(
            privileged,
            // `python3 -c CODE ARG` makes `ARG` `sys.argv[1]`; a placeholder between them would
            // shift the socket path out from under the probe, which is how the first run of this
            // test reported the WORKSHOP box unable to reach a socket that was listening.
            "exec python3 -c \"$1\" \"$2\"",
            &[probe.to_string(), sock.to_string_lossy().into_owned()],
        );
        String::from_utf8_lossy(&out).trim().to_string()
    }

    /// Run one `/bin/sh -c` probe inside the namespace the launcher's isolation block builds, and
    /// return its stdout. `args` become `$1`, `$2`, … inside it.
    fn in_box(&self, privileged: bool, probe: &str, args: &[String]) -> Vec<u8> {
        let block = isolation_block();
        let quoted: Vec<String> = args.iter().map(|a| skein::util::sh_quote(a)).collect();
        let record_bind = format!(
            "--bind {} {}",
            skein::util::sh_quote(
                self.state_parent
                    .join("web-main/claude-projects")
                    .to_string_lossy()
                    .as_ref()
            ),
            skein::util::sh_quote(
                self.dir
                    .join("boxhome/.claude/projects")
                    .to_string_lossy()
                    .as_ref()
            ),
        );
        let runner = format!(
            "set -uo pipefail\n\
             binds=({record})\n\
             box=web-main\n\
             root={root}\n\
             state={state}\n\
             export SKEIN_FLEET_ROOT={fleet} SKEIN_BOX_PRIVILEGED={priv} \
             SKEIN_FLEET_MOUNTS={mounts} SKEIN_BOX_STORE={store}\n\
             {block}\n\
             exec bwrap --dev-bind / / ${{binds[@]+\"${{binds[@]}}\"}} -- /bin/sh -c {probe} skein-probe {args}\n",
            record = record_bind,
            root = skein::util::sh_quote(self.fleet_root.join("web-main").to_string_lossy().as_ref()),
            state = skein::util::sh_quote(self.state_parent.join("web-main").to_string_lossy().as_ref()),
            fleet = skein::util::sh_quote(self.fleet_root.to_string_lossy().as_ref()),
            priv = if privileged { "1" } else { "0" },
            mounts = skein::util::sh_quote(&self.mounts()),
            store = skein::util::sh_quote(self.store().to_string_lossy().as_ref()),
            probe = skein::util::sh_quote(probe),
            args = quoted.join(" "),
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
        out.stdout
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
        let mut paths = vec![
            self.state_parent.join("web-main/git-tokens/owner%2Frepo"),
            self.dir.join("boxhome/.claude/projects"),
            self.store(),
            self.repos.join("other/store/.claude"),
            self.elsewhere.join("store/.claude"),
            self.fleet_root.join("web-main"),
            self.fleet_root.join("other-main"),
            self.fleet_root.join(".skein"),
            self.fleet_root.join(".skein/src"),
            self.fleet_root.join(".skein/toolchain/cargo/bin"),
            self.fleet_root.join(".skein/private"),
            self.fleet_root.join(".skein/private/fleet-agent.token"),
            self.fleet_root.join(".skein/private/review-github.token"),
            self.state_parent.join("web-main"),
            self.state_parent.join("other-main"),
            self.fleet_root.join(".skein/substrate/requests"),
            self.fleet_root.join(".skein/substrate/requests/web-main"),
            self.fleet_root.join(".skein/substrate/requests/other-main"),
            self.fleet_root.join(".skein/gitgate/requests"),
            self.fleet_root.join(".skein/gitgate/requests/web-main"),
            self.fleet_root.join(".skein/gitgate/requests/other-main"),
        ];
        if let Some(volume) = &self.volume {
            paths.extend([
                volume.join("credentials/claude.json"),
                volume.join("github-pats/acme"),
                volume.join("api-token"),
                volume.join("warden/secret"),
            ]);
        }
        let quoted: Vec<String> = paths
            .iter()
            .map(|p| skein::util::sh_quote(p.to_string_lossy().as_ref()))
            .collect();

        let record_bind = format!(
            "--bind {} {}",
            skein::util::sh_quote(
                self.state_parent
                    .join("web-main/claude-projects")
                    .to_string_lossy()
                    .as_ref()
            ),
            skein::util::sh_quote(
                self.dir
                    .join("boxhome/.claude/projects")
                    .to_string_lossy()
                    .as_ref()
            ),
        );
        let runner = format!(
            "set -uo pipefail\n\
             binds=({record})\n\
             box=web-main\n\
             root={root}\n\
             state={state}\n\
             export SKEIN_FLEET_ROOT={fleet} SKEIN_BOX_PRIVILEGED={priv} \
             SKEIN_FLEET_MOUNTS={mounts} SKEIN_BOX_STORE={store}\n\
             {block}\n\
             exec bwrap --dev-bind / / ${{binds[@]+\"${{binds[@]}}\"}} -- /bin/sh -c {probe} skein-probe {paths}\n",
            // What the launcher has already put in `binds` by the time the isolation block runs:
            // the box's conversation, bound READ-WRITE at the `$HOME` path the agent writes it
            // through. Reproduced here because the block's read-only cover of the state directory
            // is only correct if this bind exists — testing the cover without it would assert that
            // a box cannot write its own conversation, which would be a bug rather than a property.
            record = record_bind,
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

/// A fleet whose state lives on a mounted VOLUME still covers it — and still leaves the box the
/// two directories it owns (SKEIN-219).
///
/// The 4a cover skipped any mount that was an ancestor of what the box owns, because a tmpfs over
/// `~/.skein` written after the binds of `~/.skein/boxes/<box>` would throw them away. The volume
/// skein-server-in-fleet is given is exactly that ancestor, so from every box on such a fleet
/// `credentials/`, `api-token` and `github-pats/` were a `cat` away. Ordering, not enumeration, is
/// the fix: the ancestor is covered BEFORE the entitlements are bound back.
#[test]
fn a_box_on_a_mounted_volume_cannot_read_the_fleets_credentials() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot \
             create a user namespace here, so the volume cover was NOT exercised against a real \
             namespace on this machine",
        );
    }
    let fleet = Fleet::make_on_volume("volume");
    let volume = fleet.volume.clone().expect("this fleet is on a volume");
    let report = fleet.seen_by_box(false);

    for secret in [
        volume.join("credentials/claude.json"),
        volume.join("github-pats/acme"),
        volume.join("api-token"),
        // The warden's shared secret, which is what will authenticate skein once the bind widens.
        volume.join("warden/secret"),
    ] {
        assert_eq!(
            verdict(&report, &secret),
            "gone",
            "a box on a volume-mounted fleet can read {} — that credential IS the fleet:\n{report}",
            secret.display()
        );
    }
    // **The "gone" assertions above are only worth having if the files were there to hide.** A
    // fixture that failed to write one would report "gone" for a path that never existed, and the
    // loop would pass while proving nothing — the exact shape of a test that cannot fail. So the
    // same volume is read again by a WORKSHOP box, which `box-session.sh:1109` deliberately exempts
    // from the cover: it must see the secret the ordinary box could not.
    let workshop = fleet.seen_by_box(true);
    assert_ne!(
        verdict(&workshop, &volume.join("warden/secret")),
        "gone",
        "the workshop box cannot see the warden secret either, so the cover is not what hid it \
         from the ordinary box and the assertion above proves nothing:\n{workshop}"
    );

    // …and the box still has everything it is entitled to, which is the half a blunt tmpfs breaks.
    assert_eq!(
        verdict(&report, &fleet.store()),
        "write",
        "covering the volume took the box's own store with it:\n{report}"
    );
    assert_eq!(
        verdict(&report, &fleet.state_parent.join("web-main")),
        "see",
        "covering the volume took the box's own state with it:\n{report}"
    );
    assert_eq!(
        verdict(
            &report,
            &fleet.state_parent.join("web-main/git-tokens/owner%2Frepo")
        ),
        "see",
        "covering the volume took the git token the host placed for this box:\n{report}"
    );
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join("web-main")),
        "write",
        "covering the volume took the box's own checkout with it:\n{report}"
    );
    // The neighbour is still gone, so this is a cover rather than an accident of layout.
    assert_eq!(
        verdict(&report, &fleet.state_parent.join("other-main")),
        "gone",
        "another box's state is reachable on a volume fleet:\n{report}"
    );
}

/// A box can read what skein is built from and cannot write any of it (SKEIN-448).
///
/// The sandbox builds its own server now, which means the compiler and the source live inside the
/// fleet — and a compiler every box can overwrite is a worse position than the host build it
/// replaces, because the thing it compiles holds `credentials/`, `github-pats/` and the API token.
/// Architecture §9.2 states the rule: **no shared writable path may contain anything another box
/// executes.**
///
/// Asserted against real bwrap rather than against the placement, because "it is under `.skein`"
/// is a claim about a string and this is a claim about a namespace. The unit test
/// `fleet::nothing_the_sandbox_builds_skein_with_is_writable_by_a_box` covers the placement; this
/// covers whether the launcher actually delivers it.
///
/// `see` and not `write` is the whole assertion. `gone` would be a failure too, and a different
/// one: the sandbox has to be able to run what it built.
#[test]
fn a_box_can_read_what_skein_was_built_from_and_cannot_write_it() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot \
             create a user namespace here, so the toolchain cover was NOT exercised against a real \
             namespace on this machine",
        );
    }
    let fleet = Fleet::make("toolchain");
    let report = fleet.seen_by_box(false);

    for path in [
        fleet.fleet_root.join(".skein/src"),
        fleet.fleet_root.join(".skein/toolchain/cargo/bin"),
    ] {
        assert_eq!(
            verdict(&report, &path),
            "see",
            "a box can WRITE {} — it can choose what the fleet's own server is built from, and \
             the server holds every credential the fleet has (architecture §9.2):\n{report}",
            path.display()
        );
    }
}

/// An ordinary box reaches its own repo and its own state, and nothing else the sandbox mounts.
#[test]
fn a_box_run_under_bwrap_can_reach_its_own_repo_and_no_one_elses() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot \
             create a user namespace here, so the isolation cover was NOT exercised against a real \
             namespace on this machine",
        );
    }
    let fleet = Fleet::make("ordinary");
    let report = fleet.seen_by_box(false);

    // Its own, and read-write: the store is where its memory and mailbox are written.
    assert_eq!(
        verdict(&report, &fleet.store()),
        "write",
        "a box lost its own repo's store:\n{report}"
    );
    // Its own state, READ-ONLY. §5 divides this directory by who writes what: the conversation is
    // the box's and comes back writable through a separate bind at `$HOME`; the git token is the
    // host's, minted and placed for this box, and read here.
    assert_eq!(
        verdict(&report, &fleet.state_parent.join("web-main")),
        "see",
        "the box's own state directory is writable, so it can rewrite what the host placed there:\n{report}"
    );
    // The trap this item exists to avoid: bind only the conversation and every box silently loses
    // git push, because the credential helper reads its token out of this path.
    assert_eq!(
        verdict(
            &report,
            &fleet.state_parent.join("web-main/git-tokens/owner%2Frepo")
        ),
        "see",
        "a box cannot read the git token the host placed for it, so it cannot push:\n{report}"
    );
    // And the other half of the same rule: the conversation IS writable, through the bind the
    // launcher makes at `$HOME`. Read-only state that also took this away would be a bug wearing a
    // security argument.
    assert_eq!(
        verdict(&report, &fleet.dir.join("boxhome/.claude/projects")),
        "write",
        "a box cannot write its own conversation:\n{report}"
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
        return skip(
            "bwrap cannot create a user \
             namespace here",
        );
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

/// A box cannot read what skein keeps under `.skein/private/` (SKEIN-516 Rule 1, ISO-2, ISO-4).
///
/// `.skein` is bound back into every box READABLE — the launcher, the credential helper and the
/// toolchain live there and a box needs all three. Everything skein authenticates with lived there
/// too, and one of the four was covered: an empty file was bound over `fleet-agent.token`, by name.
/// The review call's GitHub credential was written into the same directory "beside the fleet
/// agent's token and for the same reason" and got the path without the cover, so every box could
/// `cat` the token the review queue acts on GitHub with as the person who owns the fleet.
///
/// The fix is a directory rather than a list, so the next secret is covered by having been put in
/// the right place rather than by somebody remembering to add a line here.
///
/// **What would make this fail**: deleting the `--tmpfs "$private"` line from the isolation block
/// in `box-session.sh`. Both files then read back through the `--ro-bind` of `.skein` as `see`.
#[test]
fn a_box_cannot_read_what_skein_keeps_under_private() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user \
             namespace here, so the private cover was NOT exercised against a real namespace on \
             this machine",
        );
    }
    let fleet = Fleet::make("private");
    let report = fleet.seen_by_box(false);

    for secret in [
        fleet.fleet_root.join(".skein/private/fleet-agent.token"),
        fleet.fleet_root.join(".skein/private/review-github.token"),
    ] {
        assert_eq!(
            verdict(&report, &secret),
            "gone",
            "a box can read {} — that credential runs commands as the sandbox, or acts on GitHub \
             as the person who owns the fleet:\n{report}",
            secret.display()
        );
    }
    // The directory is still *there* and still empty, which is what a tmpfs looks like from inside
    // and is the difference between covering it and deleting it: skein writes here at fleet scope
    // on every review call, and a box writing into its own tmpfs copy reaches nobody.
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join(".skein/private")),
        "empty",
        "the private directory is not a tmpfs from inside the box:\n{report}"
    );
    // The rest of `.skein` is untouched: this is a cover over one directory, not over the launcher
    // and the toolchain a box has to be able to read.
    assert_eq!(
        verdict(&report, &fleet.fleet_root.join(".skein")),
        "see",
        "covering `private/` took the fleet's own scripts with it:\n{report}"
    );

    // **The "gone" assertions are only worth having if the files were there to hide.** A fixture
    // that failed to write one would report "gone" for a path that never existed. The workshop box
    // skips the cover deliberately, so it must read back exactly what the ordinary box could not.
    let workshop = fleet.seen_by_box(true);
    for secret in [
        fleet.fleet_root.join(".skein/private/fleet-agent.token"),
        fleet.fleet_root.join(".skein/private/review-github.token"),
    ] {
        assert_eq!(
            verdict(&workshop, &secret),
            "see",
            "the workshop box cannot see {} either, so the cover is not what hid it from the \
             ordinary box and the assertions above prove nothing:\n{workshop}",
            secret.display()
        );
    }
}

/// A box cannot `connect()` to the fleet agent's socket (ISO-4).
///
/// The agent used to bind `0.0.0.0:8317` and be restarted by a `sleep 2` loop, in a network and pid
/// namespace every box shares at the same uid — so a box could kill it, take the port in the gap,
/// and be handed the fleet agent's token by skein's next call, which sends `X-Skein-Token` before
/// it has learned anything about who answered. That token runs commands as the sandbox in any
/// box's namespace.
///
/// The socket is under the `private/` cover, so this asks the question the cover is answering: not
/// "is the file listed" but "does the kernel let this process connect". They are different
/// questions — a read-only mount refuses neither `connect()` nor `bind()` — and only the second one
/// is the boundary.
///
/// **What would make this fail**: deleting the `--tmpfs "$private"` line. The ordinary box then
/// connects, and the assertion below prints `connected`.
#[test]
fn a_box_cannot_connect_to_the_fleet_agents_socket() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user \
             namespace here, so the agent socket was NOT exercised against a real namespace on \
             this machine",
        );
    }
    if Command::new("python3").arg("-V").output().is_err() {
        return skip(
            "no python3, so no connect() \
             was attempted from inside a namespace on this machine",
        );
    }
    let fleet = Fleet::make("agentsock");
    let sock = fleet.fleet_root.join(".skein/private/fleet-agent.sock");
    // A real listener, accepting for the life of the test: `connect()` against nothing would be
    // refused for a reason that has nothing to do with the cover.
    // Bound and never accepted from, deliberately. A `connect()` to a unix socket succeeds as soon
    // as the kernel can queue it on the listen backlog — no `accept()` is involved — so this is a
    // listening socket for the only question being asked, without a thread whose lifetime the test
    // would then have to manage. Held in scope so it stays bound for BOTH probes: a listener closed
    // after the first would refuse the second for a reason that is not the cover, which is exactly
    // the failure this test exists to avoid.
    let _listener = std::os::unix::net::UnixListener::bind(&sock).expect("a listener");

    // The workshop box FIRST, so the refusal below is measured against a socket that is provably
    // accepting. An absence that was never a presence proves nothing.
    assert_eq!(
        fleet.connect_from_box(true, &sock),
        "connected",
        "the workshop box could not reach a socket that is listening, so the refusal below would \
         not be the cover's doing"
    );
    let ordinary = fleet.connect_from_box(false, &sock);
    assert!(
        ordinary.starts_with("refused"),
        "a box reached the fleet agent's socket ({ordinary}) — whoever can speak here is handed \
         the token that runs commands as the sandbox"
    );
}

/// A box cannot `connect()` to the socket the **cockpit's own tmux server** listens on (ISO-3,
/// SKEIN-529).
///
/// This socket sat beside `private/` rather than in it, at the top of `.skein`, which the launcher
/// binds back into every box readable. A read-only bind refuses nothing at all to a socket — the
/// kernel's `sb_permission` returns `EROFS` for regular files, directories and symlinks, and a
/// socket is none of those — and tmux admits any client whose peer uid matches its own, which under
/// one fleet-wide uid is every box's. A tmux client is not a reader: the server honours `MSG_SHELL`
/// and `MSG_EXEC`, so `tmux -S <that path> run-shell …` from inside any box is a command at fleet
/// scope, outside every namespace. The cover was in the right shape and the socket was outside it.
///
/// **The path is taken from `fleet::server_tmux_sock_in` and not spelled here**, which is what makes
/// this a test of the boundary rather than of a string: if the production path moves back out from
/// under `private/`, this probe follows it there and the ordinary box connects.
///
/// **What would make this fail**, run rather than reasoned about: changing `server_tmux_sock_in`
/// back to `format!("{fleet_root}/.skein/server.tmux")`. The workshop leg still says `connected`
/// and the ordinary box says `connected` too, where it must say `refused`. Deleting the
/// `--tmpfs "$private"` line from the launcher's isolation block does the same thing by the other
/// route.
#[test]
fn a_box_cannot_connect_to_the_fleets_tmux_socket() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user \
             namespace here, so the cockpit's tmux socket was NOT exercised against a real \
             namespace on this machine",
        );
    }
    if Command::new("python3").arg("-V").output().is_err() {
        return skip(
            "no python3, so no connect() \
             was attempted from inside a namespace on this machine",
        );
    }
    let fleet = Fleet::make("tmuxsock");
    let sock = PathBuf::from(skein::fleet::server_tmux_sock_in(
        fleet.fleet_root.to_string_lossy().as_ref(),
    ));

    // A listener rather than the real tmux: the question is whether the kernel lets a process in
    // this namespace reach this path, and a tmux server would answer it the same way at the cost of
    // a process the test then has to stop. `bind` failing here is itself a finding — it means the
    // production path no longer lands in a directory the fleet fixture makes.
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap_or_else(|e| {
        panic!(
            "could not bind a listener at the path `fleet::server_tmux_sock_in` returns ({}): {e} \
             — the socket now lands somewhere the fleet does not create, so nothing was tested",
            sock.display()
        )
    });

    // The workshop box FIRST, so the refusal below is measured against a socket that is provably
    // accepting. An absence that was never a presence proves nothing — and the workshop leg is also
    // the half a bind-list test cannot reach: the privileged box skips the tmpfs deliberately, so
    // it must read back exactly what the ordinary box could not.
    assert_eq!(
        fleet.connect_from_box(true, &sock),
        "connected",
        "the workshop box could not reach a socket that is listening at {}, so the refusal below \
         would not be the cover's doing",
        sock.display()
    );
    let ordinary = fleet.connect_from_box(false, &sock);
    assert!(
        ordinary.starts_with("refused"),
        "a box reached the cockpit's tmux socket at {} ({ordinary}) — tmux honours MSG_SHELL and \
         MSG_EXEC from the server, so whoever can speak here runs commands at fleet scope, outside \
         every box's namespace",
        sock.display()
    );
}

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
/// marker absent and pass while proving nothing. This is the shape `tests/isolation_bwrap.rs` was
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
/// **Presence before absence**, the shape `tests/isolation_bwrap.rs` exists to keep: the same argv
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

/// The mechanism `private/` replaces leaves nothing behind (ISO-2).
///
/// The old cover was an empty file at `<box root>/no-fleet-token`, bound over the one credential
/// somebody remembered. A launcher that still created it would leave a file whose only meaning is
/// a mechanism that no longer exists — and, worse, the *shape* that made the review credential
/// reachable: a cover spelled as a file name.
///
/// **What would make this fail**: putting back the `: >"$root/no-fleet-token"` line.
#[test]
fn the_per_file_token_cover_is_gone_from_the_launcher() {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    assert!(
        !src.contains(": >\"$root/no-fleet-token\""),
        "the launcher still creates the per-file token cover, so `.skein` is being protected by an \
         enumeration again"
    );
    assert!(
        !src.contains("--ro-bind \"$root/no-fleet-token\""),
        "the launcher still binds the per-file token cover"
    );
    // The replacement, named: one tmpfs over one directory, applied by the isolation block so a
    // privileged box skips it exactly as it skips everything else there.
    let block = isolation_block();
    assert!(
        block.contains("--tmpfs \"$private\""),
        "nothing covers `.skein/private/`, so every secret in it is readable from every box"
    );
}

/// **A box may write its own request queue entry and no other box's** (ISO-7).
///
/// Architecture §8.4 puts three steps in order — bind the artifact, make the request path per box,
/// *then* unmask the queue — and the middle one was skipped. One shared read-write `requests/`
/// directory let every box delete, rewrite or flip the state of every other box's pending request,
/// and file one in a neighbour's name. On the gitgate queue that last one is not an attribution
/// nicety: `gitgate::decide` builds the grant from the request's box and the refresher writes the
/// minted GitHub token into the box the grant names, so an approval a person read as one box's ask
/// put a live write token in another's.
///
/// **Asserted against a real namespace, because it cannot be asserted anywhere else.** The
/// launcher's refusal is `--ro-bind` on the queue root with `--bind` on one directory under it, and
/// a bind list read as text says only what the arguments were. Every box in this fleet is uid 1000
/// and mode bits stop none of it; what stops it is the mount, and the mount is what bwrap builds.
#[test]
fn a_box_can_write_its_own_request_queue_and_no_other_boxs() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create \
             a user namespace here, so the per-box drop-box was NOT exercised",
        );
    }
    let fleet = Fleet::make("queues");
    let report = fleet.seen_by_box(false);

    for queue in ["substrate", "gitgate"] {
        let root = fleet.fleet_root.join(format!(".skein/{queue}/requests"));
        assert_eq!(
            verdict(&report, &root.join("web-main")),
            "write",
            "the box cannot file a {queue} request at all, which is the defect the unmask was for:\n{report}"
        );
        // The whole finding. `see` and not `gone`: boxes share a uid and the queue root is
        // deliberately readable, so a box can still READ a neighbour's ask — that is what lets a
        // repeated ask collapse across boxes. What it must not do is write one.
        assert_eq!(
            verdict(&report, &root.join("other-main")),
            "see",
            "a box can write another box's {queue} drop-box, so it can rewrite, delete or \
             impersonate that box's pending requests:\n{report}"
        );
        // And it cannot make itself a drop-box under someone else's name either, which is the
        // half a per-directory check would miss.
        assert_eq!(
            verdict(&report, &root),
            "see",
            "the {queue} queue root is writable, so a box can create or remove another box's \
             drop-box:\n{report}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The peer network — SKEIN-572
// ---------------------------------------------------------------------------------------------

/// The launcher's `/run` cover and the peer-network block that opens one named hole in it.
///
/// Lifted out of `box-session.sh` rather than copied, for the reason [`isolation_block`] is: a copy
/// keeps passing against the version it was written from, and this block's whole job is to hold two
/// decisions together that spent months drifting apart in that file.
///
/// Stops at the last line that touches `binds`, deliberately — the `SKEIN_PEERS` line after it
/// prints to stdout, which is where the probe's answers come back.
fn peer_network_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.starts_with("# --- /run:"))
        .expect("the /run cover moved");
    let to = lines
        .iter()
        .position(|l| l.starts_with(r#"[ "$peers" = "1" ] || binds+="#))
        .expect("nothing closes the socket directory when the peer network is off");
    assert!(
        to > from,
        "the peer network block moved above the /run cover"
    );
    lines[from..=to].join("\n")
}

/// A sandbox holding one peer that is **registered and listening**, and one box's view of it.
///
/// The two halves of Claude Code's session messaging, as files: a registration under
/// `~/.claude/sessions/` that `ListAgents` reads, and an inbox socket under the runtime directory's
/// `cc-socks/` that `SendMessage` connects to. Both are planted here the way a live peer would
/// leave them, so "can this box reach that peer" is asked of the kernel rather than of an argv.
struct Peers {
    dir: Scratch,
    /// The sandbox's own `$HOME`, shared by every box — where the session registry lives.
    sandbox_home: PathBuf,
    /// This box's private `$HOME`, bound over the path above the way the launcher binds it.
    box_home: PathBuf,
    /// `$SKEIN_RUNTIME_DIR` — `/run/user/<uid>` under the name that lets a test plant a file in it
    /// without writing into the live fleet's runtime directory beside real agents' sockets.
    runtime: PathBuf,
    /// Held open for the lifetime of the fixture: a socket nothing is listening on refuses
    /// `connect()` whatever the mounts say, which would make every transport answer "no" and every
    /// assertion below vacuous.
    _peer: std::os::unix::net::UnixListener,
}

impl Peers {
    fn make(tag: &str) -> Peers {
        let dir = Scratch::temp(&format!("skein-peers-{tag}"));
        let sandbox_home = dir.join("sandbox-home");
        let box_home = dir.join("box-home");
        let runtime = dir.join("runtime");
        for p in [
            sandbox_home.join(".claude/sessions"),
            box_home.join(".claude"),
            runtime.join("cc-socks"),
            // Something under the runtime directory that is NOT the socket directory. The cover
            // this block opens a hole in is only worth anything if the hole is the size of one
            // directory, and nothing here says so unless something else is there to stay hidden.
            runtime.join("some-daemon"),
        ] {
            fs::create_dir_all(&p).unwrap();
        }
        // The peer, as `ListAgents` would find it: a registration naming the socket it listens on.
        fs::write(
            sandbox_home.join(".claude/sessions/4242.json"),
            r#"{"pid":4242,"name":"other-main","messagingSocketPath":"cc-socks/4242.sock"}"#,
        )
        .unwrap();
        fs::write(
            runtime.join("some-daemon/state"),
            "a neighbour's runtime state\n",
        )
        .unwrap();
        fs::write(runtime.join("bus-socket-marker"), "not the peer network\n").unwrap();
        let peer = std::os::unix::net::UnixListener::bind(runtime.join("cc-socks/4242.sock"))
            .expect("a listening peer");
        Peers {
            sandbox_home,
            box_home,
            runtime,
            _peer: peer,
            dir,
        }
    }

    /// What this box can reach of that peer, and of the runtime directory around it.
    ///
    /// Four lines: `discovery`, `transport`, `neighbour` and `socket-dir`. `peers` is what the host
    /// puts in `$SKEIN_BOX_PEERS` — `None` for a host that never sets it, which must read as the
    /// default the switch ships with.
    fn seen(&self, privileged: bool, peers: Option<&str>) -> String {
        // python3 rather than sh: only a syscall can answer the transport half, and a directory
        // listing is exactly the wrong question — a path that resolves says nothing about whether
        // anything is listening at the other end of it.
        let probe = "import os,socket,sys\n\
                     reg, sock, neighbour, socks = sys.argv[1:5]\n\
                     print('discovery', 'yes' if os.path.isfile(reg) else 'no')\n\
                     s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
                     try:\n\
                     \x20   s.connect(sock); print('transport yes')\n\
                     except OSError as e:\n\
                     \x20   print('transport no-' + e.__class__.__name__)\n\
                     print('neighbour', 'yes' if os.path.isfile(neighbour) else 'no')\n\
                     print('socket-dir', 'yes' if os.path.isdir(socks) else 'no')\n";
        let args = [
            self.sandbox_home
                .join(".claude/sessions/4242.json")
                .to_string_lossy()
                .into_owned(),
            self.runtime
                .join("cc-socks/4242.sock")
                .to_string_lossy()
                .into_owned(),
            self.runtime
                .join("some-daemon/state")
                .to_string_lossy()
                .into_owned(),
            self.runtime.join("cc-socks").to_string_lossy().into_owned(),
        ];
        let quoted: Vec<String> = args
            .iter()
            .map(|a| skein::util::sh_quote(a))
            .collect::<Vec<_>>();
        let runner = format!(
            "set -uo pipefail\n\
             export HOME={home}\n\
             {peers}\
             export SKEIN_BOX_PRIVILEGED={priv} SKEIN_RUNTIME_DIR={rt}\n\
             binds=(--bind {boxhome} {home})\n\
             {block}\n\
             exec bwrap --dev-bind / / ${{binds[@]+\"${{binds[@]}}\"}} -- \
             /bin/sh -c 'exec python3 -c \"$1\" \"$2\" \"$3\" \"$4\" \"$5\"' skein-probe {probe} {args}\n",
            // The launcher's own `--bind "$home" "$HOME"` (`grep -n 'binds=(--bind' box-session.sh`),
            // reproduced here for the reason `seen_by_box` reproduces the conversation bind: the
            // block under test binds two paths BACK through this one, so testing it without the
            // private HOME would assert that a box can read the shared registry — which is true of
            // every path in the sandbox and proves nothing about the peer network.
            home = skein::util::sh_quote(self.sandbox_home.to_string_lossy().as_ref()),
            boxhome = skein::util::sh_quote(self.box_home.to_string_lossy().as_ref()),
            rt = skein::util::sh_quote(self.runtime.to_string_lossy().as_ref()),
            priv = if privileged { "1" } else { "0" },
            peers = match peers {
                None => String::new(),
                Some(v) => format!("export SKEIN_BOX_PEERS={}\n", skein::util::sh_quote(v)),
            },
            block = peer_network_block(),
            probe = skein::util::sh_quote(probe),
            args = quoted.join(" "),
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
        let _ = &self.dir;
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

/// One answer out of a [`Peers::seen`] report.
fn answer<'a>(report: &'a str, key: &str) -> &'a str {
    report
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("the probe said nothing about {key}:\n{report}"))
}

/// **A box is never discoverable on a socket it cannot reach** (SKEIN-572, architecture §9.5 R11).
///
/// The invariant, not the flag: whatever `$SKEIN_BOX_PEERS` says and whichever side of the
/// privileged switch a box is on, the registry that advertises it and the socket that serves it are
/// reachable together or not at all. Half-open is the state this fleet was actually in — six
/// sessions in the shared registry, every one advertising a socket, exactly one resolving — and it
/// is worse than either whole state, because a sender addresses a box the registry says is live,
/// the message goes out through Anthropic's servers or nowhere, and nothing tells either end.
///
/// **What would make this fail.** Dropping the `cc-socks` bind gives discovery without transport,
/// which is precisely what the `--tmpfs` over `/run/user/<uid>` did on the day it landed and what
/// no test here noticed for months. Dropping the `.claude/sessions` bind gives the mirror image.
/// Either one breaks the `assert_eq!` below; nothing else in this file would have.
#[test]
fn a_box_is_never_discoverable_on_a_socket_it_cannot_reach() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so the peer network's two halves were \
             NOT checked against each other",
        );
    }
    let f = Peers::make("invariant");
    // `None` is the host that never sets the variable, which must read as the shipped default —
    // an old host must not silently take a working fleet off its own peer network.
    for privileged in [false, true] {
        for (setting, want_on) in [
            (None, true),
            (Some("1"), true),
            (Some("0"), false),
            (Some("off"), false),
        ] {
            let report = f.seen(privileged, setting);
            let discovery = answer(&report, "discovery") == "yes";
            let transport = answer(&report, "transport") == "yes";
            assert_eq!(
                discovery, transport,
                "SKEIN_BOX_PEERS={setting:?} privileged={privileged} leaves the peer network \
                 half-open: discovery and transport must move together, and a box that is \
                 advertised but unreachable is the failure this asserts against:\n{report}"
            );
            assert_eq!(
                discovery, want_on,
                "SKEIN_BOX_PEERS={setting:?} privileged={privileged} put the box on the wrong \
                 side of its own switch:\n{report}"
            );
        }
    }
}

/// **The cover that remains: one hole, the size of one directory** (architecture §9.5 R11).
///
/// Opening `cc-socks` is only safe if the next path under `/run/user/<uid>` cannot be opened by
/// accident, and until now that cover had no test at all — `isolation_block` stops at the mount
/// cover, hundreds of lines above this one. So the most open configuration is the one asserted
/// against: peers ON, the hole at its widest, and a neighbour's runtime state still gone.
///
/// **What would make this fail.** Binding `$runtime_dir` back instead of `$runtime_dir/cc-socks`,
/// or dropping the `--tmpfs` and relying on the bind alone — both leave `neighbour` readable, and
/// both are the shape of edit somebody makes while fixing a socket that will not connect.
#[test]
fn nothing_but_the_socket_directory_comes_through_the_run_cover() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so what the /run cover still hides was \
             NOT checked",
        );
    }
    let f = Peers::make("containment");

    let open = f.seen(false, Some("1"));
    assert_eq!(
        answer(&open, "transport"),
        "yes",
        "the hole is not open at all, so what it does not reach proves nothing:\n{open}"
    );
    assert_eq!(
        answer(&open, "neighbour"),
        "no",
        "the peer network's bind carried the whole runtime directory through the cover, not just \
         the socket directory:\n{open}"
    );

    // And with the switch off, the socket directory itself is a private tmpfs rather than a hole:
    // the box gets a directory of its own, so its own agent still starts, and nothing in it is
    // anybody else's.
    let shut = f.seen(false, Some("0"));
    assert_eq!(
        answer(&shut, "socket-dir"),
        "yes",
        "an isolated box has no socket directory at all, so its own agent cannot open an inbox:\n{shut}"
    );
    // `FileNotFoundError` and not a permission error, which is the launcher's own argument for a
    // tmpfs over a `--ro-bind` of an empty directory: read-only refuses nothing to a socket, so the
    // cover has to make the NAME absent rather than the connection refused. An errno saying the
    // path was still there would mean the peer's socket was reachable and merely unserved.
    assert_eq!(
        answer(&shut, "transport"),
        "no-FileNotFoundError",
        "an isolated box still reached a peer's inbox socket:\n{shut}"
    );
    assert_eq!(answer(&shut, "neighbour"), "no", "{shut}");
}
