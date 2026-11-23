//! The fence: two fleets on one volume, against fixture roots and the real `skein` binary. Each
//! test names the change that fails it.

mod common;

use common::{env_lock, env_pins, have, skip, Scratch};
use skein::fence::{self, read_owner, FlipError, Owner, Step};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};

const KILL_AT: &str = "SKEIN_TEST_FENCE_KILL_AT";
const KILL_HOME: &str = "SKEIN_TEST_FENCE_HOME";
const KILL_FROM: &str = "SKEIN_TEST_FENCE_FROM";

struct Volume {
    _scratch: Scratch,
    base: PathBuf,
    home: PathBuf,
}

/// A volume with no owner record, as every install before the fence has.
fn volume(tag: &str) -> Volume {
    let scratch = Scratch::temp(&format!("migrate-fence-{tag}"));
    let base = scratch.path().to_path_buf();
    let home = base.join("volume");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("VERSION"), "1\n").unwrap();
    Volume {
        _scratch: scratch,
        base,
        home,
    }
}

impl Volume {
    /// A fleet root of its own, as a sandbox has.
    fn fleet(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        fs::create_dir_all(root.join(".skein")).unwrap();
        root
    }

    fn skein(&self, root: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_skein"))
            .args(args)
            .env("SKEIN_HOME", &self.home)
            .env("SKEIN_FLEET_ROOT", root)
            .env_remove("SKEIN_BOX")
            .current_dir(&self.base)
            .output()
            .unwrap()
    }

    fn owner(&self) -> Owner {
        read_owner(&self.home).unwrap().expect("an owner")
    }
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn id_of(root: &Path) -> String {
    fs::read_to_string(root.join(".skein/fleet-id"))
        .unwrap()
        .trim()
        .to_string()
}

fn passes(volume: &Volume, root: &Path) -> bool {
    let out = volume.skein(root, &["fleet-owner", "check"]);
    match out.status.code() {
        Some(0) => true,
        Some(fence::FENCED_EXIT) => false,
        other => panic!("check ended {other:?}: {}", said(&out)),
    }
}

fn repo(home: &Path) -> skein::repos::Repo {
    skein::repos::Repo {
        id: "thing".to_string(),
        source: "https://example.invalid/o/thing.git".to_string(),
        store: home.join("repos/thing/store/data").display().to_string(),
        ..Default::default()
    }
}

/// Fails if `start_box` loses its owner check, or if the check refuses the owner.
#[test]
fn only_the_owner_can_start_a_box() {
    let _guard = env_lock();
    let v = volume("start");
    let (a, b) = (v.fleet("fleet-a"), v.fleet("fleet-b"));
    assert!(passes(&v, &a), "the first fleet adopts");
    assert!(!passes(&v, &b), "the second fleet is fenced");

    let crossings = v.base.join("crossings");
    let log = crossings.clone();
    let _seam = skein::place::seam::install(Box::new(move |argv: &[String]| {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        writeln!(f, "{}", argv.join(" ")).unwrap();
        Some(vec!["false".to_string()])
    }));
    let mut env = env_pins();
    env.set("SKEIN_HOME", &v.home);
    env.unset("SKEIN_BOX");

    env.set("SKEIN_FLEET_ROOT", &b);
    let why = skein::fleet::start_box(
        "thing-x",
        &repo(&v.home),
        "main",
        "true",
        skein::place::Purpose::Manual,
    )
    .expect_err("a fleet that is not the owner starts nothing");
    assert!(why.contains("does not own"), "{why}");
    assert!(why.contains(&id_of(&a)), "names the owner: {why}");
    assert!(!crossings.exists(), "the fenced start reached the fleet");
    assert!(
        !v.home.join("starts").exists(),
        "the fenced start wrote to the volume"
    );

    env.set("SKEIN_FLEET_ROOT", &a);
    let why = skein::fleet::start_box(
        "thing-x",
        &repo(&v.home),
        "main",
        "true",
        skein::place::Purpose::Manual,
    )
    .expect_err("the stand-in fails every command");
    assert!(!why.contains("does not own"), "{why}");
    // Written only past the gate, by the start that went on to fail at the stand-in.
    assert!(
        v.home.join("starts/thing-x.err").exists(),
        "the owner's start did not get past the fence: {why}"
    );
}

/// Fails if the CLI's gate is removed: the second fleet would list the first fleet's volume.
#[test]
fn a_cli_on_a_fenced_fleet_refuses_and_says_what_to_do() {
    let v = volume("cli");
    let (a, b) = (v.fleet("fleet-a"), v.fleet("fleet-b"));
    assert!(passes(&v, &a));
    let before = manifest(&v.home);
    let out = v.skein(&b, &["ls"]);
    assert_eq!(
        manifest(&v.home),
        before,
        "a refused CLI wrote to the volume"
    );
    assert!(!out.status.success(), "{}", said(&out));
    let text = said(&out);
    assert!(text.contains(&id_of(&a)), "names the owner: {text}");
    assert!(
        text.contains(&format!("fleet-owner claim {}", id_of(&a))),
        "says what to do: {text}"
    );
}

/// The child half of the kill test: flip, and SIGKILL itself at the named step.
#[test]
fn flip_child() {
    let Ok(at) = std::env::var(KILL_AT) else {
        return;
    };
    let home = PathBuf::from(std::env::var(KILL_HOME).unwrap());
    let expected = std::env::var(KILL_FROM)
        .ok()
        .map(|_| read_owner(&home).unwrap().unwrap());
    let at: usize = at.parse().unwrap();
    let _ = fence::flip_observed(&home, expected.as_ref(), "fleet-b", "mac", &mut |step| {
        if step == Step::ALL[at] {
            unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        }
    });
    unreachable!("the flip should have been killed");
}

fn killed_flip(home: &Path, at: usize, from_owner: bool) {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", "flip_child", "--nocapture"])
        .env(KILL_AT, at.to_string())
        .env(KILL_HOME, home);
    if from_owner {
        child.env(KILL_FROM, "1");
    }
    let status = child.output().unwrap().status;
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "step {at}: {status:?}"
    );
}

/// Fails if the record is written before the claim, or if a reader trusts the record over a claim:
/// the step after the claim would then answer with the old fleet.
#[test]
fn a_kill_at_every_step_of_a_flip_leaves_exactly_one_owner() {
    for (at, step) in Step::ALL.iter().enumerate() {
        let v = volume(&format!("kill-{at}"));
        let (a, b) = (v.fleet("fleet-a"), v.fleet("fleet-b"));
        assert!(passes(&v, &a));
        fs::write(b.join(".skein/fleet-id"), "fleet-b\n").unwrap();
        let before = v.owner();

        killed_flip(&v.home, at, true);

        let after = v.owner();
        let (pass_a, pass_b) = (passes(&v, &a), passes(&v, &b));
        assert_ne!(pass_a, pass_b, "{step:?}: exactly one fleet may run");
        if *step == Step::BeforeClaim {
            assert_eq!(after, before, "nothing was claimed yet");
            assert!(pass_a, "{step:?}: the old fleet still owns it");
        } else {
            assert_eq!(after.fleet_id, "fleet-b", "{step:?}");
            assert_eq!(after.epoch, before.epoch + 1, "{step:?}");
            assert!(pass_b, "{step:?}: the new fleet owns it");
        }

        // Rerunning the same flip finishes it, from either side of the commit.
        let done = fence::flip(&v.home, Some(&after), "fleet-b", "mac").unwrap();
        assert_eq!(done.fleet_id, "fleet-b", "{step:?}");
        let record: Owner =
            serde_json::from_slice(&fs::read(v.home.join("fleet-owner")).unwrap()).unwrap();
        assert_eq!(record, done, "{step:?}: the record catches up");
    }
}

/// Fails if adoption can leave a state no fleet can start from.
#[test]
fn a_kill_during_adoption_leaves_the_volume_usable() {
    for (at, step) in Step::ALL.iter().enumerate() {
        let v = volume(&format!("adopt-{at}"));
        let b = v.fleet("fleet-b");
        fs::write(b.join(".skein/fleet-id"), "fleet-b\n").unwrap();
        killed_flip(&v.home, at, false);
        assert!(passes(&v, &b), "{step:?}: the adopter starts");
        assert_eq!(v.owner().fleet_id, "fleet-b", "{step:?}");
    }
}

/// Fails if a claim stops being exclusive, for example if it is written by rename.
#[test]
fn a_racing_adoption_has_one_winner() {
    for round in 0..40 {
        let v = volume(&format!("race-{round}"));
        let home = Arc::new(v.home.clone());
        let gate = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let (home, gate) = (home.clone(), gate.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    let id = format!("fleet-{n}");
                    (id.clone(), fence::flip(&home, None, &id, "mac"))
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        let owner = v.owner();
        assert_eq!(owner.epoch, 1);
        let winners: Vec<_> = results.iter().filter(|(_, r)| r.is_ok()).collect();
        assert_eq!(winners.len(), 1, "{results:?}");
        assert_eq!(winners[0].0, owner.fleet_id);
        for (_, result) in &results {
            if let Err(e) = result {
                assert_eq!(*e, FlipError::Lost(Some(owner.clone())));
            }
        }
    }
}

/// The same race through real processes and real first starts.
/// Fails if minting the id or adopting stops being atomic across processes.
#[test]
fn racing_first_starts_of_two_sandboxes_have_one_winner() {
    for round in 0..30 {
        let v = volume(&format!("procs-{round}"));
        let roots: Vec<_> = (0..6).map(|n| v.fleet(&format!("fleet-{n}"))).collect();
        // Each spins until `go` exists, so all six reach the volume together.
        let go = v.base.join("go");
        let children: Vec<_> = roots
            .iter()
            .map(|root| {
                Command::new("sh")
                    .arg("-c")
                    .arg("while [ ! -e \"$1\" ]; do :; done; exec \"$2\" fleet-owner check")
                    .args(["sh", go.to_str().unwrap(), env!("CARGO_BIN_EXE_skein")])
                    .env("SKEIN_HOME", &v.home)
                    .env("SKEIN_FLEET_ROOT", root)
                    .env_remove("SKEIN_BOX")
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        fs::write(&go, "").unwrap();
        let codes: Vec<_> = children
            .into_iter()
            .map(|c| c.wait_with_output().unwrap().status.code())
            .collect();
        assert_eq!(
            codes.iter().filter(|c| **c == Some(0)).count(),
            1,
            "{codes:?}"
        );
        assert_eq!(
            codes
                .iter()
                .filter(|c| **c == Some(fence::FENCED_EXIT))
                .count(),
            5,
            "{codes:?}"
        );
    }
}

/// Fails if the fence refuses a volume that has no record, which would lock out every upgrade.
#[test]
fn a_volume_with_no_record_keeps_working_and_is_adopted_once() {
    let v = volume("upgrade");
    let a = v.fleet("fleet-a");
    assert!(read_owner(&v.home).unwrap().is_none());
    let out = v.skein(&a, &["repos"]);
    assert!(out.status.success(), "{}", said(&out));
    let first = v.owner();
    assert_eq!(first.fleet_id, id_of(&a));
    assert_eq!(first.epoch, 1);
    assert!(v.skein(&a, &["repos"]).status.success());
    assert_eq!(v.owner(), first, "a restart does not move the epoch");
}

/// Fails if a fleet that cannot record its id is refused instead of carrying on unowned.
#[test]
fn a_fleet_that_cannot_write_its_id_is_not_locked_out() {
    let v = volume("readonly");
    let a = v.fleet("fleet-a");
    // A directory where the id belongs: the id can be neither read nor linked into place.
    fs::create_dir(a.join(".skein/fleet-id")).unwrap();
    let out = v.skein(&a, &["repos"]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(read_owner(&v.home).unwrap().is_none());
}

/// Fails if a box can adopt: what it sees at `$SKEIN_HOME` is its own cover, not the volume.
#[test]
fn a_box_never_adopts() {
    let v = volume("box");
    let a = v.fleet("fleet-a");
    let out = Command::new(env!("CARGO_BIN_EXE_skein"))
        .arg("repos")
        .env("SKEIN_HOME", &v.home)
        .env("SKEIN_FLEET_ROOT", &a)
        .env("SKEIN_BOX", "thing-x")
        .current_dir(&v.base)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", said(&out));
    assert!(read_owner(&v.home).unwrap().is_none());
    assert!(!a.join(".skein/fleet-id").exists());
}

/// Fails if `doctor`, `repoint`, `migrate` or `fleet-owner` go back behind the gate.
#[test]
fn a_fenced_fleet_can_still_run_the_repair_commands() {
    let v = volume("repair");
    let (a, b) = (v.fleet("fleet-a"), v.fleet("fleet-b"));
    assert!(passes(&v, &a));
    let refused = said(&v.skein(&b, &["repos"]));
    assert!(refused.contains("does not own"), "control: {refused}");
    // Before `repoint` below, which writes by design.
    let before = manifest(&v.home);
    let doctor = said(&v.skein(&b, &["doctor"]));
    assert_eq!(
        manifest(&v.home),
        before,
        "a fenced doctor wrote to the volume"
    );

    for args in [
        &["doctor"][..],
        &["repoint"],
        &["migrate", "plan"],
        &["fleet-owner"],
    ] {
        let text = said(&v.skein(&b, args));
        assert!(
            !text.contains("does not own"),
            "{args:?} was refused: {text}"
        );
    }
    assert!(doctor.contains("FENCED"), "{doctor}");
    assert!(doctor.contains(&id_of(&a)), "{doctor}");
}

/// Fails if a lost id strands the owner, or if the claim is not checked against the real owner.
#[test]
fn a_fleet_that_lost_its_id_recovers_with_the_line_it_is_given() {
    let v = volume("rebuild");
    let old = v.fleet("old");
    assert!(passes(&v, &old));
    let gone = id_of(&old);
    let rebuilt = v.fleet("rebuilt");

    let text = said(&v.skein(&rebuilt, &["repos"]));
    assert!(
        text.contains(&format!("fleet-owner claim {gone}")),
        "{text}"
    );

    let wrong = v.skein(&rebuilt, &["fleet-owner", "claim", "fleet-wrong"]);
    assert!(!wrong.status.success(), "{}", said(&wrong));
    assert_eq!(v.owner().fleet_id, gone, "a wrong id changes nothing");

    let out = v.skein(&rebuilt, &["fleet-owner", "claim", &gone]);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(v.owner().fleet_id, id_of(&rebuilt));
    assert_eq!(v.owner().epoch, 2);
    assert!(passes(&v, &rebuilt));
    assert!(
        !passes(&v, &old),
        "the old fleet is fenced if it comes back"
    );
}

/// Fails if a receive-mode sandbox can adopt: it would take a live fleet's volume from it.
#[test]
fn a_sandbox_in_receive_mode_never_adopts_and_runs_once_it_is_the_owner() {
    let v = volume("receive");
    let new = v.fleet("new");
    fs::write(new.join(".skein/receive-mode"), "").unwrap();
    fs::write(new.join(".skein/fleet-id"), "fleet-new\n").unwrap();

    assert!(!passes(&v, &new));
    assert!(read_owner(&v.home).unwrap().is_none(), "it adopted");

    fence::flip(&v.home, None, "fleet-new", "mac").unwrap();
    assert!(passes(&v, &new));
    assert!(!new.join(".skein/receive-mode").exists());
}

/// What `bootstrap.sh` needs beyond the machine's own tools: a build that installs the real CLI.
fn bootstrap_stubs(v: &Volume, root: &Path) -> (PathBuf, PathBuf) {
    let bin = v.base.join("stubs");
    let log = v.base.join("stubs.log");
    fs::create_dir_all(&bin).unwrap();
    let release = root.join(".skein/src/target/release");
    let stubs = [
        (
            "git",
            "case \"$*\" in *rev-parse*) echo deadbee ;; esac\nexit 0".to_string(),
        ),
        (
            "cargo",
            format!(
                "case \"$*\" in *--version*) exit 0 ;; esac\nmkdir -p {r}\ncat {real} > {r}/skein\n\
                 chmod 755 {r}/skein\nprintf 'ELF' > {r}/skein-server\nmkdir -p {s}\n\
                 cat {doorway} > {s}/server-doorway.py\nexit 0",
                r = release.display(),
                s = root.join(".skein/src/src").display(),
                doorway = concat!(env!("CARGO_MANIFEST_DIR"), "/src/server-doorway.py"),
                real = env!("CARGO_BIN_EXE_skein"),
            ),
        ),
        ("tmux", "exit 1".to_string()),
        ("nproc", "echo 2".to_string()),
    ];
    for (name, body) in stubs {
        let at = bin.join(name);
        fs::write(
            &at,
            format!(
                "#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> {}\n{body}\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&at, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (bin, log)
}

fn bootstrap(v: &Volume, root: &Path, bin: &Path, receive: bool) -> Output {
    let meminfo = v.base.join("meminfo");
    fs::write(&meminfo, "MemTotal:        4194304 kB\n").unwrap();
    let cpuinfo = v.base.join("cpuinfo");
    fs::write(&cpuinfo, "processor\t: 0\nprocessor\t: 1\n").unwrap();
    let mut command = Command::new("bash");
    command
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/bootstrap.sh"))
        .current_dir(&v.base)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env_remove("BASH_ENV")
        .env_remove("SKEIN_BOX")
        .env_remove("SKEIN_BOOTSTRAP_STOP_AFTER")
        .env("SKEIN_MEMINFO", &meminfo)
        .env("SKEIN_CPUINFO", &cpuinfo)
        .env("SKEIN_FLEET_MEMORY", "4g")
        .env("SKEIN_FLEET_CPUS", "2")
        .env("SKEIN_HOME", &v.home)
        .env("SKEIN_FLEET_ROOT", root);
    if receive {
        command.env("SKEIN_BOOTSTRAP_RECEIVE", "1");
    }
    command.output().unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Fails if the receive check moves after the door, or if the install writes to the volume.
#[test]
fn a_receive_install_leaves_the_volume_alone_and_opens_no_door() {
    let v = volume("bootstrap-receive");
    let root = v.base.join("new");
    fs::create_dir_all(&root).unwrap();
    let (bin, log) = bootstrap_stubs(&v, &root);
    let before = listing(&v.home);

    let out = bootstrap(&v, &root, &bin, true);
    assert!(out.status.success(), "{}", said(&out));
    let ran = fs::read_to_string(&log).unwrap();
    assert!(ran.contains("cargo build"), "the build ran: {ran}");
    assert!(root.join(".skein/skein").exists(), "the CLI is installed");
    assert_eq!(listing(&v.home), before, "the volume was written");
    assert!(!root.join(".skein/start-door.sh").exists());
    assert!(!ran.contains("tmux"), "the door was approached: {ran}");

    // A plain run of the same file afterwards is still a receive install.
    let again = bootstrap(&v, &root, &bin, false);
    assert!(again.status.success(), "{}", said(&again));
    assert_eq!(listing(&v.home), before, "the second run wrote the volume");
    assert!(!root.join(".skein/start-door.sh").exists());
}

/// Fails if a fenced install writes the fleet kit, or stops short of the door again.
#[test]
fn a_fenced_install_goes_on_to_the_door_and_leaves_the_volume_alone() {
    let v = volume("bootstrap-fenced");
    let old = v.fleet("old");
    assert!(passes(&v, &old));
    let root = v.base.join("rebuilt");
    fs::create_dir_all(&root).unwrap();
    let (bin, log) = bootstrap_stubs(&v, &root);
    let before = manifest(&v.home);

    let out = bootstrap(&v, &root, &bin, false);
    let text = said(&out);
    assert!(
        text.contains(&format!("fleet-owner claim {}", id_of(&old))),
        "{text}"
    );
    let ran = fs::read_to_string(&log).unwrap();
    assert!(ran.contains("tmux"), "the door was not opened: {ran}");
    assert!(root.join(".skein/start-door.sh").exists());
    assert!(root.join(".skein/private/fenced").exists(), "log not held");
    assert_eq!(manifest(&v.home), before, "the install wrote to the volume");
}

/// Every path under `dir` with its type, size, mtime and content.
fn manifest(dir: &Path) -> Vec<String> {
    use std::hash::{Hash, Hasher};
    use std::os::unix::fs::MetadataExt;
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap().map(|e| e.unwrap()) {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let body = if meta.is_file() {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                fs::read(&path).unwrap().hash(&mut hasher);
                hasher.finish().to_string()
            } else if meta.is_symlink() {
                fs::read_link(&path).unwrap().display().to_string()
            } else {
                String::new()
            };
            out.push(format!(
                "{} {:o} {} {}.{} {body}",
                path.display(),
                meta.mode(),
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec()
            ));
            if meta.is_dir() {
                walk(&path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

const TOKEN: &str = "skein-test-fence-token";

/// A real doorway holding a port, with the real server behind it.
struct Door {
    child: std::process::Child,
    port: u16,
    root: PathBuf,
}

impl Drop for Door {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(self.root.join(".skein/private/server.tmux"))
            .arg("kill-server")
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn door(v: &Volume, root: &Path) -> Door {
    let dot = root.join(".skein");
    let doorway = dot.join("server-doorway.py");
    fs::write(
        &doorway,
        fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/server-doorway.py"
        ))
        .unwrap(),
    )
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let port_arg = port.to_string();
    let child = common::skein_server_behind("python3", [doorway.as_os_str(), port_arg.as_ref()])
        .arg(dot.join("server.door"))
        .env("SKEIN_HOME", &v.home)
        .env("SKEIN_FLEET_ROOT", root)
        .env("SKEIN_SERVER_PORT", port.to_string())
        .env("SKEIN_REGISTRY", v.base.join("sandboxes.json"))
        .env("SKEIN_GITHUB_API", "http://127.0.0.1:1")
        .env("SKEIN_CLAUDE_BIN", "/bin/false")
        .env("GH_TOKEN", "skein-test-fence-gh")
        .env("TMUX_TMPDIR", &v.base)
        .env_remove("GITHUB_TOKEN")
        .env_remove("SKEIN_BOX")
        .env_remove("SKEIN_IN_BOX")
        .env_remove("SKEIN_LISTEN_INHERITED_ONLY")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .current_dir(&v.base)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let door = Door {
        child,
        port,
        root: root.to_path_buf(),
    };
    for _ in 0..200 {
        if http(door.port, "GET", "/api/fleet/owner", Some(TOKEN), "").0 != 0 {
            return door;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the server never answered");
}

/// One plain HTTP request. Status 0 means the connection failed.
fn http(port: u16, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return (0, String::new());
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n{auth}\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return (0, String::new());
    }
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_string();
    (status, body)
}

/// A volume owned by one fleet, with the files a real one holds, and a second fleet beside it.
fn fenced_fixture(tag: &str, with_token: bool) -> (Volume, PathBuf, PathBuf) {
    let v = volume(tag);
    fs::write(v.home.join("config.json"), r#"{"fleet_sandbox":"example"}"#).unwrap();
    fs::write(v.home.join("repos.json"), "[]").unwrap();
    fs::write(v.base.join("sandboxes.json"), "{}").unwrap();
    if with_token {
        fs::write(v.home.join("api-token"), TOKEN).unwrap();
    }
    let (a, b) = (v.fleet("fleet-a"), v.fleet("fleet-b"));
    assert!(passes(&v, &a));
    (v, a, b)
}

fn offer(owner: &Owner) -> String {
    format!(
        r#"{{"fleet_id":"{}","epoch":{}}}"#,
        owner.fleet_id, owner.epoch
    )
}

/// Fails if a fenced server runs any of the normal startup or ticks, or serves a normal route.
#[test]
fn a_fenced_server_shows_the_owner_and_writes_nothing_to_the_volume() {
    if !have("python3") {
        skip("the take-over tests run the real doorway, which is python3");
        return;
    }
    let (v, a, b) = fenced_fixture("server-quiet", true);
    let before = manifest(&v.home);
    let owner = v.owner();
    let d = door(&v, &b);

    let (status, body) = http(d.port, "GET", "/api/fleet/owner", Some(TOKEN), "");
    assert_eq!(status, 200, "{body}");
    let seen: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(seen["fenced"], true, "{body}");
    assert_eq!(seen["owner"]["fleet_id"], id_of(&a), "{body}");
    assert_eq!(seen["owner"]["machine"], owner.machine, "{body}");
    assert_eq!(seen["owner"]["epoch"], owner.epoch, "{body}");
    assert!(seen["claim"]
        .as_str()
        .unwrap()
        .contains("fleet-owner claim"));

    for page in ["/", "/v2", "/vendor/fence.js"] {
        assert_eq!(http(d.port, "GET", page, Some(TOKEN), "").0, 200, "{page}");
    }
    let (status, body) = http(d.port, "GET", "/api/health", Some(TOKEN), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"fenced\":true"), "{body}");
    for (method, path) in [
        ("GET", "/api/boxes"),
        ("GET", "/api/usage"),
        ("GET", "/api/settings"),
        ("POST", "/api/settings"),
        ("POST", "/api/boxes/thing-x/create"),
        ("POST", "/api/boxes/thing-x/restart"),
        ("POST", "/api/repos"),
    ] {
        let (status, body) = http(d.port, method, path, Some(TOKEN), "{}");
        assert_eq!(status, 409, "{method} {path}: {body}");
    }
    // Past the shortest background tick a normal server runs, the 5 s mailbox relay.
    std::thread::sleep(std::time::Duration::from_secs(7));
    assert_eq!(
        manifest(&v.home),
        before,
        "a fenced server wrote to the volume"
    );
    assert_eq!(v.owner(), owner, "nothing took the volume over by itself");
}

/// Fails if a fenced server mints a token on a volume that has none.
#[test]
fn a_fenced_server_never_mints_a_token_on_the_volume() {
    if !have("python3") {
        skip("the take-over tests run the real doorway, which is python3");
        return;
    }
    let (v, _a, b) = fenced_fixture("server-token", false);
    let before = manifest(&v.home);
    let d = door(&v, &b);
    let (status, _) = http(
        d.port,
        "GET",
        "/api/fleet/owner",
        Some("skein-test-wrong"),
        "",
    );
    drop(d);
    assert_eq!(status, 401);
    assert_eq!(
        manifest(&v.home),
        before,
        "a token was written to the volume"
    );
}

/// Fails if the take-over route loses the token gate.
#[test]
fn a_take_over_without_the_token_is_refused() {
    if !have("python3") {
        skip("the take-over tests run the real doorway, which is python3");
        return;
    }
    let (v, _a, b) = fenced_fixture("server-auth", true);
    let owner = v.owner();
    let d = door(&v, &b);
    let (status, _) = http(d.port, "POST", "/api/fleet/take-over", None, &offer(&owner));
    assert_eq!(status, 401);
    let (status, _) = http(
        d.port,
        "POST",
        "/api/fleet/take-over",
        Some("skein-test-wrong"),
        &offer(&owner),
    );
    assert_eq!(status, 401);
    assert_eq!(v.owner(), owner);
}

/// Fails if the route stops comparing the owner the page was shown with the owner now.
#[test]
fn a_take_over_from_a_stale_page_is_refused() {
    if !have("python3") {
        skip("the take-over tests run the real doorway, which is python3");
        return;
    }
    let (v, _a, b) = fenced_fixture("server-stale", true);
    let shown = v.owner();
    let d = door(&v, &b);
    let newer = fence::flip(&v.home, Some(&shown), "fleet-newer", "mac").unwrap();

    let (status, body) = http(
        d.port,
        "POST",
        "/api/fleet/take-over",
        Some(TOKEN),
        &offer(&shown),
    );
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("\"why\":\"stale\""), "{body}");
    assert!(body.contains("fleet-newer"), "names the new owner: {body}");
    assert_eq!(v.owner(), newer, "a stale page took the volume");
}

/// Fails if a take-over does not flip, or the server does not come back as a normal one.
#[test]
fn a_take_over_leaves_one_owner_and_boxes_can_start() {
    if !have("python3") {
        skip("the take-over tests run the real doorway, which is python3");
        return;
    }
    let _guard = env_lock();
    let (v, a, b) = fenced_fixture("server-take", true);
    let shown = v.owner();
    let d = door(&v, &b);
    let (status, body) = http(
        d.port,
        "POST",
        "/api/fleet/take-over",
        Some(TOKEN),
        &offer(&shown),
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"restarting\":true"), "{body}");

    let mut normal = false;
    for _ in 0..200 {
        let (status, body) = http(d.port, "GET", "/api/health", Some(TOKEN), "");
        if status == 200 && !body.contains("\"fenced\":true") {
            normal = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(normal, "the cockpit did not come back as a normal server");
    let (_, body) = http(d.port, "GET", "/api/fleet/owner", Some(TOKEN), "");
    assert!(body.contains("\"fenced\":false"), "{body}");
    assert_eq!(v.owner().fleet_id, id_of(&b));
    assert_eq!(v.owner().epoch, shown.epoch + 1);
    assert!(passes(&v, &b));
    assert!(!passes(&v, &a), "the old fleet is refused");
    assert!(
        !b.join(".skein/private/fenced").exists(),
        "the log is still held"
    );
    drop(d);

    let _seam =
        skein::place::seam::install(Box::new(|_: &[String]| Some(vec!["false".to_string()])));
    let mut env = env_pins();
    env.set("SKEIN_HOME", &v.home);
    env.set("SKEIN_FLEET_ROOT", &b);
    env.unset("SKEIN_BOX");
    let why = skein::fleet::start_box(
        "thing-x",
        &repo(&v.home),
        "main",
        "true",
        skein::place::Purpose::Manual,
    )
    .expect_err("the stand-in fails every command");
    assert!(!why.contains("does not own"), "{why}");
    assert!(v.home.join("starts/thing-x.err").exists(), "{why}");
}

/// Fails if receive mode starts serving: its door stays closed.
#[test]
fn a_server_in_receive_mode_does_not_serve() {
    let (v, _a, b) = fenced_fixture("server-receive", true);
    fs::write(b.join(".skein/receive-mode"), "").unwrap();
    let before = manifest(&v.home);
    let mut server = common::skein_server()
        .env("SKEIN_HOME", &v.home)
        .env("SKEIN_FLEET_ROOT", &b)
        .env("SKEIN_ADDR", "127.0.0.1:0")
        .env_remove("SKEIN_BOX")
        .env_remove("SKEIN_LISTEN_INHERITED_ONLY")
        .current_dir(&v.base)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut ended = None;
    for _ in 0..100 {
        ended = server.try_wait().unwrap();
        if ended.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if ended.is_none() {
        let _ = server.kill();
        let _ = server.wait();
    }
    assert!(
        ended.is_some_and(|status| !status.success()),
        "it went on to serve"
    );
    assert_eq!(manifest(&v.home), before);
}

/// Fails if the log reader keeps writing to the volume while the hold exists.
#[test]
fn the_server_log_is_not_written_while_it_is_held() {
    let v = volume("log-hold");
    let hold = v.base.join("fenced");
    let log = v.home.join("server.log");
    let script = format!(
        "{}\nSKEIN_LOG_HOLD='{}' skein_logged '{}' 1000 sh -c 'echo line'; sleep 0.2",
        include_str!("../src/door.sh"),
        hold.display(),
        log.display()
    );
    fs::write(&hold, "").unwrap();
    assert!(Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .unwrap()
        .status
        .success());
    assert!(!log.exists(), "the held log was written");
    fs::remove_file(&hold).unwrap();
    assert!(Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .unwrap()
        .status
        .success());
    assert!(fs::read_to_string(&log).unwrap().contains("line"));
}
