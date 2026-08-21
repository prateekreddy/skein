//! Which GitHub credential a box actually gets, run through the real scripts.
//!
//! Every box in this fleet used to hold one user token — `repo`, `admin:public_key`, `gist` — that
//! reached 460 repositories read and write, plus a forwarded ssh-agent signing for any of them. What
//! replaces it is a per-repository write token, a read-only one for the repos the App is installed
//! on, and nothing at all for anything else — chosen between by `git-credential-skein`.
//!
//! Two shell scripts carry that, and both sit on paths every box takes constantly, so both are
//! driven here as the real thing drives them rather than as a Rust re-implementation of what they
//! are believed to do: the helper over git's own protocol on stdin, and the `git` shim generated
//! from the launcher and executed.
//!
//! The cases that matter most are the refusals and the pass-throughs. A helper handing the write
//! token to the wrong repository has quietly rebuilt the blast radius all of this exists to remove;
//! a shim that changes what `git` does for anything but a push has broken every box in the fleet.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(name)
}

/// A throwaway token directory, so the real box state is never touched.
struct Box_ {
    tokens: PathBuf,
    fleet: PathBuf,
}

impl Box_ {
    fn new(what: &str) -> Box_ {
        let root = PathBuf::from("/var/tmp")
            .join(format!("skein-gitgate-it-{what}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let tokens = root.join("tokens");
        let fleet = root.join("fleet");
        fs::create_dir_all(&tokens).unwrap();
        fs::create_dir_all(&fleet).unwrap();
        Box_ { tokens, fleet }
    }

    /// Place the write token the host would have minted for `slug`.
    fn place_token(&self, slug: &str, token: &str) {
        fs::write(self.tokens.join(slug.replace('/', "%2F")), token).unwrap();
    }

    /// Place the read-only token the host mints for one App installation owner.
    fn place_read(&self, owner: &str, token: &str) {
        let dir = self.tokens.join("read");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(owner), token).unwrap();
    }

    /// Ask the helper for a credential exactly as git does.
    fn credential(&self, host: &str, path: &str) -> String {
        use std::io::Write;
        let mut child = Command::new("sh")
            .arg(script("git-credential-skein.sh"))
            .arg("get")
            .env("SKEIN_GIT_TOKENS", &self.tokens)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the helper to run");
        write!(
            child.stdin.take().unwrap(),
            "protocol=https\nhost={host}\npath={path}\n\n"
        )
        .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run `box-session.sh --request-write`, returning (exit code, output).
    fn ask(&self, box_name: &str, argv: &[&str]) -> (i32, String) {
        let out = Command::new("bash")
            .arg(script("box-session.sh"))
            .arg("--request-write")
            .arg(box_name)
            .args(argv)
            .env("SKEIN_FLEET_ROOT", &self.fleet)
            .output()
            .expect("bash to run the launcher");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    fn queued(&self) -> Vec<serde_json::Value> {
        let dir = self.fleet.join(".skein/gitgate/requests");
        let Ok(entries) = fs::read_dir(&dir) else {
            return vec![];
        };
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect()
    }
}

#[test]
fn the_write_token_reaches_exactly_the_repository_it_was_minted_for() {
    let b = Box_::new("scoped");
    b.place_token("acme/thing", "APP-TOKEN-FOR-THING");

    let own = b.credential("github.com", "acme/thing.git");
    assert!(
        own.contains("password=APP-TOKEN-FOR-THING"),
        "the box's own repo must get its write token: {own}"
    );

    // The whole point. A helper that leaked this to a second repository would have rebuilt the
    // 460-repo reach one token at a time.
    let other = b.credential("github.com", "someone-else/private.git");
    assert!(
        !other.contains("APP-TOKEN-FOR-THING"),
        "the write token escaped to a repo it was not minted for: {other}"
    );
}

#[test]
fn a_repo_with_no_token_gets_nothing_rather_than_a_token_that_cannot_work() {
    // Silence is the correct answer, and it is load-bearing. Answering with some other repo's token
    // would turn a clone that would have succeeded ANONYMOUSLY — every public repo on GitHub — into
    // a 403. With no answer, git falls through to unauthenticated access: public works, private and
    // not-yours does not, which is exactly the intended shape.
    let b = Box_::new("readonly");
    b.place_token("acme/thing", "APP-TOKEN-FOR-THING");
    assert_eq!(
        b.credential("github.com", "some/public-repo.git"),
        "",
        "a repo with no token must get no credential at all"
    );
}

#[test]
fn a_read_token_covers_its_owner_and_stops_there() {
    // The App's installation list is the control: one read token per owner, so an org the App is
    // not installed on is not readable through this box, and nothing forges a link between them.
    let b = Box_::new("readowner");
    b.place_read("acme", "READ-ACME");

    let mine = b.credential("github.com", "acme/anything.git");
    assert!(mine.contains("password=READ-ACME"), "{mine}");
    assert_eq!(
        b.credential("github.com", "other-org/thing.git"),
        "",
        "an owner with no installation must not borrow another owner's read token"
    );
}

#[test]
fn write_beats_read_for_the_one_repo_a_box_owns() {
    // Both files exist for a box whose own repo is under an installed owner. Handing over the
    // read-only one would make every push fail with a credential that looked perfectly valid.
    let b = Box_::new("precedence");
    b.place_read("acme", "READ-ACME");
    b.place_token("acme/thing", "WRITE-THING");

    let own = b.credential("github.com", "acme/thing.git");
    assert!(own.contains("password=WRITE-THING"), "{own}");
    // And a sibling repo of the same owner still gets read, not the write token.
    let sibling = b.credential("github.com", "acme/other.git");
    assert!(sibling.contains("password=READ-ACME"), "{sibling}");
}

#[test]
fn nothing_is_answered_when_the_host_has_placed_nothing() {
    // A box in its first seconds, before the refresher has run. It must read anonymously rather
    // than be handed anything the sandbox happens to be holding.
    let b = Box_::new("empty");
    assert_eq!(b.credential("github.com", "any/repo.git"), "");
}

#[test]
fn a_repository_name_shaped_like_a_path_gets_no_credential() {
    // The path git sends becomes a filename. `..` in it must not address a token outside the box's
    // own directory — and the helper answers with nothing rather than with the wrong token.
    let b = Box_::new("traversal");
    b.place_token("acme/thing", "APP-TOKEN-FOR-THING");
    for path in ["../../etc/passwd", "a/../../b.git", "-flag/x.git"] {
        let got = b.credential("github.com", path);
        assert!(
            !got.contains("APP-TOKEN"),
            "{path:?} reached a token it must not: {got}"
        );
    }
}

#[test]
fn a_host_that_is_not_github_is_left_entirely_alone() {
    // git chains helpers. Answering for a host these credentials mean nothing to would replace
    // whatever the box legitimately has for it.
    let b = Box_::new("otherhost");
    assert_eq!(b.credential("gitlab.com", "a/b.git"), "");
    assert_eq!(b.credential("git.internal", "a/b.git"), "");
}

#[test]
fn asking_to_write_another_repo_files_one_request_however_often_it_is_asked() {
    if !have("jq") {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let b = Box_::new("ask");
    let (code, out) = b.ask(
        "web-main",
        &["acme/thing", "fix", "the", "shared", "type"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("pending approval"), "{out}");

    let queued = b.queued();
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(queued[0]["box"], "web-main");
    assert_eq!(queued[0]["repo"], "acme/thing");
    assert_eq!(queued[0]["state"], "pending");
    assert_eq!(
        queued[0]["reason"], "fix the shared type",
        "the reason is what the approver reads: {queued:?}"
    );

    // A stuck agent retrying a push must not grow the queue by one decision per attempt.
    let (code, out) = b.ask("web-main", &["acme/thing", "again"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("already asked"), "{out}");
    assert_eq!(b.queued().len(), 1, "a second ask filed a second request");
}

#[test]
fn a_repository_name_that_could_address_something_else_never_reaches_the_queue() {
    if !have("jq") {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let b = Box_::new("badname");
    for bad in [
        "../../.skein/fleet-agent.token",
        "a/b/c",
        "-flag/x",
        "nameonly",
    ] {
        let (code, out) = b.ask("web-main", &[bad]);
        assert_eq!(code, 3, "{bad:?} was not refused: {out}");
    }
    assert!(b.queued().is_empty(), "{:?}", b.queued());
}

/// Build the `git` shim exactly as a box start builds it, and return its path.
///
/// Lifted from the launcher rather than reimplemented, for the same reason the sudo shim's test
/// does it: a shim generated into a box's private namespace has nowhere else to be tested from, and
/// this one sits on the path every git command in every box takes.
fn build_git_shim(fleet: &std::path::Path, box_root: &std::path::Path, real_git: &str) -> PathBuf {
    let launcher = script("box-session.sh");
    let src = fs::read_to_string(&launcher).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.starts_with("  git_real=$(command -v git"))
        .expect("the git shim block moved");
    let to = lines
        .iter()
        .position(|l| l.contains(r#"binds+=(--ro-bind "$root/bin/git" "$git_real")"#))
        .expect("the bind line moved");
    // `to` is the last line of the block, so the slice stops just short of it — and short of the
    // `fi` that closes the whole thing. Both are restored, exactly as the sudo shim's test does.
    let block = format!(
        "{}\n{}\nfi",
        lines[from..to].join("\n"),
        r#"    binds+=(--ro-bind "$root/bin/git" "$git_real")"#
    );

    let installed = fleet.join(".skein/box-session.sh");
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::copy(&launcher, &installed).unwrap();
    fs::create_dir_all(box_root).unwrap();

    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail; binds=(); root={}; box=web-main; skein_launcher={}; {block}",
            box_root.display(),
            installed.display(),
        ))
        .env("SKEIN_FLEET_ROOT", fleet)
        .output()
        .expect("bash to run the git shim block");
    assert!(
        out.status.success(),
        "the shim block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The bind is what puts the real git behind the shim; outside bwrap the file is placed by hand.
    fs::copy(real_git, box_root.join("bin/git.real")).unwrap();
    let mut perms = fs::metadata(box_root.join("bin/git.real"))
        .unwrap()
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    fs::set_permissions(box_root.join("bin/git.real"), perms).unwrap();
    box_root.join("bin/git")
}

fn real_git() -> Option<String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg("command -v git")
        .output()
        .ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!p.is_empty()).then_some(p)
}

#[test]
fn the_git_shim_is_git_for_everything_that_is_not_a_push() {
    // The property that makes shimming git tolerable at all. `git` runs on every path in every box,
    // so anything but a push has to reach the real binary unchanged — same output, same exit code,
    // nothing extra on stdout for a script to trip over.
    let Some(git) = real_git() else { return };
    let b = Box_::new("shim-passthrough");
    let shim = build_git_shim(&b.fleet, &b.fleet.join("boxroot"), &git);

    for args in [
        vec!["--version"],
        vec!["rev-parse", "--is-inside-work-tree"],
    ] {
        let via_shim = Command::new(&shim)
            .args(&args)
            .env("SKEIN_GIT_TOKENS", &b.tokens)
            .env("SKEIN_FLEET_ROOT", &b.fleet)
            .output()
            .unwrap();
        let direct = Command::new(&git).args(&args).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&via_shim.stdout),
            String::from_utf8_lossy(&direct.stdout),
            "the shim changed what `git {}` prints",
            args.join(" ")
        );
        assert_eq!(
            via_shim.status.code(),
            direct.status.code(),
            "the shim changed the exit code of `git {}`",
            args.join(" ")
        );
    }
}

#[test]
fn a_push_to_a_repo_this_box_cannot_write_files_the_ask_and_still_runs() {
    if !have("jq") {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let Some(git) = real_git() else { return };
    let b = Box_::new("shim-push");
    let root = b.fleet.join("boxroot");
    let shim = build_git_shim(&b.fleet, &root, &git);

    // A real repo with a GitHub remote it holds no token for.
    let work = b.fleet.join("work");
    fs::create_dir_all(&work).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "git@github.com:someone-else/private.git",
        ],
    ] {
        Command::new(&git)
            .args(&args)
            .current_dir(&work)
            .output()
            .unwrap();
    }

    let out = Command::new(&shim)
        .args(["push", "origin", "HEAD"])
        .current_dir(&work)
        .env("SKEIN_GIT_TOKENS", &b.tokens)
        .env("SKEIN_FLEET_ROOT", &b.fleet)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);

    let queued = b.queued();
    assert_eq!(queued.len(), 1, "the push filed no request: {said}");
    assert_eq!(queued[0]["repo"], "someone-else/private");
    assert_eq!(queued[0]["box"], "web-main");
    assert!(
        said.contains("pending approval"),
        "the agent was not told where the ask went: {said}"
    );
    assert!(
        said.contains("will not change it"),
        "the agent was not told retrying is pointless, which is the whole point: {said}"
    );
    // And it still ran the real git — the shim adds a message, it never blocks.
    assert!(
        !out.status.success(),
        "the push must still be attempted and still fail, not be swallowed by the shim"
    );
}

#[test]
fn a_push_to_the_repo_this_box_owns_says_nothing_at_all() {
    if !have("jq") {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let Some(git) = real_git() else { return };
    let b = Box_::new("shim-own");
    let root = b.fleet.join("boxroot");
    let shim = build_git_shim(&b.fleet, &root, &git);
    b.place_token("acme/thing", "WRITE-THING");

    let work = b.fleet.join("work");
    fs::create_dir_all(&work).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/acme/thing.git",
        ],
    ] {
        Command::new(&git)
            .args(&args)
            .current_dir(&work)
            .output()
            .unwrap();
    }

    let out = Command::new(&shim)
        .args(["push", "origin", "HEAD"])
        .current_dir(&work)
        .env("SKEIN_GIT_TOKENS", &b.tokens)
        .env("SKEIN_FLEET_ROOT", &b.fleet)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        !said.contains("pending approval") && !said.contains("skein:"),
        "a box pushing its own repo must hear nothing from the shim: {said}"
    );
    assert!(b.queued().is_empty(), "it filed a request for its own repo");
}

#[test]
fn a_push_to_a_remote_that_is_not_github_is_left_alone() {
    if !have("jq") {
        eprintln!("skipping: jq is not installed");
        return;
    }
    let Some(git) = real_git() else { return };
    let b = Box_::new("shim-other-host");
    let root = b.fleet.join("boxroot");
    let shim = build_git_shim(&b.fleet, &root, &git);

    let work = b.fleet.join("work");
    fs::create_dir_all(&work).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "git@gitlab.com:a/b.git"],
    ] {
        Command::new(&git)
            .args(&args)
            .current_dir(&work)
            .output()
            .unwrap();
    }

    let out = Command::new(&shim)
        .args(["push", "origin", "HEAD"])
        .current_dir(&work)
        .env("SKEIN_GIT_TOKENS", &b.tokens)
        .env("SKEIN_FLEET_ROOT", &b.fleet)
        .output()
        .unwrap();
    assert!(
        b.queued().is_empty(),
        "a non-GitHub remote is not this gate's business: {:?}",
        b.queued()
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("skein:"));
}

#[test]
fn an_unscoped_box_gets_a_shim_that_does_nothing() {
    // With no token directory the box is unscoped, and the shim must be indistinguishable from git —
    // including for a push, which is the one verb it has an opinion about.
    let Some(git) = real_git() else { return };
    let b = Box_::new("shim-unscoped");
    let shim = build_git_shim(&b.fleet, &b.fleet.join("boxroot"), &git);

    let out = Command::new(&shim)
        .args(["push", "origin", "HEAD"])
        .env_remove("SKEIN_GIT_TOKENS")
        .env("SKEIN_FLEET_ROOT", &b.fleet)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("skein:"),
        "an unscoped box heard from the gate"
    );
    assert!(b.queued().is_empty());
}

#[test]
fn a_missing_argument_says_so_instead_of_aborting_the_shell_it_ran_in() {
    // `box-session.sh` runs under `set -u`, so an unguarded `$2` would abort with a bash error about
    // an unbound variable — from a command an agent typed, in the middle of its own work.
    let b = Box_::new("usage");
    let (code, out) = b.ask("web-main", &[]);
    assert_eq!(code, 4, "{out}");
    assert!(out.contains("usage:"), "{out}");
    assert!(
        !out.contains("unbound variable"),
        "the shell aborted instead of explaining: {out}"
    );
}

/// The shortest path out of a box was `cat`, not an exploit.
///
/// The fleet agent runs `/exec` at fleet scope — outside every box's namespace, where `sudo` works,
/// because that is how a package gets installed for the whole fleet. Its token sat at a fixed path
/// in the fleet root at mode 0600, owned by uid 1000 — which every box also is. So any box could
/// read it, POST a script, and be root in the sandbox: every other box's git tokens, every other
/// box's conversation history, and the credential helper itself.
///
/// Nothing inside a box ever called that agent; only the host does. So the file is covered by an
/// empty one, exactly as the forwarded ssh-agent socket is, and for the same reason — the path is
/// well known, so removing a variable would stop nobody.
///
/// Driven through the launcher's own lines rather than a re-implementation: this is a bind list
/// assembled in shell, and the only thing worth asserting is what that shell actually produces.
#[test]
fn a_box_cannot_read_the_fleet_agents_token() {
    let dir = std::env::temp_dir().join(format!("skein-fleettok-{}", std::process::id()));
    let fleet = dir.join("fleet");
    let box_root = dir.join("boxroot");
    fs::create_dir_all(fleet.join(".skein")).unwrap();
    fs::create_dir_all(&box_root).unwrap();
    let token = fleet.join(".skein/fleet-agent.token");
    fs::write(&token, "s3cret-fleet-token").unwrap();

    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.starts_with(r#": >"$root/no-fleet-token""#))
        .expect("the fleet-token block moved");
    let to = lines
        .iter()
        .position(|l| l.starts_with("unset fleet_token"))
        .expect("the fleet-token block moved");
    let block = lines[from..=to].join("\n");

    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail; binds=(); root={}; {block}; printf '%s\\n' \"${{binds[@]}}\"",
            box_root.display()
        ))
        .env("SKEIN_FLEET_ROOT", &fleet)
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(
        printed.contains("--ro-bind"),
        "the launcher produced no bind at all: {printed}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        printed.contains(token.to_string_lossy().as_ref()),
        "the bind must land on the token's own path, or it covers nothing: {printed}"
    );
    let cover = box_root.join("no-fleet-token");
    assert!(cover.exists(), "nothing was created to cover it with");
    assert_eq!(
        fs::read_to_string(&cover).unwrap(),
        "",
        "the cover must be empty — a copy of the token would be the same leak by another name"
    );
    // The host's own copy is what the agent authenticates against, and it is untouched: the cover
    // exists only inside a box's namespace.
    assert_eq!(fs::read_to_string(&token).unwrap(), "s3cret-fleet-token");

    let _ = fs::remove_dir_all(&dir);
}

/// Build the isolation block from the launcher and return the bind list it produces.
fn isolation_binds(
    fleet: &std::path::Path,
    state_parent: &std::path::Path,
    privileged: bool,
) -> String {
    isolation_binds_with(fleet, state_parent, privileged, "", "")
}

/// The same, told what the sandbox mounts and which two paths this box owns of it.
fn isolation_binds_with(
    fleet: &std::path::Path,
    state_parent: &std::path::Path,
    privileged: bool,
    mounts: &str,
    store: &str,
) -> String {
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
    let block = lines[from..=to].join("\n");

    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -uo pipefail; binds=(); root={}; state={}; export SKEIN_FLEET_ROOT={} SKEIN_BOX_PRIVILEGED={} \
             SKEIN_FLEET_MOUNTS={} SKEIN_BOX_STORE={}; \
             {block}; printf '%s\\n' \"${{binds[@]-}}\"",
            fleet.join("web-main").display(),
            state_parent.join("web-main").display(),
            fleet.display(),
            if privileged { "1" } else { "0" },
            skein::util::sh_quote(mounts),
            skein::util::sh_quote(store),
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One box cannot see another's files.
///
/// Measured before this existed, from inside a box on this fleet: another box's `claude-projects`
/// was readable, because every box is uid 1000 and `--dev-bind / /` shows it the whole sandbox.
///
/// A tmpfs over the two directories that hold every box, and this box's own bound back through it —
/// which works only because bwrap resolves sources against the original filesystem, the same
/// property `--bind "$home" "$HOME"` already relies on. Covering the *parents* rather than listing
/// siblings is what also covers boxes created after this one starts.
#[test]
fn a_box_sees_its_own_directories_and_no_other_boxs() {
    let dir = std::env::temp_dir().join(format!("skein-iso-{}", std::process::id()));
    let fleet = dir.join("boxes");
    let states = dir.join("state");
    for p in [
        fleet.join(".skein"),
        fleet.join("web-main"),
        fleet.join("other-box"),
        states.join("web-main"),
        states.join("other-box"),
    ] {
        fs::create_dir_all(&p).unwrap();
    }

    let binds = isolation_binds(&fleet, &states, false);
    let has = |a: &str, b: &str| {
        binds
            .lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w[0] == a && w[1] == b)
    };

    assert!(
        has("--tmpfs", fleet.to_string_lossy().as_ref()),
        "the directory holding every box must be covered: {binds}"
    );
    assert!(
        has("--tmpfs", states.to_string_lossy().as_ref()),
        "and so must the one holding every box's host state: {binds}"
    );
    assert!(
        has("--bind", fleet.join("web-main").to_string_lossy().as_ref()),
        "the box must get its own root back: {binds}"
    );
    assert!(
        has("--bind", states.join("web-main").to_string_lossy().as_ref()),
        "and its own state: {binds}"
    );
    assert!(
        has("--ro-bind", fleet.join(".skein").to_string_lossy().as_ref()),
        "the fleet root holds the launcher and the credential helper: {binds}"
    );
    // The whole point. Naming a sibling anywhere in the list would mean it survived the tmpfs.
    assert!(
        !binds.contains("other-box"),
        "a sibling box was named in the bind list: {binds}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A box sees its own repo, and nothing the sandbox mounts for anyone else.
///
/// The other half of the cover, and the half no rule could be written for. `fleet_mounts()` also
/// binds in every repo's store and every repo's *work tree*, and for a repo added by path those are
/// wherever the person keeps their code — `/home/you/code/thing`, which no pattern over `~/.skein`
/// reaches. Until this, a box could read every other repo's memory and mailbox, and write every
/// repo's checkout on the host.
///
/// The last of those was the sharpest: skein runs `git -C <repo.source_tree>` on the HOST, so a box that
/// could write `.git/config` there had `core.fsmonitor` executed as the host user. Its own checkout
/// does not come back at all now, in any form: a box cloned from the checkout and read its
/// gitignored files, and it does neither — it clones from the repo's mirror, and skein copies those
/// files into the store on the host. Nothing left in a box has a use for the tree its user works in.
#[test]
fn a_box_sees_its_own_repo_and_no_one_elses() {
    let dir = std::env::temp_dir().join(format!("skein-iso-mounts-{}", std::process::id()));
    let fleet = dir.join("boxes");
    let states = dir.join("state");
    let repos = dir.join("skein-repos");
    let elsewhere = dir.join("home-code-thing"); // an adopted repo, at a path skein did not choose
    let mine_store = repos.join("web/store/.claude");
    let mine_work = repos.join("web/work");
    for p in [
        fleet.join(".skein"),
        fleet.join("web-main"),
        states.join("web-main"),
        mine_store.clone(),
        mine_work.clone(),
        repos.join("other/store/.claude"),
        repos.join("other/work"),
        elsewhere.clone(),
    ] {
        fs::create_dir_all(&p).unwrap();
    }

    let mounts = format!("{}\n{}\n", repos.display(), elsewhere.display());
    let binds = isolation_binds_with(
        &fleet,
        &states,
        false,
        &mounts,
        mine_store.to_string_lossy().as_ref(),
    );
    let has = |a: &str, b: &str| {
        binds
            .lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w[0] == a && w[1] == b)
    };

    assert!(
        has("--tmpfs", repos.to_string_lossy().as_ref()),
        "the directory holding every repo's store must be covered: {binds}"
    );
    assert!(
        has("--tmpfs", elsewhere.to_string_lossy().as_ref()),
        "and so must a repo mounted from wherever its owner keeps it: {binds}"
    );
    assert!(
        has("--bind", mine_store.to_string_lossy().as_ref()),
        "the box must get its own repo's store back, read-write: {binds}"
    );
    assert!(
        !binds.contains(mine_work.to_string_lossy().as_ref()),
        "the box can see the tree its user works in, in any form: {binds}"
    );
    assert!(
        !binds.contains("other"),
        "another repo was named in the bind list: {binds}"
    );

    // The property that makes this an inversion rather than a hide-list: a path skein starts
    // mounting later is covered by the same code, with nobody remembering to add it. A hide-list
    // would need a line per mount, and the failure of a missing line is silent exposure.
    let newly = dir.join("something-skein-mounts-next-year");
    fs::create_dir_all(&newly).unwrap();
    let binds = isolation_binds_with(
        &fleet,
        &states,
        false,
        &format!("{mounts}{}\n", newly.display()),
        mine_store.to_string_lossy().as_ref(),
    );
    assert!(
        has_pair(&binds, "--tmpfs", newly.to_string_lossy().as_ref()),
        "a mount nobody wrote a rule for was left exposed: {binds}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// The two directories the older cover already owns are never re-covered here.
///
/// A tmpfs lands in the argument list in order, so one written over `$fleet_root` or over the box
/// state parent AFTER their binds throws those binds away — and the box comes up with no root of
/// its own and no state, which is worse than the exposure the loop exists to close.
#[test]
fn covering_the_mounts_does_not_uncover_the_box() {
    let dir = std::env::temp_dir().join(format!("skein-iso-order-{}", std::process::id()));
    let fleet = dir.join("boxes");
    let states = dir.join("state");
    for p in [
        fleet.join(".skein"),
        fleet.join("web-main"),
        states.join("web-main"),
    ] {
        fs::create_dir_all(&p).unwrap();
    }

    // skein naming its own two directories in the mount set, plus an ancestor of one of them.
    let mounts = format!(
        "{}\n{}\n{}\n",
        fleet.display(),
        states.display(),
        dir.display()
    );
    let binds = isolation_binds_with(&fleet, &states, false, &mounts, "");

    let tmpfs_after_bind = |covered: &std::path::Path, bound: &std::path::Path| {
        let lines: Vec<&str> = binds.lines().collect();
        let bind_at = lines.iter().position(|l| *l == bound.to_string_lossy());
        lines.windows(2).enumerate().any(|(i, w)| {
            w[0] == "--tmpfs" && w[1] == covered.to_string_lossy() && bind_at.is_some_and(|b| i > b)
        })
    };
    assert!(
        !tmpfs_after_bind(&fleet, &fleet.join("web-main")),
        "the fleet root was re-covered after the box got its root back: {binds}"
    );
    assert!(
        !tmpfs_after_bind(&states, &states.join("web-main")),
        "the state parent was re-covered after the box got its state back: {binds}"
    );
    assert!(
        !has_pair(&binds, "--tmpfs", dir.to_string_lossy().as_ref()),
        "an ancestor of both was covered, which erases both binds: {binds}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `--tmpfs <path>` present as an adjacent pair in the bind list.
fn has_pair(binds: &str, a: &str, b: &str) -> bool {
    binds
        .lines()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w[0] == a && w[1] == b)
}

/// The workshop box opts out of both — it exists to debug skein, which means reading the fleet.
#[test]
fn the_workshop_box_keeps_the_fleet_in_view() {
    let dir = std::env::temp_dir().join(format!("skein-iso-priv-{}", std::process::id()));
    let fleet = dir.join("boxes");
    let states = dir.join("state");
    for p in [
        fleet.join(".skein"),
        fleet.join("web-main"),
        states.join("web-main"),
    ] {
        fs::create_dir_all(&p).unwrap();
    }

    let binds = isolation_binds(&fleet, &states, true);
    assert!(
        binds.trim().is_empty(),
        "a privileged box must get no isolation binds at all: {binds}"
    );

    let _ = fs::remove_dir_all(&dir);
}
