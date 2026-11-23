//! `skein migrate plan` against a fixture fleet: the real binary, real git inside its sandbox, and
//! a stub `docker`. Every path it reads is pinned to the fixture.

mod common;

use common::{have, skip, Scratch};
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ANON: &str = "3f2a91b8c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1";

fn git(dir: &Path, args: &[&str]) {
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
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn checkout(root: &Path, name: &str) -> PathBuf {
    let tree = root.join(name).join("tree");
    fs::create_dir_all(&tree).unwrap();
    git(&tree, &["init", "-q"]);
    write(&tree.join(".gitignore"), "build/\n.worktrees/\n");
    write(&tree.join("src.txt"), &"s".repeat(9000));
    write(&tree.join("build/out.bin"), &"b".repeat(40000));
    git(&tree, &["add", ".gitignore", "src.txt"]);
    git(&tree, &["commit", "-qm", "first"]);
    tree
}

fn record(home: &Path, name: &str) {
    let text = format!(r#"{{"sandbox":"skein-fleet","ns_pid":1,"home":"/h","tree":"/t/{name}"}}"#);
    write(&home.join("places").join(format!("{name}.json")), &text);
}

fn stub_docker(bin: &Path, body: &str) {
    fs::create_dir_all(bin).unwrap();
    let path = bin.join("docker");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn answering_docker() -> String {
    format!(
        r#"case "$1 $2" in
"volume ls") printf 'pgdata\n{ANON}\n' ;;
"image ls") printf 'sha256:aa\tpulled:1\t10MB\n<none>\tlocal-app:dev\t2.0GB\n<none>\t<none>:<none>\t1MB\n' ;;
"system df") printf '{{"Volumes":[{{"Name":"pgdata","Size":"1MB"}},{{"Name":"{ANON}","Size":"5MB"}}]}}\n' ;;
"ps -aq") echo c1 ;;
"inspect --format") printf '/thing-postgres\tvolume={ANON}\n' ;;
*) exit 1 ;;
esac"#
    )
}

struct Fixture {
    _scratch: Scratch,
    base: PathBuf,
    home: PathBuf,
    root: PathBuf,
    bin: PathBuf,
}

/// All four directory shapes, a nested worktree, and a credential directory nothing may enter.
fn fixture(tag: &str) -> Fixture {
    let scratch = Scratch::temp(&format!("migrate-plan-{tag}"));
    let base = scratch.path().to_path_buf();
    let home = base.join("home");
    let root = base.join("fleet");
    let bin = base.join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root).unwrap();

    let tree = checkout(&root, "box-recorded");
    record(&home, "box-recorded");
    git(
        &tree,
        &["worktree", "add", "-q", ".worktrees/lane", "-b", "lane"],
    );
    write(
        &tree.join(".worktrees/lane/unpushed.txt"),
        &"w".repeat(30000),
    );

    checkout(&root, "checkout-unrecorded");
    record(&home, "record-only");
    record(&home, "record-with-empty-dir");
    fs::create_dir_all(root.join("record-with-empty-dir")).unwrap();
    write(&root.join("no-tree-dir/leftover.txt"), &"l".repeat(5000));
    write(&root.join("stray-file"), "x");
    stub_docker(&bin, &answering_docker());
    Fixture {
        _scratch: scratch,
        base,
        home,
        root,
        bin,
    }
}

fn plan(fx: &Fixture) -> Output {
    Command::new(env!("CARGO_BIN_EXE_skein"))
        .args(["migrate", "plan"])
        .current_dir(&fx.base)
        .env("SKEIN_HOME", &fx.home)
        .env("SKEIN_FLEET_ROOT", &fx.root)
        .env(
            "PATH",
            format!("{}:{}", fx.bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "the plan failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn usable() -> bool {
    if !have("bwrap") || !have("git") {
        skip("the plan's git probe needs bwrap and git");
        return false;
    }
    let works = Command::new("bwrap")
        .args(["--dev-bind", "/", "/", "--", "/bin/true"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !works {
        skip("bwrap cannot make a namespace here");
    }
    works
}

/// Path to (type, mode, length, mtime, content hash) for everything under `dir`.
fn fingerprint(dir: &Path, into: &mut BTreeMap<PathBuf, (u32, u64, i64, i64, u64)>) {
    let meta = fs::symlink_metadata(dir).unwrap();
    let mut hasher = DefaultHasher::new();
    if meta.is_file() {
        fs::read(dir).unwrap().hash(&mut hasher);
    } else if meta.file_type().is_symlink() {
        fs::read_link(dir).unwrap().hash(&mut hasher);
    }
    into.insert(
        dir.to_path_buf(),
        (
            meta.mode(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            hasher.finish(),
        ),
    );
    if meta.is_dir() {
        for entry in fs::read_dir(dir).unwrap() {
            fingerprint(&entry.unwrap().path(), into);
        }
    }
}

/// The KiB after `label` on a report line, which must be whole KB in this small fixture.
fn part_kb(line: &str, label: &str) -> u64 {
    let (_, rest) = line
        .split_once(label)
        .unwrap_or_else(|| panic!("no {label:?}: {line}"));
    let (number, _) = rest
        .split_once(" KB")
        .unwrap_or_else(|| panic!("not a KB size after {label:?}: {line}"));
    number.parse().unwrap()
}

fn ignored_kb(line: &str) -> u64 {
    part_kb(line, "(ignored ")
}

fn tool_caches_kb(line: &str) -> u64 {
    part_kb(line, "tool caches ")
}

fn line_of<'a>(said: &'a str, name: &str) -> &'a str {
    let wanted = format!("  {name}:");
    said.lines()
        .find(|l| l.starts_with(&wanted))
        .unwrap_or_else(|| panic!("no line for {name}: {said}"))
}

/// Fails if a shape is dropped from the report or filed under the wrong heading.
#[test]
fn every_directory_shape_is_listed_under_its_own_heading() {
    if !usable() {
        return;
    }
    let fx = fixture("shapes");
    let said = stdout(&plan(&fx));
    let under = |heading: &str, name: &str| {
        let start = said
            .find(heading)
            .unwrap_or_else(|| panic!("no {heading}: {said}"));
        let body = &said[start..];
        let end = body[1..].find("\n\n").map_or(body.len(), |i| i + 1);
        assert!(
            body[..end].contains(name),
            "{name} not under {heading}: {said}"
        );
    };
    under("Boxes with a checkout and a record (1)", "box-recorded");
    under("Checkouts with no record", "checkout-unrecorded");
    for name in ["record-only", "record-with-empty-dir"] {
        under(
            "a start would clone a new box, so never start these (2)",
            name,
        );
    }
    under("`skein save` does not carry these (1)", "no-tree-dir");
    under("Not directories", "stray-file");
    assert!(said.ends_with("Nothing was changed.\n"), "{said}");
}

/// Fails if ignored output stops counting as rebuildable, or if an ignored folder holding a
/// worktree is counted as rebuildable, which would leave unpushed work behind.
#[test]
fn only_what_git_ignores_is_rebuildable_and_a_worktree_never_is() {
    if !usable() {
        return;
    }
    let fx = fixture("rebuild");
    let said = stdout(&plan(&fx));
    let line = said
        .lines()
        .find(|l| l.contains("checkout-unrecorded:"))
        .unwrap();
    assert!(
        (40..70).contains(&ignored_kb(line)),
        "build/ is 40 KB: {line}"
    );
    let boxed = said.lines().find(|l| l.contains("box-recorded:")).unwrap();
    assert!(
        (40..70).contains(&ignored_kb(boxed)),
        "only build/, not the 30 KB worktree: {boxed}"
    );
    assert!(said.contains("hold their own git history"), "{said}");
}

/// Fails if the anonymous volume is filtered out, as the resize probe filters it.
#[test]
fn an_anonymous_volume_is_named_with_the_container_using_it() {
    if !usable() {
        return;
    }
    let fx = fixture("anon");
    let said = stdout(&plan(&fx));
    assert!(
        said.contains(&format!(
            "ANONYMOUS volume {ANON}: 4.8 MB, used by thing-postgres"
        )),
        "{said}"
    );
    assert!(said.contains("Named volume pgdata"), "{said}");
    assert!(said.contains("no registry digest local-app:dev"), "{said}");
    assert!(
        !said.contains("pulled:1"),
        "a pulled image has a digest: {said}"
    );
}

/// Fails if a failed `docker` is read as an empty answer.
#[test]
fn docker_not_answering_reads_differently_from_docker_holding_nothing() {
    if !usable() {
        return;
    }
    let fx = fixture("docker");
    stub_docker(&fx.bin, "echo 'cannot connect' >&2; exit 1");
    let dead = stdout(&plan(&fx));
    assert!(
        dead.contains("Docker did not answer (cannot connect)"),
        "{dead}"
    );
    assert!(!dead.contains("no volumes"), "{dead}");
    assert!(
        dead.contains("Thin copy (rebuildable left behind): needs unknown"),
        "{dead}"
    );

    stub_docker(&fx.bin, "exit 0");
    let empty = stdout(&plan(&fx));
    assert!(empty.contains("Docker answered: no volumes"), "{empty}");
    assert!(!empty.contains("did not answer"), "{empty}");
}

/// Fails if the walk enters a credential directory: this one cannot be listed, so entering it
/// turns the box's size to unknown.
#[test]
fn a_credential_directory_is_never_entered() {
    if !usable() {
        return;
    }
    let fx = fixture("creds");
    let pats = fx.root.join("no-tree-dir/github-pats");
    fs::create_dir_all(&pats).unwrap();
    fs::set_permissions(&pats, fs::Permissions::from_mode(0o000)).unwrap();
    let readable_anyway = fs::read_dir(&pats).is_ok();
    let out = plan(&fx);
    fs::set_permissions(&pats, fs::Permissions::from_mode(0o700)).unwrap();
    if readable_anyway {
        skip("running as root, which ignores the mode that makes this provable");
        return;
    }
    let said = stdout(&out);
    let line = said.lines().find(|l| l.contains("no-tree-dir:")).unwrap();
    assert!(!line.contains("unknown"), "{line}");
    assert!(
        said.contains("1 credential location(s) were left unopened"),
        "{said}"
    );
}

/// An unreadable directory must read as unknown. Fails if it is measured as if it were empty.
#[test]
fn a_size_that_cannot_be_read_is_unknown_and_never_zero() {
    if !usable() {
        return;
    }
    let fx = fixture("unknown");
    let locked = fx.root.join("no-tree-dir/locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let readable_anyway = fs::read_dir(&locked).is_ok();
    let out = plan(&fx);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    if readable_anyway {
        skip("running as root, which ignores the mode that makes this provable");
        return;
    }
    let said = stdout(&out);
    let line = said.lines().find(|l| l.contains("no-tree-dir:")).unwrap();
    assert!(line.contains("unknown (at least"), "{line}");
    assert!(said.contains("cannot say, because"), "{said}");
}

/// Fails if the plan writes, touches or removes anything under the fixture.
#[test]
fn the_plan_changes_no_file() {
    if !usable() {
        return;
    }
    let fx = fixture("hash");
    let (mut before, mut after) = (BTreeMap::new(), BTreeMap::new());
    fingerprint(&fx.base, &mut before);
    assert!(before.len() > 20, "the fixture is not populated");
    stdout(&plan(&fx));
    fingerprint(&fx.base, &mut after);
    assert_eq!(before, after, "the plan changed something on disk");
}

/// A box's `core.fsmonitor` runs under plain git here, and must not run under the plan.
#[test]
fn a_command_in_a_boxs_git_config_does_not_run() {
    if !usable() {
        return;
    }
    let fx = fixture("exec");
    let tree = fx.root.join("checkout-unrecorded/tree");
    let marker = fx.base.join("config-ran");
    let hook = tree.join("monitor.sh");
    write(&hook, &format!("#!/bin/sh\ntouch {}\n", marker.display()));
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(&tree, &["config", "core.fsmonitor", hook.to_str().unwrap()]);

    git(&tree, &["status", "--short"]);
    assert!(
        marker.exists(),
        "the planted command does not fire, so this proves nothing"
    );
    fs::remove_file(&marker).unwrap();

    let said = stdout(&plan(&fx));
    assert!(
        !marker.exists(),
        "the plan ran a command from a box's config"
    );
    let line = said
        .lines()
        .find(|l| l.contains("checkout-unrecorded:"))
        .unwrap();
    assert!(
        (40..70).contains(&ignored_kb(line)),
        "git still answered: {line}"
    );
}

/// Fails if any one of these paths is dropped from the list of tool caches.
#[test]
fn each_fixed_tool_cache_counts_apart_from_git_ignored_output() {
    if !usable() {
        return;
    }
    let fx = fixture("caches");
    let boxed = fx.root.join("checkout-unrecorded");
    for rel in [
        "home/.cache/ms-playwright",
        "home/.cache/pip",
        "home/.cache/uv",
        "home/.cache/pnpm",
        "home/.rustup",
        "home/.cargo/registry",
        "home/.npm",
        "home/.pnpm-store",
        ".pnpm-store",
        "pnpm-store",
    ] {
        write(&boxed.join(rel).join("blob"), &"c".repeat(200_000));
        let said = stdout(&plan(&fx));
        let line = line_of(&said, "checkout-unrecorded");
        assert!(tool_caches_kb(line) >= 195, "{rel} is not counted: {line}");
        assert!((40..70).contains(&ignored_kb(line)), "{rel}: {line}");
        fs::remove_dir_all(boxed.join(rel)).unwrap();
    }
}

/// Fails if `home/.cache` is treated as disposable by prefix, which would drop session scratchpads
/// and whatever an agent keeps there.
#[test]
fn skeins_own_cache_and_an_agents_files_are_carried() {
    if !usable() {
        return;
    }
    let fx = fixture("skein-cache");
    let cache = fx.root.join("checkout-unrecorded/home/.cache");
    write(
        &cache.join("skein/claude/claude-1000/-proj/session/scratchpad/notes"),
        &"n".repeat(300_000),
    );
    write(&cache.join("agent-notes.log"), &"a".repeat(300_000));
    let said = stdout(&plan(&fx));
    let line = line_of(&said, "checkout-unrecorded");
    assert!(tool_caches_kb(line) < 100, "{line}");
    assert!(part_kb(line, "carry ") >= 580, "{line}");
    assert!(
        said.contains("including skein's own session files"),
        "{said}"
    );
}

/// Fails if a directory outside the fixed list starts counting as a tool cache.
#[test]
fn nothing_else_in_a_home_is_rebuildable() {
    if !usable() {
        return;
    }
    let fx = fixture("home-rest");
    let boxed = fx.root.join("checkout-unrecorded");
    for rel in [
        "home/src",
        "home/runs",
        "home/opt",
        "home/nix-root",
        "home/backups",
        "home/.config",
        "home/.claude",
        "home/.cargo/bin",
    ] {
        write(&boxed.join(rel).join("kept"), &"k".repeat(200_000));
    }
    let said = stdout(&plan(&fx));
    let line = line_of(&said, "checkout-unrecorded");
    assert_eq!(tool_caches_kb(line), 0, "{line}");
    let carried = line.split_once("carry ").unwrap().1;
    let (mb, _) = carried
        .split_once(" MB")
        .unwrap_or_else(|| panic!("eight 200 KB directories are over a MB: {line}"));
    assert!(mb.parse::<f64>().unwrap() >= 1.5, "{line}");
}

/// A pnpm store shares inodes with `node_modules`. Fails if a file also linked from carried work
/// counts as rebuildable, or if a file in two rebuildable places counts twice.
#[test]
fn a_hard_link_counts_once_and_only_when_all_of_it_is_rebuildable() {
    if !usable() {
        return;
    }
    let fx = fixture("links");
    let boxed = fx.root.join("checkout-unrecorded");
    write(&boxed.join("pnpm-store/carried"), &"c".repeat(200_000));
    fs::hard_link(
        boxed.join("pnpm-store/carried"),
        boxed.join("tree/kept-by-hand"),
    )
    .unwrap();
    let said = stdout(&plan(&fx));
    let line = line_of(&said, "checkout-unrecorded");
    assert!(
        tool_caches_kb(line) < 100,
        "linked from carried work: {line}"
    );

    fs::remove_file(boxed.join("tree/kept-by-hand")).unwrap();
    fs::hard_link(
        boxed.join("pnpm-store/carried"),
        boxed.join("tree/build/linked"),
    )
    .unwrap();
    let said = stdout(&plan(&fx));
    let line = line_of(&said, "checkout-unrecorded");
    let both = ignored_kb(line) + tool_caches_kb(line);
    assert!(
        (200..300).contains(&both),
        "counted twice or not at all: {line}"
    );
}

/// Fails if the cache paths are followed through a link a box planted.
#[test]
fn a_cache_path_behind_a_link_is_not_measured() {
    if !usable() {
        return;
    }
    let fx = fixture("cache-link");
    let elsewhere = fx.root.join("no-tree-dir/elsewhere");
    write(&elsewhere.join("pip/blob"), &"e".repeat(300_000));
    let home = fx.root.join("checkout-unrecorded/home");
    fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(&elsewhere, home.join(".cache")).unwrap();
    let said = stdout(&plan(&fx));
    let line = line_of(&said, "checkout-unrecorded");
    assert_eq!(tool_caches_kb(line), 0, "{line}");
}

/// Fails if a tree with no repository is reported as git failing for some other reason.
#[test]
fn a_tree_that_is_not_a_repository_says_so() {
    if !usable() {
        return;
    }
    let fx = fixture("no-repo");
    fs::remove_dir_all(fx.root.join("checkout-unrecorded/tree/.git")).unwrap();
    let said = stdout(&plan(&fx));
    assert!(said.contains("its tree is not a git repository"), "{said}");
}

/// Fails if the tag's signature goes unchecked, a link counts, or skein's own directory can be
/// declared away. Fails the other way if a real tag or Cargo marker stops counting.
#[test]
fn only_a_directory_that_declares_itself_leaves_home_cache() {
    if !usable() {
        return;
    }
    const SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";
    let size = 200_000;
    type Markers<'a> = &'a [(&'a str, &'a str)];
    let cases: &[(&str, Markers, bool)] = &[
        ("tagged", &[("CACHEDIR.TAG", SIGNATURE)], true),
        ("cargo-target", &[(".rustc_info.json", "{}")], true),
        ("sccache", &[], true),
        (
            "wrong-tag",
            &[(
                "CACHEDIR.TAG",
                "Signature: 00000000000000000000000000000000\n",
            )],
            false,
        ),
        ("no-marker", &[], false),
        ("skein", &[("CACHEDIR.TAG", SIGNATURE)], false),
    ];
    for (name, markers, leaves) in cases {
        let fx = fixture(&format!("declared-{name}"));
        let dir = fx.root.join("checkout-unrecorded/home/.cache").join(name);
        write(&dir.join("payload"), &"p".repeat(size));
        for (marker, text) in *markers {
            write(&dir.join(marker), text);
        }
        let said = stdout(&plan(&fx));
        let line = line_of(&said, "checkout-unrecorded");
        assert_eq!(tool_caches_kb(line) >= 195, *leaves, "{name}: {line}");
    }

    let fx = fixture("declared-link");
    let elsewhere = fx.base.join("elsewhere");
    write(&elsewhere.join("payload"), &"p".repeat(size));
    write(&elsewhere.join("CACHEDIR.TAG"), SIGNATURE);
    let cache = fx.root.join("checkout-unrecorded/home/.cache");
    fs::create_dir_all(&cache).unwrap();
    std::os::unix::fs::symlink(&elsewhere, cache.join("linked")).unwrap();
    let said = stdout(&plan(&fx));
    let line = line_of(&said, "checkout-unrecorded");
    assert!(tool_caches_kb(line) < 100, "a link was followed: {line}");
}
