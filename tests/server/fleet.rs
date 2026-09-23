//! What a spawned server does to the fleet it is given: heals the root it was handed and no
//! other, stops its doorway supervisor on teardown, and streams an upload on a crossing's stdin.

use super::*;

/// **The fleet root a spawned server was given is the one it writes into — and given none, it
/// refuses to start at all.**
///
/// This is SKEIN-685 as a test, and it needs both halves. `skein-server`'s `main` runs
/// `fleet::heal_fleet()` before it binds a port, and that writes: measured against a fixture root,
/// five files land under `<root>/.skein/` — `box-session.sh`, `server-doorway.py`,
/// `git-credential-skein`, `skein-startup.sh`, and `private/server.tmux`, which was directly in
/// `.skein` until SKEIN-529. Every spawn in this file passed `$SKEIN_HOME` and
/// not `$SKEIN_FLEET_ROOT`, so `util::fleet_root()` fell through to `/boxes` and those five went
/// to the LIVE fleet's own `.skein` — the launcher every real box starts through — rewritten from
/// whatever was in the tree, on every `cargo test --all`.
///
/// The second half is a *process*, not a thread, and that is the point: the guard keys on
/// `$SKEIN_TEST`, which reaches an integration binary from `.cargo/config.toml`'s `[env]` table
/// and reaches the child because `Command` passes this process's environment on. A suite that only
/// ever exercises the pinned path proves nothing about what an unpinned one does.
///
/// **What makes it fail**, both run rather than reasoned about: deleting the `assert!` from
/// `util::fleet_root` lets the second child bind and reach the `assert!(!status.success())`;
/// dropping the `.env("SKEIN_FLEET_ROOT", …)` from the first half turns it into the second and the
/// launcher assertion fires.
#[test]
fn a_server_heals_the_fleet_root_it_was_given_and_refuses_when_given_none() {
    // ── given a root: what heal_fleet writes lands in it ──
    let home = token_home("fleet-root");
    let root = fleet_root_in(&home);
    let (child, _addr) = serving(
        Command::new(env!("CARGO_BIN_EXE_skein-server"))
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", &root)
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_REGISTRY", "")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);
    let launcher = root.join(".skein/box-session.sh");
    assert!(
        launcher.is_file(),
        "the server answered and wrote no launcher at {} — if `heal_fleet` no longer writes one, \
         this test is aimed at a mechanism that has moved, and the pin it justifies has to be \
         argued again rather than quietly dropped",
        launcher.display()
    );

    // ── given none: it refuses, before the port and before any write ──
    let bare = token_home("fleet-root-bare");
    // A port this test is holding, which is what turns "before the port" into something checked
    // rather than described: a server that got as far as the bind would fail on this address and
    // say `cannot bind`, and the refusal below says something else entirely. `free_port` would give
    // a number nobody holds, where reaching the bind and refusing look identical.
    let held = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let out = Command::new(env!("CARGO_BIN_EXE_skein-server"))
        .env(
            "SKEIN_ADDR",
            held.local_addr().expect("it knows its address").to_string(),
        )
        .env("SKEIN_HOME", bare.path())
        .env_remove("SKEIN_FLEET_ROOT")
        .env_remove("SKEIN_SHARED")
        .output()
        .expect("the server binary ran");
    assert!(
        !out.status.success(),
        "a server started with no fleet root and lived — which means it resolved one, and the only \
         one it can resolve unasked is /boxes"
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("SKEIN_FLEET_ROOT"),
        "the refusal does not name the variable to set, so it tells whoever hit it nothing: {said}"
    );
    assert!(
        !said.contains("cannot bind"),
        "it reached the bind before refusing, so 'before the port' is no longer true: {said}"
    );
    assert!(
        !bare.join("fleet").exists(),
        "the refusal came after something had already been written"
    );
}

/// **The teardown stops the supervisor; removing the directory is not what stops it** (SKEIN-765).
///
/// The distinction is the whole test, and it is the one a leak count taken after a *passing* run
/// cannot make. On the passing path `Scratch` removes the fixture, the doorway script goes with it,
/// and the loop notices within its two-second sleep — so "the processes are gone a moment later"
/// is true whether or not [`stop_doorway`] does anything at all. On the failing path the directory
/// is KEPT, deliberately, because it is the only evidence a failure leaves (`tests/common/mod.rs`)
/// — and then the loop's exit condition is kept with it and the supervisor runs for ever. That is
/// SKEIN-645: a kept fixture restarted a python every two seconds for nine hours.
///
/// So this asserts against a fixture that is still there — `.skein` is checked for below — which is
/// exactly the shape of the panic path, without needing a panic to produce it.
///
/// **And "still there" is not enough on its own** (SKEIN-920). The directory surviving does not
/// mean the *script* survived, and the script is the loop's own exit condition: while
/// [`stop_doorway`] removed it before killing tmux, the loop ended itself within seconds
/// regardless, and an empty count afterwards said nothing about the kill. Measured, not argued —
/// the bounded poll that was the obvious repair for the 250 ms sleep below passed with
/// `kill-server` sabotaged into `list-sessions`, in 10.13s. The kill is now taken first and counted
/// while the exit condition still holds, and [`Killed::script_was_there`] is asserted so that the
/// count can be dated.
///
/// **What makes each assertion fail**, run rather than reasoned about:
///
/// * presence, before: the supervisor starts inside `heal_fleet`, which runs before the socket is
///   served, so a server that has answered and has none means the mechanism this teardown is aimed
///   at has moved.
/// * presence, after `Kid`: making `Kid::drop` also stop the doorway would empty it — and that is
///   the belief this whole item corrects, that killing the server is enough.
/// * `script_was_there`: putting the `remove_file` back above the `kill-server` in
///   [`stop_doorway`]. This is the assertion that keeps the next one honest, and it is here because
///   without it the next one was green under a `kill-server` that had been replaced by
///   `list-sessions`.
/// * `left` empty: sabotaging that `kill-server` — `list-sessions` in place of it, or a socket
///   argument of `server.tmux.wrong` — leaves the loop mid-`sleep 2` with its exit condition still
///   true, so all three processes survive [`KILL_WINDOW`] and are named in the panic. Run, both
///   ways: three pids, failing in 10.36s against 0.38s green.
#[test]
fn the_doorway_supervisor_stops_when_the_teardown_runs_and_not_when_the_fixture_is_removed() {
    if !have("tmux") {
        return skip("no tmux, so a spawned server starts no supervisor for this to stop");
    }
    let home = token_home("doorway");
    let root = fleet_root_in(&home);
    let (child, _addr) = serving(
        Command::new(env!("CARGO_BIN_EXE_skein-server"))
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", &root)
            // The warden too, where nothing listens — see the first spawn above.
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("SKEIN_REGISTRY", "")
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let kid = Kid(child);

    let running = naming(&root);
    assert!(
        !running.is_empty(),
        "the server answered and nothing anywhere names {} — `heal_fleet` reaches `start_server` \
         before the socket is served, so if it no longer starts a tmux supervisor this test is \
         aimed at a mechanism that has moved, and the teardown it justifies has to be argued again \
         rather than quietly dropped",
        root.display()
    );

    // The server goes, and the supervisor does not: it is tmux's child, not this process's.
    drop(kid);
    let orphaned = naming(&root);
    assert!(
        !orphaned.is_empty(),
        "killing the spawned server emptied {} of named processes, so the supervisor IS reachable \
         from `Kid` after all — which would make this whole teardown unnecessary, and is worth \
         knowing before it is deleted",
        root.display()
    );

    let stopped = stop_doorway(&root);
    assert!(
        root.join(".skein").is_dir(),
        "{} is gone, so an empty count below would be the fixture's removal rather than the \
         teardown — the one thing this test exists to tell apart",
        root.join(".skein").display()
    );
    assert!(
        stopped.script_was_there,
        "{} was already gone when the teardown finished counting, so the supervisor loop's own \
         `while [ -f … ]` exit condition was FALSE for some of the wait and the count below would \
         be empty for a `kill-server` that does nothing whatever. That is not a worry, it is the \
         measurement in SKEIN-920: under exactly this order the kill was replaced by \
         `list-sessions` and the test passed, in 10.13s. Whatever moved the removal above the \
         kill has to be undone, not accommodated",
        root.join(".skein/server-doorway.py").display()
    );
    assert!(
        stopped.left.is_empty(),
        "the teardown ran against a fixture that is still on disk, with the doorway script still \
         under it — so the loop could not have ended itself — and {} process(es), pid(s) {}, \
         outlived its `kill-server` by {KILL_WINDOW:?}. Nothing but that kill can end the loop \
         while its exit condition holds, so on the path where the directory is KEPT, which is \
         every failing test, this supervisor restarts a python every two seconds for ever \
         (SKEIN-645)",
        stopped.left.len(),
        stopped.left.join(", ")
    );
}

// ---------------------------------------------------------------------------------------------
// The third payload-carrying spawn: an upload's body (SKEIN-824)
// ---------------------------------------------------------------------------------------------

/// The anchor's start time as the crossing guard reads it: field 22 of `/proc/<pid>/stat`.
///
/// Cut after the LAST `") "` rather than taken as whitespace field 22, which is what
/// `Place::guard` does with `sed -n 's/.*) //p' … | cut -d' ' -f20` — `comm` is parenthesised and
/// may contain spaces and parentheses of its own. Field 20 of what remains is field 22 of the
/// line, and the two spellings have to agree or the guard refuses the crossing this test is about.
fn anchor_start(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .unwrap_or_else(|e| panic!("the anchor process {pid} has no /proc entry: {e}"));
    let rest = stat
        .rsplit_once(") ")
        .unwrap_or_else(|| panic!("/proc/{pid}/stat has no comm field: {stat}"))
        .1;
    rest.split_whitespace()
        .nth(19)
        .and_then(|f| f.parse().ok())
        .unwrap_or_else(|| panic!("/proc/{pid}/stat has no start time: {stat}"))
}

/// **The body streamed into a box rides the crossing's stdin, and is nowhere in that process's own
/// `/proc/<pid>/cmdline`** (SKEIN-824). The third of the three payload-carrying spawns, and the
/// only one no library test can ask about.
///
/// `place::tests::the_payload_a_crossing_carries_is_on_its_stdin_and_not_in_its_cmdline` and
/// `…_a_write_carries_…` ask it of [`skein::place::Place::attempt`] and `Place::write`, both of
/// which go through `Place::spawning` — the seam a lib test can install a stand-in at. This path
/// goes through neither: `sandbox::box_write_argv` hands its argv **across the crate boundary** to
/// `skein-server`'s `stream_upload`, which spawns it with a `tokio::process::Command` of its own
/// and streams the upload into its stdin. `skein-server`'s `main` opens with
/// `seam::real_crossings()`, correctly — a spawned server cannot be handed a closure — so there is
/// no seam here to stand in at, and this suite, which starts a real server, is the only tier that
/// can ask.
///
/// **What guards it today is exactly what SKEIN-813 ruled insufficient**: `box_write_script(dir,
/// path)` has no parameter a body could arrive through, so the revert fails to compile. That is a
/// guard against one revert. The regression it does not stop is a convenience — buffer the upload
/// (`UPLOAD_CAP` already bounds it) and hand it to the child as an argument — and this is the
/// upload of a *user's file*, so the argv is world-readable in `ps` for as long as the write runs.
///
/// # What is real here and what stands in
///
/// Real: the server, the route, `drop_dest`, `place_of` reading a placement record, the anchor
/// guard `Place::guard` builds, `box_write_argv`, the `tokio` spawn, and the body arriving over a
/// socket as a stream. The crossing measured is the process `stream_upload` spawned — the capture
/// is that process's own `/proc/<pid>/cmdline`, read by the process itself, not an argv this test
/// built and then asserted about.
///
/// # Nothing stands in any more, and that is what changed (SKEIN-832)
///
/// This used to reach its capture through a **planted `nsenter`** on the `$PATH` of the server it
/// spawns. That worked because `Place::enter` left the hop unqualified and resolved it from
/// whatever environment `skein-server` had been started in — and the comment here said that if the
/// hole were ever closed, the stand-in would stop being reached and this would fail loudly on
/// `"ok":true` rather than pass about nothing. It was closed, this did fail exactly there, and the
/// capture had to stop depending on a PATH lookup that no longer exists.
///
/// So the hop is **real** now. A box is a bwrap namespace and this test makes one: the anchor the
/// placement record names is a `bwrap --dev-bind / /` process, so the crossing's own
/// `nsenter --user=… --mount=… --preserve-credentials` genuinely joins it, and the write lands
/// inside it. `place::tests::a_crossing_in_the_fleet_enters_the_box_without_sbx` crosses into a
/// bwrap namespace the same way; this is that, with a real server and a real body on the far end.
///
/// # How a process with nothing in front of it reports its own cmdline
///
/// A crossing ends in `bash -lc`, and a login shell reads a profile. `nsenter` carries the caller's
/// environment, so `$HOME` inside the crossing is the `$HOME` `skein-server` was started with —
/// which this test owns. A `.profile` there copies `/proc/$$/cmdline` out, and `$$` is the crossing.
///
/// **It is one process throughout, which is the point.** `bash -c <guard>` ends in `exec nsenter`,
/// and `nsenter` execs the program after its `--`; an exec replaces the image and keeps the pid. So
/// the pid reading its own cmdline in that profile is the pid `stream_upload` spawned, and what it
/// reads is what `ps` would show for it.
///
/// The capture therefore holds production's argv from the last exec onwards — `bash -lc <the
/// wrapped script>`, the tail `write_argv` built, **and the only element a body could appear in**.
/// The `env PATH=… bash -c <guard> bash` in front and the `nsenter <flags>` after it are lost to
/// the two execs; both are built from the placement record alone, and neither has a parameter a
/// body could arrive through. That is one exec further along than the planted-`nsenter` capture
/// reached, and the element given up — `nsenter`'s flags, which are `ns_pid` and nothing else — is
/// not one a regression could put a body in: anything appended to the argv `write_argv` builds
/// rides through the `-- "$@"` into exactly the element this does capture.
///
/// # The evidence that the crossing ran
///
/// The route's own answer: `stream_upload` returns `Ok(path)` only after `wait_with_output` reports
/// the child exited 0, so `"ok":true` with a path in it is the server saying the crossing it
/// spawned ran to completion. Three independent things say so and this test asserts all three: that
/// reply, a capture that exists at all — nothing writes one unless a crossing reached its login
/// shell — and the file at `path` holding the body byte for byte, written **inside the namespace**
/// by a `cat` whose stdin was the upload.
///
/// The `done` file the old stand-in touched as its last act went with the stand-in, and is not
/// missed: it existed to prove the capture was a whole argv rather than a fragment, and
/// `/proc/<pid>/cmdline` read in one shot from the kernel cannot be a fragment. The assertion that
/// the capture holds the `cat >` the script ends in says the same thing, about the content.
///
/// # What makes it fail
///
/// Named before it was written and then done: buffer the body in `stream_upload` and append it to
/// the argv before spawning — the convenience above. The `/proc/<pid>/cmdline` assertion fires.
/// And with the capture hook sabotaged to write an empty file, the "exactly one crossing carried
/// the write" guard fires instead of the assertion passing about nothing.
#[test]
fn an_uploaded_body_is_on_the_crossings_stdin_and_not_in_its_cmdline() {
    if !Path::new("/proc/self/cmdline").exists() {
        return skip(
            "no /proc, so what the kernel lists for a spawned process cannot be read here",
        );
    }

    if !common::bwrap_works() {
        return skip(
            "bwrap cannot make a namespace here, so a crossing has no box to enter and the hop \
             cannot be real",
        );
    }

    let home = token_home("uploadargv");
    let seen = home.join("crossings");
    let tree = home.join("tree");
    // The `$HOME` the server is started with, and so — `nsenter` carrying the caller's environment
    // — the `$HOME` the crossing's login shell reads its profile from.
    let served_from = home.join("serverhome");
    for dir in [&seen, &tree, &served_from] {
        std::fs::create_dir_all(dir).unwrap();
    }

    // The capture, and the whole of it. A login shell reads this before it runs the script it was
    // given, and `$$` is the crossing's own pid — the pid `stream_upload` spawned, unchanged
    // across both execs. No `$PATH` lookup decides whether this runs: `bash -lc` reads a profile
    // because it is a login shell, which is `Place::shell`'s deliberate choice for a box and not
    // something a PATH pin can take away.
    std::fs::write(
        served_from.join(".profile"),
        format!(
            "tr '\\0' '\\n' < /proc/$$/cmdline > {seen}/cmdline-$$\n",
            seen = seen.display()
        ),
    )
    .unwrap();

    // The anchor the crossing guard checks, and the namespace the crossing actually enters. A bare
    // `sleep` was enough while the hop stood in; a real `nsenter` needs a real user and mount
    // namespace to join, so this is `bwrap` — the same thing a box is.
    //
    // `--dev-bind / /` so the namespace shares this filesystem: the write lands at a path this test
    // can read back, which is what makes the body assertion possible from out here.
    // `--die-with-parent` so killing the `Kid` below takes the inner process with it rather than
    // orphaning it. `$SKEIN_HOME` rides in its environment so that if one ever did leak,
    // `tests/ui/harness/leaks.mjs` would name it — a `sleep` says nothing about this fixture in its
    // arguments, and the environment is the other half of that gate (SKEIN-687).
    let anchor_at = home.join("anchor");
    let bwrap_err = home.join("bwrap.err");
    let _anchor = Kid(Command::new("bwrap")
        .args(["--dev-bind", "/", "/", "--die-with-parent", "--"])
        .arg("bash")
        .arg("-c")
        .arg(format!("echo $$ > {}; exec sleep 300", anchor_at.display()))
        .env("SKEIN_HOME", home.path())
        .stdout(Stdio::null())
        // A FILE rather than `/dev/null`: a file holds no pipe open, so it costs nothing here and
        // it is the only place bwrap's own refusal would be recorded.
        .stderr(std::fs::File::create(&bwrap_err).expect("a file for bwrap's stderr"))
        .spawn()
        .expect("start a box-like namespace for the placement"));
    // The pid INSIDE the namespace, not bwrap's own — `/proc/<it>/ns/user` is what the crossing
    // joins, and bwrap's is this test's.
    let ns_pid: u32 = {
        let mut found = None;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&anchor_at) {
                if let Ok(pid) = text.trim().parse() {
                    found = Some(pid);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        found.unwrap_or_else(|| {
            let said = std::fs::read_to_string(&bwrap_err).unwrap_or_default();
            panic!(
                "the box-like namespace never reported its anchor; bwrap said: {}",
                said.trim()
            )
        })
    };
    assert_ne!(
        std::fs::read_link("/proc/self/ns/mnt").ok(),
        std::fs::read_link(format!("/proc/{ns_pid}/ns/mnt")).ok(),
        "the anchor is in this test's own namespace, so a crossing into it would prove nothing \
         about entering a box"
    );
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();

    const BOX: &str = "thing-drop";
    let places = home.join("places");
    std::fs::create_dir_all(&places).unwrap();
    std::fs::write(
        places.join(format!("{BOX}.json")),
        serde_json::json!({
            "sandbox": "skein-fleet",
            "ns_pid": ns_pid,
            "home": tree.display().to_string(),
            "tree": tree.display().to_string(),
            "sock": home.join("box.sock").display().to_string(),
            "generation": boot.trim(),
            "ns_start": anchor_start(ns_pid),
        })
        .to_string(),
    )
    .unwrap();

    // `drop_dest` puts a dropped file under `/tmp/skein-drop-<batch>`, and the batch is the
    // client's to choose — so it is named the way `tests/common/mod.rs` names a scratch directory,
    // `<prefix>-<pid>`, which is what `sweep_abandoned` reads when a later run tidies up after a
    // failing one.
    let batch = format!("uploadargv-{}", std::process::id());
    let drop_dir = format!("/tmp/skein-drop-{batch}");

    // `$HOME`, where the old fixture set `$PATH`. The server's environment is what `nsenter` hands
    // the crossing, so this is how the capture hook above gets in front of it — and, unlike a PATH
    // entry, it is not something `Place::enter`'s pin can take away.
    let (child, addr) = serving(
        Command::new(env!("CARGO_BIN_EXE_skein-server"))
            .env("SKEIN_HOME", home.path())
            .env("SKEIN_FLEET_ROOT", fleet_root_in(&home))
            .env("SKEIN_WARDEN", "127.0.0.1:1")
            .env("HOME", &served_from)
            .env_remove("SKEIN_SHARED")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let _kid = Kid(child);

    // Shaped so a grep for it finds this test and nothing else, and deliberately small: past
    // `MAX_ARG_STRLEN` — 131,072 bytes on 4 KiB-page hardware — a body put on the argv could not be
    // spawned at all, and this test would be rescued by a spawn failure rather than asserting
    // anything. An attachment under that cap is the case a reader of `ps` would actually have got.
    let marker = "SKEIN-824-UPLOAD-MARKER";
    let body = format!("{marker}.").repeat(64);
    assert!(
        body.len() < 131_072,
        "the body has grown past MAX_ARG_STRLEN, so this test has stopped being about what `ps` \
         shows: {} bytes",
        body.len()
    );

    let (st, reply) = http_post(
        &addr,
        &format!("/api/boxes/{BOX}/upload"),
        &format!(
            "Content-Type: application/octet-stream\r\nX-Skein-Name: note.txt\r\n\
             X-Skein-Drop: {batch}\r\n"
        ),
        body.as_bytes(),
    );
    assert_eq!(st, 200, "the upload route answered {st}: {reply}");
    assert!(
        reply.contains("\"ok\":true"),
        "the write into the box did not run to completion, so every capture below is whatever was \
         at that path beforehand — which is nothing: {reply}"
    );
    let written = reply
        .split("\"path\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_else(|| panic!("the reply names no path, so there is nothing to read: {reply}"))
        .to_string();

    // Every crossing that reported itself, and the one that carried this write. The needle is the path
    // the SERVER answered with rather than a script this test spelled: an argv assembled here and
    // compared against itself is exactly what this item exists to replace.
    let captures: Vec<(String, String)> = std::fs::read_dir(&seen)
        .expect("the capture directory the crossings write into")
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(e.path()).ok()?;
            name.strip_prefix("cmdline-")
                .map(|pid| (pid.to_string(), text))
        })
        .collect();
    let carrying: Vec<&(String, String)> = captures
        .iter()
        .filter(|(_, text)| text.contains(&written))
        .collect();
    assert_eq!(
        carrying.len(),
        1,
        "exactly one crossing should have carried the write to {written}, and {} did — so the \
         assertion below is about nothing, or about the wrong process. What the crossings reported: \
         {captures:?}",
        carrying.len()
    );
    let (_pid, cmdline) = carrying[0];
    assert!(
        cmdline.contains("cat >"),
        "the capture holds the write's path but not the `cat` that consumes its stdin, so it is \
         not the script `box_write_script` built and the assertion below would hold however the \
         body was sent: {cmdline}"
    );

    assert!(
        !cmdline.contains(marker),
        "the body is in the spawned crossing's /proc/<pid>/cmdline — world readable in `ps`, to \
         anything sharing this machine, for as long as the upload runs: {cmdline}"
    );
    assert!(
        !captures.iter().any(|(_, text)| text.contains(marker)),
        "the body is in the cmdline of some other crossing this upload spawned: {captures:?}"
    );

    assert_eq!(
        std::fs::read_to_string(&written).unwrap_or_else(|e| panic!(
            "the box was told the file was written to {written} and nothing is there: {e}"
        )),
        body,
        "the box was not handed the body on stdin, or not all of it"
    );

    let _ = std::fs::remove_dir_all(&drop_dir);
}
