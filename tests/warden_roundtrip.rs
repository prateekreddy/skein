//! skein asks, a person approves, `sbx` runs — the whole of delivery step 3, end to end.
//!
//! Both callers exercised, which was the argument for building the warden before anything moved.
//! Nothing here is stubbed except `sbx` itself: the warden is the real binary, built and started;
//! the approval is a real one typed at a real terminal; and skein reaches it through
//! `warden_client`, the same path `ensure_fleet` and `resize_fleet` now take.
//!
//! **Under a pty, and that is the point rather than a detail.** The warden's approval surface is
//! `/dev/tty` — it opens it, or it decides nobody is there and refuses everything (§8.1). A test
//! that could approve without a terminal would be testing a warden with a hole in it. So this one
//! allocates a pty, starts the warden inside it, and types.

use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// An `sbx` that records the argv and the environment it was given, and succeeds.
fn recording_sbx(dir: &Path, log: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("sbx");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\nprintf 'argv %s\\n' \"$*\" >> {log}\n\
             printf 'disk %s\\n' \"${{DOCKER_SANDBOXES_ROOT_SIZE:-unset}}\" >> {log}\n\
             echo made\nexit 0\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Read from the pty until `want` appears, or give up.
fn wait_for(reader: &mut Box<dyn Read + Send>, seen: &mut String, want: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut buf = [0u8; 1024];
    while Instant::now() < deadline {
        match reader.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) => {
                seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                if seen.contains(want) {
                    return true;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

#[test]
fn skein_asks_the_warden_a_person_approves_and_sbx_runs_once() {
    if !have("cargo") {
        eprintln!("skipping: no cargo on PATH, so the warden cannot be built");
        return;
    }
    let root = PathBuf::from("/var/tmp").join(format!("skein-warden-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let target = root.join("target");
    let log = root.join("sbx.log");
    recording_sbx(&root.join("bin"), &log);

    let built = Command::new("cargo")
        .args([
            "build",
            "--quiet",
            "--manifest-path",
            "warden/Cargo.toml",
            "--target-dir",
            target.to_str().unwrap(),
        ])
        .status()
        .expect("run cargo");
    assert!(built.success(), "the warden must build");

    // A pty, so `/dev/tty` exists inside the warden and its approval surface is real.
    let pty = NativePtySystem::default()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let port: u16 = 39_611;
    let mut cmd = CommandBuilder::new(target.join("debug/skein-warden").to_str().unwrap());
    cmd.env("SKEIN_WARDEN_PORT", port.to_string());
    cmd.env("SKEIN_WARDEN_HOME", root.join("state").to_str().unwrap());
    cmd.env("SKEIN_WARDEN_LS_CMD", "printf '[]'");
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let mut child = pty.slave.spawn_command(cmd).expect("start the warden");
    let mut screen = pty.master.try_clone_reader().expect("read the pty");
    let mut keyboard = pty.master.take_writer().expect("write to the pty");

    let mut seen = String::new();
    assert!(
        wait_for(&mut screen, &mut seen, "approvals are asked at"),
        "the warden did not find a terminal to ask at:\n{seen}"
    );

    // ---- skein's side: the same call `ensure_fleet` makes ----
    let warden = skein::warden_client::Warden::at("127.0.0.1", port);
    let argv: Vec<String> = vec!["create".into(), "--name".into(), "skein-fleet".into()];
    let env: Vec<(String, String)> = vec![("DOCKER_SANDBOXES_ROOT_SIZE".into(), "200g".into())];

    let asking = {
        let (argv, env) = (argv.clone(), env.clone());
        std::thread::spawn(move || {
            skein::warden_client::Warden::at("127.0.0.1", port).create("skein-fleet", &argv, &env)
        })
    };

    // ---- the person's side ----
    assert!(
        wait_for(&mut screen, &mut seen, "Type the operation id"),
        "no approval was put to the terminal:\n{seen}"
    );
    // What is on the screen is what will run — including the environment, which is most of what
    // this command does. An approval that showed the argv and not the disk size would be showing a
    // 20 GB create as a 200 GB one.
    assert!(
        seen.contains("DOCKER_SANDBOXES_ROOT_SIZE=200g"),
        "the environment was not in what the person was shown:\n{seen}"
    );
    assert!(seen.contains("sbx create skein-fleet"), "{seen}");

    let operation =
        skein::warden_client::operation_id_with_env("create", "skein-fleet", &argv, &env);
    assert!(
        seen.contains(&operation),
        "the id on the screen is not the id skein asked under:\n{seen}"
    );
    writeln!(keyboard, "{operation}").unwrap();
    keyboard.flush().unwrap();

    let answered = asking.join().unwrap().expect("the warden answered");
    assert_eq!(
        answered.happened(),
        Some(true),
        "an approved create did not happen: {}",
        answered.detail()
    );

    // ---- and the retry, which is the whole of §8.2 ----
    // Same call, same derived id, no second prompt and no second `sbx`.
    let again = warden
        .create("skein-fleet", &argv, &env)
        .expect("the warden answered the retry");
    assert!(
        matches!(again, skein::warden_client::Answered::Replayed(_)),
        "a retry was treated as new work: {again:?}"
    );

    let _ = child.kill();
    let ran = std::fs::read_to_string(&log).unwrap_or_default();
    let creates = ran.lines().filter(|l| l.starts_with("argv create")).count();
    assert_eq!(
        creates, 1,
        "sbx ran {creates} times for one operation:\n{ran}"
    );
    assert!(
        ran.contains("disk 200g"),
        "the environment did not reach the command:\n{ran}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
