//! What a board tick actually forks, counted — and compared with what `signal.rs` declares.
//!
//! §10 says a signal whose cost is not known is not admissible, and that the board refresh is the
//! budget with teeth: it runs for every open browser tab every two seconds. `signal::board_tick`
//! sums what the board's signals declare they spend. This is the half that makes the sum true
//! rather than tidy — every process the tick forks is counted by a `PATH` of wrappers, and the
//! tally is compared with the declaration.
//!
//! **This is what catches a signal added without a declaration.** The exhaustive matches in
//! `signal.rs` force a *new variant* to declare, but nothing there notices a new fork in
//! `load_views` that never became a variant at all. The count does: the measured tick stops
//! matching the declared one, and the failure says by how much.
//!
//! The wrappers restore the real `PATH` before exec'ing, so what is counted is what **skein** forks
//! and not what those programs go on to fork themselves.

mod common;

use common::Scratch;
use skein::signal::{board_tick, Gates};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Every program skein spawns anywhere:
///     grep -rho 'Command::new("[^"]*")' --include=*.rs src/ | sort -u
/// Wrapped whether or not a board tick could reach it — the point of the count is to notice a fork
/// nobody predicted, so predicting which ones to watch would defeat it.
const WRAPPED: &[&str] = &[
    "bash",
    "cp",
    "curl",
    "df",
    "du",
    "gh",
    "git",
    "grep",
    "nsenter",
    "openssl",
    "osascript",
    "python3",
    "sbx",
    "sh",
    "sha256sum",
    "sleep",
    "ssh-add",
    "sysctl",
    "tar",
    "tmux",
];

const FLEET: &str = "cost-fleet";
const BOXES: u32 = 12;

fn which(tool: &str, path: &str) -> String {
    let out = Command::new("/usr/bin/env")
        .args(["sh", "-c", &format!("command -v {tool} || true")])
        .env("PATH", path)
        .output()
        .expect("look a tool up");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A `PATH` in which every program skein can spawn writes a line before it runs.
fn counting_path(dir: &Path, real_path: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir).unwrap();
    // Absolute, so the wrapper's own interpreter is not looked up through the PATH it is part of.
    let bash = which("bash", real_path);
    for tool in WRAPPED {
        let found = which(tool, real_path);
        let body = match *tool {
            // The one program with no real counterpart here. `ls` has to answer plausibly or the
            // board falls back to the registry and stops exercising the fleet path at all.
            "sbx" => "case \"$1\" in\n  ls) printf '[{\"name\":\"cost-fleet\"}]' ;;\nesac\nexit 0"
                .to_string(),
            _ if found.is_empty() => "exit 0".to_string(),
            _ => format!("exec {found} \"$@\""),
        };
        let p = dir.join(tool);
        fs::write(
            &p,
            format!(
                "#!{bash}\nprintf '%s\\n' \"$(basename \"$0\")\" >> \"$SKEIN_SPAWN_LOG\"\n\
                 export PATH=\"$SKEIN_REAL_PATH\"\n{body}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn registry(path: &Path, branch_known: bool) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let rows: Vec<String> = (0..BOXES)
        .map(|i| {
            format!(
                r#""cost-{i}":{{"branch":"{b}","dir":"/nowhere/{i}","lastSeen":"{now}","status":""}}"#,
                b = if branch_known { "feat/x" } else { "" }
            )
        })
        .collect();
    fs::write(path, format!("{{{}}}", rows.join(","))).unwrap();
}

/// The signals a cold board tick pays for, named so the arithmetic is over a list somebody can
/// check against `signal.rs` rather than over a literal.
const COLD_SIGNALS: [skein::signal::Signal; 2] = [
    skein::signal::Signal::FleetDisk,
    skein::signal::Signal::FleetLiveness,
];

/// Count what one tick forks, with the log cleared first.
fn tick(log: &Path) -> u32 {
    fs::write(log, "").unwrap();
    skein::board::load_views().expect("a board tick");
    fs::read_to_string(log).unwrap().lines().count() as u32
}

#[test]
fn a_board_tick_forks_exactly_what_its_signals_declare() {
    let real_path = std::env::var("PATH").unwrap_or_default();
    let root = Scratch::boxes("skein-board-cost");
    counting_path(&root.join("bin"), &real_path);

    let log = root.join("spawns");
    std::env::set_var("SKEIN_REAL_PATH", &real_path);
    std::env::set_var("SKEIN_SPAWN_LOG", &log);
    std::env::set_var(
        "PATH",
        format!("{}:{real_path}", root.join("bin").display()),
    );
    std::env::set_var("SKEIN_HOME", root.join("skein"));
    std::env::set_var("SKEIN_FLEET_ROOT", root.join("boxes"));
    // This suite may itself be running inside a box, and the self-box is promoted onto the board
    // whatever sbx says — an extra row, and the count is per box.
    std::env::remove_var("SANDBOX_VM_ID");
    std::env::remove_var("SKEIN_SELF");
    let reg = root.join("sandboxes.json");
    registry(&reg, true);
    std::env::set_var("SKEIN_REGISTRY", &reg);

    let mut config = skein::config::load_config();
    config.fleet_sandbox = FLEET.into();
    skein::config::save_config(&config).expect("turn the fleet on");
    for i in 0..BOXES {
        skein::place::record_place(
            &format!("cost-{i}"),
            &skein::place::PlaceRecord {
                sandbox: FLEET.into(),
                ns_pid: 1,
                home: format!("/boxes/cost-{i}/home"),
                tree: format!("/boxes/cost-{i}/tree"),
                sock: format!("/boxes/cost-{i}/session.sock"),
                generation: String::new(),
                ns_start: 0,
                launcher: String::new(),
                ceiling: String::new(),
                ..Default::default()
            },
        )
        .expect("place a box");
    }

    // ---- cold: nothing remembered, so every gated signal pays ----
    let cold = tick(&log);
    assert_eq!(
        skein::board::load_views().unwrap().len(),
        BOXES as usize,
        "the fixture must actually put every box on the board, or the per-box cost is untested"
    );
    assert_eq!(
        cold,
        board_tick(BOXES, 0, Gates::Cold).spawns,
        "a cold tick forked {cold} processes and `signal::board_tick` declares {}. Either a signal \
         was added to `load_views` without being declared in `signal.rs`, or one was declared and \
         is no longer paid.",
        board_tick(BOXES, 0, Gates::Cold).spawns
    );
    // **The number itself, summed from the Sources actually in play** — not spelled out, because a
    // literal is a number somebody has to keep in step with `source::Source::forks` by hand.
    //
    // §2.3: "each [Source] declares its cost". A fork is a property of how a subject is *reached*,
    // not of what is observed: the liveness sweep reaches by `file` and `socket` and forks nothing,
    // and reached through a process — which is how the host-driven skein reached it — it forked.
    // Same signal, same subject, different Source. Spelling `2` here asserted the host's
    // arithmetic, so this gate failed while measuring exactly what the design declares, and the
    // tempting fix was an axis on the deployment: it would have keyed the cost of a Source on a
    // predicate that SKEIN-521 then deleted outright.
    //
    // A twelve-box fleet still costs the same as a one-box one: the disk walk and the liveness
    // sweep each answer for the whole fleet. `sbx ls` used to be a third — it answered "which boxes
    // exist", which the placement records answer for free, and it now answers "what sandboxes are
    // on this machine" only when somebody asks.
    let declared: u32 = COLD_SIGNALS
        .iter()
        .flat_map(|s| s.sources())
        .map(|source| source.forks())
        .sum();
    assert_eq!(
        cold, declared,
        "a cold tick forked {cold}, and the Sources its signals declare add up to {declared}. \
         Either a signal reaches through something it does not name in `sources()`, or one of \
         those Sources costs something `source::Source::forks` does not say it does."
    );

    // ---- warm: within every gate's window, and the fleet costs nothing ----
    let warm = tick(&log);
    assert_eq!(
        warm,
        board_tick(BOXES, 0, Gates::Warm).spawns,
        "a warm tick forked {warm}"
    );
    assert_eq!(
        warm, 0,
        "gated signals must cost nothing inside their window"
    );

    // ---- and the per-box signal that used to be the exception ----
    // Nothing on record can name these branches now, so every row falls through to reading `HEAD`.
    // That was one `git rev-parse` per box per tick with no gate to amortise it — twelve forks
    // here, on a warm tick, measured before it was fixed. It is a file read now.
    registry(&reg, false);
    let unresolved = tick(&log);
    assert_eq!(
        unresolved,
        board_tick(BOXES, BOXES, Gates::Warm).spawns,
        "with no branch on record a warm tick forked {unresolved}"
    );
    assert_eq!(
        unresolved, 0,
        "the branch fallback forks again — `HEAD` is a file, and a fork here is paid per row, per \
         tick, per open browser tab"
    );

    // ---- and it forks nothing while it is actually observing something ----
    //
    // Framed as a second deployment until SKEIN-521: this arm set `SKEIN_IN_FLEET=1` and was paired
    // with a host arm that forked twice for the same two signals. One deployment survives, nothing
    // reads that variable (SKEIN-643), and what is left here is the half that has always carried
    // the weight — a fork count taken while the signals have something real to answer about.
    //
    // This is the half that makes the declaration mean something. `board_tick` SUMS what
    // `signal.rs` declares, so asserting the declaration is 0 would only prove that a constant was
    // edited. The counting `PATH` above is what makes it a measurement: every program skein can
    // spawn writes a line before it runs, so a local implementation that quietly shelled out — a
    // `du` per box, a `tmux has-session` per box — is counted here and fails the comparison.
    //
    // The boxes are given real contents first, so the walk has something to walk and the socket
    // path is actually taken. Measured against an empty fleet root, both would return early and
    // this would pass while proving nothing.
    registry(&reg, true);
    let fleet_root = root.join("boxes");
    for i in 0..BOXES {
        let dir = fleet_root.join(format!("cost-{i}"));
        fs::create_dir_all(dir.join("tree/nested")).unwrap();
        fs::write(dir.join("tree/file"), vec![b'x'; 4096]).unwrap();
        fs::write(dir.join("tree/nested/deeper"), vec![b'y'; 8192]).unwrap();
    }
    // The gates must be cleared or this measures nothing at all — and "nothing at all" reads as a
    // pass, since an unasked signal forks exactly as little as a local one. `cfg!(test)` is FALSE
    // from `tests/`: the library linked here was built without it, so the "no gate under test"
    // escape inside the module does not apply and the disk answer is remembered for 30s. The first
    // draft of this measured a warm gate and reported a triumphant 0; the assertion below that the
    // walk answered for every box is what caught it.
    skein::fleet::disturbing(
        &[
            skein::signal::Remembered::BoxDisk,
            skein::signal::Remembered::BoxLiveness,
        ],
        || {},
    );

    let observing = tick(&log);
    assert_eq!(
        observing,
        board_tick(BOXES, 0, Gates::Cold).spawns,
        "a cold tick over a fleet with contents forked {observing} processes and \
         `signal::board_tick` declares {}",
        board_tick(BOXES, 0, Gates::Cold).spawns
    );
    assert_eq!(
        observing, 0,
        "a board tick must fork NOTHING: the disk walk is a walk and the liveness sweep is a \
         `/proc` read plus a socket connect. A fork counted here is a local implementation \
         shelling out — which is how the branch fallback reached twelve forks a tick (SKEIN-49)."
    );

    // The assertion that stops the two above from passing against an implementation that simply
    // does nothing: the walk must actually produce figures for every box.
    let usage = skein::fleet::fleet_disk_usage();
    assert_eq!(
        usage.len(),
        BOXES as usize,
        "the walk answered for {} of {BOXES} boxes — a tick that forks nothing because it \
         observes nothing is not the thing being tested: {usage:?}",
        usage.len()
    );
    assert!(
        usage.values().all(|mb| *mb >= 1),
        "every box here holds 12 KiB, and `du -sxm` rounds up — a 0 means the walk counted \
         nothing: {usage:?}"
    );

    // And it is still one pass for the whole fleet, not one per box. Twelve boxes cost what one
    // costs; if this ever reads as per-box, the count above would have caught the forking kind and
    // this catches the kind that merely got slower.
    assert_eq!(
        board_tick(1, 0, Gates::Cold).spawns,
        board_tick(BOXES, 0, Gates::Cold).spawns,
        "the fleet signals must stay Scale::PerPass — one call for the whole fleet"
    );

    std::env::remove_var("SKEIN_REGISTRY");
    std::env::remove_var("SKEIN_SPAWN_LOG");
    std::env::set_var("PATH", real_path);
}
