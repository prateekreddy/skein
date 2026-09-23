//! Path builders for the fleet root, the private secrets directory, and each box's own
//! root, socket and pidfile.

use super::*;

/// Where box roots live inside the fleet sandbox.
///
/// Deliberately not under `$HOME` or `/tmp`: `box-session.sh` binds the box's own directories over
/// both, so a root beneath either would be visible only from inside the box that owns it — and the
/// tmux socket and anchor pidfile that skein reads from outside live in this root.
///
/// `$SKEIN_FLEET_ROOT` overrides it, in the same spirit as `$SKEIN_LS_CMD` and `$SKEIN_LAUNCH_CMD`:
/// `/boxes` needs root to create, so without this seam the launch path could only ever be exercised
/// against a real sandbox — which is precisely the part that kept going untested.
pub fn fleet_root() -> String {
    crate::util::fleet_root()
}

/// Where the launcher is installed inside the fleet sandbox.
pub fn box_session_path() -> String {
    format!("{}/.skein/box-session.sh", fleet_root())
}

/// Where git's credential helper is installed. Beside the launcher, because every box's gitconfig
/// names this path and a box that cannot find it falls back to having no credential at all.
pub fn git_credential_helper_path() -> String {
    format!("{}/.skein/git-credential-skein", fleet_root())
}

/// **The directory that covers every secret skein keeps inside the sandbox.**
///
/// `.skein` itself is `--ro-bind`ed into every box, so a file placed directly in it is readable by
/// every box in the fleet — a read-only bind is still a read. The launcher used to answer that one
/// file at a time, binding an empty file over `fleet-agent.token` alone, and the file the next
/// credential was written to was covered by nobody: the review credential sat in the open, mode
/// 600, on a fleet where every box runs as uid 1000, for the life of the fleet.
///
/// Enumerating the secrets in a readable directory is the shape of that bug. This is the other way
/// round: one directory holds them all, `box-session.sh` puts a single `--tmpfs` over it, and a
/// secret added later is covered by having been put in the right place rather than by somebody
/// remembering to add a line to the launcher. A privileged box skips the tmpfs, exactly as it skips
/// the per-file cover today.
///
/// The mode on the files is not what protects them and never was — every box is the same uid — so
/// what makes this work is the mount and nothing else.
pub fn fleet_private_dir() -> String {
    fleet_private_dir_in(&fleet_root())
}

/// [`fleet_private_dir`] against a fleet root that is **not** `$SKEIN_FLEET_ROOT`.
///
/// One spelling of `.skein/private`, reachable by a caller that has a root in its hand rather than
/// in its environment. Both kinds exist: skein resolves the root from the environment, and a test
/// fixture builds one per test and cannot put it in the environment without racing every other test
/// in the binary — `util::fleet_root` refuses an unpinned test process outright (SKEIN-690), so the
/// alternative to this parameter is a literal in the fixture, which is the thing that goes stale.
///
/// It was a literal in three fixtures until the socket moved under here, and one of them —
/// `tests/server/fixture.rs::stop_doorway` — kills a tmux server with it. A stale literal there does not
/// fail; it silently stops killing anything, and the suite leaks a tmux server, a supervisor shell
/// and a python per test.
pub fn fleet_private_dir_in(fleet_root: &str) -> String {
    format!("{fleet_root}/.skein/private")
}

/// Where skein's secrets used to live inside the sandbox, so that they can be **removed**.
///
/// A credential is not moved by writing it somewhere else; it is moved by writing it somewhere else
/// and taking the old one away. Every fleet running before that move has a readable copy at this
/// path and it stays valid — the review credential is the owner's own GitHub token, which nothing
/// rotates — so a fleet that only gained the new location would have the same secret in the covered
/// place and in the open, which is ISO-2 with an extra step.
///
/// **Measured on a live fleet while this was being written**: `review-github.token`, mode 600, two
/// days old, in the half of `.skein` every box can read. The launcher's cover hides `private/`; it
/// cannot hide a file that is not in it.
///
/// **The agent's token was the other entry here, and went with the agent** (SKEIN-521). The sweep
/// did not go with it: its caller was the agent install, which is the accident this corrects — the
/// list is about what a PREVIOUS build left behind, so it belongs on a path that runs on every
/// server start rather than on one that has been deleted. [`heal_fleet`] is that path.
///
/// They exist for exactly as long as a fleet can be upgraded from a build that predates the move.
/// When that stops being true this function and its one caller go, and nothing else changes.
pub(super) fn stale_sandbox_secrets() -> Vec<String> {
    vec![format!("{}/.skein/review-github.token", fleet_root())]
}

/// A name no other in-flight review call has, for the credential file [`box_credential_paths`]
/// names.
///
/// **Per call, not one shared file.** The unlink ([`forget_review_token`]) is what makes the name
/// matter: two readings can be in flight at once — the queue runs them on threads — and with one
/// shared path the first to finish would take the credential out from under the second between its
/// write and its `cat`. The failure would be silent, because a reading with no token is exactly the
/// reading skein did before any of this existed.
///
/// pid and a counter, the shape [`crate::secret`]'s temp names use and for the same reason: two
/// threads of one process are the case a pid alone does not separate.
pub(super) fn review_call_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{n}", std::process::id())
}

/// Where the provisioning script is installed inside the fleet sandbox.
///
/// Beside the launcher rather than in `~/.local/bin` (where the kit puts it) for two reasons: a box
/// binds its own `$HOME` over the sandbox's, and the fleet sandbox is created without a kit at all,
/// so nothing would have put it there. Under `--dev-bind / /` this path reads the same from inside
/// every box as it does from the sandbox.
pub fn box_provision_path() -> String {
    format!("{}/.skein/skein-startup.sh", fleet_root())
}

/// One box's root inside the fleet sandbox. Callers must have validated `name`; every path skein
/// derives for a box hangs off this, so a name containing `..` would escape the layout entirely.
pub fn box_root(name: &str) -> String {
    format!("{}/{name}", fleet_root())
}

/// The box's tmux socket — the same path inside the namespace and out, which is what lets skein
/// list, attach to and kill a box without entering it first.
pub fn box_sock(name: &str) -> String {
    format!("{}/session.sock", box_root(name))
}

/// The file `box-session.sh` writes the box's anchor pid to. Read from outside, so it must sit in
/// the box root rather than in the box's private `/tmp`.
pub fn box_pidfile(name: &str) -> String {
    format!("{}/anchor.pid", box_root(name))
}

/// Where boxes keep the state that must outlive the sandbox, on the **host**.
///
/// A host path, mounted into the sandbox at that same absolute path — so this one string addresses
/// it from both sides, exactly as a repo store does. The *parent* is mounted, so a box added later
/// needs no recreate.
///
/// Distinct from a repo's store on purpose: this is per-box and skein-owned, not project-scoped
/// shared data, so nothing here crosses between boxes and it is not somewhere the user puts files.
pub fn box_state_root() -> String {
    skein_home().join("boxes").to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// The pool directory must not read as a box.
    #[test]
    fn dockers_pool_is_hidden_from_the_box_enumeration_it_sits_beside() {
        // Locked and pinned, both halves. `$SKEIN_FLEET_ROOT` is process-global and this test only
        // READS it — which was already a race against the setters in this file, and is a loud one
        // now that `util::fleet_root` refuses an unset root instead of answering `/boxes`: the
        // window a setter leaves when it removes the variable used to be harmless and is now a
        // panic. A reader has to take the lock too (SKEIN-690).
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        let root = docker_data_root();
        assert!(
            root.rsplit('/').next().is_some_and(|n| n.starts_with('.')),
            "every box is enumerated with `{}/*/`, which a non-dot directory would match — and a \
             box with no repo is a thing `resize_fleet` refuses to proceed past: {root}",
            fleet_root()
        );
        assert!(
            root.starts_with(&fleet_root()),
            "the pool has to be on the boxes' own filesystem or it is not one pool: {root}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// A resize copies the box, it does not rebuild it — so the archive has to be the whole box, and
    /// it has to land somewhere that outlives the VM being destroyed.
    #[test]
    fn a_resize_copies_the_whole_box_to_a_place_the_rebuild_cannot_reach() {
        // `box_state` reads $SKEIN_HOME, which is process-global: without the lock this races any
        // other test that points it somewhere, and fails for a reason having nothing to do with resize.
        let _g = env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        // And the fleet root beside it, for the same reason: `util::fleet_root` refuses an
        // unpinned test rather than answering `/boxes`, which on any machine running skein is the
        // live fleet (SKEIN-690).
        std::env::set_var("SKEIN_FLEET_ROOT", skein_home.join("fleet"));
        let archive = box_archive("web-main", "resize-x");

        // On the host, under the box's own state directory — mounted into the sandbox precisely so
        // it survives one. A copy written anywhere under the fleet root would die with the very
        // thing it exists to outlive.
        assert!(
            archive.starts_with(&box_state("web-main")),
            "the copy belongs beside the box's other durable host state: {archive}"
        );
        assert!(
            !archive.starts_with(&fleet_root()),
            "never under {}, which `sbx rm -f` destroys",
            fleet_root()
        );

        let script = archive_script("web-main", &archive);
        // The whole box, `/tmp` and all: `tar -C <root> … .` rather than naming `tree` and `home`,
        // so a resize is invisible to whatever the agent had half-finished in scratch space.
        assert!(
            script.contains(&format!("tar -C {} ", sh_quote(&box_root("web-main")))),
            "the archive is taken from the box root, whole: {script}"
        );
        // The one exclusion that matters. `anchor.pid` names a process in the VM about to be
        // destroyed; restored, it would have the box claim a namespace that was never recreated.
        // Sockets need no exclusion — tar skips them and warns, which is right for a tmux socket
        // whose server is about to die, and `--warning=no-file-ignored` keeps that off the console.
        assert!(
            script.contains("--exclude=./anchor.pid"),
            "a stale anchor pid must not survive the rebuild: {script}"
        );
        assert!(
            !script.contains("--exclude=./tmp") && !script.contains("--exclude=./home"),
            "nothing else is excluded — an exact copy is the point: {script}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// **Every secret skein keeps inside the sandbox is under one directory, and the launcher
    /// covers that directory** — ISO-2, and the reason it is a placement test at all.
    ///
    /// The literal is shared with `box-session.sh`, which puts a single `--tmpfs` over exactly this
    /// path. Two spellings of it is a cover that misses, so the string is asserted here rather than
    /// only read: a rename on either side has to fail somewhere, and the alternative is a
    /// credential in the open with every test still green.
    ///
    /// And the box-side destination is asserted to be **outside** the fleet root, which is the
    /// half that is easy to get backwards. `place::Place::crossing` runs a box call's script after
    /// `exec nsenter`, so the `$(cat …)` happens inside the box — where this directory is a tmpfs
    /// with nothing in it. A credential written under the cover for a call that reads it from
    /// inside a box is a reading with no GitHub access and no message about it.
    #[test]
    fn every_secret_skein_keeps_in_the_sandbox_is_under_one_covered_directory() {
        let _g = crate::testutil::env_lock();
        std::env::set_var("SKEIN_FLEET_ROOT", "/boxes");
        let private = fleet_private_dir();
        let stale = stale_sandbox_secrets();
        let (box_dir, box_file) = box_credential_paths("7-0");
        std::env::remove_var("SKEIN_FLEET_ROOT");

        assert_eq!(private, "/boxes/.skein/private");
        // The paths the upgrade has to REMOVE, and none of them may be one it writes: a stale-path
        // helper that returned a current location would delete what it had just installed. The
        // agent's token was the other entry and went with the agent (SKEIN-521); the review
        // credential is still the owner's own GitHub token, which nothing rotates, so an upgraded
        // fleet still has a readable copy to take away.
        assert_eq!(
            stale,
            vec!["/boxes/.skein/review-github.token".to_string()],
            "the readable copies an upgraded fleet still holds are not the ones being removed"
        );
        assert!(
            !stale.iter().any(|p| p.starts_with(&private)),
            "the sweep would delete a credential skein had just written under the cover: {stale:?}"
        );
        assert!(
            !box_file.contains("/boxes") && !box_file.contains(&private),
            "a box call's credential was put where the box cannot read it: {box_file}"
        );
        assert!(
            box_dir.starts_with("\"$HOME\"/") && box_file.starts_with("\"$HOME\"/"),
            "the box side must be a path the box's own shell expands: {box_file}"
        );
    }

    // The layout is load-bearing rather than cosmetic: box-session.sh binds the box's own /tmp and
    // $HOME over the sandbox's, so anything skein must read from OUTSIDE the box — the tmux socket
    // and the anchor pid — has to live somewhere neither bind covers.
    ///
    /// **Under `env_lock`, because every path here is read out of `$SKEIN_FLEET_ROOT`.** Half a
    /// dozen tests in this file and one in `sandbox` set that variable around a fixture root, and
    /// they take the lock; this one only READ it and did not, so it saw whichever fixture root was
    /// current and failed with `/tmp/skein-test-…/boxes/web-main escaped the layout` — about one
    /// run in three, on a machine with enough cores to overlap them. A reader has to take the lock
    /// too: a lock only one side holds is not one.
    ///
    /// **And it pins the root at the shipped default rather than leaving it unset**, which is what
    /// it used to do. `util::fleet_root` now refuses an unpinned test (SKEIN-690), and a fixture
    /// root cannot stand in here: every fixture directory this suite can make is under `/tmp`, and
    /// `/tmp` is the first thing the assertions below forbid. What is under test is the SHIPPED
    /// layout, so `/boxes` is the value the question is about — not a live fleet reached for by
    /// accident. Nothing here opens a path.
    #[test]
    fn a_boxs_paths_avoid_everything_its_namespace_binds_over() {
        let _g = crate::testutil::env_lock();
        std::env::set_var("SKEIN_FLEET_ROOT", "/boxes");
        for path in [
            box_root("web-main"),
            box_sock("web-main"),
            box_pidfile("web-main"),
            box_session_path(),
        ] {
            assert!(path.starts_with("/boxes/"), "{path} escaped the layout");
            assert!(
                !path.starts_with("/tmp/"),
                "{path} is under the private /tmp"
            );
            assert!(!path.contains("/home/"), "{path} is under a private HOME");
        }
        assert_eq!(box_sock("web-main"), "/boxes/web-main/session.sock");
        assert_eq!(box_pidfile("web-main"), "/boxes/web-main/anchor.pid");
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **A box clones the branch it needs, not every branch the repo has** — and can still get the
    /// rest when it wants them.
    ///
    /// Asked for in these words: "do not pull the entire git tree, just pull the branch, if needed
    /// the agent can pull the rest." A plain `git clone` brings every branch; on the fleet this was
    /// measured against, the mirrors run to 134 MB and each box paid for all of it.
    ///
    /// The second half is the part that is easy to get wrong. `--single-branch` does not only
    /// narrow the clone — it narrows `remote.origin.fetch` to that one branch, so a later
    /// `git fetch` in the box would go on bringing nothing and the agent could NOT pull the rest.
    /// Widening the refspec back is what turns a smaller clone into a lazier one.
    ///
    /// **What would make this fail:** dropping `--single-branch` lands `other` at clone time, so
    /// the first assertion goes; dropping the `git config remote.origin.fetch` line leaves the
    /// narrow refspec, `git fetch` brings nothing, and the second one goes.
    #[test]
    fn a_box_clones_one_branch_and_can_still_fetch_the_rest() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("boxes"));

        let run = |script: &str| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(script)
                .output()
                .expect("sh")
        };
        // Two branches, so "only one arrived" is a fact about the clone rather than about a remote
        // that had nothing else to give.
        let git = "git -c user.email=t@example.com -c user.name=test -c init.defaultBranch=main";
        let remote = dir.join("remote.git");
        let seed = dir.join("seed");
        let made = run(&format!(
            "set -e; git init --bare -q -b main {r}; {git} init -q {s}; cd {s}; \
             echo hello > README.md; {git} add -A; {git} commit -qm seed; \
             {git} remote add origin {r}; {git} push -q origin main; \
             {git} checkout -qb other; echo more >> README.md; {git} commit -qam other; \
             {git} push -q origin other",
            r = remote.display(),
            s = seed.display(),
        ));
        if !made.status.success() {
            // Cleared before the refusal, so the panic `skip` raises under
            // `$SKEIN_TESTS_NO_SKIP` unwinds with this variable already put back.
            std::env::remove_var("SKEIN_FLEET_ROOT");
            crate::testutil::skip("no usable git here");
            return;
        }

        let script = clone_script(
            "clone-probe",
            &remote.to_string_lossy(),
            "main",
            "feat/x",
            "",
        );
        let cloned = run(&script);
        assert!(
            cloned.status.success(),
            "the clone script failed: {}",
            String::from_utf8_lossy(&cloned.stderr)
        );

        let tree = format!("{}/tree", box_root("clone-probe"));
        let tracking = |what: &str| {
            String::from_utf8_lossy(
                &run(&format!(
                    "git -C {tree} for-each-ref --format='%(refname:short)' refs/remotes/origin | \
                     grep -c {what} || true"
                ))
                .stdout,
            )
            .trim()
            .to_string()
        };
        assert_eq!(
            tracking("other"),
            "0",
            "the clone brought a branch this box never asked for; on a repo with many branches \
             that is the whole history of every one of them"
        );
        assert_eq!(
            tracking("main"),
            "1",
            "the base branch is the one thing the clone must have — `git merge-base` resolves \
             against it, which is what a review box's diff is"
        );

        let fetched = run(&format!("git -C {tree} fetch origin --quiet"));
        assert!(fetched.status.success(), "the box cannot fetch at all");
        assert_eq!(
            tracking("other"),
            "1",
            "a box that fetches still gets nothing, so what was saved at clone time cannot be \
             recovered on demand — `remote.origin.fetch` is still the narrow one --single-branch \
             wrote"
        );
        // Put back, exactly as the neighbour below does: this variable is process-wide, and
        // a test that leaves it set decides what `fleet_root()` answers for every test after it.
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }
}
