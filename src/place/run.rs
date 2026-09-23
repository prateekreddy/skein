//! Running a crossing inside its deadline: `exec`, `attempt`, `bytes` and `write`, what a
//! command that ran returns, and the grandchild probe the deadline tests share.

use super::*;

/// A command that RAN, whatever it exited with.
///
/// [`Place::exec`] and [`Place::bytes`] collapse a non-zero exit into an error built from stderr and
/// throw stdout away. That is right for the callers they have — a script that fails is a failure and
/// its stderr is the reason — and wrong for anything whose subject writes its diagnosis to stdout.
/// `claude -p` is exactly that: measured against the real CLI, an unknown model exits 1 with the
/// explanation on STDOUT and an unrelated stdin warning on stderr. A caller given only stderr is
/// told "exited 1" and nothing else, which is a failure this codebase shipped once already at the
/// layer above (`215d143`) and would have shipped again the moment the model call moved in here.
#[derive(Debug, Clone)]
pub struct Ran {
    pub code: i32,
    pub out: Vec<u8>,
    pub err: String,
}

impl Place {
    fn command(&self, script: &str) -> Command {
        // The one place a fleet-scope command is turned into a process, and therefore the one place
        // a test may stand in for it. See [`seam`] for why this is a compile-time substitution and
        // not a `$PATH` entry or an environment variable.
        let argv = self.spawning(self.exec_argv(script));
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        command
    }

    /// Run `script` and return its stdout as text. A non-zero exit is an error carrying the box's
    /// own stderr, because the box's words are always more use than "exited 1".
    pub fn exec(&self, script: &str, timeout: Duration) -> Result<String, String> {
        let out = self.bytes(script, timeout)?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Run `script` with `feed` on its **stdin**, and report what happened, rather than whether it
    /// worked.
    ///
    /// `Err` means it did not run at all — the sandbox was unreachable, or it outlived `timeout`.
    /// Any exit code is `Ok`, because "it ran and said no" is an answer, and only the caller knows
    /// what to make of it. See [`Ran`].
    ///
    /// **`feed` is a parameter and not a second method** (SKEIN-799). There is one caller —
    /// [`crate::fleet::model_call_in_box`] — and a twin that differed only in its stdin would be
    /// one function nothing calls plus one place for the two to stop agreeing. `&[]` is a pipe
    /// that closes immediately rather than `/dev/null`, which is the same thing to every script
    /// this could ever carry: a script that reads stdin gets EOF either way.
    ///
    /// **Why this grew a stdin at all.** [`Self::exec_argv`] ends `argv.push(self.wrap(script))`,
    /// so the whole script is ONE argv element — and Linux caps a single element at
    /// `MAX_ARG_STRLEN`, 32 pages, which is 131,072 bytes on ordinary 4 KiB-page hardware. A model
    /// call's prompt carries a diff and 3 of 27 real ones measured for SKEIN-706 are over that, so
    /// a script with the prompt inside it could not be spawned at all. A pipe has no such ceiling,
    /// and it is also not `/proc/<pid>/cmdline` — the rule [`Self::write`] was written for, applied
    /// to the payload rather than only to the credential beside it.
    pub fn attempt(&self, script: &str, feed: &[u8], timeout: Duration) -> Result<Ran, String> {
        let mut command = self.command(script);
        // `output_with_timeout_fed`, not `bounded_output`: the second says "it failed to start or
        // exceeded the 30s timeout" for both, and this is the one caller where the difference is
        // the whole answer. A model call that never left the host was reported to a
        // person as "skein could not start `claude` … set SKEIN_CLAUDE_BIN to its full path", for a
        // host where `claude` was fine and `sbx` was missing. The message it gets instead names the
        // program that failed AND the PATH skein had, which is the one fact the reader cannot
        // recover afterwards — by the time they look, they are looking at their shell's PATH.
        //
        // `_fed` rather than `_why` only for the stdin: it is the same `run_bounded`, and it is
        // what already carries the prompt on the LOCAL arm of this same call (`ai::tried`), so the
        // two destinations now deliver the prompt by the same mechanism as well as to the same
        // place.
        let out = crate::util::output_with_timeout_fed(&mut command, feed.to_vec(), timeout)?;
        Ok(Ran {
            code: out.status.code().unwrap_or(-1),
            out: out.stdout,
            err: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }

    /// Run `script` here and return its stdout as **raw bytes**.
    ///
    /// **One way in.** This used to try the in-sandbox agent first and fall back to spawning the
    /// crossing — a pairing that existed because the crossing was a host-to-guest hop that could
    /// stall, and the agent was the thing built to survive it. The hop is gone and so is the agent
    /// (architecture §13a, SKEIN-521), which takes the fallback twin with it: there is no
    /// transport-failure-versus-command-failure distinction left to draw, because there is no
    /// transport between the caller and the command.
    pub fn bytes(&self, script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
        let mut command = self.command(script);
        let out = bounded_output(&mut command, "the crossing", timeout)?;
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if detail.is_empty() {
                format!("the crossing exited {}", out.status)
            } else {
                detail
            });
        }
        Ok(out.stdout)
    }

    /// The argv that runs `script` here with a body arriving on its stdin.
    ///
    /// It used to differ from [`Self::exec_argv`] by an `-i` on the hop, without which `sbx exec`
    /// wired no pipe to the guest and the body was silently discarded. There is no hop to flag now
    /// (SKEIN-576): the pipe is [`Self::write`]'s own `Stdio::piped()`, on a process this one
    /// spawns directly. The two argvs are the same shape, and this stays its own function because
    /// the *call* still differs — a write has a body and a deadline that has to cover sending it.
    pub fn write_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach();
        argv.extend(self.enter());
        argv.extend(self.shell());
        argv.push(self.wrap(script));
        argv
    }

    /// Run `script` with `body` on its **stdin**.
    ///
    /// **The only way skein sends a box anything sensitive**, and that predates everything else
    /// here: an argv is visible in `ps` to anything sharing this machine, so a token passed as an
    /// argument is a token in every process listing and every shell history. A body on stdin is
    /// not.
    ///
    /// There used to be a second implementation of this — a chunked write to the in-sandbox agent,
    /// chosen for bodies under a cap — because the spawned path crossed a host-to-guest hop that
    /// could stall. There is no hop and no agent (SKEIN-521), and the surviving path is the one
    /// that never had a ceiling: it streams from a thread with a deadline.
    pub fn write(&self, script: &str, body: &[u8], timeout: Duration) -> Result<(), String> {
        // The seam covers this path too, and it has to: a write is a fleet-scope command like any
        // other, and a test that could stand in for `exec` but not for `write` would run the real
        // one — which is the hazard `seam` exists for, on the path that carries a body. Through
        // `spawning` for the same reason: the refusal has to be on every path that spawns, or the
        // one it is missing from is the one a fixture reaches the real fleet through.
        let argv = self.spawning(self.write_argv(script));
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            // Nothing reads stdout here, and an unread pipe blocks the child once its buffer fills
            // (~64KB) — a chatty command would look like a hang until the deadline killed it.
            .stdout(Stdio::null())
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own process group, so the deadline below ends the WORK. Nothing on this path is
            // a leaf program: `sbx` starts work of its own, and the shell it runs the script under
            // is `dash` here, which FORKS a `-c` command rather than exec'ing it — so the guest
            // command is already a grandchild by the time there is anything to kill. Killing the
            // recorded pid left it running with `ppid` 1, on the path EVERY install skein does
            // takes (SKEIN-912, SKEIN-916). A NEW group and not the inherited one: a negative kill
            // against skein's own group is skein killing itself.
            //
            // **The cost, paid rather than taken**, as `util::run_bounded` states it: a child in
            // its own group no longer shares the terminal's foreground group, so Ctrl-C stops
            // reaching this command by that route. The guard below hands the group to
            // `util::forward_interrupts`'s handler instead, which is the same payment
            // `run_bounded` makes. This loop has no grace arm of its own, so a script that ignores
            // `SIGINT` is waited on until `timeout`; a second Ctrl-C ends the group at once.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("the crossing could not be started: {e}"))?;
        // Registered before this side blocks on anything, so a Ctrl-C arriving between the spawn
        // and the first `try_wait` finds the group rather than an empty table. Underscored because
        // every way out of the loop below is a `return`: the guard is dropped by the scope ending,
        // on the line after the reap rather than somewhere a `drop` call could be written.
        let _forwarding = crate::util::forwarding(child.id() as libc::pid_t);
        // Drained on a thread for the same reason, and kept: this used to pipe stderr and never
        // read it, so every failure here reported a bare "exited 1" with the cause discarded.
        let errors = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                let _ = pipe.read_to_string(&mut buf);
                buf
            })
        });
        // Written on its own thread, and that is not symmetry with the stderr drain — it is the one
        // way this call has a deadline at all. A pipe holds ~64KB; past that the write blocks
        // until the guest reads, and the guest is `cat` in a sandbox that may be exactly the thing
        // that has stopped answering. The deadline below starts *after* this returns, so a blocking
        // write was an unbounded wait no timeout covered — with an `sbx exec` held open for its
        // whole duration. Every install skein does goes through here, so a sandbox that went quiet
        // took the caller with it.
        let mut pipe = child.stdin.take().ok_or("sbx exec: no stdin")?;
        let wanted = body.len();
        let body = body.to_vec();
        // How many bytes the pipe has ACCEPTED, published as they are accepted — and the reason
        // there is a counter here at all rather than a `write_all` (SKEIN-944). The deadline arm
        // below has to say which half stalled, and it used to ask `writing.is_finished()`. That
        // question is asked *after* `end_group`, and `end_group` is what kills the guest — which
        // closes the read end, which releases a writer blocked on a full pipe, which finishes the
        // thread. So the kill created the answer the message then reported: on a loaded box the
        // main thread could be descheduled between the two, the writer woke with `EPIPE` first,
        // and a body that had never left the pipe buffer was reported as "sent, so the box has
        // it". Measured at 11 failures in 40 runs with 16 busy loops on 11 CPUs, and a probe on
        // either side of `end_group` read `false` before it in all 40 and `true` after it in 7.
        //
        // A byte count cannot race that way. Bytes are only added when the kernel has taken them,
        // nothing can be accepted once the read end is gone, and — the second thing
        // `is_finished()` got wrong — a write that FAILED also finishes its thread, so a body that
        // died with `EPIPE` at byte zero read as fully sent too.
        //
        // **In chunks, and the chunk size is the whole reason the count is worth reading.** A
        // blocking pipe write does not return when the buffer fills — the kernel holds the call
        // until every requested byte has landed — so handing the write the whole body reports 0
        // until it reports all of it, which is the same single bit `is_finished()` gave. Measured:
        // against a guest that never reads, a one-call 1 MB write sat on `0 of 1048576 bytes
        // accepted` with a quarter of a megabyte provably in the pipe. A chunk smaller than any
        // pipe's buffer turns that into progress, so the refusal can tell a guest that stopped
        // reading immediately from one that read most of the body and then stopped.
        const CHUNK: usize = 8 * 1024;
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = std::sync::Arc::clone(&accepted);
        let writing = std::thread::spawn(move || {
            let mut at = 0;
            while at < body.len() {
                match pipe.write(&body[at..body.len().min(at + CHUNK)]) {
                    // What `write_all` calls `WriteZero`, spelled out: the pipe stopped taking
                    // bytes without saying why, and looping on it would spin for ever.
                    Ok(0) => {
                        return Err("sbx exec: writing stdin: the pipe accepted nothing".into())
                    }
                    Ok(n) => {
                        at += n;
                        counting.store(at, std::sync::atomic::Ordering::SeqCst);
                    }
                    // Exactly what `write_all` does with it: a signal interrupted the call before
                    // any bytes moved, so retry rather than fail.
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(format!("sbx exec: writing stdin: {e}")),
                }
            }
            pipe.flush()
                .map_err(|e| format!("sbx exec: writing stdin: {e}"))
            // `pipe` drops here, which is the EOF the guest command is waiting for.
        });
        // Deadlined rather than a bare wait: a box that never exits would otherwise hang the
        // caller — and one of this function's callers is holding a freshly minted credential.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) if status.success() => {
                    // Joined only once the child is gone, so this cannot be the thing that blocks:
                    // a writer still stuck on a full pipe is released by the child's exit closing
                    // the read end.
                    return match writing.join() {
                        Ok(Err(e)) => Err(e),
                        _ => Ok(()),
                    };
                }
                Some(status) => {
                    let detail = errors
                        .and_then(|h| h.join().ok())
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    return Err(if detail.is_empty() {
                        format!("sbx exec exited {status}")
                    } else {
                        detail
                    });
                }
                None if std::time::Instant::now() >= deadline => {
                    // The GROUP killed AND reaped. This comment used to say "killed and reaped",
                    // which was true of the direct child and of nothing it had started — see the
                    // spawn above. A kill without a wait also leaves a zombie per timed-out write,
                    // and this is the path a struggling fleet takes over and over, so `end_group`
                    // does both.
                    crate::util::end_group(&mut child);
                    // Read from the counter above rather than from `writing.is_finished()`, and
                    // the byte figures are in the message because a reader who is told which half
                    // stalled deserves the number that says so (SKEIN-944).
                    let sent = accepted.load(std::sync::atomic::Ordering::SeqCst);
                    return Err(format!(
                        "sbx exec did not finish within {}s — the body was {} ({sent} of {wanted} \
                         bytes accepted)",
                        timeout.as_secs(),
                        match sent == wanted {
                            true => "sent, so the box has it and did not finish with it",
                            // The distinction worth having: a guest that never drained the pipe is
                            // a sandbox that has stopped, not a script that is slow.
                            false => "still being sent, so nothing in the box read it",
                        }
                    ));
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

/// A grandchild that outlives the process skein holds a handle on, and the two questions to ask
/// about it.
///
/// **Shared by the four sites SKEIN-916 names**, and shared rather than copied because what it
/// encodes is one rule: a deadline has to end the WORK, and the only way to see the difference is
/// to watch something that is NOT the direct child. `src/takeover.rs`, `src/github.rs` and the
/// `write` above all reach it from their own test modules. `src/bin/skein-server/upload.rs` carries
/// its own copy (`mod upload_deadline`) and says so, because a `#[cfg(test)]` item in this crate's
/// library is not visible from a binary target — that is a process boundary, not an oversight.
///
/// **Why a fractional `sleep` and not a marker file.** The token IS the argument, so it is in the
/// grandchild's `/proc/<pid>/cmdline` and nowhere else on the machine: "is it still running" is
/// then a question about that process rather than about a pid, which a fast machine could have
/// recycled between the two readings. It is also what keeps a neighbouring suite's `sleep` from
/// answering for this one's.
#[cfg(test)]
pub(crate) mod grouptest {
    use std::path::{Path, PathBuf};

    /// Somewhere between one and two of these per test. This process's pid is in the token as well
    /// as the counter, so two test BINARIES running at once cannot mint the same one — which is
    /// what makes the `/proc` scan below a scan for THIS fixture's processes and not a pattern
    /// over a name a neighbour might share.
    static MINTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    pub(crate) struct Escapee {
        token: String,
        pidfile: PathBuf,
    }

    /// A grandchild to come, named after `label` and this process.
    pub(crate) fn escapee(dir: &Path, label: &str) -> Escapee {
        let n = MINTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Escapee {
            // A duration, because it is passed to `sleep` — and an unlikely one, because it is
            // also the name being searched for.
            token: format!("600.{}{n:03}", std::process::id()),
            pidfile: dir.join(format!("{label}.grandchild")),
        }
    }

    impl Escapee {
        /// A script that starts the grandchild, records its pid, and then does not finish.
        ///
        /// The background `sleep` is a child of the shell, which is itself the child skein spawned
        /// — so it is exactly the process a `kill` on the recorded pid does not reach. `/bin/sh` on
        /// this box is dash, which FORKS a `-c` command rather than exec'ing it, so a production
        /// site's real work sits where this `sleep` sits.
        ///
        /// **The shell's own wait carries the token too**, and that is not decoration: dash forks
        /// for it as well, so a `kill` on the recorded pid leaves TWO processes behind. Naming both
        /// is what lets [`Self::gone`] ask whether the group ended rather than whether one pid did,
        /// and what lets the drop below take everything this fixture started with it.
        pub(crate) fn script(&self) -> String {
            format!(
                "sleep {} & echo $! > {}; sleep {}",
                self.token,
                self.pidfile.display(),
                self.token
            )
        }

        /// The argv a test seam or a `$PATH` stand-in hands back for [`Self::script`].
        pub(crate) fn argv(&self) -> Vec<String> {
            vec!["/bin/sh".into(), "-c".into(), self.script()]
        }

        /// **It is THERE**: the pid of the running grandchild, or a panic naming what was looked
        /// for.
        ///
        /// This half is not optional. An absence that was never a presence proves nothing
        /// (SKEIN-833): without it, a stand-in that failed to start anything at all would pass the
        /// "gone" assertion below and report the fix working.
        pub(crate) fn there(&self) -> u32 {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while std::time::Instant::now() < until {
                if let Some(pid) = self.recorded() {
                    if self.naming().contains(&(pid as libc::pid_t)) {
                        return pid;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!(
                "no grandchild naming {} was running before the deadline — the command under test \
                 never started one, so the absence afterwards would prove nothing about the kill \
                 (pidfile {}: {:?})",
                self.token,
                self.pidfile.display(),
                std::fs::read_to_string(&self.pidfile).ok()
            );
        }

        /// **It is GONE**: nothing named by this fixture is running any more, `pid` included.
        ///
        /// The whole set rather than the one pid, because the defect leaves more than one process
        /// behind and a test that looked at one of them would report the other as fixed.
        pub(crate) fn gone(&self, pid: u32) {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(3000);
            let mut left = Vec::new();
            while std::time::Instant::now() < until {
                left = self.naming();
                if left.is_empty() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            // `drop` takes them, so a red test does not also leave behind the thing it is
            // complaining about — and the panic below says what it saw before that happened.
            panic!(
                "the deadline passed and {left:?} were still running — pid {pid} is the GRANDCHILD \
                 the script recorded, and every one of these names `sleep {}`, so the kill reached \
                 the handle this side recorded and not the work it started",
                self.token
            );
        }

        /// The pid the script wrote, once it has written all of it.
        fn recorded(&self) -> Option<u32> {
            std::fs::read_to_string(&self.pidfile)
                .ok()?
                .trim()
                .parse()
                .ok()
        }

        /// Every process whose argv carries this fixture's token.
        ///
        /// Read out of `/proc/<pid>/cmdline` rather than matched against a name, so a recycled pid
        /// answers no and a neighbour's `sleep` is not this one. The environment is not read: this
        /// fixture puts its token on the argv itself, which is the surface a scan of `cmdline`
        /// can see (SKEIN-687 is the same lesson from the other direction).
        fn naming(&self) -> Vec<libc::pid_t> {
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for entry in entries.flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
                    continue;
                };
                if let Ok(raw) = std::fs::read(entry.path().join("cmdline")) {
                    if String::from_utf8_lossy(&raw).contains(&self.token) {
                        found.push(pid);
                    }
                }
            }
            found
        }
    }

    impl Drop for Escapee {
        /// Nothing this fixture started outlives it — on the panicking path as much as the
        /// returning one, which is the path that matters, because a failing deadline test is
        /// exactly the one that has something still running.
        ///
        /// Only pids whose argv carries this fixture's own token: never a pattern over a program
        /// name. A stale alternation that matches nothing is indistinguishable from a clean box by
        /// its output alone (SKEIN-647), so this derives the name it kills from the same string it
        /// spawned.
        fn drop(&mut self) {
            for pid in self.naming() {
                // SAFETY: `kill` has no memory effects, and `pid` names a process whose argv
                // carries a token minted by this process — so it is one this fixture's own script
                // started.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// **A crossing that outruns its deadline loses the work, not only the shell in front of it.**
    ///
    /// `Place::write` is the path EVERY install skein does takes, and its timeout used to end with
    /// `child.kill()` + `child.wait()` under a comment saying "Killed AND reaped" — true of the
    /// direct child and of nothing it had started. `/bin/sh` here is dash, which FORKS a `-c`
    /// command rather than exec'ing it, and `sbx` starts work of its own besides, so the guest
    /// command is a grandchild by the time there is anything to kill (SKEIN-916).
    ///
    /// **Both halves, in that order.** The grandchild is asserted RUNNING while the crossing is
    /// still inside its deadline, and gone after it. Only the second is about the fix; without the
    /// first, a substitution that failed to start anything at all would pass this test and report
    /// the kill working (SKEIN-833).
    ///
    /// **What makes it fail:** removing `.process_group(0)` from the spawn in [`Place::write`].
    /// The kill then reaches the shell, the backgrounded `sleep` is reparented to init and goes on
    /// running, and `gone` fires naming the pid it can still see.
    #[test]
    fn a_crossing_that_misses_its_deadline_takes_its_grandchildren_with_it() {
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        // Pinned rather than set: `EnvPins` puts them back from `Drop`, so a failing assertion
        // below does not leave them pointing at a `TempDir` the next test will not find.
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"));
        pins.set("SKEIN_HOME", dir.join("home"));

        let escapee = grouptest::escapee(dir, "place-write");
        let argv = escapee.argv();
        let _stood_in = seam::install(Box::new(move |_argv: &[String]| Some(argv.clone())));

        // On a thread, because the assertion that matters first is about the world WHILE the call
        // is still inside its deadline.
        let here = own_sandbox("skein-fleet");
        let crossing = std::thread::spawn(move || {
            here.write("cat > /dev/null", b"a body", Duration::from_secs(4))
        });

        let pid = escapee.there();
        let outcome = crossing.join().expect("the crossing thread panicked");
        let said = outcome.expect_err("a script that sleeps for 9999s came back inside 4s");
        assert!(
            said.contains("did not finish within"),
            "the deadline is not what ended this, so what follows is not about the deadline: {said}"
        );
        escapee.gone(pid);
    }

    /// The `(N of M bytes accepted)` a deadline refusal from [`Place::write`] carries, as numbers.
    ///
    /// Written as a parse rather than as a `contains("65536 of 1048576")`, because the claim the
    /// test is making is a RELATION between the two figures and not either figure: a literal would
    /// be asserting this machine's pipe capacity, which is a property of the kernel it happens to
    /// be running on. It panics rather than returning an `Option` so that a refusal which stopped
    /// carrying the numbers fails here, naming the message, instead of silently satisfying a
    /// comparison of two zeroes.
    fn accepted_of(why: &str) -> (usize, usize) {
        let figures = why
            .rsplit_once('(')
            .and_then(|(_, tail)| tail.strip_suffix(" bytes accepted)"))
            .and_then(|figures| figures.split_once(" of "))
            .unwrap_or_else(|| panic!("the refusal carries no byte count: {why}"));
        let read = |n: &str| {
            n.parse()
                .unwrap_or_else(|e| panic!("{n:?} is not a byte count ({e}): {why}"))
        };
        (read(figures.0), read(figures.1))
    }

    /// A guest that never reads its stdin must time out, not hang for ever.
    ///
    /// A pipe holds a bounded buffer — 64 KiB on a stock kernel, 262144 bytes on the box this
    /// sentence was measured on. Past that the write blocks until something on the other end reads,
    /// and the deadline in `Place::write` only started *after* the write returned — so a body larger
    /// than the pipe, sent to a sandbox that had stopped answering, was an unbounded wait that no
    /// timeout covered, holding an `sbx exec` open for its whole duration. Every install skein does
    /// goes through this call, including the ones at server start.
    ///
    /// Stood in for through the seam, with a body far larger than the buffer.
    ///
    /// It used to put a `sleep 60` on `$PATH` as `sbx`. There is no `sbx` hop to intercept now, so
    /// that fake was bypassed and the write ran for real — against this machine (SKEIN-592). The
    /// seam is the sanctioned way for a test to say what a fleet-scope command runs, and nothing
    /// outside this process can select it.
    ///
    /// **It also reads the byte count out of the refusal, and that is the part that used to be a
    /// race** (SKEIN-944, and SKEIN-984/SKEIN-985 are the same failure seen twice more). The
    /// refusal named its half from `writing.is_finished()`, read after `end_group` — and
    /// `end_group` kills the guest, which closes the read end, which finishes the writer. So the
    /// kill manufactured the answer, and on a loaded box this test failed with `the body was sent`
    /// against a guest that provably never read a byte. It reproduced at 11 failures in 40 runs
    /// under 16 busy loops on 11 CPUs. Nothing about the test's own clock was wrong: its only
    /// wall-clock assertion allows 20s for a 2s deadline, and the 2s belongs to the test, while
    /// every production caller of `write` passes 30s or 60s (`src/fleet/`, `src/sandbox.rs`).
    #[test]
    fn a_write_to_a_box_that_never_reads_it_gives_up_instead_of_hanging() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // Never reads stdin, never exits on its own: the box that has gone quiet.
        let _stood_in = seam::install(Box::new(|_argv: &[String]| {
            Some(vec!["sh".to_string(), "-c".into(), "sleep 60".into()])
        }));

        let place = crate::place::own_sandbox("skein-fleet");
        // Bigger than any pipe this test could be handed, and the size is chosen against a
        // MEASURED ceiling rather than a remembered one. It read `1 << 20` with the comment
        // "sixteen times the pipe", which assumed the 64 KiB a stock kernel gives: on this box
        // `F_GETPIPE_SZ` answers 262144, so it was four times, and
        // `/proc/sys/fs/pipe-max-size` is 1048576 — the body's exact size. A kernel that gave a
        // pipe its own maximum would have swallowed the whole body, and the refusal would have
        // been right to say it was sent while this test called that a bug.
        let body = vec![b'x'; 4 << 20];
        let started = std::time::Instant::now();
        let why = place
            .write("cat > /tmp/x", &body, Duration::from_secs(2))
            .expect_err("a guest that never reads must not succeed");
        let spent = started.elapsed();

        assert!(
            spent < Duration::from_secs(20),
            "the write hung past its own deadline ({spent:?}) — this is the shape that took the \
             whole server with it"
        );
        assert!(why.contains("did not finish"), "{why}");
        // And it says which half stalled, because they are different faults: a body that was sent
        // means the box has it and is slow, one still being sent means nothing read it at all.
        assert!(why.contains("nothing in the box read it"), "{why}");

        // And the half it names is DERIVED, which is what stops this being a coin toss on a busy
        // box. The refusal carries the two numbers it decided from, and the only thing that can
        // put a figure here strictly between nothing and the whole body is a pipe that took a
        // bufferful and then stopped — which is the guest not reading. A classification taken
        // from the writer thread's liveness could not produce this line at all.
        let (sent, wanted) = accepted_of(&why);
        assert_eq!(
            wanted,
            body.len(),
            "the refusal misreports the body size: {why}"
        );
        assert!(
            0 < sent && sent < wanted,
            "the refusal claims {sent} of {wanted} bytes reached a guest that never read: {why}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    // The connection is *held*, and that is the entire transport rather than an optimisation: the
    // stall this exists to survive blocks new channels into the sandbox while established ones keep
    // flowing, so a client that reconnected per call would reproduce the failure it was built to
    // avoid — and only under load, where it would look like the agent had made no difference.
    //
    // It reconnects silently on a dead socket, which is why this has to be asserted from the other
    // end: the agent defaulted to HTTP/1.0, where `BaseHTTPRequestHandler` hangs up after every
    // response no matter what the client asks for, and every call had been paying for a fresh
    // connection with nothing to show it.
    // Everything skein installs in a box goes through `write`, so leaving it on `sbx exec` meant
    // starting a box still needed the daemon two or three times however healthy the transport was.
    // The agent is installed *into* the sandbox and outlives the skein that installed it, so a
    // running agent may be older than the host talking to it. `/write` has to be declined before
    // the body is sent, because an upload's bytes come off a network socket that has already been
    // drained — there is no second copy to fall back with.
    // A write that fails used to report `sbx exec exited 1` and drop the reason on the floor, which
    // is how "mkdir: cannot create directory '/boxes': Permission denied" reached nobody.
    #[test]
    fn a_failed_write_reports_what_the_sandbox_said() {
        let _g = env_lock();
        let home = tempdir();
        // A home of its own, so `unreachable_from_fleet` reads a config this test wrote rather than
        // whatever a neighbour left behind. Bound after `home`, so the pin goes back before the
        // directory it names is removed.
        let mut env = env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // **Stood in for through the seam, not through `$PATH`.** This used to put a failing
        // `sbx` on PATH; there is no `sbx` hop to intercept now, so the fake was bypassed and the
        // command ran for real (SKEIN-592). The seam is the sanctioned way for a test to say what
        // a fleet-scope command runs, and nothing outside this process can select it.
        let _stood_in = seam::install(Box::new(|_argv: &[String]| {
            Some(vec![
                "sh".to_string(),
                "-c".into(),
                "cat >/dev/null; echo 'mkdir: cannot create directory' >&2; exit 1".into(),
            ])
        }));

        let place = Place {
            name: "b".into(),
            sandbox: "fleet".into(),
            at: Where::SandboxItself,
        };
        let err = place
            .write("cat > /boxes/x", b"body", Duration::from_secs(10))
            .unwrap_err();
        assert!(err.contains("cannot create directory"), "{err}");
    }

    /// What a spawned crossing reported about how it was spawned.
    struct Captured {
        script: String,
        cmdline_at: PathBuf,
        stdin_at: PathBuf,
        done_at: PathBuf,
    }

    /// A fixture that reports how it was spawned: the spawned process's **own**
    /// `/proc/<pid>/cmdline`, and everything it was fed.
    ///
    /// **Shared by the two tests below deliberately, and what makes that honest is worth stating**
    /// (SKEIN-822). [`Place::attempt`] and [`Place::write`] differ in the *call*, not in the
    /// capture: [`Place::write_argv`] and [`Place::exec_argv`] have the same body — `reach()`,
    /// `enter()`, `shell()`, then `wrap(script)` as ONE element — both go through
    /// [`Place::spawning`], so both are seam-covered, and both end in a `bash -c` whose `$$` is the
    /// process the kernel lists.
    ///
    /// **What differs is everything after the argv, which is why one test cannot cover both.** The
    /// body reaches stdin by two separate implementations: `attempt` hands it to
    /// `util::output_with_timeout_fed`, `write` pipes it from a thread of its own under a deadline
    /// it starts before the write rather than after. Either could grow a payload on argv without
    /// the other changing at all.
    ///
    /// **`done` exists because [`Place::write`] nulls stdout.** `attempt` returns what the program
    /// printed and can prove it ran that way; `write` reads no stdout at all — an unread pipe
    /// blocks a chatty guest at ~64KB — so the script's last act is the only evidence left that it
    /// got past `cat`. Without it a crossing that died before the redirect would leave the same
    /// "no marker found" as a clean one.
    fn reporting_how_it_was_spawned(dir: &std::path::Path, called: &str) -> Captured {
        let cmdline_at = dir.join(format!("cmdline-the-{called}-ran-under"));
        let stdin_at = dir.join(format!("stdin-the-{called}-was-fed"));
        let done_at = dir.join(format!("the-{called}-got-to-the-end"));
        let script = format!(
            "tr '\\0' '\\n' < /proc/$$/cmdline > {cmdline}\n\
             cat > {stdin}\n\
             printf 'the crossing read it'\n\
             : > {done}\n",
            cmdline = sh_quote(&cmdline_at.display().to_string()),
            stdin = sh_quote(&stdin_at.display().to_string()),
            done = sh_quote(&done_at.display().to_string()),
        );
        Captured {
            script,
            cmdline_at,
            stdin_at,
            done_at,
        }
    }

    /// The property, asked of one crossing that really ran: the payload is on its stdin, and in
    /// neither the argv skein built nor the argv the kernel lists.
    ///
    /// **Every assertion here is preceded by a guard that fails loudly on a capture holding
    /// nothing**, because "the marker is not in it" is true of an empty string — a fixture that
    /// never ran, a `/proc` read that failed, a path nothing wrote. That is the SKEIN-647 shape,
    /// and it is the half of this that decides whether the rest is worth anything.
    ///
    /// **The guards ask `contains`, and a sabotage is what settled that.** They asked `==` first,
    /// and an equality guard is the assertion that speaks when the payload goes back into the
    /// script element: it reported "the argv does not carry the script" about an argv carrying the
    /// script AND the payload. A guard has to survive the regression it guards an assertion for,
    /// or it replaces that assertion's message with its own.
    fn payload_only_on_stdin(
        called: &str,
        argv: &[String],
        it: &Captured,
        marker: &str,
        fed: &str,
    ) {
        assert!(
            it.done_at.exists(),
            "the {called} did not run to the end, so every capture below is whatever was at that \
             path beforehand — which is nothing"
        );
        assert!(
            argv.iter().any(|a| a.contains(it.script.as_str())),
            "the argv skein built for the {called} does not carry the script, so the assertion \
             below is about nothing: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.contains(marker)),
            "the payload is in the argv skein built for the {called}"
        );

        // And the same thing asked of the kernel rather than of skein's own value.
        let cmdline = fs::read_to_string(&it.cmdline_at)
            .unwrap_or_else(|e| panic!("the {called} captured no /proc/<pid>/cmdline at all: {e}"));
        assert!(
            cmdline.contains(it.script.as_str()),
            "the capture does not hold the {called}'s own script element, so it would satisfy the \
             assertion below however the payload was sent: {} bytes captured",
            cmdline.len()
        );
        assert!(
            !cmdline.contains(marker),
            "the payload is in the spawned {called}'s /proc/<pid>/cmdline — world readable for as \
             long as the call runs, to anything sharing this machine"
        );
        assert_eq!(
            fs::read_to_string(&it.stdin_at)
                .unwrap_or_else(|e| panic!("the {called} was fed nothing at all: {e}")),
            fed,
            "the {called} was not handed the payload on stdin, or not all of it"
        );
    }

    /// **The payload a crossing carries rides its stdin, and is nowhere in that process's own
    /// `/proc/<pid>/cmdline`** (SKEIN-813).
    ///
    /// The property was stated in three places and asserted in none of them: `fleet/model.rs`'s "the
    /// payload in `ps` … what was in it is the diff of a pull request, private repositories
    /// included", [`Place::attempt`]'s own "a pipe … is also not `/proc/<pid>/cmdline`", and
    /// SKEIN-706's done-when. What guarded it was that `model_call_script` no longer takes a
    /// prompt, so the exact revert fails to *compile* — a guard against one revert rather than
    /// against the property. Any new caller of [`Place::attempt`] that inlines its payload into
    /// the script, or a convenience that appends it to the argv, passes every other gate here.
    ///
    /// **Measured on a real process, running production's own argv.** The seam records the argv
    /// and hands it straight back, which is how a fixture says it meant *this* crossing rather
    /// than standing in for it (`tests/fleet_move.rs`'s run arm does the same) — so what
    /// `/proc/$$/cmdline` holds is the argv skein built, not a stand-in carrying a copy of it.
    /// [`Where::SandboxItself`] aimed at the sandbox this process stands in is the mode that makes
    /// that safe to run: [`Place::reach`] and [`Place::enter`] are both empty, so there is no
    /// `sbx` hop and no `nsenter`, and what spawns is `env PATH=… bash -c <script>` doing exactly
    /// what the fixture above says.
    ///
    /// **The payload is deliberately SMALL, and that is what this adds to
    /// `ai::tests::a_call_with_a_box_reaches_the_box_and_carries_its_prompt_on_stdin`.** That one
    /// sends 600,000 bytes because its subject is the ceiling: past `MAX_ARG_STRLEN` — 32 pages,
    /// 131,072 bytes on 4 KiB-page hardware — a payload back inside the script cannot be spawned
    /// at all, so a regression fails there on "the box was never reached" and its argv assertion
    /// never gets to speak. Under the cap the spawn *succeeds* and the leak is silent. That is the
    /// case this covers, and it is the one a reader of `ps` would actually have got.
    ///
    /// **What makes it fail**, named before it was written and then done: build the argv the old
    /// shape built, by putting `feed` back inside the script in [`Place::attempt`]. The
    /// `/proc/<pid>/cmdline` assertion is the one that fires.
    #[cfg(unix)]
    #[test]
    fn the_payload_a_crossing_carries_is_on_its_stdin_and_not_in_its_cmdline() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        // Pinned beside it: `$SKEIN_FLEET_ROOT` unpinned falls back to `/boxes`, and this fixture
        // spawns for real — an unpinned one is it operating the live fleet (SKEIN-685).
        env.set("SKEIN_FLEET_ROOT", home);
        fs::write(
            home.join("config.json"),
            r#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();

        // Where the crossing reports what it was actually handed. The argv the seam RECORDS is
        // skein's own value; these files are what any process on this machine could read off it.
        let it = reporting_how_it_was_spawned(home, "crossing");

        // Shaped so a grep for it finds this test and nothing else.
        let marker = "SKEIN-813-PAYLOAD-MARKER";
        let feed = format!("{marker} ").repeat(2_000);
        assert!(
            feed.len() < 131_072,
            "the payload has grown past MAX_ARG_STRLEN on 4 KiB-page hardware, so a payload put \
             back inside the script would fail to SPAWN and this test would stop being about what \
             `ps` shows: {} bytes",
            feed.len()
        );

        let seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> = Default::default();
        let recorder = std::sync::Arc::clone(&seen);
        let _at = seam::install(Box::new(move |argv: &[String]| {
            recorder.lock().unwrap().push(argv.to_vec());
            // Handed back unchanged: the process that runs is the crossing skein built, so the
            // cmdline read below is production's, not a copy of it passed to a stand-in.
            Some(argv.to_vec())
        }));

        let ran = own_sandbox("skein-fleet")
            .attempt(&it.script, feed.as_bytes(), Duration::from_secs(30))
            .expect("the crossing never ran, so nothing below is about a process");
        assert_eq!(
            (ran.code, String::from_utf8_lossy(&ran.out).into_owned()),
            (0, "the crossing read it".to_string()),
            "the crossing did not run to completion: {}",
            ran.err
        );

        // **The crossing was reached exactly once**, or the argv below is about some other call.
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            1,
            "the crossing was not spawned exactly once: {} times",
            seen.len()
        );
        payload_only_on_stdin("crossing", &seen[0], &it, marker, &feed);

        drop(_at);
    }

    /// **A body sent to a box rides the write's stdin, and is nowhere in that process's own
    /// `/proc/<pid>/cmdline` — and this is the path that carries CREDENTIALS** (SKEIN-822).
    ///
    /// [`Place::write`] says it in its own words: "the only way skein sends a box anything
    /// sensitive … an argv is visible in `ps` to anything sharing this machine, so a token passed
    /// as an argument is a token in every process listing and every shell history. A body on stdin
    /// is not." Nothing asserted it. The same shape as SKEIN-813, one path along.
    ///
    /// **Why the sibling test does not cover this, checked rather than assumed.** The argv half is
    /// shared — [`Place::write_argv`] and [`Place::exec_argv`] have the same body, and both go
    /// through [`Place::spawning`], so the seam covers this path too. Everything after it is a
    /// second implementation: `write` spawns the child itself, pipes the body from a thread of its
    /// own, and waits under a deadline that starts before the body is sent rather than after. It
    /// also **nulls stdout**, so the evidence that the crossing ran cannot be what it printed —
    /// hence the `done` file in the fixture above.
    ///
    /// **The body is deliberately the size of a real credential**, and that is the sharper form of
    /// the SKEIN-813 argument. A prompt is large enough that `MAX_ARG_STRLEN` would eventually
    /// refuse to spawn one on argv; a token is ~2 KB at the outside, so **nothing structural would
    /// ever stop it going on argv**. On this path the assertion is the only thing there is.
    ///
    /// **What makes it fail**: put the body back into the script in [`Place::write`] — the shape
    /// where a caller interpolates a token into the command it is about to run. Done, and both the
    /// argv assertion and the `/proc/<pid>/cmdline` assertion fire.
    #[cfg(unix)]
    #[test]
    fn the_body_a_write_carries_is_on_its_stdin_and_not_in_its_cmdline() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home);
        env.set("SKEIN_FLEET_ROOT", home);
        fs::write(
            home.join("config.json"),
            r#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();

        let it = reporting_how_it_was_spawned(home, "write");

        // Shaped like the thing this path actually carries — a credential — and so a grep for it
        // finds this test and nothing else.
        let marker = "SKEIN-822-CREDENTIAL-MARKER";
        let body = format!("{marker}.").repeat(64);
        assert!(
            body.len() < 131_072,
            "the body has grown past MAX_ARG_STRLEN, so this test would start being rescued by a \
             spawn failure rather than asserting anything: {} bytes",
            body.len()
        );

        let seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> = Default::default();
        let recorder = std::sync::Arc::clone(&seen);
        let _at = seam::install(Box::new(move |argv: &[String]| {
            recorder.lock().unwrap().push(argv.to_vec());
            Some(argv.to_vec())
        }));

        own_sandbox("skein-fleet")
            .write(&it.script, body.as_bytes(), Duration::from_secs(30))
            .expect("the write never ran, or the crossing it spawned exited non-zero");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            1,
            "the write was not spawned exactly once: {} times",
            seen.len()
        );
        payload_only_on_stdin("write", &seen[0], &it, marker, &body);

        drop(_at);
    }
}
