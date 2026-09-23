//! Every `sudo` the launcher runs is non-interactive and never waits for a password, and a
//! child that was killed is reported as killed.

use super::*;

// -------------------------------------------------------------------------------------------------
// The launcher's `sudo` calls refuse rather than wait (SKEIN-555)
// -------------------------------------------------------------------------------------------------
//
// The same defect these tests now cover on the production side was fixed on the test side first,
// and the note at the top of `a_box_lives_and_dies_inside_the_fleet_sandbox` describes what it
// looked like: a probe spelled `sudo mkdir -p /sys/fs/cgroup/skein`, with no `-n`, blocked on a
// password prompt with `cargo test`'s output captured and nothing on screen to answer. The launcher
// is worse off than the test was, because nobody is at a terminal at all when it runs.

/// The bytes `fleet::install_launcher` puts into a sandbox, read the way `src/fleet.rs` reads them.
pub(super) const LAUNCHER: &str = include_str!("../../src/box-session.sh");

/// Every genuine `sudo` invocation in the launcher, as `(line number, the command from `sudo` on)`.
///
/// Derived, and it has to be more than a grep. `box-session.sh` is the file where the reasoning
/// lives, so most of its `sudo`s are prose: `grep -cE 'sudo ' src/box-session.sh` answers 27, of
/// which 23 are comments about the shim and about the sandbox's own sudo, and 3 are inside heredocs
/// the shim prints to a box's owner — one of them the line `sudo apt-get install <package>` in the
/// help text, which is advice to a human and not something this script runs. Four more occurrences
/// are `sudo` as a *word* rather than as the command: `command -v sudo`, and the shim's own path
/// `"$root/bin/sudo"`. A check that could not tell those apart would be the fragile grep
/// `CONTRIBUTING.md` calls worse than no check.
///
/// So: heredoc bodies are skipped whole, whole-line and trailing comments are cut, and an
/// occurrence counts only where `sudo` stands in command position — at the start of a line, or
/// after one of the operators or keywords that begins a new command.
fn launcher_sudo_calls(script: &str) -> Vec<(usize, String)> {
    let mut calls = Vec::new();
    let mut here: Option<String> = None;
    for (i, raw) in script.lines().enumerate() {
        if let Some(term) = &here {
            if raw.trim() == term.as_str() {
                here = None;
            }
            continue;
        }
        if raw.trim_start().starts_with('#') {
            continue;
        }
        let code = code_before_comment(raw);
        for at in sudo_command_positions(&code) {
            calls.push((i + 1, code[at..].trim_end().to_string()));
        }
        here = heredoc_terminator(&code);
    }
    calls
}

/// The line with any trailing `#` comment cut off, quoting respected.
///
/// Respected because the launcher writes shell inside single quotes all day —
/// `sudo -n sh -c 'echo "+memory +pids +cpu" > "$1/cgroup.subtree_control"'` — and a `#` inside one
/// of those is text.
fn code_before_comment(line: &str) -> String {
    let (mut in_single, mut in_double) = (false, false);
    for (j, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => {
                let starts_a_word = j == 0 || matches!(line.as_bytes()[j - 1], b' ' | b'\t');
                if starts_a_word {
                    return line[..j].to_string();
                }
            }
            _ => {}
        }
    }
    line.to_string()
}

/// The terminator a line opens a heredoc with, if it opens one: `<<'SHIM'`, `<<SKEIN_MOUNTS`.
fn heredoc_terminator(code: &str) -> Option<String> {
    let at = code.find("<<")?;
    let rest = code[at + 2..].trim_start_matches('-').trim_start();
    let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let first_is_a_name_start = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    first_is_a_name_start.then_some(name)
}

/// Where in `code` the word `sudo` is the command being run, rather than a word inside one.
fn sudo_command_positions(code: &str) -> Vec<usize> {
    const OPENS_A_COMMAND: [char; 5] = [';', '&', '|', '(', ')'];
    const KEYWORDS: [&str; 8] = ["if", "then", "else", "elif", "do", "while", "until", "!"];
    let mut found = Vec::new();
    for (at, _) in code.match_indices("sudo") {
        // A command needs an argument after it, and the word has to end here: `sudo_real=` and
        // `"$root/bin/sudo"` are both rejected by this line alone.
        if !code[at + 4..].starts_with([' ', '\t']) {
            continue;
        }
        let before = code[..at].trim_end();
        let is_command = match before.chars().last() {
            None => true,
            Some(c) if OPENS_A_COMMAND.contains(&c) => true,
            // `command -v sudo` is rejected here: the word in front is `-v`, not a keyword.
            Some(_) => KEYWORDS.contains(&before.rsplit([' ', '\t']).next().unwrap_or("")),
        };
        if is_command {
            found.push(at);
        }
    }
    found
}

/// Sudo's own options, which end at the first word that is not one — everything after that belongs
/// to the command sudo is being asked to run, and `-n` there would be an argument to `mkdir`.
fn sudo_own_options(call: &str) -> Vec<&str> {
    call.split_whitespace()
        .skip(1)
        .take_while(|w| w.starts_with('-'))
        .collect()
}

/// Every `sudo` the launcher runs passes `-n`, so a sudo that wants a password refuses instead of
/// asking one nobody can answer.
///
/// What makes this fail: take the `-n` off any one of them. It is not a grep for the string — the
/// derivation above tells a call from the 26 mentions of `sudo` in this file's prose — and it
/// refuses to be green on an empty derivation, which is the failure mode of the leaked-process
/// check `CLAUDE.md` describes: a pattern that has never matched anything looks exactly like a
/// pattern that matches nothing.
#[test]
fn every_sudo_the_launcher_runs_is_non_interactive() {
    let calls = launcher_sudo_calls(LAUNCHER);
    assert!(
        !calls.is_empty(),
        "no `sudo` call was derived from box-session.sh at all — the launcher cannot have stopped \
         using sudo, so this is the derivation broken, and a check that derives nothing would \
         otherwise pass for ever"
    );
    for (line, call) in &calls {
        let opts = sudo_own_options(call);
        assert!(
            opts.contains(&"-n") || opts.contains(&"--non-interactive"),
            "box-session.sh:{line} runs sudo without -n, so on a machine whose sudo wants a \
             password this blocks on a prompt nobody is watching and the box never starts: {call}"
        );
    }

    // And the file's own count of them, in the note above `export PATH=`, is checked against the
    // derivation rather than trusted: that sentence is the reason the fixed PATH covers what it
    // covers, and it is the kind of prose that goes stale silently.
    let spelled = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
    ];
    let n = calls.len();
    let word = spelled.get(n).copied().unwrap_or("");
    assert!(
        !word.is_empty() && LAUNCHER.contains(&format!("the {word} `sudo` calls")),
        "the launcher has {n} sudo calls and the note above its `export PATH=` does not say so; \
         both have to move together, because that note is the argument for the fixed PATH"
    );
}

/// Drive the real function text with a `sudo` that wants a password, and watch it not wait.
///
/// The string check above cannot see the difference between `sudo -n mkdir` and a `-n` that landed
/// somewhere useless, so this runs `ensure_container_cgroup` — lifted verbatim out of the launcher,
/// not retyped — against a stub `sudo` that blocks on a read exactly where the real one blocks on a
/// password.
///
/// **The named change that makes it fail is `sudo -n` → `sudo` in that function, and the test makes
/// that change itself.** The second half runs the same body with the `-n`s stripped and asserts it
/// does NOT finish. So a run where the stub could not have blocked — an stdin already at EOF, a
/// stub that never got on PATH — fails on the control rather than passing on both halves, which is
/// the only way a timing assertion like this can be trusted.
#[test]
fn the_launchers_sudo_never_waits_for_a_password() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::temp("skein-sudo-it");
    let body = shell_function(LAUNCHER, "ensure_container_cgroup");
    // That the function still makes sudo calls — and deliberately NOT that they carry `-n`, which
    // is the property under test. Asserting the `-n` here made a dropped one fail this line in
    // 0.00s instead of failing on the hang, so the half of the test that actually drives a shell
    // never ran.
    assert_eq!(
        launcher_sudo_calls(&body).len(),
        2,
        "ensure_container_cgroup no longer makes the two sudo calls this test drives: {body}"
    );

    let stub_dir = scratch.path().join("bin");
    fs::create_dir_all(&stub_dir).unwrap();
    let stub = stub_dir.join("sudo");
    fs::write(
        &stub,
        // `printf` and not `echo`, because `echo -n` is the shell eating the very flag under test.
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$STUB_LOG"
if [ "$1" = -n ]; then
  # The real one refuses here, with status 1 and "sudo: a password is required". 0, so that the
  # caller carries on to its remaining calls instead of stopping at the first `|| return 0` — the
  # point is what it does NOT do, which is wait.
  exit 0
fi
: > "$STUB_BLOCKED"
read -r _password
exit 0
"#,
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

    // ---- the launcher as it stands: every call refuses at once ----
    let log = scratch.path().join("calls.log");
    let blocked = scratch.path().join("blocked");
    let mut child = spawn_with_stub(&body, &stub_dir, &log, &blocked);
    let finished = wait_for(&mut child, Duration::from_secs(20));
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        finished.on_its_own(),
        "ensure_container_cgroup {finished}. What this test is about is that it does not WAIT for \
         a password, which in a real launch is a box start that never finishes. Calls so \
         far: {calls:?}"
    );
    assert!(
        !blocked.exists(),
        "a call reached the stub without -n, so the stub blocked: {calls:?}"
    );
    // Both sudo lines, for both cgroups — so the body really ran rather than falling out early.
    let seen: Vec<&str> = calls.lines().collect();
    assert_eq!(
        seen.len(),
        4,
        "expected two sudo calls for each of the two cgroups: {seen:?}"
    );
    assert!(
        seen.iter().all(|c| c.starts_with("-n ")),
        "a call reached sudo with -n somewhere other than first among sudo's own options: {seen:?}"
    );

    // ---- the control: the same body with the flag taken off must hang ----
    let log = scratch.path().join("sabotage.log");
    let blocked = scratch.path().join("sabotage-blocked");
    let without = body.replace("sudo -n ", "sudo ");
    assert_ne!(without, body, "the sabotage did not apply");
    let mut child = spawn_with_stub(&without, &stub_dir, &log, &blocked);
    // Wait for the stub to say it is blocking, so that a slow box cannot pass this by being slow.
    let reached = poll_for(|| blocked.exists(), Duration::from_secs(20));
    assert!(
        reached,
        "the stub sudo was never reached without -n, so the control proves nothing about the half \
         above: {:?}",
        fs::read_to_string(&log).unwrap_or_default()
    );
    // **The assertion that used to accuse the wrong thing** (SKEIN-901). Written as
    // `assert!(!wait_for(..))` it says "a sudo with no -n returned anyway" about a child that was
    // SIGKILLed by a sibling test's teardown just as readily as about one that really did return,
    // and those are opposite diagnoses: the first is somebody else's bug four directories away and
    // the second is this control being worthless. Naming which ending happened costs nothing — the
    // `ExitStatus` is already in hand.
    let blocking = wait_for(&mut child, Duration::from_secs(2));
    assert!(
        matches!(blocking, Ended::No),
        "the control child {blocking}. If it exited, a sudo with no -n returned anyway, this stub \
         cannot block, and the first half of this test passes for a reason that has nothing to do \
         with -n"
    );
    // Closing its stdin is the EOF the stub's `read` is waiting for, so nothing is left running.
    drop(child.stdin.take());
    let unwound = wait_for(&mut child, Duration::from_secs(20));
    assert!(
        unwound.on_its_own(),
        "the control {unwound} after its stdin closed, rather than unwinding"
    );
}

/// A top-level shell function lifted out of a script, matched at its own indentation (column 0).
fn shell_function(script: &str, name: &str) -> String {
    let open = format!("{name}() {{");
    let mut body = String::new();
    for line in script.lines() {
        if body.is_empty() && !line.starts_with(&open) {
            continue;
        }
        body.push_str(line);
        body.push('\n');
        if !body.is_empty() && line == "}" {
            return body;
        }
    }
    panic!("`{name}` is no longer a top-level function in box-session.sh");
}

/// Run a shell function body with `sudo` resolving only to the stub, and stdin a pipe nobody writes
/// to — which is the thing a password prompt waits on.
fn spawn_with_stub(body: &str, stub_dir: &Path, log: &Path, blocked: &Path) -> std::process::Child {
    // Named absolutely, because the PATH below is the stub directory alone — a `bash` looked up on
    // it would not be found either, which is how this first failed.
    let bash = ["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .expect("a bash to run the launcher's own function body with");
    Command::new(bash)
        .arg("-c")
        .arg(format!("{body}\nensure_container_cgroup\n"))
        // The stub directory ALONE: there is no path by which the machine's real sudo can be
        // reached from here, which is what makes this safe to run anywhere.
        .env("PATH", stub_dir)
        .env("STUB_LOG", log)
        .env("STUB_BLOCKED", blocked)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("bash")
}

/// How a child ended within `budget`, or that it did not — because "still running" and "killed" and
/// "returned" are three different findings and a `bool` reports two of them as one (SKEIN-901).
///
/// A child that EXITED because the thing under test let it, and a child that was KILLED by something
/// else on this box, are indistinguishable to "is it still running" — and the assertion below names
/// only the first. When the second happened here, the message sent the reader to look at `sudo -n`
/// and at box load; the cause was a SIGKILL from a sibling test's teardown four directories away,
/// and it cost about an hour of looking for a load flake that does not exist.
///
/// **The distinction is free**, which is the whole argument for making it: the `ExitStatus` is
/// already in hand, and `ExitStatusExt::signal()` is `Some(9)` for a killed child and `None` for one
/// that exited on its own. Nothing is waited for that was not already waited for.
#[derive(Debug)]
enum Ended {
    /// Still running when the budget ran out.
    No,
    /// Ran to completion on its own, with this status code.
    Exited(i32),
    /// Ended by a signal. **Something outside this test killed it**, so whatever the test was about
    /// to conclude from the ending is about that killer and not about the code under test.
    Killed(i32),
}

impl Ended {
    /// Did it end *of its own accord* — which is what every caller that wants "it returned" means,
    /// and what a bare "is it still running" quietly answers `true` to for a corpse.
    fn on_its_own(&self) -> bool {
        matches!(self, Ended::Exited(_))
    }
}

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ended::No => write!(f, "was still running when the budget ran out"),
            Ended::Exited(code) => write!(f, "exited on its own with status {code}"),
            Ended::Killed(signal) => write!(
                f,
                "was KILLED by signal {signal} — something outside this test ended it, and nothing \
                 here is evidence about the behaviour under test. On this box that has meant a \
                 sibling test's teardown: see `fixture_processes`, which exempts a live descendant \
                 of this process for exactly that reason"
            ),
        }
    }
}

fn wait_for(child: &mut std::process::Child, budget: Duration) -> Ended {
    use std::os::unix::process::ExitStatusExt;
    let mut status = None;
    poll_for(
        || match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                true
            }
            _ => false,
        },
        budget,
    );
    match status {
        None => Ended::No,
        // `signal()` first and not `code()` first: a killed child's `code()` is `None`, so a match
        // written the other way round reports every kill as an unknown exit status.
        Some(s) => match s.signal() {
            Some(signal) => Ended::Killed(signal),
            None => Ended::Exited(s.code().unwrap_or(-1)),
        },
    }
}

/// **A child that was killed does not read as one that returned** (SKEIN-901).
///
/// Both endings, because one alone proves nothing. Reporting every ended child as `Killed` would
/// satisfy the first half and fail the second; reporting every one as `Exited` — which is what the
/// `bool` this replaced effectively did, since it said only "not running any more" — fails the
/// first. And each child is asserted to be RUNNING before it is ended, so the ending being reported
/// on is the one this test caused rather than a spawn that never happened (SKEIN-833).
///
/// `sleep` with a bounded argument rather than an unbounded blocker, so a failure between the spawn
/// and the kill cannot leave this test's own orphan behind.
#[test]
fn a_killed_child_is_reported_as_killed_and_not_as_having_returned() {
    let mut killed = Command::new("sleep")
        .arg("400")
        .spawn()
        .expect("a child to kill");
    let running = wait_for(&mut killed, Duration::from_millis(200));
    let killed_pid = killed.id();
    killed.kill().expect("SIGKILL it by pid — never a pattern");
    let after_kill = wait_for(&mut killed, Duration::from_secs(20));

    let mut returns = Command::new("/bin/true")
        .spawn()
        .expect("a child that ends");
    let after_exit = wait_for(&mut returns, Duration::from_secs(20));

    assert!(
        matches!(running, Ended::No),
        "pid {killed_pid} {running} before anything killed it, so what is asserted below is not \
         about a kill at all"
    );
    assert!(
        matches!(after_kill, Ended::Killed(9)),
        "a child this test SIGKILLed itself came back as `{after_kill:?}`. A reader of that is \
         sent to look at the code under test for an ending that something outside it caused"
    );
    assert!(
        !after_kill.on_its_own(),
        "a killed child answers `on_its_own`, so every caller that asks whether the thing under \
         test returned is answered `yes` by a corpse: {after_kill:?}"
    );
    assert!(
        matches!(after_exit, Ended::Exited(0)) && after_exit.on_its_own(),
        "a child that ran to completion came back as `{after_exit:?}`, so the kill above is \
         reported as a kill only because nothing is ever reported as an exit"
    );
    assert!(
        after_kill.to_string().contains("KILLED by signal 9")
            && !after_kill.to_string().contains("exited"),
        "the message a failing assertion would print does not say which ending it saw: {after_kill}"
    );
}
