//! Where skein's own files live in the sandbox, and building and installing the server there
//! (`bootstrap.sh` and the build scripts).

use super::*;

/// The install, as one downloadable file — and the only implementation of the build.
///
/// At the repo ROOT rather than under `src/`, because its path is a published interface: a person
/// installs skein by fetching it raw from GitHub, so moving it breaks the line in the README and
/// every line anyone has pasted anywhere else.
const BOOTSTRAP_SH: &str = include_str!("../../bootstrap.sh");

/// Where the server binary is installed inside the fleet sandbox. Beside the launcher for the same
/// reason everything else is: the fleet root belongs to skein and outlives every box.
pub fn server_path() -> String {
    format!("{}/.skein/skein-server", fleet_root())
}

/// Where `bootstrap.sh` records the memory and CPUs somebody stated at install.
pub fn fleet_size_path() -> String {
    format!("{}/.skein/fleet-size", fleet_root())
}

/// The size somebody stated, as `(memory, cpus)`, or `None` when nobody ever did.
///
/// **The file's existence is the answer, not the numbers in it.** `Config::fleet_memory` reads back
/// this build's `26g` on a fleet nobody configured — that is what [`crate::config::configured_field`]
/// exists to see past — and `fleet_cpus` is empty on every fleet made before the gate existed. This
/// file is only written after `bootstrap.sh` has checked a stated size against what the sandbox
/// actually has, so it is the one record that means "a person decided this, and it was true".
///
/// Shell-shaped rather than JSON because the writer is `bootstrap.sh`, which runs before there is
/// any skein to parse it with.
pub fn recorded_fleet_size() -> Option<(String, String)> {
    let text = std::fs::read_to_string(fleet_size_path()).ok()?;
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Some((field("memory=")?, field("cpus=")?))
}

/// Where the `skein` CLI is installed inside the fleet sandbox — **beside the server, and that
/// adjacency is load-bearing**.
///
/// [`crate::sandbox::skein_exe`] spells `skein` as the sibling of the running executable, on the
/// premise that "the two binaries are built and installed together". That is true of a cargo build
/// directory and was false of this install, which built and copied `--bin skein-server` alone: the
/// sibling lookup found nothing, fell back to the bare name, and nothing named `skein` is on a
/// sandbox's `$PATH` either. So `launch_command` produced `skein start <box> …` and every box start
/// in-fleet died on `sh: skein: command not found` — which is the exact failure `skein_exe`'s doc
/// comment describes and believed it had closed.
pub fn skein_cli_path() -> String {
    format!("{}/.skein/skein", fleet_root())
}

/// Where skein's own checkout lives inside the sandbox, so the sandbox can build the server it
/// runs (SKEIN-448, under SKEIN-312: nothing is built or run on the host).
///
/// Under the fleet root's `.skein` **for a security reason and not for tidiness**, and that is the
/// whole of this item. `box-session.sh` puts a tmpfs over the fleet root and binds `.skein` back
/// **read-only** into every box, so anything here is readable by a box and writable by none.
/// [`shared_into_every_box`] is the other half of the same rule.
pub fn skein_source_path() -> String {
    format!("{}/.skein/src", fleet_root())
}

/// Where the Rust toolchain that builds skein lives — and **why it is not the sandbox's own**.
///
/// The sandbox already has `~/.cargo` and `~/.rustup`, and using them would be the obvious thing.
/// They are also in `box-session.sh`'s `share_paths`, bound read-write from the sandbox's real
/// `$HOME` into *every* box, under architecture §9.2's rule: **no shared writable path may contain
/// anything another box executes.** Build the server with that toolchain and the process holding
/// `credentials/`, `github-pats/` and the API token is downstream of a compiler any box can
/// overwrite — which is strictly worse than the host build it replaces, where the sandbox's most
/// privileged process is the one thing the sandbox did not build.
///
/// So `CARGO_HOME` and `RUSTUP_HOME` are pointed here explicitly. Doing nothing is the unsafe
/// option, which is exactly the kind of default worth a paragraph.
pub fn skein_toolchain_path() -> String {
    format!("{}/.skein/toolchain", fleet_root())
}

/// Every path `src/box-session.sh` binds read-write into every box, read from the launcher itself.
///
/// **Read rather than restated.** A copy of this list in Rust would be right on the day it was
/// written and would not fail when somebody adds an entry to the shell — and the entry that
/// matters is the one nobody thought about. The launcher is the authority; this parses it, so a
/// new share is a test failure rather than a silent grant.
pub fn shared_into_every_box() -> Vec<String> {
    let mut shared = Vec::new();
    for line in BOX_SESSION_SH.lines() {
        let line = line.trim();
        // Both spellings the launcher uses: the initial list, and the `+=` that adds to it.
        let Some(rest) = line
            .strip_prefix("share_paths=(")
            .or_else(|| line.strip_prefix("share_paths+=("))
        else {
            continue;
        };
        let Some(inside) = rest.split_once(')').map(|(a, _)| a) else {
            continue;
        };
        shared.extend(
            inside
                .split_whitespace()
                .map(|w| w.trim_matches('"').to_string())
                .filter(|w| !w.is_empty()),
        );
    }
    shared
}

/// Where the socket-holder is installed.
pub fn server_doorway_path() -> String {
    format!("{}/.skein/server-doorway.py", fleet_root())
}

/// Where the doorway records that it holds the cockpit's port.
///
/// The distinction this file exists to make is between *something answers on the port* and *the
/// doorway answers on the port* — a squatter satisfies the first, and publishing the cockpit's
/// mapping to one is exactly how the browser hands it the fleet token (§9.4). A TCP connect cannot
/// tell them apart; a pid that is alive and is this doorway can.
pub fn server_door_stamp_path() -> String {
    format!("{}/.skein/server.door", fleet_root())
}

/// The tmux socket the server session lives on. **Under [`fleet_private_dir`] — ISO-3, answered.**
///
/// It sat beside `private/` rather than in it for most of a year, in the half of `.skein` every box
/// can read, and the hole that left is not one a mode bit narrows: a read-only bind mount refuses
/// nothing at all to a socket, and tmux admits a client whose peer uid matches its own, which every
/// box's does under one fleet-wide uid. So every box could `connect()` to the session supervising
/// the cockpit, and tmux honours `MSG_SHELL` and `MSG_EXEC` — `run-shell` at fleet scope, from any
/// box. `tests/isolation_bwrap.rs::a_box_cannot_connect_to_the_fleets_tmux_socket` is the check,
/// and it asks the kernel rather than a bind list.
///
/// Moving it costs nothing on the skein side, which was true the whole time it did not move: every
/// caller reaches it at *sandbox* scope, outside every box, where the launcher's tmpfs never
/// applies. What held it here was that the path is spelled twice more in `bootstrap.sh`, which
/// installs the fleet and starts the same session from shell before any Rust exists to ask — and
/// moving one spelling without the other gives a fleet two tmux servers and two doorways contending
/// for the cockpit's port, which is worse than the exposure. So they moved in one commit, and
/// `a_bootstrap_run_by_hand_puts_everything_where_skein_looks_for_it` is what fails if they ever
/// stop agreeing.
///
/// **`private/` has to exist before tmux can bind here**, which is not true of the path this
/// replaces: `.skein` is made by `bootstrap.sh` long before anything starts a session. [`start_server`]
/// and `start-door.sh` each `mkdir -p` it on the line above their `tmux -S`.
///
/// **Eight bytes longer, and a unix socket path is 108** (SKEIN-442, where a deep checkout put the
/// per-box socket over the limit and the launcher died with `File name too long`). Measured rather
/// than assumed, at the longest realistic fleet roots: `/boxes` gives 33; `tests/fleet_move.rs`'s
/// `/var/tmp/skein-move-it-<pid>/boxes` gives 63; the browser tier's default
/// `/var/tmp/skein-uifix/<prefix>-<pid>-XXXXXX/fleet` gives 88 at its longest prefix. The binding
/// constraint is still the *box* socket, `<fleet root>/<box>/session.sock`, which is longer than
/// this for any box name over twelve characters and is what `tests/ui/onboarding.mjs` projects
/// against its own limit.
pub fn server_tmux_sock() -> String {
    server_tmux_sock_in(&fleet_root())
}

/// [`server_tmux_sock`] against a fleet root that is not `$SKEIN_FLEET_ROOT` — see
/// [`fleet_private_dir_in`] for why a parameter and not a literal, which is a question this path in
/// particular has already answered the expensive way.
pub fn server_tmux_sock_in(fleet_root: &str) -> String {
    format!("{}/server.tmux", fleet_private_dir_in(fleet_root))
}

/// The port the cockpit listens on **inside** the sandbox. 7878 because that is the number every
/// browser bookmark and README already carries; `$SKEIN_SERVER_PORT` overrides it in the same
/// spirit as `$SKEIN_FLEET_ROOT` — without the seam this path could only be exercised against a
/// real sandbox, and binding the real 7878 in a test collides with a real cockpit.
pub fn server_sandbox_port() -> u16 {
    std::env::var("SKEIN_SERVER_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7878)
}

/// The repository the sandbox builds skein from. Public by default, because the owner's install
/// story is "download one file, run one sbx command" and a default that needs a credential is not
/// that. `$SKEIN_SOURCE_URL` overrides — a fork, or a private mirror the sandbox has been given an
/// `sbx secret` for.
pub fn skein_source_url() -> String {
    std::env::var("SKEIN_SOURCE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://github.com/prateekreddy/skein.git".to_string())
}

/// Which revision to build. A branch, tag or sha — whatever `git checkout` takes.
///
/// **Empty by default, and that is the answer rather than a missing one.** Empty means "whatever
/// the remote's HEAD is", which is what a bare `git clone` already takes, so there is no branch
/// name here to be right about. The literal that used to be here was `main`, and this repo has no
/// `main`: the documented install 404ed fetching the bootstrap and then failed the clone
/// (SKEIN-461). A default branch is also not skein's to choose — it is a property of whatever
/// remote [`skein_source_url`] points at, including a fork.
pub fn skein_source_ref() -> String {
    std::env::var("SKEIN_SOURCE_REF")
        .ok()
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

/// Build skein-server **in the sandbox**, from the sandbox's own checkout, and install it.
///
/// This is SKEIN-312's default path: the host holds one single-use file and never a toolchain, so
/// the machine that runs the server is the machine that builds it. Minutes on a cold build, and
/// that cost is accepted — what runs is what was published.
///
/// **Three placements carry the whole security argument**, and each is a path rather than a check:
///
///   * the checkout at [`skein_source_path`] and the toolchain at [`skein_toolchain_path`], both
///     under the fleet root's `.skein`, which the launcher binds **read-only** into every box;
///   * `CARGO_HOME`/`RUSTUP_HOME` pointed at that toolchain rather than the sandbox's own, which
///     `share_paths` hands every box read-write (architecture §9.2);
///   * the binary renamed into place at [`server_path`] rather than written over — a `cat >` onto a
///     running ELF fails `ETXTBSY`, which is why an install renames into place rather than writing.
///
/// `--locked` because a build that silently resolved a different dependency tree than the one the
/// revision pins is not "what was published"; it is whatever crates.io looked like this morning.
pub fn build_server_in_sandbox(sandbox: &str) -> Result<String, String> {
    // Long, because it is a cold Rust build on a fresh sandbox: rustup, the registry, and every
    // dependency. A timeout short enough to feel "safe" here is a timeout that fails the install it
    // is meant to protect, while the sandbox does exactly what it was asked to.
    own_sandbox(sandbox)
        .exec(&build_script(), Duration::from_secs(45 * 60))
        .map(|out| out.trim().to_string())
        .map_err(|e| format!("building skein in {sandbox}: {e}"))
}

/// The build, as the shell the sandbox runs — which is [`BOOTSTRAP_SH`] and not a second copy of
/// it.
///
/// **One implementation, two entry points.** A person installing skein downloads `bootstrap.sh` and
/// hands it to `sbx exec` (SKEIN-449); the cockpit upgrading itself runs the same bytes with
/// `SKEIN_BOOTSTRAP_STOP_AFTER=build`, which stops after the binary is installed and the revision
/// printed. This used to be a Rust transcription of those steps, and a transcription of a build is
/// right on the day it is written: the two would have drifted at the first change to either, and
/// the way that failure presents is an upgrade producing a different binary from an install.
///
/// The paths are passed as environment rather than interpolated, because the script must run with
/// no skein to ask — it is the first thing that runs on a fresh sandbox.
/// `a_bootstrap_run_by_hand_puts_everything_where_skein_looks_for_it` asserts the shell derives the
/// same paths this module does, by running it.
fn build_script() -> String {
    build_script_stopping("SKEIN_BOOTSTRAP_STOP_AFTER=build\nexport SKEIN_BOOTSTRAP_STOP_AFTER\n")
}

/// ALL of `bootstrap.sh`, for [`crate::update`] (SKEIN-1031; `update::run_script` says why). Both
/// are names for the one assembler below, not copies of it.
pub fn build_script_for_update() -> String {
    build_script_stopping("")
}

/// Stop the detached session named `session` and the process group it runs — nothing else — for
/// the Update pane's Cancel (SKEIN-1037). `Ok` once the session is gone, whether this stopped it or
/// it had already ended.
///
/// **Exact, and only what that session started.** `=session` is tmux's exact-match form: a bare
/// `-t skein-update` also resolves to any session whose name merely *begins* that way. What is
/// signalled is the pane's own process group — the pane's process is a session leader, so the run
/// and everything under it share the group and nothing outside it does — with SIGTERM, before the
/// session is ended. Ending the session alone sends only SIGHUP, which anything started under
/// `nohup` ignores. No `pkill`, no pattern. `crate::update`'s `cancel_stops_the_named_run…` test runs these bytes
/// against a real tmux beside a session whose name begins the same.
///
/// Beside the build script rather than beside `detach_named` and `detached_alive` only because
/// those live in a file another piece of work is changing as this is written; the three are one
/// family and belong together.
pub fn stop_detached(sandbox: &str, session: &str) -> Result<(), String> {
    if !crate::util::valid_name(session) {
        return Err(format!("invalid session name {session:?}"));
    }
    let exact = sh_quote(&format!("={session}"));
    let pane = sh_quote(&format!("={session}:"));
    // `display-message` rather than `list-panes`: it answers for exactly one target and fails,
    // rather than listing nothing, when the target is not there. A pid that is not a number is not
    // signalled, because `kill -- -` of an empty string would be a different command.
    let script = format!(
        "pid=$(tmux display-message -p -t {pane} '#{{pane_pid}}' 2>/dev/null) || exit 0\n\
         case \"$pid\" in '' | *[!0-9]*) exit 0 ;; esac\n\
         kill -TERM -- \"-$pid\" 2>/dev/null\n\
         tmux kill-session -t {exact} 2>/dev/null\n\
         exit 0\n"
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(30))
        .map(|_| ())
}

fn build_script_stopping(stop: &str) -> String {
    format!(
        "{exports}\n{stop}{BOOTSTRAP_SH}",
        exports = bootstrap_env()
            .iter()
            .map(|(k, v)| format!("{k}={}\nexport {k}", sh_quote(v)))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// **The `sudo` on these tests' `$PATH` runs the command it was asked to, whatever options come
    /// in front of it** (SKEIN-811).
    ///
    /// The four bootstrap tests below install it, and it used to be `exec "$@"` — right only while
    /// `bootstrap.sh` put nothing before the command. With `-n` there, `exec -n mkdir …` fails and
    /// the `case "$1"` the escalation test dispatches on sees `-n` instead of `mkdir`, so both
    /// shapes are asserted: the command runs, and the line before it saw the command.
    ///
    /// **What makes it fail:** `sudo_stub` without `sudo_drops_its_own_options!()` — the `-n` half
    /// then neither marks nor makes, and the first assertion names the argv.
    #[test]
    fn the_sudo_stand_in_runs_the_command_whatever_options_come_first() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempdir();
        let bin = scratch.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let saw = scratch.join("saw-mkdir");
        let sudo = bin.join("sudo");
        std::fs::write(
            &sudo,
            format!(
                "#!/bin/sh\n{}\n",
                sudo_stub(&format!(
                    "case \"$1\" in mkdir) echo \"$*\" >> {} ;; esac",
                    saw.display()
                ))
            ),
        )
        .unwrap();
        std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).unwrap();

        for (argv, made) in [
            ("sudo -n mkdir -p made-with-n", "made-with-n"),
            ("sudo -n -E mkdir -p made-with-two", "made-with-two"),
            ("sudo mkdir -p made-bare", "made-bare"),
        ] {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(argv)
                .current_dir(&scratch)
                .env("PATH", format!("{}:{}", bin.display(), env!("PATH")))
                .output()
                .unwrap();
            let marks = std::fs::read_to_string(&saw).unwrap_or_default();
            assert!(
                out.status.success() && scratch.join(made).is_dir(),
                "`{argv}` did not run its command through the stand-in: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                marks.lines().any(|l| l == format!("mkdir -p {made}")),
                "the line before the command saw something other than the command for `{argv}`: \
                 {marks:?}"
            );
        }
    }

    /// Nothing the sandbox builds skein with is writable by a box.
    ///
    /// This is SKEIN-448's whole point, and the failure it guards against is quiet: build the
    /// server with the sandbox's own `~/.cargo` and everything works, for ever, while the process
    /// holding `credentials/` and the API token is compiled by a toolchain every box can rewrite
    /// (architecture §9.2 — "no shared writable path may contain anything another box executes").
    /// There is no symptom. There is only the property, so the property is what is asserted.
    ///
    /// The shared list is READ FROM `box-session.sh`, not restated here: a copy would be correct
    /// the day it was written and would not fail when somebody adds an entry to the shell, and the
    /// entry that matters is the one nobody thought about. Adding `.skein` to `share_paths` fails
    /// this test.
    #[test]
    fn nothing_the_sandbox_builds_skein_with_is_writable_by_a_box() {
        let _env = env_lock();
        std::env::set_var("SKEIN_FLEET_ROOT", "/boxes");

        let shared = shared_into_every_box();
        assert!(
            shared.iter().any(|s| s == ".cargo") && shared.iter().any(|s| s == ".rustup"),
            "box-session.sh no longer shares .cargo/.rustup, so this test is asserting against a \
             rule that has changed — read the launcher again before deleting it. Found: {shared:?}"
        );

        // **Not "is it under a share_path"**, which is the check this test had first and which can
        // never fail: `share_paths` entries are `$HOME`-relative, and the fleet root is not under
        // `$HOME`, so `$HOME/.cargo` and `/boxes/.skein` can never overlap however wrong the
        // placement gets. Adding `.skein` to the launcher's list passed it. A test that cannot fail
        // is worse than no test, because it reads as cover.
        //
        // The two things that CAN go wrong are these. Anywhere under the sandbox's own `$HOME` is
        // either shared read-write into every box or shadowed by each box's private home — neither
        // is somewhere skein's toolchain can live. And the fleet root's `.skein` is the one place
        // boxes get read-only, so being merely *outside* the shared set is not enough: `/tmp` is
        // outside it too, and every box can write there.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/agent".into());
        for path in [skein_source_path(), skein_toolchain_path()] {
            assert!(
                !path.starts_with(&format!("{home}/")),
                "{path} is under the sandbox's own home, where {shared:?} are bound read-write into \
                 every box — a box could overwrite the compiler the fleet's server is built with"
            );
            assert!(
                path.starts_with(&format!("{}/.skein/", fleet_root())),
                "{path} is outside the fleet root's .skein, which is the directory boxes get \
                 read-only — being merely unshared is not the same as being unwritable"
            );
        }

        // The launcher's side of the same claim, so this test fails if the ro-bind is dropped.
        assert!(
            BOX_SESSION_SH.contains("--ro-bind \"$fleet_root_dir/.skein\" \"$fleet_root_dir/.skein\""),
            "box-session.sh no longer binds the fleet root's .skein read-only, so everything under \
             it — the server binary, its source and the toolchain that built it — is writable by \
             every box"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// A size declaration, and the machine it is a declaration *about*, for the bootstrap tests
    /// that are not asking about sizing.
    ///
    /// The gate refuses an install whose memory and CPUs were never stated, so every test that runs
    /// [`BOOTSTRAP_SH`] has to state them. The reason this is a helper rather than three `.env`
    /// calls is the second half: it pins the **machine** as well as the claim. Left to read the real
    /// `/proc/meminfo` and the real `nproc`, these tests pass on the laptop they were written on and
    /// refuse on the next one — and they proved it, by reading the developer's own 11-CPU sandbox
    /// and their live `config.json` the first time the gate ran under them.
    ///
    /// `nproc` is stubbed into the same `bin` the other stubs go in, so it is found the same way.
    fn stated_size(
        bin: &std::path::Path,
        scratch: &std::path::Path,
    ) -> Vec<(&'static str, String)> {
        use std::os::unix::fs::PermissionsExt;
        let at = bin.join("nproc");
        std::fs::write(&at, "#!/bin/sh\necho 4\n").unwrap();
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        // 4 GiB exactly, against a declared 4g: the gate allows the VM's own overhead below the
        // number asked for, and nothing above it.
        let meminfo = scratch.join("meminfo");
        std::fs::write(&meminfo, "MemTotal:        4194304 kB\n").unwrap();
        vec![
            ("SKEIN_MEMINFO", meminfo.to_string_lossy().into_owned()),
            ("SKEIN_FLEET_MEMORY", "4g".to_string()),
            ("SKEIN_FLEET_CPUS", "4".to_string()),
        ]
    }

    /// Memory and CPUs must be *stated*, and the statement is checked against the sandbox.
    ///
    /// sbx fixes both at create and has no resize, so getting them wrong costs the sandbox and every
    /// box checkout on its disk — and omitting the flags is not an error, it is sbx quietly taking
    /// half the host's memory and all of its cores. The gate exists so that cannot happen silently.
    ///
    /// **Stating alone would be a rubber stamp**, so the third and fourth cases matter most: a
    /// create that forgot `-m` and an exec that claims `26g` are a matched pair of assertions about
    /// a sandbox that has neither, and it is what the sandbox HAS that is permanent.
    ///
    /// The `cargo` assertions are the ones a refactor would break. A refusal that arrives after the
    /// build is a refusal that cost the build, and this gate sits where it does on purpose — so the
    /// test pins both directions: nothing compiled when it refused, and something did when it did
    /// not. Without the second, a gate that refused every install would pass this test.
    #[test]
    fn an_install_whose_size_was_never_stated_refuses_before_it_builds() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = crate::testutil::tempdir();

        // Each case gets its own fleet root, so `fleet-size` written by one cannot answer for the
        // next — which is exactly the mistake the gate's own three-source precedence could hide.
        let run = |case: &str, cpus: &str, mem_kb: u64, env: Vec<(&str, &str)>| {
            let root = scratch.join(case);
            let bin = root.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let log = root.join("ran.log");
            let stub = |name: &str, body: &str| {
                let at = bin.join(name);
                std::fs::write(
                    &at,
                    format!(
                        "#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> {log}\n{body}\n",
                        log = log.display()
                    ),
                )
                .unwrap();
                std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
            };
            stub("nproc", &format!("echo {cpus}"));
            stub(
                "git",
                "case \"$*\" in *rev-parse*) echo deadbee ;; esac\nexit 0",
            );
            stub(
                "cargo",
                &format!(
                    "mkdir -p {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\nexit 0",
                    src = root.join(".skein/src").display(),
                ),
            );
            let meminfo = root.join("meminfo");
            std::fs::write(&meminfo, format!("MemTotal: {mem_kb} kB\n")).unwrap();

            let mut cmd = std::process::Command::new("bash");
            cmd.arg("-c")
                .arg(BOOTSTRAP_SH)
                .env("PATH", format!("{}:{}", bin.display(), env!("PATH")))
                .env_remove("BASH_ENV")
                .env_remove("SKEIN_FLEET_MEMORY")
                .env_remove("SKEIN_FLEET_CPUS")
                .env("SKEIN_FLEET_ROOT", &root)
                .env("SKEIN_HOME", root.join("home"))
                .env("SKEIN_MEMINFO", &meminfo)
                .env("SKEIN_BOOTSTRAP_STOP_AFTER", "build");
            for (k, v) in env {
                cmd.env(k, v);
            }
            let out = cmd.output().expect("bootstrap.sh ran");
            let said = String::from_utf8_lossy(&out.stderr).to_string();
            let ran = std::fs::read_to_string(&log).unwrap_or_default();
            (out.status.success(), said, ran, root)
        };

        // 4 GiB in kB, and the same figure a little short of it: a VM keeps some of its own memory
        // back (247 MiB of a 26g fleet, measured), so the gate allows below and never above.
        const FOUR_GIB_KB: u64 = 4 * 1024 * 1024;

        let (ok, said, ran, _) = run("unstated", "4", FOUR_GIB_KB, vec![]);
        assert!(
            !ok,
            "an install that stated no size was allowed to proceed:\n{said}"
        );
        assert!(
            said.contains("never stated"),
            "the refusal did not say the size was never stated:\n{said}"
        );
        assert!(
            !ran.contains("cargo"),
            "the size was refused only AFTER the build ran, which is the cost the gate's placement \
             exists to avoid:\n{ran}"
        );

        let (ok, said, ran, _) = run(
            "wrong-cpus",
            "4",
            FOUR_GIB_KB,
            vec![("SKEIN_FLEET_MEMORY", "4g"), ("SKEIN_FLEET_CPUS", "2")],
        );
        assert!(
            !ok,
            "a fleet claiming 2 CPUs on a 4-CPU sandbox was allowed:\n{said}"
        );
        assert!(
            said.contains("asked for 2 CPUs and has 4"),
            "the refusal did not name both CPU counts:\n{said}"
        );
        assert!(
            !ran.contains("cargo"),
            "it built before refusing the CPU count:\n{ran}"
        );

        let (ok, said, _, _) = run(
            "wrong-memory",
            "4",
            FOUR_GIB_KB,
            vec![("SKEIN_FLEET_MEMORY", "26g"), ("SKEIN_FLEET_CPUS", "4")],
        );
        assert!(
            !ok,
            "a fleet claiming 26g on a 4 GiB sandbox was allowed:\n{said}"
        );
        assert!(
            said.contains("asked for 26g"),
            "the refusal did not name the memory it was told to expect:\n{said}"
        );

        // Stated, correct, and the build proceeds — without this the three refusals above would all
        // pass against a gate that simply never let anything through.
        let (ok, said, ran, root) = run(
            "stated",
            "4",
            FOUR_GIB_KB,
            vec![("SKEIN_FLEET_MEMORY", "4g"), ("SKEIN_FLEET_CPUS", "4")],
        );
        assert!(ok, "a correctly stated size was refused:\n{said}");
        assert!(
            ran.contains("cargo"),
            "a stated, matching size did not reach the build:\n{ran}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".skein/fleet-size")).unwrap_or_default(),
            "memory=4g\ncpus=4\n",
            "the size that was checked was not recorded, so `skein doctor` cannot tell a chosen \
             size from one sbx picked"
        );
    }

    /// The bootstrap, RUN — with git and cargo stubbed, because the only parts this machine
    /// cannot do are the network fetch and a cold Rust build.
    ///
    /// This used to assert substrings of a script Rust built. That could only ever check that the
    /// text said the right thing; it could not check that the text *works*, and the text is now a
    /// file a person downloads and runs by hand, where "works" is the entire requirement. So the
    /// stubs record their argv and the assertions are about what actually happened: where cargo was
    /// pointed, what it was asked to build, and where the binary ended up.
    ///
    /// The two variables are the whole security property and they fail **silently**: unset, the
    /// build succeeds against the sandbox's shared toolchain, which `box-session.sh` binds
    /// read-write into every box (architecture §9.2).
    #[test]
    fn the_bootstrap_builds_with_the_private_toolchain_and_renames_the_binary_into_place() {
        // The root is pinned in THIS process as well as in the two children below, and the lock is
        // what lets it be. The children always carried it; the two `skein_cli_path`/`server_path`
        // assertions at the end are resolved here, and with the variable unset they were answered
        // `/boxes/.skein/…` — a pair of paths under the live fleet, agreeing with each other for a
        // reason that had nothing to do with what this test installed. `util::fleet_root` refuses
        // an unpinned test now (SKEIN-690), and pinning it moves those two back onto the fixture.
        let _g = crate::testutil::env_lock();
        let scratch = crate::testutil::tempdir();
        let root = scratch.join("fleet");
        std::env::set_var("SKEIN_FLEET_ROOT", &root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = root.join("ran.log");

        // Stubs that record and then do the least they can get away with. `git` answers the one
        // question the script asks of it; `cargo` produces the file the script will install.
        let stub = |name: &str, body: &str| {
            let at = bin.join(name);
            std::fs::write(
                &at,
                format!(
                    "#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> {log}\nCARGO_HOME_SEEN=\"$CARGO_HOME\"\nprintf '{name}-cargo-home %s\\n' \"$CARGO_HOME_SEEN\" >> {log}\n{body}\n",
                    log = log.display(),
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        stub(
            "git",
            "case \"$*\" in *rev-parse*) echo deadbee ;; esac\nexit 0",
        );
        stub(
            "cargo",
            &format!(
                "mkdir -p {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\nexit 0",
                src = root.join(".skein/src").display(),
            ),
        );

        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(BOOTSTRAP_SH)
            .env("PATH", format!("{}:{}", bin.display(), env!("PATH")))
            // See `the_image_is_given_everything_the_install_runs_before_it_runs_it`: `$BASH_ENV` is
            // sourced ahead of the script and can put a real cargo in front of the stub.
            .env_remove("BASH_ENV")
            .env("SKEIN_FLEET_ROOT", &root)
            .envs(stated_size(&bin, &scratch))
            // Pinned for the same reason as the machine above: the volume discovery now runs
            // BEFORE the build, so an unset `$SKEIN_HOME` sends these tests reading the real
            // `/proc/self/mountinfo` — and they found the developer's live fleet when it did.
            .env("SKEIN_HOME", scratch.join("home"))
            .env("SKEIN_BOOTSTRAP_STOP_AFTER", "build")
            .env("SKEIN_SOURCE_REF", "some-branch")
            .output()
            .expect("bootstrap.sh ran");
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "the bootstrap failed:\n{said}\nit ran:\n{ran}"
        );

        let private = root.join(".skein/toolchain/cargo");
        assert!(
            ran.contains(&format!("cargo-cargo-home {}", private.display())),
            "cargo was not pointed at the private toolchain at {}, so it used the sandbox's own — \
             which box-session.sh binds read-write into every box, putting the process that holds \
             the fleet's credentials downstream of a compiler any box can overwrite.\nit ran:\n{ran}",
            private.display(),
        );
        assert!(
            ran.contains("--locked"),
            "the build is not --locked, so it resolves whatever crates.io looks like today rather \
             than what the revision pins:\n{ran}"
        );
        assert!(
            ran.contains("some-branch"),
            "SKEIN_SOURCE_REF was ignored, so an upgrade cannot ask for a revision:\n{ran}"
        );

        // And with no ref asked for, the clone names no branch at all — a bare clone takes the
        // remote's own default, which is the whole of SKEIN-461's fix. Asserted by running it,
        // because "the shell takes the other arm of the `if`" is not something reading it proves.
        let fresh = scratch.join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        let log2 = fresh.join("ran.log");
        std::fs::write(
            bin.join("cargo"),
            format!(
                "#!/bin/sh\nprintf 'cargo %s\\n' \"$*\" >> {log2}\nmkdir -p \
                 {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\nexit 0\n",
                log2 = log2.display(),
                src = fresh.join(".skein/src").display(),
            ),
        )
        .unwrap();
        std::fs::write(
            bin.join("git"),
            format!(
                "#!/bin/sh\nprintf 'git %s\\n' \"$*\" >> {log2}\ncase \"$*\" in \
                 *rev-parse*) echo deadbee ;; esac\nexit 0\n",
                log2 = log2.display(),
            ),
        )
        .unwrap();
        let out2 = std::process::Command::new("bash")
            .arg("-c")
            .arg(BOOTSTRAP_SH)
            .env("PATH", format!("{}:{}", bin.display(), env!("PATH")))
            // See `the_image_is_given_everything_the_install_runs_before_it_runs_it`: `$BASH_ENV` is
            // sourced ahead of the script and can put a real cargo in front of the stub.
            .env_remove("BASH_ENV")
            .env("SKEIN_FLEET_ROOT", &fresh)
            .envs(stated_size(&bin, &scratch))
            .env("SKEIN_HOME", scratch.join("home"))
            .env("SKEIN_BOOTSTRAP_STOP_AFTER", "build")
            .env_remove("SKEIN_SOURCE_REF")
            .output()
            .expect("bootstrap.sh ran with no ref");
        let ran2 = std::fs::read_to_string(&log2).unwrap_or_default();
        assert!(
            out2.status.success(),
            "the bootstrap failed with no ref asked for:\n{}\n{ran2}",
            String::from_utf8_lossy(&out2.stderr)
        );
        let cloned = ran2
            .lines()
            .find(|l| l.starts_with("git clone"))
            .unwrap_or_else(|| panic!("nothing cloned:\n{ran2}"));
        assert!(
            !cloned.contains("--branch"),
            "with no SKEIN_SOURCE_REF the clone still names a branch, so it cannot take the \
             remote's own default: {cloned}"
        );
        // The binary is where skein will look for it, and it arrived by rename — a `cp` onto a
        // running ELF fails ETXTBSY, so an upgrade against a live fleet would refuse to install.
        assert!(
            root.join(".skein/skein-server").exists(),
            "nothing was installed at the server path:\n{ran}"
        );
        // The CLI, BESIDE the server. `sandbox::skein_exe` resolves `skein` as the sibling of the
        // running executable, so installing the server alone left every box start running a bare
        // `skein` that is on no sandbox's PATH: `sh: skein: command not found`. Asserted as a
        // sibling rather than merely as a file, because adjacency is the whole contract.
        assert_eq!(
            std::path::Path::new(&skein_cli_path()).parent(),
            std::path::Path::new(&server_path()).parent(),
            "the CLI and the server are no longer installed in the same directory, so \
             `skein_exe`'s sibling lookup cannot find one from the other"
        );
        assert!(
            root.join(".skein/skein").is_file(),
            "the `skein` CLI was not installed beside the server, so `launch_command` builds a bare \
             `skein start …` and every box start dies on `sh: skein: command not found`:\n{ran}"
        );
        assert!(
            ran.contains("--bin skein") && ran.contains("--bin skein-server"),
            "the build did not ask for both binaries:\n{ran}"
        );
        assert!(
            !root.join(".skein/skein-server.new").exists(),
            "the staging file was left behind, so the install was a copy and not a rename:\n{ran}"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "deadbee",
            "the build did not report the revision it built, which is what the cockpit records"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// A rustup shim is not a toolchain, and the gate has to ask for the toolchain.
    ///
    /// The install got as far as the clone and then died on the build:
    ///
    ///     error: rustup could not choose a version of cargo to run, because one wasn't specified
    ///     explicitly, and no default is configured.
    ///
    /// The gate was `command -v cargo`, and that answer is worthless *in this script* — because
    /// three lines above it the script points `RUSTUP_HOME` at the private toolchain, which is
    /// empty until the gate fills it. So every rustup shim on the PATH keeps answering `command
    /// -v` while resolving against a rustup home with no default in it: the image's own
    /// `~/.cargo/bin/cargo`, and the one an interrupted earlier run of this very script left under
    /// `$CARGO_HOME/bin`. Both are a file called cargo that cannot build.
    ///
    /// Two arms, because there are two states and only one of them is fixed by installing:
    ///
    ///   1. **A shim on the PATH.** rustup-init has to run. Restore `command -v cargo` and it does
    ///      not, and the build dies exactly as it did in the sandbox.
    ///   2. **A shim under `$RUSTUP_HOME` already.** rustup-init *runs* and does not help: finding
    ///      a rustup it can update, it leaves the toolchains alone and never honours
    ///      `--default-toolchain`. Only `rustup default stable` names one. Delete that line and
    ///      this arm reaches the refusal.
    ///
    /// The stubs are arranged so the fix is what makes them work, rather than the assertion being
    /// about which line the script contains: the cargo that can build only ever comes into
    /// existence *inside* the block being tested.
    #[test]
    fn a_rustup_shim_that_cannot_choose_a_toolchain_is_not_a_cargo() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = crate::testutil::tempdir();

        // One arm. `emits` is the rustup-init that this arm's `curl` will print down the pipe —
        // the only thing in the test allowed to produce a working cargo.
        let arm = |name: &str, emits: &str| -> (std::process::Output, String, std::path::PathBuf) {
            let root = scratch.join(name);
            let bin = root.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let log = root.join("ran.log");
            let src = root.join(".skein/src");

            let write = |at: std::path::PathBuf, body: String| {
                std::fs::write(&at, body).unwrap();
                std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
            };
            // The image's shim: it is on the PATH, it is called cargo, and it cannot build.
            write(
                bin.join("cargo"),
                format!(
                    "#!/bin/sh\nprintf 'cargo-shim %s\\n' \"$*\" >> {log}\nprintf 'error: rustup \
                     could not choose a version of cargo to run\\n' >&2\nexit 1\n",
                    log = log.display()
                ),
            );
            write(
                bin.join("git"),
                format!(
                    "#!/bin/sh\nprintf 'git %s\\n' \"$*\" >> {log}\ncase \"$*\" in *rev-parse*) \
                     echo deadbee ;; esac\nexit 0\n",
                    log = log.display()
                ),
            );
            write(
                bin.join("curl"),
                format!(
                    "#!/bin/sh\nprintf 'curl %s\\n' \"$*\" >> {log}\ncase \"$*\" in\n  \
                     *sh.rustup.rs*)\n{emits}\n    ;;\n  *) exit 1 ;;\nesac\nexit 0\n",
                    log = log.display()
                ),
            );
            // Silences the apt step rather than testing it — `the_image_is_given_everything_the_\
            // install_runs_before_it_runs_it` owns that, and a real `sudo apt-get` here would be
            // a network round trip inside a unit test.
            for present in ["cc", "tmux", "jq", "python3"] {
                write(bin.join(present), "#!/bin/sh\nexit 0\n".to_string());
            }

            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(BOOTSTRAP_SH)
                .env("PATH", format!("{}:{}", bin.display(), env!("PATH")))
                // `$BASH_ENV` is sourced ahead of the script and puts a real, working cargo in
                // front of the stub — which is the one thing this test must not have.
                .env_remove("BASH_ENV")
                .env("SKEIN_FLEET_ROOT", &root)
                .envs(stated_size(&bin, &scratch))
                .env("SKEIN_HOME", scratch.join("home"))
                .env("SKEIN_BOOTSTRAP_STOP_AFTER", "build")
                .env_remove("SKEIN_SOURCE_REF")
                .output()
                .expect("bootstrap.sh ran");
            let ran = std::fs::read_to_string(&log).unwrap_or_default();
            let _ = src;
            (out, ran, root)
        };

        // 1. rustup-init installs a cargo that works. Nothing else in this arm can.
        let installs = format!(
            "    cat <<'RUSTUP'\n#!/bin/sh\nmkdir -p \"$CARGO_HOME/bin\"\ncat > \
             \"$CARGO_HOME/bin/cargo\" <<'CARGO'\n{cargo}CARGO\nchmod 755 \
             \"$CARGO_HOME/bin/cargo\"\nRUSTUP",
            cargo = good_cargo(
                &scratch.join("shim/ran.log"),
                &scratch.join("shim/.skein/src")
            ),
        );
        let (out, ran, root) = arm("shim", &installs);
        assert!(
            out.status.success(),
            "a rustup shim on the PATH answered the gate, so no toolchain was installed and the \
             build died the way it died in the sandbox:\n{}\nit ran:\n{ran}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            ran.contains("curl") && ran.contains("sh.rustup.rs"),
            "rustup-init was never fetched, so the gate was satisfied by a shim that cannot \
             build:\n{ran}"
        );
        assert!(
            ran.contains("cargo-good build"),
            "the build did not run under the toolchain the gate installed:\n{ran}"
        );
        assert!(
            root.join(".skein/skein-server").is_file() && root.join(".skein/skein").is_file(),
            "nothing was installed:\n{ran}"
        );

        // 2. rustup-init runs, updates the rustup it found, and leaves the toolchains alone — so
        //    cargo still cannot choose one until something names a default.
        let leaves_it_alone = format!(
            "    cat <<'RUSTUP'\n#!/bin/sh\nmkdir -p \"$CARGO_HOME/bin\"\ncat > \
             \"$CARGO_HOME/bin/cargo\" <<'CARGO'\n#!/bin/sh\nprintf 'cargo-still-shim %s\\n' \
             \"$*\" >> {log}\nexit 1\nCARGO\ncat > \"$CARGO_HOME/bin/rustup\" \
             <<'RUSTUPBIN'\n#!/bin/sh\nprintf 'rustup %s\\n' \"$*\" >> {log}\ncase \"$*\" in\n  \
             'default stable')\n    cat > \"$CARGO_HOME/bin/cargo\" <<'CARGO2'\n{cargo}CARGO2\n    \
             chmod 755 \"$CARGO_HOME/bin/cargo\"\n    ;;\nesac\nexit 0\nRUSTUPBIN\nchmod 755 \
             \"$CARGO_HOME/bin/cargo\" \"$CARGO_HOME/bin/rustup\"\nRUSTUP",
            log = scratch.join("half/ran.log").display(),
            cargo = good_cargo(
                &scratch.join("half/ran.log"),
                &scratch.join("half/.skein/src")
            ),
        );
        let (out2, ran2, root2) = arm("half", &leaves_it_alone);
        assert!(
            out2.status.success(),
            "an install that found a rustup to update left it with no default toolchain, and \
             nothing named one — which is what an interrupted earlier run leaves behind, and it \
             cannot be got out of by running the install again:\n{}\nit ran:\n{ran2}",
            String::from_utf8_lossy(&out2.stderr)
        );
        assert!(
            ran2.contains("rustup default stable"),
            "no default toolchain was named after an install that would not name one:\n{ran2}"
        );
        assert!(
            ran2.contains("cargo-good build")
                && root2.join(".skein/skein-server").is_file()
                && root2.join(".skein/skein").is_file(),
            "the repaired toolchain did not go on to build:\n{ran2}"
        );
    }

    /// The body of a `cargo` stub that can build: it records, and it leaves the two binaries the
    /// install renames into place. Shared by the arms above because in both of them it is what
    /// rustup-init writes, and the two arms differ only in what it takes to get there.
    fn good_cargo(log: &std::path::Path, src: &std::path::Path) -> String {
        format!(
            "#!/bin/sh\nprintf 'cargo-good %s\\n' \"$*\" >> {log}\nmkdir -p \
             {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' \
             > {src}/target/release/skein\nexit 0\n",
            log = log.display(),
            src = src.display(),
        )
    }

    /// The install's very first write is at the filesystem root, and it escalates for it.
    ///
    /// The whole install stopped here — `sbx exec -i skein-fleet bash < bootstrap.sh` printed two
    /// bare `mkdir: Permission denied` lines and nothing else, which are the two arguments of the
    /// script's `mkdir -p "$src" "$toolchain"`: `/boxes` is at the filesystem root and the sandbox
    /// user cannot create it. [`ensure_fleet_root`] has escalated for exactly this since long
    /// before `bootstrap.sh` existed, but it runs from a skein binary and the bootstrap's job is to
    /// build the first one, so the step had to be carried across.
    ///
    /// The reason it shipped broken is written into [`ensure_fleet_root`]'s own doc comment: every
    /// test points `$SKEIN_FLEET_ROOT` at a writable temp dir, so the seam that makes this file
    /// testable at all is the seam that hides its first real step. This test is that comment's
    /// answer — a fleet root whose PARENT is unwritable, so the plain `mkdir -p` fails the way it
    /// failed in the sandbox and the only way through is the escalation.
    ///
    /// `sudo` is stubbed rather than real: the assertion is that the script reaches for it with the
    /// right argv, and a test that needed a password (or a passwordless sudoers) would be a test
    /// about the machine it runs on. Delete the escalation and the bootstrap exits non-zero here.
    #[test]
    fn the_bootstrap_escalates_for_a_fleet_root_it_cannot_create() {
        let scratch = crate::testutil::tempdir();
        let bin = scratch.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = scratch.join("ran.log");

        // The unwritable parent, and the root beneath it. Mode 0o555 on a directory we own is
        // enough: the write bit is what `mkdir` needs, and owning it is what lets the stubbed sudo
        // put it back — which is also how the temp dir can be cleaned up afterwards.
        use std::os::unix::fs::PermissionsExt;
        let locked = scratch.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        let root = locked.join("boxes");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(
            std::fs::create_dir(locked.join("probe"))
                .expect_err("the fixture's parent is still writable")
                .kind(),
            std::io::ErrorKind::PermissionDenied,
            "the fixture's parent can be written to after all, so the script's plain `mkdir -p` \
             would succeed and this test would assert nothing about escalation — which is exactly \
             how the bug shipped"
        );

        let stub = |name: &str, body: &str| {
            let at = bin.join(name);
            std::fs::write(
                &at,
                format!(
                    "#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> {log}\n{body}\n",
                    log = log.display(),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // The stub is what a real sudo would be: it runs the command. It only has to lift the write
        // bit first, because everything downstream of the mkdir — the chown to our own uid, the
        // chmod on a directory we own — an unprivileged process can already do.
        stub(
            "sudo",
            &non_interactive_sudo_stub(&format!(
                "case \"$1\" in mkdir) chmod u+w {locked} ;; esac",
                locked = locked.display(),
            )),
        );
        stub(
            "git",
            "case \"$*\" in *rev-parse*) echo deadbee ;; esac\nexit 0",
        );
        stub(
            "cargo",
            &format!(
                "mkdir -p {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\nexit 0",
                src = root.join(".skein/src").display(),
            ),
        );

        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(BOOTSTRAP_SH)
            // A working directory of its own, so a stub that misread its arguments writes into the
            // fixture and not the repository (SKEIN-811; `fleet::limits`'s harness has the story).
            .current_dir(&scratch)
            .env("PATH", format!("{}:{}", bin.display(), env!("PATH")))
            // See `the_image_is_given_everything_the_install_runs_before_it_runs_it`: `$BASH_ENV` is
            // sourced ahead of the script and can put a real cargo in front of the stub.
            .env_remove("BASH_ENV")
            .env("SKEIN_FLEET_ROOT", &root)
            .envs(stated_size(&bin, &scratch))
            // Pinned for the same reason as the machine above: the volume discovery now runs
            // BEFORE the build, so an unset `$SKEIN_HOME` sends these tests reading the real
            // `/proc/self/mountinfo` — and they found the developer's live fleet when it did.
            .env("SKEIN_HOME", scratch.join("home"))
            .env("SKEIN_BOOTSTRAP_STOP_AFTER", "build")
            .output()
            .expect("bootstrap.sh ran");
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        let said = String::from_utf8_lossy(&out.stderr).to_string();

        // Put the parent back before any assertion can panic, so a failure does not also leave an
        // undeletable temp directory behind.
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));

        assert!(
            out.status.success(),
            "the bootstrap could not create a fleet root it does not have permission to mkdir, \
             which is every real install: `/boxes` is at the filesystem root.\nit said:\n{said}\n\
             it ran:\n{ran}"
        );
        assert!(
            ran.contains(&format!("sudo -n mkdir -p {}", root.display())),
            "the fleet root was not created with `sudo -n`, so it was made some other way that \
             will not work at the filesystem root, or by a sudo that can wait on a password prompt \
             nobody sees (SKEIN-1038):\nit ran:\n{ran}"
        );
        assert!(
            root.join(".skein/src").is_dir() && root.join(".skein/toolchain").is_dir(),
            "the fleet root exists but is not usable by the sandbox user — the escalation created \
             it and did not hand it over, which is the `mkdir: Permission denied` again one \
             directory deeper:\nit ran:\n{ran}"
        );
        assert!(
            root.join(".skein/skein-server").exists(),
            "the build did not finish past the fleet root:\n{said}\nit ran:\n{ran}"
        );
    }

    /// The `shell` image ships none of what this script runs, and it is given all of it, once.
    ///
    /// The `shell` image ships no compiler. Past the fleet root, the install therefore downloaded
    /// every crate and then failed the first three build scripts it tried to link — `libc`,
    /// `proc-macro2`, `quote` — with `error: linker `cc` not found`. Nothing skein depends on needs
    /// a C library; a Rust toolchain is simply not a build on its own.
    ///
    /// The seam that hid this is the one that hid the fleet root: every other test of this file
    /// inherits the developer's `$PATH`, where `cc` has always been. So this test builds the PATH
    /// instead of inheriting it — the handful of real binaries the script actually runs, and
    /// nothing else — which is the only way "the image does not have it" is a state a test can be
    /// in. `apt-get` is stubbed, and its stub does what the real one would: it puts `cc` on the
    /// PATH. A stub that only recorded would leave the script correctly refusing to continue.
    ///
    /// `cargo` is stubbed too, and its stub refuses to link when `cc` is absent — the one thing
    /// about the real cargo this is about. So deleting the apt step does not merely drop a line
    /// from the log; it reproduces the failure, with the sandbox's own words in it.
    #[test]
    fn the_image_is_given_everything_the_install_runs_before_it_runs_it() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = crate::testutil::tempdir();
        let bin = scratch.join("bin");
        let sys = scratch.join("sys");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&sys).unwrap();

        // The real programs the script runs that no stub can stand in for, symlinked one by one so
        // that everything absent from this list is absent from the run. `bash` is here because
        // `Command` resolves the program through the PATH it is given, not the one it inherits.
        let real = |name: &str| -> std::path::PathBuf {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {name}"))
                .output()
                .expect("looked for a program");
            let at = String::from_utf8_lossy(&out.stdout).trim().to_string();
            assert!(
                !at.is_empty(),
                "this machine has no {name}, so the fixture cannot be built"
            );
            std::path::PathBuf::from(at)
        };
        // `awk` and `sed` joined this list when the size gate did. They were always in the file —
        // the volume discovery has always used `awk` — but that ran after the build, so a run that
        // stopped at `build` never reached it. The gate runs before the toolchain, which is what
        // makes them part of what the image must already have.
        for name in ["bash", "mkdir", "cp", "mv", "chmod", "sleep", "awk", "sed"] {
            std::os::unix::fs::symlink(real(name), sys.join(name)).unwrap();
        }
        let path = format!("{}:{}", bin.display(), sys.display());

        // The machine the size gate measures, pinned. Note there is deliberately no `nproc` on the
        // PATH above: the count falls through to the `awk` reading below, so this fixture also
        // proves the fallback works on an image that ships no `nproc`.
        let meminfo = scratch.join("meminfo");
        std::fs::write(&meminfo, "MemTotal:        4194304 kB\n").unwrap();
        let cpuinfo = scratch.join("cpuinfo");
        std::fs::write(&cpuinfo, "processor\t: 0\nprocessor\t: 1\n").unwrap();

        let run = |root: &std::path::Path, stop_after_build: bool| {
            let mut command = std::process::Command::new("bash");
            command
                .arg("-c")
                .arg(BOOTSTRAP_SH)
                // Its own working directory, for the reason at the escalation test's (SKEIN-811).
                .current_dir(&scratch)
                .env("PATH", &path)
                // The built PATH is the whole fixture, and `$BASH_ENV` is sourced by every
                // non-interactive bash before the first line runs — which is where a developer's
                // `~/.cargo/env` puts a real cargo back in front of the stub. Removing it is what
                // makes "this image does not have that" mean it.
                .env_remove("BASH_ENV")
                .env("SKEIN_MEMINFO", &meminfo)
                .env("SKEIN_CPUINFO", &cpuinfo)
                .env("SKEIN_FLEET_MEMORY", "4g")
                .env("SKEIN_FLEET_CPUS", "2")
                // Pinned so the volume DISCOVERY is skipped: it is `the_servers_home_is_the_mounted
                // _volume_and_not_the_sandboxs_own`'s subject, not this test's, and letting it run
                // here would put its `sort` on the list of things this image is claimed to need —
                // and send this test reading the developer's real mounts to find it.
                .env("SKEIN_HOME", root.join("home"))
                .env("SKEIN_FLEET_ROOT", root);
            match stop_after_build {
                true => command.env("SKEIN_BOOTSTRAP_STOP_AFTER", "build"),
                // The serve path, which no other test of this file runs — and which is where the
                // third missing package was found, after a build that had already succeeded.
                false => command.env_remove("SKEIN_BOOTSTRAP_STOP_AFTER"),
            };
            command.output().expect("bootstrap.sh ran")
        };
        let stub = |name: &str, log: &std::path::Path, body: &str| {
            let at = bin.join(name);
            std::fs::write(
                &at,
                format!(
                    "#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> {log}\n{body}\n",
                    log = log.display(),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        };

        // ---- an image with no compiler ----
        let root = scratch.join("bare");
        let log = scratch.join("bare.log");
        stub(
            "git",
            &log,
            "case \"$*\" in *rev-parse*) echo deadbee ;; esac\nexit 0",
        );
        stub(
            "cargo",
            &log,
            &format!(
                "command -v cc >/dev/null 2>&1 || {{ echo 'error: linker `cc` not found' >&2; exit 101; }}\n                 mkdir -p {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\nexit 0",
                src = root.join(".skein/src").display(),
            ),
        );
        stub("sudo", &log, &non_interactive_sudo_stub(""));
        // What apt really does, in one line of it: the package arrives and `cc` is on the PATH. The
        // script asks the PATH and not apt, so a stub that recorded and installed nothing would be
        // testing the wrong claim — and would fail, correctly.
        stub(
            "apt-get",
            &log,
            &format!(
                "case \"$*\" in *build-essential*) printf '#!/bin/sh\\nexit 0\\n' > {cc}; chmod 755 {cc} ;; esac\nexit 0",
                cc = bin.join("cc").display(),
            ),
        );

        let out = run(&root, true);
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "the bootstrap could not build on an image with no C compiler, which is the `shell` \
             image every install starts from:\n{said}\nit ran:\n{ran}"
        );
        for package in ["build-essential", "tmux"] {
            assert!(
                ran.contains(&format!("apt-get install -y -qq {package}"))
                    || ran
                        .lines()
                        .any(|l| l.starts_with("apt-get install") && l.contains(package)),
                "{package} was not installed, and the image does not have it:\nit ran:\n{ran}"
            );
        }
        assert!(
            ran.contains("apt-get install -y -qq build-essential"),
            "no compiler was installed, so `cargo build` reaches `error: linker `cc` not found` \
             after downloading every crate:\nit ran:\n{ran}"
        );
        assert!(
            ran.lines().any(|l| l.starts_with("apt-get update")),
            "apt-get install ran without an update first; a fresh image ships an empty index, \
             where that reports `no installation candidate` and reads as a missing package:\n{ran}"
        );
        assert!(
            ran.contains("cargo build"),
            "the build never ran, so nothing above it is being tested:\nit ran:\n{ran}"
        );
        assert!(
            root.join(".skein/skein-server").exists(),
            "the binary was not installed:\n{said}\nit ran:\n{ran}"
        );

        // ---- the same image, run all the way to the cockpit ----
        //
        // The build is not the end of this script, and the third missing package was found past it:
        // a finished release binary, and then `bash: line 227: tmux: command not found`. Every other
        // test of this file stops at `SKEIN_BOOTSTRAP_STOP_AFTER=build`, so the serve path had no
        // coverage at all — which is why a `tmux` this script has always run went unnoticed.
        //
        // Here apt is asked for tmux and does not deliver it, which is the case the message is for.
        let root = scratch.join("serving");
        let log = scratch.join("serving.log");
        stub(
            "cargo",
            &log,
            &format!(
                "command -v cc >/dev/null 2>&1 || {{ echo 'error: linker `cc` not found' >&2; exit 101; }}\n\
                 mkdir -p {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\nexit 0",
                src = root.join(".skein/src").display(),
            ),
        );
        stub("apt-get", &log, "exit 0");

        let out = run(&root, false);
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            !out.status.success(),
            "the bootstrap reported success with no tmux to run the cockpit under:\n{said}"
        );
        assert!(
            root.join(".skein/skein-server").exists(),
            "the run did not get as far as installing the binary, so it is failing somewhere \
             earlier than the door and this asserts nothing about tmux:\n{said}\nit ran:\n{ran}"
        );
        // NOT `said.contains("tmux")`, which cannot fail: the apt step announces `the image is
        // missing tmux curl python3 jq` on the same stream, so that assertion passed with the whole
        // check deleted. The sabotage found that, not the author. What discriminates is the two
        // below — the message's own words, and the absence of anything from further down the
        // script.
        assert!(
            said.contains("nothing to run the cockpit under"),
            "the install stopped for want of tmux without saying so. The `has-session` call \
             swallows its own stderr, so what a person is left with is a bash line number:\n{said}"
        );
        for downstream in ["command not found", "cannot stat"] {
            assert!(
                !said.contains(downstream),
                "the script ran past the missing tmux and failed at `{downstream}` instead, so the \
                 message a person gets is the shell's rather than skein's:\n{said}"
            );
        }

        // ---- and an image that already has one ----
        //
        // `apt-get update` is a minute against a mirror. The bootstrap is documented as idempotent
        // and is what an upgrade re-runs, so an unconditional apt would put that minute on every
        // single upgrade for no package at all.
        let root = scratch.join("stocked");
        let log = scratch.join("stocked.log");
        for present in ["cc", "curl", "python3", "tmux", "jq"] {
            stub(present, &log, "exit 0");
        }
        stub(
            "git",
            &log,
            "case \"$*\" in *rev-parse*) echo deadbee ;; esac\nexit 0",
        );
        stub(
            "cargo",
            &log,
            &format!(
                "command -v cc >/dev/null 2>&1 || {{ echo 'error: linker `cc` not found' >&2; exit 101; }}\n                 mkdir -p {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\nexit 0",
                src = root.join(".skein/src").display(),
            ),
        );
        stub("sudo", &log, &non_interactive_sudo_stub(""));
        stub("apt-get", &log, "exit 0");

        let out = run(&root, true);
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            out.status.success(),
            "the bootstrap failed on an image that has everything:\n{}\nit ran:\n{ran}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !ran.contains("apt-get"),
            "apt ran on an image that was already complete — that is a mirror round trip on every \
             upgrade, for no package:\nit ran:\n{ran}"
        );
    }

    /// Opening the door is a **file in the sandbox**, and running that file is all it takes.
    ///
    /// Nothing in the fleet sandbox starts the cockpit at boot. pid 1 is `tini`; there is no
    /// systemd, no cron, no `systemctl` — measured in the live fleet, not assumed. So a sandbox
    /// that stops and starts comes back with the entire install intact on disk and nothing
    /// serving: no tmux session, no doorway, :7878 unbound, and the host's published port
    /// connecting to nothing. It is not rare either — `sbx exec` arms a ~30s stop as it
    /// disconnects, so every command run against the fleet causes one.
    ///
    /// While the four lines that open the door lived *inside* `bootstrap.sh`, the only way to run
    /// them again was to run the installer again: a fetch, a build, and a minute, to redo four
    /// lines that were already right. They are now `start-door.sh`, installed beside the binaries.
    ///
    /// Three things, and the second is the one that matters:
    ///
    ///   1. the install writes the file and opens the door by running it — not by doing it itself,
    ///      so there is one implementation and a restart cannot run different bytes than a person;
    ///   2. **the file works alone**, with no `$SKEIN_HOME` and nothing else in its environment,
    ///      which is the state anything running at sandbox start would be in;
    ///   3. the supervisor string it hands tmux actually *runs*, and carries the volume — asserted
    ///      by executing it, because a command line that looks right and a command line that works
    ///      are exactly what came apart to produce this whole class of bug.
    #[test]
    fn the_door_is_a_file_the_install_runs_rather_than_a_passage_of_the_install() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = crate::testutil::tempdir();
        let root = scratch.join("fleet");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log = root.join("ran.log");
        let session = root.join("session.cmd");
        let volume = scratch.join("volume");
        std::fs::create_dir_all(&volume).unwrap();

        let write = |at: std::path::PathBuf, body: String| {
            std::fs::write(&at, body).unwrap();
            std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let stub = |name: &str, body: &str| {
            write(
                bin.join(name),
                format!(
                    "#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> {log}\n{body}\n",
                    log = log.display()
                ),
            )
        };

        // tmux, which is the whole fixture. `has-session` says no, so the start branch is taken;
        // `new-session`'s LAST argument is the supervisor, and it is kept rather than parsed out of
        // the log because assertion 3 has to run it.
        write(
            bin.join("tmux"),
            format!(
                "#!/bin/sh\nprintf 'tmux %s\\n' \"$*\" >> {log}\nfor a in \"$@\"; do\n  case \"$a\" \
                 in has-session) exit 1 ;; esac\ndone\nwhile [ $# -gt 1 ]; do shift; done\nprintf \
                 '%s' \"$1\" > {session}\nexit 0\n",
                log = log.display(),
                session = session.display(),
            ),
        );
        // The clone leaves the doorway where the install copies it from. A stub that only recorded
        // would leave the script correctly failing on a `cp` of a file that is not there.
        let checkout = root.join(".skein/src/src");
        stub(
            "git",
            &format!(
                "mkdir -p {checkout}\nprintf 'DOORWAY' > {checkout}/server-doorway.py\ncase \"$*\" \
                 in *rev-parse*|*describe*) echo deadbee ;; esac\nexit 0",
                checkout = checkout.display()
            ),
        );
        stub(
            "cargo",
            &format!(
                "mkdir -p {src}/target/release\nprintf 'ELF' > {src}/target/release/skein-server\n\
                 printf 'ELF' > {src}/target/release/skein\nexit 0",
                src = root.join(".skein/src").display(),
            ),
        );
        for present in ["cc", "curl", "jq"] {
            stub(present, "exit 0");
        }
        // python3 is a stub here so the image counts as complete, and so the install's last step
        // — asking the server on the port which build it is (`python3 -`) — is answered with the
        // build the git stub reports, since nothing here serves. Assertion 3 replaces it with one
        // that records. The real question, against a real doorway, is `tests/fleet_move.rs`'s.
        stub("python3", "[ \"$1\" = - ] && echo deadbee\nexit 0");

        let path = format!("{}:{}", bin.display(), env!("PATH"));
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(BOOTSTRAP_SH)
            .env("PATH", &path)
            // `$BASH_ENV` is sourced ahead of the script and puts real programs in front of stubs.
            .env_remove("BASH_ENV")
            .env("SKEIN_FLEET_ROOT", &root)
            .envs(stated_size(&bin, &scratch))
            .env("SKEIN_HOME", &volume)
            .env_remove("SKEIN_BOOTSTRAP_STOP_AFTER")
            .env_remove("SKEIN_SOURCE_REF")
            .output()
            .expect("bootstrap.sh ran");
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "the install did not reach the end:\n{said}\nit ran:\n{ran}"
        );

        // 1. The file exists, and the install opened the door by RUNNING it.
        let door = root.join(".skein/start-door.sh");
        assert!(
            door.is_file(),
            "the install did not leave a start-door.sh, so putting the door back after a sandbox \
             restart costs a fetch and a build again:\n{said}"
        );
        assert!(
            ran.contains("tmux -S") && ran.contains("new-session -d -s skein-server"),
            "the install finished without starting the cockpit's supervisor:\nit ran:\n{ran}"
        );

        // 2. And it works ALONE — no `$SKEIN_HOME`, nothing but the fleet root it is installed
        //    under, which is every environment a restart could give it.
        std::fs::remove_file(&log).unwrap();
        std::fs::remove_file(&session).unwrap();
        let out2 = std::process::Command::new(&door)
            .env("PATH", &path)
            .env_remove("BASH_ENV")
            .env("SKEIN_FLEET_ROOT", &root)
            .env_remove("SKEIN_HOME")
            .output()
            .expect("start-door.sh ran");
        let ran2 = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            out2.status.success(),
            "start-door.sh cannot open the door on its own, so nothing that runs at sandbox start \
             could use it:\n{}\nit ran:\n{ran2}",
            String::from_utf8_lossy(&out2.stderr)
        );
        assert!(
            ran2.contains("new-session -d -s skein-server"),
            "start-door.sh ran and started no supervisor:\nit ran:\n{ran2}"
        );

        // 3. The supervisor it handed tmux is a command that RUNS, and it carries the volume.
        //    `$SKEIN_HOME` was not in the environment above, so the only place this can have come
        //    from is the marker the install wrote — which is the point: the container's own `$HOME`
        //    is not the mount, and a server that writes its token there loses it at the next
        //    restart.
        let supervise = std::fs::read_to_string(&session).expect("tmux was given a command");
        let doorway = root.join(".skein/server-doorway.py");
        write(
            bin.join("python3"),
            format!(
                "#!/bin/sh\nprintf 'python3 %s home=%s\\n' \"$*\" \"$SKEIN_HOME\" \
                 >> {log}\nrm -f {doorway}\nexit 0\n",
                log = log.display(),
                doorway = doorway.display(),
            ),
        );
        let out3 = std::process::Command::new("sh")
            .arg("-c")
            .arg(&supervise)
            .env("PATH", &path)
            .env_remove("BASH_ENV")
            .output()
            .expect("the supervisor ran");
        let ran3 = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            out3.status.success(),
            "the supervisor tmux was given does not run:\n{}\nfrom:\n{supervise}",
            String::from_utf8_lossy(&out3.stderr)
        );
        let started = ran3
            .lines()
            .find(|l| l.starts_with("python3 "))
            .unwrap_or_else(|| panic!("the supervisor never started the doorway:\n{ran3}"));
        assert!(
            started.contains(&format!("home={}", volume.display())),
            "the doorway was started with the wrong home, so skein-server writes its token and box \
             state somewhere that does not survive the sandbox: {started}"
        );
        assert!(
            started.contains(&doorway.display().to_string())
                && started.contains("7878")
                && started.contains(&root.join(".skein/skein-server").display().to_string()),
            "the doorway was not given the port and the server behind it: {started}"
        );
    }

    /// `$SKEIN_HOME` is the mounted volume, never the sandbox's own `$HOME`.
    ///
    /// This is where skein keeps `api-token`, `repos.json`, `config.json` and every box's state, and
    /// it defaulted to `$HOME/.skein` — right on a host, wrong inside the sandbox, and wrong in the
    /// way that costs most: it *works*. The server starts, generates a token, and writes all of it
    /// into the container's own `/home/<user>/.skein`, which is not the volume and does not survive
    /// the sandbox. What a person sees is a cockpit asking for the fleet's token while
    /// `~/.skein/api-token` on the host holds a different one, or none.
    ///
    /// The two differ because sbx bind-mounts a workspace at its HOST absolute path while giving the
    /// sandbox a home of its own — read off a live sandbox's `mountinfo`, not assumed:
    ///
    /// ```text
    /// 74 107 0:54 /Users/you/work/x /Users/you/work/x rw,... - virtiofs host rw
    /// HOME=/home/agent
    /// ```
    ///
    /// So the fixture sets `$HOME` to a decoy and asserts the decoy is *absent* from what the
    /// supervisor is given. Asserting the volume is present would pass just as well with both.
    #[test]
    fn the_servers_home_is_the_mounted_volume_and_not_the_sandboxs_own() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = crate::testutil::tempdir();
        let bin = scratch.join("bin");
        let sys = scratch.join("sys");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&sys).unwrap();
        let real = |name: &str| -> std::path::PathBuf {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {name}"))
                .output()
                .expect("looked for a program");
            let at = String::from_utf8_lossy(&out.stdout).trim().to_string();
            assert!(
                !at.is_empty(),
                "this machine has no {name}, so the fixture cannot be built"
            );
            std::path::PathBuf::from(at)
        };
        // `awk`, `sort`, `grep` and `sed` are what the discovery itself runs; `cat` is the closing
        // message. All base-image programs, none of them stubbable without testing the stub.
        for name in [
            "bash", "mkdir", "cp", "mv", "chmod", "sleep", "awk", "sort", "grep", "sed", "cat",
        ] {
            std::os::unix::fs::symlink(real(name), sys.join(name)).unwrap();
        }
        let path = format!("{}:{}", bin.display(), sys.display());

        let meminfo = scratch.join("meminfo");
        std::fs::write(&meminfo, "MemTotal:        4194304 kB\n").unwrap();
        let cpuinfo = scratch.join("cpuinfo");
        std::fs::write(&cpuinfo, "processor\t: 0\nprocessor\t: 1\n").unwrap();

        let stub = |name: &str, log: &std::path::Path, body: &str| {
            let at = bin.join(name);
            std::fs::write(
                &at,
                format!(
                    "#!/bin/sh\nprintf '{name} %s\\n' \"$*\" >> {log}\n{body}\n",
                    log = log.display(),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).unwrap();
        };

        // A decoy `$HOME`, which is what the old default would have used. Everything the sandbox
        // really has is here except the volume.
        let decoy = scratch.join("container-home");
        std::fs::create_dir_all(decoy.join(".skein")).unwrap();
        let volume = scratch.join("Users/you/.skein");
        std::fs::create_dir_all(&volume).unwrap();

        // `mountinfo` as the kernel writes it: six fixed fields, then optionals, then `-`. Only the
        // fifth is read. The fleet root's own `.skein` is included precisely because it must be
        // skipped — it is a directory the script made, not the volume.
        let mountinfo = |lines: &[String]| -> std::path::PathBuf {
            let at = scratch.join(format!("mountinfo-{}", lines.len()));
            std::fs::write(&at, lines.join("\n") + "\n").unwrap();
            at
        };
        let line =
            |at: &str| format!("74 107 0:54 / {at} rw,nosuid,nodev,relatime - virtiofs host rw");

        let run = |root: &std::path::Path, mounts: &std::path::Path| {
            std::process::Command::new("bash")
                .arg("-c")
                .arg(BOOTSTRAP_SH)
                // Its own working directory, for the reason at the escalation test's (SKEIN-811).
                .current_dir(&scratch)
                .env("PATH", &path)
                .env_remove("BASH_ENV")
                .env_remove("SKEIN_HOME")
                // Stated, because the size gate runs immediately after the discovery this is about
                // and would otherwise refuse before the assertion below could be reached. Pinned to
                // a fixture for the same reason it is everywhere else: unpinned, this passes on the
                // machine it was written on.
                .env("SKEIN_MEMINFO", &meminfo)
                .env("SKEIN_CPUINFO", &cpuinfo)
                .env("SKEIN_FLEET_MEMORY", "4g")
                .env("SKEIN_FLEET_CPUS", "2")
                .env_remove("SKEIN_BOOTSTRAP_STOP_AFTER")
                .env("HOME", &decoy)
                .env("SKEIN_MOUNTINFO", mounts)
                .env("SKEIN_FLEET_ROOT", root)
                .output()
                .expect("bootstrap.sh ran")
        };

        // ---- exactly one volume ----
        let root = scratch.join("fleet");
        let log = scratch.join("one.log");
        stub(
            "git",
            &log,
            "case \"$*\" in *rev-parse*|*describe*) echo deadbee ;; esac\nexit 0",
        );
        stub(
            "cargo",
            &log,
            &format!(
                "mkdir -p {src}/target/release {src}/src\n\
                 printf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\n\
                 printf 'doorway' > {src}/src/server-doorway.py\nexit 0",
                src = root.join(".skein/src").display(),
            ),
        );
        stub("sudo", &log, &non_interactive_sudo_stub(""));
        stub("apt-get", &log, "exit 0");
        for present in ["cc", "curl", "python3", "tmux", "jq"] {
            stub(present, &log, "exit 0");
        }
        // Nothing serves here, so the install's closing question — which build answers on the
        // port, asked as `python3 -` — is answered by the stub with the build git reports.
        stub("python3", &log, "[ \"$1\" = - ] && echo deadbee\nexit 0");
        // Re-stubbed after the loop: the supervisor must be *started*, so `has-session` has to say
        // there is none. A stub that exited 0 for everything would take the reload branch and this
        // would assert nothing about what the session is given.
        stub(
            "tmux",
            &log,
            "case \"$*\" in *has-session*) exit 1 ;; esac\nexit 0",
        );

        let mounts = mountinfo(&[
            line(&root.join(".skein").display().to_string()),
            line(&volume.display().to_string()),
            line(&decoy.join(".claude/skills").display().to_string()),
        ]);
        let out = run(&root, &mounts);
        let ran = std::fs::read_to_string(&log).unwrap_or_default();
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "the bootstrap failed with one volume mounted:\n{said}\nit ran:\n{ran}"
        );
        let started = ran
            .lines()
            .find(|l| l.contains("new-session"))
            .unwrap_or_else(|| panic!("no supervisor was started:\n{ran}"));
        assert!(
            started.contains(&format!("SKEIN_HOME='{}'", volume.display())),
            "the server was not pointed at the mounted volume {}:\n{started}",
            volume.display()
        );
        assert!(
            !started.contains(&format!("{}/.skein", decoy.display())),
            "the server was pointed at the container's own $HOME, where its token and every repo it \
             is told about die with the sandbox:\n{started}"
        );

        // ---- no volume, and two volumes: both refuse ----
        //
        // Refusing is the point. A guess here is invisible until somebody cannot open the cockpit,
        // and the old default guessed every time.
        for what in ["none", "two"] {
            let root = scratch.join(format!("fleet-{what}"));
            let log = scratch.join(format!("{what}.log"));
            // Built from THIS arm's fleet root, not the previous one. A first draft took the outer
            // `root`, so the "none" arm's only line named a directory the run did not skip — it
            // found one candidate, succeeded, and asserted nothing.
            let lines = match what {
                "none" => vec![line(&root.join(".skein").display().to_string())],
                _ => vec![
                    line(&volume.display().to_string()),
                    line(&scratch.join("other/.skein").display().to_string()),
                ],
            };
            stub(
                "cargo",
                &log,
                &format!(
                    "mkdir -p {src}/target/release {src}/src\n\
                     printf 'ELF' > {src}/target/release/skein-server\nprintf 'ELF' > {src}/target/release/skein\n\
                     printf 'doorway' > {src}/src/server-doorway.py\nexit 0",
                    src = root.join(".skein/src").display(),
                ),
            );
            let out = run(&root, &mountinfo(&lines));
            let said = String::from_utf8_lossy(&out.stderr).to_string();
            assert!(
                !out.status.success(),
                "with {what} volumes the bootstrap carried on and guessed:\n{said}"
            );
            assert!(
                said.contains("SKEIN_HOME"),
                "the refusal does not name the variable that would fix it, so it is a dead end \
                 ({what}):\n{said}"
            );
        }
    }

    /// No branch name is written down anywhere, in any of the three places that would have to
    /// agree.
    ///
    /// SKEIN-461: the install said `main`, this repo has no `main`, and the failure landed in two
    /// stages — a 404 fetching `bootstrap.sh`, then a failed clone for anyone who had the file
    /// already. A literal branch is a fact about somebody's remote, not about skein, and a fork
    /// would make it wrong again for a different reason.
    ///
    /// So the assertion is an ABSENCE, which is the only shape that holds: no default branch, in
    /// the README's URL, in `bootstrap.sh`, or in [`skein_source_ref`]. `HEAD` and a bare clone
    /// resolve to the remote's own default, whatever it is called.
    #[test]
    fn no_branch_name_is_hardcoded_in_the_install() {
        let _env = env_lock();
        std::env::remove_var("SKEIN_SOURCE_REF");
        assert_eq!(
            skein_source_ref(),
            "",
            "skein_source_ref names a branch by default; if that branch is not on the remote, \
             every install and every upgrade fails at the clone"
        );

        // Read from the tree, so a change to either file is what fails rather than a stale copy.
        let readme = include_str!("../../README.md");
        let url = readme
            .lines()
            .find(|l| l.contains("raw.githubusercontent.com"))
            .expect("the README no longer shows how to fetch the bootstrap");
        assert!(
            url.contains("/HEAD/"),
            "the README fetches the bootstrap from a named branch: {url}"
        );
        assert!(
            url.contains("/bootstrap.sh"),
            "the README's install URL does not name bootstrap.sh, so it fetches something else: \
             {url}"
        );

        // The bootstrap's own header shows the same three lines the README does, and it was still
        // fetching itself from `/main/` — a 404 for anyone who copied the install out of the file
        // rather than out of the README. Two places said it, only one of them was checked.
        for line in BOOTSTRAP_SH
            .lines()
            .filter(|l| l.contains("raw.githubusercontent.com"))
        {
            assert!(
                line.contains("/HEAD/"),
                "bootstrap.sh shows an install that fetches from a named branch: {line}"
            );
        }

        for (line, no) in BOOTSTRAP_SH
            .lines()
            .filter(|l| l.trim_start().starts_with("ref="))
            .flat_map(|l| ["main", "master", "trunk", "develop"].map(move |b| (l, b)))
        {
            assert!(
                !line.contains(no),
                "bootstrap.sh defaults its ref to `{no}`, which is a fact about one remote rather \
                 than about skein: {line}"
            );
        }
    }

    /// The shell derives the same paths this module does — asserted by running it, not by reading
    /// it.
    ///
    /// `bootstrap.sh` cannot ask skein where anything goes; it is the first thing that runs on a
    /// sandbox where skein does not exist. So it computes `$fleet_root/.skein/…` itself, and this
    /// is the join: the same four paths, out of the shell and out of Rust, compared. A rename on
    /// either side fails here rather than at somebody's install.
    #[test]
    fn a_bootstrap_run_by_hand_puts_everything_where_skein_looks_for_it() {
        let _env = env_lock();
        let root = "/boxes";
        std::env::set_var("SKEIN_FLEET_ROOT", root);
        let mine = [
            skein_source_path(),
            skein_toolchain_path(),
            server_path(),
            skein_cli_path(),
            server_doorway_path(),
            server_door_stamp_path(),
            server_tmux_sock(),
        ];
        std::env::remove_var("SKEIN_FLEET_ROOT");

        // The script's own variable names, printed by the script's own assignments — everything
        // above the first line that does real work.
        let prelude: String = BOOTSTRAP_SH
            .lines()
            .take_while(|l| !l.starts_with("say()"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "{prelude}\nprintf '%s\\n' \"$src\" \"$toolchain\" \"$server\" \"$skein_dir/skein\" \"$doorway\" \"$stamp\" \"$sock\""
            ))
            .env("SKEIN_FLEET_ROOT", root)
            .output()
            .expect("the bootstrap's prelude ran");
        let theirs: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(
            theirs,
            mine.to_vec(),
            "bootstrap.sh and src/fleet/install.rs disagree about where skein's own files live, so an \
             install would put them somewhere skein never looks. stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
