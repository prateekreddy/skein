//! How a crossing is spelled: the argv every way into a box or a sandbox builds, the proofs
//! `guard` runs in front of the hop, and `spawning`, the one gate between an argv and a process.

use super::*;

impl Place {
    /// The argv that runs `script` in this place.
    ///
    /// Its own function so the wire format is testable without a sandbox — and because it is the
    /// contract the takeover guard asserts.
    pub fn exec_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach();
        argv.extend(self.enter());
        argv.extend(self.shell());
        argv.push(self.wrap(script));
        argv
    }

    /// How to spell `tmux` for this box: bare when the sandbox is the box, socket-qualified when it
    /// is shared. A shell fragment, because every tmux call skein makes is already part of one.
    ///
    /// Session *names* stay the same in both shapes (`skein-agent`, `skein-agent-<runtime>`) — under
    /// the shared model the socket is what separates one box's sessions from another's. Two boxes
    /// with a `skein-agent` session are then unambiguous, where sharing a server would collide on
    /// the first name and silently attach a box to its neighbour's agent.
    ///
    /// Note there is no `nsenter` here: the socket lives outside the box's private mounts, so the
    /// server answers from the sandbox directly. Commands the *session* runs are inside the
    /// namespace regardless, because the server itself is.
    pub fn tmux(&self) -> String {
        match &self.at {
            Where::SandboxItself => "tmux".into(),
            Where::Shared { sock, .. } => format!("tmux -S {}", sh_quote(sock)),
        }
    }

    /// This box's tmux socket, empty when the sandbox is the box. For the few callers that need the
    /// bare path rather than the `tmux` spelling — the pane observer runs its own tmux commands.
    pub fn tmux_sock(&self) -> &str {
        match &self.at {
            Where::SandboxItself => "",
            Where::Shared { sock, .. } => sock,
        }
    }

    /// The two argv elements that put the rest of a crossing on a **fixed** PATH (ISO-1).
    ///
    /// One function because [`Self::enter`] and [`Self::shell`] both need it, and they are two
    /// halves of one property: everything skein runs at fleet scope resolves its programs from
    /// root-owned directories, whatever PATH the process that built the argv happened to inherit.
    /// They were written apart, only [`Self::shell`] had it, and [`Self::enter`] records what that
    /// cost.
    ///
    /// **`env` itself still resolves from the inherited PATH**, because argv[0] must. That is one
    /// program rather than five, it is the same one [`Self::shell`] has always been exposed to, and
    /// spelling it `/usr/bin/env` would trade a PATH lookup for a hard-coded location nothing else
    /// in this tree assumes. Said out loud rather than left for a reader to find.
    fn path_pin() -> Vec<String> {
        vec!["env".into(), format!("PATH={FLEET_PATH}")]
    }

    /// The `nsenter` hop that puts a command inside this box's namespace — empty when the sandbox
    /// is the box, which is what keeps the original model byte-for-byte unchanged.
    ///
    /// # Everything in front of the hop runs at fleet scope, so it runs on a fixed PATH (ISO-1)
    ///
    /// The outer `bash`, the `nsenter`, and the `cat`, `sed` and `cut` that [`Self::guard`] spends
    /// on `/proc/<ns_pid>/stat` all run **outside the box's namespace, before any hop** — at fleet
    /// scope, where `sudo` works and the fleet root is readable. Unpinned, all five resolved from
    /// whatever PATH the spawning process inherited, and `~/.local/bin` is bound read-**write** into
    /// every box, every one of them uid 1000 ([`Self::shell`] carries ISO-1's measurement). So a box
    /// that dropped a file called `nsenter` there had it run at fleet scope — or one called `cat`,
    /// which is worse, because the guard is the check that stops a crossing entering some *other*
    /// box, and a planted `cat` answers it. A file copy, not an exploit.
    ///
    /// **[`Self::shell`] pinned this for a fleet-scope script and this did not, and the difference
    /// was where the two were written rather than a decision** (SKEIN-832).
    ///
    /// # The PATH a crossing inherits is not skein's to trust — counted, not sampled
    ///
    /// The tempting argument for leaving this unpinned is that `skein-server` is exec'd by
    /// `src/server-doorway.py`, which copies `dict(os.environ)` through untouched
    /// (`src/server-doorway.py:212-220`), from a `tmux new-session` skein started at fleet scope:
    /// [`crate::fleet::start_server`] does go through `own_sandbox(..).exec(..)`, so that one is
    /// under `env PATH={FLEET_PATH}` (`src/fleet/server.rs:422-435`). A tmux session does take its
    /// environment from the **client** that asked for it, so that much survives contact — measured
    /// on tmux 3.6 here, a session created by a client holding a clean PATH got the clean one even
    /// though the tmux server had been started with a planted directory at its head.
    ///
    /// It is still one start path of several, and not the one that matters. Every process that
    /// spawns a crossing, and the PATH each carries:
    ///
    /// | what spawns the crossing | how it was started | the PATH it resolves `nsenter` from |
    /// |---|---|---|
    /// | `skein-server` | `fleet::start_server`, a `Place` at fleet scope (`src/fleet/server.rs:433`) | `FLEET_PATH` — the only pinned one |
    /// | `skein-server` | sbx's `commands.startup` runs `start-door.sh` at every sandbox start (`src/fleet-kit-spec.yaml:31`) | sbx's, for a uid-1000 `bash -c`. Not skein's to set |
    /// | `skein-server` | `bootstrap.sh:618` runs `start-door.sh`, having done `export PATH="$CARGO_HOME/bin:$PATH"` (`bootstrap.sh:348`) | a toolchain directory, then whatever ran `bootstrap.sh` |
    /// | `skein-server` | a person putting the door back: `sbx exec -i <sandbox> /boxes/.skein/start-door.sh` (`bootstrap.sh:478`) | that person's shell's |
    /// | `skein-server` | a developer: `./target/release/skein-server` (`README.md:162`) | that developer's shell's |
    /// | `skein` | **a person typing `skein attach <box>`** — `run_attach` spawns the crossing argv with `Command::new(program)` (`src/bin/skein.rs:1346`) | that person's shell's, `~/.local/bin` at its head |
    /// | `skein` | spawned by `skein-server`, which copies its whole environment in (`src/bin/skein-server.rs:4411-4413`) | the server's, whatever the rows above left it |
    ///
    /// `start-door.sh` pins nothing (`bootstrap.sh:556`), and the `SIGUSR1` reload re-execs across
    /// the same environment (`src/server-doorway.py:185-196`), so whatever PATH a fleet's first
    /// `start-door.sh` had is frozen into every `skein-server` after it, upgrades included.
    ///
    /// **The last two rows are why this is pinned rather than written down as safe.** `skein
    /// attach` is a documented command a person runs in their own terminal; there is no wording of
    /// "every start path has a trusted PATH" that is true while it exists. skein's own code says as
    /// much where it can see the consequence — [`crate::ai::Unread`] tells a reader that "the server
    /// inherits the PATH of whatever launched it" and to "start the server from a shell that has
    /// it" (`src/ai/unread.rs:85-90`). Pinning also makes the property local: it is one line here, not a
    /// claim about every start path anyone adds later, which is the list that goes stale.
    ///
    /// **Nothing is lost by pinning.** Every program a crossing runs before the hop — `env`,
    /// `bash`, `nsenter`, `cat`, `sed`, `cut` — is in `/usr/bin` on this substrate and so inside
    /// `FLEET_PATH`: `env PATH={FLEET_PATH} sh -c 'command -v …'` finds all six.
    ///
    /// **It also stops the spawner's PATH riding into the box**, finishing a job [`Self::wrap`]
    /// already does for `HOME`, `SKEIN_BOX` and the working directory — `nsenter` carries the
    /// caller's environment, not the box's.
    ///
    /// **What it must not do is leave `FLEET_PATH` standing as the box's PATH**, and for one commit
    /// it did. This pin was written believing the trailing `-lc` would rebuild PATH from the box's
    /// profile; there is no profile on this substrate to rebuild it from, so the box ran on
    /// `FLEET_PATH` — which has no `~/.local/bin` and therefore no agent CLI. [`Self::wrap`] sets
    /// the box's own PATH past the hop and carries the measurement. The two are one property: **in
    /// front of the hop, root-owned directories; past it, the box's.**
    ///
    /// Asserted by `tests/isolation_bwrap.rs::a_planted_nsenter_is_not_what_a_crossing_runs`.
    pub(super) fn enter(&self) -> Vec<String> {
        match &self.at {
            Where::SandboxItself => vec![],
            // A shell rather than a bare `nsenter`, because the check has to happen in the process
            // that crosses. `"$@"` carries whatever the caller appends through untouched, so this
            // stays an argv splice and nothing gets re-quoted on the way in.
            // An address that cannot be proved builds no `nsenter` at all, rather than one behind
            // a check. Nothing then has to hold for the refusal to hold.
            //
            // Pinned on this arm too, though its script spends no external program: without the
            // pin argv[0] is `bash`, and a refusal that runs a box's planted `bash` at fleet scope
            // in order to print itself has still run it.
            Where::Shared { .. } if !self.provable() => {
                let mut argv = Self::path_pin();
                argv.extend(["bash".to_string(), "-c".into(), self.guard(), "bash".into()]);
                argv
            }
            // The environment is cut to the launcher's list here, in the process that crosses, so
            // every builder that enters — exec, write, interactive and raw — gets it from one place
            // (SKEIN-1085; see [`keep_only_listed`]). `SKEIN_IN_BOX=1` is set on the same `exec`,
            // deliberately, for the reason the launcher sets it: `apiauth::off_switch_refused`
            // reads it, and a `skein-server` somebody starts from a crossing's shell is as much in
            // a box as one started from the session.
            //
            // The names the launcher decides per box, `GH_TOKEN` above all, are then taken from
            // the box's own session rather than from skein-server, so a crossing into a scoped box
            // carries that box's token or none, never the fleet's (SKEIN-1095; see
            // [`as_the_session_holds`]). After the guard, because it reads the anchor the guard
            // has just proved.
            Where::Shared { ns_pid, .. } => {
                let mut argv = Self::path_pin();
                argv.extend([
                    "bash".to_string(),
                    "-c".into(),
                    format!(
                        "{}{}{}exec env \"${{skein_odd[@]}}\" SKEIN_IN_BOX=1 {} -- \"$@\"",
                        self.guard(),
                        keep_only_listed(),
                        as_the_session_holds(*ns_pid),
                        self.nsenter()
                    ),
                    "bash".into(),
                ]);
                argv
            }
        }
    }

    /// How skein gets to the **sandbox**, before [`Self::enter`] gets it to the box: **it is
    /// already there, so this is nothing at all.**
    ///
    /// A crossing used to have two hops, and the first was `sbx exec [flags] <sandbox>` — a host
    /// reaching into a guest. Skein runs inside the fleet sandbox now (SKEIN-576), so the machine
    /// an address names is the machine this process is on, and `sbx` is host-only: not merely
    /// unnecessary here but absent.
    ///
    /// **Kept as a named empty rather than deleted at the four call sites.** "Which hops does a
    /// crossing have" is exactly the sort of question that gets answered differently in one builder
    /// after somebody changes the other three, and the answer wants somewhere to live. It took a
    /// `flags` argument while there was a hop to give flags to; passing `-i` to nothing was the
    /// kind of argument that reads as load-bearing and is not, so it went with the hop.
    pub(super) fn reach(&self) -> Vec<String> {
        Vec::new()
    }

    /// **A sandbox that is not the one this process is standing in cannot be reached at all** — so
    /// an address for one is refused rather than aimed.
    ///
    /// That is the whole invariant, and it is about *which sandbox*, not about what kind of box
    /// once lived in it. Found by writing [`Self::reach`] rather than by planning. A crossing used
    /// to have two hops: `sbx exec` to the sandbox, then `nsenter` to the box inside it. The first
    /// is gone, correctly — skein is already in the sandbox, and `sbx` is host-only, so it is
    /// not merely unnecessary but unavailable. [`Where::SandboxItself`] has no second hop either:
    /// `enter()` is empty, because the address is the sandbox and there is no box in it to enter.
    /// Drop both and the command does not fail — it *runs*, in whatever sandbox skein happens to be
    /// standing in, with the same paths on it and other people's files at them.
    ///
    /// So it refuses, in-band, the way [`crate::sandbox::refusal_argv`] does — the caller is
    /// usually a terminal, and an argv that prints why is read where an `Err` several layers up is
    /// not.
    ///
    /// # The sandbox we ARE standing in is the case this exists to let through
    ///
    /// That is not a corner case, it is most of [`crate::fleet`]: `ensure_substrate`,
    /// `ensure_fleet_root`, `install_launcher` and `install_docker_config` all address the fleet's
    /// own sandbox through [`own_sandbox`], which is how a fleet provisions itself. Refusing there
    /// refused skein's own setup: every box start in-fleet printed this message instead of doing
    /// the work, and the fleet could not provision itself at all.
    ///
    /// Having no hops at all is exactly right there. Nothing to the sandbox because skein is
    /// already inside it, and no `nsenter` because the address is a sandbox rather than a box. The
    /// command runs on the machine it was addressed to, which is the whole test.
    ///
    /// An unnamed fleet matches nothing, so it still refuses — a sandbox this build cannot identify
    /// as its own is one it has no business assuming it is standing in.
    pub(super) fn unreachable_from_fleet(&self) -> Option<Vec<String>> {
        // Nothing to enter: this address names a sandbox, so `enter()` adds no second hop and
        // `reach()`'s first hop is the only one there was.
        let no_hop_inside = matches!(self.at, Where::SandboxItself);
        // The sandbox this process is standing in — the one address that needs no hop at all.
        let ours = fleet_sandbox();
        let the_one_we_are_in = !ours.is_empty() && self.sandbox == ours;
        (no_hop_inside && !the_one_we_are_in).then(|| {
            vec![
                "sh".to_string(),
                "-c".into(),
                format!(
                    "echo 'skein: {sandbox} is not the sandbox this skein is running inside, and \
                     sbx is host-only — there is no sbx here to reach another sandbox with. Move \
                     its work into this fleet.' >&2; exit 1",
                    sandbox = self.sandbox
                ),
            ]
        })
    }

    /// The `nsenter` invocation itself, without the guard in front of it.
    fn nsenter(&self) -> String {
        match &self.at {
            Where::SandboxItself => String::new(),
            Where::Shared { ns_pid, .. } => format!(
                "nsenter --user=/proc/{ns_pid}/ns/user --mount=/proc/{ns_pid}/ns/mnt \
                 --preserve-credentials"
            ),
        }
    }

    /// Does this address carry the two halves that make it checkable?
    ///
    /// False for a record written before the stamp existed. Not "unknown" — unprovable, which is
    /// treated exactly as a mismatch is.
    fn provable(&self) -> bool {
        match &self.at {
            Where::SandboxItself => true,
            Where::Shared {
                generation,
                ns_start,
                ..
            } => !generation.is_empty() && *ns_start > 0,
        }
    }

    /// Refuse the crossing unless the anchor is still the process skein recorded.
    ///
    /// **Why it is here and not where the address is looked up.** A pid is a name that can be
    /// reused, so any check with a gap after it is a check on a different question than the one the
    /// crossing asks. This runs in the shell that is about to `exec nsenter`, one line before it —
    /// the smallest gap available without a kernel handle.
    ///
    /// **Why a refusal and never a fallback.** A pid that no longer names what skein recorded names
    /// something *else in the same sandbox*, and everything else in that sandbox is another box. So
    /// there is no degraded mode to fall back to: "enter this instead" is the vulnerability, not the
    /// recovery from it.
    ///
    /// An address recorded before the stamp existed cannot be checked, so it is refused too. The
    /// alternative is a fleet where the guard is present and silently does nothing for every box
    /// that has not been restarted, which is worse than one that says so.
    fn guard(&self) -> String {
        let Where::Shared {
            ns_pid,
            generation,
            ns_start,
            ..
        } = &self.at
        else {
            return String::new();
        };
        let name = &self.name;
        if generation.is_empty() || *ns_start == 0 {
            return format!(
                "echo \"skein: {name} was placed before skein checked anchors, so its address \
                 cannot be proved to be its own; restart it with: skein restart {name}\" >&2; \
                 exit 78\n"
            );
        }
        // Cut after the LAST `) ` rather than taking whitespace field 22: `comm` is the process's
        // own name in parentheses and may contain spaces and parentheses of its own, so `$22` is
        // right until something is called an awkward name and then it is silently off.
        // **Two facts, two answers.** These were one branch and one sentence — "{name} is gone —
        // pid N is no longer the session skein recorded" — which names neither of the things it
        // just measured. A person who reads that about the box they were working in cannot tell
        // whether they lost one box or the whole sandbox, and those want opposite reactions:
        //
        //   * the boot id differs -> the SANDBOX restarted. Nothing that was running in it
        //     survived, every box is in this same state, and the fleet needs starting again. That
        //     is a fleet-wide fact arriving one box at a time.
        //   * the start time differs -> that one pid was reused by another process. About this box
        //     and nothing else.
        //
        // `fleet::anchor_matches` already separates them on the other path into a box, so this was
        // the odd one out — with both values in hand at the moment it decided.
        format!(
            "skein_gen=\"$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)\"\n\
             skein_start=\"$(sed -n 's/.*) //p' /proc/{ns_pid}/stat 2>/dev/null | cut -d' ' -f20)\"\n\
             if [ \"$skein_gen\" != {gen_q} ]; then\n\
             \x20 echo \"skein: the sandbox has restarted since {name} was placed, so {name} is \
             gone and so is everything else that was running in it — start them again with: skein \
             start <box>\" >&2\n\
             \x20 exit 78\n\
             fi\n\
             if [ \"$skein_start\" != {start_q} ]; then\n\
             \x20 echo \"skein: {name} is gone — pid {ns_pid} has been reused by another process \
             since skein recorded it, so entering it would be entering some other box; restart it \
             with: skein restart {name}\" >&2\n\
             \x20 exit 78\n\
             fi\n",
            gen_q = sh_quote(generation),
            start_q = sh_quote(&ns_start.to_string()),
        )
    }

    /// Put the script where it expects to be: at the repo root, with the box's own HOME **and the
    /// box's own PATH**.
    ///
    /// `nsenter` carries the *caller's* environment and working directory into the namespace, so
    /// neither is inherited from the box. A script that assumed it started at the tree root would
    /// otherwise run somewhere arbitrary, and one reading `~/.config/sync/env` would read skein's.
    ///
    /// # PATH is the third member of that family, and it was the one this forgot (SKEIN-832)
    ///
    /// The same sentence covers it: `nsenter` carries the caller's environment, so the PATH inside
    /// the box was never the box's either. That was invisible for as long as the value being
    /// carried happened to *contain* `~/.local/bin` — a person's login PATH does, and so does the
    /// PATH a doorway-started `skein-server` inherits. [`Self::path_pin`] changed which wrong value
    /// crosses, from the caller's to [`FLEET_PATH`], and [`FLEET_PATH`] contains no `~/.local/bin`
    /// at all. That is where `claude` lives, so `tests/fleet_launch.rs` caught it at once: a box
    /// answered `command -v claude` with `/usr/local/bin/claude` — **the substrate's copy, not the
    /// one the fleet installs and shares** — and a sandbox without one would have answered nothing.
    ///
    /// **The tempting fix is to carry the caller's PATH across the hop instead of the pin, and it
    /// is wrong**: it makes what a box resolves depend on who spawned the crossing.
    /// [`crate::fleet::start_server`] starts the server through `own_sandbox(..).exec(..)`
    /// (`src/fleet/server.rs:433`), which is itself under `env PATH={FLEET_PATH}` — so on that start path
    /// the caller's PATH *is* `FLEET_PATH`, and carrying it would reproduce this same bug while
    /// looking like a fix. The box's PATH has to be derived from the box.
    ///
    /// **And `-lc` does not rescue it**, which is the assumption [`Self::shell`] used to record.
    /// Measured on this substrate rather than assumed: there is no `~/.profile` in a box's private
    /// home and none in the sandbox's home either, and `/etc/profile` here touches PATH nowhere —
    /// `env PATH={FLEET_PATH} bash -lc 'echo $PATH'` prints `FLEET_PATH` back unchanged, with
    /// `command -v cargo` empty. A login shell rebuilds nothing; the PATH a box runs on is the one
    /// it was handed.
    ///
    /// So it is handed the box's own: `$HOME/.local/bin`, [`BOX_PATH_HEAD`], then [`FLEET_PATH`] —
    /// written from the `home` in the placement rather than from `$HOME` in the shell, so it cannot
    /// depend on the order two words of one `export` are expanded in.
    ///
    /// **This is the far side of the hop and only the far side.** Everything in front of the hop
    /// still runs on [`FLEET_PATH`] ([`Self::enter`]), and so does [`Self::raw_argv`], which builds
    /// no shell and so never reaches here: its one production caller is
    /// [`crate::takeover::copy_guest_file`] running `cat` (`src/takeover.rs:128`, the only one of
    /// ten `raw_argv` mentions outside this file's own tests), which `FLEET_PATH` resolves — and
    /// resolving skein's own utilities from root-owned directories even inside a box is the
    /// stronger answer, not the weaker one.
    pub(super) fn wrap(&self, script: &str) -> String {
        match &self.at {
            Where::SandboxItself => script.to_string(),
            // SKEIN_BOX as well as HOME, because entering the namespace is not the same as being
            // launched into it. `box-session.sh` exports the identity for the session it starts, but
            // a later `nsenter` gets a fresh environment — so anything skein runs through a
            // placement had only `SANDBOX_VM_ID` to go on, which names the SANDBOX and is the same
            // string for every box in it.
            //
            // Measured: every fleet box's screen observer wrote `skein-fleet.pane.json` into its own
            // repo's store, so no box had a fresh screen observation and the board said "screen
            // lost" for all of them — while each box's *hooks*, which inherit from the agent process
            // that `box-session.sh` did launch, were filing correctly under the box's own name.
            Where::Shared { home, tree, .. } => format!(
                "export HOME={} SKEIN_BOX={} PATH={} && cd {} && {script}",
                sh_quote(home),
                sh_quote(&self.name),
                sh_quote(&box_path(home)),
                sh_quote(tree)
            ),
        }
    }

    /// The shell a script runs under, and **which one depends on whose machine it is**.
    ///
    /// **Fleet scope gets `-c` under a fixed PATH, never `-lc`** (ISO-1). A script addressed to the
    /// sandbox itself runs OUTSIDE every box's mount namespace, where `sudo` works and the whole of
    /// the fleet root is there to read. A login shell sources the profile, and on this substrate
    /// that profile puts `~/.local/bin` and `/usr/local/share/npm-global/bin` at the head of PATH —
    /// both owned by uid 1000, which is what every box runs as, and `.local` is bound read-WRITE
    /// into every box so that eleven of them share one toolchain. So a box that dropped a `sudo`, a
    /// `tmux` or a `python3` into `~/.local/bin` had it run at fleet scope with the real one behind
    /// it: a file copy, not an exploit.
    ///
    /// **This property used to live in the in-sandbox agent**, which built its own argv and was the
    /// only fleet-scope path that closed the hole; the spawned path beside it still used `-lc`. The
    /// agent is deleted (SKEIN-573) and this is the surviving path, so the property moved here
    /// rather than going with it — which is the whole of what "delete the transport, keep what it
    /// was carrying" has to mean.
    ///
    /// **A crossing into a box still ends in `-lc`**, and that is not an oversight: `enter()` has
    /// already put it inside the box's namespace, where a box's own profile is the box's own
    /// business.
    ///
    /// **What `-lc` is NOT is a way to get a PATH.** It was read as one, and the reading was never
    /// measured: on this substrate a box has no `~/.profile` — nor does the sandbox's home — and
    /// `/etc/profile` sets no PATH, so `env PATH={FLEET_PATH} bash -lc 'echo $PATH'` prints
    /// `FLEET_PATH` straight back. The box's PATH is whatever it was handed, which is why
    /// [`Self::wrap`] hands it one (SKEIN-832).
    ///
    /// **What WAS an oversight is the part of a crossing that runs before that hop**, and for a
    /// long time this was the only arm that pinned anything. [`Self::enter`] pins it now, through
    /// the same [`Self::path_pin`] — the two are one property with two halves, and they are written
    /// as one function so they cannot drift (SKEIN-832).
    ///
    /// Nothing skein sends at fleet scope wants the sandbox user's profile — the scripts name what
    /// they need, and the one that builds skein exports its own `CARGO_HOME`/PATH
    /// (`bootstrap.sh`).
    pub(super) fn shell(&self) -> Vec<String> {
        match &self.at {
            Where::SandboxItself => {
                let mut argv = Self::path_pin();
                argv.extend(["bash".to_string(), "-c".into()]);
                argv
            }
            // No fleet pin here, and none wanted: [`Self::enter`] has already put one in front of
            // this for the arm that runs anything at fleet scope, and a second one would be saying
            // something about a machine the first has already left. Past the hop the PATH is the
            // box's own — set by [`Self::wrap`] in the script this shell runs, because nothing else
            // out here sets it and `-l` does not.
            Where::Shared { .. } => vec!["bash".into(), "-lc".into()],
        }
    }

    /// The **whole** argv for an interactive attach — a terminal, not a captured command.
    ///
    /// This used to return everything *after* the program name, because both callers handed a
    /// literal `"sbx"` to a PTY spawner. That was kept deliberately, on the grounds that changing it
    /// would touch the terminal plumbing on both ends "for no behavioural gain". The gain arrived
    /// with the hop's removal: the program is not `sbx` at all, and a builder that returns arguments
    /// for a program it does not name cannot say so.
    ///
    /// The whole attach runs inside the namespace, not just the tmux call. The shell it carries
    /// refreshes the runtime's instruction file, runs the runtime's setup and starts the pane
    /// observer — all of which read and write the box's own HOME and tree. Outside the hop they
    /// would quietly operate on skein's.
    pub fn interactive_argv(&self, script: &str) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach();
        argv.extend(self.enter());
        argv.extend(self.shell());
        argv.push(self.wrap(script));
        argv
    }

    /// The argv for running a command here *without* a shell — `["cat", path]` and friends.
    ///
    /// For callers that stream stdout somewhere other than a buffer, so they keep their own
    /// plumbing while the sandbox name still resolves through here rather than being assumed.
    pub fn raw_argv(&self, args: &[&str]) -> Vec<String> {
        if let Some(refusal) = self.unreachable_from_fleet() {
            return refusal;
        }
        let mut argv = self.reach();
        argv.extend(self.enter());
        argv.extend(args.iter().map(|a| a.to_string()));
        argv
    }

    /// The argv a fleet-scope command is about to be spawned with: the substitution applied, and
    /// **a test process that installed none refused rather than run for real**.
    ///
    /// [`seam`]'s own note already says what happens without this — "every fleet-scope script now
    /// runs straight at this machine, and a fixture that forgets this seam reaches the real one by
    /// default". [`Place::reach`] is empty and [`Where::SandboxItself`] adds no `nsenter`, so for
    /// the addresses most of [`crate::fleet`] uses the argv here is a shell command on **this**
    /// machine, at fleet scope, beside every real box on it. Forgetting the seam is invisible:
    /// nothing fails, the command runs, and what it touched is only discoverable afterwards by
    /// looking. That is the shape SKEIN-530 is the general version of — five tests installed
    /// uncommitted code onto the owner's live fleet, and the run was green.
    ///
    /// So the omission is made loud instead. In production [`crate::util::in_test`] is false and
    /// this is one predicate on an environment variable; in a test it is the difference between a
    /// panic naming the seam and a command nobody meant to run.
    ///
    /// **A test that means it says so** — [`seam::real_crossings`], which `skein`'s and
    /// `skein-server`'s `main` and `tests/fleet_launch.rs` call, because a spawned skein cannot be
    /// handed a closure and an end-to-end suite's subject is the real command. That is a declared
    /// exemption in the shape of `tests/platform_gates.rs`'s `GATED`: it costs a line in the diff,
    /// where the omission it replaces cost nothing and said nothing.
    ///
    /// # The gap this used to have, and the one it still has
    ///
    /// This is a checkpoint on the argv, not on the builder, and for a while only [`Self::command`]
    /// and [`Self::write`] passed through it — the two paths where `Place` spawns the process
    /// itself. [`Self::exec_argv`], [`Self::raw_argv`] and [`Self::interactive_argv`] *return* an
    /// argv the caller spawns, so a caller that took one of those and spawned it was outside both
    /// halves of the seam: no substitution, and no refusal. Three such callers were in this crate,
    /// and they now call this before they spawn (SKEIN-764):
    ///
    /// | caller | builder | how it spawns |
    /// |---|---|---|
    /// | [`crate::takeover::copy_guest_file`] | `raw_argv` | `Command::new` with the guest file streamed to a host file |
    /// | `sandbox::resume_box` | `exec_argv` | quoted into a bigger shell string, run as `sh -c` |
    /// | `sandbox::restart_agent_session` | `exec_argv` | `util::run_capture` |
    ///
    /// None of the three can use [`Self::command`] instead — one redirects stdout to a file, one
    /// needs the argv as *text* inside another command, one wants the capture helper — which is
    /// why the seam is a checkpoint they call rather than a wrapper they go through.
    ///
    /// **The remaining callers are in `src/bin/`, and a checkpoint cannot help them.** `shell_argv`,
    /// `attach_argv_as`, `initial_attach_argv_as` and `box_write_argv` hand their argv across the
    /// crate boundary to `skein.rs` and `skein-server.rs`, which spawn it — and both of those
    /// `main`s open with [`seam::real_crossings`] (`src/bin/skein.rs:22`,
    /// `src/bin/skein-server.rs:111`), so the refusal is declared away before the argv is built.
    /// That is not an oversight to close: a spawned skein cannot be handed a closure, which is the
    /// whole reason the exemption exists.
    ///
    /// **And the checkpoint deliberately does not move up into those builders**, which is where it
    /// would have to go to cover them. Building an argv touches nothing, and most of what calls a
    /// builder never spawns what it gets. Count them rather than take that on trust:
    ///
    /// ```sh
    /// grep -rnE '(exec|raw|interactive|write)_argv\(' src/ tests/ | grep -vE 'fn |///'
    /// ```
    ///
    /// Twenty-nine lines, and every one is one of four things. Five are production spawns and all
    /// five now pass through here ([`Self::command`], [`Self::write`], and the three in the table
    /// above). Four are the builders whose argv leaves the crate for `src/bin/` and the
    /// `skein-server` line that spawns one — `box_write_argv`, `agent_attach_argv`,
    /// `shell_argv`. Two are tests whose subject IS the
    /// crossing, `tests/fleet_launch.rs`'s shape in miniature: this module's
    /// `a_crossing_in_the_fleet_enters_the_box_without_sbx` runs its argv into a bwrap namespace it
    /// built itself, and `tests/isolation_bwrap.rs`'s
    /// `a_planted_binary_is_not_what_a_fleet_scope_script_runs` runs it with a `$HOME` and `$PATH`
    /// of its own — neither can be stood in for without deleting what it proves.
    ///
    /// **The remaining eighteen assert the wire format and spawn nothing**, and
    /// refusing those would be [`crate::warden_client::Warden::configured`]'s mistake exactly:
    /// guarding the address rather than the connection, so that asking *what skein would run*
    /// costs a fixture it does not need.
    pub(crate) fn spawning(&self, argv: Vec<String>) -> Vec<String> {
        if let Some(instead) = seam::taken(&argv) {
            return instead;
        }
        // **A refusal is not a crossing.** For an address that cannot be reached from here the argv
        // builders short-circuit to [`Self::unreachable_from_fleet`]'s in-band
        // `echo …; exit 1` — a message and an exit code, touching nothing — and it is deliberately
        // spawned rather than returned as an `Err` so a terminal shows it. Refusing that would fail
        // the tests that assert skein *declines* to reach another sandbox, which is the opposite of
        // what this guard is for.
        if self.unreachable_from_fleet().is_some() {
            return argv;
        }
        assert!(
            !crate::util::in_test() || seam::installed() || seam::meant(),
            "a fleet-scope command is about to run FOR REAL in a test process \
             (${marker}), because no stand-in is installed. `Place::reach` is empty, so this \
             runs on the machine skein is standing on — the owner's live fleet on any machine \
             running skein, where five tests once installed uncommitted code (SKEIN-530). \
             Install one first:\n    \
             let _seam = skein::place::seam::install(Box::new(|argv| Some(vec![..])));\n\
             A substitution that returns `None` for an argv still lets it run, which is how a \
             fixture says it meant that one. If the real command IS the subject — an end-to-end \
             suite against a fixture fleet — say so instead:\n    \
             let _real = skein::place::seam::real_crossings();\n\
             The argv this would have spawned: {argv:?}",
            marker = crate::util::TEST_MARKER,
        );
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-fleet, the sandbox skein is STANDING IN is reached by running the command; any other
    /// sandbox is refused.
    ///
    /// That distinction is the whole of [`Place::unreachable_from_fleet`], and both arms are
    /// asserted in one test because a version that simply ran everything locally would pass the
    /// first and be exactly the bug the refusal was written to prevent — a command aimed at another
    /// sandbox, executed against this one's files at the same paths.
    ///
    /// The refusal caught the fleet's own sandbox once, and that is the reason for the first arm:
    /// most of [`crate::fleet`] addresses it through [`own_sandbox`] (`ensure_substrate`,
    /// `ensure_fleet_root`, `install_launcher`, `install_docker_config`), so every box start
    /// in-fleet printed
    ///
    /// ```text
    /// skein: skein-fleet is not the sandbox this skein is running inside …
    /// ```
    ///
    /// and provisioned nothing.
    #[test]
    fn in_fleet_runs_in_the_sandbox_it_stands_in_and_refuses_every_other() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            r#"{"fleet_sandbox":"skein-fleet"}"#,
        )
        .unwrap();

        let ours = Place {
            name: "skein-fleet".into(),
            sandbox: "skein-fleet".into(),
            at: Where::SandboxItself,
        };
        assert_eq!(
            ours.exec_argv("echo hi"),
            [
                "env",
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "bash",
                "-c",
                "echo hi"
            ],
            "skein refused to run a command in the sandbox it is standing in, which is where its \
             own substrate, launcher and fleet root are installed"
        );

        // The case the refusal was written for: any OTHER sandbox — a second fleet, someone's own
        // sbx box, or one of skein's original per-box VMs — really is a different machine, and
        // there is no sbx in here to reach it with.
        let elsewhere = Place {
            name: "web-main".into(),
            sandbox: "another-fleet".into(),
            at: Where::SandboxItself,
        };
        let argv = elsewhere.exec_argv("echo hi");
        assert_eq!(argv.first().map(String::as_str), Some("sh"), "{argv:?}");
        assert!(
            argv.iter()
                .any(|a| a.contains("is not the sandbox this skein is running inside")),
            "a sandbox other than this one is now addressed rather than refused, so the command \
             runs in the sandbox skein is standing in — a different machine with the same paths \
             and other people's files at them: {argv:?}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    // The argv IS the contract. The fleet's own sandbox is the address skein provisions itself
    // through, and pinning all three spellings here is what makes a change to the crossing
    // reviewable in one place rather than as a diff across a dozen files.
    //
    // It used to open with `sbx exec skein-fleet` — the host's hop into the guest — and every
    // spelling below carried it. Skein runs inside that sandbox now (SKEIN-576), so there is no
    // hop and the argv starts at the shell. What is pinned is what was always the interesting
    // half: **`env PATH=… bash -c`, never `bash -lc`**. This address is fleet scope, outside every
    // box's mount namespace, and a login shell would source a profile that puts the box-writable
    // `~/.local/bin` at the head of PATH (ISO-1 — see `Place::shell`, and `tests/isolation_bwrap.rs`,
    // which runs a planted binary against it). That property did not depend on the hop, and it is
    // the one somebody could lose without noticing.
    #[test]
    fn a_whole_sandbox_is_addressed_by_the_shell_alone() {
        // `exec_argv` reads the config (via `unreachable_from_fleet`, which asks which sandbox this
        // process is standing in), and the neighbours that write `$SKEIN_HOME` do it under the
        // shared lock — reading it without that lock is how this test flaked.
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let p = Place {
            name: "skein-fleet".into(),
            sandbox: "skein-fleet".into(),
            at: Where::SandboxItself,
        };
        assert_eq!(
            p.exec_argv("echo hi"),
            [
                "env",
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "bash",
                "-c",
                "echo hi"
            ],
            "no hop, no nsenter, no wrapper — and no login shell"
        );
        // A write is the same argv: the body arrives on the stdin of the process `Place::write`
        // spawns, rather than through an `-i` on a hop that no longer exists.
        assert_eq!(
            p.write_argv("cat > f"),
            [
                "env",
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "bash",
                "-c",
                "cat > f"
            ]
        );
        assert_eq!(
            p.raw_argv(&["cat", "/tmp/x"]),
            ["cat", "/tmp/x"],
            "no shell for a streamed copy — the path is an argv element, not a word to split"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // The three details that are easy to get wrong and all look like permissions bugs: the user
    // and mount namespaces must be joined TOGETHER (mount alone is refused), credentials must be
    // preserved (or setgroups fails unprivileged), and HOME/cwd/SKEIN_BOX must be set explicitly
    // because nsenter carries the caller's environment, not the box's.
    fn shared(generation: &str, ns_start: u64) -> Place {
        Place {
            name: "web-main".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: 4242,
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
                generation: generation.into(),
                ns_start,
            },
        }
    }

    #[test]
    fn a_shared_sandbox_is_entered_by_namespace_with_the_boxs_own_home() {
        // `exec_argv` reads the config (via `unreachable_from_fleet`), so it resolves
        // `config::skein_home`, which refuses an unpinned test rather than answering with the real
        // `~/.skein` (SKEIN-626). This one passed only because a neighbour in the same process had
        // left `$SKEIN_HOME` set — including the lock, without which reading it flaked (SKEIN-646).
        let _g = crate::testutil::env_lock();
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let p = shared("boot-a", 900);
        let argv = p.exec_argv("git status");
        // **The crossing starts at the namespace, with nothing in front of it.** This used to open
        // with `sbx exec skein-fleet`, the host's hop into the guest; skein is inside that guest
        // now (SKEIN-576). Asserted as an absence rather than by index alone, because an index
        // that shifted back would still pass a length check while the hop was back.
        assert!(
            !argv.iter().any(|a| a == "sbx"),
            "a hop into the sandbox came back, and there is no sbx here to run it: {argv:?}"
        );
        // **The PATH is pinned before anything runs** (ISO-1, SKEIN-832). The outer `bash`, the
        // `nsenter` and the guard's `cat`/`sed`/`cut` all run at fleet scope, before the hop, and
        // unpinned they resolved from the PATH of whoever spawned this — a person's shell, for
        // `skein attach`. Asserted by value rather than by presence: an `env` with some other
        // PATH in it would satisfy a `contains("env")`.
        assert_eq!(&argv[..2], ["env", &format!("PATH={FLEET_PATH}")]);
        // A shell, because the anchor check has to run in the process that crosses. The caller's
        // argv rides in as `"$@"`, so nothing between here and `nsenter` re-quotes it.
        assert_eq!(&argv[2..4], ["bash", "-c"]);
        // The far side of the hop, by value: the box's HOME, its name, **its PATH**, and its tree.
        // The PATH's directories are spelled out rather than built from the constants, so that
        // changing either constant has to be a deliberate edit to a string a reader can compare
        // against a real box's environment — which is where it came from (see `BOX_PATH_HEAD`).
        //
        // The box's home is the one part held in a variable, and only because `residue-check` reads
        // the literal that would otherwise appear here as somebody's home directory. The assertion
        // is unchanged by that: `box_home` is a constant of this test, not a value from the code
        // under test.
        let box_home = "/boxes/web-main/home";
        assert_eq!(
            argv.last().unwrap(),
            &format!(
                "export HOME='{box_home}' SKEIN_BOX='web-main' PATH='{box_home}/.local/bin:\
                 /usr/local/share/npm-global/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:\
                 /usr/bin:/sbin:/bin' && cd '/boxes/web-main/tree' && git status"
            )
        );
        // Stated as a property as well as a value, because the value above passes for a PATH that
        // merely CONTAINS the box's bin directory somewhere behind `/usr/local/bin` — which is the
        // failure this is here to stop. `~/.local/bin` must come first, and it must be THIS box's.
        let wrapped = argv.last().unwrap();
        let path = wrapped
            .split("PATH='")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .expect("the wrapper exports a PATH");
        assert!(
            path.starts_with(&format!("{box_home}/.local/bin:")),
            "the box's own `~/.local/bin` leads its PATH, or `claude` resolves to the substrate's \
             copy instead of the fleet's: {path}"
        );
        assert!(
            path.ends_with(FLEET_PATH),
            "and the root-owned directories are still behind it: {path}"
        );
        assert_eq!(&argv[5..8], ["bash", "bash", "-lc"]);

        let crossing = &argv[4];
        // The order is the property: refuse, THEN cross. Reversed, the check is a log line.
        let checked = crossing
            .find("skein_start=")
            .expect("the anchor is re-read");
        let entered = crossing
            .find("SKEIN_IN_BOX=1 nsenter")
            .expect("and then entered, marked as in a box");
        assert!(
            checked < entered,
            "the check must precede the crossing: {crossing}"
        );
        // And the environment is cut to the launcher's list before the hop, not after it: behind
        // the hop a login shell has sourced the box's own profile, which is the box's business.
        let cut = crossing
            .find("unset -v")
            .expect("the crossing keeps only the launcher's list (SKEIN-1085)");
        assert!(
            cut < entered,
            "the filter must precede the crossing: {crossing}"
        );
        assert!(
            crossing.contains("!= 'boot-a'") && crossing.contains("!= '900'"),
            "both halves of the identity are compared: {crossing}"
        );
        assert!(
            crossing.contains("--user=/proc/4242/ns/user")
                && crossing.contains("--mount=/proc/4242/ns/mnt")
                && crossing.contains("--preserve-credentials"),
            "both namespaces together, credentials preserved: {crossing}"
        );

        // The stdin path crosses the same way. It used to carry an `-i` in front of the sandbox so
        // `sbx exec` would wire a pipe; the pipe is `Place::write`'s own now, and what still has to
        // be true is that the body lands inside the box's namespace rather than the sandbox's.
        let w = p.write_argv("cat > f");
        assert_eq!(
            &w[..4],
            ["env", &format!("PATH={FLEET_PATH}"), "bash", "-c"]
        );
        assert!(w.iter().any(|a| a.contains("--preserve-credentials")));
        // And a streamed copy enters the namespace too, or it would `cat` the wrong /tmp entirely.
        let raw = p.raw_argv(&["cat", "/tmp/artifact"]);
        assert_eq!(&raw[raw.len() - 2..], ["cat", "/tmp/artifact"]);
        assert!(raw.iter().any(|a| a.contains("SKEIN_IN_BOX=1 nsenter")));
        std::env::remove_var("SKEIN_HOME");
    }

    /// An address skein cannot prove is refused, on every transport, rather than used.
    ///
    /// A record written before the stamp existed is the upgrade case, and the tempting thing is to
    /// let it through "just this once" — which produces a fleet where the guard is present and does
    /// nothing for every box nobody has restarted. The refusal names the fix instead.
    #[test]
    fn an_address_that_cannot_be_proved_is_refused_rather_than_entered() {
        // Pinned for the same reason as the test above: `exec_argv` resolves `config::skein_home`,
        // and unpinned that is the owner's real `~/.skein` (SKEIN-626/646).
        let _g = crate::testutil::env_lock();
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let p = shared("", 0);
        // Index 4, because the crossing is the third element of `bash -c <crossing> bash …` and
        // that shell is behind the PATH pin `enter` puts in front of every crossing (SKEIN-832).
        // There is still no hop in front of THAT (SKEIN-576) — the pin is two argv elements, not a
        // machine boundary.
        let argv = p.exec_argv("git status");
        assert_eq!(&argv[..2], ["env", &format!("PATH={FLEET_PATH}")]);
        let crossing = argv[4].clone();
        assert!(
            crossing.contains("exit 78") && !crossing.contains("nsenter --user"),
            "an unprovable address must not reach nsenter at all: {crossing}"
        );
        assert!(
            crossing.contains("skein restart web-main"),
            "and it names the fix: {crossing}"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    // A box's tmux server is addressed by socket, never by nsenter — the socket sits outside the
    // private mounts precisely so liveness and attach work from the sandbox. A whole sandbox
    // addressed as itself has no box in it and so no per-box socket: the spelling is the bare
    // `tmux` the sandbox's own server answers on, which is what the fleet's supervisor uses.
    #[test]
    fn a_shared_box_tmux_server_is_addressed_by_its_own_socket() {
        let whole = Place {
            name: "skein-fleet".into(),
            sandbox: "skein-fleet".into(),
            at: Where::SandboxItself,
        };
        assert_eq!(whole.tmux(), "tmux");

        let shared = Place {
            name: "web-main".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: 4242,
                home: "/boxes/web-main/home".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/boxes/web-main/session.sock".into(),
                generation: "boot-a".into(),
                ns_start: 900,
            },
        };
        assert_eq!(shared.tmux(), "tmux -S '/boxes/web-main/session.sock'");
        assert!(
            !shared.tmux().contains("nsenter"),
            "the server answers from the sandbox; entering its namespace to talk to it would be \
             both unnecessary and wrong — the socket does not exist inside the private /tmp"
        );
    }

    /// The refusal to enter says WHICH of the two things it measured went wrong.
    ///
    /// Reported live, about the box its owner was working in — its name stood in for here:
    ///
    /// ```text
    /// skein: example-work is gone — pid 625094 is no longer the session skein recorded,
    /// so entering it would be entering some other box
    /// ```
    ///
    /// The refusal itself is correct and is the whole point of the anchor — a stale pid is ANOTHER
    /// box, and a wrong address is not a degraded address. But the guard checks two facts and that
    /// sentence named neither. A boot id that has moved means **the sandbox restarted**: nothing
    /// that was running in it survived, every box is in the same state, and it is a fleet-wide fact
    /// arriving one box at a time. A start time that has moved means **one pid was reused**, about
    /// that box alone. A person cannot act on "is gone" without knowing which.
    ///
    /// Run rather than read, and against a process that is really there: the start time is cut out
    /// of `/proc/<pid>/stat` by a `sed`/`cut` pair, and the only way to know the guard agrees with
    /// what stamped the record is to point both at the same live pid.
    ///
    /// Linux only: the guard's whole subject is `/proc/<pid>/stat` and the kernel's boot id.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_box_that_cannot_be_entered_says_which_proof_failed() {
        let stat = std::fs::read_to_string("/proc/self/stat").expect("a linux /proc");
        let start: u64 = stat
            .rsplit_once(") ")
            .expect("a stat line")
            .1
            .split_whitespace()
            .nth(19)
            .and_then(|f| f.parse().ok())
            .expect("field 22 of /proc/self/stat");
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .expect("a boot id")
            .trim()
            .to_string();

        let placed = |generation: &str, ns_start: u64| Place {
            name: "web-main".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: std::process::id(),
                home: "/home/agent".into(),
                tree: "/boxes/web-main/tree".into(),
                sock: "/tmp/skein-web-main".into(),
                generation: generation.to_string(),
                ns_start,
            },
        };
        let run = |place: Place| {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(place.guard())
                .output()
                .expect("bash");
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )
        };

        // Both proofs hold: the guard says nothing and gets out of the way.
        let (code, said) = run(placed(&boot, start));
        assert_eq!((code, said.as_str()), (0, ""), "a live box was refused");

        // The sandbox restarted. Every box in it is in this state, so the sentence has to be about
        // the sandbox — being told "web-main is gone", one box at a time, is what sent a person
        // looking for what happened to one box.
        let (code, said) = run(placed("a-different-boot", start));
        assert_eq!(code, 78, "a restarted sandbox was entered anyway");
        assert!(
            said.contains("the sandbox has restarted") && said.contains("everything else"),
            "a sandbox restart was reported as one box going missing: {said}"
        );

        // One pid, reused. About this box and nothing else — and it must still refuse, because the
        // process at that number now is somebody else's.
        let (code, said) = run(placed(&boot, start + 1));
        assert_eq!(code, 78, "a reused pid was entered");
        assert!(
            said.contains("reused") && said.contains("web-main"),
            "a reused pid was not named as one: {said}"
        );
        assert!(
            !said.contains("the sandbox has restarted"),
            "a reused pid was reported as a sandbox restart, which would send a person to start a \
             fleet that is running: {said}"
        );
    }

    /// The in-fleet crossing, entering a real namespace.
    ///
    /// bwrap and tmux are here even though `sbx` is not, which is the whole reason this is
    /// testable: a box is a bwrap mount namespace anchored by a pid, and `nsenter` into one is the
    /// same call whether skein reached the sandbox first or was already in it. What differs is only
    /// the hop before it, which is what `Place::reach` decides.
    ///
    /// The proof is the namespace itself. Running `readlink /proc/self/ns/mnt` through the crossing
    /// and comparing it with the *test process's* is the one assertion that cannot pass by
    /// accident: an argv that failed to enter reports this process's namespace, and an argv that
    /// entered reports the box's.
    #[test]
    fn a_crossing_in_the_fleet_enters_the_box_without_sbx() {
        if !crate::testutil::bwrap_works() {
            crate::testutil::skip(
                "bwrap cannot make a namespace here, so there is none to cross into",
            );
            return;
        }
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        // Pinned, because `config::skein_home` refuses an unpinned test rather than answering
        // with the real `~/.skein` — where this fixture's state would otherwise land (SKEIN-626).
        std::env::set_var("SKEIN_HOME", &dir);
        let anchor_at = dir.join("anchor");
        let bwrap_err = dir.join("bwrap.err");
        // **Into the guard at the spawn, not at the end.** Everything below unwinds past a plain
        // teardown — four assertions, an `expect`, and the `panic!` in the anchor wait a few lines
        // down, which is itself a panic that would strand the bwrap it is complaining about
        // (SKEIN-1008). `BoxlikeNamespace` kills the anchor and then the bwrap from its `Drop`, so
        // the failing run leaves as little behind as the passing one.
        //
        // There is **no `--unshare-pid`**, which is what makes the anchor a separate thing to kill:
        // it is an ordinary process in this pid namespace, and `bash -c` execs the last command of
        // its string, so the anchor pid IS the `sleep 60`. Killing the bwrap reaches the bwrap and
        // nothing else, and bwrap is what was WAITING on the anchor — so bwrap first reparents the
        // sleep to pid 1 to run out its full minute (SKEIN-1005, which is SKEIN-861/892 in the Rust
        // tier long after `tests/ui/lift.mjs` fixed the same two lines).
        let mut boxlike = crate::testutil::BoxlikeNamespace::holding(
            std::process::Command::new("bwrap")
                .args(["--dev-bind", "/", "/", "--"])
                .arg("bash")
                .arg("-c")
                .arg(format!("echo $$ > {}; sleep 60", anchor_at.display()))
                // stdout nulled: a child that outlives this holds an inherited pipe open, and
                // `cargo test` then looks like a hang long after the test finished. stderr goes to
                // a FILE rather than to `/dev/null` for the same reason inverted — a file holds no
                // pipe open, so it costs nothing here and it is the only place bwrap's own refusal
                // is recorded. Nulling it is why 179KB of CI log never said `apparmor` or `userns`.
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(&bwrap_err).expect("a file for bwrap's stderr"))
                .spawn()
                .expect("start a box-like namespace"),
        );
        let anchor: u32 = {
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

        // **The first statement after the pid is known**, so there is no window at all in which
        // this test has an anchor the guard has not been told about. The guard hands back the stamp
        // it recorded, so the number this placement record is addressed by IS the number compared
        // against before anything is signalled: read twice it could differ twice, read once it
        // cannot.
        let ns_start: u64 = boxlike.inside(anchor);

        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        let place = Place {
            name: "demo".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: anchor,
                home: std::env::var("HOME").unwrap_or_else(|_| "/root".into()),
                tree: "/".into(),
                sock: dir.join("session.sock").to_string_lossy().into_owned(),
                generation: boot.trim().to_string(),
                ns_start,
            },
        };
        let mine = std::fs::read_link("/proc/self/ns/mnt")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let theirs = std::fs::read_link(format!("/proc/{anchor}/ns/mnt"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_ne!(mine, theirs, "the fixture is not in a namespace of its own");

        // **No first hop at all**, and the crossing still lands inside the box. The half that
        // used to sit above this asserted the `sbx exec` prefix, calling it "the fallback that
        // makes 4c revertible" — 4c is not revertible now (SKEIN-576), so that assertion was
        // about a decision rather than a behaviour, and it went with the decision.
        let argv = place.exec_argv("readlink /proc/self/ns/mnt");
        assert!(
            !argv.iter().any(|a| a == "sbx"),
            "the in-fleet crossing still spells sbx, which does not exist here: {argv:?}"
        );
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .expect("run the in-fleet crossing");
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            said,
            theirs,
            "the crossing did not enter the box (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_ne!(said, mine, "the command ran here rather than in the box");

        // **No teardown here, and that is the point.** SKEIN-1005 put the kill at the bottom of
        // this body — anchor first and bwrap second, which is the right order and was the whole of
        // its finding — but four assertions and an `expect` stand between it and the spawn, so the
        // run that failed was exactly the run that leaked. It is `crate::testutil::BoxlikeNamespace`
        // now, and `boxlike` is dropped by every way out of this function (SKEIN-1008).
        std::env::remove_var("SKEIN_HOME");
    }

    /// The guard still spends the anchor, and dropping the `sbx` hop did not drop it with it.
    ///
    /// `reach` only ever decided what ran *before* the crossing, so the shell is the same one it
    /// always was — but that is a claim worth a test rather than a reading, because the whole of
    /// the refusal lives in an argv that a change to argv-building can quietly stop producing.
    ///
    /// It used to run the loop below twice, once per deployment. There is one (SKEIN-576), and the
    /// property was never about the hop: an address that cannot be proved must not reach `nsenter`,
    /// whatever is or is not in front of it.
    #[test]
    fn dropping_the_sbx_hop_does_not_drop_the_anchor_check() {
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let unprovable = Place {
            name: "demo".into(),
            sandbox: "skein-fleet".into(),
            at: Where::Shared {
                ns_pid: 4242,
                home: "/home/agent".into(),
                tree: "/boxes/demo/tree".into(),
                sock: "/boxes/demo/session.sock".into(),
                // Written before skein stamped anchors: exactly the record `provable` refuses.
                generation: String::new(),
                ns_start: 0,
            },
        };
        let argv = unprovable.exec_argv("echo reached");
        let joined = argv.join(" ");
        assert!(
            !joined.contains("nsenter"),
            "an address that cannot be proved built an nsenter anyway: {joined}"
        );
        assert!(
            joined.contains("skein restart demo"),
            "the refusal does not say what would fix it: {joined}"
        );
        std::env::remove_var("SKEIN_HOME");
    }

    /// A sandbox other than the one skein is standing in is refused, by EVERY argv builder, and
    /// says so.
    ///
    /// Found while writing `reach` rather than planned: dropping the `sbx exec` hop is right when a
    /// second hop enters a namespace, and `SandboxItself` has no second hop. Drop both and the
    /// command runs in the sandbox skein is standing in — a different machine with the same paths
    /// on it. All four builders are checked because the refusal has to be in the one place they
    /// share; three of four would be a hole with no symptom until somebody used the fourth.
    ///
    /// This used to open by asserting the *other* deployment still aimed such an address, at
    /// `sbx exec another-fleet`. That was the escape hatch — drive the other sandbox from a host —
    /// and it went with the host (SKEIN-576). What is left is the refusal, which is now the only
    /// answer rather than one of two.
    ///
    /// **What would make this fail**: dropping the `!the_one_we_are_in` guard's companion, so
    /// `unreachable_from_fleet` returns `None` for a foreign sandbox. Every builder would then
    /// produce a runnable argv, and `echo hello` would appear in the joined string.
    #[test]
    fn another_sandbox_is_not_silently_run_in_the_one_skein_stands_in() {
        let _g = crate::testutil::env_lock();
        // Pinned, because what this exercises resolves `config::skein_home`, which refuses an
        // unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        let skein_home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &skein_home);
        let elsewhere = crate::place::own_sandbox("another-fleet");
        for argv in [
            elsewhere.exec_argv("echo hello"),
            elsewhere.write_argv("cat > /tmp/x"),
            elsewhere.raw_argv(&["cat", "/etc/hostname"]),
            elsewhere.interactive_argv("bash -l"),
        ] {
            let joined = argv.join(" ");
            assert!(
                joined.contains("no sbx here") && joined.contains("exit 1"),
                "another sandbox was addressed from inside the fleet instead of refused: {joined}"
            );
            assert!(
                !joined.contains("echo hello") && !joined.contains("/etc/hostname"),
                "the refusal still carries the command it refused: {joined}"
            );
        }
        std::env::remove_var("SKEIN_HOME");
    }
}
