//! Stopping a box: killing and sweeping its namespace, reaching its session, and the anchor
//! that says which process tree is the box's.

use super::*;

/// End every process living in a box's mount namespace — the backstop under `cgroup.kill`.
///
/// **A box IS its namespace.** `box-session.sh` says so where it explains the anchor: box alive ⇔
/// tmux server alive ⇔ namespace joinable. Membership of it is the one property a process cannot
/// shed by forking, by leaving the tmux tree, or by being started before whatever ceiling skein
/// meant to apply — which is exactly the set `tmux kill-server` misses and `cgroup.kill` misses
/// whenever the cgroup was never made.
///
/// It is not a replacement for [`box_cgroup_kill`], and both run. The cgroup is the better tool
/// where it exists: one write, atomic, no window in which something forks away. This is what
/// answers the three ways `box-session.sh` fails to make one — no limits computed, no delegation in
/// the sandbox, or the join itself failing — each of which leaves the kill writing to a path that
/// is not there, where `2>/dev/null || true` turns it into silence. That silence is the reported
/// bug: a box stops, and what it started keeps running.
///
/// # The two guards, and why neither is optional
///
/// A pid is a name inside one boot and pids recycle inside one. If this swept the namespace of
/// whatever now holds `ns_pid`, and that were an ordinary sandbox process, the namespace would be
/// **the sandbox's own** — and the sweep would kill init, dockerd, the fleet agent and every other
/// box. So:
///
///   * `(generation, ns_start)` must match, exactly as [`crate::place::Place::guard`] spends them
///     immediately before a crossing, and parsed the same way as [`crate::place::anchor_probe`] —
///     the field after the last `) `, because a process's own name can contain spaces and brackets.
///   * **the namespace must not be the killer's own.** Cheap, absolute, and independent of the
///     first: whatever else has gone wrong, a sweep that would end the process running it is not a
///     box being stopped. This is the one that makes the catastrophic case impossible rather than
///     unlikely.
///
/// The first guard is spelled `[ -n {gen} ]` and **not** `[ -n "{gen}" ]`, which is not a style
/// choice: `{gen}` arrives already single-quoted by `sh_quote`, so the double quotes made the test
/// ask about the literal two-character string `''` — always non-empty, always true. It was inert
/// for as long as it existed, and what covered for it was the `$boot` comparison two tests along,
/// which happens to be false as well when the boot id is readable. On a sandbox where it is not,
/// an empty generation and an empty `$boot` compared equal and the sweep went ahead on the start
/// time alone (FLEET-8).
///
/// Read **before** anything is killed. The anchor is in the box's own cgroup, so it is among the
/// first things `cgroup.kill` ends, and a namespace looked up afterwards is a dead pid and an empty
/// answer.
///
/// `TERM` then `KILL`, with a pause between: a build, a database, an editor with unsaved state all
/// have something to do on the way out, and a stop that only ever `KILL`s is a stop people learn to
/// route around.
pub fn namespace_kill(ns_pid: u32, generation: &str, ns_start: u64) -> String {
    format!(
        "boxns=\"\"; \
         mine=\"$(readlink /proc/self/ns/mnt 2>/dev/null)\"; \
         boot=\"$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)\"; \
         seen=\"$(sed -n 's/.*) //p' /proc/{pid}/stat 2>/dev/null | cut -d' ' -f20)\"; \
         if [ -n {gen} ] && [ \"$boot\" = {gen} ] && [ \"$seen\" = {start} ]; then \
           boxns=\"$(readlink /proc/{pid}/ns/mnt 2>/dev/null)\"; \
         fi; \
         [ -n \"$boxns\" ] && [ \"$boxns\" = \"$mine\" ] && boxns=\"\"",
        pid = ns_pid,
        gen = sh_quote(generation),
        start = sh_quote(&ns_start.to_string()),
    )
}

/// The sweep itself, run after the namespace has been captured by [`namespace_kill`].
///
/// Split from the capture because the capture has to happen *before* the cgroup is killed and the
/// sweep after it — the anchor is among the first things that dies, and a namespace read afterwards
/// is an empty string. Two calls in one script rather than one, and the shell variable between them
/// is what carries the answer across the killing.
///
/// # Membership is read once, and the second pass kills processes rather than a number
///
/// **`$boxns` is an inode number, and the kernel hands it straight back out.** `mnt:[4026533200]`
/// names the box's namespace only while that namespace exists; the moment its last member exits the
/// number is free, and the next `unshare` — another box starting, a `bwrap`, a container — is given
/// it. Killing every member is precisely what frees it, so a *second* scan for that number, after
/// the pause the graceful signal needs, is a scan for whoever holds the number now.
///
/// This was not a worry. Instrumenting the old two-scan form on this box, with `bwrap` running
/// beside it as any other test binary or box start does: **2 sweeps in 20 matched a stranger** —
/// once `pid=1801 comm=sleep cmd="sleep 0.4"`, in a namespace created 1.89 seconds *after* the
/// sweep began, reporting the box's own `mnt:[4026533200]`. The old script would have sent it
/// `KILL`. That is a stop of one box ending processes in another, and it is the same catastrophe
/// [`namespace_kill`]'s two guards exist to make impossible, arriving through the identifier
/// instead of through the pid.
///
/// And with `tests/fleet_launch/` running beside it rather than a bare `bwrap`, **2 of 8** —
/// where what matched was the whole of the box that test had just built: its
/// `tmux -S …/boxes/demo-smoke/session.sock`, its `skein-startup.sh`, its `sync-install.sh`, its
/// `claude plugin marketplace add`, its `ssh git@github.com`. Every one of them in
/// `mnt:[4026533197]`, the number a namespace that had died two seconds earlier used to have.
///
/// So the scan happens once, before anything is signalled — the one moment the namespace is
/// provably alive, because the anchor's own members are still in it — and `TERM` goes out as each
/// member is found. The escalation then revisits *those processes*, by the identity this codebase
/// already spends on the anchor ([`crate::place::anchor_probe`]): pid **and** start time, plus the
/// namespace still reading as the box's. A pid that has been reused fails the start time; a number
/// that has been reused was never in `$members`. Neither guard is the other's backstop.
///
/// What it gives up, said plainly: a process forked by a member *after* that member was scanned,
/// whose whole cohort then dies inside the pause, is not signalled. That window is microseconds
/// wide, `cgroup.kill` covers it wherever the cgroup exists, and the alternative — rescanning a
/// number the kernel has already given to somebody else — is measured above.
///
/// **It reports success for having run**, never for having found something. The old form's last
/// command was `[ "$sig" = TERM ]` on the `KILL` pass, so the fragment exited non-zero exactly when
/// the sweep had done the most work, with nothing on stderr to say so. Both callers end their
/// script with `exit 0` and never saw it; the test that composes this fragment alone did, as a
/// failure with an empty message.
pub fn namespace_sweep() -> String {
    "if [ -n \"$boxns\" ]; then \
       members=\"\"; \
       for entry in /proc/[0-9]*; do \
         [ \"$(readlink \"$entry/ns/mnt\" 2>/dev/null)\" = \"$boxns\" ] || continue; \
         born=\"$(sed -n 's/.*) //p' \"$entry/stat\" 2>/dev/null | cut -d' ' -f20)\"; \
         kill -TERM \"${entry##*/}\" 2>/dev/null || continue; \
         [ -n \"$born\" ] && members=\"$members ${entry##*/}:$born\"; \
       done; \
       [ -n \"$members\" ] && sleep 2; \
       for member in $members; do \
         pid=\"${member%%:*}\"; \
         [ \"$(readlink \"/proc/$pid/ns/mnt\" 2>/dev/null)\" = \"$boxns\" ] || continue; \
         [ \"$(sed -n 's/.*) //p' \"/proc/$pid/stat\" 2>/dev/null | cut -d' ' -f20)\" \
           = \"${member#*:}\" ] || continue; \
         kill -KILL \"$pid\" 2>/dev/null; \
       done; \
       :; \
     fi"
    .to_string()
}

/// The shell that stops a shared box: end its session, then end whatever outlived it.
///
/// Its own function because it is a contract rather than a detail — the two steps answer different
/// halves of "stop the box", and a test can run it against a scratch cgroup without a sandbox.
///
/// **`cgroup.kill` alone, and no `tmux kill-server`** (ISO-6). The two overlapped rather than
/// dividing the work: `box-session.sh` puts its own shell in the box's cgroup *before* it starts
/// tmux ("a process's children start in its cgroup, so putting THIS shell in it means the tmux
/// [server inherits it]"), so the server and every pane are in the cgroup that one write ends.
/// What `kill-server` added was a fleet-scope tmux client on a socket under the box's own
/// read-write root — see [`session_socket_probe`] — which is to say it added the exposure and not
/// the kill. `namespace_sweep` still covers the three ways the launcher fails to make a cgroup at
/// all. See [`box_cgroup_kill`].
///
/// The socket is unlinked last. It is what `place::local_liveness` asks about, so removing it before
/// the processes are gone would make the box read as stopped while it was still running.
/// Lives here rather than in `sandbox` because it is built of this module's own pieces — the
/// namespace look, the cgroup kill, the sweeps — and because [`ensure_box_session`] needs it to end
/// an orphan session; `sandbox` already leans on `fleet`, and the reverse edge would be a cycle.
pub(crate) fn stop_script(name: &str, rec: &crate::place::PlaceRecord) -> String {
    format!(
        "{look}; {kill}; {sweep}; {containers}; {orphans}; rm -f {sock}; exit 0",
        look = namespace_kill(rec.ns_pid, &rec.generation, rec.ns_start),
        sock = sh_quote(&rec.sock),
        kill = box_cgroup_kill(name),
        sweep = namespace_sweep(),
        containers = box_containers_kill(name),
        orphans = unattributed_containers(),
    )
}

/// **Is anything listening on a box's session socket — asked by connecting, never by being a tmux
/// client** (ISO-6).
///
/// A box's root is bound read-write into that box (`box-session.sh`'s bind table, row 12), and
/// `session.sock` is in it. So the socket is a thing the box controls, and every fleet-scope
/// `tmux -S <that socket> …` skein ran was a tmux *client* attaching to a server the box could
/// replace: tmux's own client honours MSG_SHELL and MSG_EXEC from the server unconditionally — its
/// dispatch loop execs what it is sent, in tmux's client.c — so a rogue server runs a command **in
/// the client**, at fleet scope, on the owner's next stop, start or readiness check. (Those are
/// tmux's names, not skein's, which is why they are not spelled as symbols here.) Architecture
/// §9.5.1 treats the
/// socket as a crossing already, but reasons about reading it rather than about what a client does
/// when the server is hostile.
///
/// A `connect()` and an immediate drop cannot be answered with anything: nothing is read, so there
/// is no message to honour. It is `crate::place::local_liveness`'s shape, spelled as shell so it
/// can travel into the sandbox — the same question `tmux has-session` was really being asked, minus
/// the session name, which is not a distinction any caller here spends.
///
/// **`python3`, because a shell has no way to open an `AF_UNIX` socket** — no redirection, no
/// builtin. A sandbox without python3 answers "nothing is listening", and that is the direction
/// every caller already degrades in: `box_is_ready` says so out loud ("the caller's fallback is to
/// start the box, which is what it did unconditionally before"), and a launch that finds no session
/// runs the launcher, which refuses rather than starting a second one. There is deliberately no
/// `tmux` fallback for a fleet without python3: a fallback that restores the exposure is a way to
/// ask for it.
pub(super) fn session_socket_probe(sock: &str) -> String {
    format!(
        "python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).connect(sys.argv[1])' \
         {sock} 2>/dev/null",
        sock = sh_quote(sock)
    )
}

/// Whether the record still addresses the live session — the question "alive" does not answer.
pub(super) enum Reach {
    /// The anchor is this boot's process, exactly as recorded: the session is the box.
    Current,
    /// A record from before the stamp existed proves nothing in either direction.
    Unprovable,
    /// The session answers tmux and no crossing can enter it — the record names another boot, or a
    /// pid that is gone or reused. The string says which, in [`anchor_matches`]'s words.
    Orphan(String),
}

/// Ask the sandbox whether `record` still names the process behind `name`'s live session.
///
/// One `sbx exec`, on paths where a session was just observed alive — attach and resume, both
/// human-initiated. `Err` is the sandbox not answering, which is a different fact from any of the
/// three [`Reach`] answers and must not be read as one: ending a session on a question that timed
/// out would end a box for being briefly slow.
pub(super) fn session_reach(
    fleet: &Place,
    name: &str,
    record: &PlaceRecord,
) -> Result<Reach, String> {
    if record.generation.is_empty() || record.ns_start == 0 {
        return Ok(Reach::Unprovable);
    }
    let out = fleet.exec(
        &crate::place::anchor_probe(record.ns_pid),
        Duration::from_secs(10),
    )?;
    Ok(match crate::place::parse_anchor_probe(&out) {
        Some(seen) => match anchor_matches(name, record, &seen) {
            Ok(()) => Reach::Current,
            Err(why) => Reach::Orphan(why),
        },
        None => Reach::Orphan(format!(
            "its recorded anchor pid {} no longer exists while its session still answers",
            record.ns_pid
        )),
    })
}

/// What the launcher printed on stdout, or an error naming what it printed instead.
///
/// The marker rather than "the last line": the launcher runs a login shell inside the box, and a
/// profile that echoes anything at all would otherwise become the pid skein enters.
pub fn anchor_from_launch(out: &str) -> Result<u32, String> {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("SKEIN_ANCHOR "))
        .next_back()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .ok_or_else(|| {
            format!(
                "the launcher did not report an anchor pid; it said: {}",
                crate::util::clip(out.trim(), 300)
            )
        })
}

/// Ask the sandbox what `pid` is, and stamp it.
///
/// One `sbx exec` on the launch path, which is not a hot path — and it has to be a separate one
/// from the launch itself, because the answer has to come from OUTSIDE every box namespace. A box
/// holds `CAP_SYS_ADMIN` in its own user namespace and can mount over its view of `/proc`.
pub(super) fn stamp_anchor(
    sandbox: &str,
    name: &str,
    ns_pid: u32,
) -> Result<(String, u64), String> {
    let out = own_sandbox(sandbox).exec(&anchor_probe(ns_pid), Duration::from_secs(10))?;
    parse_anchor_probe(&out).ok_or_else(|| {
        format!("the sandbox could not say what pid {ns_pid} is, so {name} has no usable address")
    })
}

/// The anchor for a box whose session is already live, taken from what skein recorded and *checked*.
///
/// Never from the box. The pidfile under a box's own root is bound read-write, so a box can put a
/// sibling's tmux server pid there — and this is the path where reading it would matter most,
/// because no launcher runs here to report anything.
///
/// Refuses rather than guesses. A live session skein cannot address is a real state and a rare one
/// (it means the record predates this check, or was lost), and the honest answer is a sentence
/// naming the fix — not an address that might be another box.
pub(super) fn adopt_anchor(sandbox: &str, name: &str) -> Result<(u32, String, u64), String> {
    let record = shared_record(name).ok_or_else(|| {
        format!(
            "{name} has a live session but no placement record, so skein has no address for it \
             that did not come from the box; restart it with `skein restart {name}`"
        )
    })?;
    let seen = stamp_anchor(sandbox, name, record.ns_pid)?;
    anchor_matches(name, &record, &seen)?;
    Ok((record.ns_pid, seen.0, seen.1))
}

/// Is the process at the recorded pid still the one skein recorded?
///
/// Pure, and separate from the reading, because this is the decision: every way of answering "no"
/// means **the box is gone**, and none of them means "enter this instead". Getting that backwards
/// is the whole vulnerability — a wrong address is not a degraded address, it is another box.
fn anchor_matches(name: &str, record: &PlaceRecord, seen: &(String, u64)) -> Result<(), String> {
    let restart = format!("restart it with `skein restart {name}`");
    if record.generation.is_empty() || record.ns_start == 0 {
        return Err(format!(
            "{name}'s placement record predates the anchor check, so skein cannot prove the \
             session it would enter is {name}'s; {restart}"
        ));
    }
    if record.generation != seen.0 {
        return Err(format!(
            "{name}'s anchor belongs to an earlier boot of the sandbox, so that pid now names \
             some other process; {restart}"
        ));
    }
    if record.ns_start != seen.1 {
        return Err(format!(
            "{name}'s anchor pid has been reused by a different process since skein recorded it; \
             {restart}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The anchor is whatever the launcher *marked*, not whatever it printed last.
    ///
    /// The launcher runs a login shell inside the box, so the box's own `.profile` gets to write to
    /// that stream first. Taking the last line would let a box choose the pid skein enters by
    /// echoing a number on login — the same confused deputy the pidfile gave it, through the
    /// channel that replaced the pidfile.
    #[test]
    fn the_launcher_report_is_read_by_its_marker_and_not_by_position() {
        assert_eq!(anchor_from_launch("SKEIN_ANCHOR 4242\n").unwrap(), 4242);
        assert_eq!(
            anchor_from_launch("welcome to your box\nSKEIN_ANCHOR 4242\n1234\n").unwrap(),
            4242,
            "a profile that prints a number after the report must not become the anchor"
        );
        let said = anchor_from_launch("1234\n").unwrap_err();
        assert!(
            said.contains("did not report") && said.contains("1234"),
            "an unmarked stream is a failure that quotes what it saw: {said}"
        );
    }

    /// Half an answer is no answer. Both halves are required, and a missing one must not read as a
    /// match against a record that also has a missing one.
    #[test]
    fn an_anchor_probe_that_could_not_answer_is_not_an_identity() {
        assert_eq!(
            parse_anchor_probe("abc-123 987\n"),
            Some(("abc-123".to_string(), 987))
        );
        assert_eq!(parse_anchor_probe(" 987\n"), None, "no boot id");
        assert_eq!(parse_anchor_probe("abc-123 0\n"), None, "no start time");
        assert_eq!(parse_anchor_probe("abc-123\n"), None, "one field");
        assert_eq!(parse_anchor_probe(""), None);
    }

    /// Never "enter this instead": a pid that no longer names what skein recorded names something
    /// else in the same sandbox, and every other candidate is another box.
    #[test]
    fn an_anchor_that_does_not_match_is_a_dead_box_not_a_different_one() {
        let good = PlaceRecord {
            ns_pid: 42,
            generation: "boot-a".into(),
            ns_start: 900,
            ..Default::default()
        };
        assert!(anchor_matches("web-main", &good, &("boot-a".into(), 900)).is_ok());

        let cycled = anchor_matches("web-main", &good, &("boot-b".into(), 900)).unwrap_err();
        assert!(cycled.contains("earlier boot"), "{cycled}");

        let reused = anchor_matches("web-main", &good, &("boot-a".into(), 901)).unwrap_err();
        assert!(reused.contains("reused"), "{reused}");

        // A record from before the stamp existed cannot be checked, so it cannot be trusted — the
        // upgrade path is a restart, not a shrug.
        let old = PlaceRecord {
            ns_pid: 42,
            ..Default::default()
        };
        let said = anchor_matches("web-main", &old, &("boot-a".into(), 900)).unwrap_err();
        assert!(said.contains("predates"), "{said}");
        assert!(
            said.contains("skein restart web-main"),
            "every refusal names the fix: {said}"
        );
    }

    /// **skein never reads the anchor pidfile**, and this is what makes that a rule rather than a
    /// sentence in a comment.
    ///
    /// The file exists on purpose: the launcher writes it "only for the box to read", and §9.5 R1
    /// says so in words — *the pidfile in the box's tree may remain for the box's own use, and skein
    /// must never read it*. It sits under the box's own root, which is bound read-write, so a box
    /// can put a **sibling tmux server's pid** there; anything of skein's that read it would then
    /// provision, diff, upload or take over inside a namespace the box chose. That is the confused
    /// deputy SKEIN-4 closed, and it is closed by *nobody reading*, which is exactly the kind of
    /// property that decays the first time somebody needs a pid and sees an obvious file.
    ///
    /// So the source is the thing asserted. Two uses are allowed and named: the helper itself, and
    /// the launch command, which passes the path to the launcher so the launcher can write it.
    #[test]
    fn nothing_in_skein_reads_the_anchor_pidfile() {
        let mut offenders: Vec<String> = Vec::new();
        let mut walk = vec![std::path::PathBuf::from("src")];
        while let Some(dir) = walk.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                for (n, line) in body.lines().enumerate() {
                    if !line.contains("box_pidfile") && !line.contains("anchor.pid") {
                        continue;
                    }
                    // Reading is what is forbidden. Naming the path to hand to the launcher, and
                    // excluding it from an archive, are not reads.
                    let reads = line.contains("read_to_string")
                        || line.contains("read(")
                        || line.contains("cat ");
                    if reads {
                        offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "something reads the anchor pidfile, which a box can point at a sibling namespace:\n{}",
            offenders.join("\n")
        );
    }

    /// Read `(boot_id, starttime)` for a live pid, exactly as `anchor_probe` does.
    fn stamp_of(pid: u32) -> (String, u64) {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let start = stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().nth(19))
            .and_then(|f| f.parse().ok())
            .unwrap_or(0);
        (boot.trim().to_string(), start)
    }

    fn alive(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }

    /// The reported bug, reproduced and then closed: a process that left the box's tmux tree.
    ///
    /// `setsid` is the reparenting, and it is the honest stand-in for what people actually do —
    /// `npm run dev &`, a watcher, `nohup`, anything an agent starts and walks away from. It is
    /// gone from tmux's tree the moment it exists, so `kill-server` cannot see it, and if the box
    /// never got a cgroup then `cgroup.kill` writes to a path that is not there and says nothing.
    /// What is left is the namespace, which is the one thing the process cannot leave.
    #[test]
    fn a_stop_reaches_what_walked_out_of_the_tmux_tree() {
        if !crate::testutil::bwrap_works() {
            crate::testutil::skip(
                "bwrap cannot make a namespace here, so there is none to be a box",
            );
            return;
        }
        let dir = crate::testutil::tempdir();
        let anchor_at = dir.join("anchor");
        let strayed_at = dir.join("strayed");
        let stubborn_at = dir.join("stubborn");
        let bwrap_err = dir.join("bwrap.err");
        // A namespace with three processes in it: one that would be the tmux server, one that
        // reparented away from it, and one that ignores `TERM`. No `--unshare-pid`, for the reason
        // `box-session.sh` gives — the anchor has to be the pid skein sees from outside.
        let spawned = std::process::Command::new("bwrap")
            // The root, and nothing else bound over it. A private `/tmp` is what a real box gets and
            // it is wrong here: the two pid files are written by absolute path, and binding over
            // `/tmp` made those paths resolve to nothing inside the namespace — so the fixture
            // never reported, and the test hung instead of failing.
            .args(["--dev-bind", "/", "/", "--"])
            .arg("bash")
            .arg("-c")
            .arg(format!(
                // The strayed process reports its OWN pid. `$!` names the `setsid` wrapper, whose
                // fork is the thing that actually reparents — so the first version of this recorded
                // the anchor's child and the premise assertion below caught it, which is what that
                // assertion is for.
                // A minute, not five. On the passing path the sweep is what ends all of these,
                // and on a failing one the guard below does — so the number only matters when the
                // test binary itself is killed before that guard's `Drop` can run.
                //
                // The stubborn one ignores `TERM` and is what makes the escalation to `KILL` a
                // tested path rather than a hoped-for one: a build, a database, an editor with
                // unsaved state are all processes that take their time or refuse outright, and a
                // sweep that only ever manages the polite half ends a box that is still running.
                // The trap is installed before the pid is reported, so a process this test has
                // heard of is always already stubborn.
                "setsid bash -c 'echo $$ > {strayed}; exec sleep 60' </dev/null >/dev/null 2>&1 & \
                 bash -c 'trap \"\" TERM; echo $$ > {stubborn}; while :; do sleep 0.5; done' \
                 </dev/null >/dev/null 2>&1 & \
                 echo $$ > {anchor}; sleep 60",
                strayed = strayed_at.display(),
                stubborn = stubborn_at.display(),
                anchor = anchor_at.display(),
            ))
            // Both nulled, and it is not tidiness. A spawned child inherits this process's stdout,
            // and the strayed process is by construction one that outlives its parent — so an
            // inherited pipe is held open by a process nothing is waiting for, and `cargo test`
            // appears to hang long after the test itself has finished. Diagnosed the slow way.
            // ...and stderr to a FILE rather than to `/dev/null`, which costs nothing against the
            // reasoning above — a file holds no pipe open — and is the only place bwrap's own
            // refusal is recorded. Nulling it is why a whole CI log never said `userns`.
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(&bwrap_err).expect("a file for bwrap's stderr"))
            .spawn()
            .expect("start a box-like namespace");
        // **Into the guard on the line after the spawn, and each process into it as it reports
        // itself** (SKEIN-1011). On the passing path the sweep under test is what ends all three; on
        // a failing one — exactly the run where the sweep did NOT end them — the two statements
        // that used to sit at the bottom of this body never ran, and the stubborn loop has no clock
        // to run out, so it spun until somebody noticed. It traps `TERM`; the guard sends `KILL`.
        //
        // The guard cannot be what makes this test pass: it acts only in its `Drop`, at the end of
        // this scope, after every assertion below. Proven by making the sweep kill nothing — the
        // test then fails at "the box's own anchor survived being stopped", and the three recorded
        // pids are gone afterwards all the same.
        let mut boxlike = crate::testutil::BoxlikeNamespace::holding(spawned);

        let read = |at: &std::path::Path| -> u32 {
            for _ in 0..100 {
                if let Ok(text) = std::fs::read_to_string(at) {
                    if let Ok(pid) = text.trim().parse() {
                        return pid;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let said = std::fs::read_to_string(&bwrap_err).unwrap_or_default();
            panic!(
                "the box-like namespace never reported {}; bwrap said: {}",
                at.display(),
                said.trim()
            );
        };
        // Recorded one at a time rather than after all three: a `read` that panics on the second
        // must not strand the first. The anchor's `starttime` is the one `inside` recorded, so the
        // stamp the sweep is handed and the stamp the guard kills by are one read, not two.
        let anchor = read(&anchor_at);
        let anchor_start = boxlike.inside(anchor);
        let strayed = read(&strayed_at);
        boxlike.inside(strayed);
        let stubborn = read(&stubborn_at);
        boxlike.inside(stubborn);
        assert!(
            alive(anchor) && alive(strayed) && alive(stubborn),
            "the fixture never started: anchor {anchor} alive {}, strayed {strayed} alive {}, \
             stubborn {stubborn} alive {}",
            alive(anchor),
            alive(strayed),
            alive(stubborn)
        );
        // The premise, checked rather than assumed, or this test would pass against `kill-server`
        // alone. And checked on the **session**, not the parent: what puts a process beyond tmux is
        // leaving its session, which is what `setsid` does and what `npm run dev &` inside a box
        // amounts to. The parent can stay exactly where it was — the first version of this asserted
        // on `ppid` and failed, correctly, because `setsid` execs in place when the caller is not
        // already a process-group leader.
        let field = |pid: u32, at: usize| -> String {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .unwrap_or_default()
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.split_whitespace().nth(at).map(str::to_string))
                .unwrap_or_default()
        };
        assert_ne!(
            field(strayed, 3),
            field(anchor, 3),
            "the strayed process is still in the anchor's session, so tmux would have reached it \
             and this proves nothing"
        );

        let (generation, _) = stamp_of(anchor);
        let script = format!(
            "{look}; {sweep}",
            look = namespace_kill(anchor, &generation, anchor_start),
            sweep = namespace_sweep(),
        );
        let ran = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("run the sweep");
        // Everything the run can still tell anyone, because the failure this assertion used to
        // report was the literal string "the sweep failed: " — an exit status with nothing after
        // the colon, which is why it took three people and two work items to find out that the
        // status was the sweep's own success at escalating to `KILL`. A message that cannot name
        // what went wrong is why a red run gets re-run instead of read.
        assert!(
            ran.status.success(),
            "the sweep exited {code} — it must report success for having run, whatever it found. \
             stdout {out:?}; stderr {err:?}; anchor {anchor} alive {}, strayed {strayed} alive {}, \
             stubborn {stubborn} alive {}. The script was:\n{script}",
            alive(anchor),
            alive(strayed),
            alive(stubborn),
            code = ran
                .status
                .code()
                .map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
            out = String::from_utf8_lossy(&ran.stdout),
            err = String::from_utf8_lossy(&ran.stderr),
        );

        for _ in 0..100 {
            if !alive(anchor) && !alive(strayed) && !alive(stubborn) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !alive(anchor),
            "the box's own anchor survived being stopped"
        );
        assert!(
            !alive(strayed),
            "the process that left the tmux tree is still running — which is the whole report"
        );
        assert!(
            !alive(stubborn),
            "the process that ignores TERM is still running: the sweep never escalated to KILL, so \
             a box stops with its build, its database or its editor still in it"
        );
    }

    /// The sweep looks for the box **once**, before it signals anything — and this is the guard on
    /// the thing that number can become.
    ///
    /// `$boxns` is `mnt:[4026533200]`: an inode number, live only while the namespace is. Killing
    /// every member is what frees it, and the kernel hands it straight to the next `unshare` — so a
    /// second scan for that number, after the pause `TERM` needs, asks "who holds this number now"
    /// and kills the answer. Measured on the two-scan form, with `bwrap` running beside it as any
    /// other test binary does: 2 sweeps in 20 matched a stranger, one of them a process created
    /// **1.89 seconds after the sweep began**.
    ///
    /// Asserted on the script's shape rather than by running it, for the same reason
    /// [`a_stop_never_sweeps_the_namespace_it_is_running_in`] is: a test that demonstrated the
    /// mis-kill would have to arrange a victim, and the victim on a real box is another box.
    #[test]
    fn the_sweep_reads_membership_once_while_the_box_is_still_alive() {
        let sweep = namespace_sweep();
        assert_eq!(
            sweep.matches("/proc/[0-9]*").count(),
            1,
            "the sweep walks every process more than once, and only the first walk happens while \
             the namespace is provably alive — a later one is a search for whoever inherited the \
             box's namespace number:\n{sweep}"
        );
        let scan = sweep
            .find("/proc/[0-9]*")
            .expect("the sweep no longer looks for the box's processes at all");
        let pause = sweep
            .find("sleep 2")
            .expect("the sweep no longer gives anything time to leave on its own");
        assert!(
            scan < pause,
            "membership is read after the pause, by which time the box's own processes are gone \
             and the number may name something else entirely:\n{sweep}"
        );
        assert!(
            sweep.contains("${member#*:}"),
            "the escalation stopped checking what it remembered about each process, so a pid \
             reused inside the pause is killed in place of the one that ignored TERM:\n{sweep}"
        );
    }

    /// The guard that makes the catastrophic case impossible rather than unlikely.
    ///
    /// Pids recycle. If the anchor's number were taken by an ordinary sandbox process, its mount
    /// namespace is **the sandbox's own** — and a sweep over that ends init, dockerd, the fleet
    /// agent and every other box, on a `stop` of one. The comparison against the killer's own
    /// namespace costs one `readlink` and is independent of every other check, which is why it is
    /// there as well as the stamp rather than instead of it.
    ///
    /// Deliberately checks that `$boxns` comes out **empty** rather than running the sweep: a test
    /// that ran it to prove the point would be a test that killed the test runner if it were wrong.
    /// Linux only: it reads this process's own start time out of `/proc/self/stat`, and the
    /// mechanism under test IS that file — a box's identity is `(generation, pid, starttime)`.
    /// There is nothing here to port to a Mac; the fleet a box lives in is Linux by construction.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_stop_never_sweeps_the_namespace_it_is_running_in() {
        let me = std::process::id();
        let (generation, start) = stamp_of(me);
        assert!(start > 0, "this process has no readable start time");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "{look}; printf '%s' \"$boxns\"",
                look = namespace_kill(me, &generation, start)
            ))
            .output()
            .expect("run the lookup");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "",
            "the stop was about to sweep its own mount namespace, which is the sandbox's — every \
             box, dockerd and init included"
        );
    }

    /// An anchor that cannot be verified decides nothing, in the same direction `place` refuses in.
    ///
    /// Every record written before the stamp existed has an empty generation, and a pid whose start
    /// time disagrees is a pid that was reused. Both mean "this number no longer names the box", and
    /// sweeping on either would end whatever holds the number now.
    #[test]
    fn an_anchor_that_does_not_check_out_is_not_swept() {
        let me = std::process::id();
        let (generation, start) = stamp_of(me);
        let boxns = |gen: &str, start: u64| -> String {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "{look}; printf '%s' \"$boxns\"",
                    look = namespace_kill(me, gen, start)
                ))
                .output()
                .expect("run the lookup");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        assert_eq!(boxns("", start), "", "a record with no stamp was believed");
        assert_eq!(
            boxns(&generation, start + 1),
            "",
            "a pid whose start time disagrees was believed, so a recycled pid is swept"
        );
        assert_eq!(
            boxns("some-other-boot", start),
            "",
            "an anchor from another boot of the sandbox was believed"
        );
    }

    /// **The first of the two guards is the one that has to work on its own** (FLEET-8).
    ///
    /// `[ -n "{gen}" ]` received a value `sh_quote` had already wrapped in single quotes, so it
    /// tested the literal two-character string `''` and was true for every record ever written. It
    /// looked correct in `an_anchor_that_does_not_check_out_is_not_swept` because the *second*
    /// guard, `[ "$boot" = '' ]`, is also false whenever the boot id is readable — which it is on
    /// every machine those tests run on. Two guards where one is inert and the other happens to
    /// cover for it is one guard, and the doc above it says neither is optional.
    ///
    /// So this runs the lookup on a machine where the boot id is **not** readable, which is the
    /// state that separates them: `cat` and `readlink` are resolved through `PATH`, so a planted
    /// `cat` makes `$boot` empty and a planted `readlink` gives the target a mount namespace
    /// different from the runner's — without which the third guard ("never sweep my own namespace")
    /// blanks the answer and hides the result either way.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_unstamped_anchor_is_refused_even_where_the_boot_id_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::testutil::tempdir();
        let bin = (dir.as_ref() as &std::path::Path).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // Empty for everything, and `boot_id` is the only thing the lookup `cat`s.
        std::fs::write(bin.join("cat"), "#!/bin/sh\nexit 0\n").unwrap();
        // Two different namespaces, so the self-namespace guard cannot be what refuses.
        std::fs::write(
            bin.join("readlink"),
            "#!/bin/sh\ncase \"$1\" in /proc/self/ns/mnt) echo 'mnt:[1]' ;; *) echo 'mnt:[2]' ;; \
             esac\n",
        )
        .unwrap();
        for stub in ["cat", "readlink"] {
            std::fs::set_permissions(bin.join(stub), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let me = std::process::id();
        let (_, start) = stamp_of(me);
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "{look}; printf '%s' \"$boxns\"",
                look = namespace_kill(me, "", start)
            ))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .expect("run the lookup");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "",
            "a record with no stamp was swept on a sandbox whose boot id cannot be read — the \
             empty-generation guard is testing the literal `''`"
        );
    }
}
