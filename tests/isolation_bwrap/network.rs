//! A box is never discoverable on a socket it cannot reach, and nothing but the socket
//! directory comes through the run cover.

use super::*;

// ---------------------------------------------------------------------------------------------
// The peer network — SKEIN-572
// ---------------------------------------------------------------------------------------------

/// The launcher's `/run` cover and the peer-network block that opens one named hole in it.
///
/// Lifted out of `box-session.sh` rather than copied, for the reason [`isolation_block`] is: a copy
/// keeps passing against the version it was written from, and this block's whole job is to hold two
/// decisions together that spent months drifting apart in that file.
///
/// Stops at the last line that touches `binds`, deliberately — the `SKEIN_PEERS` line after it
/// prints to stdout, which is where the probe's answers come back.
fn peer_network_block() -> String {
    let src = fs::read_to_string(script("box-session.sh")).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let from = lines
        .iter()
        .position(|l| l.starts_with("# --- /run:"))
        .expect("the /run cover moved");
    let to = lines
        .iter()
        .position(|l| l.starts_with(r#"[ "$peers" = "1" ] || binds+="#))
        .expect("nothing closes the socket directory when the peer network is off");
    assert!(
        to > from,
        "the peer network block moved above the /run cover"
    );
    lines[from..=to].join("\n")
}

/// A sandbox holding one peer that is **registered and listening**, and one box's view of it.
///
/// The two halves of Claude Code's session messaging, as files: a registration under
/// `~/.claude/sessions/` that `ListAgents` reads, and an inbox socket under the runtime directory's
/// `cc-socks/` that `SendMessage` connects to. Both are planted here the way a live peer would
/// leave them, so "can this box reach that peer" is asked of the kernel rather than of an argv.
struct Peers {
    dir: Scratch,
    /// The sandbox's own `$HOME`, shared by every box — where the session registry lives.
    sandbox_home: PathBuf,
    /// This box's private `$HOME`, bound over the path above the way the launcher binds it.
    box_home: PathBuf,
    /// `$SKEIN_RUNTIME_DIR` — `/run/user/<uid>` under the name that lets a test plant a file in it
    /// without writing into the live fleet's runtime directory beside real agents' sockets.
    runtime: PathBuf,
    /// Held open for the lifetime of the fixture: a socket nothing is listening on refuses
    /// `connect()` whatever the mounts say, which would make every transport answer "no" and every
    /// assertion below vacuous.
    _peer: std::os::unix::net::UnixListener,
}

impl Peers {
    fn make(tag: &str) -> Peers {
        let dir = Scratch::temp(&format!("skein-peers-{tag}"));
        let sandbox_home = dir.join("sandbox-home");
        let box_home = dir.join("box-home");
        let runtime = dir.join("runtime");
        for p in [
            sandbox_home.join(".claude/sessions"),
            box_home.join(".claude"),
            runtime.join("cc-socks"),
            // Something under the runtime directory that is NOT the socket directory. The cover
            // this block opens a hole in is only worth anything if the hole is the size of one
            // directory, and nothing here says so unless something else is there to stay hidden.
            runtime.join("some-daemon"),
        ] {
            fs::create_dir_all(&p).unwrap();
        }
        // The peer, as `ListAgents` would find it: a registration naming the socket it listens on.
        fs::write(
            sandbox_home.join(".claude/sessions/4242.json"),
            r#"{"pid":4242,"name":"other-main","messagingSocketPath":"cc-socks/4242.sock"}"#,
        )
        .unwrap();
        fs::write(
            runtime.join("some-daemon/state"),
            "a neighbour's runtime state\n",
        )
        .unwrap();
        fs::write(runtime.join("bus-socket-marker"), "not the peer network\n").unwrap();
        let peer = std::os::unix::net::UnixListener::bind(runtime.join("cc-socks/4242.sock"))
            .expect("a listening peer");
        Peers {
            sandbox_home,
            box_home,
            runtime,
            _peer: peer,
            dir,
        }
    }

    /// What this box can reach of that peer, and of the runtime directory around it.
    ///
    /// Four lines: `discovery`, `transport`, `neighbour` and `socket-dir`. `peers` is what the host
    /// puts in `$SKEIN_BOX_PEERS` — `None` for a host that never sets it, which must read as the
    /// default the switch ships with.
    fn seen(&self, privileged: bool, peers: Option<&str>) -> String {
        // python3 rather than sh: only a syscall can answer the transport half, and a directory
        // listing is exactly the wrong question — a path that resolves says nothing about whether
        // anything is listening at the other end of it.
        let probe = "import os,socket,sys\n\
                     reg, sock, neighbour, socks = sys.argv[1:5]\n\
                     print('discovery', 'yes' if os.path.isfile(reg) else 'no')\n\
                     s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n\
                     try:\n\
                     \x20   s.connect(sock); print('transport yes')\n\
                     except OSError as e:\n\
                     \x20   print('transport no-' + e.__class__.__name__)\n\
                     print('neighbour', 'yes' if os.path.isfile(neighbour) else 'no')\n\
                     print('socket-dir', 'yes' if os.path.isdir(socks) else 'no')\n";
        let args = [
            self.sandbox_home
                .join(".claude/sessions/4242.json")
                .to_string_lossy()
                .into_owned(),
            self.runtime
                .join("cc-socks/4242.sock")
                .to_string_lossy()
                .into_owned(),
            self.runtime
                .join("some-daemon/state")
                .to_string_lossy()
                .into_owned(),
            self.runtime.join("cc-socks").to_string_lossy().into_owned(),
        ];
        let quoted: Vec<String> = args
            .iter()
            .map(|a| skein::util::sh_quote(a))
            .collect::<Vec<_>>();
        let runner = format!(
            "set -uo pipefail\n\
             export HOME={home}\n\
             {peers}\
             export SKEIN_BOX_PRIVILEGED={priv} SKEIN_RUNTIME_DIR={rt}\n\
             binds=(--bind {boxhome} {home})\n\
             {block}\n\
             exec bwrap --dev-bind / / ${{binds[@]+\"${{binds[@]}}\"}} -- \
             /bin/sh -c 'exec python3 -c \"$1\" \"$2\" \"$3\" \"$4\" \"$5\"' skein-probe {probe} {args}\n",
            // The launcher's own `--bind "$home" "$HOME"` (`grep -n 'binds=(--bind' box-session.sh`),
            // reproduced here for the reason `seen_by_box` reproduces the conversation bind: the
            // block under test binds two paths BACK through this one, so testing it without the
            // private HOME would assert that a box can read the shared registry — which is true of
            // every path in the sandbox and proves nothing about the peer network.
            home = skein::util::sh_quote(self.sandbox_home.to_string_lossy().as_ref()),
            boxhome = skein::util::sh_quote(self.box_home.to_string_lossy().as_ref()),
            rt = skein::util::sh_quote(self.runtime.to_string_lossy().as_ref()),
            priv = if privileged { "1" } else { "0" },
            peers = match peers {
                None => String::new(),
                Some(v) => format!("export SKEIN_BOX_PEERS={}\n", skein::util::sh_quote(v)),
            },
            block = peer_network_block(),
            probe = skein::util::sh_quote(probe),
            args = quoted.join(" "),
        );
        let out = Command::new("bash")
            .arg("-c")
            .arg(&runner)
            .output()
            .expect("bash");
        assert!(
            out.status.success(),
            "the namespace could not be built: {}\n--- script ---\n{runner}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = &self.dir;
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}

/// One answer out of a [`Peers::seen`] report.
pub(super) fn answer<'a>(report: &'a str, key: &str) -> &'a str {
    report
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("the probe said nothing about {key}:\n{report}"))
}

/// **A box is never discoverable on a socket it cannot reach** (SKEIN-572, architecture §9.5 R11).
///
/// The invariant, not the flag: whatever `$SKEIN_BOX_PEERS` says and whichever side of the
/// privileged switch a box is on, the registry that advertises it and the socket that serves it are
/// reachable together or not at all. Half-open is the state this fleet was actually in — six
/// sessions in the shared registry, every one advertising a socket, exactly one resolving — and it
/// is worse than either whole state, because a sender addresses a box the registry says is live,
/// the message goes out through Anthropic's servers or nowhere, and nothing tells either end.
///
/// **What would make this fail.** Dropping the `cc-socks` bind gives discovery without transport,
/// which is precisely what the `--tmpfs` over `/run/user/<uid>` did on the day it landed and what
/// no test here noticed for months. Dropping the `.claude/sessions` bind gives the mirror image.
/// Either one breaks the `assert_eq!` below; nothing else in this file would have.
#[test]
fn a_box_is_never_discoverable_on_a_socket_it_cannot_reach() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so the peer network's two halves were \
             NOT checked against each other",
        );
    }
    let f = Peers::make("invariant");
    // `None` is the host that never sets the variable, which must read as the shipped default —
    // an old host must not silently take a working fleet off its own peer network.
    for privileged in [false, true] {
        for (setting, want_on) in [
            (None, true),
            (Some("1"), true),
            (Some("0"), false),
            (Some("off"), false),
        ] {
            let report = f.seen(privileged, setting);
            let discovery = answer(&report, "discovery") == "yes";
            let transport = answer(&report, "transport") == "yes";
            assert_eq!(
                discovery, transport,
                "SKEIN_BOX_PEERS={setting:?} privileged={privileged} leaves the peer network \
                 half-open: discovery and transport must move together, and a box that is \
                 advertised but unreachable is the failure this asserts against:\n{report}"
            );
            assert_eq!(
                discovery, want_on,
                "SKEIN_BOX_PEERS={setting:?} privileged={privileged} put the box on the wrong \
                 side of its own switch:\n{report}"
            );
        }
    }
}

/// **The cover that remains: one hole, the size of one directory** (architecture §9.5 R11).
///
/// Opening `cc-socks` is only safe if the next path under `/run/user/<uid>` cannot be opened by
/// accident, and until now that cover had no test at all — `isolation_block` stops at the mount
/// cover, hundreds of lines above this one. So the most open configuration is the one asserted
/// against: peers ON, the hole at its widest, and a neighbour's runtime state still gone.
///
/// **What would make this fail.** Binding `$runtime_dir` back instead of `$runtime_dir/cc-socks`,
/// or dropping the `--tmpfs` and relying on the bind alone — both leave `neighbour` readable, and
/// both are the shape of edit somebody makes while fixing a socket that will not connect.
#[test]
fn nothing_but_the_socket_directory_comes_through_the_run_cover() {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so what the /run cover still hides was \
             NOT checked",
        );
    }
    let f = Peers::make("containment");

    let open = f.seen(false, Some("1"));
    assert_eq!(
        answer(&open, "transport"),
        "yes",
        "the hole is not open at all, so what it does not reach proves nothing:\n{open}"
    );
    assert_eq!(
        answer(&open, "neighbour"),
        "no",
        "the peer network's bind carried the whole runtime directory through the cover, not just \
         the socket directory:\n{open}"
    );

    // And with the switch off, the socket directory itself is a private tmpfs rather than a hole:
    // the box gets a directory of its own, so its own agent still starts, and nothing in it is
    // anybody else's.
    let shut = f.seen(false, Some("0"));
    assert_eq!(
        answer(&shut, "socket-dir"),
        "yes",
        "an isolated box has no socket directory at all, so its own agent cannot open an inbox:\n{shut}"
    );
    // `FileNotFoundError` and not a permission error, which is the launcher's own argument for a
    // tmpfs over a `--ro-bind` of an empty directory: read-only refuses nothing to a socket, so the
    // cover has to make the NAME absent rather than the connection refused. An errno saying the
    // path was still there would mean the peer's socket was reachable and merely unserved.
    assert_eq!(
        answer(&shut, "transport"),
        "no-FileNotFoundError",
        "an isolated box still reached a peer's inbox socket:\n{shut}"
    );
    assert_eq!(answer(&shut, "neighbour"), "no", "{shut}");
}
