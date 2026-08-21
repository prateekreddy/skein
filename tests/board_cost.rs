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

use skein::signal::{board_tick, Gates};
use std::fs;
use std::path::{Path, PathBuf};
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

/// Count what one tick forks, with the log cleared first.
fn tick(log: &Path) -> u32 {
    fs::write(log, "").unwrap();
    skein::board::load_views().expect("a board tick");
    fs::read_to_string(log).unwrap().lines().count() as u32
}

#[test]
fn a_board_tick_forks_exactly_what_its_signals_declare() {
    let real_path = std::env::var("PATH").unwrap_or_default();
    let root = PathBuf::from("/var/tmp").join(format!("skein-board-cost-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
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
    // The number itself, spelled out: a twelve-box fleet costs the same three as a one-box one,
    // because the listing, the disk walk and the liveness sweep each answer for the whole fleet.
    assert_eq!(cold, 3, "the cold tick's three: listing, disk, liveness");

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

    std::env::remove_var("SKEIN_REGISTRY");
    std::env::remove_var("SKEIN_SPAWN_LOG");
    std::env::set_var("PATH", real_path);
    let _ = fs::remove_dir_all(&root);
}
