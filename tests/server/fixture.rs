//! The real server this suite starts and how it is stopped: the socket it is handed, the
//! wait until it answers, the doorway supervisor's teardown, and the plain HTTP client every
//! test speaks to it with.

use super::*;

/// A port number nothing of ours is listening on.
///
/// **This is not how a server in this file gets its port** — [`serving`] is, and the difference is
/// SKEIN-526. Binding `127.0.0.1:0`, reading the number back and dropping the listener leaves that
/// number unbound from the instant it is returned until the child reaches `bind`, which is hundreds
/// of milliseconds later — `main` runs `ensure_probe_all`, `ensure_fleet_kit` and `heal_fleet`
/// first. It had fourteen call sites here: thirteen were the address of a spawned server, and
/// eleven of those servers went on to bind it, all of them racing for ephemeral ports inside that
/// window. Reproduced on demand rather than waited for, by binding the returned number from the test itself: the child
/// died with `skein-server: cannot bind 127.0.0.1:41713: Address already in use`, the
/// `while TcpStream::connect(&addr).is_err()` wait loop was satisfied **in 869µs** by the other
/// listener, and the first request after it failed with `Connection refused (os error 111)` — the
/// failure the item was filed from.
///
/// It survives at a call site that wants the opposite of a server: an address where *nothing*
/// answers, so that "no warden is running" is a state the test reaches rather than one it inherits
/// from whatever the machine happens to be running. Nothing there is racing to bind it, and a
/// sibling that took the number would still leave that test asserting what our server printed about
/// the address it tried.
///
/// And at one more, which is not a server of this file's own spawning:
/// `no_process_the_server_starts_holds_the_cockpits_listening_socket` hands its numbers to the real
/// doorway, which binds for itself and retries `EADDRINUSE` for ten seconds before refusing by name.
pub(super) fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

// `dup`, `dup2` and `close`, declared rather than added to the manifest: they are three lines of the
// libc every Rust binary already links, against a dev-dependency in a workspace manifest two other
// authors are editing. All three are async-signal-safe, which is the whole of what `pre_exec`
// requires of what runs inside it.
extern "C" {
    fn dup(oldfd: RawFd) -> RawFd;
    fn dup2(oldfd: RawFd, newfd: RawFd) -> RawFd;
    fn close(fd: RawFd) -> i32;
}

/// Put `fd` on descriptor [`FIRST`] with `CLOEXEC` cleared — in the child, between fork and exec.
///
/// A duplicate **is** the clearing: neither `dup` nor `dup2` copies `CLOEXEC`, which is why this
/// needs no `fcntl`. `dup` first rather than `dup2(fd, FIRST)` on its own, because the listener this
/// process just opened is very often descriptor 3 already — it is the first one a test binary has
/// free — and `dup2(3, 3)` is defined to do *nothing at all*, the flag included. That path would
/// exec a server with no socket on fd 3 and no error anywhere to say so.
fn on_the_first_descriptor(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: between fork and exec, in a child with one thread. `dup` returns a descriptor this
    // process owns and nothing else has seen; `close` is called only on that copy, never on `fd`
    // itself, which the parent still owns.
    unsafe {
        let copy = dup(fd);
        if copy < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if copy != FIRST {
            if dup2(copy, FIRST) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            close(copy);
        }
    }
    Ok(())
}

/// Spawn a server **holding the socket it will serve on**, and give back the address it is on.
///
/// This is the SKEIN-526 fix, and it is a fix by construction rather than a narrower window: the
/// port is claimed here, by this process, and from the fork onwards the child holds a duplicate of
/// that same listening socket. There is no instant between the bind and the serve at which the
/// number is free, so there is nothing for a sibling to take — and no `bind` in the child to lose,
/// because `SKEIN_LISTEN_INHERITED_ONLY=1` makes a missing descriptor a startup failure rather than
/// a reason to bind one (`skein::doorway::inherited_only`, asserted by
/// `told_the_socket_comes_from_outside_and_given_none_the_server_refuses_to_bind` below).
///
/// It is also the shape the fleet actually starts a server in — `src/server-doorway.py:213` sets
/// the same `LISTEN_FDS=1` on the same descriptor — so every server this file spawns through here
/// or through [`serving`] now exercises the production start, the way only the doorway's own spawn
/// used to. That is the line in the doorway's `spawn`, which is the one that starts a *server*; the
/// cite here used to be `:186`, which is the same assignment in `reexec`, where the doorway
/// replaces its own image.
///
/// **No count of spawns here, on purpose** (SKEIN-993). An earlier version of this paragraph said
/// "the thirteen spawns in this file now exercise the production start"; the file grew and the
/// number did not move with it, so it went on saying thirteen once there were eighteen and nothing
/// noticed — nothing read it. The property the paragraph needs is that `handed_with`, below, sets
/// `LISTEN_FDS` unconditionally on whatever it is given, which does not get truer or falser as
/// tests are added or removed, so there is nothing here for a number to protect.
///
/// `LISTEN_PID` is removed rather than set: it is the half of the convention that names the process
/// the descriptors are for, and this side of the fork there is no pid to name. `descriptor` accepts
/// its absence deliberately (`src/doorway.rs:251`, and
/// `one_descriptor_is_the_one_the_convention_names` asserts it).
pub(super) fn handed(cmd: &mut Command) -> (Child, String) {
    handed_with(cmd, TheDoorwaysPin::Set)
}

/// Whether the spawn carries `SKEIN_LISTEN_INHERITED_ONLY=1` — which is what the fleet's doorway
/// sets on its child and what nothing else in the tree sets (`src/server-doorway.py`'s `spawn`).
///
/// It is the difference between the two starts, and `src/doorway.rs`'s own note says so: in-fleet a
/// missing descriptor means the start sequence did not do its job, host-driven there is nobody
/// upstream to have opened a socket. SKEIN-962 makes the same variable decide whether
/// `$SKEIN_NO_API_AUTH` is honoured, so a test of that needs to spawn both ways.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum TheDoorwaysPin {
    Set,
    Unset,
}

impl TheDoorwaysPin {
    /// Which of the two starts this is, in words, for a failure message.
    ///
    /// The two differ only by an environment variable, so a panic that named neither left the
    /// reader to guess which spawn of a test that makes both had failed — which is what
    /// `the_auth_off_switch_is_refused_under_the_fleets_doorway_and_honoured_outside_it` did when
    /// SKEIN-989 hit it.
    fn said(self) -> &'static str {
        match self {
            TheDoorwaysPin::Set => "the way the fleet's doorway starts one",
            TheDoorwaysPin::Unset => "the way a host starts one, outside the fleet's doorway",
        }
    }
}

/// [`handed`], with a say in whether the doorway's own declaration rides along.
///
/// Both shapes still take the socket from this process, so neither races for a port: the descriptor
/// is what makes SKEIN-526 impossible, and the pin is only what the child is *told* about where it
/// came from.
fn handed_with(cmd: &mut Command, pin: TheDoorwaysPin) -> (Child, String) {
    // **One `Command`, one hand-over — and a refusal rather than a comment** (SKEIN-989).
    //
    // `CommandExt::pre_exec` *stacks*: registering a second closure does not replace the first, and
    // the child runs every closure it carries, in order, before it execs. Measured rather than
    // recalled — two closures on one `Command`, each writing its own name to descriptor 2, print
    // both. So a `Command` spawned twice through here takes the FIRST call's closure into the
    // SECOND call's child, still naming the listener this function dropped at the end of the first
    // call: a descriptor this process no longer owns.
    //
    // What that does next depends on what took the number in the meantime, which is why it reads as
    // load rather than as a bug:
    //
    //   * nothing took it — the stale `dup` fails with `EBADF`, and `spawn` hands that back with no
    //     message anywhere saying which descriptor, or whose, or that there were two;
    //   * something took it — the stale closure puts *that* on descriptor [`FIRST`], closing what
    //     was already there. When what was already there is this call's own listener (it lands on
    //     descriptor 3 whenever 3 is free) this call's closure then duplicates the stale object
    //     instead, and the server refuses a descriptor that is not a listening socket and exits 1.
    //
    // Both were reproduced against the unfixed file: 6 failures in 40 executed whole-binary runs
    // under 16 busy loops on 11 CPUs, split between `EBADF` out of `spawn` and "the server exited
    // (exit status: 1) without serving the socket it was handed". And it passed 25 of 25 alone,
    // because alone the freed number is taken straight back — both listeners landed on descriptor 3
    // every time, so the stale closure duplicated the right socket by coincidence.
    //
    // `LISTEN_FDS` is the marker because it is already the fact: the line below is the only place
    // in this file that sets it on a `Command`, so finding it on one that arrives here means this
    // `Command` has been handed a socket before.
    assert!(
        !cmd.get_envs()
            .any(|(key, _)| key == OsStr::new("LISTEN_FDS")),
        "the `Command` for {:?} has already been handed a listening socket once, and spawning it \
         again would carry the first hand-over's `pre_exec` closure — and the descriptor this \
         process dropped after that spawn — into the second child. Build a fresh `Command` per \
         spawn; `pre_exec` closures stack rather than replace (SKEIN-989)",
        cmd.get_program()
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let addr = listener
        .local_addr()
        .expect("the listener knows its address")
        .to_string();
    let fd = listener.as_raw_fd();
    // SAFETY: the closure runs between fork and exec and calls nothing but `dup`, `dup2` and
    // `close`. `fd` is valid for the whole of `spawn`, which is what runs it — the parent's copy is
    // dropped below, after it has returned.
    unsafe {
        cmd.pre_exec(move || on_the_first_descriptor(fd));
    }
    cmd.env("LISTEN_FDS", "1").env_remove("LISTEN_PID");
    match pin {
        TheDoorwaysPin::Set => cmd.env(INHERITED_ONLY, "1"),
        TheDoorwaysPin::Unset => cmd.env_remove(INHERITED_ONLY),
    };
    // The box launcher's marker goes in both shapes (SKEIN-1086): it also refuses
    // `$SKEIN_NO_API_AUTH`, and it is ambient when this suite runs inside a box, so leaving it would
    // make "outside the fleet's doorway" mean "outside the doorway, unless you are in a box".
    cmd.env_remove(skein::apiauth::IN_BOX);
    // Not `.expect("the server binary spawned")`. The only thing this spawn does between fork and
    // exec is move one descriptor, so a failure here is about that descriptor and nothing else —
    // and the reader needs to be told which one, and which of the two starts was being made. Only
    // the errno crosses the fork: `pre_exec` reports through a pipe that carries
    // `raw_os_error()` and not a message, so this sentence can only be written on this side of it.
    let child = cmd.spawn().unwrap_or_else(|e| {
        panic!(
            "the server was not started {}: {e}. All this spawn had to do between fork and exec \
             was put the listener on {addr} — descriptor {fd} in this process — onto descriptor \
             {FIRST} in the child, so a bad descriptor here is that one (SKEIN-989)",
            pin.said()
        )
    });
    // The parent's copy goes and the child's stays, so from here the server is the *sole* holder of
    // the socket. That is deliberate and it is about failing fast: were this process to keep a copy,
    // a server that died would leave a listener nobody accepts on, `connect` would keep succeeding
    // into its backlog, and every request would hang instead of being refused.
    drop(listener);
    (child, addr)
}

/// How long a spawned server gets to answer on the socket it was handed.
///
/// A ceiling on liveness, not a tolerance: [`until_it_answers`] returns the moment a byte arrives
/// and fails the moment the child exits, so nothing reaches this number unless the server is
/// genuinely stuck. The loops it replaces gave a bind 15 to 20 seconds, which on the failing path
/// they spent waiting for something that had already happened in another process.
const BOOT: Duration = Duration::from_secs(20);

/// Block until the server answers on `addr` — or say what it did instead.
///
/// **Not `while TcpStream::connect(&addr).is_err()`**, which is what this replaces at every spawn
/// below, and which cannot tell our server from anybody else's: with a handed socket it is answered
/// by the kernel before the child has even exec'd, and with a bound one it was answered by the
/// sibling that had taken the port (SKEIN-526). Three things make this one fail faster and for the
/// right reason:
///
/// * it waits for a **response**, not a connection — the accept loop has run and the server is
///   serving, which is what every caller below actually needs before it measures anything;
/// * it asks `try_wait` on every turn, so a child that died in its start-up sequence fails the test
///   in milliseconds, with its exit status, rather than spinning out the full ceiling;
/// * it asks `try_wait` **after a successful answer too**. A byte on a port our own child is no
///   longer alive to have sent came from another process, which is the whole of SKEIN-526 — and
///   this is the one place in the file that could ever see it happen.
pub(super) fn until_it_answers(child: &mut Child, addr: &str) {
    let start = Instant::now();
    loop {
        let left = BOOT
            .checked_sub(start.elapsed())
            .filter(|left| !left.is_zero());
        let Some(left) = left else {
            panic!(
                "the server was handed a socket on {addr} and had not answered on it in {BOOT:?}"
            )
        };
        let answered = first_byte(addr, left);
        let gone = child.try_wait().expect("the spawned server is waitable");
        match (answered, gone) {
            (Ok(()), None) => return,
            (Ok(()), Some(status)) => panic!(
                "something answered on {addr}, and the server this test spawned had already exited \
                 ({status}) — so the answer came from another process, which is SKEIN-526 itself"
            ),
            (Err(e), Some(status)) => panic!(
                "the server exited ({status}) without serving the socket it was handed on {addr}; \
                 the last attempt to reach it said: {e}"
            ),
            // Alive and not answering yet. Ordinarily `first_byte` has just spent its patience
            // blocking on the read, so this is one more turn rather than a spin; an error that
            // comes back faster than that is bounded by the same ceiling either way.
            (Err(_), None) => continue,
        }
    }
}

/// One byte of a response, or the error that stopped it arriving.
///
/// Deliberately indifferent to what the byte says: any status line means the server accepted a
/// connection and wrote to it, and asking `/api/health` without caring whether it answers 200 or
/// 401 keeps this usable by a fixture whose token it does not know.
fn first_byte(addr: &str, patience: Duration) -> std::io::Result<()> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(patience))?;
    s.write_all(
        format!("GET /api/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut byte = [0u8; 1];
    match s.read(&mut byte)? {
        0 => Err(std::io::Error::other(
            "the connection closed without a byte on it",
        )),
        _ => Ok(()),
    }
}

/// [`handed`], then [`until_it_answers`]: the two lines every spawn below used to write for itself.
pub(super) fn serving(cmd: &mut Command) -> (Child, String) {
    serving_with(cmd, TheDoorwaysPin::Set)
}

/// [`serving`], for a test that needs to say which of the two starts it is making.
pub(super) fn serving_with(cmd: &mut Command, pin: TheDoorwaysPin) -> (Child, String) {
    let (mut child, addr) = handed_with(cmd, pin);
    until_it_answers(&mut child, &addr);
    (child, addr)
}

/// The token every fixture writes into its `$SKEIN_HOME`, and that every request below carries.
///
/// Fixed rather than read back after startup: the server mints one on first use, and a test racing
/// that would fail for a reason unrelated to what it tests. The refusal path has its own coverage in
/// `tests/ui/smoke.mjs`, against the running server.
pub(super) const API_TOKEN: &str =
    "tttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttt";

/// A `$SKEIN_HOME` holding nothing but the API token, so a spawned server authenticates the requests
/// below and never touches the developer's real `~/.skein`.
pub(super) fn token_home(tag: &str) -> Scratch {
    let dir = doorway_stopped(Scratch::temp(&format!("skein-it-{tag}")));
    std::fs::write(dir.join("api-token"), API_TOKEN).unwrap();
    dir
}

/// **A spawned server starts a supervisor that is not its child, and `Kid` cannot take it with it**
/// (SKEIN-765).
///
/// `main` runs `fleet::heal_fleet`, which runs `ensure_fleet_door` → `start_server`, which is a
/// `tmux new-session` holding `while [ -f <root>/.skein/server-doorway.py ]; do python3 … ; sleep
/// 2; done`. Three processes per spawned server — the tmux server, the shell, and whichever python
/// the loop is on — none of them descended from the `skein-server` this file kills. Twelve tests
/// left better than thirty behind on a run where every one of them passed, and
/// `node tests/ui/harness/leaks.mjs` — the check `CLAUDE.md` tells every agent to trust after a
/// run — exited 1 naming `skein-it-`, `skein-settings-it` and `skein-repos-it`.
///
/// **Removing the directory is not what stops them, and relying on it is the SKEIN-645 shape.**
/// `Scratch` keeps the directory when the thread is panicking, and the doorway script inside it is
/// the loop's own exit condition — so on the one path where a leak costs the most, the loop
/// restarts a python every two seconds for as long as the evidence is kept. On the passing path it
/// does stop, about two seconds late, which is late enough for the gate to fail and for the next
/// run's count to be wrong.
///
/// So this rides on `Scratch::quiesce_with`, which runs on **every** way out, panic included, and
/// only the removal is conditional — the same argument `tests/fleet_move.rs`'s `scratch()` makes,
/// and the same one `quiesceOnExit` makes for the node tier.
///
/// It takes an already-built `Scratch` rather than building one, so the `Scratch::temp("…")` and
/// its literal prefix stay at the call site: `tests/ui/harness/leaks.mjs` reads the fixture names
/// it checks for out of exactly that shape (`rustPrefixes`), and a helper that swallowed the
/// literal would delete this file's three names from the gate that catches this defect.
pub(super) fn doorway_stopped(dir: Scratch) -> Scratch {
    dir.quiesce_with(|home| {
        stop_doorway(&fleet_root_in(home));
    })
}

/// How long [`stop_doorway`] gives `tmux kill-server` to take the supervisor with it.
///
/// Not a guess at how long that takes — `kill-server` SIGHUPs the pane's process group, so the
/// supervisor shell and whichever python the loop is on go with the tmux server in milliseconds on
/// an idle box. It is set far enough above that for the 40-binary parallel suite not to reach it,
/// which is the load the 250 ms sleep this replaces lost to (SKEIN-920). Only a failure ever pays
/// it: [`until_none_names`] returns on the first clear `ps`.
pub(super) const KILL_WINDOW: Duration = Duration::from_secs(10);

/// How long [`stop_doorway`]'s sweep gives a `SIGKILL` to be delivered, per round.
///
/// The sweep is the fallback and never the measurement: it runs only after [`Killed::left`] has
/// been recorded, so it cannot turn a failed kill into a clean count. It exists because the run
/// that REPORTS a failed kill must not also be the run that leaks — and because the obvious
/// fallback, "remove the script and wait for the loop to notice", was measured and does not do
/// that. Sabotaging the kill into `list-sessions` left all three processes alive through the
/// removal, through its `sleep 2`, and through several further seconds of waiting for it. A
/// `SIGKILL` at a pid the loop's own exit condition can no longer restart is the thing that is
/// actually true, so that is what this waits on, and a second is already a hundred times what
/// signal delivery costs.
const SWEEP_WINDOW: Duration = Duration::from_secs(1);

/// What [`stop_doorway`]'s `kill-server` achieved, as measured before anything else could have.
///
/// Two fields, because one of them is what makes the other mean anything. `left` is empty on a kill
/// that worked — and also on a kill that did nothing at all, if the loop's exit condition was taken
/// away first. See [`stop_doorway`]: that is not a hypothesis, it is the measurement that made
/// SKEIN-920 an item rather than a one-liner.
pub(super) struct Killed {
    /// The doorway script — the loop's own `while [ -f … ]` exit condition — was still on disk when
    /// the wait that produced `left` finished, so for the whole of that wait nothing but the kill
    /// could have emptied it.
    pub(super) script_was_there: bool,
    /// Pids still [`naming`] the root when the wait gave up. Empty means the kill took them.
    pub(super) left: Vec<String>,
}

/// Poll until nothing names `root`, or `within` elapses; the pids still there.
///
/// **This is what replaces a `sleep`.** It returns on the first clear `ps`, so the passing path
/// costs one call rather than a fixed 250 ms and only a failure pays `within`. The 20 ms between
/// tries is a poll interval and not a beat: nothing is decided by it, and doubling or halving it
/// changes only how many `ps` calls a failure makes.
///
/// It is deliberately NOT enough on its own, and that is the whole of SKEIN-920. A bounded poll
/// written here and nothing else still passed with `kill-server` sabotaged into `list-sessions`, in
/// 10.13s, because the caller had already removed the loop's exit condition and the poll was
/// watching a self-exit. What gives it teeth is *when* [`stop_doorway`] calls it.
fn until_none_names(root: &Path, within: Duration) -> Vec<String> {
    let deadline = Instant::now() + within;
    loop {
        let left = naming(root);
        if left.is_empty() || Instant::now() >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Stop the supervisor under `root`, leaving the directory alone, and report what the kill did.
///
/// **The kill comes first and the script removal last, which is the reverse of what this used to
/// do** (SKEIN-920). The loop is `while [ -f <root>/.skein/server-doorway.py ]; do … sleep 2; done`
/// — `supervised` at `src/fleet/server.rs:104`, built by `start_server` at `src/fleet/server.rs:402` — so the
/// script is the loop's own exit condition. Remove it first, as this did, and the loop ends itself
/// within seconds whether or not anything kills tmux: harmless for a teardown, fatal for
/// the test that justifies one, because every count taken afterwards is then empty for a
/// `kill-server` that does nothing. That was measured and not argued: a bounded poll written over
/// the old order passed with the kill sabotaged into `list-sessions`, in 10.13s.
///
/// So the kill is measured while the exit condition is still TRUE, and [`Killed::script_was_there`]
/// reports that in the same breath as the count — a count nobody can date is exactly what went
/// wrong. The removal then happens unconditionally, so the end state is the one the old order left:
/// the only difference is that the window in which tmux is dead while the script is still on disk
/// is now a wait that returns in milliseconds, instead of being the whole of the teardown.
///
/// **No fixed beat.** The `sleep(250ms)` this replaces is what made the test load-dependent — 3/3
/// alone, one failure inside the 40-binary parallel suite. [`until_none_names`] is bounded on the
/// condition instead, so the passing path is faster than the sleep was.
///
/// Not `tmux has-session` to decide whether to bother, and not as an assertion anywhere: the socket
/// lives *inside* `root`, so a missing socket answers "No such file or directory", which reads as
/// "already stopped" whether it is true or not — the exact hole
/// `a_supervisor_whose_fleet_is_gone_stops_rather_than_restarting_for_ever` fell into.
///
/// **The socket path is derived and not spelled** (SKEIN-529). It was `skein.join("server.tmux")`,
/// and the `sock.exists()` below is what makes that dangerous: when the socket moved under
/// `private/`, a literal here would not have failed — it would have skipped the kill, every time,
/// leaving every spawned server's tmux, supervisor shell and python alive. That is the leak
/// SKEIN-765 put this function here to stop, reintroduced by a rename, and green. It is not silent
/// any more: the wait runs whether or not there was a socket to kill, so a kill that was skipped
/// leaves its processes in `left` rather than being shrugged off by an early return.
pub(super) fn stop_doorway(root: &Path) -> Killed {
    let script = root.join(".skein").join("server-doorway.py");
    let sock = PathBuf::from(skein::fleet::server_tmux_sock_in(
        root.to_string_lossy().as_ref(),
    ));
    if sock.exists() {
        let _ = Command::new("tmux")
            .args(["-S", &sock.to_string_lossy(), "kill-server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let killed = Killed {
        left: until_none_names(root, KILL_WINDOW),
        script_was_there: script.is_file(),
    };
    // Last, and unconditionally: the loop's exit condition, so nothing that supervises the
    // supervisor finds a reason to restart it and no further python is started.
    let _ = std::fs::remove_file(&script);
    // Then whatever the kill did not reach, by pid. Removing the script stops the NEXT python and
    // not the one already running — `src/server-doorway.py` holds its socket for as long as it is
    // alive — so a teardown that stopped here would report the leak and leave it (SKEIN-645). Three
    // rounds because a `ps` is a sample: the first can miss a process the second sees.
    for _ in 0..3 {
        let stragglers = naming(root);
        if stragglers.is_empty() {
            break;
        }
        let _ = Command::new("kill")
            .arg("-9")
            .args(&stragglers)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = until_none_names(root, SWEEP_WINDOW);
    }
    killed
}

/// Every process whose command line names `root`, by pid.
///
/// Command lines and not environments, unlike `tests/ui/harness/leaks.mjs`, and the difference is
/// the subject: what SKEIN-687 could not see was the `skein-server` itself, which is exec'd as a
/// bare binary path and carries its fixture only in `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT`. The
/// supervisor is the opposite — its whole loop, doorway path included, *is* its argv — and the
/// server is this file's own child, taken by `Kid`. `ps` rather than `/proc` so this needs no
/// `#[cfg(target_os = "linux")]`, which would be a gate to declare in `tests/platform_gates.rs`
/// for a question both kernels answer.
pub(super) fn naming(root: &Path) -> Vec<String> {
    let needle = root.to_string_lossy().into_owned();
    let out = Command::new("ps")
        .args(["-eo", "pid=,args="])
        .output()
        .expect("ps ran");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(&needle))
        .map(|l| l.split_whitespace().next().unwrap_or("?").to_string())
        .collect()
}

/// The fleet root a spawned `skein-server` is pinned at: a directory inside the fixture's own
/// scratch, made by the server itself if it wants one.
///
/// **Every spawn in this file needed it and none of them had it.** `main` calls
/// `probes::ensure_probe_all`, `fleet::ensure_fleet_kit` and `fleet::heal_fleet` before it binds a
/// port, and those resolve `util::fleet_root()` — which fell through to `"/boxes"`, the owner's
/// LIVE fleet on any machine running skein, and `heal_fleet` writes there rather than only reading
/// (SKEIN-685, an instance of SKEIN-530's class). `cargo test --all` runs this file every time.
///
/// `util::fleet_root` refuses an unpinned test process now (SKEIN-690), and the child inherits
/// `$SKEIN_TEST` from this binary — `.cargo/config.toml`'s `[env]` table puts it here and
/// `Command` passes the environment on — so a spawn that forgets this dies at the pin with a
/// message naming the variable, instead of quietly healing somebody's fleet. That is asserted
/// both ways in `a_server_heals_the_fleet_root_it_was_given_and_refuses_when_given_none`.
///
/// Under `$SKEIN_HOME` rather than beside it, because
/// `a_request_string_that_becomes_a_path_cannot_climb_out_of_skein_home` asserts that nothing is
/// created above the home, and a sibling fleet root would be a fixture breaking that test's own
/// premise.
/// `impl AsRef<Path>` rather than `&Scratch` so [`doorway_stopped`]'s callback, which is handed a
/// bare `&Path`, derives the root the same way every spawn does. Two spellings of "the fleet root
/// is `<home>/fleet`" is two places for it to stop agreeing, and the teardown would be the copy
/// that went wrong silently.
pub(super) fn fleet_root_in(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join("fleet")
}

/// How many times a request is tried, and the pause before try n+1 (`BACKOFF * n`).
///
/// SKEIN-173: under a loaded machine one run died inside `http_post` — `read_to_end` hit a dropped
/// connection, the bare `unwrap` panicked, and the suite failed on a socket accident instead of an
/// assertion (707/707 green on re-run). A dropped connection is not what any test here tests, so
/// the helpers retry it a bounded number of times; what they must never do, on any path, is fail
/// without naming the request that died and the socket error that killed it — an anonymous panic
/// teaches people to re-run instead of read.
const TRIES: u32 = 3;
const BACKOFF: Duration = Duration::from_millis(200);

/// Send one raw request on a fresh socket and read to EOF. Every socket-level failure comes back
/// as `Err` — including a response so truncated it has no status line, which is the same dropped
/// connection wearing a different face — so the caller can retry all of them the same way.
fn send_once(addr: &str, raw: &[u8]) -> std::io::Result<(u16, String)> {
    let mut s = TcpStream::connect(addr)?;
    s.write_all(raw)?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| {
            std::io::Error::other(format!("no status line in a {}-byte response", buf.len()))
        })?;
    Ok((status, text))
}

/// The retry wrapper under `http_get` and `http_post`. `what` names the request being made
/// ("GET /api/boxes") and appears in the panic, with the last socket error, when every try failed.
pub(super) fn send(addr: &str, what: &str, raw: &[u8]) -> (u16, String) {
    let mut last = None;
    for attempt in 1..=TRIES {
        match send_once(addr, raw) {
            Ok(got) => return got,
            Err(e) => last = Some(e),
        }
        if attempt < TRIES {
            std::thread::sleep(BACKOFF * attempt);
        }
    }
    panic!(
        "{what} to {addr} failed {TRIES} times; last error: {}",
        last.unwrap()
    );
}

/// One request over a fresh `Connection: close` socket → (status, full raw response incl. headers).
pub(super) fn http_get(addr: &str, path: &str) -> (u16, String) {
    let raw = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\
         Connection: close\r\n\r\n"
    );
    send(addr, &format!("GET {path}"), raw.as_bytes())
}

/// One GET carrying **no credential at all** — what a box on the shared namespace can send.
///
/// [`http_get`] always sends the token, so it cannot tell "the switch turned auth off" from "the
/// token worked". Only an unauthenticated request can, which is what SKEIN-962 is about.
pub(super) fn http_get_unauthenticated(addr: &str, path: &str) -> (u16, String) {
    let raw = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    send(addr, &format!("GET {path} (no token)"), raw.as_bytes())
}

/// One GET that reads for at most `patience` — for a stream, which never closes.
pub(super) fn http_get_for(addr: &str, path: &str, patience: Duration) -> (u16, String) {
    let raw =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\r\n");
    let mut last = None;
    for attempt in 1..=TRIES {
        match stream_once(addr, raw.as_bytes(), patience) {
            Ok(got) => return got,
            Err(e) => last = Some(e),
        }
        if attempt < TRIES {
            std::thread::sleep(BACKOFF * attempt);
        }
    }
    panic!(
        "GET {path} (stream) to {addr} failed {TRIES} times; last error: {}",
        last.unwrap()
    );
}

/// One attempt at a stream read. `Err` only when the connection died before a single byte arrived
/// — the retryable shape. Once bytes are in hand they are returned whatever ends the read, because
/// partial evidence in an assertion message beats a retry that throws it away.
fn stream_once(addr: &str, raw: &[u8], patience: Duration) -> std::io::Result<(u16, String)> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(patience))?;
    s.write_all(raw)?;
    // **Bounded, not read-to-end.** A stream does not end, and a producer that keeps sending keeps
    // `read_to_end` reading — which is a test that hangs rather than one that fails. Enough bytes
    // for the headers and the opening event is the whole question here.
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0;
    while got < buf.len() {
        match s.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => {
                got += n;
                // The opening event has arrived; anything after it is the next tick, and waiting for
                // one is waiting for the fleet to change.
                if String::from_utf8_lossy(&buf[..got]).contains("\n\n") {
                    break;
                }
            }
            // Patience ran out: the normal end of reading a quiet stream, not a dead socket.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break
            }
            Err(e) if got == 0 => return Err(e),
            Err(_) => break,
        }
    }
    buf.truncate(got);
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Ok((status, text))
}

/// One POST with a raw body + headers → (status, full raw response).
///
/// A retried POST is re-sent whole. In the sliver where the first attempt was applied and only its
/// response was lost, the second answer reports the collision and the caller's assertion prints it
/// — a named failure, which is still strictly better than the socket panic it replaces.
pub(super) fn http_post(addr: &str, path: &str, headers: &str, body: &[u8]) -> (u16, String) {
    let mut raw = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {API_TOKEN}\r\n\
         Connection: close\r\nContent-Length: {}\r\n{headers}\r\n",
        body.len()
    )
    .into_bytes();
    raw.extend_from_slice(body);
    send(addr, &format!("POST {path}"), &raw)
}

/// Kill the server when the test ends, however it ends.
pub(super) struct Kid(pub(super) Child);
impl Drop for Kid {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
