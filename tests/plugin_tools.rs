//! skein's in-box tools, the plugin's MCP server (box-plugin §2.3, SKEIN-1059), driven over stdio
//! the way an agent's CLI drives it, against a fake `/proc`, `/sys` and fleet root.
//!
//! The server is the shipped bytes — `include_str!` of the file `runtime::PLUGIN_FILES` installs —
//! run by `python3` with a JSON-RPC conversation on stdin. Nothing here reads the real `/proc`.
//!
//! The two tests the item names are the first two: `top` never names a pid outside the box's own
//! cgroup, and a `state` a box wrote into its own request is never an answer.

mod common;

use common::Scratch;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

const SERVER: &str = include_str!("../src/plugin/bin/skein-mcp");

/// A fake machine: `root` stands in for `/`, `state` is the box's state directory, `fleet` its
/// fleet root, and the server script sits beside them.
struct Box_ {
    dir: Scratch,
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
        Box_ { dir }
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
        let mut child = Command::new("python3")
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
/// lists exactly its three read-only tools, and answers an unknown method with an error rather
/// than exiting.
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
        vec!["skein_resources", "skein_top", "skein_requests"]
    );
    for t in replies[1]["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{t}");
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
    let allowed = ["json", "os", "re", "subprocess", "sys", "time"];
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
    // The one subprocess is `du`, over the box's own checkout.
    assert_eq!(SERVER.matches("subprocess.run(").count(), 1);
    assert!(SERVER.contains(r#"["du", "-x", "-k", "--max-depth=1", where]"#));
}
