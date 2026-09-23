//! Which GitHub credential a box actually gets, run through the real scripts.
//!
//! Every box in this fleet used to hold one user token — `repo`, `admin:public_key`, `gist` — that
//! reached 460 repositories read and write, plus a forwarded ssh-agent signing for any of them. What
//! replaces it is a per-repository write token, a read-only one for the repos the App is installed
//! on, and nothing at all for anything else — chosen between by `git-credential-skein`.
//!
//! **Most assertions here are about the credential the helper hands over; one is about what a box
//! reaches.** The comments used to slide from the first to the second and then disown the slide:
//! "git falls through to unauthenticated access" was true of the DIRECT path but false through the
//! sandbox's credential-injecting proxy, which answers a request carrying no credential as the
//! account (SKEIN-548). That is now closed for the git path — the launcher puts the GitHub hosts in
//! `NO_PROXY` for a scoped box, so silence really does fall through to an unauthenticated DIRECT
//! request, and `a_scoped_box_routes_github_direct_and_a_fleet_box_does_not` asserts the routing.
//! The credential assertions are unaffected either way — silence is still the right answer, and the
//! wrong token is still a 403 where silence is not.
//!
//! Two shell scripts carry that, and both sit on paths every box takes constantly, so both are
//! driven here as the real thing drives them rather than as a Rust re-implementation of what they
//! are believed to do: the helper over git's own protocol on stdin, and the `git` shim generated
//! from the launcher and executed.
//!
//! The cases that matter most are the refusals and the pass-throughs. A helper handing the write
//! token to the wrong repository has quietly rebuilt the blast radius all of this exists to remove;
//! a shim that changes what `git` does for anything but a push has broken every box in the fleet.

mod common;

use common::{have, skip, Scratch};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(name)
}

/// A throwaway token directory, so the real box state is never touched.
///
/// The scratch directory is held rather than derived, because holding it is what removes it: every
/// one of the fifteen tests here copied `/usr/bin/git` into `bin/git.real` and left the tree behind
/// (1,082 directories, 1.6 GB in `/var/tmp` on the box this was found on). `Scratch` keeps it when
/// the test fails, which is when somebody wants to look inside. The copy itself is gone as well —
/// `build_git_shim` links the real git rather than copying it — so what is left behind now is a
/// tree of small files rather than gigabytes of them.
struct Box_ {
    root: Scratch,
    tokens: PathBuf,
    fleet: PathBuf,
}

impl Box_ {
    fn new(what: &str) -> Box_ {
        let root = Scratch::boxes(&format!("skein-gitgate-it-{what}"));
        let tokens = root.join("tokens");
        let fleet = root.join("fleet");
        fs::create_dir_all(&tokens).unwrap();
        fs::create_dir_all(&fleet).unwrap();
        Box_ {
            root,
            tokens,
            fleet,
        }
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

    /// Run `box-session.sh --request-write` the way the git shim runs it: the box's own name baked
    /// into argument 1, and **no `SKEIN_BOX` in the environment at all**.
    ///
    /// Removed rather than left alone, because this test process runs inside a box and would
    /// otherwise hand the launcher that box's name. Not *set* either, which is what it used to do:
    /// setting it to the same name that was passed made every assertion here blind to which of the
    /// two the launcher read — and which it reads was the property the doc comment claimed to pin.
    fn ask(&self, box_name: &str, argv: &[&str]) -> (i32, String) {
        self.ask_as(box_name, None, argv)
    }

    /// The same, with `SKEIN_BOX` set to a name of the caller's choosing — which is what a box can
    /// do to itself, and therefore the only interesting case.
    fn ask_as(&self, arg_box: &str, env_box: Option<&str>, argv: &[&str]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(script("box-session.sh"))
            .arg("--request-write")
            .arg(arg_box)
            .args(argv)
            .env("SKEIN_FLEET_ROOT", &self.fleet);
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
        self.fleet.join(".skein/gitgate/requests")
    }

    fn json_in(dir: &PathBuf) -> Vec<serde_json::Value> {
        let Ok(entries) = fs::read_dir(dir) else {
            return vec![];
        };
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect()
    }

    /// What one box filed, read from **that box's own directory** — which is the only thing that
    /// says it is that box's, and on this queue decides which box a write token would land in.
    fn filed_under(&self, box_name: &str) -> Vec<serde_json::Value> {
        Box_::json_in(&self.queue().join(box_name))
    }

    /// Every request in every box's drop-box. Deliberately **not** the queue root: a file written
    /// loose there belongs to no box, and a grant needs a box to be granted to.
    fn queued(&self) -> Vec<serde_json::Value> {
        let Ok(dirs) = fs::read_dir(self.queue()) else {
            return vec![];
        };
        dirs.flatten()
            .filter(|e| e.path().is_dir())
            .flat_map(|e| Box_::json_in(&e.path()))
            .collect()
    }

    /// Anything sitting loose in the queue root, which nothing may write any more.
    fn loose(&self) -> Vec<serde_json::Value> {
        Box_::json_in(&self.queue())
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
    // would turn a clone that would have succeeded without one — every public repo on GitHub — into
    // a 403. With no answer, git falls through to whatever the network answers a request carrying no
    // credential, which is a strictly wider set than this helper offers (SKEIN-548) — so what is
    // asserted below is the helper's silence, not a boundary that follows from it.
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
    // not installed on gets no token from this helper, and nothing forges a link between them.
    // (Gets no token, not "is not readable" — SKEIN-548.)
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
    // A box in its first seconds, before the refresher has run. It must be handed nothing rather
    // than anything the sandbox happens to be holding.
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
        return skip("jq is not installed");
    }
    let b = Box_::new("ask");
    let (code, out) = b.ask("web-main", &["acme/thing", "fix", "the", "shared", "type"]);
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
        return skip("jq is not installed");
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
    build_git_shim_named(fleet, box_root, real_git, Some(FLEET_FIXTURE))
}

/// The sandbox name a shim built here is told it lives in.
///
/// Deliberately NOT `skein-fleet`, which is `config::default_fleet_sandbox`: a fixture that equalled
/// the default would still pass if the name stopped being carried and something fell back to it, so
/// the assertion would be blind to exactly the regression it exists to catch.
const FLEET_FIXTURE: &str = "skein-fleet-probe";

/// The same, with the sandbox name the launcher was given — `None` for a box started by an OLDER
/// launcher, which carries no `SKEIN_FLEET_NAME` at all.
fn build_git_shim_named(
    fleet: &std::path::Path,
    box_root: &std::path::Path,
    real_git: &str,
    fleet_name: Option<&str>,
) -> PathBuf {
    build_git_shim_for(fleet, box_root, real_git, fleet_name, "web-main")
}

/// The same, for a box of the caller's naming — so two shims stacked one over the other can be
/// told apart by the name each files its ask under.
fn build_git_shim_for(
    fleet: &std::path::Path,
    box_root: &std::path::Path,
    real_git: &str,
    fleet_name: Option<&str>,
    box_name: &str,
) -> PathBuf {
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

    let mut builder = Command::new("bash");
    builder
        .arg("-c")
        .arg(format!(
            "set -uo pipefail; binds=(); root={}; box={box_name}; skein_launcher={}; {block}",
            box_root.display(),
            installed.display(),
        ))
        .env("SKEIN_FLEET_ROOT", fleet)
        // Pinned beside the fleet root because the two are coupled: a scope that pins one and says
        // nothing about the other reads the real `~/.skein` on whatever machine it runs on
        // (SKEIN-654). This child is a shell block that reads neither, so the value is only ever
        // this test's own scratch — written down rather than left to a default.
        .env("SKEIN_HOME", fleet.join("home"));
    // The environment is how `fleet::session_script` carries the sandbox name to the launcher, so
    // it is how a test has to hand one over too. Removed rather than left alone for the `None`
    // case: this test process runs inside a box and would otherwise leak the real fleet's name in.
    match fleet_name {
        Some(name) => builder.env("SKEIN_FLEET_NAME", name),
        None => builder.env_remove("SKEIN_FLEET_NAME"),
    };
    let out = builder.output().expect("bash to run the git shim block");
    assert!(
        out.status.success(),
        "the shim block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The bind is what puts the real git behind the shim; outside bwrap it is linked by hand.
    //
    // **A symlink and not a copy, and the reason is `ETXTBSY`** (SKEIN-584). Copying and then
    // executing raced: `fs::copy` closes its own handles before it returns, so the write handle
    // was never ours to hold on to — what fails the exec is that *another thread's* `Command`
    // forked in the window while the copy was open, and the child inherits the descriptor until
    // its own exec clears it. `cargo test` runs this file's twenty-three tests on eleven threads,
    // five of them building a shim, so the window is open constantly and a busy box widens it.
    // Measured with a standalone reproducer of this exact shape — copy, then exec, with eight
    // threads forking alongside — the copy fails 609 times in 900; the symlink, 0 in 900.
    //
    // It is also nearer to what a box really does: `src/box-session.sh` binds the real git over
    // this path read-only rather than copying it, so nothing here ever wanted a second inode.
    //
    // No `set_permissions` to follow, deliberately: `chmod` follows a symlink, so setting a mode
    // here would set it on the machine's own `/usr/bin/git`. The target is already executable,
    // which is the whole reason `command -v git` found it.
    let real = box_root.join("bin/git.real");
    let _ = fs::remove_file(&real);
    #[cfg(unix)]
    std::os::unix::fs::symlink(real_git, &real).unwrap();
    box_root.join("bin/git")
}

/// The machine's real git — **never a skein shim**, which is what `command -v git` finds when these
/// tests run inside a skein box (SKEIN-956).
///
/// A box binds its own shim over the git on its PATH, so `command -v git` there answers with a
/// script whose `skein_git=` line names the real binary. Taken at face value, every shim built
/// here wrapped the running box's shim: a push filed a second ask under the running box's name,
/// and this file's verdict depended on which box ran it and what launcher started that box — green
/// at the batch-10 gate, red on the same tree days later, with no commit between. The nesting is
/// still tested, deliberately and on every machine, by
/// `a_shim_wrapping_another_shim_files_one_request_for_one_push`.
fn real_git() -> Option<String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg("command -v git")
        .output()
        .ok()?;
    let mut p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Followed rather than refused, and bounded, because a box started inside a box stacks them.
    for _ in 0..8 {
        let Some(inner) = shim_target(std::path::Path::new(&p)) else {
            break;
        };
        p = inner;
    }
    (!p.is_empty()).then_some(p)
}

/// The binary a skein git shim execs, read from the `skein_git=` line the launcher bakes into it;
/// `None` when `path` is not one. A real git is a binary and has no such line.
fn shim_target(path: &std::path::Path) -> Option<String> {
    let text = fs::read(path).ok()?;
    let text = String::from_utf8(text).ok()?;
    if !text.starts_with("#!/bin/sh") {
        return None;
    }
    text.lines()
        .find_map(|l| l.strip_prefix("skein_git="))
        .map(|v| v.trim_matches('\'').to_string())
}

/// The whole `SKEIN_GIT_SCOPE` block, lifted from the launcher rather than copied — the same rule
/// [`build_git_shim`] follows, so a change to the launcher is a change to what this runs.
///
/// It runs from the opening `if [ "${SKEIN_GIT_SCOPE-repo}" != "fleet" ]` to the `fi` that closes
/// it, which is the blank line before the docker shim's comment. Balanced on its own.
fn scope_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.starts_with(r#"if [ "${SKEIN_GIT_SCOPE-repo}" != "fleet" ]; then"#))
        .expect("the git-scope block moved");
    let to = lines
        .iter()
        .position(|l| *l == "# The docker shim: whose container is this?")
        .expect("the docker shim marker moved");
    lines[from..to].join("\n")
}

/// **Making the per-repo boundary real is a routing choice, and this is where it is decided
/// (SKEIN-548).** A scoped box reaches GitHub DIRECT — the launcher adds the injected GitHub hosts
/// to `NO_PROXY`, so git and gh present the box's own token instead of the proxy answering as the
/// account — while a `fleet`-mode box keeps the proxy and the account-wide token on purpose.
///
/// Counterfactual that makes this fail: if the `NO_PROXY` export moved outside the `if`, the fleet
/// case would carry `github.com`; if the host list dropped `github.com`/`api.github.com`, the scoped
/// case would not. Both were watched to fail before this was trusted (see the test's own proof run).
#[test]
fn a_scoped_box_routes_github_direct_and_a_fleet_box_does_not() {
    let block = scope_block();
    let dir = Scratch::temp("ghdirect");
    let root = dir.path().join("root");
    let home = dir.path().join("home");
    let state = dir.path().join("state");
    for d in [&root, &home, &state] {
        fs::create_dir_all(d).unwrap();
    }
    let run = |scope: Option<&str>| -> String {
        let prelude = format!(
            "set -uo pipefail; binds=(); root={root}; home={home}; state={state}; box=web-main; \
             export SKEIN_FLEET_ROOT={fleet}; unset SKEIN_BOX_REPO SSH_AUTH_SOCK; \
             no_proxy='localhost,127.0.0.1'; NO_PROXY='localhost,127.0.0.1';",
            root = root.display(),
            home = home.display(),
            state = state.display(),
            fleet = dir.path().display(),
        );
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(format!(
            "{prelude}\n{block}\necho \"RESULT=$NO_PROXY|$no_proxy\""
        ));
        match scope {
            Some(v) => {
                cmd.env("SKEIN_GIT_SCOPE", v);
            }
            None => {
                cmd.env_remove("SKEIN_GIT_SCOPE");
            }
        }
        let out = cmd.output().expect("the scope block to run");
        assert!(
            out.status.success(),
            "the scope block failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("RESULT=").map(str::to_string))
            .expect("the block to print its NO_PROXY")
    };

    // Scoped (the default, and any value that is not `fleet`): the GitHub hosts are added, direct.
    for scope in [None, Some("repo")] {
        let scoped = run(scope);
        for host in [
            "github.com",
            "api.github.com",
            "raw.githubusercontent.com",
            "gist.github.com",
            "copilot.github.com",
        ] {
            assert!(
                scoped.contains(host),
                "a scoped box ({scope:?}) must route {host} direct, but NO_PROXY was: {scoped}"
            );
        }
        // The pre-existing loopback entries survive, and both cases are exported.
        assert!(
            scoped.contains("localhost") && scoped.contains('|'),
            "the existing no_proxy and both spellings must be kept: {scoped}"
        );
    }

    // Fleet mode: the block is skipped entirely, so nothing GitHub is added — it keeps the proxy.
    let fleet = run(Some("fleet"));
    assert!(
        !fleet.contains("github.com"),
        "a fleet-mode box keeps the proxy, so NO_PROXY must NOT name github hosts: {fleet}"
    );
    assert_eq!(
        fleet, "localhost,127.0.0.1|localhost,127.0.0.1",
        "a fleet-mode box's NO_PROXY is exactly what it started with: {fleet}"
    );
}

/// The environment every `git` in this file runs under — the shim's own `git.real` included, since
/// it inherits whatever the shim was started with.
///
/// **`cargo test` may not reach a network or present a credential**, and until this existed the
/// three push tests below ran a real `git push` at `github.com` and `gitlab.com` with whatever ssh
/// key and credential helper the person running them happened to have. Two halves to closing that:
/// the push is pointed at a bare repo on disk (`Repo::new`), and the machine's own git
/// configuration is taken out of the picture here, so no `insteadOf`, no `credential.helper` and no
/// `user.email` from `~/.gitconfig` can change what these tests exercise or what they reach.
///
/// The prompt switches are the belt to that brace: if a push ever does escape to a host, git fails
/// with a message rather than opening `/dev/tty` and hanging a suite nobody is watching.
fn git_env(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("GIT_SSH_COMMAND", "false")
        .env("GIT_AUTHOR_NAME", "gitgate test")
        .env("GIT_AUTHOR_EMAIL", "gitgate@example.invalid")
        .env("GIT_COMMITTER_NAME", "gitgate test")
        .env("GIT_COMMITTER_EMAIL", "gitgate@example.invalid")
}

/// A repository to push from, with somewhere on this disk for the push to land.
///
/// `origin`'s **fetch** URL is whatever the case under test needs it to read as, and that is the one
/// the gate reads: `src/box-session.sh:1638` runs `git remote get-url "$remote"`, which answers the
/// fetch URL and applies no `pushurl`. `git push` prefers `remote.origin.pushurl`, so the transfer
/// itself goes into `bare` and no further. The gate decision and the push are thereby split — which
/// is what lets these tests keep asserting on a real push without one leaving the machine.
struct Repo {
    work: PathBuf,
    bare: PathBuf,
    /// The commit that ought to arrive in `bare`, which is how "the shim still ran git" is read.
    head: String,
}

impl Repo {
    fn new(b: &Box_, git: &str, url: &str) -> Repo {
        let work = b.root.join("work");
        let bare = b.root.join("remote.git");
        fs::create_dir_all(&work).unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let out = git_env(Command::new(git).args(args).current_dir(dir))
                .output()
                .expect("git to run");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run(&b.root, &["init", "-q", "--bare", "remote.git"]);
        run(&work, &["init", "-q"]);
        fs::write(work.join("a.txt"), "one line\n").unwrap();
        run(&work, &["add", "a.txt"]);
        run(&work, &["commit", "-qm", "something to push"]);
        run(&work, &["remote", "add", "origin", url]);
        run(
            &work,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                &bare.display().to_string(),
            ],
        );
        let head = run(&work, &["rev-parse", "HEAD"]);
        Repo { work, bare, head }
    }

    /// Every branch the bare remote actually received, as `<sha> <ref>` lines.
    fn received(&self, git: &str) -> String {
        let out = git_env(Command::new(git).args([
            "--git-dir",
            &self.bare.display().to_string(),
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads/",
        ]))
        .output()
        .expect("git to run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Did the push land? The property behind "the shim never blocks": it adds a message and then
    /// execs the real git, so the commit has to be in the remote afterwards.
    fn got_the_push(&self, git: &str) -> bool {
        self.received(git).contains(&self.head)
    }
}

/// Run the shim from inside `repo`, as a box's `git` would be run.
fn shim_push(shim: &std::path::Path, b: &Box_, repo: &Repo, scoped: bool) -> std::process::Output {
    let mut cmd = Command::new(shim);
    cmd.args(["push", "origin", "HEAD"])
        .current_dir(&repo.work)
        .env("SKEIN_FLEET_ROOT", &b.fleet)
        .env_remove("SKEIN_BOX");
    if scoped {
        cmd.env("SKEIN_GIT_TOKENS", &b.tokens);
    } else {
        cmd.env_remove("SKEIN_GIT_TOKENS");
    }
    git_env(&mut cmd).output().expect("the shim to run")
}

#[test]
fn the_git_shim_is_git_for_everything_that_is_not_a_push() {
    // The property that makes shimming git tolerable at all. `git` runs on every path in every box,
    // so anything but a push has to reach the real binary unchanged — same output, same exit code,
    // nothing extra on stdout for a script to trip over.
    let Some(git) = real_git() else {
        return skip("git is not installed, and the shim under test is a wrapper around it");
    };
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
            .env_remove("SKEIN_BOX")
            .output()
            .unwrap();
        let direct = Command::new(&git).args(&args).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&via_shim.stdout),
            String::from_utf8_lossy(&direct.stdout),
            // **The shim's own stderr, in the message, because this test fails intermittently and
            // six sightings produced no cause.** It compared stdout and the exit code and threw
            // away the one thing that says WHY — so every failure was "left: \"\"" against a real
            // `git --version`, which says the shim did not run and nothing about what stopped it.
            // It reproduces only when the whole `--tests` set runs at once, never alone and never
            // as its own suite (20/20), so whoever sees it next may not be able to summon it
            // again: the answer has to be in the failure itself.
            "the shim changed what `git {}` prints — the shim exited {:?} and said {:?}",
            args.join(" "),
            via_shim.status.code(),
            String::from_utf8_lossy(&via_shim.stderr)
        );
        assert_eq!(
            via_shim.status.code(),
            direct.status.code(),
            "the shim changed the exit code of `git {}`",
            args.join(" ")
        );
    }
}

/// **A scoped box that reaches GitHub direct gets the `sbx policy allow network` hint when the
/// connection is BLOCKED, and only then (SKEIN-548/926).** The shim runs the real git, and on a
/// failed GitHub-reaching verb it probes reachability: a probe that cannot connect is a block and
/// prints the hint; a probe that gets any HTTP status (a 401/403 auth answer) does not. The probe
/// target is `$SKEIN_GITHUB_REACH_URL`, so this drives the real shell without a network.
///
/// Counterfactual: point the probe at a live listener (a 401) and the hint must be absent; point it
/// at a dead port and it must be present. Proven by sabotage — forcing `skein_reach_hint` to always
/// or never print made one half fail.
#[test]
fn a_blocked_scoped_box_gets_the_policy_hint_and_an_auth_answer_does_not() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let Some(git) = real_git() else {
        return skip("git is not installed, and the shim under test is a wrapper around it");
    };
    if !have("curl") {
        return skip("curl is not installed, and the reachability probe is a wrapper around it");
    }
    let b = Box_::new("shim-blocked");
    let shim = build_git_shim(&b.fleet, &b.fleet.join("boxroot"), &git);

    // A GitHub-reaching verb that fails to connect, so the shim reaches its reachability probe. The
    // remote is a dead loopback port; git's own transport error is what makes it fail.
    let run = |reach_url: &str| -> String {
        let mut cmd = Command::new(&shim);
        cmd.args(["ls-remote", "https://127.0.0.1:1/nope.git"])
            .env("SKEIN_GIT_TOKENS", &b.tokens)
            .env("SKEIN_FLEET_ROOT", &b.fleet)
            // Coupled with the fleet root: pinned so this never reads the real `~/.skein`.
            .env("SKEIN_HOME", b.fleet.join("home"))
            .env("SKEIN_GITHUB_REACH_URL", reach_url)
            .env_remove("SKEIN_BOX");
        let out = git_env(&mut cmd).output().expect("the shim to run");
        assert!(
            !out.status.success(),
            "the ls-remote to a dead port should have failed, so the probe path is reached"
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    };

    // Blocked: the probe target is a dead port, so the connection cannot be made — hint printed,
    // naming THIS box's sandbox so the command can be pasted unedited.
    let blocked = run("http://127.0.0.1:1/");
    assert!(
        blocked.contains("GitHub is blocked by the sandbox's network policy")
            && blocked.contains(&format!(
                "sbx policy allow network --sandbox {FLEET_FIXTURE} github.com,api.github.com"
            )),
        "a blocked box must get the exact policy hint, with the real sandbox name substituted, \
         got: {blocked}"
    );
    // And never a half-written command: no placeholder left in, and no empty `--sandbox `.
    assert!(
        !blocked.contains("--sandbox <fleet>") && !blocked.contains("--sandbox  "),
        "the hint must not offer an unusable command: {blocked}"
    );

    // Reachable: a listener that answers 401 — GitHub answering, not a block — so no hint.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        // One 401 per probe; the probe may retry connect within its timeout, so answer a few.
        let _ = listener.set_nonblocking(false);
        for _ in 0..4 {
            match listener.accept() {
                Ok((mut sock, _)) => {
                    let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf);
                    let _ =
                        sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
                    let _ = sock.flush();
                }
                Err(_) => break,
            }
        }
    });
    let answered = run(&format!("http://127.0.0.1:{port}/"));
    drop(handle); // the thread ends with the listener when the test scope drops it
    assert!(
        !answered.contains("blocked by the sandbox's network policy"),
        "a 401 is GitHub answering, so no policy hint should be printed, got: {answered}"
    );
}

/// **A box started by an OLDER launcher carries no sandbox name, and must SAY so rather than print
/// half a command.** `SKEIN_FLEET_NAME` is new, so a launcher already installed in a running sandbox
/// does not set it — the case that actually happens in the field, on every box that has not been
/// restarted since this shipped.
///
/// The failure being prevented is specific: `--sandbox ` with nothing after it reads as a finished
/// command, gets pasted, and fails on the host for a reason that has nothing to do with the boundary
/// it was trying to fix. So the empty name takes a different sentence, not an empty substitution.
#[test]
fn a_box_never_told_its_sandbox_name_says_so_instead_of_writing_half_a_command() {
    let Some(git) = real_git() else {
        return skip("git is not installed, and the shim under test is a wrapper around it");
    };
    if !have("curl") {
        return skip("curl is not installed, and the reachability probe is a wrapper around it");
    }
    let b = Box_::new("shim-noname");
    // `None`: exactly what an older launcher hands over — no SKEIN_FLEET_NAME at all.
    let shim = build_git_shim_named(&b.fleet, &b.fleet.join("boxroot"), &git, None);

    let mut cmd = Command::new(&shim);
    cmd.args(["ls-remote", "https://127.0.0.1:1/nope.git"])
        .env("SKEIN_GIT_TOKENS", &b.tokens)
        .env("SKEIN_FLEET_ROOT", &b.fleet)
        // Coupled with the fleet root: pinned so this never reads the real `~/.skein`.
        .env("SKEIN_HOME", b.fleet.join("home"))
        .env("SKEIN_GITHUB_REACH_URL", "http://127.0.0.1:1/")
        .env_remove("SKEIN_BOX");
    let out = git_env(&mut cmd).output().expect("the shim to run");
    let said = String::from_utf8_lossy(&out.stderr);

    assert!(
        said.contains("never told its sandbox's name"),
        "a box with no sandbox name must say that plainly, got: {said}"
    );
    // The whole point: no unusable command. Not an empty `--sandbox `, and not a stale placeholder.
    assert!(
        !said.contains("--sandbox  ")
            && !said.contains("--sandbox <fleet>")
            && !said.contains("--sandbox g"),
        "a half-written or wrongly-filled command reached the person: {said}"
    );
}

#[test]
fn a_push_to_a_repo_this_box_cannot_write_files_the_ask_and_still_runs() {
    if !have("jq") {
        return skip("jq is not installed");
    }
    let Some(git) = real_git() else {
        return skip("git is not installed, and the shim under test is a wrapper around it");
    };
    let b = Box_::new("shim-push");
    let root = b.fleet.join("boxroot");
    let shim = build_git_shim(&b.fleet, &root, &git);

    // A real repo with a GitHub remote it holds no token for, pushing into a bare repo on this disk.
    let repo = Repo::new(&b, &git, "git@github.com:someone-else/private.git");

    let out = shim_push(&shim, &b, &repo, true);
    let said = String::from_utf8_lossy(&out.stderr);

    let queued = b.queued();
    assert_eq!(
        queued.len(),
        1,
        "one push must file exactly one request, and it filed {} — {queued:?}: {said}",
        queued.len()
    );
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
    //
    // Asserted as **the commit arriving in the remote**, not as a non-zero exit. The exit code was
    // what this checked while the remote was `github.com`: it read "the push failed", which a shim
    // that refused outright and never ran git satisfies just as well, and it was true only because
    // the network refused. What cannot be faked by a shim that swallows the push is the object
    // being in the other repository afterwards.
    assert!(
        repo.got_the_push(&git),
        "the shim swallowed the push instead of running it — the remote holds {:?}, not {}: {said}",
        repo.received(&git),
        repo.head
    );
}

/// **One push files one request when a shim wraps another shim** (SKEIN-956). A box finds its
/// "real" git with `command -v git`, and inside a box that is the box's own shim — so a box started
/// from a box, or skein's own tests run in one, put a shim in front of a shim. Each used to file an
/// ask under its own box name, and the person approving saw two cockpit rows for one push.
///
/// Built here on purpose, with two names, so it is caught on any machine: before this test the
/// nesting existed only by accident, when `cargo test` happened to run inside a scoped box.
#[test]
fn a_shim_wrapping_another_shim_files_one_request_for_one_push() {
    if !have("jq") {
        return skip("jq is not installed");
    }
    let Some(git) = real_git() else {
        return skip("git is not installed, and the shim under test is a wrapper around it");
    };
    let b = Box_::new("shim-nested");
    // The inner shim is what the outer box's `git.real` turns out to be: the enclosing box's git.
    let inner = build_git_shim_for(
        &b.fleet,
        &b.fleet.join("enclosing"),
        &git,
        Some(FLEET_FIXTURE),
        "web-enclosing",
    );
    let outer = build_git_shim(
        &b.fleet,
        &b.fleet.join("boxroot"),
        &inner.display().to_string(),
    );

    let repo = Repo::new(&b, &git, "git@github.com:someone-else/private.git");
    let out = shim_push(&outer, &b, &repo, true);
    let said = String::from_utf8_lossy(&out.stderr);

    let queued = b.queued();
    assert_eq!(
        queued.len(),
        1,
        "one push through two shims must file exactly one request, and it filed {} — {queued:?}: {said}",
        queued.len()
    );
    // The ask belongs to the box the agent is in — the outermost shim — not the one around it.
    assert_eq!(queued[0]["box"], "web-main", "{queued:?}");
    assert_eq!(
        said.matches("nothing here grants you").count(),
        1,
        "the agent was told twice: {said}"
    );
    assert!(
        repo.got_the_push(&git),
        "the stacked shims swallowed the push — the remote holds {:?}, not {}: {said}",
        repo.received(&git),
        repo.head
    );
}

#[test]
fn a_push_to_the_repo_this_box_owns_says_nothing_at_all() {
    if !have("jq") {
        return skip("jq is not installed");
    }
    let Some(git) = real_git() else {
        return skip("git is not installed, and the shim under test is a wrapper around it");
    };
    let b = Box_::new("shim-own");
    let root = b.fleet.join("boxroot");
    let shim = build_git_shim(&b.fleet, &root, &git);
    b.place_token("acme/thing", "WRITE-THING");

    let repo = Repo::new(&b, &git, "https://github.com/acme/thing.git");

    let out = shim_push(&shim, &b, &repo, true);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        !said.contains("pending approval") && !said.contains("skein:"),
        "a box pushing its own repo must hear nothing from the shim: {said}"
    );
    assert!(b.queued().is_empty(), "it filed a request for its own repo");
    // Silence proves nothing on its own — a shim that exited before running git would also say
    // nothing. The push has to have happened.
    assert!(
        repo.got_the_push(&git),
        "silent, but the push never ran: the remote holds {:?}, not {}",
        repo.received(&git),
        repo.head
    );
}

#[test]
fn a_push_to_a_remote_that_is_not_github_is_left_alone() {
    if !have("jq") {
        return skip("jq is not installed");
    }
    let Some(git) = real_git() else {
        return skip("git is not installed, and the shim under test is a wrapper around it");
    };
    let b = Box_::new("shim-other-host");
    let root = b.fleet.join("boxroot");
    let shim = build_git_shim(&b.fleet, &root, &git);

    let repo = Repo::new(&b, &git, "git@gitlab.com:a/b.git");

    let out = shim_push(&shim, &b, &repo, true);
    assert!(
        b.queued().is_empty(),
        "a non-GitHub remote is not this gate's business: {:?}",
        b.queued()
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("skein:"));
    assert!(
        repo.got_the_push(&git),
        "left alone means the push runs: the remote holds {:?}, not {}",
        repo.received(&git),
        repo.head
    );
}

#[test]
fn an_unscoped_box_gets_a_shim_that_does_nothing() {
    // With no token directory the box is unscoped, and the shim must be indistinguishable from git —
    // including for a push, which is the one verb it has an opinion about.
    //
    // This ran with no `current_dir`, which for an integration test is `CARGO_MANIFEST_DIR` — so
    // `git push origin HEAD` was a push of the checkout under test to skein's own `origin`. It has a
    // repository of its own now, like the three tests above.
    let Some(git) = real_git() else {
        return skip("git is not installed, and the shim under test is a wrapper around it");
    };
    let b = Box_::new("shim-unscoped");
    let shim = build_git_shim(&b.fleet, &b.fleet.join("boxroot"), &git);
    let repo = Repo::new(&b, &git, "git@github.com:someone-else/private.git");

    let out = shim_push(&shim, &b, &repo, false);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("skein:"),
        "an unscoped box heard from the gate"
    );
    assert!(b.queued().is_empty());
    assert!(
        repo.got_the_push(&git),
        "an unscoped shim must be git: the remote holds {:?}, not {}",
        repo.received(&git),
        repo.head
    );
}

/// **A write request is filed in its own box's directory**, and on this queue that decides more
/// than attribution.
///
/// `gitgate::decide` builds the grant from the request's box as well as its repo, and the refresher
/// writes the minted installation token into the box the grant names. Architecture §8.4 orders the
/// three steps for this — bind the artifact, make the request path per box, *then* unmask the queue
/// — and the middle one was skipped.
#[test]
fn a_write_request_is_filed_in_its_own_boxs_drop_box() {
    if !have("jq") {
        return skip("jq is not installed");
    }
    let b = Box_::new("perbox");
    let (code, said) = b.ask("web-main", &["someone-else/private", "why"]);
    assert_eq!(code, 0, "{said}");

    let mine = b.filed_under("web-main");
    assert_eq!(mine.len(), 1, "the request is in this box's own directory");
    assert_eq!(mine[0]["repo"], "someone-else/private");
    assert!(
        b.loose().is_empty(),
        "nothing may be written loose in the queue root, where it would belong to no box: {:?}",
        b.loose()
    );
}

/// **A box cannot ask for write access in another box's name**, and the reason is the mount.
///
/// This is the request whose approval hands out a credential. A box able to file under a
/// neighbour's name could have an owner approve what reads as that neighbour's ask and watch a live
/// GitHub write token be placed in it — a box it can then drive over the cross-box messaging the
/// fleet keeps deliberately.
///
/// The mount is stood in for by mode bits: `web-main`'s drop-box is writable, `api`'s is not, and
/// the queue root is left **writable** on purpose — with it read-only the old shared-directory code
/// would fail for the wrong reason and this would pass while proving nothing.
#[test]
fn a_box_cannot_ask_for_write_access_in_another_boxs_name() {
    if !have("jq") || running_as_root() {
        return skip("needs jq and a uid that mode bits apply to");
    }
    use std::os::unix::fs::PermissionsExt;
    let b = Box_::new("impersonate");
    fs::create_dir_all(b.queue().join("web-main")).unwrap();
    fs::create_dir_all(b.queue().join("api")).unwrap();
    fs::set_permissions(b.queue().join("api"), fs::Permissions::from_mode(0o555)).unwrap();

    // Both spellings of the lie: the argument the caller typed, and the environment variable the
    // box owns. Neither is trusted, so neither works.
    let typed = b.ask("api", &["someone-else/private", "why"]);
    let exported = b.ask_as("", Some("api"), &["someone-else/other", "why"]);
    let _ = fs::set_permissions(b.queue().join("api"), fs::Permissions::from_mode(0o755));

    assert!(
        b.filed_under("api").is_empty(),
        "a write request was filed in another box's name — a grant made on it would put that box's \
         token in the wrong box: {:?}",
        b.filed_under("api")
    );
    assert!(
        b.loose().is_empty(),
        "a request was written loose in the queue root: {:?}",
        b.loose()
    );
    for (which, (code, said)) in [("typed", typed), ("exported", exported)] {
        assert_eq!(code, 4, "the {which} name was not refused: {said}");
        assert!(said.contains("could not be recorded"), "{which}: {said}");
    }
}

/// The environment does not decide which box asked for write access, and it used to.
///
/// `request_write` preferred `$SKEIN_BOX`, pointing at `request_package`'s comment saying it
/// "cannot be argued with". It is an environment variable of a process the box owns. The git shim's
/// baked-in argument is the better hint — written at generation time, outside the namespace — and
/// the directory is what actually decides.
#[test]
fn the_environment_does_not_decide_which_box_asked_for_write_access() {
    if !have("jq") {
        return skip("jq is not installed");
    }
    let b = Box_::new("envbox");
    let (code, said) = b.ask_as("web-main", Some("api"), &["someone-else/private", "why"]);
    assert_eq!(code, 0, "{said}");
    assert_eq!(
        b.filed_under("web-main").len(),
        1,
        "the launcher's own argument names the box, not the box's environment"
    );
    assert!(
        b.filed_under("api").is_empty(),
        "$SKEIN_BOX chose which box would receive the token"
    );
}

/// A box name that is not a name is refused before it becomes a path.
#[test]
fn a_box_name_that_is_not_a_name_never_becomes_a_write_queue_directory() {
    if !have("jq") {
        return skip("jq is not installed");
    }
    let b = Box_::new("badbox");
    for bad in ["../../.skein", "a/b", "-flag", "a;touch /tmp/x", ".."] {
        let (code, said) = b.ask(bad, &["someone-else/private", "why"]);
        assert_eq!(code, 4, "{bad:?} was accepted as a box name: {said}");
        assert!(said.contains("is not a box name"), "{bad:?}: {said}");
    }
    assert!(b.queued().is_empty(), "{:?}", b.queued());
    assert!(b.loose().is_empty(), "{:?}", b.loose());
    assert!(
        !b.fleet.join(".skein/gitgate/.skein").exists(),
        "a box name climbed out of the queue"
    );
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

// **`a_box_cannot_read_the_fleet_agents_token` lived here and has moved**, to
// `tests/isolation_bwrap/private.rs::a_box_cannot_read_what_skein_keeps_under_private`.
//
// It drove the launcher's `no-fleet-token` block — an empty file bound over one name — and that
// mechanism is gone: `box-session.sh` now puts a single `--tmpfs` over `.skein/private/`, which is
// where the fleet agent's token and the review credential both live (ISO-2). The replacement
// asserts strictly more, and asserts it against a real namespace rather than against a bind list:
// that the token reads back `gone` from an ordinary box, that the directory is `empty` rather than
// missing (a tmpfs, not a deletion), that the rest of `.skein` is still `see` so the launcher and
// the toolchain survive the cover, and — the half a bind-list test cannot reach — that a
// privileged box reads the same file back as `see`, so the absence was first shown to be a
// presence.
//
// Nothing the old test asserted is unasserted now. Its "the cover file is empty" had no subject
// left; its "the fleet's own copy is untouched" is the workshop leg reading the file back after
// the ordinary box's run over the same fixture.

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
            // `box` as well as `root` and `state`: the drop-box loop names `requests/$box`, because
            // the directory a request lands in is what says which box filed it.
            "set -uo pipefail; binds=(); box=web-main; root={}; state={}; export SKEIN_FLEET_ROOT={} SKEIN_BOX_PRIVILEGED={} \
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
    let dir = Scratch::temp("skein-iso");
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
    // Its own state comes back READ-ONLY: what lives there is the conversation the box writes
    // through a separate read-write bind at `$HOME`, and the git token the HOST places and the box
    // only reads. A box that could write this path could write itself a token for a repository it
    // was never given.
    assert!(
        has(
            "--ro-bind",
            states.join("web-main").to_string_lossy().as_ref()
        ),
        "and its own state, read-only: {binds}"
    );
    assert!(
        !has("--bind", states.join("web-main").to_string_lossy().as_ref()),
        "the box's state came back writable: {binds}"
    );
    assert!(
        has("--ro-bind", fleet.join(".skein").to_string_lossy().as_ref()),
        "the fleet root holds the launcher and the credential helper: {binds}"
    );
    // The two drop-boxes: **this box's own directory under each queue, never the queue root.**
    // Architecture §8.4 orders it — bind the artifact, make the request path per box, then unmask —
    // and unmasking the root alone gave every box a writable path to every other box's pending
    // requests, and a way to file one in a neighbour's name. On the gitgate queue that is a live
    // GitHub token landing in a box of the requester's choosing.
    for queue in ["substrate", "gitgate"] {
        let root = fleet.join(format!(".skein/{queue}/requests"));
        assert!(
            has("--bind", root.join("web-main").to_string_lossy().as_ref()),
            "the {queue} queue must give this box a drop-box of its own: {binds}"
        );
        assert!(
            !has("--bind", root.to_string_lossy().as_ref()),
            "the whole {queue} queue was bound writable, so every box can write every other box's \
             requests: {binds}"
        );
    }
    // The whole point. Naming a sibling anywhere in the list would mean it survived the tmpfs.
    assert!(
        !binds.contains("other-box"),
        "a sibling box was named in the bind list: {binds}"
    );
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
    let dir = Scratch::temp("skein-iso-mounts");
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
}

/// The two directories the older cover already owns are never re-covered AFTER their binds — and
/// an ancestor of them is covered BEFORE (SKEIN-219).
///
/// A tmpfs lands in the argument list in order, so one written over `$fleet_root` or over the box
/// state parent after their binds throws those binds away — and the box comes up with no root of
/// its own and no state, which is worse than the exposure the loop exists to close. That is why an
/// ancestor mount used to be skipped altogether; skipping is not covering, and the volume a fleet
/// serves from is exactly such an ancestor, so every box could read `credentials/` off it. Order is
/// what makes both true at once: the ancestor first, the entitlements bound back through it (bwrap
/// resolves a bind source against the original filesystem). What a box can actually reach after
/// that is asserted against a real namespace in `tests/isolation_bwrap/`; this one is about the
/// argument list.
#[test]
fn covering_the_mounts_does_not_uncover_the_box() {
    let dir = Scratch::temp("skein-iso-order");
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
    let lines: Vec<&str> = binds.lines().collect();
    let ancestor_at = lines
        .windows(2)
        .position(|w| w[0] == "--tmpfs" && w[1] == dir.to_string_lossy())
        .expect("the mount containing both was not covered at all, so a box can read it");
    let first_bind = lines
        .iter()
        .position(|l| *l == fleet.join("web-main").to_string_lossy())
        .expect("the box never got its own root back");
    assert!(
        ancestor_at < first_bind,
        "the ancestor was covered after the binds it contains, which throws them away: {binds}"
    );
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
    let dir = Scratch::temp("skein-iso-priv");
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
}
