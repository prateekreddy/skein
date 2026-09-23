//! The fleet kit: the scripts embedded in this binary so they can be installed into a sandbox
//! that has never seen this repo.

use super::*;

/// The launcher, embedded so it can be installed into a sandbox that has never seen this repo.
/// The fleet sandbox hosts boxes from *many* repos, so it cannot be served out of any one repo's
/// store — and shipping it through a store would put runtime tooling in shared data besides.
pub(super) const BOX_SESSION_SH: &str = include_str!("../box-session.sh");
pub(super) const GIT_CREDENTIAL_SH: &str = include_str!("../git-credential-skein.sh");
/// The fleet sandbox's own sbx kit — one startup command, and the only thing in that sandbox that
/// survives its own restart.
///
/// **Not `kit::KIT_SPEC_YAML`, and it must not be.** That one is a box's: it links the shared store
/// into a `--clone` and checks out the box's branch, in a sandbox made for one repo. The fleet
/// sandbox is none of those things — no clone, no branch, no repo.
///
/// **Here rather than in `kit`, and the module gate is what said so.** `fleet` already depends on
/// `kit`, so a `kit` that reached back for [`fleet_root`] would close a cycle between them. The
/// dependency runs one way and this is the side that owns the fleet.
const FLEET_KIT_SPEC_YAML: &str = include_str!("../fleet-kit-spec.yaml");

/// The marker the spec carries in place of the fleet root, which is not a compile-time fact:
/// `$SKEIN_FLEET_ROOT` overrides `/boxes`, and the test suite depends on that seam.
const FLEET_KIT_ROOT_MARKER: &str = "@SKEIN_FLEET_ROOT@";

/// The fleet kit spec, with the fleet root filled in.
///
/// `pub` because a test compares these bytes to the copy `bootstrap.sh` writes. Two writers is a
/// necessity ([`ensure_fleet_kit`]) and drift between them is how one quietly starts installing a
/// kit that parses and does nothing.
pub fn fleet_kit_spec() -> String {
    FLEET_KIT_SPEC_YAML.replace(FLEET_KIT_ROOT_MARKER, &fleet_root())
}

/// Where the fleet kit lives, for the callers that only need to name it.
///
/// Separate from [`ensure_fleet_kit`] because [`create_argv`] must stay a pure function of the
/// config: it is called in tests, in [`create_line`] to render a command for a person to *read*,
/// and twice more to actually build a fleet. A path-builder that wrote to disk as a side effect of
/// being displayed would be a surprise in the one place whose whole job is showing somebody what
/// will run.
///
/// **On the volume, because the path has to be right on the HOST.** `sbx create --kit <path>` runs
/// wherever `sbx` is, which is never inside the fleet; `skein_home` resolves to the volume's own
/// absolute path in both deployments (`config::volume_marker`), so this one string is valid to the
/// host that runs the create and to the in-fleet skein that composes it.
///
/// Beside the box kit and never inside it: `sbx` reads a kit as a whole directory, so a second
/// `spec.yaml` in `~/.skein/kit` would not be a second kit — it would be the first one overwritten.
pub fn fleet_kit_dir() -> std::path::PathBuf {
    skein_home().join("fleet-kit")
}

/// Write the fleet kit, and answer with the path `--kit` is given.
pub fn ensure_fleet_kit() -> Result<std::path::PathBuf, String> {
    let kit = fleet_kit_dir();
    std::fs::create_dir_all(&kit).map_err(|e| format!("mkdir {}: {e}", kit.display()))?;
    let spec = kit.join("spec.yaml");
    std::fs::write(&spec, fleet_kit_spec())
        .map_err(|e| format!("write {}: {e}", spec.display()))?;
    Ok(kit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// The create attaches the fleet kit, and attaches it where sbx will read it.
    ///
    /// Two halves, and the second is the one this file has already got wrong once. sbx's usage is
    /// `sbx create [flags] AGENT PATH...`, so a flag placed after `shell` is not a flag — it is an
    /// argument to the shell, and the sandbox is created without it while the command still
    /// succeeds. `the_create_publishes_the_cockpits_port_and_the_readme_agrees` exists because `-p`
    /// landed there; a kit landing there would be a fleet that silently never serves after a
    /// restart, which is precisely the fault it is meant to cure.
    #[test]
    fn the_create_attaches_the_fleet_kit_before_the_agent() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let argv = create_argv("skein-fleet", &["/h/.skein".to_string()]);
        // Read while `SKEIN_HOME` still points where the argv was built. Both sides of this
        // comparison resolve the volume at the moment they are called, so clearing it first would
        // compare two different fleets and fail on a correct argv.
        let expected = fleet_kit_dir().to_string_lossy().into_owned();
        std::env::remove_var("SKEIN_HOME");

        let at = argv.iter().position(|a| a == "--kit").unwrap_or_else(|| {
            panic!(
                "the create attaches no kit, so nothing runs at sandbox start and every restart \
                 leaves the fleet installed and not serving: {argv:?}"
            )
        });
        assert_eq!(
            argv.get(at + 1).map(String::as_str),
            Some(expected.as_str()),
            "the --kit path is not the directory `ensure_fleet_kit` writes: {argv:?}"
        );
        let agent = argv
            .iter()
            .position(|a| a == "shell")
            .expect("the create names no agent");
        assert!(
            at < agent,
            "--kit comes after `shell`, where sbx reads it as an argument to the shell rather than \
             as a flag — the sandbox is created with no kit and the command still succeeds: {argv:?}"
        );
    }

    /// **The kit is what runs the cure, and it is written twice — so the two copies must agree.**
    ///
    /// `start-door.sh` puts the door back; nothing ran it. sbx's `commands.startup` runs at every
    /// sandbox start and is the only hook this sandbox has, pid 1 being `tini` with no systemd, no
    /// cron and no `systemctl`. So the fleet gets a kit of its own.
    ///
    /// It has two writers by necessity. `fleet::ensure_fleet_kit` writes it on every server start,
    /// which is no use on a FIRST install — nothing has ever run against that volume, and the next
    /// line a person types is the `sbx run -d` that would attach the kit — so `bootstrap.sh` writes
    /// it too. Two writers of one file is exactly the shape that rots: the one nobody looks at
    /// starts installing a kit that parses and does nothing, and the symptom is a restart that
    /// silently does not serve, which is indistinguishable from the bug this fixes.
    ///
    /// So they are compared, byte for byte, against the real `bootstrap.sh` — the same method
    /// `the_host_and_the_launcher_agree_on_what_a_login_is` uses, and for the same reason: a copy of
    /// the expected text in this test would be a third writer.
    #[test]
    fn the_two_writers_of_the_fleet_kit_agree() {
        let _g = crate::testutil::env_lock();
        // The fleet root the shipped installer writes, which is the default and not this process's:
        // `bootstrap.sh`'s heredoc carries `/boxes` literally (it is `<<'KITEOF'`, so nothing in it
        // expands), and `fleet_kit_spec` substitutes `fleet_root()` for the same marker. A fixture
        // root would make the two sides differ for a reason that is not drift, which is the only
        // thing this compares. So it is SET to the default rather than left unset — `util::fleet_root`
        // refuses an unpinned test now (SKEIN-690), and the value, not the fleet, is the subject:
        // nothing here opens a path.
        std::env::set_var("SKEIN_FLEET_ROOT", "/boxes");
        let from_skein = fleet_kit_spec();

        let bootstrap = include_str!("../../bootstrap.sh");
        let from_bootstrap = bootstrap
            .split_once("cat > \"$fleet_kit/spec.yaml.new\" <<'KITEOF'\n")
            .expect("bootstrap.sh no longer writes a fleet kit at all, so a first install has none")
            .1
            .split_once("\nKITEOF")
            .expect("the KITEOF heredoc is unterminated")
            .0;

        assert_eq!(
            from_bootstrap.trim_end(),
            from_skein.trim_end(),
            "the installer and skein write different fleet kits, so one of them is installing a \
             startup hook that is not the one under test"
        );
        // Non-vacuity: an empty match on either side would satisfy the equality above.
        assert!(
            from_skein.contains("commands:") && from_skein.contains("start-door.sh"),
            "the fleet kit names no startup command, so a restart runs nothing: {from_skein}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **The kit's startup command actually opens the door, and is silent when there is nothing to
    /// open.**
    ///
    /// Asserted by RUNNING it rather than by reading it. The whole class of bug this closes is a
    /// command line that looks right and does not work, so a test that only inspected the YAML
    /// would be the same mistake one layer up.
    ///
    /// Two states, and the second is the one that would break a fresh install: between `sbx create`
    /// and `bootstrap.sh` there is no `.skein` at all, and a startup command that failed there would
    /// make a brand-new sandbox look broken at the one moment nobody can tell a missing feature from
    /// a missing install.
    // Deliberately NOT gated to Linux. Everything here is POSIX — `fs::write`, a mode bit, and a
    // `bash -c` with a `[ -x ]` test — so the assertion holds wherever skein is developed, and the
    // startup command sbx will run is checked on the machine of whoever is changing it.
    #[test]
    fn the_kits_startup_opens_the_door_and_says_nothing_when_there_is_no_install() {
        let _g = crate::testutil::env_lock();
        let root = crate::testutil::tempdir();
        let root = root.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_FLEET_ROOT", root);
        let spec = fleet_kit_spec();
        std::env::remove_var("SKEIN_FLEET_ROOT");

        // The command sbx would run, taken out of the spec rather than retyped.
        let line = spec
            .lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix("- '")
                    .and_then(|r| r.strip_suffix('\''))
            })
            .expect("the fleet kit carries no `bash -c` line for sbx to run");
        assert!(
            line.contains(&root.display().to_string()),
            "the kit's command does not name this fleet's root, so it would run somebody else's \
             door: {line}"
        );

        let run = || {
            std::process::Command::new("bash")
                .arg("-c")
                .arg(line)
                .output()
                .expect("bash ran the kit's startup command")
        };

        // 1. No install yet. It must succeed and do nothing — see the doc above.
        let out = run();
        assert!(
            out.status.success(),
            "the kit's startup failed on a sandbox that has not been bootstrapped yet, so a fresh \
             create looks broken: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // 2. An install. A stand-in for start-door.sh, because what is under test is whether the
        //    kit REACHES it — start-door.sh's own behaviour is
        //    `the_door_is_a_file_the_install_runs_rather_than_a_passage_of_the_install`'s job.
        let door = root.join(".skein/start-door.sh");
        std::fs::create_dir_all(door.parent().unwrap()).unwrap();
        std::fs::write(
            &door,
            format!(
                "#!/usr/bin/env bash\necho opened > {}\n",
                root.join("ran").display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&door, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = run();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("ran"))
                .unwrap_or_default()
                .trim(),
            "opened",
            "the kit's startup did not run the door script, so nothing puts the cockpit back after \
             a restart"
        );
    }

    /// `sudo` in a box explains itself instead of failing incomprehensibly — and only in a box.
    ///
    /// Two halves, and shipping either alone is worse than shipping neither:
    ///
    /// 1. **Written but never bound** is this repo's most-repeated bug — a thing installed on disk
    ///    that nothing ever reaches. The shim would sit in `$root/bin` while every agent kept
    ///    reading "owned by uid 65534, should be 0" and kept trying to chown it.
    /// 2. **Bound too widely** would be far worse than the problem it fixes. The sandbox's own
    ///    `sudo` is what `ensure_substrate` installs tmux and jq with — the very substrate this
    ///    message tells people to ask for. Shadowing it fleet-wide would stop new sandboxes being
    ///    provisioned at all, and the shim's own advice would become impossible to follow.
    #[test]
    fn sudo_in_a_box_says_why_rather_than_failing_in_hex() {
        let launcher = BOX_SESSION_SH;
        // Anchored on the heredoc that carries the shim's body rather than on the redirect that
        // writes it: the body is now preceded by a generated preamble (the box name and the
        // launcher's path, which cannot be known until a box starts), so the redirect is no longer
        // the line the body follows.
        let shim = launcher
            .lines()
            .skip_while(|l| !l.contains("cat <<'SHIM'"))
            .take_while(|l| *l != "SHIM")
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            shim.contains("does not work inside a box"),
            "the shim no longer says what happened"
        );
        assert!(
            shim.contains("--user") && shim.contains("substrate"),
            "a refusal with no way forward is the error message it replaced: {shim}"
        );
        assert!(
            launcher.contains(r#"binds+=(--ro-bind "$root/bin/sudo" "$sudo_real")"#),
            "the shim is written but never bound, so nothing in a box would ever run it"
        );
        // Inside the namespace only. `binds` is applied by the box's bwrap and by nothing else, so
        // being in that array is exactly the scope this needs — and `--dev-bind / /` above it means
        // a bind added anywhere outside it would reach the whole sandbox.
        assert!(
            !launcher.contains("chmod 755 /usr/bin/sudo")
                && !launcher.contains("rm -f /usr/bin/sudo"),
            "the sandbox's real sudo must be left alone — ensure_substrate provisions with it"
        );
        // The launcher is bash (it uses arrays); a shim that only parses under bash would still be
        // run by /bin/sh as `sudo`, so it is checked with the shell that will actually execute it.
        let checked = std::process::Command::new("sh")
            .arg("-n")
            .arg("/dev/stdin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                let body = shim
                    .split_once('\n')
                    .map(|x| x.1)
                    .unwrap_or_default()
                    .to_string();
                c.stdin.take().unwrap().write_all(body.as_bytes())?;
                c.wait()
            });
        assert!(
            checked.map(|s| s.success()).unwrap_or(false),
            "the shim is not a valid POSIX shell script, so `sudo` would fail on a syntax error \
             instead of explaining anything"
        );
    }

    /// The shim is skipped rather than allowed to fail, because failing here stops the box starting.
    ///
    /// bwrap cannot mount a file onto a symlink, and on Debian `sudo` is one
    /// (`/usr/bin/sudo` → `/etc/alternatives/sudo` → `/usr/bin/sudo.ws`). Binding the name instead
    /// of the resolved binary makes bwrap try to *create* the destination, which fails on a
    /// `/usr/bin` no unprivileged user can write — and takes the whole box down with it:
    ///
    /// ```text
    /// bwrap: Can't create file at /usr/bin/sudo: No such file or directory
    /// ```
    ///
    /// That is the trade this shim must never make. A worse error message is a nuisance; a box that
    /// will not start is an outage. So every uncertain step skips, and this proves it skips on the
    /// shape that actually broke it.
    /// Linux only: it runs a block lifted out of `box-session.sh`, which uses `readlink -f` —
    /// GNU-only, and correct, because that script only ever runs inside the sandbox. On BSD the
    /// block fails and produces no binds at all, so the assertion fires on an empty string and
    /// says nothing about the shim.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_sudo_it_cannot_shim_is_left_alone_rather_than_breaking_the_box() {
        // Ended at the block's last statement rather than at the first bare `fi`: the shim's own
        // body contains one now (it asks the launcher to file a request before explaining itself),
        // and stopping there cut the extraction off inside the heredoc — which failed as a syntax
        // error that looked like the launcher was broken when only this extraction was.
        let block = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("sudo_real=$(command -v sudo"))
            .take_while(|l| !l.contains(r#"binds+=(--ro-bind "$root/bin/sudo""#))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n  binds+=(--ro-bind \"$root/bin/sudo\" \"$sudo_real\")\nfi";
        let dir = tempdir();
        let root = dir.as_ref() as &std::path::Path;
        let bin = root.join("fakebin");
        std::fs::create_dir_all(&bin).unwrap();

        // A PATH with no system `sudo` on it at all, so "there is no sudo here" is a state this can
        // actually reach. Inheriting the real PATH makes every case find /usr/bin/sudo — including
        // the ones meant to find nothing, which is how a test like this passes while proving
        // nothing. The block still needs the handful of tools it runs, so they are linked in.
        let tools = root.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        for tool in ["readlink", "mkdir", "chmod", "cat"] {
            let from = ["/usr/bin", "/bin"]
                .iter()
                .map(|d| std::path::Path::new(d).join(tool))
                .find(|p| p.exists())
                .unwrap_or_else(|| panic!("{tool} is needed to run the launcher's sudo block"));
            std::os::unix::fs::symlink(from, tools.join(tool)).unwrap();
        }

        // `binds` printed at the end is the whole assertion: it is what the box's bwrap is handed,
        // so an entry here is a mount attempted and an empty array is the shim declining.
        let run = |sudo_is: &dyn Fn(&std::path::Path)| -> String {
            let _ = std::fs::remove_file(bin.join("sudo"));
            sudo_is(&bin);
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    // `box` because the shim now bakes the box's name into itself, and under
                    // `set -u` an unset one would fail the block for a reason that has nothing to
                    // do with what this test is about.
                    "set -uo pipefail\nbinds=()\nroot={root}\nbox=testbox\nexport PATH={bin}:{tools}\n\
                     {block}\nprintf '%s\\n' \"${{binds[@]:-}}\"\n",
                    root = root.display(),
                    bin = bin.display(),
                    tools = tools.display(),
                ))
                .output()
                .expect("bash to run the launcher's sudo block");
            assert!(
                out.status.success(),
                "the block itself failed, which would abort the box start: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        // The shape that broke it: a name that resolves to nothing. `command -v` still finds it —
        // which is why the original check was not enough.
        let dangling = run(&|bin| {
            std::os::unix::fs::symlink("/nonexistent/sudo.ws", bin.join("sudo")).unwrap();
        });
        assert!(
            !dangling.contains("--ro-bind"),
            "a dangling sudo was bound anyway; bwrap would refuse and the box would not start:\n{dangling}"
        );

        // No sudo at all: nothing to shim, and nothing to say about it.
        let absent = run(&|_| {});
        assert!(
            !absent.contains("--ro-bind"),
            "bound a sudo that is not there:\n{absent}"
        );

        // And the case it is actually for — bound, and bound at the RESOLVED binary rather than at
        // the symlink, which is the fix itself.
        let real = bin.join("sudo.ws");
        std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
        let present = run(&|bin| {
            std::os::unix::fs::symlink(bin.join("sudo.ws"), bin.join("sudo")).unwrap();
        });
        assert!(
            present.contains("--ro-bind")
                && present.contains(&crate::util::resolved(&real.to_string_lossy())),
            "the shim did not reach a sudo that is genuinely there:\n{present}"
        );
        assert!(
            std::fs::read_to_string(root.join("bin/sudo"))
                .unwrap_or_default()
                .contains("does not work inside a box"),
            "the shim was bound but its body was never written"
        );
    }
}
