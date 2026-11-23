//! A whole-skein move, rehearsed with the real `skein` binary and a fake `sbx`, and killed.
//!
//! There is no move command yet, so this runs the sequence the existing verbs make: `migrate plan`,
//! `send`, `seal`, `unseal`, `receive`, then `fleet-owner claim` on the new fleet, and the old fleet
//! claiming the volume back as the rollback. Both fleets share one volume, as they do on one Mac.
//!
//! **Kills are exact and production code has no hook for them.** A small `ptrace` tracer runs the
//! verb and sends SIGKILL at the entry of its Nth system call that changes a file, so the call it
//! stopped at never happened and everything before it did. Counting the calls first means a point
//! is named by what the process had done, rather than by how fast this machine happens to be.
//!
//! Each test names the change that fails it.

mod common;

/// The notice for a platform with no `ptrace` of this shape. `cargo test` prints test names.
#[cfg(not(target_os = "linux"))]
#[test]
fn the_move_rehearsal_does_not_run_here_because_it_kills_through_linux_ptrace() {}

#[cfg(target_os = "linux")]
mod linux {
    use super::common::{have, skip, Scratch};
    use skein::fence::{read_owner, Owner, FENCED_EXIT, RESTORED_FLEET};
    use std::collections::BTreeMap;
    use std::ffi::CString;
    use std::fs;
    use std::os::fd::IntoRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const OLD: &str = "old-sandbox";
    const NEW: &str = "new-sandbox";
    const SENTINEL: &str = "SENTINEL-MUST-NOT-TRAVEL";
    const DOCKER_VOLUME: &str = "rehearsal-pgdata";
    const PASSPHRASE: &str = "skein-test-rehearsal-passphrase";
    const TRACE: &str = "SKEIN_REHEARSAL_TRACE";
    /// Larger than `send`'s 1 MiB write buffer, so a send has several writes to die between.
    const LARGE: usize = 3 << 20;

    // -----------------------------------------------------------------------------------------
    // The tracer
    // -----------------------------------------------------------------------------------------

    const PTRACE_GET_SYSCALL_INFO: libc::c_uint = 0x420e;
    const SYSCALL_INFO_ENTRY: u8 = 1;

    /// `struct ptrace_syscall_info`, as far as a syscall entry uses it.
    #[repr(C)]
    #[derive(Default)]
    struct SyscallInfo {
        op: u8,
        pad: [u8; 3],
        arch: u32,
        instruction_pointer: u64,
        stack_pointer: u64,
        nr: u64,
        args: [u64; 6],
        rest: [u64; 2],
    }

    /// `libc` names this one on x86-64 only.
    #[cfg(target_arch = "x86_64")]
    const SYS_SENDFILE: i64 = libc::SYS_sendfile;
    #[cfg(not(target_arch = "x86_64"))]
    const SYS_SENDFILE: i64 = 71;

    /// Does this system call change a file? Output to stdout and stderr does not count.
    fn changes_a_file(nr: i64, args: &[u64; 6]) -> bool {
        #[cfg(target_arch = "x86_64")]
        if [
            libc::SYS_rename,
            libc::SYS_mkdir,
            libc::SYS_rmdir,
            libc::SYS_unlink,
            libc::SYS_symlink,
            libc::SYS_link,
            libc::SYS_chmod,
            libc::SYS_creat,
        ]
        .contains(&nr)
        {
            return true;
        }
        #[cfg(target_arch = "x86_64")]
        if nr == libc::SYS_open {
            return args[1] as i32 & (libc::O_WRONLY | libc::O_RDWR | libc::O_CREAT) != 0;
        }
        if nr == libc::SYS_openat {
            return args[2] as i32 & (libc::O_WRONLY | libc::O_RDWR | libc::O_CREAT) != 0;
        }
        if [
            libc::SYS_write,
            libc::SYS_writev,
            libc::SYS_pwrite64,
            libc::SYS_pwritev,
        ]
        .contains(&nr)
        {
            return args[0] > 2;
        }
        [
            libc::SYS_mkdirat,
            libc::SYS_unlinkat,
            libc::SYS_renameat,
            libc::SYS_renameat2,
            libc::SYS_symlinkat,
            libc::SYS_linkat,
            libc::SYS_fchmod,
            libc::SYS_fchmodat,
            libc::SYS_utimensat,
            libc::SYS_ftruncate,
            libc::SYS_fsync,
            libc::SYS_fdatasync,
            libc::SYS_copy_file_range,
            SYS_SENDFILE,
        ]
        .contains(&nr)
    }

    fn cstrings(items: &[String]) -> (Vec<CString>, Vec<*const libc::c_char>) {
        let owned: Vec<CString> = items
            .iter()
            .map(|s| CString::new(s.as_str()).unwrap())
            .collect();
        let mut pointers: Vec<*const libc::c_char> = owned.iter().map(|s| s.as_ptr()).collect();
        pointers.push(std::ptr::null());
        (owned, pointers)
    }

    /// Run `argv` under trace. Counting starts when the process has become `skein`, so the fake
    /// `sbx` in front of it is not a kill point.
    fn trace(
        argv: &[String],
        env: &[String],
        cwd: &str,
        output: &Path,
        kill_at: Option<u64>,
    ) -> (u64, bool, i32) {
        let (_argv, argv_ptrs) = cstrings(argv);
        let (_env, env_ptrs) = cstrings(env);
        let cwd = CString::new(cwd).unwrap();
        let out = fs::File::create(output).unwrap().into_raw_fd();
        let null = fs::File::open("/dev/null").unwrap().into_raw_fd();
        // SAFETY: between fork and exec the child makes only async-signal-safe calls, on pointers
        // prepared above.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                libc::dup2(null, 0);
                libc::dup2(out, 1);
                libc::dup2(out, 2);
                libc::chdir(cwd.as_ptr());
                libc::ptrace(
                    libc::PTRACE_TRACEME,
                    0,
                    std::ptr::null_mut::<libc::c_void>(),
                    std::ptr::null_mut::<libc::c_void>(),
                );
                libc::raise(libc::SIGSTOP);
                libc::execve(argv_ptrs[0], argv_ptrs.as_ptr(), env_ptrs.as_ptr());
                libc::_exit(127);
            }
        }
        let mut status = 0;
        // SAFETY: `status` is a valid out-pointer for the child just forked.
        let stopped = unsafe { libc::waitpid(pid, &mut status, libc::__WALL) };
        assert!(
            stopped == pid && libc::WIFSTOPPED(status),
            "the traced child did not stop: this machine refuses ptrace, which this suite needs"
        );
        let options = libc::PTRACE_O_TRACESYSGOOD
            | libc::PTRACE_O_TRACECLONE
            | libc::PTRACE_O_TRACEEXEC
            | libc::PTRACE_O_EXITKILL;
        // SAFETY: the child is stopped and traced by this thread.
        let set = unsafe {
            libc::ptrace(
                libc::PTRACE_SETOPTIONS,
                pid,
                std::ptr::null_mut::<libc::c_void>(),
                options as usize as *mut libc::c_void,
            )
        };
        assert_eq!(set, 0, "PTRACE_SETOPTIONS failed");

        let resume = |tid: libc::pid_t, signal: i32| {
            // SAFETY: a failure here is a task already gone, which the wait below reports.
            unsafe {
                libc::ptrace(
                    libc::PTRACE_SYSCALL,
                    tid,
                    std::ptr::null_mut::<libc::c_void>(),
                    signal as usize as *mut libc::c_void,
                );
            }
        };
        resume(pid, 0);
        let (mut count, mut armed, mut killed) = (0u64, false, false);
        loop {
            // SAFETY: `status` is a valid out-pointer.
            let tid = unsafe { libc::waitpid(-1, &mut status, libc::__WALL) };
            assert!(tid > 0, "lost the traced process");
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                if tid == pid {
                    let code = if libc::WIFEXITED(status) {
                        libc::WEXITSTATUS(status)
                    } else {
                        -libc::WTERMSIG(status)
                    };
                    return (count, killed, code);
                }
                continue;
            }
            let signal = libc::WSTOPSIG(status);
            let mut deliver = 0;
            if signal == (libc::SIGTRAP | 0x80) {
                let mut info = SyscallInfo::default();
                // SAFETY: `info` is at least as long as the size passed.
                let got = unsafe {
                    libc::ptrace(
                        PTRACE_GET_SYSCALL_INFO,
                        tid,
                        std::mem::size_of::<SyscallInfo>() as *mut libc::c_void,
                        &mut info as *mut SyscallInfo as *mut libc::c_void,
                    )
                };
                if armed
                    && !killed
                    && got > 0
                    && info.op == SYSCALL_INFO_ENTRY
                    && changes_a_file(info.nr as i64, &info.args)
                {
                    count += 1;
                    if kill_at == Some(count) {
                        // SAFETY: `pid` is this tracer's own child.
                        unsafe { libc::kill(pid, libc::SIGKILL) };
                        killed = true;
                    }
                }
            } else if signal == libc::SIGTRAP {
                if status >> 16 == libc::PTRACE_EVENT_EXEC {
                    armed = fs::read_link(format!("/proc/{tid}/exe"))
                        .is_ok_and(|exe| exe.file_name().is_some_and(|n| n == "skein"));
                }
            } else if signal != libc::SIGSTOP {
                deliver = signal;
            }
            resume(tid, deliver);
        }
    }

    /// The tracer's own process, because a `waitpid(-1)` here would take other tests' children.
    #[test]
    fn traced_run() {
        let Ok(spec) = std::env::var(TRACE) else {
            return;
        };
        let spec: serde_json::Value = serde_json::from_str(&spec).unwrap();
        let strings = |key: &str| -> Vec<String> {
            spec[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        let output = PathBuf::from(spec["output"].as_str().unwrap());
        let (count, killed, code) = trace(
            &strings("argv"),
            &strings("env"),
            spec["cwd"].as_str().unwrap(),
            &output,
            spec["kill_at"].as_u64(),
        );
        fs::write(
            output.with_extension("report"),
            format!("{count} {killed} {code}"),
        )
        .unwrap();
    }

    struct Traced {
        mutations: u64,
        killed: bool,
        code: i32,
        said: String,
    }

    // -----------------------------------------------------------------------------------------
    // The rig
    // -----------------------------------------------------------------------------------------

    type Print = BTreeMap<PathBuf, (u32, u64, i64, Vec<u8>)>;

    /// Relative path to (mode, length, mtime, content or link target), leaving out what `skip` names.
    fn print_of(top: &Path, skip: &dyn Fn(&Path) -> bool) -> Print {
        fn go(top: &Path, at: &Path, skip: &dyn Fn(&Path) -> bool, into: &mut Print) {
            let rel = at.strip_prefix(top).unwrap();
            if skip(rel) {
                return;
            }
            let meta = fs::symlink_metadata(at).unwrap();
            let body = if meta.is_file() {
                fs::read(at).unwrap()
            } else if meta.file_type().is_symlink() {
                fs::read_link(at)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes()
            } else {
                Vec::new()
            };
            let length = if meta.is_dir() { 0 } else { meta.len() };
            into.insert(rel.to_path_buf(), (meta.mode(), length, meta.mtime(), body));
            if meta.is_dir() {
                for entry in fs::read_dir(at).unwrap() {
                    go(top, &entry.unwrap().path(), skip, into);
                }
            }
        }
        let mut print = BTreeMap::new();
        go(top, top, skip, &mut print);
        print
    }

    fn assert_same(from: &Print, to: &Print, what: &str) {
        assert!(!from.is_empty(), "{what}: nothing to compare");
        assert_eq!(
            from.keys().collect::<Vec<_>>(),
            to.keys().collect::<Vec<_>>(),
            "{what}: the paths differ"
        );
        for (path, was) in from {
            let now = &to[path];
            assert!(
                was == now,
                "{what}: {} differs (mode {:o}/{:o}, length {}/{}, mtime {}/{})",
                path.display(),
                was.0,
                now.0,
                was.1,
                now.1,
                was.2,
                now.2
            );
        }
    }

    /// The ownership record, which is the one thing a move may change on the source.
    fn ownership_record(rel: &Path) -> bool {
        let first = rel.components().next().map(|c| c.as_os_str());
        first.is_some_and(|name| {
            name == "fleet-owner"
                || name == "fleet-owner.d"
                || name.as_bytes().starts_with(b".skein.tmp.")
        })
    }

    /// Credentials the fixture plants inside what otherwise travels.
    fn left_behind(rel: &Path) -> bool {
        rel.file_name().is_some_and(|name| {
            ["server.pem", "agent.pem", ".credentials.json", ".skein"]
                .iter()
                .any(|n| name == *n)
        })
    }

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn executable(path: &Path, text: &str) {
        write(path, text);
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn said(out: &Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    /// Give every directory under `dir` its owner's permissions, so it can be removed.
    fn open_up(dir: &Path) {
        let Ok(meta) = fs::symlink_metadata(dir) else {
            return;
        };
        if !meta.is_dir() {
            return;
        }
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(meta.mode() | 0o700));
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                open_up(&entry.path());
            }
        }
    }

    fn remove(dir: &Path) {
        open_up(dir);
        let _ = fs::remove_dir_all(dir);
    }

    /// A stand-in for `sbx`: each sandbox is a fleet root on the one volume, and a stopped sandbox
    /// runs nothing.
    const FAKE_SBX: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$SBX_STATE/log"
verb=$1; shift
case "$verb" in
  stop) : > "$SBX_STATE/$1.stopped"; exit 0 ;;
  start) rm -f "$SBX_STATE/$1.stopped"; exit 0 ;;
  exec)
    name=$1; shift
    if [ -e "$SBX_STATE/$name.stopped" ]; then
      echo "fake sbx: $name is stopped" >&2
      exit 125
    fi
    IFS= read -r root < "$SBX_STATE/$name.root" || exit 125
    volume=$SBX_VOLUME
    if [ -e "$SBX_STATE/$name.volume" ]; then
      IFS= read -r volume < "$SBX_STATE/$name.volume" || exit 125
    fi
    SKEIN_FLEET_ROOT=$root SKEIN_HOME=$volume exec "$@" ;;
  *) echo "fake sbx: unsupported verb $verb" >&2; exit 2 ;;
esac
"#;

    struct Rig {
        scratch: Scratch,
        base: PathBuf,
        volume: PathBuf,
        old: PathBuf,
        new: PathBuf,
        bin: PathBuf,
        state: PathBuf,
        phrase: PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            // The fixture holds a read-only directory, which `Scratch` could not remove.
            open_up(self.scratch.path());
        }
    }

    /// The source as it must stay: everything but the ownership record.
    struct Source {
        volume: Print,
        fleet: Print,
    }

    fn usable() -> bool {
        if !have("bwrap") || !have("git") {
            skip("the plan's git probe needs bwrap and git");
            return false;
        }
        let works = Command::new("bwrap")
            .args(["--dev-bind", "/", "/", "--", "/bin/true"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !works {
            skip("bwrap cannot make a namespace here");
            return false;
        }
        let free = Command::new("df")
            .args(["-Pk"])
            .arg(std::env::temp_dir())
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .nth(1)?
                    .split_whitespace()
                    .nth(3)?
                    .parse::<u64>()
                    .ok()
            })
            .unwrap_or(0);
        if free < 11 * 1024 * 1024 {
            skip("send holds a 10 GB floor on the fleet's own disk, and this disk has less free");
            return false;
        }
        true
    }

    fn rig(tag: &str) -> Option<Rig> {
        if !usable() {
            return None;
        }
        let scratch = Scratch::temp(&format!("migrate-rehearsal-{tag}"));
        let base = scratch.path().to_path_buf();
        let rig = Rig {
            base: base.clone(),
            volume: base.join("volume"),
            old: base.join("old-fleet"),
            new: base.join("arrived/fleet"),
            bin: base.join("bin"),
            state: base.join("sbx-state"),
            phrase: base.join("keys/phrase"),
            scratch,
        };
        for dir in [
            &rig.volume,
            &rig.old.join(".skein"),
            &rig.bin,
            &rig.state,
            &base.join("out"),
            &base.join("user-home"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        write(&rig.phrase, &format!("{PASSPHRASE}\n"));
        executable(&rig.bin.join("sbx"), FAKE_SBX);
        executable(
            &rig.bin.join("docker"),
            &format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n\"volume ls\") printf '{DOCKER_VOLUME}\\n' ;;\n\
                 \"image ls\") ;;\n\"system df\") echo '{{}}' ;;\n\"ps -aq\") ;;\n*) exit 1 ;;\n\
                 esac\n"
            ),
        );
        write(
            &rig.state.join(format!("{OLD}.root")),
            &format!("{}\n", rig.old.display()),
        );
        write(
            &rig.state.join(format!("{NEW}.root")),
            &format!("{}\n", rig.new.display()),
        );
        rig.plant_work();
        let adopted = rig.skein(OLD, &["fleet-owner", "check"]);
        assert!(adopted.status.success(), "{}", said(&adopted));
        assert_eq!(rig.owner().fleet_id, rig.id_of(&rig.old));
        Some(rig)
    }

    impl Rig {
        /// Every kind of work a move must keep, and a credential beside each kind.
        fn plant_work(&self) {
            let tree = self.old.join("box-one/tree");
            fs::create_dir_all(&tree).unwrap();
            git_in(&tree, &["init", "-q", "-b", "main"]);
            write(
                &tree.join(".gitignore"),
                ".env\n.worktrees/\nlarge.bin\nserver.pem\n",
            );
            write(&tree.join("src.txt"), "first\n");
            git_in(&tree, &["add", ".gitignore", "src.txt"]);
            git_in(&tree, &["commit", "-qm", "first"]);
            git_in(&tree, &["branch", "topic"]);
            write(&tree.join("src.txt"), "stashed\n");
            git_in(&tree, &["stash", "push", "-q", "-m", "kept"]);
            git_in(
                &tree,
                &["worktree", "add", "-q", ".worktrees/lane", "-b", "lane"],
            );
            write(&tree.join(".worktrees/lane/unpushed.txt"), "lane work\n");
            write(&tree.join("src.txt"), "uncommitted\n");
            write(&tree.join("untracked.txt"), "untracked\n");
            write(&tree.join(".env"), "HAND_MADE=1\n");
            fs::hard_link(tree.join("untracked.txt"), tree.join("hard-link.txt")).unwrap();
            symlink("src.txt", tree.join("link-inside")).unwrap();
            // Not compressible and not all one byte, so a torn copy cannot match by accident.
            let mut large = Vec::with_capacity(LARGE);
            let mut word = 0x9e37_79b9_7f4a_7c15u64;
            while large.len() < LARGE {
                word ^= word << 13;
                word ^= word >> 7;
                word ^= word << 17;
                large.extend_from_slice(&word.to_le_bytes());
            }
            fs::write(tree.join("large.bin"), &large).unwrap();
            // A restore that dies after setting this mode must still be able to finish.
            let locked = tree.join("locked-dir");
            write(&locked.join("kept.txt"), "inside a read-only directory\n");
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
            write(&tree.join("server.pem"), SENTINEL);

            let second = self.old.join("box-two/tree");
            fs::create_dir_all(&second).unwrap();
            git_in(&second, &["init", "-q", "-b", "main"]);
            write(&second.join("two.txt"), "second box\n");
            git_in(&second, &["add", "two.txt"]);
            git_in(&second, &["commit", "-qm", "second"]);

            let box_home = self.old.join("box-one").join("home");
            for rel in [
                ".claude/.credentials.json",
                ".ssh/id_ed25519",
                "tokens/acct",
            ] {
                write(&box_home.join(rel), SENTINEL);
            }
            write(&self.old.join(".skein/asks/q1/ask.json"), "{\"q\":1}\n");

            write(&self.volume.join("VERSION"), "1\n");
            write(&self.volume.join("config.json"), "{}\n");
            write(&self.volume.join("places/box-one.json"), "{}\n");
            write(
                &self
                    .volume
                    .join("boxes/box-one/claude-projects/-t/session.jsonl"),
                "{\"role\":\"user\"}\n",
            );
            write(
                &self.volume.join("repos/proj/store/data/notes.md"),
                "store\n",
            );
            for rel in [
                "api-token",
                "github-pats/acct",
                "repos/proj/store/agent.pem",
                "boxes/box-one/claude-projects/.credentials.json",
            ] {
                write(&self.volume.join(rel), SENTINEL);
            }
        }

        /// `sbx exec <sandbox> skein ...`, in an environment holding nothing of the caller's, so a
        /// `SKEIN_BOX` set around the suite cannot change what is tested.
        fn command(&self, program: &str, args: &[&str]) -> Command {
            let mut command = Command::new(program);
            command
                .args(args)
                .current_dir(&self.base)
                .env_clear()
                .envs(self.environment());
            command
        }

        fn environment(&self) -> Vec<(String, String)> {
            vec![
                (
                    "PATH".to_string(),
                    format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
                ),
                (
                    "HOME".to_string(),
                    self.base.join("user-home").display().to_string(),
                ),
                ("SBX_STATE".to_string(), self.state.display().to_string()),
                ("SBX_VOLUME".to_string(), self.volume.display().to_string()),
            ]
        }

        fn sbx(&self, args: &[&str]) -> Output {
            self.command(self.bin.join("sbx").to_str().unwrap(), args)
                .output()
                .unwrap()
        }

        fn skein(&self, sandbox: &str, args: &[&str]) -> Output {
            let mut full = vec!["exec", sandbox, env!("CARGO_BIN_EXE_skein")];
            full.extend_from_slice(args);
            self.sbx(&full)
        }

        /// [`Rig::attempt`], which must have died at the change named.
        fn traced(&self, sandbox: &str, args: &[&str], kill_at: Option<u64>) -> Traced {
            let traced = self.attempt(sandbox, args, kill_at);
            if let Some(at) = kill_at {
                assert!(
                    traced.killed && traced.code == -libc::SIGKILL,
                    "{args:?} was not killed at change {at}: ended {} after {} changes\n{}",
                    traced.code,
                    traced.mutations,
                    traced.said
                );
            }
            traced
        }

        /// The same crossing, under the tracer, killed at the `kill_at`th change if it gets there.
        fn attempt(&self, sandbox: &str, args: &[&str], kill_at: Option<u64>) -> Traced {
            let sbx = self.bin.join("sbx").display().to_string();
            let mut argv = vec![
                sbx,
                "exec".to_string(),
                sandbox.to_string(),
                env!("CARGO_BIN_EXE_skein").to_string(),
            ];
            argv.extend(args.iter().map(|a| a.to_string()));
            let env: Vec<String> = self
                .environment()
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let output = self.base.join("traced.out");
            let spec = serde_json::json!({
                "argv": argv,
                "env": env,
                "cwd": self.base.display().to_string(),
                "output": output.display().to_string(),
                "kill_at": kill_at,
            });
            let ran = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "linux::traced_run", "--nocapture"])
                .env(TRACE, spec.to_string())
                .output()
                .unwrap();
            assert!(ran.status.success(), "the tracer failed: {ran:?}");
            let report = fs::read_to_string(output.with_extension("report")).unwrap();
            let fields: Vec<&str> = report.split(' ').collect();
            Traced {
                mutations: fields[0].parse().unwrap(),
                killed: fields[1] == "true",
                code: fields[2].parse().unwrap(),
                said: fs::read_to_string(&output).unwrap_or_default(),
            }
        }

        /// How many changes a clean run of this verb makes, after it has run.
        fn count(&self, sandbox: &str, args: &[&str]) -> u64 {
            let clean = self.traced(sandbox, args, None);
            assert_eq!(clean.code, 0, "{args:?}: {}", clean.said);
            assert!(clean.mutations > 0, "{args:?} changed nothing under trace");
            clean.mutations
        }

        fn out(&self, name: &str) -> PathBuf {
            self.base.join("out").join(name)
        }

        fn owner(&self) -> Owner {
            read_owner(&self.volume).unwrap().expect("an owner")
        }

        fn id_of(&self, root: &Path) -> String {
            fs::read_to_string(root.join(".skein/fleet-id"))
                .unwrap()
                .trim()
                .to_string()
        }

        fn source(&self) -> Source {
            let volume = print_of(&self.volume, &ownership_record);
            let mut volume = volume;
            // The volume's own directory is where the record is renamed into.
            volume.get_mut(Path::new("")).unwrap().2 = 0;
            Source {
                volume,
                fleet: print_of(&self.old, &|_| false),
            }
        }

        /// Fails if anything of the source but the ownership record moved.
        fn assert_source(&self, was: &Source, when: &str) {
            let now = self.source();
            assert_same(&was.volume, &now.volume, &format!("{when}: the volume"));
            assert_same(&was.fleet, &now.fleet, &format!("{when}: the old fleet"));
        }

        fn passes(&self, sandbox: &str) -> bool {
            let out = self.skein(sandbox, &["fleet-owner", "check"]);
            match out.status.code() {
                Some(0) => true,
                Some(FENCED_EXIT) => false,
                other => panic!("check on {sandbox} ended {other:?}: {}", said(&out)),
            }
        }

        /// Exactly one fleet passes the door, and the other's real `start` is refused by the fence.
        /// Returns the one that may run.
        fn one_fleet(&self, when: &str) -> &'static str {
            let (old, new) = (self.passes(OLD), self.passes(NEW));
            assert_ne!(
                old, new,
                "{when}: old {old}, new {new}: exactly one may run"
            );
            let (runs, refused) = if old { (OLD, NEW) } else { (NEW, OLD) };
            let start = self.skein(refused, &["start", "box-one"]);
            assert!(!start.status.success(), "{when}: {refused} started a box");
            assert!(
                said(&start).contains("does not own"),
                "{when}: {refused} was refused for another reason: {}",
                said(&start)
            );
            runs
        }

        fn send_args(file: &Path) -> Vec<String> {
            vec!["migrate".into(), "send".into(), file.display().to_string()]
        }

        fn phrase_args(&self, verb: &str, from: &Path, to: &Path) -> Vec<String> {
            vec![
                "migrate".into(),
                verb.into(),
                from.display().to_string(),
                to.display().to_string(),
                "--passphrase-file".into(),
                self.phrase.display().to_string(),
            ]
        }

        fn receive_args(file: &Path, into: &Path) -> Vec<String> {
            vec![
                "migrate".into(),
                "receive".into(),
                file.display().to_string(),
                "--into".into(),
                into.display().to_string(),
            ]
        }

        fn run(&self, sandbox: &str, args: &[String]) -> Output {
            self.skein(sandbox, &refs(args))
        }

        fn must(&self, sandbox: &str, args: &[String]) -> String {
            let out = self.run(sandbox, args);
            assert!(out.status.success(), "{args:?}: {}", said(&out));
            said(&out)
        }

        fn bundle(&self, name: &str) -> PathBuf {
            let file = self.out(name);
            self.must(OLD, &Rig::send_args(&file));
            file
        }

        /// Fails if any kind of work differs in the copy, or a credential reached it.
        fn assert_arrived(&self, into: &Path) {
            let fleet = into.join("fleet");
            for tree in ["box-one/tree", "box-two/tree"] {
                assert_same(
                    &print_of(&self.old.join(tree), &left_behind),
                    &print_of(&fleet.join(tree), &|_| false),
                    tree,
                );
            }
            let home = into.join("home");
            for rel in [
                "boxes/box-one/claude-projects",
                "repos/proj",
                "places",
                "VERSION",
                "config.json",
            ] {
                assert_same(
                    &print_of(&self.volume.join(rel), &left_behind),
                    &print_of(&home.join(rel), &|_| false),
                    rel,
                );
            }
            let (from, to) = (self.old.join("box-one/tree"), fleet.join("box-one/tree"));
            for args in [
                &["for-each-ref", "--format=%(refname) %(objectname)"][..],
                &["stash", "list", "--format=%H %gs"][..],
                &["status", "--porcelain=v1", "--untracked-files=all"][..],
            ] {
                let was = git_in(&from, args);
                assert!(!was.is_empty(), "{args:?} has nothing to compare");
                assert_eq!(was, git_in(&to, args), "git {args:?} differs");
            }
            git_in(&to, &["fsck", "--no-dangling"]);
            assert_eq!(
                fs::read_to_string(to.join(".worktrees/lane/unpushed.txt")).unwrap(),
                "lane work\n",
                "the worktree's uncommitted file"
            );

            let copied = print_of(into, &|rel| rel.starts_with(".skein-receive"));
            assert!(copied.len() > 40, "the copy is not populated");
            for (path, entry) in &copied {
                assert!(
                    !contains(&entry.3, SENTINEL.as_bytes()),
                    "a credential arrived at {}",
                    path.display()
                );
            }
        }

        /// [`Rig::assert_arrived`], for a restore no fleet has run on yet: it names the placeholder
        /// owner, holds a token of its own, and carries no fleet's id.
        fn assert_restored(&self, into: &Path) {
            self.assert_arrived(into);
            for path in print_of(into, &|_| false).keys() {
                assert!(
                    !path.ends_with("fleet-id"),
                    "a fleet id travelled: {}",
                    path.display()
                );
            }
            let home = into.join("home");
            let owner = read_owner(&home).unwrap().expect("a restored owner");
            assert_eq!(owner.fleet_id, RESTORED_FLEET, "{}", home.display());
            assert_eq!(owner.epoch, 1, "a restore makes one claim");
            let token = home.join("api-token");
            let text = fs::read_to_string(&token).unwrap();
            assert!(
                text.trim().len() == 64 && text.trim().chars().all(|c| c.is_ascii_hexdigit()),
                "the restored token is not a whole token: {} bytes",
                text.len()
            );
            assert_eq!(
                fs::metadata(&token).unwrap().mode() & 0o777,
                0o600,
                "the restored token's mode"
            );
            assert!(
                !self
                    .on_restored(into, &["fleet-owner", "check"])
                    .status
                    .success(),
                "a fleet passed the door of a restore nobody has taken over"
            );
        }

        /// Run on a sandbox whose fleet and volume are the restore at `into`.
        fn on_restored(&self, into: &Path, args: &[&str]) -> Output {
            write(
                &self.state.join("restored.root"),
                &format!("{}\n", into.join("fleet").display()),
            );
            write(
                &self.state.join("restored.volume"),
                &format!("{}\n", into.join("home").display()),
            );
            self.skein("restored", args)
        }

        /// The new sandbox as bootstrap leaves it in receive mode, holding a finished restore.
        fn arrive(&self) {
            let bundle = self.bundle("arrive.bundle");
            self.must(NEW, &Rig::receive_args(&bundle, self.new.parent().unwrap()));
            self.assert_restored(self.new.parent().unwrap());
            fs::create_dir_all(self.new.join(".skein")).unwrap();
            fs::write(self.new.join(".skein/receive-mode"), "").unwrap();
        }

        /// Back to: the old fleet owns the volume at epoch 1, the new one is waiting to take it.
        fn reset_ownership(&self) {
            for entry in fs::read_dir(&self.volume).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name();
                if ownership_record(Path::new(&name)) {
                    let path = entry.path();
                    if path.is_dir() {
                        fs::remove_dir_all(path).unwrap();
                    } else {
                        fs::remove_file(path).unwrap();
                    }
                }
            }
            let dot = self.new.join(".skein");
            for entry in fs::read_dir(&dot).unwrap() {
                let entry = entry.unwrap();
                if entry
                    .file_name()
                    .as_bytes()
                    .windows(8)
                    .any(|w| w == b"fleet-id")
                {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
            fs::write(dot.join("receive-mode"), "").unwrap();
            assert!(self.passes(OLD), "the old fleet takes the volume back");
            assert_eq!(self.owner().epoch, 1);
        }

        fn claim_from(&self, sandbox: &str, owner: &str) -> Output {
            self.skein(sandbox, &["fleet-owner", "claim", owner])
        }
    }

    fn refs(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// The first `dense` changes, where a verb sets up what a rerun depends on, then `spread` more
    /// evenly across the rest, always including the last.
    ///
    /// `$SKEIN_REHEARSAL_EVERY_POINT` takes every change instead, which is minutes rather than
    /// seconds and is for a person checking the sample has not missed a window.
    fn points(total: u64, dense: u64, spread: u64) -> Vec<u64> {
        if std::env::var_os("SKEIN_REHEARSAL_EVERY_POINT").is_some() {
            return (1..=total).collect();
        }
        let mut at: Vec<u64> = (1..=dense.min(total)).collect();
        for i in 1..=spread {
            at.push(dense + (total.saturating_sub(dense)) * i / spread);
        }
        at.retain(|n| (1..=total).contains(n));
        at.sort_unstable();
        at.dedup();
        at
    }

    // -----------------------------------------------------------------------------------------
    // The rehearsal
    // -----------------------------------------------------------------------------------------

    /// Fails if any step changes a byte of the work, carries a credential, loses the Docker volume's
    /// name between the plan and the restore, or leaves the old fleet able to run after activation.
    #[test]
    fn a_whole_move_arrives_identical_and_leaves_credentials_behind() {
        let Some(rig) = rig("full") else { return };
        let source = rig.source();

        let plan = rig.skein(OLD, &["migrate", "plan"]);
        assert!(plan.status.success(), "{}", said(&plan));
        assert!(said(&plan).contains(DOCKER_VOLUME), "{}", said(&plan));
        rig.assert_source(&source, "after the plan");

        let bundle = rig.bundle("full.bundle");
        rig.assert_source(&source, "after send");
        let plain = fs::read(&bundle).unwrap();
        assert!(
            !contains(&plain, SENTINEL.as_bytes()),
            "a credential in the bundle"
        );
        assert!(contains(&plain, b"lane work"), "the bundle holds no work");

        let sealed = rig.out("full.sealed");
        rig.must(OLD, &rig.phrase_args("seal", &bundle, &sealed));
        assert!(
            !contains(&fs::read(&sealed).unwrap(), b"lane work"),
            "not encrypted"
        );
        let opened = rig.out("full.opened");
        rig.must(NEW, &rig.phrase_args("unseal", &sealed, &opened));
        assert!(
            fs::read(&opened).unwrap() == plain,
            "unseal changed the bundle"
        );

        let into = rig.new.parent().unwrap().to_path_buf();
        let restored = rig.must(NEW, &Rig::receive_args(&opened, &into));
        assert!(restored.contains(DOCKER_VOLUME), "{restored}");
        rig.assert_restored(&into);
        rig.assert_source(&source, "after receive");
        fs::create_dir_all(rig.new.join(".skein")).unwrap();
        fs::write(rig.new.join(".skein/receive-mode"), "").unwrap();
        assert_eq!(rig.one_fleet("before activation"), OLD);

        let old_id = rig.id_of(&rig.old);
        let out = rig.claim_from(NEW, &old_id);
        assert!(out.status.success(), "{}", said(&out));
        assert_eq!(rig.one_fleet("after activation"), NEW);
        assert!(!rig.new.join(".skein/receive-mode").exists());
        rig.assert_arrived(&into);
        rig.assert_source(&source, "after activation");
    }

    /// Fails if a killed send changes the source, a cut file restores as if it were whole, or a
    /// rerun over the cut file replaces it.
    #[test]
    fn a_send_killed_anywhere_leaves_the_source_and_a_rerun_finishes() {
        let Some(rig) = rig("send") else { return };
        let source = rig.source();
        let total = rig.count(OLD, &refs(&Rig::send_args(&rig.out("count.bundle"))));
        assert!(
            total >= 4,
            "only {total} changes: the fixture did not span the buffer"
        );

        for at in points(total, 2, 6) {
            let when = format!("send killed at change {at} of {total}");
            let file = rig.out(&format!("killed-{at}.bundle"));
            rig.traced(OLD, &refs(&Rig::send_args(&file)), Some(at));
            rig.assert_source(&source, &when);
            assert_eq!(rig.one_fleet(&when), OLD, "{when}");

            if file.exists() {
                let cut = fs::read(&file).unwrap();
                let again = rig.run(OLD, &Rig::send_args(&file));
                assert!(!again.status.success(), "{when}: send wrote over a file");
                assert!(fs::read(&file).unwrap() == cut, "{when}: the file moved");
                let try_into = rig.base.join(format!("try-{at}"));
                let out = rig.run(NEW, &Rig::receive_args(&file, &try_into));
                if out.status.success() {
                    rig.assert_restored(&try_into);
                } else {
                    let text = said(&out);
                    assert!(
                        text.contains("incomplete") || text.contains("Nothing was restored"),
                        "{when}: {text}"
                    );
                }
                remove(&try_into);
            }

            let fresh = rig.bundle(&format!("rerun-{at}.bundle"));
            let into = rig.base.join(format!("rerun-{at}"));
            rig.must(NEW, &Rig::receive_args(&fresh, &into));
            rig.assert_restored(&into);
            remove(&into);
            let _ = fs::remove_file(&file);
            let _ = fs::remove_file(&fresh);
        }
        rig.assert_source(&source, "after every killed send");
    }

    /// Fails if a killed seal touches the plain backup, or a sealed name can appear holding
    /// anything but the whole backup.
    #[test]
    fn a_seal_killed_anywhere_leaves_the_backup_and_a_rerun_finishes() {
        let Some(rig) = rig("seal") else { return };
        let bundle = rig.bundle("seal.bundle");
        let plain = fs::read(&bundle).unwrap();
        let args = rig.phrase_args("seal", &bundle, &rig.out("count.sealed"));
        let total = rig.count(OLD, &refs(&args));

        for at in points(total, 1, 3) {
            let when = format!("seal killed at change {at} of {total}");
            let sealed = rig.out(&format!("killed-{at}.sealed"));
            let args = rig.phrase_args("seal", &bundle, &sealed);
            rig.traced(OLD, &refs(&args), Some(at));
            assert!(
                fs::read(&bundle).unwrap() == plain,
                "{when}: the backup moved"
            );
            assert_eq!(rig.one_fleet(&when), OLD, "{when}");
            if !sealed.exists() {
                rig.must(OLD, &args);
            }
            let opened = rig.out(&format!("opened-{at}.bundle"));
            rig.must(NEW, &rig.phrase_args("unseal", &sealed, &opened));
            assert!(
                fs::read(&opened).unwrap() == plain,
                "{when}: sealed copy differs"
            );
            fs::remove_file(&opened).unwrap();
        }
    }

    /// Fails if a killed unseal can leave a torn file under the bundle's name, or leaves its
    /// plaintext temp readable by anyone else.
    #[test]
    fn an_unseal_killed_anywhere_leaves_no_torn_backup_and_a_rerun_finishes() {
        let Some(rig) = rig("unseal") else { return };
        let bundle = rig.bundle("unseal.bundle");
        let plain = fs::read(&bundle).unwrap();
        let sealed = rig.out("unseal.sealed");
        rig.must(OLD, &rig.phrase_args("seal", &bundle, &sealed));
        let closed = fs::read(&sealed).unwrap();
        let args = rig.phrase_args("unseal", &sealed, &rig.out("count.bundle"));
        let total = rig.count(NEW, &refs(&args));

        let mut temps_seen = 0;
        for at in points(total, 1, 3) {
            let when = format!("unseal killed at change {at} of {total}");
            let name = format!("killed-{at}.bundle");
            let opened = rig.out(&name);
            let args = rig.phrase_args("unseal", &sealed, &opened);
            rig.traced(NEW, &refs(&args), Some(at));
            assert!(
                fs::read(&sealed).unwrap() == closed,
                "{when}: the sealed file moved"
            );
            assert_eq!(rig.one_fleet(&when), OLD, "{when}");
            let partial = format!(".{name}.partial.");
            for entry in fs::read_dir(rig.base.join("out")).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name().to_string_lossy().starts_with(&partial) {
                    let mode = entry.metadata().unwrap().mode() & 0o777;
                    assert_eq!(mode, 0o600, "{when}: a plaintext temp is {mode:o}");
                    temps_seen += 1;
                }
            }
            if opened.exists() {
                assert!(fs::read(&opened).unwrap() == plain, "{when}: a torn backup");
            } else {
                rig.must(NEW, &args);
                assert!(
                    fs::read(&opened).unwrap() == plain,
                    "{when}: the rerun differs"
                );
            }
        }
        assert!(
            temps_seen > 0,
            "no kill left a temp, so its mode was never checked"
        );
    }

    /// Fails if a restore killed at any change cannot be finished by running it again, finishes
    /// with different work, or changes the source. The early points are where the target holds
    /// the restore's directory and not yet its receipt.
    #[test]
    fn a_receive_killed_anywhere_finishes_identically_when_run_again() {
        let Some(rig) = rig("receive") else { return };
        let source = rig.source();
        let bundle = rig.bundle("receive.bundle");
        let counted = rig.base.join("counted");
        let total = rig.count(NEW, &refs(&Rig::receive_args(&bundle, &counted)));
        remove(&counted);

        for (n, at) in points(total, 7, 14).into_iter().enumerate() {
            let when = format!("receive killed at change {at} of {total}");
            let into = rig.base.join(format!("into-{at}"));
            let args = Rig::receive_args(&bundle, &into);
            rig.traced(NEW, &refs(&args), Some(at));
            rig.assert_source(&source, &when);
            assert_eq!(rig.one_fleet(&when), OLD, "{when}");
            // Every other point dies a second time on the way back up. It may finish first,
            // because a resumed restore has less left to do.
            if n % 2 == 1 {
                let second = rig.attempt(NEW, &refs(&args), Some((at / 2).max(1)));
                assert!(
                    second.killed || second.code == 0,
                    "{when}: the resumed restore: {}",
                    second.said
                );
            }
            let out = rig.run(NEW, &args);
            assert!(out.status.success(), "{when}: the rerun: {}", said(&out));
            rig.assert_restored(&into);
            rig.assert_source(&source, &when);
            remove(&into);
        }
    }

    /// Fails if a fleet on a restored volume can run before an explicit claim, or if a claim of
    /// the placeholder killed at any change cannot be finished by running it again.
    #[test]
    fn a_restored_volume_waits_for_an_explicit_claim_even_if_the_claim_is_killed() {
        let Some(rig) = rig("restored") else { return };
        let bundle = rig.bundle("restored.bundle");
        let into = rig.base.join("restored");
        rig.must(NEW, &Rig::receive_args(&bundle, &into));
        rig.assert_restored(&into);
        let home = into.join("home");
        let args = ["fleet-owner", "claim", RESTORED_FLEET];
        let crossing = |at: Option<u64>| rig.traced("restored", &args, at);
        let reset = || {
            fs::remove_dir_all(home.join("fleet-owner.d")).unwrap();
            fs::remove_file(home.join("fleet-owner")).unwrap();
            for entry in fs::read_dir(&home).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name().as_bytes().starts_with(b".skein.tmp.") {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
            skein::fence::mark_restored(&home).unwrap();
            let dot = into.join("fleet/.skein");
            for entry in fs::read_dir(&dot).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name();
                if name.as_bytes().windows(8).any(|w| w == b"fleet-id") {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        };
        let clean = crossing(None);
        assert_eq!(clean.code, 0, "{}", clean.said);
        let total = clean.mutations;
        assert!(rig
            .on_restored(&into, &["fleet-owner", "check"])
            .status
            .success());
        reset();

        for at in 1..=total {
            let when = format!("claim of a restore killed at change {at} of {total}");
            crossing(Some(at));
            let again = rig.on_restored(&into, &args);
            assert!(again.status.success(), "{when}: {}", said(&again));
            assert!(
                rig.on_restored(&into, &["fleet-owner", "check"])
                    .status
                    .success(),
                "{when}: the claimed fleet is still refused"
            );
            assert_ne!(read_owner(&home).unwrap().unwrap().fleet_id, RESTORED_FLEET);
            reset();
        }
    }

    /// Fails if a fleet can adopt a restore that was killed before it finished.
    #[test]
    fn a_restore_killed_partway_is_not_adopted_by_a_fleet() {
        let Some(rig) = rig("partway") else { return };
        let bundle = rig.bundle("partway.bundle");
        let counted = rig.base.join("counted");
        let total = rig.count(NEW, &refs(&Rig::receive_args(&bundle, &counted)));
        remove(&counted);
        let mut home_seen = 0;
        for at in points(total, 0, 12) {
            let into = rig.base.join(format!("partway-{at}"));
            rig.traced(NEW, &refs(&Rig::receive_args(&bundle, &into)), Some(at));
            if !into.join("home").is_dir() {
                continue;
            }
            home_seen += 1;
            fs::create_dir_all(into.join("fleet/.skein")).unwrap();
            let check = rig.on_restored(&into, &["fleet-owner", "check"]);
            assert_eq!(
                check.status.code(),
                Some(FENCED_EXIT),
                "killed at change {at} of {total}: a fleet adopted a partial restore: {}",
                said(&check)
            );
        }
        assert!(home_seen > 0, "no kill left a partial home");
    }

    /// Fails if a flip killed at any change leaves two fleets able to run or none, or if running
    /// the same claim again after the kill does not finish it.
    #[test]
    fn an_activation_killed_anywhere_has_one_owner_and_a_rerun_finishes() {
        let Some(rig) = rig("flip") else { return };
        let source = rig.source();
        rig.arrive();
        let old_id = rig.id_of(&rig.old);
        let total = rig.count(NEW, &["fleet-owner", "claim", &old_id]);
        rig.reset_ownership();

        for at in 1..=total {
            let when = format!("activation killed at change {at} of {total}");
            rig.traced(NEW, &["fleet-owner", "claim", &old_id], Some(at));
            let runs = rig.one_fleet(&when);
            rig.assert_source(&source, &when);

            let again = rig.claim_from(NEW, &old_id);
            assert!(
                again.status.success(),
                "{when}, with {runs} holding it: the same claim run again: {}",
                said(&again)
            );
            assert_eq!(rig.one_fleet(&when), NEW, "{when}: after the rerun");
            assert_eq!(rig.owner().fleet_id, rig.id_of(&rig.new), "{when}");
            assert!(!rig.new.join(".skein/receive-mode").exists(), "{when}");
            rig.assert_source(&source, &when);
            rig.reset_ownership();
        }
    }

    /// Fails if the old fleet cannot take the volume back, its work moved, the new fleet still
    /// passes afterwards, or a stopped-then-restarted old sandbox runs before it has claimed.
    #[test]
    fn a_rollback_gives_the_old_fleet_its_work_and_refuses_the_new_one() {
        let Some(rig) = rig("rollback") else { return };
        let source = rig.source();
        rig.arrive();
        let old_id = rig.id_of(&rig.old);
        let first = rig.claim_from(NEW, &old_id);
        assert!(first.status.success(), "{}", said(&first));
        let new_id = rig.id_of(&rig.new);
        let activated = rig.owner();
        assert_eq!(activated.fleet_id, new_id);

        assert!(rig.sbx(&["stop", OLD]).status.success());
        let stopped = rig.skein(OLD, &["fleet-owner", "check"]);
        assert_eq!(stopped.status.code(), Some(125), "{}", said(&stopped));
        assert!(rig.sbx(&["start", OLD]).status.success());
        assert_eq!(rig.one_fleet("old sandbox restarted"), NEW);

        // Work done on the new fleet stays on the new fleet's disk, and is no part of the source.
        write(
            &rig.new.join("box-one/tree/after-cutover.txt"),
            "new work\n",
        );

        let back = rig.claim_from(OLD, &new_id);
        assert!(back.status.success(), "{}", said(&back));
        assert_eq!(rig.one_fleet("after rollback"), OLD);
        let owner = rig.owner();
        assert_eq!(owner.fleet_id, old_id);
        assert_eq!(
            owner.epoch,
            activated.epoch + 1,
            "a rollback is a new epoch"
        );
        rig.assert_source(&source, "after rollback");
        assert_eq!(
            fs::read_to_string(rig.new.join("box-one/tree/after-cutover.txt")).unwrap(),
            "new work\n",
            "the rollback touched the new fleet's work"
        );
    }

    /// Fails if a rollback killed at any change leaves two fleets able to run or none, or cannot be
    /// finished by running it again.
    #[test]
    fn a_rollback_killed_anywhere_has_one_owner_and_a_rerun_finishes() {
        let Some(rig) = rig("rollback-kill") else {
            return;
        };
        let source = rig.source();
        rig.arrive();
        let old_id = rig.id_of(&rig.old);
        let activate = |rig: &Rig| {
            let out = rig.claim_from(NEW, &old_id);
            assert!(out.status.success(), "{}", said(&out));
            rig.id_of(&rig.new)
        };
        let new_id = activate(&rig);
        let total = rig.count(OLD, &["fleet-owner", "claim", &new_id]);
        rig.reset_ownership();

        for at in 1..=total {
            let when = format!("rollback killed at change {at} of {total}");
            let new_id = activate(&rig);
            rig.traced(OLD, &["fleet-owner", "claim", &new_id], Some(at));
            rig.one_fleet(&when);
            rig.assert_source(&source, &when);

            let again = rig.claim_from(OLD, &new_id);
            assert!(
                again.status.success(),
                "{when}: the same claim run again: {}",
                said(&again)
            );
            assert_eq!(rig.one_fleet(&when), OLD, "{when}: after the rerun");
            assert_eq!(rig.owner().fleet_id, old_id, "{when}");
            rig.assert_source(&source, &when);
            rig.reset_ownership();
        }
    }
}
