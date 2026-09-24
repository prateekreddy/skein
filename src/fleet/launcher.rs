//! The launcher installed into the sandbox: its revision stamp, the cover text, and reading
//! what it reported back about a start.

use super::*;

/// Whether a box is running under the cover this build of skein installs.
///
/// One file read, the same record the board already opens for every row, and no subprocess — which
/// is the constraint that shaped this. The obvious implementation asks the box, and the board's
/// cost is measured (`tests/board_cost.rs`); a per-row exec to answer a question whose answer
/// changes only when somebody upgrades skein would be the most expensive cheap thing on the tick.
///
/// [`crate::place::PlaceRecord::launcher`] says why an empty revision reads as *older* rather than
/// as *unknown*.
///
/// Here rather than beside the record it reads, and the module graph is the reason: `place` does
/// not depend on `fleet` — SKEIN-22 removed the one edge it had — and this comparison needs the
/// launcher, which is `fleet`'s. A method on `PlaceRecord` would put that edge back to save an
/// import, and `module-check` said so within a minute of it being written.
pub fn cover_is_current(name: &str, record: &crate::place::PlaceRecord) -> bool {
    !record.launcher.is_empty()
        && record.launcher == launcher_revision()
        // **And the peer switch, which the revision cannot see.** `launcher_revision` hashes
        // `box-session.sh`, and `peer_messaging` lives in `repos.json` — flipping it changes no byte
        // of that script, so a box still running the mount it was born with looked exactly like one
        // running the current cover. `is_none_or` is the third answer: a launcher too old to say
        // leaves the box alone rather than being read as either position (SKEIN-572).
        && record
            .peers
            .is_none_or(|born| born == crate::repos::box_is_on_the_peer_network(name))
}

/// The line in `box-session.sh` that [`install_launcher`] replaces with [`launcher_revision`].
const LAUNCHER_REVISION_MARK: &str = "@SKEIN_LAUNCHER_REVISION@";

/// What isolation this build of skein gives a box — content-derived from the launcher's own body.
///
/// A box keeps the mount namespace it was born with. `install_launcher` refreshes the script in the
/// sandbox on every start and every heal, and none of that reaches a box that is already up: it is
/// covered by whatever `box-session.sh` said on the day it started, until somebody restarts it. So
/// there has to be a value that travels with the box, and this is it.
///
/// **Comments are cut, and nothing else is.** The narrower version — hash only the lines carrying a
/// bwrap mount directive — was written first and is wrong, for a reason worth keeping: the cover is
/// not only the binds, it is the conditions around them. `/run/user` and `/run/secrets` are tmpfs'd
/// for *non-privileged* boxes only, so a change to which boxes get that cover moves no `--tmpfs`
/// line at all, and a revision over the bind lines would go silent on exactly the class of change
/// this exists to catch. Coarse in the safe direction: a reworded error message asks somebody to
/// restart a box that did not need it, which costs a restart; the other direction costs the cover.
///
/// Comments are cut because they are the one edit that can never change what a box can reach, and
/// they are most of this file — the same argument [`crate::probes`]'s `probe_revision` makes for
/// hashing generated hook wiring rather than the probe scripts' bodies.
///
/// Computed once. It is a hash of a compile-time constant, so it cannot change while the process
/// runs — and the caller is the board, which asks it once per box per tick.
pub fn launcher_revision() -> String {
    static REVISION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    REVISION.get_or_init(|| revision_of(BOX_SESSION_SH)).clone()
}

/// Would this text behave differently from that one — the hash behind [`launcher_revision`], over
/// [`cover_text`].
///
/// Private to the module on purpose: the production answers are about the two embedded scripts,
/// and a public version taking any script would invite a caller to ask about the copy on disk —
/// which is the question this whole mechanism exists because nobody can answer.
fn revision_of(script: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for line in cover_text(script) {
        for byte in line.bytes().chain(std::iter::once(b'\n')) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("{hash:016x}")
}

/// A script with its comments and blank lines cut — what [`revision_of`] hashes. `#` opens a
/// whole-line comment in shell and in Python both, which is what lets one cut serve the launcher
/// and the agent.
///
/// Whole-line comments only. A `#` mid-line is a comment in shell and is also a character inside
/// quite ordinary strings here (`#{pid}` is tmux's format language, in the line that reports the
/// anchor), and a cut that took it would drop the anchor report from the revision.
fn cover_text(script: &str) -> impl Iterator<Item = &str> {
    script
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim_start().is_empty() && !line.trim_start().starts_with('#'))
}

/// The launcher's bytes with the revision of those bytes stamped into them.
///
/// Stamped rather than passed at launch, and the difference is the whole mechanism. A revision
/// skein handed the script on its command line would report what *skein* was running; the question
/// is what the *script* does, and the two disagree in precisely the case that matters — a sandbox
/// carrying a launcher older than the binary talking to it.
fn stamped_launcher() -> String {
    BOX_SESSION_SH.replace(LAUNCHER_REVISION_MARK, &launcher_revision())
}

/// What the launcher said about this box's ceiling: `capped <limits>`, or `uncapped <reason>`.
///
/// Reported rather than read, because there is nothing on the host to read. The launcher writes
/// `limits.state` into the box's own root, which is inside the sandbox — so "nothing reads that
/// file" was true for as long as it existed, and an uncapped box looked exactly like a capped one
/// from every surface skein has. It travels the way the anchor and the launcher revision do: over
/// the channel skein already has open, into the placement record, where a board tick reads it for
/// nothing.
///
/// Empty where no launcher answered — a record from before this, a launcher too old to print it, or
/// the adoption path. Read as *unknown*, not as *capped*: the whole point is that a box with no
/// ceiling is invisible, and defaulting to the reassuring answer would rebuild that.
pub fn limits_from_launch(out: &str) -> String {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("SKEIN_LIMITS "))
        .next_back()
        .map(|state| state.trim().to_string())
        .unwrap_or_default()
}

/// Whether the launcher put this box on the peer network — `Some(true)`, `Some(false)`, or **`None`
/// for a launcher that did not say**.
///
/// Same shape as [`limits_from_launch`] and travelling the same way, for the same reason: the
/// switch is in `repos.json` and flipping it changes no byte of `box-session.sh`, so nothing about
/// the launcher's own revision can tell you which side a running box is on. It has to come from the
/// box's birth.
///
/// The third answer is the point. `None` is a record from before this, or a launcher too old to
/// print `SKEIN_PEERS`, and [`cover_is_current`] leaves such a box alone rather than reading the
/// silence as either position — see [`crate::place::PlaceRecord::peers`].
pub fn peers_from_launch(out: &str) -> Option<bool> {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("SKEIN_PEERS "))
        .next_back()
        .map(|said| said.trim() == "1")
}

/// Everything the launcher asked skein to tell the person who started this box.
///
/// **This is the delivery half of SKEIN-846.** `box-session.sh` announced the workshop box, and
/// then the uncovered box beside it, with `echo … >&2` — and on the success path stderr goes
/// nowhere: [`crate::place::Place::bytes`] pipes it and reads it only in its `!status.success()`
/// branch, so every word of both banners was discarded by the one path that always runs. The
/// launcher writes them on STDOUT now, as `SKEIN_NOTICE <one line>`, which is the channel skein
/// already reads the anchor pid, the launcher revision, the ceiling and the peer flag off.
///
/// All of them, in order, rather than `next_back()` like its neighbours: those parse a fact with
/// one current value, where a second line means the first is stale. These are sentences, and a
/// launcher with two things to say must not have one of them silently dropped.
pub fn notices_from_launch(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("SKEIN_NOTICE "))
        .map(|said| said.trim().to_string())
        .filter(|said| !said.is_empty())
        .collect()
}

/// Put what the launcher said in front of the person who ran the command.
///
/// Plain `eprintln!`, which is the point: this process is `skein start` or `skein attach` — run
/// from a terminal, or in the PTY the cockpit opens for it — so its stderr is a screen somebody is
/// looking at, which is exactly what the launcher's own stderr was not.
pub(super) fn say_what_the_launcher_said(out: &str) {
    for notice in notices_from_launch(out) {
        eprintln!("skein: {notice}");
    }
}

/// Whether a reported ceiling state is one that actually bounds the box.
///
/// The first word is the answer and the rest is the reason, so this is a prefix test rather than a
/// list of the three ways to be uncapped — a fourth reason added in the launcher tomorrow is
/// reported correctly by a host that has never heard of it.
pub fn is_capped(state: &str) -> bool {
    state.split_whitespace().next() == Some("capped")
}

/// The revision the launcher reported on stdout, or empty if it reported none.
///
/// Empty is a real answer and not a failure: a launcher old enough to predate this line cannot say
/// anything, and neither can the adoption path, where no launcher runs at all. Both mean the same
/// thing — this box's cover is not known to be the current one — and [`crate::place::PlaceRecord`]
/// stores it as such rather than guessing.
pub fn launcher_from_launch(out: &str) -> String {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("SKEIN_LAUNCHER "))
        .next_back()
        .map(|rev| rev.trim().to_string())
        .filter(|rev| !rev.is_empty() && rev != LAUNCHER_REVISION_MARK)
        .unwrap_or_default()
}

/// Write `box-session.sh` and the provisioning script into the sandbox, over stdin rather than as
/// arguments — both are large and `sbx exec`'s argv is visible in every process listing on the host.
pub fn install_launcher(sandbox: &str) -> Result<(), String> {
    let launcher = stamped_launcher();
    for (path, body) in [
        (box_session_path(), launcher.as_str()),
        (box_provision_path(), KIT_STARTUP_SH),
        (git_credential_helper_path(), GIT_CREDENTIAL_SH),
    ] {
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/boxes");
        let script = format!(
            "mkdir -p {} && cat > {} && chmod 755 {}",
            sh_quote(dir),
            sh_quote(&path),
            sh_quote(&path)
        );
        // The fleet sandbox itself, not a box inside it — no namespace to enter.
        own_sandbox(sandbox).write(&script, body.as_bytes(), Duration::from_secs(30))?;
    }
    // skein's box plugin, beside the launcher and under the same read-only `.skein`, so the plugin
    // every box loads is always this build's (box-plugin §2.1, SKEIN-1056).
    for (rel, body) in crate::runtime::PLUGIN_FILES {
        let path = format!("{}/{rel}", crate::runtime::plugin_dir());
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/boxes");
        let script = format!(
            "mkdir -p {} && cat > {} && chmod 755 {}",
            sh_quote(dir),
            sh_quote(&path),
            sh_quote(&path)
        );
        own_sandbox(sandbox).write(&script, body.as_bytes(), Duration::from_secs(30))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The revision has to answer "would restarting this box change what it can reach", and the
    /// cheap way to get that wrong is to hash the file. Most of `box-session.sh` is prose — it is
    /// the file where the reasoning lives — so a hash over its bytes would ask every box in the
    /// fleet to restart because somebody fixed a typo, and the ask would stop being read.
    #[test]
    fn a_comment_is_not_a_reason_to_restart_a_box() {
        let commented = BOX_SESSION_SH.replace(
            "set -uo pipefail",
            "# a sentence somebody added while explaining this\nset -uo pipefail",
        );
        assert_ne!(commented, BOX_SESSION_SH, "the edit did not apply");
        assert_eq!(
            revision_of(&commented),
            revision_of(BOX_SESSION_SH),
            "a comment moved the revision, so a docs edit asks for a fleet restart"
        );
    }

    /// The other direction, and the one that costs a cover rather than a restart: anything the
    /// launcher *runs* has to move it, including the conditions around a bind rather than only the
    /// binds. `/run` is covered for non-privileged boxes only, so a change to WHICH boxes are
    /// covered moves no `--tmpfs` line — which is why the revision is not derived from those lines.
    #[test]
    fn a_change_to_who_gets_a_cover_moves_the_revision() {
        let before = revision_of(BOX_SESSION_SH);
        let widened = BOX_SESSION_SH.replace(
            "if [ \"${SKEIN_BOX_PRIVILEGED-}\" != \"1\" ]; then",
            "if true; then",
        );
        assert_ne!(widened, BOX_SESSION_SH, "the edit did not apply");
        assert_ne!(
            revision_of(&widened),
            before,
            "the cover changed for a whole class of boxes and the revision did not notice"
        );
    }

    /// What goes into the sandbox carries the revision of what went into the sandbox.
    ///
    /// Driven through `install_launcher` against a fake `sbx` that keeps what it is fed, rather
    /// than against `stamped_launcher` directly — which is the version this test was first written
    /// as, and it passed with `install_launcher` sending the unstamped constant. A test of the
    /// stamping function says nothing about whether the installer uses it, and that wiring is the
    /// entire mechanism: an unstamped launcher in the sandbox reports nothing, and every box it
    /// starts reads as uncovered forever.
    ///
    /// The marker is left in the repo's own copy on purpose — a made-up revision there would be a
    /// claim — so what is checked is that the installed bytes never carry it.
    ///
    /// **Kept through the execution seam, not a fake `sbx` on `$PATH`.** The fake was the thing
    /// that caught the bytes; with no hop to intercept it would be bypassed and `install_launcher`
    /// would write into the real fleet root (SKEIN-592). The seam substitutes the argv while
    /// leaving the pipe, the timeout and the exit handling production's — which is what makes
    /// "what was sent" the same bytes a sandbox would have received.
    #[test]
    fn the_installed_launcher_knows_which_launcher_it_is() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // A fixture fleet root, and here it is not only about the guard: `install_launcher`
        // WRITES the launcher out to whatever root it resolves, and the seam is what keeps that
        // out of a real one. Left unset the root was `/boxes` — SKEIN-530's class, and the reason
        // `util::fleet_root` now refuses an unpinned test (SKEIN-690).
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        let kept = home.join("kept");
        std::fs::create_dir_all(&kept).unwrap();

        // Each write lands in the next numbered file, so "the FIRST thing installed" below is a
        // claim about ordering rather than about whichever write happened to be last.
        let into = kept.clone();
        let _stood_in = crate::place::seam::install(Box::new(move |_argv: &[String]| {
            let n = std::fs::read_dir(&into).map(|d| d.count()).unwrap_or(0);
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                format!("cat > {}/{n}", into.display()),
            ])
        }));

        install_launcher("skein-fleet").expect("install the launcher into the stood-in sandbox");

        let installed = std::fs::read_to_string(kept.join("0")).expect("the launcher was sent");
        assert!(
            installed.starts_with("#!/usr/bin/env bash"),
            "the first thing installed was not the launcher"
        );
        assert!(
            !installed.contains(LAUNCHER_REVISION_MARK),
            "the launcher went into the sandbox unstamped, so every box it starts reports nothing"
        );
        assert!(
            installed.contains(&format!("launcher_revision=\"{}\"", launcher_revision())),
            "the stamp did not land on the line the script reads"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// **skein's box plugin is installed where every box's argv looks for it**, byte for byte
    /// (SKEIN-1056).
    ///
    /// Driven through the same seam as the launcher test above, keeping each write's argv beside
    /// its bytes. What would make it fail: the plugin loop dropped from `install_launcher` (boxes
    /// would be started with a `--plugin-dir` naming an empty directory), or its path computed
    /// from anything but `runtime::plugin_dir`, which is the path the adapter strings resolve to.
    #[test]
    fn the_box_plugin_is_installed_where_the_agent_is_told_to_load_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        env.set("SKEIN_FLEET_ROOT", home.join("fleet"));
        let kept = home.join("kept");
        std::fs::create_dir_all(&kept).unwrap();
        let into = kept.clone();
        let _stood_in = crate::place::seam::install(Box::new(move |argv: &[String]| {
            let n = std::fs::read_dir(&into).map(|d| d.count()).unwrap_or(0) / 2;
            std::fs::write(into.join(format!("{n}.argv")), argv.join("\n")).unwrap();
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                format!("cat > {}/{n}.body", into.display()),
            ])
        }));

        install_launcher("skein-fleet").expect("install into the stood-in sandbox");

        let writes: Vec<(String, String)> = (0..)
            .map_while(|n| {
                Some((
                    std::fs::read_to_string(kept.join(format!("{n}.argv"))).ok()?,
                    std::fs::read_to_string(kept.join(format!("{n}.body"))).ok()?,
                ))
            })
            .collect();
        for (rel, body) in crate::runtime::PLUGIN_FILES {
            let path = format!("{}/{rel}", crate::runtime::plugin_dir());
            let landed = writes
                .iter()
                .find(|(argv, _)| argv.contains(&format!("cat > {}", sh_quote(&path))))
                .unwrap_or_else(|| panic!("nothing was installed at {path}"));
            assert_eq!(&landed.1, body, "{path} was installed with other bytes");
        }
    }

    /// The marker line and the report have to agree, because nothing later can catch it if they do
    /// not: a launcher that prints an empty revision is indistinguishable from one too old to print
    /// anything, and both are reported as an older cover forever.
    #[test]
    fn the_launcher_reports_the_stamp_it_was_given() {
        let installed = stamped_launcher();
        let stamped: Vec<&str> = installed
            .lines()
            .filter(|l| l.trim_start().starts_with("launcher_revision="))
            .collect();
        assert_eq!(stamped.len(), 1, "expected one stamp line, got {stamped:?}");
        assert!(
            installed.contains("printf \"SKEIN_LAUNCHER %s\\n\" \"$launcher_revision\""),
            "the launcher does not report the variable it was stamped with"
        );
    }

    #[test]
    fn a_launcher_too_old_to_answer_says_nothing_rather_than_something() {
        assert_eq!(launcher_from_launch("SKEIN_LAUNCHER abc123\n"), "abc123");
        assert_eq!(
            launcher_from_launch("welcome to your box\nSKEIN_LAUNCHER abc123\nSKEIN_ANCHOR 42\n"),
            "abc123",
            "the marker was not found past a chatty profile"
        );
        assert_eq!(
            launcher_from_launch("SKEIN_ANCHOR 42\n"),
            "",
            "a launcher that predates the report was read as having answered"
        );
        assert_eq!(
            launcher_from_launch(&format!("SKEIN_LAUNCHER {LAUNCHER_REVISION_MARK}\n")),
            "",
            "an unstamped launcher's placeholder was taken for a revision, so every box it starts \
             would compare equal to every other and none of them to the current cover"
        );
    }

    /// Three answers and only one of them is "current", because the two ways of not knowing are
    /// both ways of not knowing. A record from before this field and a record from the adoption
    /// path are equally silent about what covers the box, and reading silence as agreement is how
    /// the fleet ends up with boxes nobody knows are uncovered — the state SKEIN-88 was filed from.
    #[test]
    fn a_box_that_cannot_say_which_cover_it_has_is_not_taken_to_have_the_current_one() {
        let current = launcher_revision();
        // A name no registered repo claims, so `box_is_on_the_peer_network` answers the ship
        // default (`true`) and the peer half of the comparison is constant across these three.
        // What varies is the launcher revision, which is what this test is about; the peer switch
        // has its own walk-to-the-row in `board`.
        assert!(
            cover_is_current(
                "no-such-repo-box",
                &crate::place::PlaceRecord {
                    launcher: current.clone(),
                    ..Default::default()
                }
            ),
            "a box started by this launcher was asked to restart for the cover it already has"
        );
        assert!(
            !cover_is_current(
                "no-such-repo-box",
                &crate::place::PlaceRecord {
                    launcher: String::new(),
                    ..Default::default()
                }
            ),
            "a record that says nothing was read as saying the cover is current"
        );
        assert!(
            !cover_is_current(
                "no-such-repo-box",
                &crate::place::PlaceRecord {
                    launcher: "0000000000000000".into(),
                    ..Default::default()
                }
            ),
            "a box born under a different launcher was called current"
        );
        // And the empty case is not empty-equals-empty: a build whose own revision was somehow
        // blank must not make every silent record agree with it.
        assert!(
            !current.is_empty(),
            "this build has no launcher revision to compare against"
        );
    }

    /// The host's reading of it, and the direction that must never be guessed.
    #[test]
    fn a_box_that_did_not_say_what_bounds_it_is_not_taken_to_be_bounded() {
        assert!(is_capped("capped max=1G,high=800M"));
        for silent in [
            "",
            "uncapped no-limit-computed",
            "uncapped no-cgroup-delegation",
        ] {
            assert!(
                !is_capped(silent),
                "{silent:?} was read as a box with a ceiling"
            );
        }
        assert_eq!(
            limits_from_launch("SKEIN_ANCHOR 42\nSKEIN_LIMITS capped max=1G\n"),
            "capped max=1G"
        );
        assert_eq!(
            limits_from_launch("SKEIN_ANCHOR 42\n"),
            "",
            "a launcher too old to report was read as having reported something"
        );
    }

    /// **Three answers, and the third one is why this returns an `Option`** (SKEIN-572).
    ///
    /// A launcher that did not print `SKEIN_PEERS` is not a box off the peer network — it is a box
    /// whose birth nothing recorded. Collapsing that to `false` puts it on the uncovered side of a
    /// switch nobody flipped; collapsing it to `true` claims a network it may not be on.
    /// `cover_is_current` spends the third answer by leaving such a box alone.
    ///
    /// **What would make this fail**: giving `peers_from_launch` a `bool` return with
    /// `unwrap_or(false)` or `unwrap_or(true)` — the last assertion names either.
    #[test]
    fn a_launcher_that_did_not_say_which_side_of_the_peer_switch_a_box_was_born_on_says_nothing() {
        assert_eq!(
            peers_from_launch("SKEIN_ANCHOR 42\nSKEIN_PEERS 1\n"),
            Some(true)
        );
        assert_eq!(
            peers_from_launch("SKEIN_ANCHOR 42\nSKEIN_PEERS 0\n"),
            Some(false)
        );
        // The last one wins, the way the ceiling's does: a launcher that reported twice is
        // reporting a change, and the earlier line is the state it changed from.
        assert_eq!(
            peers_from_launch("SKEIN_PEERS 1\nSKEIN_PEERS 0\n"),
            Some(false)
        );
        assert_eq!(
            peers_from_launch("SKEIN_ANCHOR 42\n"),
            None,
            "a launcher too old to report the peer switch was read as taking a side on it"
        );
    }
}
