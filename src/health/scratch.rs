//! Whether somebody else's directory has taken the temp path the model CLI derives for itself
//! in the fleet's shared `/tmp`.

use super::*;

/// What the shared `/tmp` is asked, where the fleet's `claude` actually runs.
///
/// Derives the path rather than assuming it: `${TMPDIR:-/tmp}/claude-$(id -u)` is the rule Claude
/// Code applies, and both halves are answered by the machine being asked — a fleet's uid is not the
/// host's, and `TMPDIR` is set on macOS and unset in the sandbox. `stat -c` is GNU and `stat -f` is
/// BSD, so both are tried and whichever exists answers.
///
/// Prints one line: `clear <path>` when nothing is there, or `<owner-uid> <our-uid> <path>` when
/// something is. Never deletes, never creates — see [`scratch_verdict`] for why that is a rule and
/// not an omission.
const SCRATCH_PROBE: &str = "d=\"${TMPDIR:-/tmp}/claude-$(id -u)\"\n\
     if [ ! -e \"$d\" ]; then printf 'clear %s\\n' \"$d\"; exit 0; fi\n\
     owner=\"$(stat -c %u \"$d\" 2>/dev/null || stat -f %u \"$d\" 2>/dev/null)\"\n\
     printf '%s %s %s\\n' \"${owner:-unreadable}\" \"$(id -u)\" \"$d\"\n";

/// Has somebody else's directory taken the temp path the model CLI derives for itself?
///
/// **Why this is skein's business at all.** Claude Code puts its temp directory at
/// `${os.tmpdir()}/claude-<uid>` and refuses to start when that path exists and belongs to another
/// uid — a deliberate guard against a directory somebody planted. In a fleet that path is the
/// sandbox's SHARED `/tmp`, and on the owner's fleet something running as root got there first.
/// Every model call skein makes, every box session and the login terminal now carry
/// `CLAUDE_CODE_TMPDIR` past it ([`crate::fleet::MODEL_SCRATCH`]), so this is no longer how skein
/// fails — but the directory outlives every call, and a `claude` anybody starts by hand still meets
/// it. The CLI's own message is a good one; the trouble was that it landed on whoever happened to be
/// typing rather than in the one place that reports the fleet's health.
///
/// **A `doctor` line and not part of [`health_report`]**, for the same reason `skein doctor`'s model
/// line is not: this spawns a process in the sandbox, and the health endpoint is polled every
/// fifteen seconds by every open board.
pub fn model_scratch_health() -> HealthCheck {
    // The question is about the /tmp the fleet's `claude` runs in, and **this process is standing
    // in it** — one arm now (SKEIN-576). The other asked the sandbox through `sbx exec`, a hop that
    // no longer exists in either direction; `crate::fleet::model_call_in_box` is the only crossing
    // a model call still makes, and a box has a /tmp of its own that this check is not about.
    let (reported, whose) = (run_here(SCRATCH_PROBE), "this fleet's shared /tmp");
    scratch_verdict(reported, whose)
}

/// The probe, run on this machine.
fn run_here(script: &str) -> Result<String, String> {
    let out = std::process::Command::new("bash")
        .arg("-lc")
        .arg(script)
        .output()
        .map_err(|e| format!("bash could not be run here ({e})"))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// The verdict over [`SCRATCH_PROBE`]'s answer — split from the call so every arm is testable on a
/// machine with no fleet, which is every machine this suite runs on.
///
/// **The fix is words, and it is marked destructive so nothing drives it.** Deleting a directory in
/// a shared `/tmp` that skein does not own is precisely the attack the CLI's guard exists to stop,
/// and skein would be doing it with more privilege than whoever planted it. So the recipe names the
/// path and the uid and stops there: a person decides, on a machine where they can see what it is.
pub fn scratch_verdict(reported: Result<String, String>, whose: &str) -> HealthCheck {
    let line = match reported {
        Ok(out) => out.lines().last().unwrap_or_default().trim().to_string(),
        Err(why) => {
            return HealthCheck::unknown(format!(
                "{whose} could not be asked whether anything has taken the model's temp directory \
                 ({why})"
            ))
        }
    };
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["clear", path] => HealthCheck::satisfied(format!(
            "nothing has taken {path} in {whose}, and skein's own calls carry their own scratch \
             directory either way"
        )),
        [owner, mine, path] if owner == mine => HealthCheck::satisfied(format!(
            "{path} in {whose} is this fleet's own (uid {mine})"
        )),
        [owner, mine, path] => HealthCheck::unsatisfied(
            format!(
                "{path} in {whose} belongs to uid {owner}, and the fleet runs as uid {mine} — a \
                 `claude` that derives its own temp directory refuses to start there, whatever the \
                 login says"
            ),
            format!(
                "skein's model calls, every box session and the login terminal carry \
                 CLAUDE_CODE_TMPDIR past it, so this reaches only a `claude` somebody starts by \
                 hand. Clearing it takes uid {owner} — `rm -rf {path}` on that machine, by \
                 somebody who can see what is in it. skein will not: deleting a directory in a \
                 shared /tmp it does not own is the thing the CLI's guard exists to stop"
            ),
        )
        .destroys(),
        _ => HealthCheck::unknown(format!(
            "{whose} answered something this cannot read ({line:?}), so whether anything has taken \
             the model's temp directory is unknown"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe finds the path the CLI would derive, on the machine being asked — not one skein
    /// worked out for it.
    ///
    /// `${TMPDIR:-/tmp}/claude-$(id -u)` is the rule, and both halves belong to the other machine:
    /// a fleet's uid is not the host's, and `TMPDIR` is set on macOS and unset in a sandbox. So the
    /// probe is run here against a `TMPDIR` this test controls, and asked what it found.
    #[cfg(unix)]
    #[test]
    fn the_scratch_probe_reads_the_directory_the_runtime_would_derive() {
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let ask = |tmp: &std::path::Path| {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(SCRATCH_PROBE)
                .env("TMPDIR", tmp)
                .output()
                .expect("bash");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(dir).unwrap());
        let derived = dir.join(format!("claude-{uid}"));

        // Nothing there: `clear`, and the path it looked at, so a reader can check the derivation
        // rather than take it on trust.
        let empty = ask(dir);
        assert_eq!(
            empty,
            format!("clear {}", derived.display()),
            "the probe looked somewhere other than the path the runtime derives"
        );
        assert!(
            !derived.exists(),
            "the probe CREATED the directory it was asked about, so it answers about itself"
        );
        assert_eq!(
            scratch_verdict(Ok(empty), "the fleet").level,
            Level::Satisfied
        );

        // And ours: reported as ours, with both uids, so the verdict never has to assume which one
        // it is looking at.
        std::fs::create_dir_all(&derived).unwrap();
        let mine = ask(dir);
        assert_eq!(
            mine,
            format!("{uid} {uid} {}", derived.display()),
            "the probe could not say who owns a directory that is there"
        );
        assert_eq!(
            scratch_verdict(Ok(mine), "the fleet").level,
            Level::Satisfied,
            "the fleet's own scratch directory was reported as somebody else's"
        );
    }

    /// A poisoned directory is NAMED, with a way out that is words — never a delete skein runs.
    ///
    /// The owner met this in the middle of a login that had otherwise worked: OAuth completed, and
    /// the CLI then refused because `/tmp/claude-1000` in the fleet's shared /tmp belonged to root.
    /// The CLI's message is a good one; the trouble was that it landed on whoever happened to be
    /// typing. The uid arm cannot be built without root — a test cannot plant a directory it does
    /// not own — so it is driven on the probe's own answer, which the test above pins to the real
    /// thing.
    #[test]
    fn a_poisoned_shared_tmp_is_named_and_its_removal_is_left_to_a_person() {
        let poisoned = scratch_verdict(
            Ok("0 1000 /tmp/claude-1000\n".into()),
            "the fleet sandbox's shared /tmp",
        );
        assert_eq!(
            poisoned.level,
            Level::Unsatisfied,
            "a directory the runtime refuses to start beside is reported as fine: {}",
            poisoned.detail
        );
        for said in ["/tmp/claude-1000", "uid 0", "1000"] {
            assert!(
                poisoned.detail.contains(said),
                "the fault does not name {said}, so nobody can act on it: {}",
                poisoned.detail
            );
        }
        assert!(
            poisoned.fix.contains("/tmp/claude-1000") && poisoned.fix.contains("uid 0"),
            "the way out names neither the path nor the uid that can clear it: {}",
            poisoned.fix
        );
        assert!(
            poisoned.destructive,
            "a recipe that deletes a directory in a shared /tmp is drivable — §2.4 says printed, \
             never run, and this is the exact shape of the guard the runtime applies"
        );

        // Asked and not answered is the third state, not a fault: a sandbox that will not answer
        // is not evidence that anything is wrong in it.
        let silent = scratch_verdict(Err("sbx did not answer".into()), "the fleet");
        assert_eq!(silent.level, Level::Unknown);
        assert!(
            silent.fix.is_empty(),
            "an unknown offers a fix: {}",
            silent.fix
        );
        let garbled = scratch_verdict(Ok("what\n".into()), "the fleet");
        assert_eq!(
            garbled.level,
            Level::Unknown,
            "an answer this cannot read was turned into a claim about the fleet"
        );
    }
}
