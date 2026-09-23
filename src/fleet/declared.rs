//! What skein decides about a box — privileged, uncovered, identity, disk allowance — kept
//! where the box itself cannot write it.

use super::*;

/// Is this the workshop box — the one that may see every box's files and act at fleet scope?
///
/// Ordinary boxes get a mount namespace that hides the other boxes' directories, and an empty file
/// over the fleet agent's token. That is right for a box doing a repo's work and wrong for the box
/// used to debug and extend skein itself, which needs to read the fleet to be any use at all.
///
/// **Off unless the file says exactly `1`.** Anything else — absent, empty, half-written, corrupted
/// — is off, because the two failure directions are nothing like each other: guessing "privileged"
/// hands one box every other box's credentials, and guessing "ordinary" costs a restart.
pub fn box_is_privileged(name: &str) -> bool {
    declared_read(name, "privileged").unwrap_or_default().trim() == "1"
}

/// Make `name` the workshop box, or return it to being ordinary. Takes effect at its **next start**:
/// a namespace is built when a box comes up, and a running box already has the one it was given.
///
/// Deliberately not exclusive — skein does not clear the flag on other boxes when one is set. Two
/// privileged boxes is a thing someone may want and the cockpit shows plainly; silently un-privileging
/// a box someone is working in, because they ticked a box elsewhere, is not.
pub fn set_box_privileged(name: &str, on: bool) -> Result<(), String> {
    if !crate::util::valid_name(name) {
        return Err(format!("unusable box name {name:?}"));
    }
    match on {
        false => declared_clear(name, "privileged"),
        true => declared_write(name, "privileged", b"1"),
    }
}

/// Has somebody deliberately allowed this box to start with no mount cover?
///
/// The other half of [`refuse_if_uncovered`]. An uncovered box reaches every other repository's
/// store and work tree, and on a fleet whose state sits on a mounted volume it reaches the volume
/// holding `credentials/`, `api-token` and `github-pats/` — the same reach the workshop box has,
/// and until now the only difference between them was that the workshop box was *chosen*. So it is
/// chosen here too, through the same machinery and for the same reason: **`declared/`, not the
/// box's own state directory** (§9.5 R8). A box that could write this file could hand itself the
/// fleet, which is exactly the vulnerability [`box_declared`] exists to have closed.
///
/// **Off unless the file says exactly `1`**, like [`box_is_privileged`] and by the same argument:
/// guessing "allowed" starts an exposed box silently, and guessing "not" costs one command.
///
/// Harmless once stale. It is only ever read for a box that is *already* uncovered, so a box that
/// later matches a repository is covered whatever this says, and nothing has to clear it.
pub fn uncovered_is_allowed(name: &str) -> bool {
    declared_read(name, "uncovered").unwrap_or_default().trim() == "1"
}

/// Let `name` start with no mount cover, or take that permission away.
///
/// Written by `skein start <box> --uncovered`, which is the deliberate act [`refuse_if_uncovered`]
/// names. It persists on purpose: the refusal is met once, and every later restart, attach and
/// heal of that box would otherwise meet it again with no way to answer from where the person is
/// standing — `ensure_box_session` runs inside the server, where nobody is typing flags.
pub fn allow_uncovered(name: &str, on: bool) -> Result<(), String> {
    if !crate::util::valid_name(name) {
        return Err(format!("unusable box name {name:?}"));
    }
    match on {
        false => declared_clear(name, "uncovered"),
        true => declared_write(name, "uncovered", b"1"),
    }
}

/// One box's durable host-side state directory — **which the box reads and does not write.**
///
/// The doc here said "and the box can write it" for a long time and had stopped being true. The
/// launcher covers the state parent with a tmpfs and binds this directory back `--ro-bind`
/// (`box-session.sh`, the state cover), and `tests/isolation_bwrap.rs` reads it back from a real
/// namespace as `see` rather than `write`. What the box writes are its conversation and
/// transcripts, and those reach it through a *different, read-write* mount of the same directories
/// at `$HOME/.claude/projects` and `$HOME/.codex/sessions` — separate mounts, so read-only here
/// costs nothing there. `git-tokens/` is the other half and is the reason: the host mints and
/// places those, and a box that could write this path could write itself a token for a repository
/// it was never given.
///
/// It is still the wrong home for anything skein *decides*, which is what [`box_declared`] exists
/// for, and the read-only bind is what makes putting one back here impossible rather than merely
/// discouraged (SKEIN-7). Reported state here; declared state there (§2.1).
pub fn box_state(name: &str) -> String {
    format!("{}/{name}", box_state_root())
}

/// Where skein keeps what it has **decided** about a box, as opposed to what the box reports.
///
/// Host-only, and never in [`fleet_mounts`] — so it is not merely covered inside a box's namespace,
/// it is not in the sandbox at all. That is the difference between this and a bind a box could be
/// handed by a launcher that skipped a cover.
///
/// **Why it is a separate directory and not a file mode.** Four security-deciding answers used to
/// live in `box_state`, beside the conversation, in a directory the box writes read-write:
/// `privileged`, `git-scope`, `disk` and `identity`. From inside any box, `echo 1 > .../privileged`
/// made it the workshop box at its next start — every isolation bind skipped and the fleet agent's
/// token readable, which runs scripts as root at fleet scope and reaches every other box's
/// credentials and conversations. `git-scope` was the same shape for the account-wide GitHub token,
/// `disk` for its own allowance on a shared disk, `identity` for who it commits as.
///
/// None of those was a bug in how the file was read. Each was a decision stored where the party it
/// constrains could write it, so the fix is where it lives rather than how it is parsed.
pub fn box_declared(name: &str) -> std::path::PathBuf {
    skein_home().join("declared").join(name)
}

/// Boxes whose abandoned pre-split files have already been mentioned, so a per-tick reader does not
/// say it every tick.
static SAID_ABANDONED: std::sync::Mutex<Option<std::collections::BTreeSet<String>>> =
    std::sync::Mutex::new(None);

/// Read one declared answer, or `None`.
///
/// A value left at the OLD path is **ignored, not migrated**, and said out loud once. Migrating it
/// would carry the vulnerability across: any box could have written any of those files at any point
/// before this existed, so a value found there proves nothing about who chose it. The cost of
/// ignoring is that someone re-ticks a setting; the cost of trusting is the workshop box.
pub fn declared_read(name: &str, flag: &str) -> Option<String> {
    if !crate::util::valid_name(name) {
        return None;
    }
    let fresh = std::fs::read_to_string(box_declared(name).join(flag)).ok();
    if fresh.is_none() {
        let stale = std::path::Path::new(&box_state(name)).join(flag);
        if stale.exists() {
            let mut said = SAID_ABANDONED.lock().unwrap_or_else(|e| e.into_inner());
            let seen = said.get_or_insert_with(Default::default);
            if seen.insert(format!("{name}/{flag}")) {
                eprintln!(
                    "skein: {name} has an old {flag} setting at {} — ignored, because that \
                     directory is writable from inside the box. Set it again in the cockpit and \
                     delete the old file.",
                    stale.display()
                );
            }
        }
    }
    fresh
}

pub(crate) fn declared_write(name: &str, flag: &str, body: &[u8]) -> Result<(), String> {
    if !crate::util::valid_name(name) {
        return Err(format!("unusable box name {name:?}"));
    }
    let dir = box_declared(name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    write_atomic(&dir.join(flag), &dir, body)
}

pub(crate) fn declared_clear(name: &str, flag: &str) -> Result<(), String> {
    if !crate::util::valid_name(name) {
        return Err(format!("unusable box name {name:?}"));
    }
    match std::fs::remove_file(box_declared(name).join(flag)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// This box's disk allowance in MiB: its own if it has one, else the fleet-wide default, `None` for
/// unlimited. Read at every check, so changing it takes effect on the next refresh — no restart.
pub fn box_disk_limit(name: &str) -> Option<u64> {
    let own = declared_read(name, "disk").map(|s| s.trim().to_string());
    match own {
        // An empty override is a decision — this box is allowed to use the whole disk.
        Some(v) if v.is_empty() => None,
        Some(v) => parse_mib(&v),
        None => parse_mib(&load_config().box_disk_max),
    }
}

/// Give one box a different allowance, or hand it back to the default. Takes effect immediately.
pub fn set_box_disk_limit(name: &str, limit: Option<&str>) -> Result<(), String> {
    let Some(limit) = limit else {
        return declared_clear(name, "disk");
    };
    // Three states, and only two of them are a size: `none` says this box may use the whole disk,
    // which is different from having no opinion (that is `None`, and inherits the default).
    let limit = match limit.trim().to_lowercase().as_str() {
        "none" | "unlimited" => "",
        _ => limit.trim(),
    };
    if !limit.is_empty() && parse_mib(limit).is_none() {
        return Err(format!(
            "{limit:?} is not a size — try 10g, 512m, or `none` for unlimited"
        ));
    }
    declared_write(name, "disk", limit.as_bytes())
}

/// Who a box commits as: its own choice, else the configured default, else this host's git config.
///
/// Three sources because each answers a different question. A box set at creation is working on
/// someone else's behalf — a shared machine, a different identity per client. The setting is the
/// answer for everything else. And falling back to this host's git config means an untouched skein
/// commits as you without anyone configuring anything.
///
/// "This host's git config" means its **global** one, and the scope is load-bearing rather than
/// incidental — see `from_host` below (SKEIN-541).
pub fn box_identity(name: &str) -> (String, String) {
    let config = load_config();
    // The host's own git identity. This used to ask an adopted repo's checkout first, for the
    // per-repo identity somebody may have set in its `.git/config` — there is no checkout to ask
    // now, and a URL repo never had one worth asking.
    //
    // **Scoped, and never unscoped** (SKEIN-541). `git config --get` with no scope also reads the
    // *repository* config of whatever directory this process happens to be standing in, and
    // repository config outranks global — so a `skein-server` started inside any checkout adopted
    // that repo's committer as "the host's", and `identity_script` two functions down already says
    // `--global`, which made the two disagree about the same question. The cwd of a daemon is not
    // an answer to "who is this person".
    //
    // It surfaced as a test rather than as a wrong commit, and that is the mild end of it: setting
    // a per-repo identity right after cloning is ordinary practice — near-universal for anybody who
    // contributes to work and personal repositories from one machine — so a contributor's first
    // `cargo test --all` failed, in a test whose name is about box provisioning.
    //
    // `--system` after `--global` rather than instead of it: an identity in `/etc/gitconfig` is
    // unusual but is still this host saying who it is, and dropping it would take a fleet that
    // commits today and give it `Author identity unknown` at the end of the first turn.
    let from_host = |key: &str| -> String {
        ["--global", "--system"]
            .iter()
            .find_map(|scope| {
                std::process::Command::new("git")
                    .args(["config", scope, "--get", key])
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or_default()
    };
    let pick = |own: Option<String>, configured: &str, key: &str| -> String {
        own.filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| match configured.trim() {
                "" => from_host(key),
                v => v.to_string(),
            })
    };
    let own = box_identity_override(name);
    (
        pick(own.clone().map(|(n, _)| n), &config.git_name, "user.name"),
        pick(own.map(|(_, e)| e), &config.git_email, "user.email"),
    )
}

/// A box's own committer, recorded at creation. `name\nemail`, beside its other durable state.
pub fn box_identity_override(name: &str) -> Option<(String, String)> {
    let raw = declared_read(name, "identity")?;
    let mut lines = raw.lines();
    Some((
        lines.next().unwrap_or_default().trim().to_string(),
        lines.next().unwrap_or_default().trim().to_string(),
    ))
}

/// Record (or clear) that committer. `None` returns the box to the configured default.
pub fn set_box_identity(name: &str, who: Option<(&str, &str)>) -> Result<(), String> {
    let Some((who_name, email)) = who else {
        return declared_clear(name, "identity");
    };
    let body = format!("{}\n{}\n", who_name.trim(), email.trim());
    declared_write(name, "identity", body.as_bytes())
}

/// Set that identity inside the box, so its first commit is not `Author identity unknown`.
///
/// `--global` (the box's own HOME), not the repo: the checkout is re-cloned by a rebuild, a resize
/// or a migration, and a repo-local setting goes with it every time. Only what is missing is
/// written, so an identity someone set in the box by hand is never overwritten.
pub(super) fn identity_script(name: &str, email: &str) -> String {
    let mut steps = Vec::new();
    if !name.trim().is_empty() {
        steps.push(format!(
            "git config --global --get user.name >/dev/null 2>&1 || git config --global user.name {}",
            sh_quote(name.trim())
        ));
    }
    if !email.trim().is_empty() {
        steps.push(format!(
            "git config --global --get user.email >/dev/null 2>&1 || git config --global user.email {}",
            sh_quote(email.trim())
        ));
    }
    steps.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// Nothing skein DECIDES about a box lives where that box can write.
    ///
    /// This is the bug, stated as a property. All four of these used to sit in `box_state`, which is
    /// bound read-write into the box because the conversation and the git tokens live there. From
    /// inside any box: `echo 1 > .../privileged`, and at its next start every isolation bind is
    /// skipped and the fleet agent's token is readable — and that token runs scripts as root at
    /// fleet scope, reaching every other box's credentials and conversations. `git-scope` was the
    /// same shape for the account-wide GitHub token, `disk` for its own allowance on a shared disk,
    /// `identity` for who it commits as.
    ///
    /// Written as "not under the mounted root" rather than "under this exact path", because the
    /// property is what matters: a future flag put back beside the conversation fails this, and so
    /// does mounting the declared directory into the sandbox.
    #[test]
    fn what_skein_decides_about_a_box_is_not_where_the_box_can_write_it() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        let declared = box_declared("web-main");
        assert!(
            !declared.starts_with(box_state_root()),
            "declared state is back inside the directory boxes write: {}",
            declared.display()
        );
        // And not merely covered inside a namespace — not in the sandbox at all, so a launcher that
        // skipped a cover could not hand it over either.
        for mount in fleet_mounts() {
            assert!(
                !declared.starts_with(&mount),
                "{} is mounted into the sandbox, so {} is reachable from a box",
                mount,
                declared.display()
            );
        }

        // Each of the four round-trips through the new home, and none of them writes the old one.
        set_box_privileged("web-main", true).unwrap();
        set_box_disk_limit("web-main", Some("4g")).unwrap();
        set_box_identity("web-main", Some(("A Dev", "dev@example.com"))).unwrap();
        crate::gitgate::set_box_scope("web-main", Some("fleet")).unwrap();
        assert!(box_is_privileged("web-main"));
        assert_eq!(box_disk_limit("web-main"), Some(4096));
        assert_eq!(
            box_identity_override("web-main"),
            Some(("A Dev".into(), "dev@example.com".into()))
        );
        assert!(!crate::gitgate::box_is_scoped("web-main"));

        let box_writable = std::path::Path::new(&box_state("web-main")).to_path_buf();
        for flag in ["privileged", "disk", "identity", "git-scope"] {
            assert!(
                !box_writable.join(flag).exists(),
                "{flag} was written where the box can rewrite it"
            );
        }
    }

    /// A value left at the old path is ignored, not migrated.
    ///
    /// Migrating would carry the vulnerability across: any box could have written any of these at
    /// any point before the split, so a value found there proves nothing about who chose it. The
    /// cost of ignoring is that someone re-ticks a setting once.
    #[test]
    fn a_setting_left_where_a_box_could_have_written_it_is_not_believed() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        let stale = std::path::PathBuf::from(box_state("web-main"));
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("privileged"), "1").unwrap();
        std::fs::write(stale.join("git-scope"), "fleet").unwrap();

        assert!(
            !box_is_privileged("web-main"),
            "a box that promoted itself before the split stays promoted"
        );
        // `fleet` in the old file must not un-scope the credential either; with nothing declared,
        // the answer comes from the fleet default rather than from the box.
        assert_eq!(
            crate::gitgate::declared_scope("web-main"),
            None,
            "the abandoned override was read as an answer"
        );
    }

    /// A half-failed restart leaves a session tmux answers for and no crossing can enter: launched,
    /// stamp never rewritten. Seen live (lattice-feat-design-codex-claude, 2026-08-24): the sweep's
    /// socket fallback called it alive, `ensure_box_session` believed it, and every attach refused
    /// at the guard — forever, because nothing on any path re-stamped. Alive is not the question;
    /// addressable is.
    #[test]
    fn a_live_session_the_record_cannot_address_is_ended_and_relaunched_not_believed() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // Pinned because this reaches a `Place`, and because the liveness sweep now READS this
        // root rather than asking a sandbox about it. Unpinned it is `/boxes` — the machine's live
        // fleet (SKEIN-530).
        let root = home.join("fleet");
        std::env::set_var("SKEIN_FLEET_ROOT", &root);
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();

        // Each box gets a directory and a socket something is listening on, because that is what
        // the sweep asks: a record stamped by another boot (or by none) is undecidable from its
        // anchor, and the fallback is whether the box's socket accepts. **All three read as alive**,
        // which is the setup — an orphan is precisely a session that answers.
        let mut listening = Vec::new();
        let place = |name: &str, pid: u32, generation: &str, ns_start: u64| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("session.sock");
            let held = std::os::unix::net::UnixListener::bind(&sock).expect("a listener");
            crate::place::record_place(
                name,
                &PlaceRecord {
                    sandbox: "skein-fleet".into(),
                    ns_pid: pid,
                    home: format!("/fleet/{name}/home"),
                    tree: format!("/fleet/{name}/tree"),
                    sock: sock.to_string_lossy().into_owned(),
                    generation: generation.into(),
                    ns_start,
                    ..Default::default()
                },
            )
            .unwrap();
            // None of these three belongs to a repository — there is no `repos.json` in this
            // fixture at all — so every one of them is uncovered, and `ensure_box_session` refuses
            // an uncovered box unless it has been allowed (SKEIN-846). Said here because it is a
            // property of the fixture: what this test is about is the anchor stamp, not the cover.
            crate::fleet::allow_uncovered(name, true).unwrap();
            held
        };
        // The wedge: stamped by the previous boot, while every probe below answers from "boot-b".
        listening.push(place("wedged-box", 4242, "boot-a", 900));
        // A healthy neighbour: stamped by the current boot, and its pid still is that process.
        listening.push(place("sound-box", 4243, "boot-b", 900));
        // A record from before stamps existed: alive and unprovable, which must be left alone.
        listening.push(place("elder-box", 4244, "", 0));

        let log = home.join("argv.log");
        let stopped = home.join("stopped");
        // **The seam, not a fake `sbx` on `$PATH`.** The fake was this test's whole oracle — every
        // script skein sent crossed a process boundary it owned. There is no hop to own now
        // (SKEIN-576), so it would be bypassed and each of these scripts would run for real, on
        // this machine (SKEIN-592). The dispatch is the same, in Rust: the wedged anchor reads as
        // the previous boot, the relaunch reports a fresh anchor, and the fresh anchor stamps as
        // the current boot.
        let (log_at, stop_at) = (log.clone(), stopped.clone());
        let _stood_in = crate::place::seam::install(Box::new(move |argv: &[String]| {
            use std::io::Write;
            let all = argv.join(" ");
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_at)
                .unwrap();
            writeln!(f, "{all}").unwrap();
            let answer = if all.contains("tree=0; sess=0") {
                // Before the stop: a tree and a session. After it: a tree and no session, which is
                // what sends the relaunch.
                format!(
                    "if [ -f {s} ]; then echo 10; else echo 11; fi",
                    s = stop_at.display()
                )
            } else if all.contains("cgroup.kill") {
                format!(": > {s}", s = stop_at.display())
            } else if all.contains("box-session") {
                "echo 'SKEIN_ANCHOR 5001'".to_string()
            } else if all.contains("/proc/4242/stat") || all.contains("/proc/4243/stat") {
                "echo 'boot-b 900'".to_string()
            } else if all.contains("/proc/5001/stat") {
                "echo 'boot-b 901'".to_string()
            } else {
                String::new()
            };
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                format!("cat >/dev/null; {answer}"),
            ])
        }));

        ensure_box_session("wedged-box").expect("the wedge heals rather than erroring");
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        // `cgroup.kill` and not `kill-server`: the stop no longer runs a tmux client on a socket
        // under the box's own root (ISO-6), so the cgroup write IS the stop.
        let ended = argv
            .find("cgroup.kill")
            .expect("the unaddressable session was believed instead of ended");
        let relaunched = argv
            .find("SKEIN_FLEET_LIMITS")
            .expect("the session was ended but never relaunched");
        assert!(
            ended < relaunched,
            "the relaunch ran before the orphan was ended:\n{argv}"
        );
        let after = shared_record("wedged-box").expect("the record survives the heal");
        assert_eq!(
            (after.generation.as_str(), after.ns_pid, after.ns_start),
            ("boot-b", 5001, 901),
            "the record was not re-stamped, so the next crossing would refuse again"
        );

        // The healthy neighbour is believed: no session ended, nothing relaunched.
        std::fs::write(&log, "").unwrap();
        ensure_box_session("sound-box").expect("a sound box is a no-op");
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !argv.contains("kill-server") && !argv.contains("SKEIN_FLEET_LIMITS"),
            "a sound box was restarted for being checked:\n{argv}"
        );

        // Unprovable is not orphaned: ending a session on "cannot tell" could end a working box.
        std::fs::write(&log, "").unwrap();
        ensure_box_session("elder-box").expect("an unprovable record is left to the guard");
        let argv = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !argv.contains("kill-server"),
            "a box that predates anchor stamps was killed on a guess:\n{argv}"
        );

        drop(listening);
        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// A box has a private HOME and a freshly cloned tree, so it starts with no committer at all —
    /// and finds out at `git commit`, which is after the work, not before it.
    ///
    /// **The identity this reads is the fixture's, whatever the person running it has configured**
    /// (SKEIN-541). It used to be neither: `GIT_CONFIG_GLOBAL` was pinned here and looked like
    /// ownership, while `box_identity` asked `git config --get` with no scope — which reads the
    /// *repository* config of whatever directory the test process is standing in, and repository
    /// config outranks global. So the pin was decoration, this test asserted "nothing configured at
    /// repo level", and a contributor who ran `git config user.email …` in their clone — the first
    /// thing many people do — got a red suite pointing at box provisioning.
    ///
    /// `GIT_DIR` is how the repository scope is arranged here rather than by changing the process's
    /// directory: cwd is process-global and `env_lock` does not cover it, and the local config of
    /// this very checkout is shared between every worktree on the machine, so writing one would
    /// reach three other lanes. Pointing `GIT_DIR` at the fixture's own repo is the same question
    /// asked hermetically.
    ///
    /// **What would make this fail**: dropping the scope from `from_host`. Proved — putting
    /// `["config", "--get", key]` back made the first assertion read `Repo Level`.
    #[test]
    fn a_box_is_told_who_it_commits_as_before_it_needs_to_know() {
        let _g = env_lock();
        let home = tempdir();
        // Deliberately NOT setting SKEIN_FLEET_ROOT: nothing here reads it, and a test that sets a
        // global other tests read is a test that breaks them from another thread.
        std::env::set_var("SKEIN_HOME", &home);
        let work = home.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .expect("git");
        };
        // The host's own identity, in a config this test owns. `box_identity` asks
        // `git config --global`, which used to be aimed at an adopted repo's checkout with `-C` —
        // so the fixture writes a global one rather than a repo-local one.
        let gitconfig = home.join("gitconfig");
        std::fs::write(
            &gitconfig,
            "[user]\n\tname = Host Default\n\temail = host@example.com\n",
        )
        .unwrap();
        std::env::set_var("GIT_CONFIG_GLOBAL", &gitconfig);
        git(&["init", "-q"]);
        // **And a repository-level identity that must lose**, which is the half that was missing.
        // Written into the fixture's own repo and pointed at with `GIT_DIR`, so the answer cannot
        // depend on which directory the suite happens to run in — the condition that made this test
        // fail on a contributor's machine and pass on everybody else's.
        std::fs::write(
            work.join(".git").join("config"),
            "[core]\n\trepositoryformatversion = 0\n\
             [user]\n\tname = Repo Level\n\temail = repo@example.invalid\n",
        )
        .unwrap();
        std::env::set_var("GIT_DIR", work.join(".git"));
        let _repo = Repo {
            read_prs: false,
            id: "web".into(),
            source: work.to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };

        save_config(&Config::default()).unwrap();
        assert_eq!(
            box_identity("web-main"),
            ("Host Default".into(), "host@example.com".into()),
            "with nothing configured, this host's global git config already knows — asking the \
             user would be a question skein can answer itself. `Repo Level` here means the answer \
             came from a repository's config, which for a daemon is whichever directory it was \
             started in (SKEIN-541)"
        );

        save_config(&Config {
            git_name: "Fleet".into(),
            git_email: "fleet@example.com".into(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            box_identity("web-main").0,
            "Fleet",
            "the setting is the answer for every box that did not choose one"
        );

        set_box_identity("web-main", Some(("Client A", "a@client.example"))).unwrap();
        assert_eq!(
            box_identity("web-main"),
            ("Client A".into(), "a@client.example".into()),
            "a box created on someone else's behalf commits as them"
        );
        assert_eq!(
            box_identity("web-other").0,
            "Fleet",
            "and only that box — its siblings keep the default"
        );

        set_box_identity("web-main", None).unwrap();
        assert_eq!(box_identity("web-main").0, "Fleet");

        // --global, because the checkout is re-cloned by every rebuild, resize and migration; and
        // never over an identity already set inside the box.
        let script = identity_script("Fleet", "fleet@example.com");
        assert!(script.contains("git config --global user.name 'Fleet'"));
        assert!(
            script.contains("--get user.name >/dev/null 2>&1 ||"),
            "only what is missing: an identity set in the box by hand is someone's choice: {script}"
        );
        assert!(
            identity_script("", "").is_empty(),
            "nothing configured and nothing on the host ⇒ nothing to run"
        );

        // **`/etc/gitconfig` is still this host saying who it is.** The scope fix could have been
        // `--global` alone, which is what `identity_script` writes; it is `--global` then
        // `--system` because a machine whose only identity is the system one commits today, and
        // would have got `Author identity unknown` at the end of its first turn instead. An arm
        // with no test is an arm somebody deletes as dead.
        //
        // Fails on: dropping `"--system"` from the scopes, which leaves this reading empty.
        let systemwide = home.join("systemconfig");
        std::fs::write(&systemwide, "[user]\n\tname = System Wide\n").unwrap();
        std::fs::write(&gitconfig, "").unwrap();
        std::env::set_var("GIT_CONFIG_SYSTEM", &systemwide);
        save_config(&Config::default()).unwrap();
        assert_eq!(
            box_identity("web-main").0,
            "System Wide",
            "a host whose identity lives in /etc/gitconfig has one, and a box that came up without \
             it finds out at `git commit`"
        );
        std::env::remove_var("GIT_CONFIG_SYSTEM");
        // `GIT_DIR` especially: it names a directory this test's guard is about to remove, and left
        // set it would point every later `git` in this process at a repository that is not there.
        std::env::remove_var("GIT_DIR");
        std::env::remove_var("GIT_CONFIG_GLOBAL");
        std::env::remove_var("SKEIN_HOME");
    }

    /// Memory has a kernel ceiling per box; disk has one filesystem and no ceiling at all. So the
    /// allowance is a number skein measures against — which is exactly what lets it change under a
    /// running box, and why it must never be described as a quota.
    #[test]
    fn a_boxs_disk_allowance_is_its_own_and_changes_without_a_restart() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);

        save_config(&Config::default()).unwrap();
        assert_eq!(
            box_disk_limit("web-main"),
            Some(10 * 1024),
            "10g by default — a fleet where every box may take the whole disk has no default at all"
        );

        set_box_disk_limit("web-main", Some("40g")).unwrap();
        assert_eq!(box_disk_limit("web-main"), Some(40 * 1024));
        assert_eq!(
            box_disk_limit("web-other"),
            Some(10 * 1024),
            "one box's allowance is not a decision about the rest"
        );

        // `none` is a decision — this box may use the whole disk — and distinct from having no
        // opinion, which is what a cleared field means and inherits the default again.
        set_box_disk_limit("web-main", Some("none")).unwrap();
        assert_eq!(box_disk_limit("web-main"), None);
        set_box_disk_limit("web-main", Some("unlimited")).unwrap();
        assert_eq!(box_disk_limit("web-main"), None);
        set_box_disk_limit("web-main", None).unwrap();
        assert_eq!(box_disk_limit("web-main"), Some(10 * 1024));

        // A size that parses to nothing would read as "limited" and behave as "unlimited".
        assert!(set_box_disk_limit("web-main", Some("plenty")).is_err());
        assert_eq!(box_disk_limit("web-main"), Some(10 * 1024));

        save_config(&Config {
            box_disk_max: String::new(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(
            box_disk_limit("web-main"),
            None,
            "blank default ⇒ unlimited"
        );
        std::env::remove_var("SKEIN_HOME");
    }
}
