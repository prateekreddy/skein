//! Which GitHub credential a box actually gets, run through the real scripts.
//!
//! Every box in this fleet used to hold one user token — `repo`, `admin:public_key`, `gist` — that
//! reached 460 repositories read and write, plus a forwarded ssh-agent signing for any of them. What
//! replaces it is a read-only PAT for everything and a per-repository App token for the one repo a
//! box owns, chosen between by `git-credential-skein`.
//!
//! That helper is the load-bearing piece: it runs on every git operation in every box, it decides
//! which credential is handed over, and it is a shell script that git talks to over a pipe. So it is
//! driven here exactly as git drives it — the real file, the real protocol on stdin — rather than a
//! Rust re-implementation of what it is believed to do.
//!
//! The cases that matter most are the refusals. A helper that hands the write token to the wrong
//! repository has quietly rebuilt the blast radius all of this exists to remove.

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
