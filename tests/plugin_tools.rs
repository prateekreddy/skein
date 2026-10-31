//! skein's in-box tools, the plugin's MCP server (box-plugin §2.3, SKEIN-1059), driven over stdio
//! the way an agent's CLI drives it, against a fake `/proc`, `/sys` and fleet root.
//!
//! The server is the shipped bytes — `include_str!` of the file `runtime::PLUGIN_FILES` installs —
//! run by `python3` with a JSON-RPC conversation on stdin. Nothing here reads the real `/proc`.
//!
//! The two tests the item names are the first two: `top` never names a pid outside the box's own
//! cgroup, and a `state` a box wrote into its own request is never an answer.

mod common;

use common::{have, skip, Scratch};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

const SERVER: &str = include_str!("../src/plugin/bin/skein-mcp");

/// A fake machine: `root` stands in for `/`, `state` is the box's state directory, `fleet` its
/// fleet root, and the server script sits beside them.
struct Box_ {
    dir: Scratch,
    /// `$HOME` for the server, when a test needs it to be the box's own home.
    home: std::cell::RefCell<Option<std::path::PathBuf>>,
}

impl Box_ {
    fn new(prefix: &str) -> Box_ {
        let dir = Scratch::temp(prefix);
        fs::write(dir.path().join("skein-mcp"), SERVER).unwrap();
        for d in [
            "root/proc/self",
            "state/inbox",
            "state/signals",
            "fleet",
            "checkout",
        ] {
            fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        fs::write(dir.path().join("root/proc/uptime"), "10000.00 9000.00\n").unwrap();
        Box_ {
            dir,
            home: std::cell::RefCell::new(None),
        }
    }

    fn at(&self, rel: &str) -> std::path::PathBuf {
        self.dir.path().join(rel)
    }

    fn put(&self, rel: &str, body: &str) {
        let p = self.at(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    /// Say which cgroup this process is in, as the kernel would.
    fn in_cgroup(&self, rel: &str) {
        self.put("root/proc/self/cgroup", &format!("0::{rel}\n"));
    }

    /// A process in the fake `/proc`: `rss_pages` resident, `ticks` of CPU, started at 100 s.
    fn process(&self, pid: u32, ppid: u32, rss_pages: u64, ticks: u64, cmd: &str) {
        // Fields 3.. after `(comm)`: state ppid pgrp session tty tpgid flags minflt cminflt majflt
        // cmajflt utime stime cutime cstime priority nice threads itrealvalue starttime …
        self.put(
            &format!("root/proc/{pid}/stat"),
            &format!("{pid} (x y) S {ppid} 1 1 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 10000 0 0\n"),
        );
        self.put(
            &format!("root/proc/{pid}/statm"),
            &format!("1000 {rss_pages} 0 0 0 0 0\n"),
        );
        self.put(
            &format!("root/proc/{pid}/cmdline"),
            &format!("{cmd}\0--flag\0"),
        );
    }

    /// Run the server over one conversation and return every reply, in order.
    fn talk(&self, box_name: &str, messages: &[Value]) -> Vec<Value> {
        self.talk_in(&self.at("state"), box_name, messages)
    }

    fn talk_in(&self, state: &std::path::Path, box_name: &str, messages: &[Value]) -> Vec<Value> {
        let mut cmd = Command::new("python3");
        if let Some(home) = self.home.borrow().as_ref() {
            cmd.env("HOME", home);
        }
        let mut child = cmd
            .arg(self.at("skein-mcp"))
            .current_dir(self.at("checkout"))
            .env("SKEIN_TOOLS_FAKE_ROOT", self.at("root"))
            .env("SKEIN_STATE", state)
            .env("SKEIN_FLEET_ROOT", self.at("fleet"))
            .env("SKEIN_HOME", self.at("home"))
            .env("SKEIN_BOX", box_name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("python3");
        let mut stdin = child.stdin.take().unwrap();
        for m in messages {
            writeln!(stdin, "{m}").unwrap();
        }
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "the server failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Call one tool and return what it said, parsed.
    fn call(&self, box_name: &str, tool: &str, args: Value) -> Value {
        self.call_in(&self.at("state"), box_name, tool, args)
    }

    /// Call one tool and return what it said, parsed, and the sentence it said to the agent after
    /// it (the second content block), or "" when there was none.
    fn call_said(&self, box_name: &str, tool: &str, args: Value) -> (Value, String) {
        let replies = self.talk(
            box_name,
            &[json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                     "params":{"name":tool,"arguments":args}})],
        );
        let content = &replies[0]["result"]["content"];
        let json = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
        (json, content[1]["text"].as_str().unwrap_or("").to_string())
    }

    /// The owner's answer to one question, where skein puts it: the box's read-only inbox, in
    /// `mailbox::send_question_answer`'s words.
    fn answered(&self, n: u32, body: &str) {
        self.put(
            &format!("state/inbox/{n}-skein.json"),
            &json!({"from":"skein","kind":"answer","body":body,"ts":"2026-09-24T11:00:00Z"})
                .to_string(),
        );
    }

    /// The same, with `$SKEIN_STATE` at `state`: the directory skein's own code wrote into.
    fn call_in(&self, state: &std::path::Path, box_name: &str, tool: &str, args: Value) -> Value {
        let replies = self.talk_in(
            state,
            box_name,
            &[json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                     "params":{"name":tool,"arguments":args}})],
        );
        assert_eq!(replies.len(), 1, "{replies:?}");
        let text = replies[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {}", replies[0]));
        serde_json::from_str(text).unwrap()
    }
}

fn pids(top: &Value) -> Vec<u64> {
    let mut v: Vec<u64> = top["processes"]
        .as_array()
        .unwrap_or_else(|| panic!("no processes in {top}"))
        .iter()
        .map(|p| p["pid"].as_u64().unwrap())
        .collect();
    v.sort();
    v
}

/// **`skein_top` never names a pid outside the box's own cgroup**, however much larger the
/// processes outside it are.
///
/// Four processes are in the fake `/proc`, and the two outside the cgroup are the largest by both
/// memory and CPU, so a list drawn from all of `/proc` puts them first. The PID namespace is shared
/// (SKEIN-964): that list is every box's processes.
///
/// What would make it fail: `top` listing the digit directories of `/proc` instead of reading
/// `cgroup.procs`, or unioning the two; or the server taking its own cgroup from anywhere but
/// `/proc/self/cgroup` (the neighbour's `cgroup.procs` names 200 and 300).
#[test]
fn top_never_names_a_pid_outside_the_boxs_cgroup() {
    let b = Box_::new("skein-tools-it-top");
    b.in_cgroup("/skein/example");
    b.put(
        "root/sys/fs/cgroup/skein/example/cgroup.procs",
        "100\n101\n",
    );
    b.put(
        "root/sys/fs/cgroup/skein/neighbour/cgroup.procs",
        "200\n300\n",
    );
    b.process(100, 101, 2_000, 50, "cargo build");
    b.process(101, 1, 1_000, 10, "node server.js");
    b.process(200, 1, 900_000, 90_000, "the neighbour's build");
    b.process(300, 1, 800_000, 80_000, "the neighbour's agent");
    // An absence proves nothing unless the presence was there to be missed.
    for pid in [200, 300] {
        assert!(b.at(&format!("root/proc/{pid}/stat")).exists());
    }

    for by in ["memory", "cpu"] {
        let top = b.call("example", "skein_top", json!({"by": by, "n": 50}));
        assert_eq!(pids(&top), vec![100, 101], "by {by}: {top}");
        assert_eq!(top["processes_in_box"], 2, "{top}");
        // Largest first, within the box.
        assert_eq!(top["processes"][0]["pid"], 100, "by {by}: {top}");
    }

    // SKEIN_BOX is a variable a box sets; it does not choose the cgroup.
    let top = b.call("neighbour", "skein_top", json!({"by": "memory"}));
    assert_eq!(pids(&top), vec![100, 101], "{top}");
}

/// **A box whose processes are in its leaf and its jobs' cgroups is still one box to `skein_top`**
/// (SKEIN-1419): the server runs in the leaf, `skein/<box>/box`, a heavy job's processes in
/// `skein/<box>/job-<id>`, and the box's own `cgroup.procs` is empty.
///
/// What would make it fail: `own_cgroup` accepting only `skein/<box>` (the sabotage that was run:
/// `not-in-a-box-cgroup`, so a box started by this launcher has no process list), taking the leaf
/// as the box (the job's build is missing), or walking up out of the box (the neighbour's pids).
#[test]
fn top_counts_a_boxs_leaf_and_its_jobs_as_the_box() {
    let b = Box_::new("skein-tools-it-leaf");
    b.in_cgroup("/skein/example/box");
    b.put("root/sys/fs/cgroup/skein/example/cgroup.procs", "");
    b.put("root/sys/fs/cgroup/skein/example/box/cgroup.procs", "101\n");
    b.put(
        "root/sys/fs/cgroup/skein/example/job-j-1/cgroup.procs",
        "100\n",
    );
    b.put(
        "root/sys/fs/cgroup/skein/neighbour/box/cgroup.procs",
        "200\n",
    );
    b.process(100, 101, 2_000, 50, "cargo build");
    b.process(101, 1, 1_000, 10, "node server.js");
    b.process(200, 1, 900_000, 90_000, "the neighbour's build");
    let top = b.call("example", "skein_top", json!({"by": "memory", "n": 50}));
    assert_eq!(pids(&top), vec![100, 101], "{top}");
    assert_eq!(top["processes_in_box"], 2, "{top}");
}

/// **A process that is not in a box's cgroup gets no process list at all**, never everybody's.
///
/// A box started without cgroup delegation (`uncapped no-cgroup-delegation`) runs in the
/// sandbox's own cgroup. There is no "this box's processes" to show there.
///
/// What would make it fail: any fallback from a missing box cgroup to scanning `/proc`, or the
/// server accepting a cgroup outside `skein/<box>` (the sandbox's `user.slice` here, or
/// Docker's `skein/containers`).
#[test]
fn outside_a_box_cgroup_top_lists_nothing() {
    let b = Box_::new("skein-tools-it-nocg");
    b.put("root/sys/fs/cgroup/user.slice/cgroup.procs", "100\n200\n");
    b.put(
        "root/sys/fs/cgroup/skein/containers/cgroup.procs",
        "100\n200\n",
    );
    b.process(100, 1, 2_000, 50, "cargo build");
    b.process(200, 1, 900_000, 90_000, "the neighbour's build");
    for cg in ["/user.slice", "/skein/containers", "/skein"] {
        b.in_cgroup(cg);
        let top = b.call("example", "skein_top", json!({"by": "memory"}));
        assert!(top.get("processes").is_none(), "in {cg}: {top}");
        assert_eq!(top["error"], "not-in-a-box-cgroup", "in {cg}: {top}");
    }
}

/// **Each process says how old it is and whether it has a parent**, which is what (3) points the
/// agent at: "`skein_top` lists the ones with no parent."
///
/// What would make it fail: `parentless` computed from anything but the ppid in `stat`, the
/// age read from the wrong field (start time is field 22, in clock ticks since boot), or the
/// counts not covering the whole box when `n` cuts the list.
#[test]
fn top_says_which_processes_have_no_parent_and_how_old_they_are() {
    let b = Box_::new("skein-tools-it-orphans");
    b.in_cgroup("/skein/example");
    b.put(
        "root/sys/fs/cgroup/skein/example/cgroup.procs",
        "100\n101\n102\n",
    );
    b.process(100, 101, 3_000, 5, "child");
    b.process(101, 555, 2_000, 5, "parent");
    b.process(102, 1, 1_000, 5, "orphan");
    let top = b.call("example", "skein_top", json!({"by": "memory", "n": 1}));
    assert_eq!(top["processes_in_box"], 3, "{top}");
    assert_eq!(top["parentless_in_box"], 1, "{top}");
    assert_eq!(top["processes"].as_array().unwrap().len(), 1, "{top}");
    let all = b.call("example", "skein_top", json!({"by": "memory"}));
    let orphan = all["processes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["pid"] == 102)
        .unwrap();
    assert_eq!(orphan["parentless"], true, "{orphan}");
    assert_eq!(orphan["command"], "orphan --flag", "{orphan}");
    // Uptime 10000 s; started 10000 clock ticks after boot, which is 100 s at Linux's 100 Hz.
    assert_eq!(orphan["age_seconds"], 9900, "{orphan}");
    for p in all["processes"].as_array().unwrap() {
        if p["pid"] != 102 {
            assert_eq!(p["parentless"], false, "{p}");
        }
    }
}

/// **A `state: granted` a box wrote into its own request is never reported as an answer**, in
/// either queue; an answer in the owner's inbox, in the shape skein writes it, is.
///
/// Both request files say `granted` with a `decided` time, as the host's courtesy write-back would
/// leave them and as a box can write them itself. Only the inbox, bound read-only into the box, can
/// say the owner answered, and there the answer is `mailbox::send_answer`'s body, matched whole.
///
/// What would make it fail: `requests` carrying the file's `state` (or `decided`) into its answer;
/// matching an inbox note of another kind (`fleet-disk`) as an answer; matching an answer to a
/// request whose id is only a prefix of the one it names; or a package answer answering a write
/// request that carries the same id.
#[test]
fn a_box_written_granted_is_never_an_answer() {
    let b = Box_::new("skein-tools-it-requests");
    b.put(
        "fleet/.skein/substrate/requests/example/20260924-101200-7.json",
        r#"{"id":"20260924-101200-7","box":"example","kind":"apt","packages":["libnss3"],
            "asked":"2026-09-24T10:12:00Z","state":"granted","decided":"2026-09-24T10:13:00Z"}"#,
    );
    b.put(
        "fleet/.skein/gitgate/requests/example/20260924-101500-9.json",
        r#"{"id":"20260924-101500-9","box":"example","repo":"thing/example","reason":"a fix",
            "asked":"2026-09-24T10:15:00Z","state":"granted","decided":"2026-09-24T10:16:00Z"}"#,
    );
    // Not answers: a note of another kind in the answer's own words; an answer naming a longer id;
    // and a PACKAGE answer naming the write request's id.
    b.put(
        "state/inbox/1-skein.json",
        r#"{"from":"skein","kind":"fleet-disk","body":"package request 20260924-101200-7, granted","ts":"2026-09-24T10:20:00Z"}"#,
    );
    b.put(
        "state/inbox/2-skein.json",
        r#"{"from":"skein","kind":"answer","body":"write request 20260924-101500-90, granted","ts":"2026-09-24T10:21:00Z"}"#,
    );
    b.put(
        "state/inbox/3-skein.json",
        r#"{"from":"skein","kind":"answer","body":"package request 20260924-101500-9, granted","ts":"2026-09-24T10:21:30Z"}"#,
    );

    let got = b.call("example", "skein_requests", json!({}));
    let rows = got["requests"]
        .as_array()
        .unwrap_or_else(|| panic!("{got}"));
    assert_eq!(rows.len(), 2, "{got}");
    for r in rows {
        assert_eq!(
            r["state"], "waiting",
            "reported as answered with no answer of its own in the inbox: {r}"
        );
        assert!(r["answer"].is_null(), "{r}");
        assert!(r.get("decided").is_none(), "{r}");
    }
    assert_eq!(rows[0]["packages"], json!(["libnss3"]), "{got}");
    assert_eq!(rows[1]["repo"], "thing/example", "{got}");

    // The owner's answer, where only the host can put it, and it is the opposite of what the box
    // wrote into its own file.
    b.put(
        "state/inbox/4-skein.json",
        r#"{"from":"skein","kind":"answer","body":"package request 20260924-101200-7, denied","ts":"2026-09-24T10:22:00Z"}"#,
    );
    let got = b.call("example", "skein_requests", json!({}));
    let rows = got["requests"].as_array().unwrap();
    assert_eq!(rows[0]["state"], "denied", "{got}");
    assert_eq!(
        rows[0]["answer"]["body"], "package request 20260924-101200-7, denied",
        "{got}"
    );
    assert_eq!(rows[1]["state"], "waiting", "{got}");
}

/// **An install and a revoke are answers too, in the owner's approved words**: `package request
/// <id>, installed`, `package request <id>, not installed: <why>` (the state reads `not installed`
/// and `why` carries the reason), and `write request <id>, revoked`.
///
/// What would make it fail: the server's outcome words missing `installed`, `not installed: …` or
/// `revoked` (that request keeps its earlier `granted`); `why` not split out of the body; a
/// revocation written in the same second as its grant reading as the grant (the tie must go to the
/// later file); or an outcome of the other queue counting (`revoked` on a package, `installed` on a
/// write), which the newer bogus lines below would otherwise win with.
#[test]
fn an_install_and_a_revoke_are_answers_skein_requests_reports() {
    let b = Box_::new("skein-tools-it-outcomes");
    for (id, pkg) in [
        ("20260924-130000-1", "libnss3"),
        ("20260924-130000-2", "libgbm1"),
    ] {
        b.put(
            &format!("fleet/.skein/substrate/requests/example/{id}.json"),
            &format!(
                r#"{{"id":"{id}","box":"example","kind":"apt","packages":["{pkg}"],"asked":"2026-09-24T13:00:0{}Z"}}"#,
                &id[id.len() - 1..]
            ),
        );
    }
    b.put(
        "fleet/.skein/gitgate/requests/example/20260924-130000-3.json",
        r#"{"id":"20260924-130000-3","box":"example","repo":"thing/example","reason":"a fix","asked":"2026-09-24T13:00:03Z"}"#,
    );
    let answer = |n: u32, body: &str, ts: &str| {
        b.put(
            &format!("state/inbox/{n}-skein.json"),
            &format!(r#"{{"from":"skein","kind":"answer","body":"{body}","ts":"{ts}"}}"#),
        );
    };
    answer(
        1,
        "package request 20260924-130000-1, granted",
        "2026-09-24T13:01:00Z",
    );
    answer(
        2,
        "package request 20260924-130000-2, granted",
        "2026-09-24T13:01:00Z",
    );
    answer(
        3,
        "write request 20260924-130000-3, granted",
        "2026-09-24T13:02:00Z",
    );
    answer(
        4,
        "package request 20260924-130000-1, installed",
        "2026-09-24T13:03:00Z",
    );
    answer(
        5,
        "package request 20260924-130000-2, not installed: E: Unable to locate package libgbm1",
        "2026-09-24T13:03:00Z",
    );
    // The same second as the grant: only the file name says which came last.
    answer(
        6,
        "write request 20260924-130000-3, revoked",
        "2026-09-24T13:02:00Z",
    );
    // Newer, and each in the other queue's words: not answers.
    answer(
        7,
        "package request 20260924-130000-1, revoked",
        "2026-09-24T13:09:00Z",
    );
    answer(
        8,
        "write request 20260924-130000-3, installed",
        "2026-09-24T13:09:00Z",
    );

    let got = b.call("example", "skein_requests", json!({}));
    let rows = got["requests"]
        .as_array()
        .unwrap_or_else(|| panic!("{got}"));
    let row = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no row for {id}: {got}"))
    };
    let ok = row("20260924-130000-1");
    assert_eq!(ok["state"], "installed", "{got}");
    assert_eq!(
        ok["answer"]["body"], "package request 20260924-130000-1, installed",
        "{got}"
    );
    assert!(ok.get("why").is_none(), "{ok}");
    let bad = row("20260924-130000-2");
    assert_eq!(bad["state"], "not installed", "{got}");
    assert_eq!(bad["why"], "E: Unable to locate package libgbm1", "{got}");
    let write = row("20260924-130000-3");
    assert_eq!(write["state"], "revoked", "{got}");
    assert_eq!(
        write["answer"]["body"], "write request 20260924-130000-3, revoked",
        "{got}"
    );
}

/// **A browser request the inbox answers `installed` tells the agent how to link the browser into
/// its own box now, by the one script skein and the kit run; nothing else carries that line**
/// (SKEIN-1355).
///
/// What would make it fail: the hint dropped, or naming another script than the installed
/// `skein-link-browsers.sh`; it being given for an apt install, for a browser that did not install,
/// or for one whose request file merely says `installed` (a box can write that file, and only the
/// inbox answers).
#[test]
fn a_filled_browser_request_says_how_to_link_it_now() {
    let b = Box_::new("skein-tools-it-pwlink");
    for (id, kind, pkg, state) in [
        (
            "20261030-100000-1",
            "playwright",
            "chromium@1.62.0",
            "approved",
        ),
        ("20261030-100000-2", "apt", "libnss3", "approved"),
        (
            "20261030-100000-3",
            "playwright",
            "firefox@1.62.0",
            "approved",
        ),
        (
            "20261030-100000-4",
            "playwright",
            "webkit@1.62.0",
            "installed",
        ),
    ] {
        b.put(
            &format!("fleet/.skein/substrate/requests/example/{id}.json"),
            &format!(
                r#"{{"id":"{id}","box":"example","kind":"{kind}","packages":["{pkg}"],"state":"{state}","asked":"2026-10-30T10:00:0{}Z"}}"#,
                &id[id.len() - 1..]
            ),
        );
    }
    for (n, body) in [
        (1, "package request 20261030-100000-1, installed"),
        (2, "package request 20261030-100000-2, installed"),
        (
            3,
            "package request 20261030-100000-3, not installed: the fetch staged no browser",
        ),
    ] {
        b.put(
            &format!("state/inbox/{n}-skein.json"),
            &format!(
                r#"{{"from":"skein","kind":"answer","body":"{body}","ts":"2026-10-30T10:01:00Z"}}"#
            ),
        );
    }
    let got = b.call("example", "skein_requests", json!({}));
    let rows = got["requests"]
        .as_array()
        .unwrap_or_else(|| panic!("{got}"));
    let row = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no row for {id}: {got}"))
    };
    let script = format!(
        "bash {}/.skein/skein-link-browsers.sh",
        b.at("fleet").display()
    );
    let link = row("20261030-100000-1")["link"]
        .as_str()
        .unwrap_or_else(|| panic!("a filled browser request has no link line: {got}"))
        .to_string();
    assert!(link.ends_with(&script), "{link}");
    for id in [
        "20261030-100000-2",
        "20261030-100000-3",
        "20261030-100000-4",
    ] {
        assert!(row(id).get("link").is_none(), "{id} has a link line: {got}");
    }
}

/// **A decision the owner makes is the answer `skein_requests` reports**, end to end: the real
/// `substrate::decide` and `gitgate::decide`, the real inbox they write into, and the shipped
/// server reading it back as the box would (SKEIN-1142).
///
/// Each request file says `granted`, as a box can write it. The owner denies the package and
/// grants the write, so a tool that believed the file would report the package wrong.
///
/// What would make it fail: either decide not writing to the inbox (that request stays
/// `waiting`); the body drifting from the shape the server matches; or the server reading the
/// file's `state` (the package would read `granted`).
#[test]
fn a_decision_is_the_answer_skein_requests_reports() {
    let _lock = common::env_lock();
    let b = Box_::new("skein-tools-it-decided");
    let mut env = common::env_pins();
    env.set("SKEIN_HOME", b.at("home"));
    env.set("SKEIN_FLEET_ROOT", b.at("fleet"));
    b.put(
        "fleet/.skein/substrate/requests/example/20260924-101200-7.json",
        r#"{"id":"20260924-101200-7","box":"example","kind":"apt","packages":["libnss3"],
            "asked":"2026-09-24T10:12:00Z","state":"granted"}"#,
    );
    b.put(
        "fleet/.skein/gitgate/requests/example/20260924-101500-9.json",
        r#"{"id":"20260924-101500-9","box":"example","repo":"thing/example","reason":"a fix",
            "asked":"2026-09-24T10:15:00Z","state":"granted"}"#,
    );
    let package = skein::substrate::Request {
        id: "20260924-101200-7".into(),
        box_name: "example".into(),
        kind: "apt".into(),
        packages: vec!["libnss3".into()],
        state: "pending".into(),
        ..Default::default()
    };
    let write = skein::gitgate::Request {
        id: "20260924-101500-9".into(),
        box_name: "example".into(),
        repo: "thing/example".into(),
        state: "pending".into(),
        ..Default::default()
    };
    // No sandbox: the courtesy write-back into the box's file fails and is ignored.
    skein::substrate::decide("no-such-sandbox", &package, false, false).expect("decided");
    skein::gitgate::decide("no-such-sandbox", &write, true, Some(24)).expect("decided");

    let state = b.at("home/boxes/example");
    let got = b.call_in(&state, "example", "skein_requests", json!({}));
    let rows = got["requests"]
        .as_array()
        .unwrap_or_else(|| panic!("{got}"));
    assert_eq!(rows.len(), 2, "{got}");
    assert_eq!(rows[0]["id"], "20260924-101200-7", "{got}");
    assert_eq!(rows[0]["state"], "denied", "{got}");
    assert_eq!(rows[1]["id"], "20260924-101500-9", "{got}");
    assert_eq!(rows[1]["state"], "granted", "{got}");
}

/// **Only this box's queue is read.** A neighbour's request, in the queue root every box can
/// read, is not listed as this box's.
///
/// What would make it fail: globbing `requests/*/` rather than `requests/<box>/`, or a box name
/// with a traversal (`..`) being joined into the path.
#[test]
fn requests_lists_only_this_boxs_own() {
    let b = Box_::new("skein-tools-it-own");
    b.put(
        "fleet/.skein/substrate/requests/example/a.json",
        r#"{"id":"a","kind":"npm","packages":["left-pad"],"asked":"2026-09-24T10:00:00Z"}"#,
    );
    b.put(
        "fleet/.skein/substrate/requests/neighbour/b.json",
        r#"{"id":"b","kind":"npm","packages":["right-pad"],"asked":"2026-09-24T10:00:00Z"}"#,
    );
    let got = b.call("example", "skein_requests", json!({}));
    let ids: Vec<&str> = got["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["a"], "{got}");
    let got = b.call("../requests/neighbour", "skein_requests", json!({}));
    assert_eq!(got["requests"], json!([]), "{got}");
    assert_eq!(got["error"], "no-box-name", "{got}");
}

/// **`skein_resources` is the signal file, as skein wrote it.**
///
/// What would make it fail: the tool reading anywhere but `$SKEIN_STATE/signals/resources.json`,
/// or rewriting what it read.
#[test]
fn resources_returns_the_signal_file() {
    let b = Box_::new("skein-tools-it-resources");
    let file = json!({"at":"2026-09-24T10:00:00Z","asks":[{"kind":"pids","crossing_id":"c-1"}],
                      "pids":7000});
    b.put("state/signals/resources.json", &file.to_string());
    assert_eq!(b.call("example", "skein_resources", json!({})), file);
    fs::remove_file(b.at("state/signals/resources.json")).unwrap();
    let got = b.call("example", "skein_resources", json!({}));
    assert_eq!(got["error"], "no-signal-file", "{got}");
}

/// **The server speaks MCP over stdio**: it answers `initialize`, is silent on a notification,
/// lists exactly its seven tools, the three that only read marked read-only, and answers an unknown
/// method with an error rather than exiting.
///
/// What would make it fail: a reply to `notifications/initialized` (the client would read it as a
/// reply to its next request), a tool added or renamed without this list, or one bad line ending
/// the conversation.
#[test]
fn it_speaks_mcp_over_stdio() {
    let b = Box_::new("skein-tools-it-protocol");
    let replies = b.talk(
        "example",
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2025-06-18","capabilities":{},
                             "clientInfo":{"name":"test","version":"0"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"no/such"}),
            json!({"jsonrpc":"2.0","id":4,"method":"ping"}),
        ],
    );
    let ids: Vec<i64> = replies.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, vec![1, 2, 3, 4], "{replies:?}");
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    assert!(replies[0]["result"]["capabilities"]["tools"].is_object());
    let names: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "skein_resources",
            "skein_top",
            "skein_clean",
            "skein_requests",
            "skein_request_package",
            "skein_request_write",
            "skein_ask_person",
            "skein_request_disk",
            "skein_lane"
        ]
    );
    // The two that only read say so; the three that ask, the two that remove, and
    // skein_resources, which with `restart` writes the box's restart ask (SKEIN-1372), do not.
    for t in replies[1]["result"]["tools"].as_array().unwrap() {
        let reads = !t["name"].as_str().unwrap().starts_with("skein_request_")
            && t["name"] != "skein_ask_person"
            && t["name"] != "skein_clean"
            && t["name"] != "skein_lane"
            && t["name"] != "skein_resources";
        assert_eq!(t["annotations"]["readOnlyHint"], reads, "{t}");
    }
    assert_eq!(replies[2]["error"]["code"], -32601);
    assert!(replies[3]["result"].is_object());
}

/// **It holds no token and makes no network call**: everything it imports is in this list, and
/// none of it is a network module.
///
/// What would make it fail: an `import socket`, `urllib`, `http` or any other module added to the
/// server; and it names each one it found, so a new harmless import is a one-word change here.
#[test]
fn it_imports_nothing_that_reaches_the_network() {
    let allowed = [
        "datetime",
        "json",
        "os",
        "re",
        "stat",
        "subprocess",
        "sys",
        "time",
    ];
    let mut found = Vec::new();
    for line in SERVER.lines() {
        let t = line.trim_start();
        let module = t
            .strip_prefix("import ")
            .or_else(|| t.strip_prefix("from "))
            .map(|rest| rest.split([' ', ',', '.']).next().unwrap_or(""));
        if let Some(m) = module {
            found.push(m.to_string());
        }
    }
    assert!(
        !found.is_empty(),
        "read no imports at all, so this proves nothing"
    );
    for m in &found {
        assert!(allowed.contains(&m.as_str()), "the server imports {m}");
    }
    // The three subprocesses are `du`, over the box's own checkout, the launcher's own filing
    // entry points, which write into the box's own drop-boxes, and `git`, for the box's own lanes
    // (SKEIN-1374), which runs no command that reaches a remote.
    assert_eq!(SERVER.matches("subprocess.run(").count(), 3);
    assert!(SERVER.contains(r#"subprocess.run(["git"] + argv"#));
    for remote in [
        "\"fetch\"",
        "\"push\"",
        "\"pull\"",
        "\"clone\"",
        "\"ls-remote\"",
    ] {
        assert!(!SERVER.contains(remote), "the server runs git {remote}");
    }
    assert!(SERVER.contains(r#"["du", "-x", "-k", "--max-depth=1", where]"#));
    assert!(SERVER.contains(r#"["bash", launcher()] + argv[:1] + [box] + argv[1:]"#));
}

/// **The sixth waiting question is refused, with the approved words and the five it is waiting
/// on**; and an answered or dismissed question frees its slot.
///
/// The cap is counted from the box's own drop-box, with "answered" taken from the inbox only, as
/// `skein_requests` takes it — so a box cannot free a slot by writing `answered` into its own file,
/// and the owner's answer does free one.
///
/// What would make it fail: the question tool filing without checking the cap (the sixth is
/// filed); `CAP` other than 5; the sentence drifting from feature-wording.md's; a waiting count that believes the
/// file's own `state`, or one that ignores the inbox (the seventh ask after an answer would be
/// refused).
#[test]
fn the_sixth_waiting_question_is_refused_in_the_approved_words() {
    let b = Box_::new("skein-tools-it-askcap");
    fs::create_dir_all(b.at("fleet/.skein/asks/requests/example")).unwrap();
    let mut ids = Vec::new();
    for n in 0..5 {
        let (got, said) = b.call_said(
            "example",
            "skein_ask_person",
            json!({"question": format!("question {n}?"), "options": ["Drop it", "Keep it"]}),
        );
        let id = got["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{got}"))
            .to_string();
        assert_eq!(got["state"], "waiting", "{got}");
        assert_eq!(
            said,
            format!(
                "skein: asked the person (question {id}). The answer arrives in your inbox; \
                 nothing is answered yet."
            )
        );
        ids.push(id);
        // One second apart in `asked`, so "oldest first" is a fact about the files and not about
        // the order `listdir` happens to return.
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    // A box that writes `answered` into its own file frees nothing.
    let first = b.at(&format!(
        "fleet/.skein/asks/requests/example/{}.json",
        ids[0]
    ));
    let mut forged: Value = serde_json::from_str(&fs::read_to_string(&first).unwrap()).unwrap();
    forged["state"] = json!("answered");
    fs::write(&first, forged.to_string()).unwrap();

    let (got, said) = b.call_said(
        "example",
        "skein_ask_person",
        json!({"question": "a sixth?"}),
    );
    assert_eq!(
        got["error"], "at-cap",
        "the sixth waiting question was filed: {got}"
    );
    assert_eq!(
        said,
        format!(
            "skein: you already have 5 waiting: {}. Ask again once one is answered or dismissed. \
             skein_requests lists them.",
            ids.join(", ")
        )
    );
    let files = fs::read_dir(b.at("fleet/.skein/asks/requests/example"))
        .unwrap()
        .count();
    assert_eq!(files, 5, "a refused ask still left a file");

    // The owner dismisses one: its slot is free, and the next ask is filed.
    b.answered(
        1,
        &format!("question {} was dismissed without an answer", ids[2]),
    );
    let (got, _) = b.call_said("example", "skein_ask_person", json!({"question": "now?"}));
    assert_eq!(
        got["state"], "waiting",
        "a dismissal did not free the slot: {got}"
    );
}

/// **A question over 2 KB is refused in the approved words**, counting its options with its text,
/// and nothing is filed.
///
/// What would make it fail: the size check dropped or counting the question alone (the options
/// below carry it over), or the sentence's size printed as "2.0 KB" for a question just over.
#[test]
fn a_question_over_two_kilobytes_is_refused() {
    let b = Box_::new("skein-tools-it-asksize");
    fs::create_dir_all(b.at("fleet/.skein/asks/requests/example")).unwrap();
    let (got, said) = b.call_said(
        "example",
        "skein_ask_person",
        json!({"question": "x".repeat(2040), "options": ["123456789"]}),
    );
    assert_eq!(got["error"], "too-long", "{got}");
    assert_eq!(
        said,
        "skein: a question is at most 2 KB and this one is 2.1 KB. Shorten it and ask again."
    );
    assert_eq!(
        fs::read_dir(b.at("fleet/.skein/asks/requests/example"))
            .unwrap()
            .count(),
        0
    );
}

/// **`skein_requests` lists a question with the owner's answer, from the inbox only**: answered
/// with the chosen label, dismissed, or waiting whatever the file says.
///
/// What would make it fail: the `question` queue missing from `QUEUES`; the answer read from the
/// file's `state` or `answer`; the approved inbox shapes not matched, or matched as a prefix of a
/// longer id.
#[test]
fn a_question_is_answered_only_from_the_inbox() {
    let b = Box_::new("skein-tools-it-askanswer");
    for (id, at) in [("q-1", "10:00"), ("q-2", "10:01"), ("q-3", "10:02")] {
        b.put(
            &format!("fleet/.skein/asks/requests/example/{id}.json"),
            &json!({"id": id, "question": "keep it?", "options": ["Drop it", "Keep it"],
                    "asked": format!("2026-09-24T{at}:00Z"),
                    "state": "answered", "answer": "Drop it"})
            .to_string(),
        );
    }
    b.answered(1, r#"question q-1, "Keep it""#);
    b.answered(2, "question q-2 was dismissed without an answer");
    b.answered(3, r#"question q-30, "Drop it""#);
    let got = b.call("example", "skein_requests", json!({}));
    let rows: Vec<(String, String, Value)> = got["requests"]
        .as_array()
        .unwrap_or_else(|| panic!("{got}"))
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["state"].as_str().unwrap().to_string(),
                r["answered"].clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("q-1".into(), "answered".into(), json!("Keep it")),
            ("q-2".into(), "dismissed".into(), Value::Null),
            ("q-3".into(), "waiting".into(), Value::Null),
        ],
        "{got}"
    );
}

/// **`skein_request_package` files through the launcher's own entry point, into this box's own
/// drop-box, with the agent's `why` on the request, and answers with the id and the launcher's
/// sentence**; and it is capped at five waiting, with the ending the owner chose for this queue:
/// "granted or denied", where a question's is "answered or dismissed".
///
/// What would make it fail: the tool writing the file itself (no launcher sentence, and a second
/// copy of the package-name rules); the id not parsed from what the launcher said; `--why` not
/// passed, or not written into the request by `request_package`; the cap not applied to this
/// queue, or its sentence ending as a question's does.
#[test]
fn a_package_request_goes_through_the_launcher_and_is_capped() {
    if !have("jq") {
        return skip("no jq, so the launcher cannot file a package request");
    }
    let b = Box_::new("skein-tools-it-askpkg");
    b.put(
        "fleet/.skein/box-session.sh",
        include_str!("../src/box-session.sh"),
    );
    fs::create_dir_all(b.at("fleet/.skein/substrate/requests/example")).unwrap();
    let (got, said) = b.call_said(
        "example",
        "skein_request_package",
        json!({"manager": "apt", "packages": ["libnss3"], "why": "chromium needs it to start"}),
    );
    let id = got["id"].as_str().unwrap_or_else(|| panic!("{got} {said}"));
    assert_eq!(got["state"], "waiting", "{got}");
    assert!(
        said.starts_with(&format!(
            "skein: asked the fleet for apt (libnss3). Request {id} is pending approval."
        )),
        "{said}"
    );
    let filed: Value = serde_json::from_str(
        &fs::read_to_string(b.at(&format!(
            "fleet/.skein/substrate/requests/example/{id}.json"
        )))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(filed["packages"], json!(["libnss3"]));
    assert_eq!(filed["why"], "chromium needs it to start", "{filed}");
    let id = id.to_string();

    // Four more waiting, filed by hand in the launcher's shape, make five.
    for n in 1..5 {
        b.put(
            &format!("fleet/.skein/substrate/requests/example/2026010{n}-000000-1.json"),
            &json!({"id": format!("2026010{n}-000000-1"), "kind": "apt", "packages": [format!("p{n}")],
                    "asked": format!("2026-01-0{n}T00:00:00Z"), "state": "pending"})
            .to_string(),
        );
    }
    let (got, said) = b.call_said(
        "example",
        "skein_request_package",
        json!({"manager": "npm", "packages": ["left-pad"], "why": "the build script wants it"}),
    );
    assert_eq!(got["error"], "at-cap", "{got} {said}");
    assert_eq!(
        said,
        format!(
            "skein: you already have 5 waiting: 20260101-000000-1, 20260102-000000-1, \
             20260103-000000-1, 20260104-000000-1, {id}. Ask again once one is granted or denied. \
             skein_requests lists them."
        )
    );
}

/// **A package request without a `why` is refused, and nothing is filed**: missing, blank, not
/// text, or more than one line.
///
/// What would make it fail: the tool not checking `why` before it calls the launcher — the
/// launcher files a request with or without one, because the sudo shim never has one, so only the
/// tool can require it.
#[test]
fn a_package_request_without_a_why_is_refused() {
    if !have("jq") {
        return skip("no jq, so the launcher cannot file a package request");
    }
    let b = Box_::new("skein-tools-it-askwhy");
    b.put(
        "fleet/.skein/box-session.sh",
        include_str!("../src/box-session.sh"),
    );
    fs::create_dir_all(b.at("fleet/.skein/substrate/requests/example")).unwrap();
    for (what, args) in [
        (
            "missing",
            json!({"manager": "apt", "packages": ["libnss3"]}),
        ),
        (
            "blank",
            json!({"manager": "apt", "packages": ["libnss3"], "why": "  "}),
        ),
        (
            "not text",
            json!({"manager": "apt", "packages": ["libnss3"], "why": 7}),
        ),
        (
            "two lines",
            json!({"manager": "apt", "packages": ["libnss3"], "why": "a\nb"}),
        ),
    ] {
        let got = b.call("example", "skein_request_package", args);
        assert_eq!(got["error"], "why-must-be-text", "{what}: {got}");
    }
    assert_eq!(
        fs::read_dir(b.at("fleet/.skein/substrate/requests/example"))
            .unwrap()
            .count(),
        0,
        "a request with no why was filed"
    );
}

// ------------------------------------------------------------------------------------------------
// skein_clean (SKEIN-1356)
// ------------------------------------------------------------------------------------------------

/// An RFC3339 stamp `ago` seconds in the past, as skein writes them.
fn stamp_ago(ago: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(ago))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Box `web-main` under the fixture's fleet root, laid out as a real one is, with `$HOME` its own
/// home. Planted: a download cache holding a link to `outside/keep`, a build directory, an ended
/// session's scratchpad, the agent's own notes in `~/.cache` (not on the list), and the repo's
/// shared cache with cargo's credentials in it (never on the list).
struct Cleanable {
    root: std::path::PathBuf,
    cache: std::path::PathBuf,
    outside: std::path::PathBuf,
}

impl Cleanable {
    fn plant(b: &Box_) -> Cleanable {
        let root = b.at("fleet/web-main");
        let cache = b.at("home/repos/web/store/cache");
        let outside = b.at("outside/keep");
        for rel in [
            "fleet/web-main/home/.cache/pip/wheels/x.whl",
            "fleet/web-main/home/.cache/agent-notes/today.md",
            "fleet/web-main/home/.cache/skein/claude/claude-1000/-p/old/notes",
            "fleet/web-main/tree/target/debug/big",
            "outside/keep/precious",
            "home/repos/web/store/cache/pip/http/x",
            "home/repos/web/store/cache/cargo/registry/cache/x.crate",
            "home/repos/web/store/cache/cargo/credentials.toml",
        ] {
            b.put(rel, "data");
        }
        std::os::unix::fs::symlink(&outside, root.join("home/.cache/pip/evil")).unwrap();
        *b.home.borrow_mut() = Some(root.join("home"));
        Cleanable {
            root,
            cache,
            outside,
        }
    }

    fn pip(&self) -> String {
        self.root.join("home/.cache/pip").display().to_string()
    }

    /// skein's list for the box, in the signal file. `others` is (running, unknown) for the
    /// repo's other boxes, `quiet` when skein last saw none of them running.
    fn listed(
        &self,
        b: &Box_,
        box_root: &std::path::Path,
        others: (u64, u64),
        quiet: Option<String>,
    ) {
        self.listed_ago(b, box_root, others, quiet, 30)
    }

    /// [`Cleanable::listed`], with the list itself written `ago` seconds in the past.
    fn listed_ago(
        &self,
        b: &Box_,
        box_root: &std::path::Path,
        others: (u64, u64),
        quiet: Option<String>,
        ago: i64,
    ) {
        let item = |path: std::path::PathBuf, kind: &str| json!({"path": path.display().to_string(), "shown": "x", "kind": kind, "bytes": 4096});
        let file = json!({
            "at": stamp_ago(5),
            "disk": {"mb": 2048, "limit_mb": 10240, "fleet": "under", "fleet_over_by_mb": 0},
            "asks": [],
            "clean": {
                "at": stamp_ago(ago),
                "box_root": box_root.display().to_string(),
                "items": [
                    item(self.root.join("home/.cache/pip"), "download cache"),
                    item(self.root.join("tree/target"), "build output"),
                    item(self.root.join("home/.cache/skein/claude/claude-1000/-p/old"), "scratchpad"),
                ],
                "repo_cache": {
                    "path": self.cache.display().to_string(),
                    "bytes": 8192,
                    "items": [
                        item(self.cache.join("pip"), "download cache"),
                        item(self.cache.join("cargo/registry"), "download cache"),
                    ],
                    "others_running": others.0,
                    "others_unknown": others.1,
                    "none_running_at": quiet,
                },
            },
        });
        b.put("state/signals/resources.json", &file.to_string());
    }
}

fn clean_said(b: &Box_, paths: &[String]) -> (Value, String) {
    b.call_said("web-main", "skein_clean", json!({ "paths": paths }))
}

/// **A link planted inside a listed cache is removed as a link, and what it leads to is kept**
/// (SKEIN-1356). The same for a listed directory that has itself become a link since skein looked.
///
/// A box writes its own caches, so it can put a link to anything it can name inside one. A
/// removal that follows it empties the target as if it were part of the cache.
///
/// What would make it fail: walking with links followed — `os.stat(…, follow_symlinks=True)` in
/// `remove_tree_at` and `O_NOFOLLOW` dropped from its directory opens — which empties `outside/keep` through
/// `pip/evil`. Watched: with exactly that sabotage, `outside/keep/precious` was removed and this
/// failed.
#[test]
fn a_link_in_a_listed_cache_is_removed_and_its_target_kept() {
    let b = Box_::new("skein-tools-it-clean-link");
    let c = Cleanable::plant(&b);
    c.listed(&b, &c.root, (0, 0), Some(stamp_ago(10)));
    let (got, said) = clean_said(&b, &[c.pip()]);
    // First, so a removal that followed the link is named as that, whatever else it did.
    assert!(
        c.outside.join("precious").exists(),
        "the removal followed a link out of the cache: {got}"
    );
    assert_eq!(
        got["removed"].as_array().map(Vec::len),
        Some(1),
        "{got} {said}"
    );
    assert!(
        !c.root.join("home/.cache/pip").exists(),
        "the cache is still there"
    );
    assert!(said.starts_with("skein: removed 1,"), "{said}");

    // The listed directory itself is now a link to the outside.
    let target = c.root.join("tree/target");
    fs::remove_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&c.outside, &target).unwrap();
    let (got, _) = clean_said(&b, &[target.display().to_string()]);
    assert_eq!(got["removed"], json!([]), "{got}");
    assert_eq!(got["failed"].as_array().map(Vec::len), Some(1), "{got}");
    assert!(c.outside.join("precious").exists(), "{got}");
    assert!(
        fs::symlink_metadata(&target).is_ok(),
        "the link itself was removed: {got}"
    );
}

/// **A path that is not on skein's list is refused, and then nothing is removed**, not even the
/// listed paths asked for beside it (SKEIN-1356). That covers what the agent put in `~/.cache`
/// itself, and cargo's `credentials.toml` in the repo's cache.
///
/// What would make it fail: the tool removing a path on the agent's word (the notes go), or
/// removing what is listed and skipping the rest (the pip cache goes with the refusal). Watched:
/// with the `if missing:` refusal removed, the listed pip cache was removed and this failed.
#[test]
fn a_path_not_on_the_list_is_refused_and_nothing_is_removed() {
    let b = Box_::new("skein-tools-it-clean-list");
    let c = Cleanable::plant(&b);
    c.listed(&b, &c.root, (0, 0), Some(stamp_ago(10)));
    for stray in [
        c.root.join("home/.cache/agent-notes"),
        c.cache.join("cargo/credentials.toml"),
        c.cache.join("cargo"),
        c.outside.clone(),
    ] {
        let asked = vec![c.pip(), stray.display().to_string()];
        let (got, said) = clean_said(&b, &asked);
        assert_eq!(got["error"], "not-on-the-list", "{stray:?}: {got}");
        assert!(said.starts_with("skein: removed nothing."), "{said}");
        assert!(stray.exists(), "{stray:?} was removed");
        assert!(
            c.root.join("home/.cache/pip/wheels/x.whl").exists(),
            "{stray:?}: {got}"
        );
    }
    // The list itself never names them.
    let listing = b.call("web-main", "skein_clean", json!({}));
    let text = listing.to_string();
    assert!(
        !text.contains("agent-notes") && !text.contains("credentials"),
        "{listing}"
    );
}

/// A process that takes an exclusive `flock` on the file it is given and holds it itself, as cargo
/// does. Not `flock(1)` handing its descriptor to a child: `/proc/locks` lists a lock under the pid
/// that took it, and hides it once that pid is gone, so a lock `flock(1)` took and then left to
/// `sleep` is held and not listed (measured: `fdinfo` showed it under pid 0).
const HOLD_A_LOCK: &str = "import fcntl, sys, time\n\
                           f = open(sys.argv[1], 'w')\n\
                           fcntl.flock(f, fcntl.LOCK_EX)\n\
                           time.sleep(60)\n";

/// A child holding an exclusive `flock` on a file, as cargo holds its `.cargo-lock`, killed and
/// reaped however the test ends.
struct Holder(std::process::Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `path` as `/proc/locks` names a file: the device's major and minor in hex, then the inode.
fn proc_locks_name(path: &std::path::Path) -> String {
    use std::os::unix::fs::MetadataExt;
    let dev = fs::metadata(path).unwrap().dev();
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xffff_f000);
    let minor = (dev & 0xff) | ((dev >> 12) & 0xffff_ff00);
    format!(
        "{major:02x}:{minor:02x}:{}",
        fs::metadata(path).unwrap().ino()
    )
}

/// The real `/proc/locks` once it does (or does not) list `name`, within ten seconds.
fn proc_locks_when(name: &str, listed: bool) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let text = fs::read_to_string("/proc/locks").unwrap_or_default();
        let has = text.split_whitespace().any(|w| w == name);
        if has == listed {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "/proc/locks never {} {name}",
            if listed { "listed" } else { "stopped listing" }
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// **A build directory skein listed as not running is refused when a build holds its cargo lock
/// at the moment of the call, and removed once the lock is gone** (SKEIN-1391). The list can be a
/// minute old; a build started since is as running as one skein saw.
///
/// The lock is real: a child process takes an exclusive `flock` on
/// `tree/target/debug/.cargo-lock` and holds it ([`HOLD_A_LOCK`]), and the real `/proc/locks`, read while it holds it and asserted
/// to list it, is what the server reads as the box's (the fake root stands in for `/`).
///
/// What would make it fail: skein-mcp's listing believing the list's `live` alone and dropping
/// its recheck of `/proc/locks` (the target is removed under the build), or a recheck that refuses
/// whatever the lock says (the second call is refused too). Watched: with the recheck removed from
/// the listing's build-output branch, the first call removed the target and this failed.
#[test]
fn a_build_running_at_the_call_is_refused_though_the_list_says_it_is_not() {
    let b = Box_::new("skein-tools-it-clean-live");
    let c = Cleanable::plant(&b);
    c.listed(&b, &c.root, (0, 0), Some(stamp_ago(10)));
    let target = c.root.join("tree/target");
    let lock = target.join("debug/.cargo-lock");
    b.put("fleet/web-main/tree/target/debug/.cargo-lock", "");
    let name = proc_locks_name(&lock);
    let listed = fs::read_to_string(b.at("state/signals/resources.json")).unwrap();
    assert!(
        !listed.contains("\"live\":true"),
        "the list must say the build is not running: {listed}"
    );

    let mut holder = Holder(
        Command::new("python3")
            .arg("-c")
            .arg(HOLD_A_LOCK)
            .arg(&lock)
            .stdin(Stdio::null())
            .spawn()
            .expect("python3"),
    );
    b.put("root/proc/locks", &proc_locks_when(&name, true));
    let listing = b.call("web-main", "skein_clean", json!({}));
    let row = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "build output")
        .cloned()
        .unwrap_or_else(|| panic!("no build output listed: {listing}"));
    assert_eq!(row["safe_now"], false, "{row}");
    assert_eq!(row["why_not"], "a build is running in it now", "{row}");
    assert_eq!(
        listing["removable_now_bytes"], 12288,
        "everything listed but the running build and the scratchpad can be removed now: {listing}"
    );
    let (got, said) = clean_said(&b, &[target.display().to_string()]);
    assert_eq!(got["error"], "not-safe-now", "{got}");
    assert!(said.contains("a build is running in it now"), "{said}");
    assert!(
        lock.exists(),
        "the target was removed under a running build"
    );

    let _ = holder.0.kill();
    let _ = holder.0.wait();
    b.put("root/proc/locks", &proc_locks_when(&name, false));
    let (got, _) = clean_said(&b, &[target.display().to_string()]);
    assert_eq!(got["removed"].as_array().map(Vec::len), Some(1), "{got}");
    assert!(!target.exists(), "the build ended and its target was kept");
}

/// **A lock on another filesystem's file with the target's inode number is not a build in it**
/// (SKEIN-1391): skein-mcp's recheck at the call matches `/proc/locks` by device and inode
/// together, so the target is removed.
///
/// What would make it fail: the recheck matching the inode alone (the target is refused as
/// running). Watched: with the match made `any(st.st_ino == l[2] for l in locks)`, the call was
/// refused and this failed.
#[test]
fn a_lock_with_the_targets_inode_on_another_device_does_not_refuse_it() {
    use std::os::unix::fs::MetadataExt;
    let b = Box_::new("skein-tools-it-clean-otherdev");
    let c = Cleanable::plant(&b);
    c.listed(&b, &c.root, (0, 0), Some(stamp_ago(10)));
    let target = c.root.join("tree/target");
    let lock = target.join("debug/.cargo-lock");
    b.put("fleet/web-main/tree/target/debug/.cargo-lock", "");
    let ino = fs::metadata(&lock).unwrap().ino();
    let own = proc_locks_name(&lock);
    let other = format!("ff:ff:{ino}");
    assert_ne!(own, other, "the fixture's own device is ff:ff");
    b.put(
        "root/proc/locks",
        &format!("1: FLOCK  ADVISORY  WRITE 4242 {other} 0 EOF\n"),
    );
    let (got, said) = clean_said(&b, &[target.display().to_string()]);
    assert_eq!(
        got["removed"].as_array().map(Vec::len),
        Some(1),
        "{got} {said}"
    );
    assert!(
        !target.exists(),
        "a lock on another device's file kept the target"
    );
}

/// **The repo's shared cache is refused while another box of the repo runs, while skein cannot
/// tell, and when skein's word that none runs is stale or missing; and removed when it is fresh**
/// (SKEIN-1356). The host's rule for its own clean (`fleet::clean_repo_cache`), applied by a box
/// that cannot see its siblings.
///
/// What would make it fail: ignoring `others_running` or `others_unknown`, or dropping the
/// freshness check on `none_running_at` (the cache is then removed on a word ten minutes old).
/// Watched: with `or quiet > SIBLINGS_FRESH_SECONDS` removed, the stale case removed the cache and
/// this failed.
#[test]
fn the_shared_cache_is_refused_while_a_sibling_runs_or_the_word_is_stale() {
    let b = Box_::new("skein-tools-it-clean-shared");
    let c = Cleanable::plant(&b);
    let pip = c.cache.join("pip").display().to_string();
    for (others, quiet, why) in [
        (
            (1, 0),
            Some(stamp_ago(600)),
            "1 other box of this repo is running",
        ),
        (
            (0, 2),
            Some(stamp_ago(600)),
            "cannot tell whether 2 other boxes",
        ),
        ((0, 0), Some(stamp_ago(600)), "has not confirmed"),
        ((0, 0), None, "has not confirmed"),
    ] {
        c.listed(&b, &c.root, others, quiet.clone());
        let (got, said) = clean_said(&b, std::slice::from_ref(&pip));
        assert_eq!(got["error"], "not-safe-now", "{others:?} {quiet:?}: {got}");
        assert!(said.contains(why), "{others:?} {quiet:?}: {said}");
        assert!(c.cache.join("pip/http/x").exists(), "{others:?} {quiet:?}");
        let listing = b.call("web-main", "skein_clean", json!({}));
        assert_eq!(
            listing["shared_repo_cache"]["safe_to_clean_now"], false,
            "{listing}"
        );
    }
    c.listed(&b, &c.root, (0, 0), Some(stamp_ago(20)));
    let (got, said) = clean_said(&b, std::slice::from_ref(&pip));
    assert_eq!(got["removed"], json!([pip]), "{got} {said}");
    assert!(!c.cache.join("pip").exists());
    assert!(c.cache.join("cargo/credentials.toml").exists());
    assert!(c.cache.join("cargo/registry/cache/x.crate").exists());
}

/// **skein's paths are mapped to where the box sees them, and a list naming a directory that is
/// not this box's is refused whole** (SKEIN-1356).
///
/// The list's paths are the fleet's. The box's home is the fleet's `<box>/home` bound at `$HOME`,
/// so the tool shows `$HOME/.cache/pip` and takes that path back as the same item; and it believes
/// a list only when `<box_root>/home` is one directory with `$HOME`.
///
/// What would make it fail: the plugin skipping its inode check (the list naming `other-main`
/// is then acted on), or `in_box` not mapping the home (the `$HOME` path is refused as not on the
/// list). Watched: with that inode check answering True, `other-main`'s pip cache was removed and this
/// failed.
#[test]
fn paths_are_mapped_to_the_boxs_view_and_a_foreign_list_is_refused() {
    let b = Box_::new("skein-tools-it-clean-map");
    let c = Cleanable::plant(&b);
    // A list for some other directory, with this box's items moved under it.
    let other = b.at("fleet/other-main");
    b.put("fleet/other-main/home/.cache/pip/wheels/y.whl", "data");
    let foreign = |p: String| p.replace("/web-main/", "/other-main/");
    c.listed(&b, &other, (0, 0), Some(stamp_ago(10)));
    let raw = fs::read_to_string(b.at("state/signals/resources.json")).unwrap();
    b.put("state/signals/resources.json", &foreign(raw));
    let (got, said) = clean_said(&b, &[other.join("home/.cache/pip").display().to_string()]);
    assert_eq!(got["error"], "not-safe-now", "{got}");
    assert!(said.contains("not this box's"), "{said}");
    assert!(other.join("home/.cache/pip/wheels/y.whl").exists());

    // `$HOME` as the box sees it: another name for the same directory, which is what the
    // launcher's bind of `<box>/home` at `$HOME` makes. A link stands in for the bind here.
    c.listed(&b, &c.root, (0, 0), Some(stamp_ago(10)));
    let home = b.at("boxhome");
    std::os::unix::fs::symlink(c.root.join("home"), &home).unwrap();
    *b.home.borrow_mut() = Some(home.clone());
    let listing = b.call("web-main", "skein_clean", json!({}));
    let pip_row = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "download cache")
        .cloned()
        .unwrap_or_else(|| panic!("{listing}"));
    let seen = format!("{}/.cache/pip", home.display());
    assert_eq!(pip_row["in_box"], json!(seen), "{listing}");
    let (got, said) = clean_said(&b, &[seen]);
    assert_eq!(
        got["removed"].as_array().map(Vec::len),
        Some(1),
        "{got} {said}"
    );
    assert!(!c.root.join("home/.cache/pip").exists());
}

/// **`skein_top` by disk shows what the box is charged for, by kind, and the repo's shared cache on
/// a line of its own that is not counted against the box** (SKEIN-1356).
///
/// What would make it fail: by-disk reading only the checkout (no `cleanable`), the shared cache
/// folded into the box's kinds, or its line claiming it counts against the box.
#[test]
fn top_by_disk_shows_the_box_and_the_shared_cache_apart() {
    let b = Box_::new("skein-tools-it-clean-top");
    let c = Cleanable::plant(&b);
    c.listed(&b, &c.root, (1, 0), Some(stamp_ago(600)));
    let top = b.call("web-main", "skein_top", json!({"by": "disk"}));
    assert_eq!(top["charged_mb"], 2048, "{top}");
    let kinds = &top["cleanable"]["bytes_by_kind"];
    assert_eq!(kinds["download cache"], 4096, "{top}");
    assert_eq!(kinds["build output"], 4096, "{top}");
    assert_eq!(kinds["scratchpad"], 4096, "{top}");
    assert!(kinds.get("shared download cache").is_none(), "{top}");
    let shared = &top["shared_repo_cache"];
    assert_eq!(shared["counted_against_this_box"], false, "{top}");
    assert_eq!(shared["shared_with"], "every box of this repo", "{top}");
    assert_eq!(shared["safe_to_clean_now"], false, "{top}");
    assert!(
        top["dirs"].is_array(),
        "the checkout's directories are gone: {top}"
    );
}

/// **`skein-mcp clean` on the command line is the same `skein_clean`**: one list, one set of
/// refusals, one removal, for a caller that is not an MCP client (SKEIN-1356, for the mod of
/// SKEIN-1358).
///
/// What would make it fail: the command line answering from a copy of the logic that drifts (a
/// path not on the list removed, or a listed one refused), or a refusal that exits 0.
#[test]
fn the_command_line_clean_is_the_same_tool() {
    let b = Box_::new("skein-tools-it-clean-cli");
    let c = Cleanable::plant(&b);
    c.listed(&b, &c.root, (0, 0), Some(stamp_ago(10)));
    let run = |paths: &[String]| {
        let out = Command::new("python3")
            .arg(b.at("skein-mcp"))
            .arg("clean")
            .args(paths)
            .env("SKEIN_STATE", b.at("state"))
            .env("HOME", c.root.join("home"))
            .output()
            .expect("python3");
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
        (out.status.code(), v)
    };
    let (code, listed) = run(&[]);
    assert_eq!(code, Some(0), "{listed}");
    assert_eq!(
        listed["result"]["items"].as_array().map(Vec::len),
        Some(5),
        "{listed}"
    );
    let notes = c.root.join("home/.cache/agent-notes").display().to_string();
    let (code, refused) = run(&[c.pip(), notes.clone()]);
    assert_eq!(code, Some(1), "{refused}");
    assert_eq!(refused["result"]["error"], "not-on-the-list", "{refused}");
    assert!(std::path::Path::new(&notes).exists());
    let (code, done) = run(&[c.pip()]);
    assert_eq!(code, Some(0), "{done}");
    assert!(
        done["said"]
            .as_str()
            .unwrap()
            .starts_with("skein: removed 1,"),
        "{done}"
    );
    assert!(!c.root.join("home/.cache/pip").exists());
}

/// **A list skein wrote more than fifteen minutes ago is acted on for nothing**, own items and the
/// shared cache alike, however fresh the word about the repo's other boxes (SKEIN-1356). skein
/// lists every five minutes, so a list that old means skein is not looking, and what it says may no
/// longer be true: a build directory may since have become tracked work.
///
/// What would make it fail: the plugin treating every list as fresh (`stale = False`), which
/// removes the own cache and the shared one from a list twenty minutes old. Watched: with exactly
/// that, the own-cache removal went ahead and this failed; with only the shared-cache branch's
/// stale refusal removed, the shared `pip` was removed and this failed on the second call.
#[test]
fn a_list_skein_wrote_long_ago_is_refused_for_everything() {
    let b = Box_::new("skein-tools-it-clean-stale");
    let c = Cleanable::plant(&b);
    // The sibling word is fresh; only the list is old.
    c.listed_ago(&b, &c.root, (0, 0), Some(stamp_ago(10)), 20 * 60);

    let (got, said) = clean_said(&b, &[c.pip()]);
    assert_eq!(got["error"], "not-safe-now", "{got} {said}");
    assert!(
        said.contains("has not listed this box's disk lately"),
        "{said}"
    );
    assert!(
        c.root.join("home/.cache/pip/wheels/x.whl").exists(),
        "{got}"
    );

    let shared = c.cache.join("pip").display().to_string();
    let (got, said) = clean_said(&b, std::slice::from_ref(&shared));
    assert_eq!(got["error"], "not-safe-now", "{got} {said}");
    assert!(said.contains("has not listed this cache lately"), "{said}");
    assert!(c.cache.join("pip/http/x").exists(), "{got}");

    let listing = b.call("web-main", "skein_clean", json!({}));
    let rows = listing["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{listing}"));
    assert_eq!(rows.len(), 5, "{listing}");
    for r in rows {
        assert_eq!(r["safe_now"], false, "{r}");
        if r["kind"] != "scratchpad" {
            assert!(
                r["why_not"]
                    .as_str()
                    .is_some_and(|w| w.contains("has not listed")),
                "{r}"
            );
        }
    }
    assert_eq!(listing["removable_now_bytes"], 0, "{listing}");
    assert_eq!(
        listing["shared_repo_cache"]["safe_to_clean_now"], false,
        "{listing}"
    );
}

// ------------------------------------------------------------------------------------------------
// The two doors of skein's clean, and of skein's requests (SKEIN-1362)
// ------------------------------------------------------------------------------------------------

/// The answers both doors of skein's clean are held to, shared with the hooks module's own test.
const CLEAN_CONTRACT: &str = include_str!("../src/plugin/tests/clean-contract.ts");

/// The contract's cases, read as the JSON that follows `CLEAN_CONTRACT = ` in its file.
fn contract_cases() -> Vec<Value> {
    let (_, json) = CLEAN_CONTRACT
        .split_once("export const CLEAN_CONTRACT = ")
        .expect("clean-contract.ts declares CLEAN_CONTRACT");
    let cases: Value = serde_json::from_str(json.trim()).expect("CLEAN_CONTRACT is JSON");
    cases.as_array().expect("CLEAN_CONTRACT is a list").clone()
}

/// A value with the fixture's directory written as `/fixture`, as the contract writes it.
fn as_contract(v: &Value, fixture: &str) -> Value {
    serde_json::from_str(&v.to_string().replace(fixture, "/fixture")).unwrap()
}

/// A fresh box with its list written, the contract's paths for it, and its directory.
fn contract_box(case: &Value) -> (Box_, Vec<String>, String) {
    let b = Box_::new("skein-tools-it-clean-contract");
    let c = Cleanable::plant(&b);
    c.listed(&b, &c.root, (0, 0), Some(stamp_ago(10)));
    let fixture = b.dir.path().display().to_string();
    let paths = case["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().replace("/fixture", &fixture))
        .collect();
    (b, paths, fixture)
}

/// **One input gives one answer at both doors of skein's clean**: the `skein_clean` MCP tool, and
/// `skein-mcp clean` on the command line, which the hooks module's `clean` tool runs
/// (docs/decisions/skein-is-a-mod.md, SKEIN-1362). Each is held to the answer
/// `src/plugin/tests/clean-contract.ts` writes down, and `band.test.tsx` holds the hooks module's
/// tool to the same answer given the command line's output, so the three cannot drift apart.
///
/// What would make it fail: a removal, a refusal or its sentence changed at one door and not the
/// other (the command line building its own answer rather than calling `clean`), a refusal that
/// exits 0, or the contract no longer saying what skein says. Watched: with the command line's
/// `said` replaced by "", this failed on the first case; with the success sentence's "about"
/// changed in `clean` alone, it failed on the MCP door against the contract.
#[test]
fn both_doors_of_clean_give_the_contracts_answer() {
    let cases = contract_cases();
    assert_eq!(cases.len(), 3, "the contract lost a case");
    for case in &cases {
        let wanted_error = case["result"].get("error").is_some();

        let (b, paths, fixture) = contract_box(case);
        let replies = b.talk(
            "web-main",
            &[json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                     "params":{"name":"skein_clean","arguments":{"paths":paths}}})],
        );
        let content = &replies[0]["result"]["content"];
        let result: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
        let said = content[1]["text"]
            .as_str()
            .unwrap_or("")
            .replace(&fixture, "/fixture");
        assert_eq!(
            as_contract(&result, &fixture),
            case["result"],
            "MCP: {result}"
        );
        assert_eq!(said, case["said"].as_str().unwrap(), "MCP");
        assert_eq!(replies[0]["result"]["isError"], json!(wanted_error), "MCP");

        // A fresh fixture: the MCP door's removal took the first one's cache.
        let (b, paths, fixture) = contract_box(case);
        let out = Command::new("python3")
            .arg(b.at("skein-mcp"))
            .arg("clean")
            .args(&paths)
            .env("SKEIN_STATE", b.at("state"))
            .env("HOME", b.at("fleet/web-main/home"))
            .output()
            .expect("python3");
        let line: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stderr)));
        assert_eq!(
            as_contract(&line, &fixture),
            json!({"result": case["result"], "said": case["said"]}),
            "the command line"
        );
        assert_eq!(out.status.code(), Some(if wanted_error { 1 } else { 0 }));
    }
}

/// **`skein-mcp requests` on the command line is the `skein_requests` tool**, for the band the
/// hooks module draws above the prompt (SKEIN-1362): the same rows, the same states, read the same
/// way, and only the inbox's word is an answer.
///
/// What would make it fail: the command line answering from anything but `requests` (a copy that
/// trusts the file's own `state`, which reads the box-written `granted` as granted), or exiting
/// non-zero on a list. Watched: with the door answering `{"requests": []}`, this failed.
#[test]
fn the_command_line_requests_is_the_same_tool() {
    let b = Box_::new("skein-tools-it-requests-cli");
    b.put(
        "fleet/.skein/substrate/requests/web-main/7.json",
        &json!({"id":"7","kind":"apt","packages":["jq"],"asked":"2026-09-24T10:00:00Z","state":"granted"})
            .to_string(),
    );
    b.put(
        "fleet/.skein/gitgate/requests/web-main/8.json",
        &json!({"id":"8","repo":"acme/site","reason":"docs","asked":"2026-09-24T10:01:00Z"})
            .to_string(),
    );
    b.answered(1, "write request 8, granted");
    let tool = b.call("web-main", "skein_requests", json!({}));
    let out = Command::new("python3")
        .arg(b.at("skein-mcp"))
        .arg("requests")
        .env("SKEIN_STATE", b.at("state"))
        .env("SKEIN_FLEET_ROOT", b.at("fleet"))
        .env("SKEIN_HOME", b.at("home"))
        .env("SKEIN_BOX", "web-main")
        .output()
        .expect("python3");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(line["result"], tool, "{line}");
    let states: Vec<&str> = tool["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, vec!["waiting", "granted"], "{tool}");
}

/// One tool call with `$SKEIN_STATE` at `state`, and the sentence after the result.
fn said_in(b: &Box_, state: &std::path::Path, tool: &str, args: Value) -> (Value, String) {
    let replies = b.talk_in(
        state,
        "example",
        &[json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                 "params":{"name":tool,"arguments":args}})],
    );
    let content = &replies[0]["result"]["content"];
    let json = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    (json, content[1]["text"].as_str().unwrap_or("").to_string())
}

/// A box whose state directory is skein's own (`$SKEIN_HOME/boxes/example`), with its package
/// drop-box made as the launcher makes it and a signal file saying it holds 12.6G of a 10G share.
fn disk_box(prefix: &str) -> (Box_, std::path::PathBuf) {
    let b = Box_::new(prefix);
    fs::create_dir_all(b.at("fleet/.skein/substrate/requests/example")).unwrap();
    let state = b.at("home/boxes/example");
    b.put(
        "home/boxes/example/signals/resources.json",
        &json!({"at": "2026-10-31T10:00:00Z", "disk": {"mb": 12902, "limit_mb": 10240,
                "fleet": "under", "fleet_over_by_mb": 0}, "asks": []})
        .to_string(),
    );
    (b, state)
}

/// **A box asks for a bigger disk allowance, a new ask replaces the old, and the owner's approval
/// raises the allowance at once and reaches the box** — in `skein_requests`, from the inbox, and
/// in its signal file, from the host's record.
///
/// What would make it fail: the tool writing anywhere but `disk-allowance` in the box's own package
/// drop-box, or keeping the old id on a new ask; `diskask::decide` not setting the allowance; the
/// `disk` queue missing from `skein_requests`, or its state read from the file rather than the
/// inbox; `disk_ask` missing from the signal file.
#[test]
fn a_disk_ask_approved_raises_the_allowance_and_the_box_is_told() {
    let _lock = common::env_lock();
    let (b, state) = disk_box("skein-tools-it-diskask");
    let mut env = common::env_pins();
    env.set("SKEIN_HOME", b.at("home"));
    env.set("SKEIN_FLEET_ROOT", b.at("fleet"));

    let (first, _) = said_in(
        &b,
        &state,
        "skein_request_disk",
        json!({"size": "15g", "why": "the index is 14G"}),
    );
    let (got, said) = said_in(
        &b,
        &state,
        "skein_request_disk",
        json!({"size": "20g", "why": "the index is 14G"}),
    );
    let id = got["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{got} {said}"))
        .to_string();
    assert_ne!(first["id"], got["id"], "a new ask kept the old id");
    assert_eq!(got["replaces"], first["id"], "{got}");
    assert_eq!(
        said,
        format!(
            "skein: asked the owner for a 20.0G disk allowance (ask {id}). The answer arrives in \
             your inbox, and skein_requests shows it. It replaces ask {}.",
            first["id"].as_str().unwrap()
        )
    );
    let filed: Value = serde_json::from_str(
        &fs::read_to_string(b.at("fleet/.skein/substrate/requests/example/disk-allowance"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (filed["id"].as_str(), filed["size"].as_str()),
        (Some(id.as_str()), Some("20g"))
    );

    let ask = skein::diskask::for_box("example").expect("the box's ask");
    assert_eq!((ask.mb, ask.state.as_str()), (20 * 1024, "waiting"));
    let (got, _) = said_in(&b, &state, "skein_requests", json!({}));
    assert_eq!(got["requests"][0]["queue"], "disk", "{got}");
    assert_eq!(got["requests"][0]["state"], "waiting", "{got}");

    skein::diskask::decide(&ask, true).expect("approved");
    assert_eq!(skein::fleet::box_disk_limit("example"), Some(20 * 1024));
    let (got, _) = said_in(&b, &state, "skein_requests", json!({}));
    assert_eq!(got["requests"][0]["id"], id.as_str(), "{got}");
    assert_eq!(got["requests"][0]["state"], "approved", "{got}");

    let reading = skein::resource_signal::Reading {
        at: chrono::Utc::now(),
        disk_mb: Some(12902),
        disk_limit_mb: skein::fleet::box_disk_limit("example"),
        fleet: skein::resource_signal::FleetDisk::NotAsked { over_by: 0 },
        cgroup: None,
        clean: None,
    };
    skein::resource_signal::write_box_with(
        &state,
        &reading,
        Vec::new(),
        skein::diskask::for_box("example").map(Into::into),
    )
    .unwrap();
    let (got, _) = said_in(&b, &state, "skein_resources", json!({}));
    assert_eq!(got["disk"]["limit_mb"], 20 * 1024, "{got}");
    assert_eq!(got["disk_ask"]["id"], id.as_str(), "{got}");
    assert_eq!(got["disk_ask"]["state"], "approved", "{got}");
}

/// **A denied ask changes nothing and the box is told.**
///
/// What would make it fail: a denial that sets the allowance, or one `skein_requests` does not
/// report as denied.
#[test]
fn a_disk_ask_denied_leaves_the_allowance_and_the_box_is_told() {
    let _lock = common::env_lock();
    let (b, state) = disk_box("skein-tools-it-diskdeny");
    let mut env = common::env_pins();
    env.set("SKEIN_HOME", b.at("home"));
    env.set("SKEIN_FLEET_ROOT", b.at("fleet"));
    let (got, said) = said_in(
        &b,
        &state,
        "skein_request_disk",
        json!({"size": "20g", "why": "the index is 14G"}),
    );
    assert_eq!(got["state"], "waiting", "{got} {said}");
    let ask = skein::diskask::for_box("example").expect("the box's ask");
    skein::diskask::decide(&ask, false).expect("denied");
    assert_eq!(skein::fleet::box_disk_limit("example"), Some(10 * 1024));
    let (got, _) = said_in(&b, &state, "skein_requests", json!({}));
    assert_eq!(got["requests"][0]["state"], "denied", "{got}");
}

/// **A size that is not a size, an ask for no more than the box has, and a reason that is not one
/// short line are refused at the tool, and nothing is filed.**
///
/// What would make it fail: the tool not parsing the size (or reading a bare number as GiB),
/// not comparing it with the signal file's allowance, or not capping the reason.
#[test]
fn a_bad_disk_ask_is_refused_at_the_tool() {
    let (b, state) = disk_box("skein-tools-it-diskbad");
    let long = "x".repeat(301);
    for (args, error, says) in [
        (json!({"size": "plenty", "why": "w"}), "size-is-not-a-size", "skein: \"plenty\" is not a size. Ask with one such as 20g or 20480m."),
        (json!({"size": "20", "why": "w"}), "size-is-not-a-size", "skein: \"20\" is not a size. Ask with one such as 20g or 20480m."),
        (json!({"size": "8g", "why": "w"}), "not-more", "skein: this box's allowance is already 10.0G, so 8.0G would not raise it. Ask for more than 10.0G."),
        (json!({"size": "10g", "why": "w"}), "not-more", "skein: this box's allowance is already 10.0G, so 10.0G would not raise it. Ask for more than 10.0G."),
        (json!({"size": "20g", "why": long}), "why-must-be-one-line", "skein: say why in one line of at most 300 bytes."),
        (json!({"size": "20g", "why": "two\nlines"}), "why-must-be-one-line", "skein: say why in one line of at most 300 bytes."),
    ] {
        let (got, said) = said_in(&b, &state, "skein_request_disk", args.clone());
        assert_eq!(got["error"], error, "{args} {got}");
        assert_eq!(said, says, "{args}");
    }
    assert!(
        !b.at("fleet/.skein/substrate/requests/example/disk-allowance")
            .exists(),
        "a refused ask was filed"
    );
}

/// **A box's own `state` in its disk ask is never the answer**: `approved` or `denied` written
/// into the file, with nothing in the inbox, reads as waiting.
///
/// What would make it fail: `requests` reading the disk ask's state from its file rather than
/// from `answer_for` over the inbox.
#[test]
fn a_disk_ask_the_box_says_is_decided_still_waits() {
    let (b, state) = disk_box("skein-tools-it-diskforged");
    for claimed in ["approved", "denied"] {
        b.put(
            "fleet/.skein/substrate/requests/example/disk-allowance",
            &json!({"id": "d-0000abab", "size": "20g", "why": "w", "state": claimed,
                    "asked": "2026-10-31T10:00:00Z"})
            .to_string(),
        );
        let (got, _) = said_in(&b, &state, "skein_requests", json!({}));
        assert_eq!(got["requests"][0]["queue"], "disk", "{got}");
        assert_eq!(got["requests"][0]["state"], "waiting", "{claimed}: {got}");
    }
}
