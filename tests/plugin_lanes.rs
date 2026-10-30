//! skein's lanes, made and removed by the plugin's `skein-mcp` (SKEIN-1374): the `skein_lane` tool
//! over stdio and `skein-mcp lane create|drop|list` on the command line, against a real git
//! repository in a scratch directory, with `$SKEIN_STATE` and the lane root inside it.
//!
//! The server is the shipped bytes, `include_str!` of the file `runtime::PLUGIN_FILES` installs, run
//! by `python3`. Nothing here reads or writes outside the scratch directory: `$SKEIN_STATE`,
//! `$SKEIN_LANE_ROOT`, `$HOME`, `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` are all pinned inside it, and
//! git is told to read no global or system config.

mod common;

use common::{have, skip, Scratch};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SERVER: &str = include_str!("../src/plugin/bin/skein-mcp");
const LANE_CONTRACT: &str = include_str!("../src/plugin/tests/lane-contract.ts");

/// A box with a repository: `repo` (one commit on `master`), `state`, the lane root `lanes`, a
/// rules file, and the server beside them.
struct Fixture {
    dir: Scratch,
}

impl Fixture {
    fn new(prefix: &str) -> Fixture {
        let dir = Scratch::temp(prefix);
        fs::write(dir.path().join("skein-mcp"), SERVER).unwrap();
        for d in ["state", "boxhome", "skein-home", "fleet", "repo"] {
            fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        fs::write(
            dir.path().join("rules.md"),
            "Rule one: stay in your worktree.\n",
        )
        .unwrap();
        let f = Fixture { dir };
        f.git(&f.at("repo"), &["init", "-q", "-b", "master"]);
        fs::write(f.at("repo/README"), "hello\n").unwrap();
        f.git(&f.at("repo"), &["add", "README"]);
        f.git(
            &f.at("repo"),
            &[
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        f
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn s(&self, rel: &str) -> String {
        self.at(rel).display().to_string()
    }

    fn env<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        cmd.env("SKEIN_STATE", self.at("state"))
            .env("SKEIN_LANE_ROOT", self.at("lanes"))
            .env("HOME", self.at("boxhome"))
            .env("SKEIN_HOME", self.at("skein-home"))
            .env("SKEIN_FLEET_ROOT", self.at("fleet"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(self.at("repo"))
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self
            .env(&mut Command::new("git"))
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `skein-mcp lane <args>`: its exit code and the one JSON line it printed.
    fn cli(&self, args: &[&str]) -> (Option<i32>, Value) {
        let out = self
            .env(&mut Command::new("python3"))
            .arg(self.at("skein-mcp"))
            .arg("lane")
            .args(args)
            .output()
            .expect("python3");
        let line = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
        (out.status.code(), line)
    }

    /// The `skein_lane` tool over stdio: its result, its sentence, and whether it was an error.
    fn tool(&self, args: Value) -> (Value, String, bool) {
        let mut child = self
            .env(&mut Command::new("python3"))
            .arg(self.at("skein-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("python3");
        let call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                          "params":{"name":"skein_lane","arguments":args}});
        writeln!(child.stdin.take().unwrap(), "{call}").unwrap();
        let out = child.wait_with_output().unwrap();
        let reply: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
        let content = &reply["result"]["content"];
        (
            serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap(),
            content[1]["text"].as_str().unwrap_or("").to_string(),
            reply["result"]["isError"] == json!(true),
        )
    }

    fn create(&self, name: &str) -> Value {
        let rules = self.s("rules.md");
        let (code, line) = self.cli(&[
            "create", name, "--item", "SKEIN-9", "--base", "master", "--rules", &rules,
        ]);
        assert_eq!(code, Some(0), "{line}");
        line["result"].clone()
    }
}

/// None when this machine cannot run these: they make real worktrees.
fn ready() -> bool {
    if !have("git") {
        skip("git is not installed, so no lane can be made");
        return false;
    }
    true
}

/// **A lane's build output is inside its own worktree**, in its record and in the brief its
/// subagent is handed, so it goes when the lane goes
/// (docs/decisions/build-output-stays-in-the-worktree.md).
///
/// What would make it fail: the target placed anywhere else, for example under `$SKEIN_STATE`, the
/// shared-path leak that decision was written after. Watched: with `"target"` set to
/// `os.path.join(os.environ["SKEIN_STATE"], "target")`, this failed on the record's target.
#[test]
fn a_lanes_build_output_is_inside_its_worktree() {
    if !ready() {
        return;
    }
    let f = Fixture::new("skein-lanes-it-target");
    let rec = f.create("a");
    let worktree = f.s("lanes/a");
    assert_eq!(rec["worktree"], worktree, "{rec}");
    let target = rec["target"].as_str().unwrap();
    assert!(
        target.starts_with(&format!("{worktree}/")),
        "the target {target} is not inside {worktree}"
    );
    let brief = fs::read_to_string(rec["brief"].as_str().unwrap()).unwrap();
    assert!(
        brief.contains(&format!("    export CARGO_TARGET_DIR={target}\n")),
        "{brief}"
    );
    assert!(
        brief.ends_with("Rule one: stay in your worktree.\n"),
        "{brief}"
    );
    // A real worktree on its own branch, from the base.
    assert_eq!(
        f.git(&f.at("lanes/a"), &["branch", "--show-current"])
            .trim(),
        "lane/a"
    );
    assert_eq!(rec["schema"], 1);
    assert_eq!(rec["phase"], "created");
    assert_eq!(rec["item"], "SKEIN-9");
    let on_disk: Value = serde_json::from_str(
        &fs::read_to_string(f.at("boxhome/.local/state/skein/lanes/a.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk, rec);
}

/// **A record from a newer skein is never acted on**: `list` leaves it out and says so, and `drop`
/// removes nothing for it, since this skein cannot know what the newer one put there.
///
/// What would make it fail: reading any schema as this one's. Watched: with the refusal moved to
/// `schema > LANE_SCHEMA + 1`, the list carried lane `b` and this failed there.
#[test]
fn a_record_from_a_newer_skein_is_refused() {
    if !ready() {
        return;
    }
    let f = Fixture::new("skein-lanes-it-schema");
    f.create("b");
    let path = f.at("boxhome/.local/state/skein/lanes/b.json");
    let mut rec: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    rec["schema"] = json!(2);
    fs::write(&path, rec.to_string()).unwrap();

    let (code, listed) = f.cli(&["list"]);
    assert_eq!(code, Some(0), "{listed}");
    assert_eq!(listed["result"]["lanes"], json!([]), "{listed}");
    assert_eq!(
        listed["result"]["refused"],
        json!([{"name":"b","error":"newer-schema","schema":2}]),
        "{listed}"
    );

    let (code, dropped) = f.cli(&["drop", "b"]);
    assert_eq!(code, Some(1), "{dropped}");
    assert_eq!(dropped["result"]["error"], "newer-schema", "{dropped}");
    assert!(f.at("lanes/b/README").exists(), "the worktree was removed");
    assert!(path.exists(), "the record was removed");
}

/// **Drop removes the lane's worktree, its build output and its record, and nothing outside it**:
/// not another lane, not the lane root, not what a link inside the worktree points at, not the
/// branch. A worktree with work not committed, or a record naming a worktree outside the lane root,
/// is refused and nothing is removed.
///
/// What would make it fail: a removal wider than `<lane root>/<name>`. Watched: with the removal
/// changed to `remove_listed(os.path.dirname(root), root)`, the lane root went with lane `a`, and
/// this failed on lane `b`'s worktree.
#[test]
fn drop_removes_the_worktree_and_nothing_outside_it() {
    if !ready() {
        return;
    }
    let f = Fixture::new("skein-lanes-it-drop");
    f.create("a");
    f.create("b");
    fs::create_dir_all(f.at("lanes/a/target/debug")).unwrap();
    fs::write(f.at("lanes/a/target/debug/big"), "x").unwrap();
    fs::write(f.at("lanes/beside"), "not a lane").unwrap();
    fs::create_dir_all(f.at("outside")).unwrap();
    fs::write(f.at("outside/kept"), "kept").unwrap();

    // Work not committed is somebody's: nothing is removed.
    fs::write(f.at("lanes/a/notes.txt"), "unsaved").unwrap();
    let (code, refused) = f.cli(&["drop", "a"]);
    assert_eq!(code, Some(1), "{refused}");
    assert_eq!(refused["result"]["error"], "uncommitted", "{refused}");
    assert!(f.at("lanes/a/notes.txt").exists());
    fs::remove_file(f.at("lanes/a/notes.txt")).unwrap();

    // A link inside its build output is removed as itself; what it points at stays.
    std::os::unix::fs::symlink(f.at("outside"), f.at("lanes/a/target/link")).unwrap();

    // Everything that must survive was there before the drop.
    for p in [
        "lanes/b/README",
        "lanes/beside",
        "outside/kept",
        "lanes/a/target/debug/big",
    ] {
        assert!(f.at(p).exists(), "{p} was never there");
    }
    let (code, done) = f.cli(&["drop", "a"]);
    assert_eq!(code, Some(0), "{done}");
    assert_eq!(
        done["said"],
        format!(
            "skein: dropped lane a and removed {}. Its branch lane/a is kept.",
            f.s("lanes/a")
        )
    );
    assert!(!f.at("lanes/a").exists(), "the worktree is still there");
    assert!(
        !f.at("boxhome/.local/state/skein/lanes/a.json").exists(),
        "the record is still there"
    );
    assert!(
        !f.at("boxhome/.local/state/skein/lanes/a.brief.md").exists(),
        "the brief is still there"
    );
    for p in [
        "lanes/b/README",
        "lanes/beside",
        "outside/kept",
        "boxhome/.local/state/skein/lanes/b.json",
    ] {
        assert!(f.at(p).exists(), "{p} was removed");
    }
    let listed = f.git(&f.at("repo"), &["worktree", "list", "--porcelain"]);
    assert!(
        !listed.contains(&f.s("lanes/a")),
        "git still records it: {listed}"
    );
    assert!(listed.contains(&f.s("lanes/b")), "{listed}");
    assert_eq!(
        f.git(&f.at("repo"), &["branch", "--list", "lane/a"]).trim(),
        "lane/a"
    );

    // A record naming a worktree outside the lane root is refused, and that worktree kept.
    let path = f.at("boxhome/.local/state/skein/lanes/b.json");
    let mut rec: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    rec["worktree"] = json!(f.s("outside"));
    fs::write(&path, rec.to_string()).unwrap();
    let (code, refused) = f.cli(&["drop", "b"]);
    assert_eq!(code, Some(1), "{refused}");
    assert_eq!(refused["result"]["error"], "outside-lane-root", "{refused}");
    assert!(f.at("outside/kept").exists() && f.at("lanes/b/README").exists());
}

/// The contract's records, result and sentence, read as the JSON after `LANE_CONTRACT = `.
fn contract() -> Value {
    let (_, json) = LANE_CONTRACT
        .split_once("export const LANE_CONTRACT = ")
        .expect("lane-contract.ts declares LANE_CONTRACT");
    serde_json::from_str(json.trim()).expect("LANE_CONTRACT is JSON")
}

/// **One input gives one answer at both doors of skein's lane list**: the `skein_lane` MCP tool,
/// and `skein-mcp lane list` on the command line, which the agents panel reads. Each is held to the
/// answer `src/plugin/tests/lane-contract.ts` writes down, and `agents-panel.test.tsx` holds the
/// panel to the lanes in that answer, so the three cannot drift apart.
///
/// What would make it fail: the command line building its own answer rather than calling the
/// tool's function, a refusal said at one door and not the other, or the contract no longer saying
/// what skein says. Watched: with the command line's result taken as `dict(lane(...))`, which drops
/// the sentence, this failed on the command line's `said`.
#[test]
fn both_doors_of_lane_list_give_the_contracts_answer() {
    if !ready() {
        return;
    }
    let c = contract();
    let f = Fixture::new("skein-lanes-it-contract");
    let fixture = f.dir.path().display().to_string();
    fs::create_dir_all(f.at("boxhome/.local/state/skein/lanes")).unwrap();
    for (name, rec) in c["records"].as_object().unwrap() {
        fs::write(
            f.at(&format!("boxhome/.local/state/skein/lanes/{name}.json")),
            rec.to_string().replace("/fixture", &fixture),
        )
        .unwrap();
    }
    let as_contract = |v: &Value| -> Value {
        serde_json::from_str(&v.to_string().replace(&fixture, "/fixture")).unwrap()
    };

    let (result, said, is_error) = f.tool(json!({"action": "list"}));
    assert_eq!(as_contract(&result), c["result"], "MCP: {result}");
    assert_eq!(said, c["said"].as_str().unwrap(), "MCP");
    assert!(!is_error, "MCP");

    let (code, line) = f.cli(&["list"]);
    assert_eq!(code, Some(0), "{line}");
    assert_eq!(
        as_contract(&line),
        json!({"result": c["result"], "said": c["said"]}),
        "the command line"
    );
}
