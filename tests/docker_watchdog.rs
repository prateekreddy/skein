//! The Docker daemon coming back is a blip, not a fleet restart.
//!
//! `dockerd` runs with pid 1 for a parent and nothing supervising it, so a container that gets it
//! killed costs a rebuild of the whole fleet to recover one process. The watchdog lives in
//! `fleet-agent.py`, which is already the long-lived in-sandbox process, and this drives it.
//!
//! **The shipped file is imported and run**, the way `tests/mail_provenance.rs` runs the shipped
//! mailbox script: the decisions are in Python, so asserting them in Rust would be asserting a
//! reimplementation. Every collaborator the watchdog uses — finding the daemon, reading its argv,
//! spawning it, the clock, the sleep — is injected for exactly this.

use std::process::Command;

fn python() -> Option<&'static str> {
    Command::new("sh")
        .arg("-c")
        .arg("command -v python3 >/dev/null 2>&1")
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| "python3")
}

/// Drive `DockerWatch` with a scripted world and print what it decided, one word per pass.
fn drive(scenario: &str) -> String {
    let Some(python) = python() else {
        return "SKIP".into();
    };
    let program = format!(
        r#"
import importlib.util, sys
spec = importlib.util.spec_from_file_location("agent", "src/fleet-agent.py")
agent = importlib.util.module_from_spec(spec)
spec.loader.exec_module(agent)

class World:
    def __init__(self, pids, argv=None):
        self.pids = list(pids)          # one entry per look(): a pid or None
        self.argv = argv
        self.spawned = []
        self.slept = []
        self.said = []
    def find(self):
        # `look()` asks twice in a pass where the daemon is missing — once at the top, once after
        # the grace. The second ask is what lets another supervisor win, so the script is a list of
        # answers rather than a state: `[9, None, 4242]` is "alive, then gone, then back by itself".
        return self.pids.pop(0) if self.pids else None
    def argv_of(self, pid):
        return self.argv
    def spawn(self, argv):
        self.spawned.append(list(argv))
    def sleep(self, s):
        self.slept.append(s)
    def log(self, line):
        self.said.append(line)

{scenario}
"#
    );
    let out = Command::new(python)
        .arg("-c")
        .arg(&program)
        .output()
        .expect("run python");
    assert!(
        out.status.success(),
        "the scenario failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_daemon_that_dies_is_restarted_with_the_command_line_it_had() {
    // Alive on the first look — which is when the argv is learned — then gone, and gone again after
    // the grace. The restart uses what was remembered, not a guess.
    let said = drive(
        r#"
w = World([9, None, None], argv=["/usr/bin/dockerd", "--host=unix:///run/docker.sock"])
watch = agent.DockerWatch(find=w.find, argv_of=w.argv_of, spawn=w.spawn, sleep=w.sleep, log=w.log)
first = watch.look()
second = watch.look()
print(first, second, watch.restarts, w.spawned[0][0], w.spawned[0][1], sep="|")
"#,
    );
    if said == "SKIP" {
        eprintln!("skipping: no python3");
        return;
    }
    assert_eq!(
        said, "alive|restarted|1|/usr/bin/dockerd|--host=unix:///run/docker.sock",
        "the daemon was not brought back with its own command line"
    );
}

#[test]
fn another_supervisor_that_wins_the_race_is_left_alone() {
    // The grace is the whole point: `PPID 1` does not say whether init spawned dockerd or merely
    // reaped it, so something unseen may be supervising it. Two dockerds is a worse failure than
    // none, so a daemon that comes back during the grace is not restarted again.
    let said = drive(
        r#"
w = World([9, None, 4242], argv=["/usr/bin/dockerd"])
watch = agent.DockerWatch(find=w.find, argv_of=w.argv_of, spawn=w.spawn, sleep=w.sleep, log=w.log)
watch.look()
verdict = watch.look()     # gone at the top, back by itself after the grace
print(verdict, len(w.spawned), watch.restarts, sep="|")
"#,
    );
    if said == "SKIP" {
        return;
    }
    assert_eq!(
        said, "recovered|0|0",
        "the watchdog started a second daemon beside one that was already coming back"
    );
}

#[test]
fn an_agent_that_never_saw_the_daemon_says_so_instead_of_guessing() {
    // There is no safe default command line for dockerd, and inventing one is how a fleet ends up
    // with a daemon configured differently from the one it had — cgroup-parent included, which is
    // the setting the whole memory plan rests on.
    let said = drive(
        r#"
w = World([None, None, None], argv=None)
watch = agent.DockerWatch(find=w.find, argv_of=w.argv_of, spawn=w.spawn, sleep=w.sleep, log=w.log)
first = watch.look()
second = watch.look()
print(first, second, len(w.spawned), len(w.said), sep="|")
"#,
    );
    if said == "SKIP" {
        return;
    }
    // Nothing spawned, and said exactly once — a line every five seconds would be noise about a
    // condition that cannot change on its own.
    assert_eq!(
        said, "unknown|unknown|0|1",
        "it guessed, or it repeated itself"
    );
}

#[test]
fn a_daemon_that_will_not_start_is_not_respawned_in_a_loop() {
    // The failure mode of a watchdog: a dockerd that dies immediately, respawned every grace period
    // for ever, on a sandbox that is already unwell. The wait grows instead.
    let said = drive(
        r#"
w = World([9] + [None] * 12, argv=["/usr/bin/dockerd"])
watch = agent.DockerWatch(find=w.find, argv_of=w.argv_of, spawn=w.spawn, sleep=w.sleep, log=w.log)
for _ in range(6):
    watch.look()
print(len(w.spawned), w.slept[0], w.slept[-1] > w.slept[0], w.slept[-1] <= agent.DOCKER_BACKOFF_MAX, sep="|")
"#,
    );
    if said == "SKIP" {
        return;
    }
    assert_eq!(
        said, "5|20.0|True|True",
        "the wait between attempts did not grow, or grew without a ceiling"
    );
}

#[test]
fn what_the_host_can_read_is_a_fact_rather_than_a_diagnosis() {
    let said = drive(
        r#"
w = World([9, 9], argv=["/usr/bin/dockerd"])
watch = agent.DockerWatch(find=w.find, argv_of=w.argv_of, spawn=w.spawn, sleep=w.sleep, log=w.log)
watch.look()
snap = watch.snapshot()
print(sorted(snap.keys()), snap["argv_known"], snap["restarts"], sep="|")
"#,
    );
    if said == "SKIP" {
        return;
    }
    assert_eq!(
        said, "['argv_known', 'last_restart', 'note', 'pid', 'restarts']|True|0",
        "the snapshot the host reads changed shape"
    );
}
