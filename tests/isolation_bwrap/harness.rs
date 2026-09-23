//! The fleet this suite runs bwrap over: the launcher's blocks lifted out of `box-session.sh`,
//! the fleet-shaped directory tree, a box born inside it, and what the process in there saw.

use super::*;

/// The launcher's isolation block, lifted out of the script it lives in.
///
/// Read from `box-session.sh` rather than copied, so a change to the launcher is a change to what
/// this test runs — a copy would keep passing against the version it was written from.
pub(super) fn isolation_block() -> String {
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
pub(super) enum Born {
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
pub(super) struct Fleet {
    pub(super) dir: Scratch,
    /// `$SKEIN_FLEET_ROOT` — the sandbox-local directory holding every box's checkout.
    pub(super) fleet_root: PathBuf,
    /// The host directory holding every box's durable state.
    pub(super) state_parent: PathBuf,
    /// The workspace mount, holding both repos' stores.
    pub(super) repos: PathBuf,
    /// A second mount, where somebody keeps a repo skein did not choose the location of.
    pub(super) elsewhere: PathBuf,
    /// The volume the fleet's own state lives on, when this fleet is the 4c shape: a mount that
    /// CONTAINS what a box owns, rather than sitting beside it. `None` is the 4a shape.
    pub(super) volume: Option<PathBuf>,
}

impl Fleet {
    pub(super) fn make(tag: &str) -> Fleet {
        Fleet::build(tag, false)
    }

    /// The fleet skein-server runs inside (delivery §3 4c): box state lives on the VOLUME the
    /// server is given, beside the fleet's credentials — so the path the sandbox mounts is an
    /// ancestor of the two directories a box owns, which is the case the cover used to skip.
    pub(super) fn make_on_volume(tag: &str) -> Fleet {
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

    pub(super) fn store(&self) -> PathBuf {
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
    pub(super) fn connect_from_box(&self, born: Born, sock: &Path) -> String {
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
    pub(super) fn in_box(&self, born: Born, probe: &str, args: &[String]) -> Vec<u8> {
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
    pub(super) fn seen_by_box(&self, born: Born) -> String {
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
    pub(super) fn announced(&self, born: Born) -> String {
        self.announcement_of(born).1
    }

    /// The same run, read from **stdout** — the channel skein gets to keep.
    ///
    /// This is the half SKEIN-846 is about. `Place::bytes` returns the launcher's stdout on the
    /// success path and reads its stderr only when the launcher exits non-zero, so a banner written
    /// to stderr on a launch that worked is a banner nobody will ever see. What is asserted against
    /// this is that the sentence leaves by the door skein is standing at.
    pub(super) fn announced_to_skein(&self, born: Born) -> String {
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
pub(super) fn verdict<'a>(report: &'a str, path: &Path) -> &'a str {
    let want = path.to_string_lossy();
    report
        .lines()
        .find(|l| l.split_once(' ').map(|(_, p)| p) == Some(want.as_ref()))
        .and_then(|l| l.split_once(' '))
        .map(|(v, _)| v)
        .unwrap_or_else(|| panic!("the probe said nothing about {}:\n{report}", path.display()))
}
