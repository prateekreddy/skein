//! What `skein-server` has done to its own environment before it starts anything (SKEIN-1089).
//!
//! Its own binary, and not a test in `tests/server.rs`, because it needs nothing that file's
//! fixtures give: no token, no request beyond "are you up", no tmux. The children it looks at are
//! the ones every fleet-scope command starts first: `env`, which puts the rest of the command on a
//! fixed PATH and is itself found on the inherited one (`Place::path_pin`, `src/place/argv.rs`). A
//! stand-in `env` on `$PATH` writes down the environment it was started with and fails, so no
//! fleet-scope command runs — no tmux, no doorway behind it, nothing for a teardown to chase.

mod common;

use common::{skein_server, Scratch};
use skein::doorway::{FIRST, INHERITED_ONLY};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// Declared rather than added to the manifest, as `tests/server.rs` does and for its reason: three
// lines of the libc every Rust binary already links, all async-signal-safe, which is what
// `pre_exec` requires of what runs inside it.
extern "C" {
    fn dup(oldfd: RawFd) -> RawFd;
    fn dup2(oldfd: RawFd, newfd: RawFd) -> RawFd;
    fn close(fd: RawFd) -> i32;
}

/// Put `fd` on descriptor [`FIRST`] in the child, inheritable — the doorway's hand-over. `dup` first
/// because `dup2(3, 3)` is defined to do nothing, the `CLOEXEC` flag included (`tests/server.rs`
/// says how that was found).
fn on_the_first_descriptor(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: between fork and exec, in a child with one thread; only `dup`, `dup2` and `close`,
    // and `close` only on the copy made here.
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

struct Kid(Child);
impl Drop for Kid {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Block until something answers an HTTP request on `addr`, failing at once if the child exits.
fn until_it_answers(child: &mut Child, addr: &str) {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!("the server exited ({status}) without serving the socket it was handed");
        }
        let answered = TcpStream::connect(addr).ok().and_then(|mut s| {
            s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
            s.write_all(b"GET /api/health HTTP/1.0\r\n\r\n").ok()?;
            let mut byte = [0u8; 1];
            (s.read(&mut byte).ok()? == 1).then_some(())
        });
        if answered.is_some() {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the server did not answer on {addr} within 30s"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// **No process the server starts on its way up inherits `LISTEN_FDS` or `LISTEN_PID`**
/// (SKEIN-1089).
///
/// The server is started the way the fleet's doorway starts it — the listener on descriptor 3,
/// `LISTEN_FDS=1`, `SKEIN_LISTEN_INHERITED_ONLY=1` — with `LISTEN_PID` left out, which is the
/// older half of the convention and the dangerous one: a child that inherits `LISTEN_FDS=1` with no
/// pid beside it is told, in the convention's own words, that its descriptor 3 is a socket meant
/// for it. The children looked at are the ones every start makes: `heal_fleet` runs its repairs as
/// fleet-scope commands before the port is served — the door, the launcher, the ceilings — so once
/// the server answers, each of them has already started its `env`.
///
/// **Presence before absence.** At least one `env` must have been started, and what each wrote down
/// must carry this fixture's `SKEIN_FLEET_ROOT` — so "LISTEN_FDS is not there" is about an
/// environment this server handed on, and not about a stand-in nobody ran.
///
/// **What makes it fail**: moving `doorway::from_environment()` out of `main` and down to where the
/// port is served — which is where the variables used to be cleared, after `heal_fleet` — so the
/// stand-in `env` records `LISTEN_FDS=1` and the absence assertion names it.
#[test]
fn nothing_the_server_starts_inherits_the_socket_activation_variables() {
    // The stand-in keeps every fleet-scope command from running, so nothing should be left to stop.
    // Stopped anyway on every way out: were `env` ever pinned to an absolute path, the stand-in
    // would be skipped and `heal_fleet` would start a real supervisor under this fixture — the
    // presence assertion below says so, and this keeps that failure from also being a leak.
    let home = Scratch::temp("skein-start-order-it").quiesce_with(|home| {
        let root = home.join("fleet");
        let _ = std::fs::remove_file(root.join(".skein").join("server-doorway.py"));
        let _ = Command::new("tmux")
            .args([
                "-S",
                &skein::fleet::server_tmux_sock_in(root.to_string_lossy().as_ref()),
                "kill-server",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
    let root = home.join("fleet");
    let shims = home.join("bin");
    let seen = home.join("seen");
    std::fs::create_dir_all(&shims).unwrap();
    std::fs::create_dir_all(&seen).unwrap();
    // The real `env`, found before the stand-in is put in front of it, so the stand-in can print
    // with it without finding itself.
    let real_env = Command::new("sh")
        .args(["-c", "command -v env"])
        .output()
        .expect("sh ran");
    let real_env = String::from_utf8_lossy(&real_env.stdout).trim().to_string();
    assert!(
        real_env.starts_with('/'),
        "no `env` on PATH to stand in front of: {real_env:?}"
    );
    // Written aside and renamed into place: the server goes on starting fleet-scope commands after
    // it answers (its ticks), and a record read while its `env` was still writing it would be empty.
    let shim = shims.join("env");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n'{real_env}' > '{seen}'/.env.$$ && mv '{seen}'/.env.$$ '{seen}'/env.$$\nexit 1\n",
            seen = seen.to_string_lossy()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let addr = listener.local_addr().unwrap().to_string();
    let fd = listener.as_raw_fd();
    let mut cmd = skein_server();
    // SAFETY: the closure runs between fork and exec and calls only `dup`, `dup2` and `close`; `fd`
    // is valid for the whole of `spawn`, and the parent's copy is dropped after it returns.
    unsafe {
        cmd.pre_exec(move || on_the_first_descriptor(fd));
    }
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            shims.to_string_lossy(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )
    .env("SKEIN_HOME", home.path())
    .env("SKEIN_FLEET_ROOT", &root)
    // Never 7878: the port `heal_fleet` would open a door on, were the stand-in to let it run.
    .env("SKEIN_SERVER_PORT", free_port().to_string())
    .env("SKEIN_WARDEN", "127.0.0.1:1")
    .env("SKEIN_REGISTRY", "")
    .env_remove("SKEIN_SHARED")
    .env_remove(skein::apiauth::IN_BOX)
    .env("LISTEN_FDS", "1")
    .env_remove("LISTEN_PID")
    .env(INHERITED_ONLY, "1")
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut server = Kid(cmd.spawn().expect("the server binary spawned"));
    drop(listener);
    until_it_answers(&mut server.0, &addr);

    let records: Vec<(String, String)> = std::fs::read_dir(&seen)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read_to_string(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    assert!(
        !records.is_empty(),
        "the server answered and started no `env` on the way up, so there is no child here whose \
         environment says anything — `heal_fleet` ran no fleet-scope command, or `Place::path_pin` \
         no longer finds `env` on the inherited PATH"
    );
    let ours = format!("SKEIN_FLEET_ROOT={}", root.to_string_lossy());
    for (name, env) in &records {
        assert!(
            env.lines().any(|l| l == ours),
            "{name} does not carry {ours}, so it is not an environment this server handed on:\n{env}"
        );
        let inherited: Vec<&str> = env
            .lines()
            .filter(|l| l.starts_with("LISTEN_FDS=") || l.starts_with("LISTEN_PID="))
            .collect();
        assert!(
            inherited.is_empty(),
            "an `env` the server started on its way up ({name}) inherited {inherited:?} — the \
             socket-activation variables were cleared after the server had started children \
             (SKEIN-1089), and a child that trusts `LISTEN_FDS` with no `LISTEN_PID` takes its \
             own descriptor 3 for the cockpit's socket"
        );
    }
}

/// **Nor does a THREAD exist when they are cleared** (SKEIN-1089) — which the test above cannot
/// see, because a thread started before `from_environment` starts no child and writes no record.
/// `remove_var` beside another thread that may read the environment is the unsoundness the item
/// names, and the tokio runtime is where those threads come from.
///
/// Read off `main`'s own source, in order: inside `fn main`, `keep_from_children()` and then
/// `from_environment()` come before the runtime `Builder` and before anything that starts a thread
/// or a process. **What makes it fail**: building the runtime first and calling
/// `from_environment()` after it.
#[test]
fn main_clears_the_socket_variables_before_it_builds_a_runtime_or_starts_anything() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/skein-server/main.rs"
    ));
    let start = source
        .find("\nfn main() {\n")
        .expect("skein-server has no plain `fn main() {` — is it `#[tokio::main]` again?");
    let body = &source[start..];
    let body = &body[..body.find("\n}\n").expect("fn main has no closing brace")];
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("`fn main` no longer calls `{needle}`:\n{body}"))
    };
    let keep = at("skein::doorway::keep_from_children()");
    let clear = at("skein::doorway::from_environment()");
    let runtime = at("tokio::runtime::Builder");
    assert!(
        keep < clear,
        "`fn main` clears the socket variables before withholding the socket, and \
         `keep_from_children` reads the variables `from_environment` clears:\n{body}"
    );
    assert!(
        clear < runtime,
        "`fn main` builds the tokio runtime before `from_environment()` clears LISTEN_FDS and \
         LISTEN_PID, so `remove_var` runs beside the runtime's worker threads (SKEIN-1089):\n{body}"
    );
    for starts in [
        "std::thread::spawn",
        "thread::Builder",
        "Command::new",
        "tokio::spawn",
    ] {
        assert!(
            body.find(starts).is_none_or(|i| i > clear),
            "`fn main` reaches `{starts}` before `from_environment()` clears the socket variables \
             (SKEIN-1089):\n{body}"
        );
    }
}
