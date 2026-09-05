//! What a box's `sudo` actually files, run as the shell a box actually runs.
//!
//! A box cannot install a system package — its user namespace maps one uid, so `sudo` there is
//! unfixable rather than unconfigured — and for a long time the shim said exactly that and stopped.
//! It told agents to "ask for it in the fleet" with nowhere to ask, which is a dead end dressed up
//! as advice. Now the install an agent typed becomes the request, and this drives that path through
//! the real `box-session.sh` rather than a copy of its parsing.
//!
//! The part worth testing hardest is not the happy path. Every name filed here is eventually spliced
//! into an `apt-get install` running as **root** in the sandbox, so the interesting cases are the
//! ones that must never reach the queue at all.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A throwaway fleet root, so the real `/boxes` is never touched.
struct Fleet {
    root: PathBuf,
}

impl Fleet {
    fn new(what: &str) -> Fleet {
        let root = PathBuf::from("/var/tmp")
            .join(format!("skein-substrate-it-{what}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Fleet { root }
    }

    /// Run `box-session.sh --request-package` the way the sudo shim runs it: the box's own name
    /// baked into argument 1, and **no `SKEIN_BOX` in the environment at all**.
    ///
    /// It is removed rather than left alone, because this test process is itself running inside a
    /// box and would otherwise hand the launcher that box's name. It is not *set* either, which is
    /// what it used to do: setting it to the same name that was passed made every assertion below
    /// blind to which of the two the launcher read, and the one property they claimed to pin — that
    /// a box cannot file under another box's name — was the property that made the difference.
    fn ask(&self, box_name: &str, argv: &[&str]) -> (i32, String) {
        self.ask_as(box_name, None, argv)
    }

    /// The same, with `SKEIN_BOX` set to a name of the caller's choosing — which is what a box can
    /// do to itself, and therefore the interesting case.
    fn ask_as(&self, arg_box: &str, env_box: Option<&str>, argv: &[&str]) -> (i32, String) {
        let launcher = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/box-session.sh");
        let mut cmd = Command::new("bash");
        cmd.arg(&launcher)
            .arg("--request-package")
            .arg(arg_box)
            .args(argv)
            .env("SKEIN_FLEET_ROOT", &self.root);
        match env_box {
            Some(b) => cmd.env("SKEIN_BOX", b),
            None => cmd.env_remove("SKEIN_BOX"),
        };
        let out = cmd.output().expect("bash to run the launcher");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    /// The queue root: read-only inside a box, with one directory under it bound read-write.
    fn queue(&self) -> PathBuf {
        self.root.join(".skein/substrate/requests")
    }

    fn json_in(dir: &PathBuf) -> Vec<serde_json::Value> {
        let Ok(entries) = fs::read_dir(dir) else {
            return vec![];
        };
        let mut out: Vec<_> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        out.sort_by_key(|v: &serde_json::Value| v["id"].as_str().unwrap_or("").to_string());
        out
    }

    /// What one box filed, read from **that box's own directory** — which is the only thing that
    /// says it is that box's.
    fn filed_under(&self, box_name: &str) -> Vec<serde_json::Value> {
        Fleet::json_in(&self.queue().join(box_name))
    }

    /// Every request in every box's drop-box. Deliberately **not** the queue root: a file written
    /// directly there belongs to no box, so counting one would be counting a request nobody can be
    /// asked about.
    fn queued(&self) -> Vec<serde_json::Value> {
        let Ok(dirs) = fs::read_dir(self.queue()) else {
            return vec![];
        };
        let mut out: Vec<serde_json::Value> = dirs
            .flatten()
            .filter(|e| e.path().is_dir())
            .flat_map(|e| Fleet::json_in(&e.path()))
            .collect();
        out.sort_by_key(|v: &serde_json::Value| v["id"].as_str().unwrap_or("").to_string());
        out
    }

    /// Anything sitting loose in the queue root, which nothing may write any more.
    fn loose(&self) -> Vec<serde_json::Value> {
        Fleet::json_in(&self.queue())
    }
}

/// Mode bits are how the box's mount namespace is stood in for here, and root ignores them.
fn running_as_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

impl Drop for Fleet {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn the_install_an_agent_typed_becomes_a_request() {
    if !have("jq") {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let f = Fleet::new("files");
    let (code, said) = f.ask(
        "web-main",
        &["apt-get", "install", "-y", "libnss3", "libatk1.0-0"],
    );
    assert_eq!(code, 0, "filing should succeed: {said}");

    let q = f.queued();
    assert_eq!(q.len(), 1, "one request, got {q:?}");
    assert_eq!(q[0]["box"], "web-main");
    assert_eq!(q[0]["kind"], "apt");
    assert_eq!(q[0]["state"], "pending");
    // `-y` is apt's, not a package. A request that carried it would put a flag on a root command
    // line as though someone had asked for it.
    assert_eq!(
        q[0]["packages"],
        serde_json::json!(["libnss3", "libatk1.0-0"]),
        "options must not be filed as packages"
    );
    assert_eq!(q[0]["remember"], true, "recording is the default");
    assert!(
        said.contains("pending approval") && said.contains("nothing is installed"),
        "the agent must be told nothing happened yet: {said}"
    );
}

/// **A request is filed in its own box's directory, and that is what says whose it is.**
///
/// Architecture §8.4 puts three steps in order — bind the artifact, make the request path per box,
/// *then* unmask the queue — and the middle one was skipped. One shared read-write directory meant
/// every box could delete, rewrite or flip the state of every other box's pending request, and
/// could file one in a neighbour's name.
#[test]
fn a_request_is_filed_in_its_own_boxs_drop_box() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("perbox");
    let (code, said) = f.ask("web-main", &["apt-get", "install", "ripgrep"]);
    assert_eq!(code, 0, "{said}");

    let mine = f.filed_under("web-main");
    assert_eq!(mine.len(), 1, "the request is in this box's own directory");
    assert_eq!(mine[0]["packages"], serde_json::json!(["ripgrep"]));
    assert!(
        f.loose().is_empty(),
        "nothing may be written loose in the queue root, where it would belong to no box: {:?}",
        f.loose()
    );
    assert!(
        f.filed_under("api").is_empty(),
        "it landed in another box's directory"
    );
}

/// **A box cannot file a request in another box's name**, and the reason is the mount, not a check.
///
/// The launcher binds `requests/<box>/` — and only that — read-write into each box, so a name that
/// is not this box's names a directory this box cannot write. Here the mount is stood in for by
/// mode bits: `web-main`'s drop-box is writable, `api`'s is not, and the queue root is left
/// **writable** deliberately — with it read-only, the old shared-directory code would fail for the
/// wrong reason and this test would pass without proving anything.
///
/// A box that tries anyway is told it could not file, which is the honest failure: the impersonation
/// does not half-succeed, and no request appears in the cockpit under the name it wanted.
#[test]
fn a_box_cannot_file_a_request_in_another_boxs_name() {
    if !have("jq") || running_as_root() {
        eprintln!("SKIPPED: needs jq and a uid that mode bits apply to");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let f = Fleet::new("impersonate");
    fs::create_dir_all(f.queue().join("web-main")).unwrap();
    fs::create_dir_all(f.queue().join("api")).unwrap();
    fs::set_permissions(f.queue().join("api"), fs::Permissions::from_mode(0o555)).unwrap();

    // Both spellings of the lie: the argument the caller typed, and the environment variable the
    // box owns. Neither is trusted, so neither works.
    let attempts = [
        f.ask("api", &["apt-get", "install", "ripgrep"]),
        f.ask_as("", Some("api"), &["apt-get", "install", "fd-find"]),
        // A name no box has, so there is no directory at all — under bwrap the queue root is
        // read-only and one cannot be made.
        f.ask("no-such-box", &["apt-get", "install", "jq"]),
    ];
    let _ = fs::set_permissions(f.queue().join("api"), fs::Permissions::from_mode(0o755));

    assert!(
        f.filed_under("api").is_empty(),
        "a request was filed in another box's name: {:?}",
        f.filed_under("api")
    );
    assert!(
        f.loose().is_empty(),
        "a request was written loose in the queue root: {:?}",
        f.loose()
    );
    for (n, (code, said)) in attempts.iter().enumerate() {
        // The first two must fail; the third names a directory that does not exist, and only the
        // real read-only mount stops that one, so it is asserted only not to have impersonated.
        if n < 2 {
            assert_eq!(*code, 4, "attempt {n} was not refused: {said}");
            assert!(
                said.contains("could not be recorded"),
                "attempt {n}: {said}"
            );
        }
    }
}

/// The environment does not decide which box filed a request, and it used to.
///
/// `request_package` preferred `$SKEIN_BOX` and said in a comment that it "cannot be argued with".
/// It is an environment variable of a process the box owns. The launcher's own argument is the
/// better hint of the two — the sudo shim bakes it in at generation time, outside the namespace —
/// so that is what is read, and the directory is what decides.
#[test]
fn the_environment_does_not_decide_which_box_filed_a_request() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("envbox");
    let (code, said) = f.ask_as("web-main", Some("api"), &["apt-get", "install", "ripgrep"]);
    assert_eq!(code, 0, "{said}");
    assert_eq!(
        f.filed_under("web-main").len(),
        1,
        "the launcher's own argument names the box, not the box's environment"
    );
    assert!(
        f.filed_under("api").is_empty(),
        "$SKEIN_BOX chose where the request went"
    );
}

/// A box name that is not a name is refused before it becomes a path.
///
/// Not a trust check — the mount is that — but a traversal would turn a refused ask into a write
/// somewhere else under the fleet root, and an agent that mistypes gets a sentence rather than an
/// EROFS from a path it did not mean to name.
#[test]
fn a_box_name_that_is_not_a_name_never_becomes_a_directory() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("badbox");
    for bad in ["../../.skein", "a/b", "-flag", "a;touch /tmp/x", ".."] {
        let (code, said) = f.ask(bad, &["apt-get", "install", "ripgrep"]);
        assert_eq!(code, 4, "{bad:?} was accepted as a box name: {said}");
        assert!(said.contains("is not a box name"), "{bad:?}: {said}");
    }
    assert!(f.queued().is_empty(), "{:?}", f.queued());
    assert!(f.loose().is_empty(), "{:?}", f.loose());
    assert!(
        !f.root.join(".skein/substrate/.skein").exists(),
        "a box name climbed out of the queue"
    );
}

#[test]
fn asking_twice_for_the_same_thing_is_one_decision() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("dedup");
    f.ask("web-main", &["apt-get", "install", "ripgrep", "fd-find"]);
    // The same ask from another box, with the arguments the other way round. An agent that retries
    // in a loop must not turn one decision into a hundred.
    let (code, said) = f.ask("api", &["apt", "install", "fd-find", "ripgrep"]);
    assert_eq!(code, 0);
    assert_eq!(f.queued().len(), 1, "the second ask joined the first");
    assert!(said.contains("already asked for"), "{said}");
}

#[test]
fn a_different_package_is_a_different_decision() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("distinct");
    f.ask("web-main", &["apt-get", "install", "ripgrep"]);
    f.ask("web-main", &["apt-get", "install", "jq"]);
    f.ask("web-main", &["npm", "i", "-g", "prettier"]);
    let q = f.queued();
    assert_eq!(q.len(), 3, "each distinct ask is its own decision: {q:?}");
    assert!(
        q.iter().any(|r| r["kind"] == "npm"),
        "npm asks are kept apart from apt: {q:?}"
    );
}

/// The cases that must never reach the queue.
///
/// Each of these is a way of getting an argument of one's choosing onto a command line that runs as
/// root once a human clicks Approve. Approving "install ripgrep" must not also be approving a flag.
#[test]
fn nothing_shaped_like_an_argument_can_be_filed_as_a_package() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("refused");
    for argv in [
        vec!["apt-get", "install", "foo; rm -rf /"],
        vec!["apt-get", "install", "../../etc/passwd"],
        vec!["apt-get", "install", "$(id)"],
        vec!["apt-get", "install", "a`id`b"],
        vec!["apt-get", "install", "foo|bar"],
        vec!["npm", "install", "-g", "x&&y"],
    ] {
        let (code, said) = f.ask("web-main", &argv);
        assert_eq!(code, 3, "should have been refused: {argv:?} said {said}");
        assert!(said.contains("not a package name"), "{said}");
    }
    assert!(f.queued().is_empty(), "nothing was filed: {:?}", f.queued());
}

#[test]
fn a_sudo_that_is_not_an_install_is_left_to_the_usual_explanation() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("passthrough");
    // Exit 2 is the shim's cue to print the "sudo cannot work in a box" message instead. These are
    // not installs, and turning them into package requests would be worse than refusing them.
    for argv in [
        vec!["systemctl", "restart", "nginx"],
        vec!["apt-get", "update"],
        vec!["apt-get", "remove", "tmux"],
        vec!["npm", "run", "build"],
        vec!["-u", "root", "chmod", "777", "/etc"],
    ] {
        let (code, _) = f.ask("web-main", &argv);
        assert_eq!(code, 2, "{argv:?} is not an install request");
    }
    assert!(f.queued().is_empty());
}

#[test]
fn sudos_own_options_are_not_mistaken_for_the_command() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("sudoflags");
    let (code, said) = f.ask("web-main", &["-E", "apt-get", "install", "ripgrep"]);
    assert_eq!(code, 0, "{said}");
    let q = f.queued();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0]["packages"], serde_json::json!(["ripgrep"]));
}

/// The shim a box actually gets, generated and then run.
///
/// Everything above drives `--request-package` directly, which proves the parsing and proves
/// nothing about whether a box can reach it. That gap is this repo's most-repeated bug — a thing
/// built correctly and then never wired up — and here it would be invisible: the shim falls back to
/// the old refusal when it cannot find the launcher, so a broken wire looks exactly like the
/// behaviour that existed before this feature. It was also real, not hypothetical: the launcher path
/// came from `$0`, which resolves to `bash` when the script is not started as the launcher.
#[test]
fn the_shim_a_box_gets_can_actually_reach_the_queue() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("wired");
    let launcher = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/box-session.sh");
    let src = fs::read_to_string(&launcher).unwrap();

    // The block that writes the shim, lifted from the launcher and run as a box start would run it.
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.starts_with("sudo_real=$(command -v sudo"))
        .expect("the sudo block moved");
    let to = lines
        .iter()
        .position(|l| l.contains(r#"binds+=(--ro-bind "$root/bin/sudo""#))
        .expect("the bind line moved");
    let block = format!(
        "{}\n  binds+=(--ro-bind \"$root/bin/sudo\" \"$sudo_real\")\nfi",
        lines[from..to].join("\n")
    );

    // The launcher has to be where the shim will look for it, which is the whole point.
    let installed = f.root.join(".skein/box-session.sh");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::copy(&launcher, &installed).unwrap();

    let box_root = f.root.join("boxroot");
    fs::create_dir_all(&box_root).unwrap();
    let made = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail; binds=(); root={}; box=web-main; {block}",
            box_root.display()
        ))
        .env("SKEIN_FLEET_ROOT", &f.root)
        .env_remove("SKEIN_BOX")
        .output()
        .expect("bash to run the sudo block");
    assert!(
        made.status.success(),
        "generating the shim failed: {}",
        String::from_utf8_lossy(&made.stderr)
    );

    let shim = box_root.join("bin/sudo");
    assert!(shim.is_file(), "no shim was written at all");

    // Run it with `sh`, which is what will execute it when an agent types `sudo`.
    let ran = Command::new("sh")
        .arg(&shim)
        .args(["apt-get", "install", "-y", "libnss3"])
        .env("SKEIN_FLEET_ROOT", &f.root)
        .env_remove("SKEIN_BOX")
        .output()
        .expect("sh to run the shim");
    let said = String::from_utf8_lossy(&ran.stderr).into_owned();

    assert_eq!(
        ran.status.code(),
        Some(1),
        "the command must still fail — nothing has been installed: {said}"
    );
    assert!(
        said.contains("pending approval"),
        "the shim did not reach the launcher: {said}"
    );
    let q = f.queued();
    assert_eq!(
        q.len(),
        1,
        "the shim reached the launcher but nothing was filed: {q:?}"
    );
    assert_eq!(q[0]["packages"], serde_json::json!(["libnss3"]));
    assert_eq!(
        q[0]["box"], "web-main",
        "the box's name is baked into its own shim"
    );

    // And the thing it replaced still happens for anything that is not an install.
    let other = Command::new("sh")
        .arg(&shim)
        .args(["systemctl", "restart", "nginx"])
        .env("SKEIN_FLEET_ROOT", &f.root)
        .env_remove("SKEIN_BOX")
        .output()
        .expect("sh to run the shim");
    let text = String::from_utf8_lossy(&other.stderr);
    assert!(
        text.contains("does not work inside a box"),
        "a non-install lost its explanation: {text}"
    );
    assert_eq!(f.queued().len(), 1, "a non-install must not file anything");
}

/// A request must be readable by the host exactly as `substrate.rs` expects it.
///
/// The two sides are written in different languages against the same file, which is the classic
/// place for a field to be renamed on one side only. This asserts they still agree.
#[test]
fn what_a_box_writes_is_what_the_host_reads() {
    if !have("jq") {
        return;
    }
    let f = Fleet::new("roundtrip");
    f.ask("web-main", &["apt-get", "install", "libnss3"]);
    let raw = serde_json::to_string(&f.queued()).unwrap();

    let parsed = skein::substrate::parse_requests(&raw);
    assert_eq!(
        parsed.len(),
        1,
        "the host could not read what the box wrote"
    );
    let r = &parsed[0];
    assert_eq!(
        r.box_name, "web-main",
        "the `box` field did not survive the crossing"
    );
    assert_eq!(r.packages, vec!["libnss3".to_string()]);
    assert!(r.is_pending());
    assert!(r.remember);
    assert!(
        r.problem().is_none(),
        "a request the box filed must be installable: {:?}",
        r.problem()
    );
}

/// **The live defect**: a queue a box cannot write said a request had been filed.
///
/// `/boxes/.skein` is `--ro-bind` in every unprivileged box, and the substrate queue lives under it.
/// So the write fails with EROFS, `request_package` returns 4 — and the shim's advice for "that was
/// not an install command" then told the agent to ask for the package **by running the install it
/// wanted**, which is the command that had just failed to file anything. An agent following that
/// advice loops for as long as it is willing to try, and the person who could actually approve it
/// never hears.
///
/// The existing tests in this file cannot catch it: they point the fleet root at a writable temp
/// directory with no bwrap namespace, which is exactly the condition under which the bug does not
/// happen. So this one makes the queue read-only, which is what a box really sees.
#[test]
fn a_queue_a_box_cannot_write_does_not_claim_a_request_was_filed() {
    if !have("jq") {
        eprintln!("SKIPPED: no jq, so the filing path cannot run at all");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let f = Fleet::new("readonly");
    // The bind a box actually gets: `.skein` exists and cannot be written into.
    let skein = f.root.join(".skein");
    fs::create_dir_all(&skein).unwrap();
    fs::set_permissions(&skein, fs::Permissions::from_mode(0o555)).unwrap();

    let (code, said) = f.ask("web-main", &["apt-get", "install", "ripgrep"]);
    // Put it back before any assertion, or a failure leaves an unremovable directory behind.
    let _ = fs::set_permissions(&skein, fs::Permissions::from_mode(0o755));

    assert_eq!(code, 4, "filing must report that it could not file: {said}");
    assert!(
        said.contains("could not be recorded"),
        "the failure must say what went wrong: {said}"
    );
    assert!(
        f.queued().is_empty(),
        "a request was written into a queue that is supposed to be unwritable"
    );

    // And the shim's own text, which is where the defect was visible. It must not send anybody
    // back round the loop by telling them to run the install again.
    let shim =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/box-session.sh"))
            .unwrap();
    let advice = shim
        .split_once("WHYNOFILE")
        .expect("the could-not-file advice is gone")
        .1;
    let advice = advice.split_once("WHYNOFILE").expect("unterminated").0;
    assert!(
        advice.contains("could NOT file") && advice.contains("will not file one either"),
        "the advice for a failed filing must say it failed: {advice}"
    );
    assert!(
        !advice.contains("Ask for it by running the install you wanted"),
        "the advice for a failed filing tells the agent to run the command that just failed"
    );
}
