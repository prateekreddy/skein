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

/// The launcher's start-up announcements, lifted the same way.
///
/// One block holding both: the workshop box's banner and the uncovered box's, which is how they
/// are written, so a change that made them say the same thing is a change this test runs.
///
/// **From `announce=""` to `unset uncovered`, which is wider than it was.** It used to stop at the
/// `fi`, and the `fi` used to be the end — the block was two `echo … >&2` and nothing else. The
/// writing and the delivering are separate statements now (SKEIN-846): the `if` decides a sentence
/// and the lines after it put that sentence on stdout, where skein reads it, as well as on stderr.
/// Stopping at the `fi` would run the deciding and skip the delivering, which is a harness that
/// cannot see the bug the item is about. Starting at the assignment matters too — `set -u` is on,
/// and a fragment beginning at the `if` leaves `$announce` undefined for the covered box.
fn announcement_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| *l == r#"announce="""#)
        .expect("the start-up announcement block moved");
    let to = lines[from..]
        .iter()
        .position(|l| *l == "unset uncovered")
        .map(|i| from + i)
        .expect("the announcement block has no end");
    let block = lines[from..=to].join("\n");
    assert!(
        block.contains(r#"if [ "${SKEIN_BOX_PRIVILEGED-}" = "1" ]; then"#),
        "the lifted block no longer holds the switch the two banners are chosen by, so this \
         harness is running something other than the announcement"
    );
    block
}

/// Everything the launcher **unsets** between the isolation block and the announcement block.
///
/// This exists because the two blocks are ~150 lines apart and `unset SKEIN_FLEET_MOUNTS
/// SKEIN_BOX_STORE` runs in the gap: a banner that asked `$SKEIN_FLEET_MOUNTS` directly would find
/// it empty for EVERY box and fire on all of them, and a harness that ran the two blocks back to
/// back would call that correct. Splicing the gap's `unset`s in is what makes this reproduce the
/// order production runs in.
///
/// **Derived, not listed.** Every line in the gap beginning an `unset` at column 0 comes through,
/// so an `unset` added there tomorrow is one this test runs — a spelled-out list would go on
/// passing over the one statement somebody forgot to add. It **refuses to run at all** when it
/// finds none, because zero here is indistinguishable from "a landmark moved" by its result alone.
fn unsets_between_blocks() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let iso = lines
        .iter()
        .position(|l| l.starts_with(r#"if [ "${SKEIN_BOX_PRIVILEGED-}" != "1" ]; then"#))
        .expect("the isolation block moved");
    let iso_end = lines[iso..]
        .iter()
        .position(|l| *l == "fi")
        .map(|i| iso + i)
        .expect("the isolation block has no end");
    let ann = lines
        .iter()
        .position(|l| l.starts_with(r#"if [ "${SKEIN_BOX_PRIVILEGED-}" = "1" ]; then"#))
        .expect("the start-up announcement block moved");
    assert!(
        ann > iso_end,
        "the announcement no longer comes after the isolation block, so this harness would be \
         splicing the launcher together in an order it does not run in"
    );
    let unsets: Vec<&str> = lines[iso_end + 1..ann]
        .iter()
        .filter(|l| l.starts_with("unset "))
        .copied()
        .collect();
    assert!(
        !unsets.is_empty(),
        "nothing is unset between the isolation block and the announcement — either a real change \
         or a moved landmark, and this cannot tell them apart, so it refuses rather than testing \
         an order the launcher does not have"
    );
    unsets.join("\n")
}

/// How a box was born, as the launcher's isolation block sees it. Four states, and the point of
/// naming them is that two of them are uncovered and only one was chosen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Born {
    /// Matched to a repository, so `fleet::mount_manifest` names every host mount and the box is
    /// covered — the ordinary case.
    Covered,
    /// `SKEIN_BOX_PRIVILEGED=1`: the workshop box, exempt from the whole block on purpose.
    Workshop,
    /// Not privileged, and `fleet::mount_manifest` matched no repository — so `SKEIN_FLEET_MOUNTS`
    /// and `SKEIN_BOX_STORE` arrive EMPTY and the two loops written over the manifest iterate
    /// nothing. Uncovered by accident (SKEIN-836).
    Unmatched,
    /// Both at once, and a real state rather than a contrivance: nothing stops the workshop switch
    /// being thrown on a box whose name matches no repository. It exists so the banner can be asked
    /// the question it is easiest to get wrong — whether it fires on "the manifest is empty" when
    /// what it names is "nobody chose this".
    WorkshopUnmatched,
}

impl Born {
    fn privileged(self) -> &'static str {
        match self {
            Born::Workshop | Born::WorkshopUnmatched => "1",
            Born::Covered | Born::Unmatched => "0",
        }
    }

    /// Whether `fleet::mount_manifest` had a repository to answer with.
    fn matched(self) -> bool {
        matches!(self, Born::Covered | Born::Workshop)
    }
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

    /// The manifest the host hands the launcher.
    ///
    /// **Empty for [`Born::Unmatched`], and that is the whole of the condition under test**:
    /// `fleet::mount_manifest` returns `String::new()` for a box it cannot match to a repository,
    /// so `SKEIN_FLEET_MOUNTS` arrives empty and every cover written over the manifest silently
    /// iterates nothing. Reproduced here rather than asserted about, because the question is what
    /// the box can then reach.
    fn mounts(&self, born: Born) -> String {
        if !born.matched() {
            return String::new();
        }
        let mut out = format!("{}\n{}\n", self.repos.display(), self.elsewhere.display());
        if let Some(volume) = &self.volume {
            out.push_str(&format!("{}\n", volume.display()));
        }
        out
    }

    fn store(&self) -> PathBuf {
        self.repos.join("web/store/.claude")
    }

    /// Empty for the same box and from the same `None`: `session_script` computes
    /// `SKEIN_BOX_STORE` as `repo_for_box(name).map(|r| r.store).unwrap_or_default()`, so a box
    /// with no manifest also has no store to bind back. Passing the real one here would test a
    /// combination production cannot produce.
    fn store_env(&self, born: Born) -> String {
        match born.matched() {
            false => String::new(),
            true => self.store().to_string_lossy().into_owned(),
        }
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
    fn connect_from_box(&self, born: Born, sock: &Path) -> String {
        // python3 rather than a shell: `sh` has no way to open a unix socket, and the whole point
        // is to make the syscall the kernel decides rather than to look at a directory listing.
        let probe = "import socket,sys\n\
                     s=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
                     try:\n\
                     \x20   s.connect(sys.argv[1]); print('connected')\n\
                     except OSError as e:\n\
                     \x20   print('refused', e.__class__.__name__)\n";
        let out = self.in_box(
            born,
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
    fn in_box(&self, born: Born, probe: &str, args: &[String]) -> Vec<u8> {
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
            priv = born.privileged(),
            mounts = skein::util::sh_quote(&self.mounts(born)),
            store = skein::util::sh_quote(&self.store_env(born)),
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
    fn seen_by_box(&self, born: Born) -> String {
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
            priv = born.privileged(),
            mounts = skein::util::sh_quote(&self.mounts(born)),
            store = skein::util::sh_quote(&self.store_env(born)),
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

    /// What the launcher SAYS about this box at start — its stderr, with no bwrap involved.
    ///
    /// Three lifted regions in the order the launcher runs them: the isolation block, where the
    /// flag the banner reads is set; every `unset` between the two, which is where
    /// `SKEIN_FLEET_MOUNTS` is taken away; and the announcement itself. The middle one is the point
    /// — a banner that asked the variable directly would find it gone and fire on every box, and
    /// only running the gap catches that. It is also why this does not simply grep the script.
    ///
    /// No namespace, so no `bwrap_works()` gate: what a box is TOLD is shell logic, and it is the
    /// tests of what it can REACH that need a kernel.
    fn announced(&self, born: Born) -> String {
        self.announcement_of(born).1
    }

    /// The same run, read from **stdout** — the channel skein gets to keep.
    ///
    /// This is the half SKEIN-846 is about. `Place::bytes` returns the launcher's stdout on the
    /// success path and reads its stderr only when the launcher exits non-zero, so a banner written
    /// to stderr on a launch that worked is a banner nobody will ever see. What is asserted against
    /// this is that the sentence leaves by the door skein is standing at.
    fn announced_to_skein(&self, born: Born) -> String {
        self.announcement_of(born).0
    }

    fn announcement_of(&self, born: Born) -> (String, String) {
        let runner = format!(
            "set -uo pipefail\n\
             binds=()\n\
             box=web-main\n\
             root={root}\n\
             state={state}\n\
             export SKEIN_FLEET_ROOT={fleet} SKEIN_BOX_PRIVILEGED={priv} \
             SKEIN_FLEET_MOUNTS={mounts} SKEIN_BOX_STORE={store}\n\
             {block}\n\
             {unsets}\n\
             {announce}\n",
            root =
                skein::util::sh_quote(self.fleet_root.join("web-main").to_string_lossy().as_ref()),
            state = skein::util::sh_quote(
                self.state_parent.join("web-main").to_string_lossy().as_ref()
            ),
            fleet = skein::util::sh_quote(self.fleet_root.to_string_lossy().as_ref()),
            priv = born.privileged(),
            mounts = skein::util::sh_quote(&self.mounts(born)),
            store = skein::util::sh_quote(&self.store_env(born)),
            block = isolation_block(),
            unsets = unsets_between_blocks(),
            announce = announcement_block(),
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(&runner)
            .output()
            .expect("bash");
        assert!(
            out.status.success(),
            "the launcher's announcement block did not run: {}\n--- script ---\n{runner}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
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
    let report = fleet.seen_by_box(Born::Covered);

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
    let workshop = fleet.seen_by_box(Born::Workshop);
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
    let report = fleet.seen_by_box(Born::Covered);

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
    let report = fleet.seen_by_box(Born::Covered);

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
    let report = fleet.seen_by_box(Born::Workshop);
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

/// **A box skein could not match to a repository is uncovered, and it is uncovered exactly this
/// far** (SKEIN-836).
///
/// `fleet::mount_manifest` returns an empty manifest for such a box, so `SKEIN_FLEET_MOUNTS`
/// arrives empty and the two covers written *over the manifest* — the SKEIN-219 ancestor cover and
/// the per-mount cover — iterate nothing. Everything the launcher spells from paths skein chose
/// still runs, because none of it is inside that guard.
///
/// Both halves are asserted because only one of them is believable on its own:
///
///   * **the exposure**, and the same volume is read by a COVERED box in the same test — otherwise
///     a fixture that failed to write a secret would report it reachable-because-absent, or
///     hidden-because-absent, and neither direction proves anything (the rule the
///     `a_box_on_a_mounted_volume…` test states);
///   * **the limit**, which is the half this line used to get wrong. `mount_manifest` said such a
///     box "starts with the sandbox's whole view, as boxes did before covers", and it does not:
///     the other boxes' checkouts and state are still gone, and `private/` is still covered. A
///     warning that overstates is one a reader learns to discount.
///
/// **What would make this fail.** Moving `binds+=(--tmpfs "$fleet_root_dir")` or the state-parent
/// cover inside the `[ -n "${SKEIN_FLEET_MOUNTS-}" ]` guard breaks the limit half — the other box's
/// checkout comes back. Hoisting the ancestor cover out of that guard breaks the exposure half —
/// the credentials go, and the assertions that the covered box cannot see them stop proving that
/// the cover is what hid them.
#[test]
fn a_box_with_no_mount_manifest_is_uncovered_and_a_matched_box_is_not() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so the unmatched-repo path was NOT \
             exercised against a real namespace on this machine",
        );
    }
    let fleet = Fleet::make_on_volume("unmatched");
    let volume = fleet.volume.clone().expect("this fleet is on a volume");
    let loose = fleet.seen_by_box(Born::Unmatched);
    let covered = fleet.seen_by_box(Born::Covered);

    // The fleet's own credentials. Reachable from the unmatched box, gone from the matched one —
    // the second assertion is what makes the first a statement about the cover.
    for secret in [
        volume.join("credentials/claude.json"),
        volume.join("github-pats/acme"),
        volume.join("api-token"),
        volume.join("warden/secret"),
    ] {
        assert_ne!(
            verdict(&loose, &secret),
            "gone",
            "a box with no manifest cannot see {} — then the ancestor cover is not what the \
             manifest gates, and the exposure this test names is somewhere else:\n{loose}",
            secret.display()
        );
        assert_eq!(
            verdict(&covered, &secret),
            "gone",
            "a box WITH a manifest can read {} — that credential IS the fleet:\n{covered}",
            secret.display()
        );
    }

    // Every other repo's store and work tree, same shape and the commoner case: most fleets are not
    // on a volume, and this half is the one they still have.
    for (path, what) in [
        (
            fleet.repos.join("other/store/.claude"),
            "another repo's store",
        ),
        (
            fleet.elsewhere.join("store/.claude"),
            "a store outside the workspace",
        ),
    ] {
        assert_eq!(
            verdict(&loose, &path),
            "write",
            "{what} is not reachable from a box with no manifest, so the empty manifest is not \
             what this test thinks it is:\n{loose}"
        );
        assert!(
            matches!(verdict(&covered, &path), "gone" | "empty"),
            "{what} is reachable from a box WITH a manifest:\n{covered}"
        );
    }

    // And the limit. These covers are spelled from paths skein chose, so they do not need the
    // manifest and they still apply — which is why "the sandbox's whole view" was the wrong words.
    for (path, what) in [
        (
            fleet.fleet_root.join("other-main"),
            "another box's checkout",
        ),
        (
            fleet.state_parent.join("other-main"),
            "another box's conversation",
        ),
        (
            fleet.fleet_root.join(".skein/private/fleet-agent.token"),
            "the fleet agent's token",
        ),
    ] {
        assert!(
            matches!(verdict(&loose, &path), "gone" | "empty"),
            "{what} is reachable from a box with no manifest — either a cover moved inside the \
             manifest guard, or the launcher's banner is now overstating what is exposed:\n{loose}"
        );
    }

    // …and the box still has its own, which is the half a blunt "cover everything" would break.
    assert_eq!(
        verdict(&loose, &fleet.fleet_root.join("web-main")),
        "write",
        "a box with no manifest lost its own checkout:\n{loose}"
    );
    assert_eq!(
        verdict(&loose, &fleet.state_parent.join("web-main")),
        "see",
        "a box with no manifest lost its own state:\n{loose}"
    );
}

/// **The box that is uncovered by accident says so, and says something different from the box that
/// is uncovered on purpose** (SKEIN-836).
///
/// Two boxes can run without the mount cover and only one of them was chosen. The workshop box has
/// announced itself at every start for as long as the switch has existed; the box no manifest
/// reached announced nothing, and the one line that mentioned it went to skein-server's own stderr,
/// where SKEIN-799 established nobody reads it. So from inside, the deliberate state and the
/// accidental one were the same state.
///
/// **This is a status display, which in this repo is the thing that reports on something other than
/// what it names**, so all four cases are asserted rather than the one that motivated the change:
/// it must fire on an empty manifest, stay silent on a full one, and lose to the workshop banner
/// when both conditions hold at once — because what that box is, is chosen.
///
/// **What would make this fail.** Deleting `[ -n "${SKEIN_FLEET_MOUNTS-}" ] || uncovered=1` from
/// the top of the isolation block, or reading `$SKEIN_FLEET_MOUNTS` at the banner instead of the
/// flag — the `unset` between them means the banner would then never fire, and the empty-manifest
/// case goes silent. Dropping the `elif`'s condition fires it on a covered box. Making the two
/// banners one sentence collapses the distinction the whole item is about.
/// And for the delivery half at the end: deleting the `printf 'SKEIN_NOTICE %s\n'` line from the
/// launcher, which leaves both banners on a stderr that `Place::bytes` throws away on every
/// successful launch — the state SKEIN-846 found and the reason this test grew a second half.
#[test]
fn an_unmatched_box_announces_that_it_is_uncovered_and_a_covered_box_says_nothing() {
    let fleet = Fleet::make_on_volume("announce");

    // Covered: nothing to report, and reporting anyway is the failure mode that makes a banner
    // worth ignoring.
    let covered = fleet.announced(Born::Covered);
    assert!(
        !covered.contains("UNCOVERED"),
        "a box with a manifest is told it came up uncovered:\n{covered}"
    );

    // Unmatched: it says so, it names itself, and it names what is reachable rather than just
    // sounding alarmed.
    let loose = fleet.announced(Born::Unmatched);
    assert!(
        loose.contains("UNCOVERED"),
        "a box with no manifest is told nothing, which is the state this item found:\n{loose}"
    );
    assert!(
        loose.contains("web-main"),
        "the banner does not say WHICH box, which is the one thing a reader cannot recover from \
         it:\n{loose}"
    );
    for term in ["store", "work tree", "still separate"] {
        assert!(
            loose.contains(term),
            "the banner no longer names `{term}` — it has to say what is exposed AND what is not, \
             or the next reader re-derives it from the launcher:\n{loose}"
        );
    }

    // The workshop box says its own thing, and it is not this one.
    let workshop = fleet.announced(Born::Workshop);
    assert!(
        workshop.contains("WORKSHOP box"),
        "the privileged box stopped announcing itself:\n{workshop}"
    );
    assert!(
        !workshop.contains("UNCOVERED"),
        "the workshop box is told it came up uncovered by accident, which is the opposite of what \
         its switch means:\n{workshop}"
    );
    assert_ne!(
        loose.trim(),
        workshop.trim(),
        "the deliberate and the accidental uncovered box now say the same thing, so a reader still \
         cannot tell which one they are in"
    );

    // Both conditions at once: privileged wins, because that state was chosen.
    let both = fleet.announced(Born::WorkshopUnmatched);
    assert!(
        both.contains("WORKSHOP box") && !both.contains("UNCOVERED"),
        "a privileged box with no manifest is reported as an accident — the banner is firing on \
         the empty manifest rather than on the condition it names:\n{both}"
    );

    // ---- and it leaves by the door skein is standing at (SKEIN-846) ----
    //
    // Everything above is about WHICH sentence. This is about whether anyone gets it. The launcher
    // runs under `Place::exec`, which keeps stdout on success and reads stderr only on failure, so
    // for as long as these banners were `echo … >&2` alone they were written on every start and
    // seen on none. Read back through `fleet::notices_from_launch` — the production parser, not a
    // `contains` of my own — so that a marker the launcher writes in a shape skein cannot lift out
    // again fails here rather than in a fleet.
    let recovered = skein::fleet::notices_from_launch(&fleet.announced_to_skein(Born::Unmatched));
    assert_eq!(
        recovered.len(),
        1,
        "an uncovered box wrote {} notices to the stream skein reads; the banner is the one thing \
         on that stream that has to survive a successful launch",
        recovered.len()
    );
    assert!(
        recovered[0].contains("came up UNCOVERED") && recovered[0].contains("web-main"),
        "the sentence skein recovers from the launcher is not the sentence the launcher decided: \
         {:?}",
        recovered[0]
    );
    assert!(
        skein::fleet::notices_from_launch(&fleet.announced_to_skein(Born::Workshop))
            .iter()
            .any(|said| said.contains("WORKSHOP box")),
        "the workshop box announces itself only where nobody reads, which is where both banners \
         were for months"
    );
    assert!(
        skein::fleet::notices_from_launch(&fleet.announced_to_skein(Born::Covered)).is_empty(),
        "a covered box puts a notice on skein's stream, so every launch would print one and the \
         two that matter would be lost in them"
    );
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
    let report = fleet.seen_by_box(Born::Covered);

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
    let workshop = fleet.seen_by_box(Born::Workshop);
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
        fleet.connect_from_box(Born::Workshop, &sock),
        "connected",
        "the workshop box could not reach a socket that is listening, so the refusal below would \
         not be the cover's doing"
    );
    let ordinary = fleet.connect_from_box(Born::Covered, &sock);
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
        fleet.connect_from_box(Born::Workshop, &sock),
        "connected",
        "the workshop box could not reach a socket that is listening at {}, so the refusal below \
         would not be the cover's doing",
        sock.display()
    );
    let ordinary = fleet.connect_from_box(Born::Covered, &sock);
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
///     `mv`, so no report is written at all. The `ends_with` assertion below therefore has NO
///     sabotage that reaches it, and is documentation rather than a proved guard.
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
    // `python3` at all. `before.path` is that PATH, measured in run 1 rather than spelled here.
    assert!(
        now.path.ends_with(&before.path),
        "the box's session PATH no longer ends with the launcher's fixed PATH, so a box has the \
         agent and not the userland: {} does not end with {}",
        now.path,
        before.path
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
    let skein_home = dir.join("skein-home");
    fs::create_dir_all(skein_home.join("places")).unwrap();
    std::env::set_var("SKEIN_HOME", &skein_home);
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
    let report = fleet.seen_by_box(Born::Covered);

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
