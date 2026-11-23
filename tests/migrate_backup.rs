//! `skein migrate send` and `receive` against fixture roots: the real binary, real git, and a stub
//! `docker`. Every path is pinned to the fixture, and each test names the change that fails it.

mod common;

use common::{have, skip, Scratch};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ANON: &str = "3f2a91b8c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1";
const SENTINEL: &str = "SENTINEL-MUST-NOT-TRAVEL";

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

struct Fixture {
    _scratch: Scratch,
    base: PathBuf,
    home: PathBuf,
    root: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn bundle(&self) -> PathBuf {
        self.base.join("out/skein.bundle")
    }

    fn target(&self) -> PathBuf {
        self.base.join("restored")
    }

    fn skein(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_skein"))
            .args(args)
            .current_dir(&self.base)
            .env("SKEIN_HOME", &self.home)
            .env("SKEIN_FLEET_ROOT", &self.root)
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap()
    }

    fn send(&self) -> String {
        let out = self.skein(&["migrate", "send", self.bundle().to_str().unwrap()]);
        assert!(out.status.success(), "send failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn receive_from(&self, file: &Path) -> Output {
        self.skein(&[
            "migrate",
            "receive",
            file.to_str().unwrap(),
            "--into",
            self.target().to_str().unwrap(),
        ])
    }

    fn receive(&self) -> String {
        let out = self.receive_from(&self.bundle());
        assert!(out.status.success(), "receive failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// A box with every kind of work a backup must keep, plus a second box, state and transcripts.
fn fixture(tag: &str) -> Fixture {
    let scratch = Scratch::temp(&format!("migrate-backup-{tag}"));
    let base = scratch.path().to_path_buf();
    let home = base.join("home");
    let root = base.join("fleet");
    let bin = base.join("bin");
    for dir in [&home, &root, &bin, &base.join("out")] {
        fs::create_dir_all(dir).unwrap();
    }
    let docker = bin.join("docker");
    fs::write(
        &docker,
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n\"volume ls\") printf 'pgdata\\n{ANON}\\n' ;;\n\
             \"image ls\") printf '<none>\\tlocal-app:dev\\t2.0GB\\n' ;;\n\
             \"system df\") echo '{{}}' ;;\n\"ps -aq\") ;;\n*) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();

    let tree = root.join("box-one/tree");
    fs::create_dir_all(&tree).unwrap();
    git_in(&tree, &["init", "-q", "-b", "main"]);
    write(&tree.join(".gitignore"), "target/\n.env\n.worktrees/\n");
    write(&tree.join("src.txt"), "first\n");
    git_in(&tree, &["add", ".gitignore", "src.txt"]);
    git_in(&tree, &["commit", "-qm", "first"]);
    git_in(&tree, &["branch", "tokens"]);
    write(&tree.join("src.txt"), "stashed\n");
    git_in(&tree, &["stash", "push", "-q", "-m", "kept"]);
    git_in(
        &tree,
        &["worktree", "add", "-q", ".worktrees/lane", "-b", "lane"],
    );
    write(&tree.join(".worktrees/lane/unpushed.txt"), "lane work\n");
    write(&tree.join("src.txt"), "uncommitted\n");
    write(&tree.join("untracked.txt"), "untracked\n");
    write(&tree.join(".env"), "HAND_MADE=1\n");
    write(&tree.join("target/out.bin"), "built\n");
    let script = tree.join("run.sh");
    write(&script, "#!/bin/sh\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o750)).unwrap();
    symlink("src.txt", tree.join("link-inside")).unwrap();
    symlink("/nonexistent/elsewhere", tree.join("link-outside")).unwrap();
    fs::hard_link(tree.join("untracked.txt"), tree.join("untracked-link.txt")).unwrap();
    write(
        &root.join("box-one/home/.cache/skein/claude/session/notes"),
        "scratchpad\n",
    );
    write(&root.join("box-one/home/.cache/pip/blob"), "rebuildable\n");
    write(&root.join("no-tree-dir/leftover.txt"), "kept\n");
    write(&root.join("stray-file"), "stray\n");
    write(&root.join(".skein/asks/q1/ask.json"), "{\"q\":1}\n");
    write(&root.join(".skein/plugin-turn-state/x"), "not carried\n");
    write(&root.join(".repo-tools/tool"), "rebuilt\n");

    write(&home.join("VERSION"), "1\n");
    write(&home.join("config.json"), "{}\n");
    write(&home.join("places/box-one.json"), "{}\n");
    write(
        &home.join("boxes/box-one/claude-projects/-t/session.jsonl"),
        "{\"role\":\"user\"}\n",
    );
    write(&home.join("boxes/box-one/private-note"), "stays\n");
    write(&home.join("repos/proj/store/data/notes.md"), "store\n");
    write(&home.join("brand-new-secret-file"), SENTINEL);
    Fixture {
        _scratch: scratch,
        base,
        home,
        root,
        bin,
    }
}

/// Every kind of credential the bundle must not hold, each holding the sentinel.
fn plant_credentials(fx: &Fixture) {
    let boxed = fx.root.join("box-one");
    for rel in [
        "home/.claude/.credentials.json",
        "home/.credentials-old",
        "home/github-pats/acct",
        "home/git-tokens/acct",
        "home/tokens/acct",
        "tree/server.pem",
        "home/api-token",
        "home/warden/secret",
        "home/fleet-agent.token",
        "home/fleet-agent.port",
        "home/.config/sync/env",
        "tree/.skein/private/key",
        "home/.git-credentials",
        "home/.netrc",
        "home/.ssh/id_ed25519",
        "home/.aws/credentials",
        "home/.config/gh/hosts.yml",
    ] {
        write(&boxed.join(rel), SENTINEL);
    }
    for rel in [
        "api-token",
        "fleet-agent.token",
        "fleet-agent.port",
        "warden/secret",
        "github-pats/acct",
        "git-tokens/acct",
        "tokens/acct",
        "fleet-home/.claude/.credentials.json",
        "repos/proj/store/.skein/private/key",
        "repos/proj/store/agent.pem",
        "boxes/box-one/claude-projects/.credentials.json",
    ] {
        write(&fx.home.join(rel), SENTINEL);
    }
    fs::hard_link(
        boxed.join("home/.claude/.credentials.json"),
        boxed.join("home/renamed-login"),
    )
    .unwrap();
}

fn usable() -> bool {
    if !have("bwrap") || !have("git") {
        skip("the backup's git probe needs bwrap and git");
        return false;
    }
    let works = Command::new("bwrap")
        .args(["--dev-bind", "/", "/", "--", "/bin/true"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !works {
        skip("bwrap cannot make a namespace here");
        return false;
    }
    let free = Command::new("df")
        .args(["-Pk", "."])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .nth(1)?
                .split_whitespace()
                .nth(3)?
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(0);
    if free < 11 * 1024 * 1024 {
        skip("send holds a 10 GB floor on the fleet's own disk, and this disk has less free");
        return false;
    }
    true
}

type Print = BTreeMap<PathBuf, (u32, u64, i64, Vec<u8>)>;

/// Relative path to (mode, length, mtime, content or link target).
fn print_of(dir: &Path) -> Print {
    fn go(top: &Path, at: &Path, into: &mut Print) {
        let meta = fs::symlink_metadata(at).unwrap();
        let body = if meta.is_file() {
            fs::read(at).unwrap()
        } else if meta.file_type().is_symlink() {
            fs::read_link(at)
                .unwrap()
                .into_os_string()
                .into_encoded_bytes()
        } else {
            Vec::new()
        };
        let length = if meta.is_dir() { 0 } else { meta.len() };
        let mtime = meta.mtime();
        into.insert(
            at.strip_prefix(top).unwrap().to_path_buf(),
            (meta.mode(), length, mtime, body),
        );
        if meta.is_dir() {
            for entry in fs::read_dir(at).unwrap() {
                go(top, &entry.unwrap().path(), into);
            }
        }
    }
    let mut print = BTreeMap::new();
    go(dir, dir, &mut print);
    print
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Offsets of each unit's first data frame, found by reading the frames.
fn frame_offsets(bundle: &[u8]) -> Vec<(u8, usize, usize)> {
    let mut at = 12;
    let mut frames = Vec::new();
    while at < bundle.len() {
        let kind = bundle[at];
        let length = u64::from_be_bytes(bundle[at + 1..at + 9].try_into().unwrap()) as usize;
        frames.push((kind, at, length));
        at += 9 + length + if kind == b'D' { 0 } else { 32 };
    }
    frames
}

/// Fails if any carried kind of work differs after the round trip: a mode, a link, a stash, a ref,
/// a worktree's file, a transcript, or a git-ignored file a person made.
#[test]
fn a_round_trip_keeps_every_kind_of_work_identical() {
    if !usable() {
        return;
    }
    let fx = fixture("round");
    fx.send();
    let said = fx.receive();
    assert!(said.contains("verified by SHA-256"), "{said}");

    let source = fx.root.join("box-one/tree");
    let copy = fx.target().join("fleet/box-one/tree");
    let (from, to) = (print_of(&source), print_of(&copy));
    assert!(from.len() > 40, "the fixture is not populated");
    assert_eq!(
        from.keys().collect::<Vec<_>>(),
        to.keys().collect::<Vec<_>>(),
        "the tree's paths differ"
    );
    for (path, was) in &from {
        assert_eq!(Some(was), to.get(path), "{} differs", path.display());
    }

    let refs = ["for-each-ref", "--format=%(refname) %(objectname)"];
    let source_refs = git_in(&source, &refs);
    assert!(source_refs.contains("refs/stash"), "{source_refs}");
    assert!(source_refs.contains("refs/heads/tokens"), "{source_refs}");
    assert_eq!(source_refs, git_in(&copy, &refs));
    assert_eq!(
        git_in(&source, &["stash", "list", "--format=%H %gs"]),
        git_in(&copy, &["stash", "list", "--format=%H %gs"])
    );
    assert_eq!(
        fs::read(source.join(".git/worktrees/lane/HEAD")).unwrap(),
        fs::read(copy.join(".git/worktrees/lane/HEAD")).unwrap()
    );
    let inode = |path: PathBuf| fs::metadata(path).unwrap().ino();
    assert_eq!(
        inode(copy.join("untracked.txt")),
        inode(copy.join("untracked-link.txt")),
        "a hard link came back as two files"
    );
    for rel in [
        "home/boxes/box-one/claude-projects/-t/session.jsonl",
        "home/repos/proj/store/data/notes.md",
        "home/config.json",
        "fleet/box-one/home/.cache/skein/claude/session/notes",
        "fleet/no-tree-dir/leftover.txt",
        "fleet/stray-file",
        "fleet/.skein/asks/q1/ask.json",
    ] {
        let original = match rel.strip_prefix("home/") {
            Some(rest) => fx.home.join(rest),
            None => fx.root.join(rel.strip_prefix("fleet/").unwrap()),
        };
        assert_eq!(
            fs::read(original).unwrap(),
            fs::read(fx.target().join(rel)).unwrap(),
            "{rel}"
        );
    }
}

/// Fails if a named cache starts travelling, or if the allow-list stops holding back a state file
/// nobody listed.
#[test]
fn caches_and_unlisted_state_stay_behind_and_are_named() {
    if !usable() {
        return;
    }
    let fx = fixture("left");
    let said = fx.send();
    fx.receive();
    let into = fx.target();
    assert!(!into.join("fleet/box-one/home/.cache/pip").exists());
    assert!(!into.join("home/brand-new-secret-file").exists());
    assert!(!into.join("home/boxes/box-one/private-note").exists());
    assert!(!into.join("fleet/.repo-tools").exists());
    assert!(!into.join("fleet/.skein/plugin-turn-state").exists());
    assert!(said.contains("fleet/box-one/home/.cache/pip"), "{said}");
    assert!(said.contains("home/brand-new-secret-file"), "{said}");
}

/// Fails if any one credential path is carried, under its own name or a hard link to it.
#[test]
fn no_credential_reaches_the_bundles_raw_bytes() {
    if !usable() {
        return;
    }
    let fx = fixture("creds");
    plant_credentials(&fx);
    let said = fx.send();
    assert!(said.contains("Credentials, which never travel"), "{said}");
    let raw = fs::read(fx.bundle()).unwrap();
    assert!(
        contains(&raw, b"HAND_MADE=1"),
        "the search would not find plain file content"
    );
    assert!(
        !contains(&raw, SENTINEL.as_bytes()),
        "a credential travelled"
    );
}

/// Fails if credential names are applied inside git's own ref directories, which deletes a branch.
#[test]
fn a_branch_named_like_a_credential_directory_survives() {
    if !usable() {
        return;
    }
    let fx = fixture("branch");
    fx.send();
    fx.receive();
    let copy = fx.target().join("fleet/box-one/tree");
    assert!(copy.join(".git/refs/heads/tokens").is_file());
}

/// Fails if the URL check is removed, because the token then sits in the bundle.
#[test]
fn a_token_inside_a_remote_url_refuses_the_backup() {
    if !usable() {
        return;
    }
    let fx = fixture("url");
    let tree = fx.root.join("box-one/tree");
    git_in(
        &tree,
        &[
            "remote",
            "add",
            "origin",
            "https://x:SENTINEL@host.example/r.git",
        ],
    );
    let out = fx.skein(&["migrate", "send", fx.bundle().to_str().unwrap()]);
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("inside a remote URL"));
    assert!(!fx.bundle().exists(), "a refusal wrote the file");
}

/// Fails if Docker's absence from the bundle stops being stated, by name, in the output and the
/// manifest.
#[test]
fn the_output_and_manifest_say_docker_volumes_are_not_included() {
    if !usable() {
        return;
    }
    let fx = fixture("docker");
    let said = fx.send();
    assert!(said.contains("Docker was NOT backed up"), "{said}");
    assert!(said.contains("pgdata") && said.contains(ANON), "{said}");
    let raw = fs::read(fx.bundle()).unwrap();
    let frames = frame_offsets(&raw);
    let (kind, at, length) = frames[0];
    assert_eq!(kind, b'M');
    let manifest: serde_json::Value =
        serde_json::from_slice(&raw[at + 9..at + 9 + length]).unwrap();
    assert_eq!(manifest["docker"]["included"], false);
    assert!(manifest["docker"]["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == ANON));
    assert_eq!(manifest["format"], 1);
    assert!(manifest["units"].as_array().unwrap().len() >= 5);
    assert!(fx.receive().contains("Docker was NOT backed up"));
}

/// Fails if a rerun extracts a finished unit again, or if a cut file loses the units before the cut.
#[test]
fn a_cut_file_keeps_its_complete_units_and_a_rerun_resumes() {
    if !usable() {
        return;
    }
    let fx = fixture("cut");
    fx.send();
    let raw = fs::read(fx.bundle()).unwrap();
    let frames = frame_offsets(&raw);
    let trailers: Vec<usize> = frames
        .iter()
        .filter(|(kind, _, _)| *kind == b'T')
        .map(|(_, at, length)| at + 9 + length + 32)
        .collect();
    assert!(trailers.len() >= 5);
    let cut = fx.base.join("out/cut.bundle");
    fs::write(&cut, &raw[..trailers[1] + 20]).unwrap();

    let out = fx.receive_from(&cut);
    assert!(!out.status.success(), "a cut file read as complete");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("2 of"), "{said}");
    let settled = fx.target().join("home/config.json");
    assert_eq!(fs::read(&settled).unwrap(), b"{}\n");
    assert!(!fx.target().join("fleet/box-one").exists());

    // A mark only a re-extraction would erase.
    fs::write(&settled, "marked\n").unwrap();
    let said = fx.receive();
    assert!(said.contains("2 already done"), "{said}");
    assert_eq!(
        fs::read(&settled).unwrap(),
        b"marked\n",
        "a verified unit was redone"
    );
    assert!(fx.target().join("fleet/box-one/tree/src.txt").is_file());
}

/// Fails if some offset of a cut file is read as complete, publishes a torn unit, or cannot resume.
#[test]
fn a_file_cut_at_any_point_resumes_to_the_same_result() {
    if !usable() {
        return;
    }
    let fx = fixture("cuts");
    fx.send();
    let raw = fs::read(fx.bundle()).unwrap();
    fx.receive();
    // `fleet/` and `fleet/.skein` are parents no unit holds, so their times are the restore's.
    let settled = |dir: &Path| {
        let mut print = print_of(dir);
        for parent in ["", ".skein"] {
            print.get_mut(Path::new(parent)).unwrap().2 = 0;
        }
        print
    };
    let whole = settled(&fx.target().join("fleet"));
    let step = (raw.len() / 29).max(1);
    for (round, end) in (1..raw.len()).step_by(step).enumerate() {
        let cut = fx.base.join("out/at.bundle");
        fs::write(&cut, &raw[..end]).unwrap();
        let into = fx.base.join(format!("cut-{round}"));
        let run = |file: &Path| {
            fx.skein(&[
                "migrate",
                "receive",
                file.to_str().unwrap(),
                "--into",
                into.to_str().unwrap(),
            ])
        };
        let out = run(&cut);
        assert!(!out.status.success(), "cut at {end} read as complete");
        if !into.join(".skein-receive").exists() {
            continue;
        }
        let out = run(&fx.bundle());
        assert!(out.status.success(), "no resume from {end}: {out:?}");
        assert_eq!(whole, settled(&into.join("fleet")), "cut at {end}");
    }
}

/// Fails if a crash while a unit is moved into place leaves a target that can never finish.
#[test]
fn a_crash_while_publishing_a_unit_finishes_on_the_next_run() {
    if !usable() {
        return;
    }
    let fx = fixture("publish");
    fx.send();
    let raw = fs::read(fx.bundle()).unwrap();
    let ends: Vec<usize> = frame_offsets(&raw)
        .iter()
        .filter(|(kind, _, _)| *kind == b'T')
        .map(|(_, at, length)| at + 9 + length + 32)
        .collect();
    let cut = fx.base.join("out/three.bundle");
    fs::write(&cut, &raw[..ends[2]]).unwrap();
    assert!(!fx.receive_from(&cut).status.success());
    let half = fx.target().join("home/repos/proj/store/data/notes.md");
    write(&half, "half-placed");
    let path = fx.target().join(".skein-receive/receipt.json");
    let mut receipt: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(receipt["done"].as_array().unwrap().len(), 3);
    receipt["publishing"] = serde_json::json!(3);
    fs::write(&path, receipt.to_string()).unwrap();

    let said = fx.receive();
    assert!(said.contains("3 already done"), "{said}");
    assert_eq!(fs::read(&half).unwrap(), b"store\n");
}

/// Fails if `send` reads a volume that was moved away, whose copy is stale.
#[test]
fn a_volume_that_was_moved_away_is_not_backed_up() {
    if !usable() {
        return;
    }
    let fx = fixture("moved");
    write(&fx.home.join("moved-to"), "/somewhere/else\n");
    let out = fx.skein(&["migrate", "send", fx.bundle().to_str().unwrap()]);
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("/somewhere/else"));
    assert!(!fx.bundle().exists());
}

/// Fails if the receive report stops warning that a restored worktree still names the original.
#[test]
fn the_restore_warns_that_worktrees_point_at_the_original() {
    if !usable() {
        return;
    }
    let fx = fixture("worktree-note");
    fx.send();
    let said = fx.receive();
    assert!(said.contains("would act on the original"), "{said}");
    let pointer =
        fs::read_to_string(fx.target().join("fleet/box-one/tree/.worktrees/lane/.git")).unwrap();
    assert!(
        pointer.contains(fx.root.to_str().unwrap()),
        "the pointer no longer names the source, so the warning is stale: {pointer}"
    );
}

/// Fails if the unit hash is not checked: the flipped byte is inside a file's body.
#[test]
fn a_flipped_byte_in_a_unit_is_refused() {
    if !usable() {
        return;
    }
    let fx = fixture("flip");
    fx.send();
    let mut raw = fs::read(fx.bundle()).unwrap();
    let at = raw
        .windows(b"HAND_MADE=1".len())
        .position(|w| w == b"HAND_MADE=1")
        .unwrap();
    raw[at] ^= 1;
    let bad = fx.base.join("out/flipped.bundle");
    fs::write(&bad, &raw).unwrap();
    let out = fx.receive_from(&bad);
    assert!(!out.status.success(), "a damaged unit was accepted");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("SHA-256"), "{said}");
    assert!(
        !fx.target().join("fleet/box-one").exists(),
        "the damaged unit was published"
    );
}

/// Build a one-unit bundle around hand-made tar bytes.
fn hostile_bundle(tar: &[u8]) -> Vec<u8> {
    fn sha(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut child = Command::new("sha256sum")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(data).unwrap();
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let hex = text.split_whitespace().next().unwrap();
        (0..32)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }
    fn frame(out: &mut Vec<u8>, kind: u8, payload: &[u8]) {
        let mut head = vec![kind];
        head.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        let mut summed = head.clone();
        summed.extend_from_slice(payload);
        out.extend_from_slice(&head);
        out.extend_from_slice(payload);
        out.extend_from_slice(&sha(&summed));
    }
    let hash: String = sha(tar).iter().map(|b| format!("{b:02x}")).collect();
    let line = serde_json::json!({"name": "box:x", "files": 1, "bytes": tar.len(), "sha256": hash});
    let manifest = serde_json::json!({
        "format": 1, "volume_schema": 1, "skein_commit": "x", "arch": "x", "os": "x",
        "created": "x", "encrypted": false, "units": [line],
        "docker": {"included": false, "answered": true, "why_unknown": "", "volumes": [], "images": []},
    });
    let mut out = b"SKEINBDL".to_vec();
    out.extend_from_slice(&1u32.to_be_bytes());
    frame(&mut out, b'M', manifest.to_string().as_bytes());
    frame(
        &mut out,
        b'U',
        serde_json::json!({"index": 0, "name": "box:x"})
            .to_string()
            .as_bytes(),
    );
    out.push(b'D');
    out.extend_from_slice(&(tar.len() as u64).to_be_bytes());
    out.extend_from_slice(tar);
    frame(
        &mut out,
        b'T',
        serde_json::json!({"files": 1, "bytes": tar.len(), "sha256": hash})
            .to_string()
            .as_bytes(),
    );
    out
}

fn tar_entry(path: &str, flag: u8, link: &str, body: &[u8]) -> Vec<u8> {
    let mut block = [0u8; 512];
    block[..path.len()].copy_from_slice(path.as_bytes());
    block[100..108].copy_from_slice(b"0000644\0");
    block[108..116].copy_from_slice(b"0000000\0");
    block[116..124].copy_from_slice(b"0000000\0");
    block[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
    block[136..148].copy_from_slice(b"00000000000\0");
    block[148..156].fill(b' ');
    block[156] = flag;
    block[157..157 + link.len()].copy_from_slice(link.as_bytes());
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    let sum: u32 = block.iter().map(|b| u32::from(*b)).sum();
    block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut out = block.to_vec();
    out.extend_from_slice(body);
    out.resize(out.len().next_multiple_of(512), 0);
    out
}

fn finish_tar(mut tar: Vec<u8>) -> Vec<u8> {
    tar.extend_from_slice(&[0u8; 1024]);
    tar
}

/// Fails if the extractor accepts a path that climbs, an absolute path, a write through a link it
/// just planted, a hard link out, or a device node.
#[test]
fn a_hostile_bundle_writes_nothing_outside_the_target() {
    if !usable() {
        return;
    }
    let fx = fixture("hostile");
    let victim = fx.base.join("victim");
    fs::create_dir_all(&victim).unwrap();
    let victim_text = victim.to_str().unwrap().to_string();
    let absolute = format!("{victim_text}/absolute");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "climbs",
            tar_entry("fleet/../../victim/climbed", b'0', "", b"x"),
        ),
        ("climbs-top", tar_entry("../victim/top", b'0', "", b"x")),
        ("absolute", tar_entry(&absolute, b'0', "", b"x")),
        (
            "outside-roots",
            tar_entry(".skein-receive/receipt.json", b'0', "", b"x"),
        ),
        (
            "through-link",
            [
                tar_entry("fleet/escape", b'2', &victim_text, b""),
                tar_entry("fleet/escape/through", b'0', "", b"x"),
            ]
            .concat(),
        ),
        (
            "dir-through-link",
            [
                tar_entry("fleet/escape", b'2', &victim_text, b""),
                tar_entry("fleet/escape/sub", b'5', "", b""),
            ]
            .concat(),
        ),
        (
            "hard-link-out",
            tar_entry("fleet/copied", b'1', "../../victim/secret", b""),
        ),
        (
            "hard-link-via-symlink",
            [
                tar_entry("fleet/escape", b'2', &victim_text, b""),
                tar_entry("fleet/copied", b'1', "fleet/escape/secret", b""),
            ]
            .concat(),
        ),
        ("device", tar_entry("fleet/dev", b'3', "", b"")),
    ];
    fs::write(victim.join("secret"), "victim").unwrap();
    let before = print_of(&victim);
    for (name, tar) in cases {
        let file = fx.base.join(format!("out/{name}.bundle"));
        fs::write(&file, hostile_bundle(&finish_tar(tar))).unwrap();
        let into = fx.base.join(format!("restored-{name}"));
        let out = fx.skein(&[
            "migrate",
            "receive",
            file.to_str().unwrap(),
            "--into",
            into.to_str().unwrap(),
        ]);
        assert!(!out.status.success(), "{name} was accepted: {out:?}");
        let said = String::from_utf8_lossy(&out.stderr);
        // The entry itself must be what is refused, not a count that happens to disagree.
        assert!(said.contains("refused the entry"), "{name}: {said}");
        assert_eq!(before, print_of(&victim), "{name} wrote outside its target");
        assert!(!into.join("fleet").exists(), "{name} published something");
    }
}

/// A later unit must not pass through a link an earlier unit published. Fails if `merge` follows a
/// link at its destination.
#[test]
fn a_link_from_one_unit_cannot_carry_the_next_unit_outside() {
    if !usable() {
        return;
    }
    let fx = fixture("two-units");
    let victim = fx.base.join("victim");
    fs::create_dir_all(&victim).unwrap();
    let first = finish_tar(tar_entry(
        "fleet/escape",
        b'2',
        victim.to_str().unwrap(),
        b"",
    ));
    let second = finish_tar(tar_entry("fleet/escape/landed", b'0', "", b"x"));
    let one = hostile_bundle(&first);
    let two = hostile_bundle(&second);
    // Splice the second bundle's unit into the first, renumbered through a fresh manifest.
    let frames_one = frame_offsets(&one);
    let frames_two = frame_offsets(&two);
    let unit_bytes = |raw: &[u8], frames: &[(u8, usize, usize)]| {
        let start = frames[1].1;
        raw[start..].to_vec()
    };
    let hash_of = |raw: &[u8], frames: &[(u8, usize, usize)]| {
        let (_, at, length) = *frames.iter().find(|f| f.0 == b'T').unwrap();
        let t: serde_json::Value = serde_json::from_slice(&raw[at + 9..at + 9 + length]).unwrap();
        t["sha256"].as_str().unwrap().to_string()
    };
    let (h1, h2) = (hash_of(&one, &frames_one), hash_of(&two, &frames_two));
    let manifest = serde_json::json!({
        "format": 1, "volume_schema": 1, "skein_commit": "x", "arch": "x", "os": "x",
        "created": "x", "encrypted": false,
        "units": [
            {"name": "box:x", "files": 1, "bytes": first.len(), "sha256": h1},
            {"name": "box:y", "files": 1, "bytes": second.len(), "sha256": h2}
        ],
        "docker": {"included": false, "answered": true, "why_unknown": "", "volumes": [], "images": []},
    })
    .to_string();
    let reframe = |kind: u8, payload: &[u8]| {
        let mut wrapped = hostile_frame(kind, payload);
        wrapped.shrink_to_fit();
        wrapped
    };
    let mut out = b"SKEINBDL".to_vec();
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend(reframe(b'M', manifest.as_bytes()));
    out.extend(unit_bytes(&one, &frames_one));
    let two_body = unit_bytes(&two, &frames_two);
    let head_len = 9 + frames_two[1].2 + 32;
    out.extend(reframe(
        b'U',
        serde_json::json!({"index": 1, "name": "box:y"})
            .to_string()
            .as_bytes(),
    ));
    out.extend_from_slice(&two_body[head_len..]);
    let file = fx.base.join("out/two.bundle");
    fs::write(&file, out).unwrap();
    let result = fx.receive_from(&file);
    assert!(!result.status.success(), "{result:?}");
    let said = String::from_utf8_lossy(&result.stderr);
    assert!(said.contains("refused"), "{said}");
    assert!(
        fs::symlink_metadata(fx.target().join("fleet/escape")).is_ok(),
        "the first unit never landed, so this proves nothing"
    );
    assert!(
        !victim.join("landed").exists(),
        "a unit escaped through a link"
    );
}

fn hostile_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut head = vec![kind];
    head.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    let mut child = Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&head).unwrap();
        stdin.write_all(payload).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let hex = text.split_whitespace().next().unwrap();
    let digest: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    [head, payload.to_vec(), digest].concat()
}

/// Fails if the format check is removed: a newer bundle would then be parsed as this one.
#[test]
fn a_newer_format_is_refused() {
    if !usable() {
        return;
    }
    let fx = fixture("newer");
    fx.send();
    let mut raw = fs::read(fx.bundle()).unwrap();
    raw[8..12].copy_from_slice(&2u32.to_be_bytes());
    let file = fx.base.join("out/newer.bundle");
    fs::write(&file, raw).unwrap();
    let out = fx.receive_from(&file);
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("format 2"), "{said}");
    assert!(!fx.target().exists(), "a refused bundle touched the target");
}

/// Fails if `send` opens its file without `create_new`, or `receive` accepts a target with files.
#[test]
fn neither_command_overwrites_anything() {
    if !usable() {
        return;
    }
    let fx = fixture("overwrite");
    fs::write(fx.bundle(), "precious").unwrap();
    let out = fx.skein(&["migrate", "send", fx.bundle().to_str().unwrap()]);
    assert!(!out.status.success());
    assert_eq!(fs::read(fx.bundle()).unwrap(), b"precious");
    fs::remove_file(fx.bundle()).unwrap();

    let dangling = fx.base.join("out/dangling");
    symlink(fx.base.join("out/elsewhere"), &dangling).unwrap();
    let out = fx.skein(&["migrate", "send", dangling.to_str().unwrap()]);
    assert!(!out.status.success(), "wrote through a dangling link");
    assert!(!fx.base.join("out/elsewhere").exists());

    fx.send();
    fs::create_dir_all(fx.target()).unwrap();
    fs::write(fx.target().join("mine"), "precious").unwrap();
    let out = fx.receive_from(&fx.bundle());
    assert!(!out.status.success());
    assert_eq!(fs::read(fx.target().join("mine")).unwrap(), b"precious");
    assert!(!fx.target().join("fleet").exists());
}

/// Fails if the destination check is removed, because the bundle would then land in what it reads.
#[test]
fn a_destination_inside_the_fleet_is_refused() {
    if !usable() {
        return;
    }
    let fx = fixture("inside");
    let file = fx.root.join("box-one/skein.bundle");
    let out = fx.skein(&["migrate", "send", file.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(!file.exists());
}

/// Fails if `send` writes, touches or removes anything it reads, or runs a command from a box's
/// git config.
#[test]
fn send_leaves_the_source_byte_identical_and_runs_nothing_from_a_box() {
    if !usable() {
        return;
    }
    let fx = fixture("source");
    let tree = fx.root.join("box-one/tree");
    let marker = fx.base.join("hook-ran");
    let hook = fx.base.join("monitor.sh");
    write(&hook, &format!("#!/bin/sh\ntouch {}\n", marker.display()));
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git_in(&tree, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
    git_in(&tree, &["status", "--short"]);
    assert!(marker.exists(), "the planted command does not fire");
    fs::remove_file(&marker).unwrap();

    let before = (print_of(&fx.root), print_of(&fx.home));
    let dirs_before = dir_times(&fx.root);
    let out = fx.skein(&[
        "migrate",
        "send",
        fx.bundle().to_str().unwrap(),
        "--leave-out-build-output",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(before, (print_of(&fx.root), print_of(&fx.home)));
    assert_eq!(dirs_before, dir_times(&fx.root), "a directory was written");
    assert!(!marker.exists(), "a command from a box's config ran");
}

fn dir_times(dir: &Path) -> BTreeMap<PathBuf, (i64, i64)> {
    let mut times = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let meta = fs::symlink_metadata(&at).unwrap();
        if meta.is_dir() {
            times.insert(at.clone(), (meta.mtime(), meta.mtime_nsec()));
            for entry in fs::read_dir(&at).unwrap() {
                stack.push(entry.unwrap().path());
            }
        }
    }
    times
}

/// Fails if the opt-in drops a hand-made ignored file, or if it runs without being asked.
#[test]
fn leaving_out_build_output_is_opt_in_and_keeps_hand_made_files() {
    if !usable() {
        return;
    }
    let fx = fixture("opt-in");
    fx.send();
    fx.receive();
    let copy = fx.target().join("fleet/box-one/tree");
    assert!(copy.join("target/out.bin").is_file(), "left out unasked");

    let second = fx.base.join("out/thin.bundle");
    let out = fx.skein(&[
        "migrate",
        "send",
        second.to_str().unwrap(),
        "--leave-out-build-output",
    ]);
    assert!(out.status.success(), "{out:?}");
    let into = fx.base.join("restored-thin");
    let out = fx.skein(&[
        "migrate",
        "receive",
        second.to_str().unwrap(),
        "--into",
        into.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    let thin = into.join("fleet/box-one/tree");
    assert!(!thin.join("target").exists(), "the opt-in did nothing");
    assert_eq!(fs::read(thin.join(".env")).unwrap(), b"HAND_MADE=1\n");
    assert!(thin.join(".worktrees/lane/unpushed.txt").is_file());
}

/// Fails if an unreadable file is skipped without being asked and without being named.
#[test]
fn an_unreadable_file_refuses_unless_told_and_is_then_named() {
    if !usable() {
        return;
    }
    let fx = fixture("unreadable");
    let locked = fx.root.join("no-tree-dir/locked");
    write(&locked, "x");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&locked).is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
        skip("running as root, which ignores the mode that makes this provable");
        return;
    }
    let out = fx.skein(&["migrate", "send", fx.bundle().to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--skip-unreadable"));
    assert!(!fx.bundle().exists());
    let out = fx.skein(&[
        "migrate",
        "send",
        fx.bundle().to_str().unwrap(),
        "--skip-unreadable",
    ]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(out.status.success(), "{out:?}");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("UNREADABLE, so NOT backed up (1)"), "{said}");
    assert!(said.contains("fleet/no-tree-dir/locked"), "{said}");
}
