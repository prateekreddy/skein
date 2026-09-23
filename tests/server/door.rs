//! The door and the socket behind it: a flood that never authenticates, a socket handed in
//! rather than bound, the auth off switch, a warden that is not answering, and no child
//! holding the cockpit's listening socket.

use super::*;

/// The doorstep's own account of itself: room, how many are on it, how many it has turned away,
/// and the grace it is actually using.
///
/// Every one of those is a number the server computed under its own lock, which is why the flood
/// test below asks for them instead of timing things. `turned_away` in particular is incremented
/// inside `admit`, in the same critical section as the arrival that caused the eviction, so reading
/// it says what the server *did* and not whether this thread got there in time to watch.
fn doorstep(addr: &str) -> serde_json::Value {
    let (st, seen) = http_get(addr, "/api/machine/doorstep");
    assert_eq!(st, 200, "the doorstep is not being served: {seen}");
    let body = seen.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body.trim()).expect("the doorstep is JSON")
}

/// A box gets a free denial of the control plane if connections cost nothing until they
/// authenticate — architecture §9.4, "pre-auth connection exhaustion". The port is reachable from
/// every box (one network namespace) and both existing caps are inside handlers, so they count only
/// clients that already presented a credential.
///
/// Two properties, and the second is the one worth stating: a flood is **bounded**, so it cannot
/// climb to the file-descriptor limit; and it does not stop an authenticated client being served,
/// because the doorstep evicts the oldest stranger rather than refusing the newest arrival. A cap
/// that refused would satisfy the first and fail the second, which is why the test asserts both.
#[test]
fn a_flood_that_never_authenticates_cannot_hold_the_door() {
    let home = token_home("flood");
    let (child, addr) = serving(
        Command::new(env!("CARGO_BIN_EXE_skein-server"))
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            // Two seconds instead of ten: the deadline is the same mechanism at either length, and
            // the default would make this test spend most of its life waiting for a clock.
            .env("SKEIN_DOORSTEP_GRACE", "2")
            .env_remove("SKEIN_REGISTRY")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

    // What one authenticated request costs on this box, right now, with nothing in the way. The
    // one bound below that is genuinely about elapsed time is measured against this rather than
    // against a constant: a constant wide enough for a contended box is one that no longer fails
    // when the property breaks, and a constant tight enough to catch the property fails on a box
    // that is merely busy. Taken before the flood, so it is the same machine under the same load.
    let t_base = Instant::now();
    let (st, _) = http_get(&addr, "/api/boxes");
    assert_eq!(st, 200, "the fixture's own token does not open the api");
    let base = t_base.elapsed();

    // Sockets that connect and say nothing at all — no request line, no credential. This is the
    // whole of the attack: it needs no token, because §9.4's answer is that connecting is not
    // authenticating, and that answers reading rather than exhausting.
    let room = skein::knock::ROOM;
    let flood: Vec<TcpStream> = (0..room + 16)
        .map(|_| {
            let s = TcpStream::connect(&addr).expect("the port accepts");
            // A ceiling on liveness, not the assertion. This used to be 700ms, picked to sit under
            // the grace so that a patient read could not let the deadline masquerade as an
            // eviction — which quietly made the answer depend on whether the box got round to
            // accepting eighty connections inside 700ms, and it did not when five other things
            // were building on it (SKEIN-601). What tells an eviction from the deadline now is
            // `turned_away` and `knocking`, both read from the server below, so this number only
            // has to outlast a scheduler hiccup.
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            s
        })
        .collect();

    // **Wait for the server to say so, rather than for a clock.** `TcpStream::connect` returns as
    // soon as the kernel has the connection in the listen backlog, which says nothing whatever
    // about the server having accepted it; on a loaded box the accept loop is exactly what falls
    // behind. Polling the count the server keeps takes that out of the assertion: the loop below
    // ends on a fact, and its ceiling is only there so a server that evicts nobody fails in
    // seconds instead of hanging.
    let waited = Instant::now();
    let door = loop {
        let door = doorstep(&addr);
        if door["turned_away"].as_u64().unwrap_or(0) >= 16 {
            break door;
        }
        assert!(
            waited.elapsed() < Duration::from_secs(30),
            "the room turned nobody away in {:?} — a flood that is not bounded reaches the \
             file-descriptor limit and the cockpit stops answering: {door}",
            waited.elapsed()
        );
        std::thread::sleep(Duration::from_millis(10));
    };

    // And at that same instant the rest of the flood was **still standing**. This is what lets the
    // reads below be patient: on its own a patient read proves nothing about eviction, because the
    // grace deadline closes every stranger eventually and a room that evicted nobody would pass
    // it. `knocking` being near the room's size says the deadline has not started closing anyone,
    // so an end-of-stream here can only have come from the arrivals after them.
    let standing = door["knocking"].as_u64().unwrap_or(0);
    assert!(
        standing >= (room / 2) as u64,
        "only {standing} strangers were left on the doorstep when the evictions were counted, so \
         the grace deadline had already begun closing them and an eviction can no longer be told \
         from a timeout: {door}"
    );

    // The oldest are gone: at the limit, an arrival takes the place of the stranger that has been
    // standing longest. Read returns end-of-stream on a socket the server closed.
    let mut closed = 0;
    for held in flood.iter().take(16) {
        let mut byte = [0u8; 1];
        if matches!((&mut &*held).read(&mut byte), Ok(0)) {
            closed += 1;
        }
    }
    assert_eq!(
        closed,
        16,
        "the first 16 of {} connections should have been evicted by the arrivals after them — a \
         flood that is not bounded reaches the file-descriptor limit and the cockpit stops \
         answering",
        flood.len()
    );

    // The other end of the same control: the flood is still on the doorstep *after* those sixteen
    // end-of-streams were read. Had the grace deadline been what closed them, this would be near
    // zero — so this pair is the whole of "immediately, by the arrivals after them", and no part
    // of it is a duration.
    let after = doorstep(&addr);
    let still = after["knocking"].as_u64().unwrap_or(0);
    assert!(
        still >= (room / 2) as u64,
        "only {still} strangers were still standing once the sixteen evictions had been read \
         back, so those end-of-streams cannot be told apart from the grace deadline firing: \
         {after}"
    );

    // And the point of evicting rather than refusing: the client that *will* authenticate arrives
    // into a room that is full, and is served anyway.
    let t0 = Instant::now();
    let (st, _) = http_get(&addr, "/api/boxes");
    assert_eq!(
        st, 200,
        "an authenticated request was refused while a flood held the door — a cap that refuses \
         when full lets the flooder decide who gets in"
    );
    // Scaled from the baseline above, and it has to be: the flat five seconds this replaced was
    // **longer than the grace**, so a request that really had been queued behind the flood — which
    // comes back when the deadline releases them, one grace period later — passed it. Eight times
    // an idle request absorbs the scheduling noise of a busy box; half a grace period of headroom
    // keeps the bound underneath the wait it exists to catch.
    let grace = Duration::from_secs(door["grace_secs"].as_u64().unwrap_or(0));
    let bound = base * 8 + grace / 2;
    assert!(
        t0.elapsed() < bound,
        "an authenticated request took {:?} while the flood held the door, against {bound:?} — \
         the same request took {base:?} with the doorstep empty, and the grace is {grace:?}, which \
         is what a request queued behind the strangers would have waited",
        t0.elapsed()
    );

    // And the flood is **visible**, which is the other half of harmless. `knock` keeps the cockpit
    // answering, and that is exactly what would leave a flood showing up as "the board felt slow
    // once" with nothing to look at. Read back above rather than here, because the evictions are
    // now what this test waits on rather than something it checks at the end.
    assert!(
        door["turned_away"].as_u64().unwrap_or(0) >= 16,
        "the evictions are not reachable from outside the process: {door}"
    );
    assert_eq!(door["room"].as_u64(), Some(skein::knock::ROOM as u64));
    // The grace the server is really using, asserted rather than assumed. The five-second read
    // below used to stand in for this — badly, since five seconds is under the ten-second default
    // as well, so a server that ignored `SKEIN_DOORSTEP_GRACE` failed it for a reason the message
    // never named. Asserted here, the read below is free to be patient.
    assert_eq!(
        door["grace_secs"].as_u64(),
        Some(2),
        "the server is not using the grace this fixture set, so the wait below is not the \
         mechanism under test: {door}"
    );

    // Behind the token, like everything that is not a static asset — otherwise the flooder can
    // watch its own progress, and a defence that reports on itself to whoever is attacking it is
    // helping. Asked with no credential at all, which is what a box has.
    let mut bare = TcpStream::connect(&addr).unwrap();
    bare.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    bare.write_all(
        format!("GET /api/machine/doorstep HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .unwrap();
    let mut refused = Vec::new();
    let _ = bare.read_to_end(&mut refused);
    let refused = String::from_utf8_lossy(&refused).into_owned();
    assert!(
        !refused.contains("turned_away"),
        "an unauthenticated caller was told how the flood is going: {refused}"
    );

    // The second half: a stranger that survived the eviction still does not get to stand there for
    // free. Every one of them is closed once the grace period passes.
    let last = flood.last().expect("the flood is not empty");
    // A ceiling on liveness. That the deadline is the configured two seconds and not the
    // ten-second default is asserted from `grace_secs` above; what is left for this read to
    // establish is that the deadline closes the connection **at all**, and a server that never
    // closes it fails this however long the ceiling is.
    last.set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut byte = [0u8; 1];
    let deadline = Instant::now();
    let ended = matches!((&mut &*last).read(&mut byte), Ok(0));
    assert!(
        ended,
        "a connection that never presented a credential was still open after {:?} — the grace \
         deadline is what makes a slot cost a reconnection instead of nothing",
        deadline.elapsed()
    );
    drop(flood);
}

/// The socket is opened before skein and handed to it, and skein serves on **that** one.
///
/// architecture §9.4: one network namespace and a port mapping that outlives skein means a box that
/// binds the cockpit's port before skein does becomes the cockpit, and the browser hands it the
/// fleet's token on the first request. The token cannot answer that — the only answer is that the
/// port is never free, which means skein takes a socket somebody else opened rather than racing for
/// one. This is the taking half; the in-fleet start that does the opening is 4c.
///
/// Driven through `python3`, and **kept that way now that [`handed`] does the same thing in Rust**:
/// this is a second, independent implementation of the convention, so a mistake in the one every
/// other spawn in this file shares cannot hide here too. It is also the nearer copy of what
/// actually starts a server in the fleet — `src/server-doorway.py` is python, and does exactly
/// this and no more. (The standard library still exposes neither `dup2` nor a way to clear
/// `CLOEXEC`; `handed` declares the three libc calls it needs.)
#[test]
fn the_server_serves_on_a_socket_it_was_handed_rather_than_one_it_bound() {
    if Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        return skip("no python3 to stand in for the process manager");
    }
    let home = token_home("handover");
    let where_port = home.join("port");
    // Binds, puts the listener on fd 3 with CLOEXEC cleared, writes the port it got, and execs the
    // server. `LISTEN_PID` is left out deliberately: it is the older half of the convention and is
    // accepted, and setting it would mean predicting a pid this side of the fork.
    let handover = r#"
import os, socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.bind(("127.0.0.1", 0))
s.listen(64)
open(sys.argv[1], "w").write(str(s.getsockname()[1]))
os.set_inheritable(s.fileno(), True)
if s.fileno() != 3:
    os.dup2(s.fileno(), 3, inheritable=True)
os.environ["LISTEN_FDS"] = "1"
os.execv(sys.argv[2], sys.argv[2:])
"#;
    let child = Command::new("python3")
        .args(["-c", handover])
        .arg(&where_port)
        .arg(env!("CARGO_BIN_EXE_skein-server"))
        // Somewhere it could never have bound by itself, so a pass cannot be a bind that happened to
        // work: the address served below is read back from the socket python opened.
        .env("SKEIN_ADDR", "127.0.0.1:1")
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _kid = Kid(child);

    let start = Instant::now();
    let addr = loop {
        if let Ok(port) = std::fs::read_to_string(&where_port) {
            let addr = format!("127.0.0.1:{}", port.trim());
            if TcpStream::connect(&addr).is_ok() {
                break addr;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "the server never served on the socket it was handed"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    let (st, _) = http_get(&addr, "/api/health");
    assert_eq!(
        st, 200,
        "the handed-in socket accepted a connection but the server behind it did not answer"
    );
}

/// **The port a server in this file serves on is never free — not even while it is still starting.**
///
/// This is the property SKEIN-526 did not have. `free_port` bound `127.0.0.1:0`, read the number
/// back and dropped the listener, so the number was unbound from the moment it was returned until
/// the child reached `bind` — which is after `ensure_probe_all`, `ensure_fleet_kit` and
/// `heal_fleet`, hundreds of milliseconds later. Thirteen spawns in this file raced for ephemeral
/// ports inside that window; when one lost it, the child died in `bind` and
/// `while TcpStream::connect(&addr).is_err()` was answered by the *sibling's* server, so the wait
/// loop was satisfied and the requests after it failed with `Connection refused` once the sibling
/// finished.
///
/// So this asks the question that window existed to answer, at the worst instant for it: it tries
/// to take the port for itself while the server is still booting and has served nothing. A `bind`
/// that **succeeds** is the defect — it says the number is lying there for anyone to take.
///
/// **What makes it fail**, run rather than reasoned about: put `free_port` back. Spawning with
/// `SKEIN_ADDR` from a dropped listener instead of [`handed`] makes the `bind` below return `Ok`,
/// and the assertion fails with the port it was able to take.
#[test]
fn a_spawned_server_holds_its_port_from_before_it_starts() {
    let home = token_home("port-held");
    // `handed` rather than `serving`: the claim is about the window *before* the server is up, so
    // this must not wait for it to come up first.
    let (child, addr) = handed(
        Command::new(env!("CARGO_BIN_EXE_skein-server"))
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_REGISTRY", "")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let mut kid = Kid(child);

    let taken = TcpListener::bind(&addr);
    match taken {
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::AddrInUse,
            "the port was refused for a reason other than being held, so this proves nothing \
             about who holds it: {e}"
        ),
        Ok(mine) => panic!(
            "{addr} was there to be taken while the server that is supposed to be holding it was \
             still starting — which is SKEIN-526 exactly: the next thing to bind it becomes the \
             server this test's requests reach ({:?})",
            mine.local_addr()
        ),
    }

    // And it is our own server holding it rather than a leftover from somewhere: it answers.
    until_it_answers(&mut kid.0, &addr);
    let (st, _) = http_get(&addr, "/api/health");
    assert_eq!(
        st, 200,
        "the port was held by something that does not answer as this fixture's server"
    );
}

/// **An answer from a server that is not ours is caught rather than believed.**
///
/// This is the half of SKEIN-526 that made it nasty rather than merely flaky. The child had died in
/// `bind`, and the wait loop was *satisfied* — by the sibling that had taken the port, which was
/// answering on it perfectly well. Everything after that was a test talking to another test's
/// server, until the sibling finished and the requests turned into `Connection refused`.
/// [`until_it_answers`] asks `try_wait` after a **successful** answer for exactly that reason, and
/// this is the construction that makes the question earn its place: a real server answering on the
/// address, and a child that is certainly dead.
///
/// The dead one is a real `skein-server` that died in its start-up sequence, which is the shape the
/// racing child had: started with no `$SKEIN_FLEET_ROOT`, it refuses before the port and before it
/// writes anything (`a_server_heals_the_fleet_root_it_was_given_and_refuses_when_given_none`).
///
/// **What makes it fail**: drop the `(Ok(()), Some(status))` arm of that match — let a successful
/// answer return whoever sent it — and nothing panics, so there is no message to find.
///
/// `catch_unwind` rather than an attribute that expects the panic, because `Scratch` keeps its
/// directory while the thread is panicking (`tests/common/mod.rs:253`) — a test that let the panic
/// out would leave a fixture behind on every green run. The panic printed on the way past is this
/// assertion working.
#[test]
fn an_answer_from_a_server_that_is_not_ours_is_caught_rather_than_believed() {
    let home = token_home("not-ours");
    let (child, addr) = serving(
        Command::new(env!("CARGO_BIN_EXE_skein-server"))
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_REGISTRY", "")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

    let bare = token_home("not-ours-dead");
    let mut dead = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_HOME", bare.path())
        .env_remove("SKEIN_FLEET_ROOT")
        .env_remove("SKEIN_SHARED")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the server binary spawned");
    let status = dead.wait().expect("it is waitable");
    assert!(
        !status.success(),
        "the stranger's start succeeded, so it is not the dead child this needs: {status}"
    );

    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until_it_answers(&mut dead, &addr)
    }));
    let payload = caught.expect_err("a live server answered for a dead child and was believed");
    let said = payload
        .downcast::<String>()
        .map(|s| *s)
        .unwrap_or_else(|_| String::from("<the panic carried no message>"));
    assert!(
        said.contains("had already exited"),
        "it did panic, but not about whose answer it got: {said}"
    );
}

/// A start that was told the socket comes from outside, and got none, stops.
///
/// The two modes want opposite answers and the difference has to be *said*. Host-driven there is
/// nobody upstream to open a socket, so binding is the only way to start. In the fleet a missing
/// descriptor means the start sequence did not do its job — and binding anyway runs the very race
/// this closes, from the one process that was supposed to have closed it.
#[test]
fn told_the_socket_comes_from_outside_and_given_none_the_server_refuses_to_bind() {
    let home = token_home("inherited-only");
    // The one spawn here that must NOT be handed a socket — having none is the whole question — so
    // it is also the one that still needs an address of its own. **Held for the length of the test
    // rather than read from `free_port` and let go**: what is asserted below is that the refusal
    // happened *before* the bind, and against a port this process is holding a server that reached
    // the bind would fail there and say so, in the words the SKEIN-526 demonstration printed. A
    // number nobody holds cannot tell those two apart, and can itself be taken mid-test.
    let held = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let addr = held
        .local_addr()
        .expect("the listener knows its address")
        .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env("SKEIN_ADDR", &addr)
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
        // The warden too, where nothing listens — see the first spawn above.
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env(INHERITED_ONLY, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Waited for rather than read to the end. A server that ignored the setting and bound is a
    // server that never exits, so `output()` here would *hang* instead of failing — which is a
    // failure a person has to interpret from a stuck run. Written the other way round the first
    // time, and the sabotage took two minutes to say what this says in five seconds.
    let start = Instant::now();
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None if start.elapsed() > Duration::from_secs(10) => {
                let _ = child.kill();
                panic!("the server stayed up, so it bound a port it was told not to bind");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    assert!(
        !status.success(),
        "the server exited cleanly rather than refusing"
    );
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let why = stderr;
    assert!(why.contains("LISTEN_FDS=1"), "{why}");
    assert!(why.contains(INHERITED_ONLY), "{why}");
    // And it really did not bind — otherwise the refusal is a message printed on the way past a
    // bind that had already happened. `$SKEIN_ADDR` names a port this test is holding, so a server
    // that got that far fails there and says `cannot bind <addr>: Address already in use`; that it
    // said neither is what makes this a refusal rather than a bind that lost.
    assert!(!why.contains("cannot bind"), "{why}");
    drop(held);
}

/// **`$SKEIN_NO_API_AUTH` is refused where the cockpit is the fleet's, and honoured where it is
/// not** (SKEIN-962; architecture §9.4 said the first half from the day it was written, and until
/// this test nothing made it true — `apiauth::disabled` returned a bool and the server printed a
/// warning and served the whole API to every box in the namespace).
///
/// Two spawns, identical but for the one variable the fleet's doorway sets on its child
/// ([`TheDoorwaysPin`]). Both are handed the same kind of socket by this process, so neither races
/// for a port and the descriptor is not what differs.
///
/// **What would make each half fail**, named before it was written and then planted (see the commit
/// message):
///
///   * the first half — make `apiauth::off_switch_refused` return `false`, which is what the code
///     did before this item, and `/api/boxes` answers 200 with `thing-a` in it to a request
///     carrying no credential at all. `an in-fleet cockpit served the API` is the assertion.
///   * the second half — make `off_switch_refused` return `switch_set()` alone, refusing
///     everywhere rather than in the fleet, and the documented off-switch stops working for the
///     owner it exists for. `outside the fleet the documented off-switch` is that assertion.
///
/// The requests are deliberately **unauthenticated**: [`http_get`] carries the token, so under it a
/// 200 cannot tell "the switch turned auth off" from "the token was accepted", which is the whole
/// distinction being measured.
#[test]
fn the_auth_off_switch_is_refused_under_the_fleets_doorway_and_honoured_outside_it() {
    let dir = Scratch::temp("skein-it-apiauth-switch");
    let reg = dir.join("sandboxes.json");
    std::fs::write(
        &reg,
        r#"{"thing-a":{"branch":"a","dir":"/d","lastSeen":"2026-01-01T00:00:00Z","status":"done"}}"#,
    )
    .unwrap();
    // A placement, because that is what makes a box a box — the same fixture
    // `server_serves_ui_vendor_and_guards_routes` builds, and for the same reason: without it
    // `/api/boxes` is an empty list, and an empty list is what a refusal looks like too.
    let home = token_home("apiauth-switch");
    let places = home.to_path_buf().join("places");
    std::fs::create_dir_all(&places).unwrap();
    std::fs::write(
        places.join("thing-a.json"),
        r#"{"sandbox":"skein-fleet","ns_pid":1,"home":"/boxes/thing-a/home","tree":"/boxes/thing-a/tree","sock":"/boxes/thing-a/session.sock"}"#,
    )
    .unwrap();

    // **A builder rather than one `Command` spawned twice, and that is SKEIN-989.** This test makes
    // the only two-spawn measurement in the file, and it made both of them off one `Command` —
    // which carried the first spawn's `pre_exec` closure, and the descriptor `handed_with` had
    // dropped after it, into the second spawn's child. `handed_with` refuses that now, so this
    // shape is the one that compiles *and* runs; the note there has the measurement.
    let cockpit = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_skein-server"));
        cmd.env("SKEIN_REGISTRY", &reg)
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            // A warden at an address the kernel refuses, as every spawn in this file does.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_NO_API_AUTH", "1")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    };

    // ---- the fleet's cockpit: nothing is served, to anybody ----
    let (child, addr) = serving_with(&mut cockpit(), TheDoorwaysPin::Set);
    let _kid = Kid(child);
    let (st, body) = http_get_unauthenticated(&addr, "/api/boxes");
    assert_eq!(
        st, 503,
        "an in-fleet cockpit served the API to a request with no credential while \
         $SKEIN_NO_API_AUTH was set: {body}"
    );
    assert!(
        body.contains("SKEIN_NO_API_AUTH") && body.contains("9.4"),
        "the refusal does not say what was refused or where that is written down: {body}"
    );
    assert!(
        !body.contains("thing-a"),
        "the refusal carried the fleet's boxes in it: {body}"
    );
    // A refusal and not a gate: the fleet's own token does not buy the API back either.
    let (st, body) = http_get(&addr, "/api/boxes");
    assert_eq!(
        st, 503,
        "the token reopened an API the cockpit had refused to serve: {body}"
    );
    // Including the page `open_to_all` would otherwise hand to anyone — a cockpit that loads and
    // then fails every call it makes is a worse answer than one that says what is wrong.
    let (st, body) = http_get_unauthenticated(&addr, "/");
    assert_eq!(st, 503, "the cockpit page was served anyway: {body}");

    // ---- and it is the doorway's pin that decides, not the switch on its own ----
    let (child, addr) = serving_with(&mut cockpit(), TheDoorwaysPin::Unset);
    let _outside = Kid(child);
    let (st, body) = http_get_unauthenticated(&addr, "/api/boxes");
    assert_eq!(
        st, 200,
        "outside the fleet the documented off-switch stopped working, so this refuses an owner \
         who has some other boundary rather than a box that has none: {body}"
    );
    assert!(
        body.contains("thing-a"),
        "the off-switch answered without the fleet's boxes in it: {body}"
    );
}

/// **A `Command` is handed a socket once, and the second time is refused where it is asked for.**
///
/// The guard in [`handed_with`] is what turns SKEIN-989 from a thing that happens on a busy box
/// into a thing that cannot be written. `pre_exec` closures stack rather than replace, so a
/// `Command` spawned twice through that helper carries the first spawn's closure — and the
/// descriptor the helper dropped after it — into the second spawn's child. Under load that came
/// back as `EBADF` out of `spawn`, or as a server that exited 1 holding something that was not a
/// listening socket; alone it passed, because the freed descriptor number was taken straight back
/// by the next listener.
///
/// **What makes it fail**: delete the `assert!` at the top of [`handed_with`] and the second
/// hand-over is accepted, so the `Ok` arm below fires by name. That was run, and it did.
///
/// `/bin/sh` rather than the server binary, because nothing here is about the server: the subject
/// is the helper, and a shell that exits at once costs no start-up sequence and leaves no
/// supervisor behind.
///
/// `catch_unwind` rather than `#[should_panic]`, for the reason
/// `an_answer_from_a_server_that_is_not_ours_is_caught_rather_than_believed` gives: the assertion
/// is about *what the refusal says*, and a test that let the panic out could not read it. The
/// panic printed on the way past is this assertion working.
#[test]
fn a_command_that_has_already_been_handed_a_socket_is_refused_a_second_one() {
    let mut once = Command::new("/bin/sh");
    once.arg("-c")
        .arg("exit 0")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let (mut first, _addr) = handed(&mut once);
    first.wait().expect("the first child is waitable");

    let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handed(&mut once)));
    let why = match again {
        Ok((mut extra, _)) => {
            let _ = extra.wait();
            panic!(
                "a `Command` that had already been handed a listening socket was handed a second \
                 one, so the first hand-over's stale descriptor can still reach a second child"
            )
        }
        Err(why) => why,
    };
    let why = why
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| why.downcast_ref::<&str>().copied())
        .unwrap_or("<the refusal's payload was not a string>");
    assert!(
        why.contains("already been handed a listening socket"),
        "the refusal does not say what was refused: {why}"
    );
    assert!(
        why.contains("pre_exec") && why.contains("stack"),
        "the refusal does not say why one `Command` cannot be spawned twice: {why}"
    );
    assert!(
        why.contains("/bin/sh"),
        "the refusal does not name the `Command` it refused: {why}"
    );
}

/// The server says the warden is missing **at boot**, not at the first Launch.
///
/// Every other thing this server depends on is checked when it starts — the turn-state probes, the
/// kit, the fleet's launcher, the gh token, the ssh key — and each says so on stderr when it is not
/// there. The warden was the exception, so a host without one found out from a 500 after pressing a
/// button, which on an upgrade lands weeks after the change that caused it with nothing pointing
/// back.
///
/// **And it must not be fatal.** A fleet that already exists runs perfectly well without a warden;
/// what it cannot do is be created or resized. Refusing to start over a capability somebody may not
/// use today would be the wrong trade — so this asserts both halves: it complains, and it serves.
#[test]
fn the_server_says_at_boot_when_no_warden_is_answering() {
    let home = token_home("nowarden");
    // A port with nothing on it, so "no warden" is a state this actually reaches rather than one it
    // inherits from whatever the machine happens to be running. **The one surviving `free_port`**,
    // and it is the one case the socket handover has no answer for: what is wanted here is an
    // address nothing serves, which is the opposite of a socket somebody is holding open.
    let quiet = free_port();

    // It serves, and the complaint is a complaint rather than a refusal — which `serving` is now
    // what establishes, since it returns on an answered request rather than on a connection.
    let (mut child, addr) = serving(
        Command::new(env!("CARGO_BIN_EXE_skein-server"))
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            .env("SKEIN_WARDEN", format!("127.0.0.1:{quiet}"))
            .env("SKEIN_REGISTRY", "")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let (code, _) = http_get(&addr, "/");
    assert_eq!(code, 200, "the server did not come up without a warden");

    let _ = child.kill();
    let out = child
        .wait_with_output()
        .expect("collect the server's output");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains("warden"),
        "starting with no warden said nothing about it:\n{said}"
    );
    // The address it asked, because a wrong port is the likeliest cause and the reader cannot check
    // a number nothing printed.
    assert!(
        said.contains(&quiet.to_string()),
        "it did not say where it looked:\n{said}"
    );
}

/// The inode of the socket listening on `port`, from the kernel's own table — `/proc/net/tcp` and
/// its v6 twin, state `0A`, which is LISTEN. `None` when nothing listens there.
fn listening_inode(port: u16) -> Option<String> {
    let local = format!(":{port:04X}");
    ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .find_map(|table| {
            let text = std::fs::read_to_string(table).ok()?;
            text.lines().skip(1).find_map(|row| {
                let cols: Vec<&str> = row.split_whitespace().collect();
                (cols.len() > 9 && cols[1].ends_with(&local) && cols[3] == "0A")
                    .then(|| cols[9].to_string())
            })
        })
}

/// Which of `pid`'s descriptors are the socket `inode` — read from `/proc/<pid>/fd`, and an error
/// rather than an empty list when that cannot be read, because a process that could not be looked
/// at is not a process found clean.
fn holding(pid: u32, inode: &str) -> Result<Vec<String>, String> {
    let want = format!("socket:[{inode}]");
    let dir = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map_err(|e| format!("/proc/{pid}/fd could not be read ({e})"))?;
    Ok(dir
        .filter_map(Result::ok)
        .filter(|fd| std::fs::read_link(fd.path()).is_ok_and(|to| to.as_os_str() == want.as_str()))
        .map(|fd| fd.file_name().to_string_lossy().into_owned())
        .collect())
}

/// The pids whose parent is `parent`, from `/proc/<pid>/stat` — the field after the parenthesised
/// command, which is read from the LAST `)` because a command name may contain one.
fn children_of(parent: u32) -> Vec<u32> {
    let mut kids = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let ppid = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|p| p.parse::<u32>().ok());
        if ppid == Some(parent) {
            kids.push(pid);
        }
    }
    kids
}

/// **No process skein-server starts holds the cockpit's listening socket** (SKEIN-1035).
///
/// The real doorway (`src/server-doorway.py`, run from the tree) opens the socket and starts the
/// real server behind it, handing the listener over on descriptor 3 — inheritable, because that is
/// the only way it crosses the exec. The server then starts children of its own, and the one used
/// here is the one every start makes: `heal_fleet` at boot runs `start_server`, whose `tmux
/// new-session` leaves a tmux server behind for the fixture's fleet. That tmux server is exactly
/// the process the item measured holding :7878 in a real fleet, and it is long-lived, so reading
/// its descriptors is not a race against a child that has already exited.
///
/// Two ports, neither of them 7878: the doorway's, which this test opens, and
/// `$SKEIN_SERVER_PORT` for the fleet door `heal_fleet` opens inside the fixture — set so that the
/// nested doorway can never reach for a real cockpit's port.
///
/// **Presence before absence.** The server itself must hold the listener (so the inode was read
/// right and the handover happened), and the fixture's tmux server must exist and be readable (so
/// "it holds nothing" is about a process that was there).
///
/// **What makes it fail**: drop `close_on_exec` from `doorway::keep_from_children` (or the call to
/// it at the top of `main`) — the tmux server is started long before `doorway::inherited` adopts
/// the socket, so the CLOEXEC in `adopt` alone comes too late for it, and this fails naming the
/// tmux pid and the descriptor that holds the listener.
#[test]
fn no_process_the_server_starts_holds_the_cockpits_listening_socket() {
    if !have("tmux") || !have("python3") {
        return skip("this machine lacks tmux/python3, so it cannot run the doorway or its fleet");
    }
    if !Path::new("/proc/net/tcp").exists() {
        return skip("no /proc/net/tcp to read the listener's inode from");
    }
    let home = token_home("listenfd");
    let root = fleet_root_in(&home);
    // `free_port`'s window is harmless here: the doorway retries `EADDRINUSE` for ten seconds, and
    // a sibling that took the number would make it refuse loudly rather than serve somewhere else.
    let (door_port, fleet_port) = (free_port(), free_port());
    let child = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/server-doorway.py"
        ))
        .arg(door_port.to_string())
        .arg(env!("CARGO_BIN_EXE_skein-server"))
        .arg(home.join("door-stamp"))
        .env("SKEIN_HOME", home.path())
        .env("SKEIN_FLEET_ROOT", &root)
        .env("SKEIN_SERVER_PORT", fleet_port.to_string())
        .env("SKEIN_WARDEN", "127.0.0.1:1")
        .env("SKEIN_REGISTRY", "")
        .env_remove("SKEIN_SHARED")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("python3 runs the doorway");
    let mut door = Kid(child);
    let door_pid = door.0.id();
    let addr = format!("127.0.0.1:{door_port}");
    // `heal_fleet` runs before the server serves, so once it answers the tmux server exists.
    until_it_answers(&mut door.0, &addr);

    let inode = listening_inode(door_port)
        .unwrap_or_else(|| panic!("nothing is listening on :{door_port}, and something answered"));
    let servers = children_of(door_pid);
    assert!(
        servers
            .iter()
            .any(|&pid| holding(pid, &inode).is_ok_and(|fds| !fds.is_empty())),
        "no child of the doorway ({door_pid}) holds socket:[{inode}] — so either the inode was \
         read wrong or the server is not serving the socket it was handed; children: {servers:?}"
    );

    let tmux: Vec<u32> = naming(&root)
        .iter()
        .filter_map(|pid| pid.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .is_ok_and(|c| c.starts_with("tmux"))
        })
        .collect();
    assert!(
        !tmux.is_empty(),
        "the server started no tmux server for the fixture's fleet, so there is no child here to \
         find clean: {:?}",
        naming(&root)
    );
    // Every process naming the fixture's fleet, tmux or not — the supervisor shell and the nested
    // doorway are the server's descendants too.
    for pid in naming(&root)
        .iter()
        .filter_map(|pid| pid.parse::<u32>().ok())
    {
        let fds = match holding(pid, &inode) {
            Ok(fds) => fds,
            // A `ps` sample of a process that has exited since; the tmux server cannot be one,
            // because it is asserted readable below.
            Err(_) if !tmux.contains(&pid) => continue,
            Err(why) => panic!("the fixture's tmux server {pid}: {why}"),
        };
        assert!(
            fds.is_empty(),
            "process {pid} ({}), started by skein-server, holds the cockpit's listening socket \
             socket:[{inode}] on descriptor(s) {fds:?} — it will keep :{door_port} bound after \
             the doorway and the server are gone (SKEIN-1035)",
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .unwrap_or_default()
                .trim()
        );
    }
}
