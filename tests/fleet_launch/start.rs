//! `start_box` leaves a box that is actually usable, and a server restart repairs a fleet that
//! predates it.

use super::*;

/// The whole of `start_box`, rather than its pieces called in the right order by hand.
///
/// The test above assembles the launch itself — install, clone, session — and that is precisely why
/// it kept passing while every real launch failed. Each bug lived in the *seams*: the placement
/// recorded an empty HOME, so provisioning resolved `$HOME/shared` to `/shared`; no launch spec was
/// written, so the box stayed on the clone's default branch; the fleet root was never created. None
/// of it is visible unless the entry point itself is the thing under test.
#[test]
fn start_box_leaves_a_box_that_is_actually_usable() {
    let _env = env_lock();
    // **The real crossing is this suite's subject**, so it says so rather than being refused:
    // `Place::spawning` turns a fleet-scope command into a panic in a test process that has
    // installed no stand-in (SKEIN-530), and a stand-in here would delete what the module note
    // above promises — a real clone, a real bwrap namespace, a real tmux server, real `nsenter`
    // re-entry. What keeps all of that inside the fixture is the `$SKEIN_FLEET_ROOT` these tests
    // pin at their own scratch tree.
    let _real = skein::place::seam::real_crossings();
    // **`python3` is new in this guard and was always needed here** (SKEIN-957). The launcher's
    // whole credential leg is python — `login_life`, `merge_login`, and now the onboarding flag
    // asserted below — so on a machine without it a box is seeded with a copy and none of the
    // judgement, which is a different thing from what this test says it starts.
    if !bwrap_works() || !have("tmux") || !have("git") || !have("python3") {
        return skip(
            "this machine cannot make a bwrap namespace, or lacks tmux/git/python3, so it cannot \
             host a box",
        );
    }
    let root = scratch_named("start");
    write_fake_sbx(&root.join("bin"));
    let remote = write_remote(&root);
    let name = "demo-smoke";

    // Bound after `root`, so every name stops pointing into the scratch tree before the tree is
    // removed — `$HOME` below included, which the `set_var` on this test's last line put back only
    // when the test passed.
    let mut pins = env_pins();
    // And the warden, at an address where nothing listens: a box teardown reports the destroy
    // into the host audit log, and `warden_client` refuses a test process that has not said
    // which warden to ask rather than letting it reach the owner's (SKEIN-762).
    pins.set("SKEIN_WARDEN", "127.0.0.1:1");
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("SKEIN_HOME", root.join("skein"))
    .set("SKEIN_FLEET_ROOT", root.join("boxes"))
    // No runtime installs: this harness's `sbx exec` runs on THIS machine, so the substrate step
    // would npm-install an agent runtime onto the developer's box. It did exactly that once.
    .set("SKEIN_RUNTIME_PACKAGES", "");
    // Stand in for the SANDBOX's home. Without this the fake `sbx` reports this machine's real
    // $HOME, and the launcher would seed a box from — and reconcile credentials back into — the
    // developer's own ~/.claude. A test must not be able to touch that; the first run of this test
    // read a real credential file, which is exactly how it was caught.
    let sandbox_home = root.join("sandbox-home");
    fs::create_dir_all(sandbox_home.join(".claude")).unwrap();
    // Shaped like the real thing, because the sync now reads it rather than moving it about: it
    // tells a login from the husk a logout leaves, and it carries only the login across. A
    // placeholder like `{"tok":"…"}` passed straight through the old copy and would say nothing
    // about either. The `mcpOAuth` block is what the sandbox must KEEP when a box's login arrives —
    // those grants are per-repo, and copying the file whole used to discard the receiver's.
    fs::write(
        sandbox_home.join(".claude/.credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"SEEDED","refreshToken":"r"},"mcpOAuth":{"sync|sandbox":{"accessToken":"GRANT-SHARED"}}}"#,
    )
    .unwrap();
    pins.set("HOME", &sandbox_home)
        // The fleet sandbox already exists, so `ensure_fleet` goes straight to substrate +
        // launcher.
        .set("SKEIN_LS_CMD", format!("echo '[{{\"name\":\"{FLEET}\"}}]'"));

    let store = root.join("store");
    fs::create_dir_all(&store).unwrap();
    ensure_store(&store).expect("seed the store");
    ensure_probe_in(&store).expect("seed the store's scripts");
    let repo = Repo {
        read_prs: false,
        id: "demo".into(),
        source: remote.clone(),
        store: store.to_string_lossy().into_owned(),
        plane_project: String::new(),
        sync_connection: String::new(),
        review_queue: true,
        sync_gateway_url: String::new(),
        ..Default::default()
    };
    save_repos(std::slice::from_ref(&repo)).expect("register the repo");
    let mut config = load_config();
    config.fleet_sandbox = FLEET.into();
    save_config(&config).expect("turn the fleet on");

    // ---- the gate, warmed before the act, so the act has something wrong to settle ----
    // Read once here and once after each of the three acts below, with no `forget_fleet_liveness()`
    // in between. That is the whole test: the gate serves its last good answer while it refreshes
    // behind the caller, so the only thing that can make the second read agree with the fleet is the
    // act having invalidated it. This is also the only place the check can live — `cfg!(test)` is
    // false for the library these tests link, so the gate is real here and disabled in unit tests.
    assert_eq!(
        fleet_liveness().get(name).copied(),
        None,
        "nothing is placed under this name yet, so the warm answer must not mention it"
    );

    start_box(
        name,
        &repo,
        "feat/smoke",
        "exec sleep 300",
        skein::place::Purpose::Manual,
    )
    .expect("start the box");

    // Remove `start_box`'s settle and this reads back the map from before the launch — no entry at
    // all for a box that is up and whose row the person who pressed the button is looking at.
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(true),
        "starting a box must settle the liveness gate, or the board serves the pre-start picture"
    );

    // The placement must carry a real HOME: `Place::wrap` exports it, and an empty one sends every
    // `$HOME/…` path in provisioning to the filesystem root.
    let placed = shared_record(name).expect("the box is placed");
    assert!(
        placed.home.starts_with('/') && placed.home.len() > 1,
        "a placement with no HOME makes every box command write to /: {:?}",
        placed.home
    );

    // And which isolation it got. The whole chain, in one assertion, because every link is silent
    // on its own: `install_launcher` stamps the script it installs, the script reports the stamp it
    // was given, and `start_box` records what was reported. Break any of them and a box that IS
    // covered reads as uncovered forever — a restart that never clears the thing asking for it.
    //
    // A box keeps the mount namespace it was born with, so this is the only moment the answer
    // exists; there is nothing on the host to check it against afterwards, which is the whole
    // reason the value has to travel with the box.
    assert_eq!(
        placed.launcher,
        skein::fleet::launcher_revision(),
        "the box that this launcher just started does not know which launcher started it"
    );

    // And what bounds it. The value depends on the machine — a scratch fleet with no cgroup
    // delegation reports `uncapped no-cgroup-delegation` and that is the honest answer — so what is
    // asserted is that the box **said something**, which is the whole of the defect. Nothing on the
    // host can read `limits.state`; it is written inside the sandbox, so a box that reported nothing
    // is a box whose ceiling is unknowable, and it used to look exactly like a bounded one.
    assert!(
        !placed.ceiling.is_empty(),
        "the box did not say what bounds its memory, so an uncapped one is invisible again"
    );
    assert!(
        skein::fleet::is_capped(&placed.ceiling) || placed.ceiling.starts_with("uncapped "),
        "the ceiling state is neither capped nor a named reason: {:?}",
        placed.ceiling
    );

    // The launch spec is how the box, and skein, learn which branch this box is for. Asserting on
    // the checkout alone proves nothing: `clone_script` checks the branch out itself, so that stays
    // green with no spec at all — while the box's own restart falls back to the clone's default and
    // the board reports the wrong branch.
    let tree = format!("{}/tree", box_root(name));
    assert_eq!(
        sh(&format!("git -C {tree} rev-parse --abbrev-ref HEAD")),
        "feat/smoke",
        "the box is on its own branch, not the clone's default"
    );
    assert_eq!(
        branch_of(name).as_deref(),
        Some("feat/smoke"),
        "skein must be able to read the box's branch back from its launch spec"
    );

    // Provisioning ran *inside* the box: its shared home resolves under the store, not under /.
    let boxed = place_of(name).expect("placed");
    let link = boxed
        .exec("readlink \"$HOME/shared\" || true", Duration::from_secs(30))
        .expect("read the shared-home link");
    assert!(
        link.trim().starts_with(store.to_str().unwrap()),
        "shared home must point into the mounted store, got {link:?}"
    );

    // And the store is reachable from the checkout, which is what makes hooks and the probe work:
    // through `.claude/skein`, in the directory of the box's own that `.claude` is (SKEIN-1053).
    let claude = boxed
        .exec(
            &format!("readlink -f {tree}/.claude/skein || true"),
            Duration::from_secs(30),
        )
        .expect("resolve .claude");
    assert!(
        claude.trim().starts_with(store.to_str().unwrap()),
        "the box's .claude/skein must resolve into the store, got {claude:?}"
    );
    // The file only the kit writes, and the one a box with a `.claude` directory reads its status
    // line from (SKEIN-1220). Present after the first start, or the relaunch half below asserts a
    // file that was never there to come back.
    let local_settings = PathBuf::from(format!("{tree}/.claude/settings.local.json"));
    assert!(
        local_settings.is_file(),
        "the first start's kit wrote no {}, so the relaunch assertion below would be about nothing",
        local_settings.display()
    );
    let first_start = start_id_of(name);
    assert!(
        !first_start.is_empty(),
        "the launcher wrote no start id on the first start"
    );

    // ---- the sandbox cycles: the tree survives, the session does not ----
    // Measured against a real sandbox, not imagined: after sbx restarted skein-fleet, the box's
    // checkout, private HOME and cgroup ceiling were all intact and its tmux server was gone. Every
    // such box was then unreachable, and what a user saw first was an nsenter error about a pid.
    let before = shared_record(name).unwrap().ns_pid;
    let place = own_sandbox(FLEET);
    // A re-login inside the box, made just before the session dies. It is newer than the sandbox's
    // copy and it STAYS HERE: the file a box writes is not evidence about itself, and a box that
    // could improve the fleet's copy could also poison it. See the launcher's direction rule.
    let box_cred = PathBuf::from(format!("{}/home/.claude/.credentials.json", box_root(name)));
    let seeded = fs::read_to_string(&box_cred).unwrap_or_default();
    assert!(
        seeded.contains("SEEDED"),
        "a new box inherits the sandbox's login rather than asking for its own: {seeded}"
    );
    // ---- and it is not then asked to log in anyway (SKEIN-957) ----
    // The invariant, over a box that was really started rather than over a fixture: a box skein has
    // handed a credential to must not meet Claude Code's onboarding screen, which is gated on
    // `hasCompletedOnboarding` in `~/.claude.json` and not on the credential. Derived over every
    // home under the fleet root that the launcher's own `login_life` calls a login, so a seed path
    // that acquires a credential some other way and forgets the flag fails here too. The sandbox's
    // copy in this fixture has no `.claude.json` at all, which is the state that produced the bug:
    // the flag cannot have arrived by being copied down.
    let carrying = homes_carrying_a_login(&root.join("boxes"));
    assert!(
        !carrying.is_empty(),
        "no box under the fleet root carries a login, so this assertion is about nothing — the \
         launch above did not seed the credential it was given"
    );
    for home in &carrying {
        assert_eq!(
            onboarding_flag(home),
            Some(serde_json::Value::Bool(true)),
            "{} holds a working credential and would still be asked to onboard — which is the \
             login screen on every new box",
            home.display()
        );
    }
    // ---- nor then asked to trust the tree skein cloned for it (SKEIN-959) ----
    // The same invariant over a really-started box: every box under the fleet root has ITS OWN
    // tree trusted in its own `~/.claude.json`, under the exact path its session starts in. No
    // login condition here, on purpose — trust is about the tree, not the credential.
    let mut trusted_boxes = 0;
    for at in fs::read_dir(root.join("boxes"))
        .expect("the fleet root has a boxes directory after a start")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|at| at.join("home").is_dir() && at.join("tree").is_dir())
    {
        let tree = at.join("tree").display().to_string();
        assert_eq!(
            trust_of(&at.join("home"), &tree),
            Some(serde_json::Value::Bool(true)),
            "{} was started by skein and would still ask the person to trust the tree skein cloned \
             for it",
            at.display()
        );
        trusted_boxes += 1;
    }
    assert!(
        trusted_boxes > 0,
        "no box under the fleet root has a home and a tree, so the trust assertion is about nothing"
    );
    std::thread::sleep(Duration::from_millis(1100)); // mtime granularity, not a race
                                                     // A re-login, and a grant of this box's own alongside it. Both are written here because the two
                                                     // must travel differently: the login belongs to the person and goes everywhere, the grant
                                                     // belongs to this box's repository and goes nowhere.
    fs::write(
        &box_cred,
        br#"{"claudeAiOauth":{"accessToken":"RELOGIN","refreshToken":"r"},"mcpOAuth":{"sync|box":{"accessToken":"GRANT-MINE"}}}"#,
    )
    .unwrap();
    // **Asserted, not discarded.** This used to be `.ok()`, which threw the kill's result away —
    // so a `tmux kill-server` that never reached the box left it running, and the assertion below
    // then reported `Some(true)`: correct about a live box, and silent about the sweep this test
    // is for. A kill that failed has to fail here, where the message names the kill.
    place
        .exec(
            &format!("tmux -S {} kill-server", box_sock(name)),
            Duration::from_secs(30),
        )
        .expect("the kill must reach the box's tmux server");
    // And waited out on the anchor, because the kill is not synchronous — see `anchor_gone`, which
    // is where the measurement is. One act, then one read.
    anchor_gone(before);
    // The server was killed behind skein's back, which is the one thing the gate cannot know. A warm
    // gate hands back its last picture immediately and refreshes behind the caller — deliberately,
    // so the board never blanks on a slow tick — so without this the assertion reads whatever the
    // *previous* test left there. Skein invalidates at every point it changes the fleet itself; this
    // is that, for a change skein did not make.
    forget_fleet_liveness();
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(false),
        "a killed server is a stopped box, and the sweep must see it"
    );

    // A snapshot exists to rescue work, so it must not need the box's session. Taken here, with the
    // server dead — the state resize hit on the first real run, where it refused with an nsenter
    // error and left the work it was trying to save unreachable.
    let snap = snapshot_box(name, repo.store.as_str(), "test-run").expect("snapshot a dead box");
    let bundle = store.join(&snap).join("repo.bundle");
    assert!(
        bundle.exists() && bundle.metadata().map(|m| m.len()).unwrap_or(0) > 0,
        "the box's commits must be saved even with no session: {}",
        bundle.display()
    );
    assert!(
        store.join(&snap).join("agent-state.tgz").exists(),
        "and its private agent state with them"
    );
    // A setting that arrived after this box's last provisioning, as SKEIN-1218's default did for
    // every box on the fleet: gone from the box's own file, so only a kit run can bring it back.
    fs::remove_file(&local_settings).expect("remove the box's settings.local.json");
    ensure_box_session(name).expect("restart the session from the tree");
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(true),
        "the box is reachable again without a re-clone"
    );

    // ---- the relaunch is a start, and the kit runs for it (SKEIN-1220) ----
    // `ensure_box_session` is the path every attach and every cockpit terminal takes to bring back
    // a box whose sandbox cycled, and it used to launch without provisioning: on the live fleet,
    // not one box had a ready marker for the start it was on. What fails each assertion: dropping
    // the provisioning from `ensure_box_session` fails the first; a kit that writes its ready
    // marker under any name but the current start's fails the third.
    assert!(
        local_settings.is_file(),
        "a box relaunched by ensure_box_session did not get its kit run, so its settings.local.json \
         (skein's status line and defaults) was not written: {}",
        local_settings.display()
    );
    let second_start = start_id_of(name);
    assert_ne!(
        first_start, second_start,
        "the relaunch did not mint a new start id, so this proves nothing about a new start"
    );
    let ready = PathBuf::from(format!(
        "{}/tmp/skein-startup.ready.{second_start}",
        box_root(name)
    ));
    assert!(
        ready.exists(),
        "a box relaunched by ensure_box_session has no ready marker for the start it is on ({}), \
         so its kit never ran for it",
        ready.display()
    );
    let after = shared_record(name).unwrap().ns_pid;
    assert_ne!(
        before, after,
        "the anchor is a new process, so the placement must name it — a stale pid addresses nothing"
    );

    // ---- one login, seeded down, and never written back up ----
    // The fleet's copy is what every box is seeded from, so a box that could write it could hand
    // every later box a credential of its choosing — and the expiry it would win on is a number
    // inside a file the box writes. So the flow is one-way here: the box keeps its re-login and the
    // fleet's copy is untouched, whatever the two files claim about themselves.
    let reconciled =
        fs::read_to_string(sandbox_home.join(".claude/.credentials.json")).unwrap_or_default();
    assert!(
        reconciled.contains("SEEDED") && !reconciled.contains("RELOGIN"),
        "a box wrote the copy every later box is seeded from: {reconciled}"
    );
    // Nothing else travelled either, and this half fails silently. Copying the file whole would
    // have handed the sandbox this box's per-repo grant and destroyed the sandbox's own —
    // surfacing much later as an MCP server asking to be authorised again.
    assert!(
        reconciled.contains("GRANT-SHARED"),
        "the sandbox's own MCP grant was destroyed by a login sync: {reconciled}"
    );
    assert!(
        !reconciled.contains("GRANT-MINE"),
        "one box's per-repo MCP grant escaped into the shared copy: {reconciled}"
    );
    // And the box was not handed the fleet's older copy over its own newer one either: it keeps
    // what it has. Losing a working login to a stale one is the failure this rule replaced, not
    // one it is allowed to reintroduce.
    let kept = fs::read_to_string(&box_cred).unwrap_or_default();
    assert!(
        kept.contains("RELOGIN"),
        "the box's own login was overwritten with the fleet's older one: {kept}"
    );

    // ---- stopping and destroying settle the gate too, and this is also the teardown ----
    // Waited out through `/proc` rather than by polling `fleet_liveness`: every extra read is a
    // chance for the refresh running behind an earlier one to land, which would hide exactly the
    // staleness under test. One act, one read.
    let anchor = shared_record(name).expect("the box is placed").ns_pid;
    stop_box(name).expect("stop the box");
    // Through `anchor_gone` rather than a loop of its own: this site had the rule and the site
    // sixty lines above did not, which is the whole of why that one raced. Two spellings of one
    // wait is two places for it to stop agreeing.
    anchor_gone(anchor);
    assert_eq!(
        fleet_liveness().get(name).copied(),
        Some(false),
        "stopping a box must settle the liveness gate, or the board keeps it running"
    );

    // The same rule with a worse failure: the box is not stopped but gone, and a gate serving its
    // last good answer leaves a destroyed box on the board for anyone to click.
    destroy_box(name).expect("destroy the box");
    // **The post-condition of a destroy, asked before the gate is.** `destroy_script` ends
    // `rm -rf <root>/<name>; …; exit 0`, so a removal that failed — a straggler mount from the
    // box's namespace is the way it can — is swallowed, and the box's directory is one of the two
    // registers `fleet::live_box_names` and `place::local_liveness` both read. Without this the
    // symptom is `Some(false)` where `None` was expected, sixty characters of Option that name
    // neither the directory nor the removal. Asked with no wait: the removal is inside a script
    // this call already waited for, so a directory still here is a defect and not a delay.
    assert!(
        !Path::new(&box_root(name)).exists(),
        "the destroy left {} behind, so the box still reads as live to `live_box_names` and to \
         the sweep — the `rm -rf` in `destroy_script` failed and its `exit 0` swallowed it",
        box_root(name)
    );
    assert_eq!(
        fleet_liveness().get(name).copied(),
        None,
        "destroying a box must settle the liveness gate, or the board keeps a box that is gone"
    );
    assert!(
        shared_record(name).is_none(),
        "a destroyed box is unplaced, so nothing can be sent into what used to be its namespace"
    );
}

/// A fleet outlives the skein that made it, so restarting the server has to repair one.
///
/// The sandbox keeps whichever `box-session.sh` it was last given. Upgrade the host and the two
/// disagree: this skein passes a spec the installed launcher cannot read, the launcher exits before
/// tmux, and every reconnect enters an anchor pid from the last boot — `nsenter: cannot open
/// /proc/<pid>/ns/user`, forever, because nothing on the reconnect path ever replaced the copy that
/// could not start. Nothing else in a run observes that mismatch, which is why the repair belongs to
/// the restart.
///
/// And it must not cost a VM boot. Starting the cockpit is not a request to run the fleet, so a
/// sleeping sandbox is asked about (`sbx ls`) rather than asked *of* (`sbx exec`) — the launcher it
/// carries is repaired by `ensure_box_session` on the path that wakes it instead.
#[test]
fn a_server_restart_repairs_a_fleet_that_predates_it() {
    let _env = env_lock();
    // **The real crossing is this suite's subject**, so it says so rather than being refused:
    // `Place::spawning` turns a fleet-scope command into a panic in a test process that has
    // installed no stand-in (SKEIN-530), and a stand-in here would delete what the module note
    // above promises — a real clone, a real bwrap namespace, a real tmux server, real `nsenter`
    // re-entry. What keeps all of that inside the fixture is the `$SKEIN_FLEET_ROOT` these tests
    // pin at their own scratch tree.
    let _real = skein::place::seam::real_crossings();
    let root = scratch_named("box");
    write_fake_sbx(&root.join("bin"));
    // Bound after `root`, so every name stops pointing into the scratch tree before it is removed.
    let mut pins = env_pins();
    pins.set(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .set("SKEIN_HOME", root.join("skein"))
    .set("SKEIN_FLEET_ROOT", root.join("boxes"));
    save_config(&Config {
        fleet_sandbox: FLEET.into(),
        fleet_memory: "26g".into(),
        ..Config::default()
    })
    .expect("configure a fleet");

    // The launcher an older skein left behind, in the one place the sandbox looks for it.
    install_launcher(FLEET).expect("install box-session.sh");
    let launcher = box_session_path();
    let stale = "#!/usr/bin/env bash\nexit 9 # an older skein's copy\n";
    fs::write(&launcher, stale).unwrap();

    // **The sleeping half of this test is gone, and the property with it** (SKEIN-576). It asserted
    // that a fleet reported `stopped` by `sbx ls` was left alone — that starting the cockpit is not
    // a request to boot a VM — and it was a host's property: only a skein OUTSIDE the sandbox can
    // observe one that is not running. Skein runs inside the fleet now, so a fleet it can heal is
    // running by definition; there is no state in which this process exists and its sandbox does
    // not. What replaced the observation is that `sbx ls` is not asked at all from in here.
    //
    // ---- the copy out there becomes this binary's copy ----
    pins.set(
        "SKEIN_LS_CMD",
        format!(r#"echo '[{{"name":"{FLEET}","status":"running"}}]'"#),
    );
    await_ls(Some(Liveness::Running));
    heal_fleet().expect("heal a running fleet");
    let now = fs::read_to_string(&launcher).unwrap();
    assert_ne!(
        now, stale,
        "a running fleet keeps the launcher it was given"
    );
    assert!(
        now.contains("apply_fleet_ceilings"),
        "the launcher installed is the embedded one, whole: {now:.120}"
    );

    // ---- and the doorway it started does not outlive the fixture ----
    //
    // **This is the producer** SKEIN-855 was looking for. `heal_fleet` reaches `ensure_fleet_door`
    // → `fleet::start_server`, which leaves a tmux server holding a loop that restarts
    // `server-doorway.py` for as long as that file exists — and nothing in this binary ever stopped
    // it. Every green `--test fleet_launch` run left one behind per fixture; the only reason they
    // were not there an hour later is that removing the fixture directory eventually took the
    // loop's own condition away with it, which is a fixture *directory* doing a teardown's job and
    // stops happening the moment a test fails and the directory is kept as evidence.
    //
    // **Presence, then absence, in that order.** An absence that was never a presence proves
    // nothing (SKEIN-833): asserted the other way round this passes on a `heal_fleet` that started
    // no server at all, which is the one outcome it must not be green about. `start_server` runs
    // `tmux new-session -d` through `own_sandbox(..).exec(..)` and returns once tmux has taken it,
    // so this is read straight out of `/proc` with nothing waited on.
    //
    // **And presence-then-absence was still not enough** (SKEIN-919). Until the teardown was
    // reordered, the absence below was green under a `kill-server` that had been replaced by
    // `list-sessions` — run, not reasoned about: 1 passed, in 0.53s. Two things made it so, and
    // both are now gone. The script removal came first, so the loop's own exit condition was
    // already false; and the `SIGKILL` sweep ran before anything was counted, so it laundered the
    // result of a kill that had done nothing. `stop_fixture` is called by hand below for exactly
    // that reason: it returns what the kill achieved, measured before either could interfere.
    //
    // **What makes each assertion fail**, run rather than reasoned about:
    //
    // * presence: nothing needed — `heal_fleet` not reaching `start_server` empties it, which is
    //   the state this suite was in before SKEIN-855.
    // * `script_was_there`: moving the `remove_file` back above the `kill-server` in
    //   `stop_fixture`. It is what keeps the next one honest.
    // * `left` empty: `kill-server` → `list-sessions`. Fails in 5.54s naming three pids — the tmux
    //   server, the supervisor shell, and the python holding 7878 — against 23.83s green for the
    //   whole binary. The `Scratch` drop that follows the panic still cleaned the fixture up
    //   through the sweep, and `node tests/ui/harness/leaks.mjs` exited 0 after it, which is the
    //   evidence that the sweep is a fallback and this assertion is about the mechanism.
    let running = fixture_processes(&root);
    assert!(
        running
            .iter()
            .any(|(_, argv)| argv.contains("server-doorway.py")),
        "`heal_fleet` came back without leaving a doorway supervisor running, so the absence \
         asserted below would be about a process that was never started. Running out of {}: {:#?}",
        root.display(),
        running
    );
    // Dropped in the order the `Scratch` doc comment requires — the pins first, so no variable is
    // left naming a directory that is already gone — and by hand rather than at the closing brace,
    // because the point is to read `/proc` on the far side of the teardown while this test can
    // still fail about it.
    let fixture = root.to_path_buf();
    drop(pins);

    // ---- the teardown, called by hand, against a fixture that is STILL ON DISK ----
    //
    // Which is the shape of the panic path — the one that keeps the directory — without needing a
    // panic to produce it, and the only shape in which the kill can be measured at all. `drop(root)`
    // below runs the same teardown a second time and then removes the directory; it is idempotent,
    // and the assertions in between are what this call is for.
    let stopped = stop_fixture(&fixture);
    // **`left` cannot be read without this** (SKEIN-919, and SKEIN-920 before it). The script is the
    // supervisor loop's own `while [ -f … ]` exit condition: gone, the loop ends itself and an empty
    // count afterwards says nothing whatever about the kill. Measured, not argued — under the order
    // this replaces, `kill-server` swapped for `list-sessions` left this whole suite green.
    assert!(
        stopped.script_was_there,
        "{} was already gone when the teardown finished counting, so the supervisor loop's exit \
         condition was FALSE for some of the wait and the count below would be empty for a \
         `kill-server` that does nothing at all. Whatever moved the removal above the kill in \
         `stop_fixture` has to be undone, not accommodated",
        fixture.join("boxes/.skein/server-doorway.py").display()
    );
    // And now the kill, and only the kill: the script is still on disk, so the loop could not have
    // ended itself, and the `SIGKILL` sweep has not run yet, so it cannot have laundered this.
    assert!(
        stopped.left.is_empty(),
        "the teardown ran against a fixture that is still on disk, with the doorway script still \
         under it — so the loop could not have ended itself — and {} process(es) outlived its \
         `kill-server` by {KILL_WINDOW:?}. Nothing but that kill can end the loop while its exit \
         condition holds, so on the path where the directory is KEPT, which is every failing test, \
         this supervisor holds the cockpit's port and restarts a python for ever (SKEIN-645): \
         {:#?}",
        stopped.left.len(),
        stopped.left
    );

    drop(root);
    let left = fixture_processes(&fixture);
    assert!(
        left.is_empty(),
        "the fixture is gone and these are still running out of it. A box whose root is deleted \
         under it is the state nothing else in this suite can observe, and the count that would \
         report it is `node tests/ui/harness/leaks.mjs`, not this suite's own result: {left:#?}"
    );
}

/// Wait until `fleet_boxes` serves what `sbx ls` is now saying about the fleet sandbox.
///
/// Changing what sbx says is not the same as skein seeing it. The answer is gated (1.5s), and once
/// it ages out the *first* caller is handed the remembered one while the refresh runs behind — that
/// is deliberate, so a wedged daemon makes the cockpit stale instead of making it stop, and it is
/// exactly why a test cannot assume its next call reflects the change it just made. Bounded, so a
/// gate that never comes round fails the assertion it was called for rather than hanging the suite.
fn await_ls(want: Option<Liveness>) {
    for _ in 0..60 {
        if fleet_boxes()
            .unwrap_or_default()
            .iter()
            .any(|b| b.name == FLEET && b.live == want)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The start a box is on, as its launcher wrote it: `<root>/tmp/skein-start-id`, the id the kit
/// suffixes its markers with. Empty when there is none.
fn start_id_of(name: &str) -> String {
    fs::read_to_string(format!("{}/tmp/skein-start-id", box_root(name)))
        .unwrap_or_default()
        .trim()
        .to_string()
}
