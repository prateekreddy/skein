//! Starting a box: the session and provisioning scripts, the start itself, the failure it
//! leaves behind, ensuring its session, and whether it is ready.

use super::*;

/// Drop what the *previous* session said about this box, as a new one starts.
///
/// Turn state is a claim about a session — "working", "waiting", "ended" — and it is written into
/// the repo's store, which lives on the host and outlives the box entirely. So a migrated box came
/// up reading `ended`: stopping the old sandbox killed its agent, the SessionEnd hook faithfully
/// recorded that, and the new box inherited a dead session's last word and looked terminated while
/// sitting there perfectly alive. The same would greet every box after a resize.
///
/// Only the claim about the current turn goes. The narrative signal, the telemetry and the hook log
/// are history — they describe what happened, not what is happening, and a box that has just moved
/// is exactly when its history is worth keeping. The in-flight sub-agent counter goes too: it counts
/// processes that died with the old session, and a stale one would make the first Notification of
/// the new session read as "still busy" instead of "needs you".
///
/// Absent files are the normal case (a box being created for the first time), so this is silent.
fn forget_turn_state(repo: &Repo, name: &str) {
    let status = std::path::Path::new(&repo.store).join("status");
    for file in [format!("{name}.json"), format!("{name}.agents")] {
        let _ = std::fs::remove_file(status.join(file));
    }
}

/// The shell that starts a box: its namespace, its tmux server, and the agent inside it.
pub fn session_script(name: &str, session: &str, agent_command: &str) -> String {
    format!(
        // The shared ceilings ride in the environment rather than as an eighth positional, because
        // the launcher already installed in a running sandbox does not know about them: a new
        // argument would be read as part of the command, and every box restart would fail until
        // something reinstalled the script. An old launcher ignores an environment variable.
        // `SKEIN_GIT_SCOPE` and `SKEIN_BOX_REPO` ride in the environment for the same reason as the
        // ceilings above, and it is not a stylistic one: a launcher already installed in a running
        // sandbox would read an eighth positional as part of the command, and every box restart
        // would fail until something reinstalled the script. An old launcher ignores an env var.
        "SKEIN_FLEET_LIMITS={fleet_q} SKEIN_FLEET_GUARANTEES={guard_q} \
         SKEIN_GIT_SCOPE={scope_q} SKEIN_BOX_REPO={repo_q} \
         SKEIN_BOX_PRIVILEGED={priv_q} SKEIN_MODEL_SCRATCH={scratch_q} \
         SKEIN_FLEET_MOUNTS={mounts_q} SKEIN_BOX_STORE={store_q} \
         SKEIN_BOX_PEERS={peers_q} SKEIN_FLEET_NAME={fleetname_q} \
         {launcher} {name_q} {root_q} {pid_q} {session_q} {state_q} {limits_q} bash -lc {cmd_q}",
        launcher = sh_quote(&box_session_path()),
        // Where the agent in this box keeps its model scratch, as a path relative to the box's own
        // HOME — the launcher joins the two, because only the shell inside the namespace can name
        // that HOME. [`MODEL_SCRATCH`] is the definition; this is the box's copy of the same rule
        // the model call and the login terminal run on, and the value travels rather than the path
        // being spelled out a second time in the shell script (SKEIN-289).
        //
        // In the environment for the same reason as everything above it: a launcher already
        // installed in a running sandbox would read an eighth positional as part of the agent
        // command. Unset, the launcher exports nothing and the box is exactly where it was.
        scratch_q = sh_quote(MODEL_SCRATCH),
        // The mount set the launcher cannot learn for itself, and the two paths out of it this box
        // is entitled to. The launcher covers every mount and binds these back — an inversion, not
        // a list of things to hide, because a repo's `store` is an arbitrary host path chosen at
        // repo-add time (`--store` takes one) and no rule written over one root reaches
        // `/home/you/code/thing`.
        //
        // Empty when there is no repo for this box, and that is the safe direction: the box gets a
        // covered view with nothing bound back rather than an uncovered one.
        mounts_q = sh_quote(&mount_manifest(name)),
        store_q = sh_quote(&repo_for_box(name).map(|r| r.store).unwrap_or_default()),
        // Whether this box joins the fleet's peer network. In the environment for the same reason
        // as everything above it, and read back out of the launcher's own report by
        // [`peers_from_launch`] so the record says what the box was BORN with rather than what the
        // config says now. A box belonging to no registered repo is on, which is the ship default
        // (`repos::box_is_on_the_peer_network`).
        peers_q = sh_quote(if crate::repos::box_is_on_the_peer_network(name) {
            "1"
        } else {
            "0"
        }),
        // Off unless the file says otherwise, and an unreadable answer is off. The two directions
        // are not equal: guessing "privileged" hands one box every other box's credentials, and
        // guessing "not" costs the workshop box a restart after someone flips the switch.
        priv_q = sh_quote(if box_is_privileged(name) { "1" } else { "0" }),
        scope_q = sh_quote(if crate::gitgate::box_is_scoped(name) {
            "repo"
        } else {
            "fleet"
        }),
        repo_q = sh_quote(&crate::gitgate::box_repo_slug(name)),
        name_q = sh_quote(name),
        root_q = sh_quote(&box_root(name)),
        pid_q = sh_quote(&box_pidfile(name)),
        session_q = sh_quote(session),
        state_q = sh_quote(&box_state(name)),
        limits_q = sh_quote(&box_limits()),
        fleet_q = sh_quote(&fleet_limits()),
        guard_q = sh_quote(&fleet_guarantees()),
        // The sandbox's own name, so a box can finish a sentence about the sandbox it lives in.
        //
        // A box cannot work this out for itself: `sbx` is not on its PATH and no sandbox
        // configuration is mounted into it (architecture §9.6 measured exactly that). The one place
        // the answer exists is out here, so it travels — in the environment, for the same reason as
        // every variable above it: a launcher already installed in a running sandbox would read a
        // new positional as part of the agent's command and every box restart would fail, where an
        // old launcher simply ignores a variable it does not know.
        //
        // What reads it is the git shim's blocked-egress hint (`src/box-session.sh`), which prints
        // `sbx policy allow network --sandbox <name>` for someone to paste unedited — the same
        // string `health::github_reach_line` builds on the host, from this same
        // `place::fleet_sandbox`, so the two surfaces cannot drift into naming different sandboxes.
        // Unset (an older launcher), the shim says it was not told the name rather than emitting an
        // empty `--sandbox `, which would look complete and not be.
        fleetname_q = sh_quote(&crate::place::fleet_sandbox()),
        cmd_q = sh_quote(agent_command),
    )
}

/// The fleet's mount set, one host path per line, for the launcher to cover.
///
/// Newline-separated because these are arbitrary host paths and every other separator can occur in
/// one. A path that contains a newline is *dropped* with a warning rather than passed: dropped, it
/// stays covered and a box loses access to it loudly; passed, it would split into two lines and the
/// launcher would bind back a directory nobody named.
///
/// **Empty when skein cannot name the box's repo**, which leaves the box uncovered rather than
/// covered-with-nothing-back. `repo_for_box` resolves by longest id prefix, so a box named after
/// its repo resolves and a box someone named themselves may not — and a box whose entitlements
/// skein cannot compute is exactly the box that must not have them computed as "none": it would
/// come up with no store, and provisioning gates startup. Said out loud, because a cover that
/// silently did not apply is the failure this whole mechanism exists to prevent.
///
/// **And said in the BOX as well, not only here** (SKEIN-836). This `eprintln!` goes to the
/// server's own stderr, which SKEIN-799 established nobody reads, so for a long time the only
/// difference between a box that is uncovered on purpose — the workshop box, which announces
/// itself at every start — and one that is uncovered because its name matched nothing was that the
/// deliberate one said so. `box-session.sh` now announces the accidental one beside it, off the
/// empty manifest this returns. (Neither announcement is *delivered* yet; SKEIN-846 is that.)
///
/// **What "uncovered" is not**, because this line claimed for a long time that such a box "starts
/// with the sandbox's whole view, as boxes did before covers", and it does not. An empty manifest
/// only silences the two loops written over the manifest — the SKEIN-219 ancestor cover and the
/// per-mount cover. Everything the launcher spells from paths skein chose still applies: the tmpfs
/// over the fleet root with this box's own checkout bound back, the read-only state parent, the
/// `private/` cover, the `/run` covers. So the other BOXES are still gone, and what stays reachable
/// is the host mounts — every other repo's store and work tree, and, on a fleet whose state sits on
/// a mounted volume, the volume holding `credentials/`, `api-token` and `github-pats/`. That is
/// narrower than "everything" and much worse than "nothing", and a message that overstates it is
/// one a reader learns to discount. `tests/isolation_bwrap/` asserts both halves against real
/// bwrap rather than leaving this paragraph to be believed.
fn mount_manifest(name: &str) -> String {
    if repo_for_box(name).is_none() {
        eprintln!(
            "skein: {name} matches no repository skein knows, so it cannot be told which mounts \
             are its own — it comes up with no mount cover over the host's, so every other repo's \
             store and work tree, and whatever this fleet's state sits on, are readable from it. \
             Its own checkout and the other boxes' are still separate."
        );
        return String::new();
    }
    let mut out = String::new();
    for mount in fleet_mounts() {
        if mount.contains('\n') {
            eprintln!(
                "skein: {mount:?} has a newline in it, so boxes cannot be told about it — \
                 it stays covered, and a box of that repo will not see it"
            );
            continue;
        }
        out.push_str(&mount);
        out.push('\n');
    }
    out
}

/// What a box's mount cover amounts to — the one fact `box-session.sh` announces at start.
///
/// **Three states, because two of them are uncovered and only one was chosen.** That was the whole
/// of SKEIN-836: the workshop box is exempt from the cover by a switch somebody threw and says so
/// at every start, while a box skein cannot match to a repository is exempt by accident and said
/// nothing at all. From inside the box, and from every surface skein had, they were the same box.
///
/// Derived rather than reported, and the difference is worth naming. The launcher's own banner is
/// the *running* box's answer, decided from the manifest it was actually handed; this is what the
/// next start would decide, from the same two inputs ([`box_is_privileged`] and [`mount_manifest`]),
/// which is what a settings pane is for. They differ only for a box whose repository was registered
/// or removed since it came up — and for that box the honest answer is the one that changes, since
/// what it says is what a restart would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// Skein can name this box's mounts, so the cover in `box-session.sh` applies.
    Covered,
    /// The workshop box: uncovered deliberately, by [`box_is_privileged`].
    Workshop,
    /// Uncovered with nobody having chosen it — no manifest, so the two cover loops written over
    /// the manifest iterate nothing and the host's mounts stay exactly where the sandbox put them.
    Uncovered,
}

impl Exposure {
    /// The word skein's API serves and the cockpit renders.
    ///
    /// Lowercase and stable: it is a value in a JSON body that a page switches on, not a sentence.
    pub fn spelled(self) -> &'static str {
        match self {
            Exposure::Covered => "covered",
            Exposure::Workshop => "workshop",
            Exposure::Uncovered => "uncovered",
        }
    }
}

/// Which of the three a box is in.
///
/// Privileged first, and that ordering is the launcher's own (`box-session.sh`, the announcement
/// block): a privileged box never even computes the uncovered flag, because "uncovered" here means
/// *nobody chose this* and the workshop switch is a choice. A privileged box that also matches no
/// repository is still the workshop box.
///
/// **The second arm asks [`mount_manifest`]'s own first question rather than calling it**, and the
/// two have to keep agreeing or this lies: a panel that says *covered* over a launcher about to be
/// handed an empty manifest is worse than one that says nothing. Calling it would be the obvious
/// way to guarantee that and is the wrong one here — `mount_manifest` narrates, deliberately, and
/// this is read every time somebody opens a settings panel. So the agreement is asserted instead,
/// in both directions, by `what_a_panel_is_told_about_a_cover_is_what_the_launcher_will_do`.
pub fn box_exposure(name: &str) -> Exposure {
    if box_is_privileged(name) {
        return Exposure::Workshop;
    }
    match repo_for_box(name).is_none() {
        true => Exposure::Uncovered,
        false => Exposure::Covered,
    }
}

/// Refuse to start a box that would come up with no mount cover, unless somebody has said to.
///
/// **The reach is why.** An uncovered box keeps every host mount the sandbox gave it: every other
/// repository's store and work tree, and, on a fleet whose state lives on a mounted volume, the
/// volume holding `credentials/`, `api-token` and `github-pats/` (SKEIN-219). That is the workshop
/// box's reach handed to a box nobody decided anything about, and the only thing standing between
/// the two was which of them announced itself.
///
/// **Here rather than in the CLI**, and the difference is the whole point of putting it here.
/// `skein start` already turns away a box with no registered repo, and that refusal is the *name*
/// check — it never sees the box that is uncovered for the other reasons, and it is not on the path
/// `reviewbox`, `takeover` or [`ensure_box_session`] take. The condition belongs where the manifest
/// is computed, so every way of bringing a box up meets the same wall.
///
/// **What it says, and why in that order** — the two standing UX rules, applied. *Inform and offer*:
/// the cost is stated plainly, the safe step is the offer, and the unsafe one is left whole and
/// copyable rather than hidden behind a hint. *Never strand*: adopting the repository is usually the
/// real fix, so it is named first and skein simply carries on the moment the name resolves — no
/// flag to un-set, because [`uncovered_is_allowed`] is only ever read for a box that is still
/// uncovered.
pub fn refuse_if_uncovered(name: &str) -> Result<(), String> {
    if box_exposure(name) != Exposure::Uncovered || uncovered_is_allowed(name) {
        return Ok(());
    }
    Err(format!(
        "box {name} would come up UNCOVERED, so skein has not started it.\n\
         \n\
         Nothing can tell it which mounts are its own: skein names a box's mounts from the \
         repository its name matches, and {name} matches none. Started like that it reads and \
         writes every other repository's store and work tree in this fleet, and whatever this \
         fleet's own state sits on — on a volume fleet, the directory holding credentials/, \
         api-token and github-pats/. Its own checkout and the other boxes' stay separate either \
         way.\n\
         \n\
         Usually the repository is the fix rather than the exception:\n\
         \x20 `skein repos` lists the ids skein knows, and a box whose name starts with one of \
         them is covered with no further ceremony.\n\
         \x20 `skein add <git-url> --id <id>` registers the repository this box is for.\n\
         \n\
         To start it uncovered anyway, having read that, add --uncovered to the command you just \
         ran:\n\
         \x20 skein start {name} --uncovered   \u{2014} or skein attach {name} --uncovered\n\
         skein remembers that for {name}, so its restarts, attaches and heals do not ask again."
    ))
}

/// How long provisioning gets, and it is **derived from what the script itself allows**.
///
/// `skein-startup.sh` bounds every network step of its own, and those bounds add up: it waits up to
/// 240s for the agent image's background `apt update` to finish rather than racing it, then allows
/// 120s to install, 120s to update, and 120s to install again. Six hundred seconds, worst case.
///
/// The caller used to give it three hundred. So a box that started while apt was busy spent four
/// minutes waiting by design, was killed at five, and the start failed — reported as "the restart
/// never really completes", on a fleet whose agent was current and whose script was working
/// perfectly. A deadline shorter than the callee's own budget turns its answer into silence.
///
/// The codebase already had the rule and applied it one layer down: the agent client asked for
/// `timeout + 5s` because "a socket deadline that fired first would turn its answer into silence".
/// This is the same rule, at the layer above, and `the_provisioning_budget_outlasts_the_script`
/// keeps the two in step by reading the script rather than trusting this comment.
pub(crate) const PROVISION_BUDGET: Duration = Duration::from_secs(900);

/// How much longer the agent launch waits than provisioning is allowed to take.
///
/// The direction is the whole point. This wait opens BEFORE provisioning is invoked — `start_box`
/// starts the session, reads the anchor, records the placement, and only then provisions — so a wait
/// equal to the provisioning budget closes while provisioning is still legitimately running. It was
/// 600s against a script that allows itself 840, which is a start killed by its own watcher on a
/// fleet where everything works.
///
/// Lives here rather than in `runtime` because what it waits FOR lives here: `/tmp/skein-startup.ready`
/// is written by the last line of `KIT_STARTUP_SH`, and its bound is derived from the budget above.
/// The alternative was `runtime` reaching into `fleet` for that budget — an edge from a low-level
/// module to a high-level one, for a constant that was never `runtime`'s to own.
pub(crate) const SETUP_WAIT_MARGIN: std::time::Duration = std::time::Duration::from_secs(120);

/// How long the first `sbx exec` waits for the kit's handshake before calling the box broken.
pub(crate) fn setup_wait_secs() -> u64 {
    (crate::fleet::PROVISION_BUDGET + SETUP_WAIT_MARGIN).as_secs()
}

/// `sbx create` returns before its durable startup hooks finish. The first `sbx exec` keeps the box
/// alive and waits for the kit's provider-neutral handshake; later attaches skip this entirely and
/// go straight to tmux. A bounded wait makes a broken kit visible instead of hanging the terminal.
///
/// **Bounded, not short.** The bound is derived from provisioning's, because the failure it is here
/// to catch — a kit that broke — announces itself: the script's EXIT trap writes
/// `/tmp/skein-startup.failed` and the loop below exits on it in under a second. The full wait
/// elapses only when provisioning is still running, and cutting THAT short reports a working box as
/// a broken one.
pub(crate) fn initial_setup_wait() -> String {
    setup_wait_script("/tmp", setup_wait_secs())
}

/// The wait itself, over marker names that carry the current start's id.
///
/// A box's /tmp is `$root/tmp` on disk and a restart keeps it, so the markers of every previous
/// start are still sitting there when this wait opens — and it opens BEFORE provisioning is
/// invoked (see [`SETUP_WAIT_MARGIN`]), which is exactly the window a stale marker fills. Bare
/// names read the previous start's `ready` as this one's — a box whose provisioning timed out came
/// up "working" — and would read a leftover `failed`, which is tested FIRST, as a refusal of a
/// start it knows nothing about. So the launcher writes a fresh id into `skein-start-id` on every
/// launch, the kit suffixes the markers with it, and this reads the same id at run time: a stale
/// marker is inert, not deleted — deleting `failed` would erase the one record of a start that
/// genuinely broke. No id file (a per-VM sandbox, whose /tmp dies with it) falls back to the bare
/// names, which there still mean what they always did.
///
/// `markers` is `/tmp` in production; a parameter so the two-consecutive-starts case is testable
/// against a directory that is not this machine's /tmp.
fn setup_wait_script(markers: &str, wait_secs: u64) -> String {
    format!(
        "echo 'skein: waiting for box setup…'; \
         sid=$(cat \"{m}/skein-start-id\" 2>/dev/null | tr -cd 'A-Za-z0-9._-'); \
         ready=\"{m}/skein-startup.ready${{sid:+.$sid}}\"; \
         failed=\"{m}/skein-startup.failed${{sid:+.$sid}}\"; \
         n=0; while [ \"$n\" -lt {wait_secs} ]; do \
         if [ -e \"$failed\" ]; then echo 'skein: box setup failed; inspect /var/log/sbx-kit-startup.log'; tail -40 /var/log/sbx-kit-startup.log 2>/dev/null || true; exit 1; fi; \
         [ ! -e \"$ready\" ] || break; n=$((n + 1)); sleep 1; done; \
         if [ ! -e \"$ready\" ]; then echo 'skein: box setup timed out; inspect /var/log/sbx-kit-startup.log'; exit 1; fi; ",
        m = markers,
    )
}

/// The shell that provisions a box: the store link, the branch, the hooks, the guide, the tracker.
///
/// This runs the kit's own startup script — the same bytes sbx runs at startup in a `--clone`
/// sandbox — rather than a fleet-shaped reimplementation of it. Provisioning is a dozen steps and
/// most of them fail *quietly*: a box whose store never got linked looks perfectly healthy and
/// simply never reports. Two implementations of that would be two sets of ways to be silently dark.
///
/// Four env vars carry what the script cannot work out for itself in a shared sandbox, because
/// every signal it normally reads there belongs to the sandbox rather than to the box:
///   * `SKEIN_PROVISION` — say so explicitly, since `/run/sandbox/source` does not exist here;
///   * `SKEIN_BOX`       — the identity, or every box reads one launch spec and one boot report;
///   * `SKEIN_STORE`     — the repo's store, a directory inside the mounted workspace rather than
///     a mount of its own, so the script's scan would find nothing;
///   * `WORKSPACE_DIR`   — the box's checkout, which is not this process's cwd.
///
/// **Must run inside the box's namespace**, not the sandbox: it writes `~/.codex`, `~/.claude` and
/// `~/shared`, and outside the namespace those are the sandbox's, shared by every box.
pub fn provision_script(name: &str, store: &str) -> String {
    format!(
        "SKEIN_PROVISION=1 SKEIN_BOX={name_q} SKEIN_STORE={store_q} WORKSPACE_DIR={tree_q} \
         bash {script_q}",
        name_q = sh_quote(name),
        store_q = sh_quote(store),
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        script_q = sh_quote(&box_provision_path()),
    )
}

/// Bring one box up inside the fleet sandbox, from nothing to a running, provisioned agent.
///
/// The ordering is forced by what each step produces rather than chosen: the anchor pid does not
/// exist until the session runs, the placement is meaningless without the anchor, and provisioning
/// must go *through* the placement or it writes the sandbox's `~/.claude` instead of the box's.
///
///   ensure the sandbox → clone the tree → start the session → read the anchor → record the
///   placement → provision inside it
///
/// Idempotent at the sandbox level and deliberately **not** at the box level: `clone_script` refuses
/// a tree that already exists and `box-session.sh` refuses a second server, because both are how a
/// re-run would otherwise hand a box someone else's uncommitted work or strand its namespace.
pub fn start_box(
    name: &str,
    repo: &Repo,
    branch: &str,
    agent_command: &str,
    purpose: crate::place::Purpose,
) -> Result<(), String> {
    // Whatever happened, the sweep's picture is now older than the act. On success there is a box
    // that was not there; on failure there may be a half-started one — and the caller is a person
    // who just pressed a button and is looking at the row.
    let out = disturbing_liveness(|| start_box_inner(name, repo, branch, agent_command, purpose));
    // Kept, because the person who needs it is not looking at this terminal. Creating a box from the
    // cockpit runs `skein start` in a PTY; when it fails, that terminal closes, the browser
    // reconnects, and the fresh one has none of the output. What it said instead was "its last start
    // failed, and the error came from that run rather than from this terminal" — an admission that
    // the answer existed and had been thrown away.
    match &out {
        Ok(()) => forget_start_failure(name),
        Err(why) => remember_start_failure(name, why),
    }
    out
}

/// Where the last failed start's reason is kept, per box.
///
/// Under `$SKEIN_HOME` rather than in the fleet, because a start that failed may never have reached
/// the sandbox — the commonest failure of all is not being able to see it.
fn start_failure_path(name: &str) -> Option<std::path::PathBuf> {
    valid_name(name).then(|| skein_home().join("starts").join(format!("{name}.err")))
}

/// Keep why a start failed, for the terminal that will ask later. Public because the CLI fails
/// *before* [`start_box`] too — no repo registered for the name, no branch to start on — and those
/// vanish with the create terminal exactly as the others did.
pub fn remember_start_failure(name: &str, why: &str) {
    let Some(path) = start_failure_path(name) else {
        return;
    };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_ok() {
        let _ = crate::util::write_atomic(&path, dir, why.trim().as_bytes());
    }
}

/// Drop the record once the box starts. A stale reason on a working box is worse than none: it
/// would explain a failure that is over.
fn forget_start_failure(name: &str) {
    if let Some(path) = start_failure_path(name) {
        let _ = std::fs::remove_file(path);
    }
}

/// How much of a recorded start failure [`last_start_failure`] hands back.
///
/// **A runaway guard, not a display budget** — see that function for why the difference decides
/// the number. Set where nothing skein composes can reach it: the longest of those is
/// `start_box_inner`'s store-visibility refusal, past 600 characters before a single variable is
/// substituted into it, plus `sandbox::launch_never_ran`'s arm that carries the server's entire
/// `$PATH`. That leaves better than 3,000 characters of `$PATH` before this is felt at all.
const START_FAILURE_CAP: usize = 4000;

/// Why this box's last start failed, if one did and the box still is not there.
///
/// **The cap here is a runaway guard, and the old one was being used as a display budget**
/// (SKEIN-675). Something has to bound this: it reads a file off disk, and `absent_box_reason`
/// puts whatever comes back into a reconnecting terminal, so a `starts/<box>.err` left by an older
/// skein — or one some command's output ended up in — must not be poured into it whole.
///
/// What it must never do is fall INSIDE a sentence skein wrote, and at 400 that is exactly what it
/// did. Every one of these sentences puts the cure at the END: `spawn_failure` closes with "what
/// matters is the PATH the server was started with", and the store-visibility refusal closes with
/// "Do NOT `sbx rm` … it destroys every box in the sandbox". Cutting there keeps the complaint and
/// removes the only half a person can act on, and the sentence that overflowed first is the one
/// whose length IS the reader's `$PATH` — so the readers it failed were exactly the readers it was
/// written for.
///
/// Still cutting the head-first way, and that is the other half of the decision. `keep_tail` exists
/// for sources whose news is at the end, and swapping to it would be right if the overflow were a
/// skein sentence. At this size it is not: what reaches 4,000 characters is a blob, and a blob's
/// front is skein's own framing of it.
pub fn last_start_failure(name: &str) -> Option<String> {
    let text = std::fs::read_to_string(start_failure_path(name)?).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| crate::util::clip(text, START_FAILURE_CAP))
}

/// **Where to look when provisioning failed** — and the point is that it depends on how it failed.
///
/// This used to be one sentence for every failure: *"if it keeps timing out, something in the
/// sandbox's apt is stuck"*. That is good advice for a timeout and actively misleading for
/// anything else. On 2026-09-03 a box start was reported as `exited -15` — the script was KILLED,
/// which is neither a timeout nor a fault in the script — and this line sent the reader to apt,
/// where nothing was wrong. Two hypotheses and an hour went into a queue that was empty.
///
/// So the advice is chosen by what actually happened, and a failure it cannot classify gets none:
/// no advice is better than advice about a mechanism that had no part in it.
fn what_to_look_at(why: &str) -> &'static str {
    if why.contains("was killed by") {
        return ". Nothing in the script failed — it was stopped from outside, so the fault is not \
                necessarily in this fleet; run it again and see whether it repeats before looking \
                for a cause";
    }
    if why.contains("did not finish in time") || why.contains("timed out") {
        return "; if it keeps timing out, something in the sandbox's apt is stuck and \
                `skein doctor` reports the substrate queue";
    }
    ""
}

/// **A box may be restarted, and it may not be repurposed.**
///
/// Everything in [`start_box_inner`] ADOPTS what it finds: a checkout already there is kept, a live
/// session is kept, and the placement record is rewritten. That is exactly right for a restart —
/// which is what `skein start` on an existing box means, and how a box survives a sandbox rebuild.
/// It is exactly wrong across a change of purpose, and the direction that matters is not the
/// obvious one: skein is about to start boxes of its own, and a review box that took the name of a
/// box somebody is working in would re-provision it, re-record it, and report success.
///
/// **The guard is the RECORD, not the name.** A convention like `<repo>-pr-<n>` is a good name and
/// a bad guard: a branch called `pr-123` slugs to exactly that, so the collision it is meant to
/// prevent is still reachable, and a guard that is usually right is the kind this file has had to
/// remove before. What cannot be a coincidence is the purpose already written down for this name.
///
/// Its own function rather than a block inside the caller so that both halves can be asserted
/// without a fleet: the half that refuses, and — the one that keeps this honest — the half that
/// must NOT refuse, since a guard that turned every restart into a collision would be worse than
/// the fault it prevents.
fn refuse_a_repurpose(name: &str, purpose: crate::place::Purpose) -> Result<(), String> {
    let Some(held) = crate::place::shared_record(name) else {
        return Ok(());
    };
    if held.purpose == purpose {
        return Ok(());
    }
    Err(format!(
        "{name} is already a {} box, and this would start it as a {} one. Nothing has been \
         changed. Boxes are told apart by their placement record rather than by their names, so \
         this is a real collision and not a naming accident: destroy {name} first if it is \
         finished with, or start the new one under another name.",
        held.purpose.spelled(),
        purpose.spelled(),
    ))
}

fn start_box_inner(
    name: &str,
    repo: &Repo,
    branch: &str,
    agent_command: &str,
    purpose: crate::place::Purpose,
) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("invalid box name {name:?}"));
    }
    refuse_a_repurpose(name, purpose)?;
    // Before the sandbox, the clone and the launcher: an uncovered box is refused on what skein
    // already knows about its NAME, so the refusal costs nothing and leaves nothing half-made.
    refuse_if_uncovered(name)?;
    let sandbox = fleet_sandbox();
    // The fleet first: this refreshes the launcher, the in-sandbox agent and the docker config
    // before anything starts a box under them. On a sandbox that is asleep or busy it is where the
    // first minute goes, and it used to go there in silence.
    eprintln!("skein: bringing {sandbox} into line with this build…");
    ensure_fleet(&sandbox)?;
    // There were two calls here that copied a repo's gitignored files — `.env`, a `CLAUDE.md` some
    // repos take their direction from — out of the user's checkout and into the store. They went
    // with local-path repos: a repo is a remote now, no checkout is reachable from inside the fleet,
    // and the pair had already been reduced to printing a warning that the files had not arrived.
    // What a store already holds under `shared-rw/` is still surfaced by the box's own bootstrap.

    // The store is a HOST path used verbatim inside the sandbox, so this is the one precondition
    // worth paying a round-trip for: unreachable, every later step still "succeeds" and the box
    // comes up with no hooks and no probe — the failure this whole path is least able to see.
    let fleet = own_sandbox(&sandbox);
    let probe = format!("test -d {} && echo ok", sh_quote(&repo.store));
    if fleet
        .exec(&probe, Duration::from_secs(20))
        .ok()
        .as_deref()
        .map(str::trim)
        != Some("ok")
    {
        // **The way out is [`fleet_lifecycle_refusal`]'s, not a third one written here**
        // (SKEIN-707). This used to say "`skein resize <memory>` … It carries every existing box
        // across", and both halves had stopped being true: `skein resize` refuses through that
        // function before it reaches any work (SKEIN-679), and the phases that carried boxes back
        // are deleted. A person met the same wall as the cockpit's rebuild and the CLI's resize,
        // from a third direction, and was handed the one instruction of the three that could not be
        // followed — ending in a reassurance, above a line steering them off the act that works.
        //
        // So this states the diagnosis, which is its own, and delegates the remedy to the sentence
        // the other two surfaces already print. What makes that the remedy rather than a generic
        // rebuild is [`fleet_mounts`]: the create line is rendered from *this* installation's
        // mounts, and this repo's store is in them the moment it is registered.
        //
        // The "Do NOT `sbx rm`" line is gone with it, and had to be: the refusal now hands over
        // that exact command as step one, with what it costs in boxes and the save to take first.
        // Two sentences about the same command, one forbidding and one instructing, is worse than
        // either alone.
        let how = fleet_lifecycle_refusal("rebuild", true).unwrap_or_else(|| {
            "Fleet lifecycle lives on the host (docs/architecture.md \u{a7}7.5), and no line could \
             be worked out from here — `skein doctor` prints what it can."
                .to_string()
        });
        return Err(format!(
            "the fleet sandbox cannot see {store}, so box {name} would come up with no store.\n\
             Host paths are mounted when the sandbox is created, and this repo was registered after \
             that — a repo whose store is under the workspace lands on a path that is already \
             mounted, one pointed somewhere else does not.\n\
             The create line below is rendered from this installation's mounts and so includes \
             {store}, which is what makes remaking the sandbox the fix rather than a coincidence.\
             \n\n{how}",
            store = repo.store,
        ));
    }

    // A launch that dies partway leaves a checkout, and sometimes a live session, behind. Carry on
    // from there rather than demand the box be destroyed: cloning is the only step here that is not
    // idempotent, and it is also the only one whose work a repeat would throw away.
    let (has_tree, has_session) = box_progress(&fleet, name)?;
    // What the remote has *now*, not what it had when this mirror was last touched. Best-effort:
    // a box created while the network is down is still worth creating, from a mirror that is a day
    // old, and the alternative is a fleet that cannot start a box because GitHub is unreachable.
    if let Err(why) = crate::repos::fetch_mirror(repo) {
        eprintln!("skein: {name} is cloning from a mirror that could not be refreshed: {why}");
    }
    let source = clone_source(repo);
    // Every repo needs this now, not only an adopted one: the clone comes from the mirror, so
    // `git clone` sets `origin` to a path on the volume, and a box that pushed there would push
    // into skein's own mirror instead of the repo it came from.
    let upstream = crate::repos::repo_origin_url(repo).unwrap_or_default();
    if has_tree {
        eprintln!("skein: {name} already has a checkout; keeping it");
        // Every box cloned before origin was re-pointed still pushes at the host, and they are not
        // going to be re-cloned to fix it. Guarded on origin still *being* what it cloned from, so a box
        // whose remote someone set deliberately keeps it.
        if !upstream.is_empty() {
            let _ = fleet.exec(
                &origin_repair_script(name, &source, &upstream),
                Duration::from_secs(60),
            );
        }
    } else {
        fleet.exec(
            &clone_script(name, &source, &base_branch(repo), branch, &upstream),
            Duration::from_secs(600),
        )?;
    }
    // Before the session, not after: `box-session.sh` reads the token as it comes up to decide what
    // `GH_TOKEN` holds, so a token placed afterwards would leave the box's first turn — the one
    // most likely to push — holding only the read credential. Failures are reported and not fatal:
    // a box that cannot push its own repo yet is recoverable, a box that will not start is not.
    for problem in crate::gitgate::refresh_tokens(name) {
        eprintln!("skein: {name} has no write token yet — {problem}");
    }

    let mut launched: Option<String> = None;
    if has_session {
        eprintln!("skein: {name} already has a live session; keeping it");
    } else {
        forget_turn_state(repo, name);
        // Two minutes' budget, and the same silence problem as the provisioning below: the launcher
        // covers the mounts, makes the cgroup and starts tmux before it reports, so there is nothing
        // to see until it is done.
        eprintln!("skein: starting {name}'s session and its isolation…");
        let out = fleet.exec(
            &session_script(name, "skein-shell", agent_command),
            Duration::from_secs(120),
        )?;
        // Before the anchor is even read: what the launcher has to say is about the box someone is
        // starting right now, and the next step is a clone-and-provision that takes minutes.
        say_what_the_launcher_said(&out);
        launched = Some(out);
    }

    // Two different questions, and reading the anchor unconditionally answered the wrong one on the
    // adoption branch: there, no launcher ran, so "the launcher reports it" reached nothing and the
    // only file left to read was the box's own — which the box writes.
    let (ns_pid, generation, ns_start) = match &launched {
        Some(out) => {
            let pid = anchor_from_launch(out)?;
            let (generation, start) = stamp_anchor(&sandbox, name, pid)?;
            (pid, generation, start)
        }
        None => adopt_anchor(&sandbox, name)?,
    };
    // Empty on the adoption branch, and correctly so: nothing launched, so nothing reported which
    // cover this namespace has, and the copy of `box-session.sh` on disk answers a different
    // question. An adopted box reads as running an older cover until it is restarted, which is the
    // only claim the evidence supports.
    let launcher = launched
        .as_deref()
        .map(launcher_from_launch)
        .unwrap_or_default();
    let ceiling = launched
        .as_deref()
        .map(limits_from_launch)
        .unwrap_or_default();
    // `None` on the adoption branch for the reason `launcher` is empty there: nothing launched, so
    // nothing said which side of the peer switch this namespace was born on. `None` is the third
    // answer and not a guess at either position — see `PlaceRecord::peers` (SKEIN-572).
    let peers = launched.as_deref().and_then(peers_from_launch);
    record_place(
        name,
        &PlaceRecord {
            sandbox: sandbox.clone(),
            ns_pid,
            home: sandbox_home(&fleet)?,
            tree: format!("{}/tree", box_root(name)),
            sock: box_sock(name),
            generation,
            ns_start,
            launcher,
            ceiling,
            peers,
            // Threaded from the caller rather than defaulted, because this is the one place in
            // the tree where the answer is *decided* rather than copied: everything else that
            // builds a record is a fixture. The guard at the top of this function is what stops it
            // being decided a second, different way for a name that already has one.
            purpose,
        },
    )?;

    // The launch spec is how the provisioning script learns the box's branch and runtime — without
    // it the box stays on the clone's default branch, which is a silently wrong box rather than a
    // failed one. The legacy path writes it while building the `sbx create` line; this path had no
    // equivalent. Never overwrite a restore's spec: that one also carries the handoff snapshot.
    if launch_spec(repo, name).is_none() {
        write_launch_spec_for_agent(name, branch, repo, &agent_for_box(name))?;
    }

    // Through the placement, so it lands in the box's private HOME rather than the sandbox's.
    let boxed = place_of(name).ok_or_else(|| format!("box {name} was not placed"))?;
    // **Said before it starts, because this step can take minutes and says nothing while it does.**
    // It installs the kit, the hooks and any approved packages, over a captured `exec` with a
    // five-minute budget — so a start that is working normally prints two lines and then goes
    // completely quiet, which reads as a hang. Reported as "somebody ran restart and it never
    // completed"; it had completed, or was about to.
    eprintln!(
        "skein: provisioning {name} (kit, hooks, approved packages) — up to {} minutes",
        PROVISION_BUDGET.as_secs() / 60
    );
    boxed
        .exec(&provision_script(name, &repo.store), PROVISION_BUDGET)
        .map_err(|why| {
            format!(
                "{why}\n       {name} IS running — its session and namespace came up — but it has \
                 no hooks or kit, so the board cannot see its turns. Run `skein restart {name}` \
                 again{}",
                what_to_look_at(&why)
            )
        })?;

    // Every start, not just a migration's. `migrate_box` used to be the only caller, and its call
    // sits *after* `start_box` — so a migration that failed here left the conversation under the old
    // sandbox's slug with nothing that would ever move it, and the retry (`skein start`, because the
    // placement already exists and `migrate` now refuses the box) started the agent on an empty
    // transcript. Measured on lattice-feat-design-codex-claude: the work restored, the conversation
    // did not. It is idempotent — a box that already has a conversation at its own slug keeps it —
    // so the honest place for it is wherever a box comes up, not on one path through that.
    // After provisioning, because it writes into the box's private HOME, and on every start because
    // a repo registered since the box was built adds a host it has never trusted.
    ensure_box_known_hosts(name);

    // Same reasoning, same moment: a private HOME starts with no committer, and the box finds out
    // when it tries to commit rather than when it was built.
    let (who, email) = box_identity(name);
    let script = identity_script(&who, &email);
    if !script.is_empty() {
        if let Err(e) = boxed.exec(&script, Duration::from_secs(30)) {
            eprintln!("skein: could not set {name}'s git identity ({e}); its first commit will ask who you are");
        }
    }

    match realign_transcript(name) {
        Ok(0) => {}
        Ok(n) => eprintln!("skein: pointed {n} transcript file(s) at {name}'s working directory"),
        Err(e) => {
            eprintln!("skein: {name} came up, but its conversation could not be located ({e})")
        }
    }

    // A box that started without a ceiling started *successfully*, so nothing else would ever say
    // so — and it is the one condition under which one box's runaway build can kill the others.
    if let Some(why) = uncapped_reason(name) {
        eprintln!(
            "skein: {name} is running WITHOUT a memory ceiling ({why}); a runaway build in it can \
             take down every other box in the fleet"
        );
    }
    Ok(())
}

/// Why this box cannot be attached to at all, when that is knowable. `None` ⇒ go ahead and try.
///
/// A box with no placement used to be assumed legacy — one that owns a sandbox named after itself,
/// whose lifecycle sbx manages. That is one of two possibilities. The other is a box whose start
/// **failed**, which has no placement for the same reason it has no checkout: it was never created.
///
/// Treating the second as the first is what produced the loop: the terminal addressed it as its own
/// sandbox, sbx answered `no sandbox named …`, the browser reconnected, and the real error from
/// `skein start` — printed once, at the top — scrolled away behind an endless repeat of a message
/// about a sandbox that was never meant to exist. Worse, sbx's advice there is `sbx create AGENT
/// WORKSPACE`, which builds exactly the per-box VM the fleet exists to replace.
pub fn absent_box_reason(name: &str) -> Option<String> {
    if shared_record(name).is_some() {
        return None;
    }
    // Only speak when sbx has answered at least once. `None` is "cannot tell", and refusing a
    // terminal on that would be worse than letting sbx speak for itself.
    //
    // Note what this does *not* guarantee: [`crate::util::Gate`] serves the last good snapshot while sbx is
    // failing, so this can be reading a stale list. Safe in the direction that matters — a box created
    // since the snapshot has a placement record, which is checked first.
    // **In-fleet there is no second register to consult, and `None` was the wrong answer.**
    // `fleet_boxes` returns `None` in here by design, `?` turned that into "cannot tell", and the
    // caller reads that as "go ahead and try" — so a box whose start FAILED got no refusal at all
    // and its terminal reconnected for ever behind the real error. That is the exact case this
    // function was written for.
    //
    // A placement record is the whole of what skein knows in-fleet, and it was checked above. The
    // foreign-sandbox arm below cannot be decided here (it is a question about the host's machine),
    // so it is not guessed at — the answer is the one that is true either way, with the start
    // failure attached.
    // **A placement record is the whole of what skein knows, and it was checked above.**
    //
    // There used to be a second register and a third answer. `sbx ls` could report a sandbox skein
    // did not place — somebody's own `sbx` box, or one made by a skein old enough to give every box
    // its own VM — and the refusal for those named them as foreign and said how to reach one
    // anyway (`sbx exec -it <name> bash -l`). That question is about the HOST's machine, which
    // nothing in here can see (SKEIN-576), so the arm is gone rather than guessed at, and a foreign
    // sandbox now reads as absent. Recorded in `docs/parity.md` §7: it is a sentence a person could
    // see, and they no longer see it.
    Some(format!(
        "box {name} does not exist: skein has no placement for it{}.\r\n\
         {}\r\n\
         Run `skein start {name} --branch <branch>` on the host to try again.\r\n\
         Do not run `sbx create` — sbx suggests it, and it would build the per-VM box skein no longer \
         supports, reserving a whole VM's memory whether or not the box is working.\r\n",
        // Not "and sbx has no sandbox by that name": skein never asked sbx — it cannot, from
        // in here — and a refusal that cites evidence it does not have is the kind of confident
        // wrong sentence this whole function exists to replace.
        "",
        match last_start_failure(name) {
            Some(why) => format!("Its last start failed: {why}"),
            None => "There is no record of a start having been attempted.".to_string(),
        }
    ))
}

/// Make sure a placed box has a live session, restarting it from its own tree if not.
///
/// The box is its **tree**; the session is disposable. A fleet box's tmux server does not survive
/// the sandbox stopping — measured: `skein start` brought a box up at 14:12 with a live server, and
/// after the sandbox cycled the checkout, the private HOME and the cgroup ceiling were all intact
/// while the server was gone. Without this, every such box is unreachable until someone re-runs
/// `skein start`, and what they see first is `nsenter: cannot open /proc/<pid>/ns/user` — an error
/// about a namespace, for a box that simply needs starting again.
///
/// A no-op for a box with a live session, and for a box that isn't placed (its sandbox is its box,
/// and sbx starts that itself). Never clones: a missing tree is a different problem and saying so is
/// more useful than silently rebuilding one.
pub fn ensure_box_session(name: &str) -> Result<(), String> {
    let Some(record) = shared_record(name) else {
        return Ok(()); // not skein's to start — see `absent_box_reason`
    };
    let fleet = own_sandbox(&record.sandbox);
    // "Alive" is the socket's answer, not the record's. For a record from an earlier boot the
    // sweep deliberately falls back to asking the socket (see `place::local_liveness`) — and a
    // session tmux answers for is exactly what a half-failed restart leaves behind: launched, then the
    // stamp never rewritten. Believing it here is how a box stays unreachable *forever*: every
    // attach finds it "alive", skips the relaunch, and then refuses at the crossing on the stale
    // stamp — seen live on lattice-feat-design-codex-claude, 2026-08-24. So a live session only
    // counts once the record still addresses it.
    let mut progress = None;
    let alive = fleet_liveness().get(name).copied().unwrap_or(false) || {
        let p = box_progress(&fleet, name)?;
        progress = Some(p);
        p.1 // raced with someone else's restart, or the sweep was stale
    };
    if alive {
        match session_reach(&fleet, name, &record)? {
            Reach::Current => return Ok(()),
            // Cannot prove whose the session is, and a working box placed before stamps existed
            // reads exactly like this — so never end it on a guess. The crossing's own refusal
            // names the fix (`skein restart`) without killing anything.
            Reach::Unprovable => return Ok(()),
            Reach::Orphan(why) => {
                // tmux answers for it, but no crossing can enter it, so it is not the box — it is
                // what a half-failed restart left running. End it and start over; the tree, the
                // private HOME and the ceiling survive either way.
                eprintln!(
                    "skein: {name} has a session nothing can enter — {why} — so it is being ended \
                     and {name} started again"
                );
                fleet.exec(&stop_script(name, &record), Duration::from_secs(30))?;
                LIVENESS_GATE.invalidate();
                progress = None; // the stop just changed both of its answers
            }
        }
    }
    let (has_tree, has_session) = match progress {
        Some(p) => p,
        None => box_progress(&fleet, name)?,
    };
    if has_session {
        return Ok(()); // raced with someone else's restart between the checks above
    }
    if !has_tree {
        return Err(format!(
            "box {name} has no checkout in {} — `skein start {name}` to build one",
            record.sandbox
        ));
    }
    // The launcher first, and this is not belt-and-braces. A sandbox keeps whichever copy of
    // `box-session.sh` was installed when it was last provisioned, so a fleet that predates the
    // running skein starts its boxes with an older launcher — and the failure is total rather than
    // partial, because a launcher that cannot parse what this skein passes it exits before tmux and
    // leaves the anchor pid naming a process from the last boot. What anyone sees then is
    // `nsenter: cannot open /proc/<pid>/ns/user` on every reconnect, forever, since nothing on this
    // path ever replaced the copy that could not start. [`heal_fleet`] does this at server start
    // too; here it also covers a sandbox that was asleep then and is being woken now.
    // The same wall `start_box` puts up, on the path that does not go through it. This is where a
    // box whose repository was removed since it started comes back — an attach, a heal, a sandbox
    // that cycled — and re-launching it here would rebuild the uncovered namespace without anybody
    // having asked for one.
    refuse_if_uncovered(name)?;
    if let Err(e) = install_launcher(&record.sandbox) {
        eprintln!("skein: could not refresh the launcher in {} ({e}); {name} starts with whichever copy is already there", record.sandbox);
    }
    let out = fleet.exec(
        &session_script(name, "skein-shell", "exec bash -l"),
        Duration::from_secs(120),
    )?;
    say_what_the_launcher_said(&out);
    // The anchor is a new process, so the old record addresses nothing. Re-record before anyone
    // tries to enter the namespace — that is the whole point of doing this here.
    let ns_pid = anchor_from_launch(&out)?;
    let (generation, ns_start) = stamp_anchor(&record.sandbox, name, ns_pid)?;
    record_place(
        name,
        &PlaceRecord {
            ns_pid,
            generation,
            ns_start,
            // A new namespace, made by the launcher `install_launcher` just refreshed above — so
            // the record's old cover is as dead as its old pid, and carrying it over would leave a
            // just-restarted box still asking to be restarted.
            launcher: launcher_from_launch(&out),
            ceiling: limits_from_launch(&out),
            peers: peers_from_launch(&out),
            ..record.clone()
        },
    )?;
    // The sweep just became wrong in the other direction; a stale "dead" answer would send the very
    // next caller through this again.
    LIVENESS_GATE.invalidate();
    // What was observed is that the tmux server was gone, not *why*. A cycled sandbox is the common
    // cause and the one this exists for, but it is not the only one — a killed server or an OOM'd
    // box reach here identically — and asserting it sends anyone debugging to look for a restart
    // that never happened. The tree, the private HOME and the ceiling are all intact either way.
    eprintln!("skein: {name} had no live session, so it was restarted (its work is untouched)");
    Ok(())
}

/// The sandbox user's `$HOME`, which is the HOME every command in a box must run with.
///
/// Not a private directory and not empty: `box-session.sh` binds the box's own `home` *over* this
/// path, so entering the namespace with it is what gives the box its private view. Recording an
/// empty string here instead made `Place::wrap` export `HOME=`, and every provisioning step then
/// resolved `$HOME/x` to `/x` — which is how a box tried to symlink `/shared` at the filesystem root
/// and reported "shared home unavailable" and a bare "mkdir: Permission denied".
///
/// Asked of the sandbox rather than assumed to be `/home/agent`: the image chooses the user.
fn sandbox_home(fleet: &Place) -> Result<String, String> {
    let home = fleet
        .exec("printf %s \"$HOME\"", Duration::from_secs(20))?
        .trim()
        .to_string();
    if home.is_empty() || !home.starts_with('/') {
        return Err(format!(
            "the fleet sandbox reported no usable HOME ({home:?}); every box command would run with \
             HOME unset and write to the filesystem root"
        ));
    }
    Ok(home)
}

/// How far a previous launch of this box got: does it have a checkout, and is its session alive?
///
/// One round-trip rather than two, and asked of the sandbox rather than inferred from a placement
/// record — after a failed launch the record is exactly what may be missing.
///
/// **It no longer takes a session name**, because [`session_socket_probe`] cannot ask for one and
/// no caller was spending the answer: all three passed `skein-shell`, which is the only session a
/// launcher ever starts on that socket, and what they do with a `false` is run the launcher — which
/// refuses rather than starting a second one. What the name bought was `tmux has-session`, and that
/// is a tmux client on a socket the box can replace (ISO-6).
fn box_progress(fleet: &Place, name: &str) -> Result<(bool, bool), String> {
    let out = fleet.exec(&box_progress_script(name), Duration::from_secs(30))?;
    let out = out.trim();
    Ok((out.starts_with('1'), out.ends_with('1')))
}

/// The two questions as one script, so what crosses into the sandbox can be read without one.
fn box_progress_script(name: &str) -> String {
    format!(
        "tree=0; sess=0; \
         [ -e {tree_q}/.git ] && tree=1; \
         {alive} && sess=1; \
         echo \"$tree$sess\"",
        tree_q = sh_quote(&format!("{}/tree", box_root(name))),
        alive = session_socket_probe(&box_sock(name)),
    )
}

/// **Is this box up and already provisioned for the start it is on?** — the script, so it can be
/// read without a fleet.
///
/// Two facts and both are needed. A live `skein-shell` says the session is there; a
/// `skein-startup.ready` marker carrying the CURRENT start id says provisioning finished for *this*
/// session rather than for some earlier one. `box-session.sh` writes `skein-start-id` fresh on every
/// launch and the kit suffixes its markers with it, so a marker from a previous start cannot answer
/// for this one — which is the whole reason the id exists.
///
/// Prints `ready` and nothing else. Silence is "no", and so is anything unreadable: the caller's
/// fallback is to start the box, which is what it did unconditionally before.
pub fn box_ready_script(name: &str) -> String {
    box_ready_script_in(&box_root(name))
}

/// [`box_ready_script`] with the box's root passed in rather than read off `$SKEIN_FLEET_ROOT`.
///
/// **So the test does not have to set a process-wide variable**, which is not a stylistic
/// preference here: `fleet_root()` reads an environment variable, several tests in this file read it
/// too and do not take `testutil::env_lock`, and a test that sets it is a test that can fail its
/// neighbours. That is SKEIN-471's shape and it had already been paid for twice in this file. A
/// function that takes its root cannot cause it a third time.
fn box_ready_script_in(root: &str) -> String {
    format!(
        "id=$(cat {id_q} 2>/dev/null); \
         [ -n \"$id\" ] || exit 0; \
         {alive} || exit 0; \
         [ -e {tmp_q}/skein-startup.ready.\"$id\" ] && echo ready; \
         exit 0",
        id_q = sh_quote(&format!("{root}/tmp/skein-start-id")),
        // Connecting, not `tmux has-session` — see [`session_socket_probe`]. The session's *name*
        // is not what this was asking: the marker beside it carries the current start id, which is
        // the half that says provisioning finished for THIS session.
        alive = session_socket_probe(&format!("{root}/session.sock")),
        tmp_q = sh_quote(&format!("{root}/tmp")),
    )
}

/// [`box_ready_script`], asked of the fleet. `false` whenever the answer is not a clear yes.
pub fn box_is_ready(name: &str) -> bool {
    if !valid_name(name) {
        return false;
    }
    let sandbox = fleet_sandbox();
    own_sandbox(&sandbox)
        .exec(&box_ready_script(name), Duration::from_secs(20))
        .map(|said| said.trim() == "ready")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::testkit::*;
    use crate::testutil::*;

    /// Every way of failing to match means the box is GONE, and says which way.
    ///
    /// **A restart is not a collision, and a repurpose is** — the guard, asserted in BOTH
    /// directions because only one of them is the interesting half.
    ///
    /// `start_box_inner` adopts whatever it finds, which is how a restart keeps its checkout and
    /// how a box survives a sandbox rebuild. So a guard here is one step away from breaking every
    /// restart in the fleet, and a test that only checked the refusal would not notice: it would
    /// pass just as happily if `refuse_a_repurpose` refused everything.
    ///
    /// Sabotage for the first: drop the `held.purpose == purpose` early return, and the restart
    /// below is refused. For the second: drop the comparison the other way, and the repurpose is
    /// waved through.
    #[test]
    fn a_box_is_restarted_but_never_repurposed() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // Nothing placed under this name yet: neither purpose is a collision, because there is no
        // record to disagree with. A guard that refused here would refuse every first start.
        assert!(refuse_a_repurpose("acme-feat-x", crate::place::Purpose::Manual).is_ok());
        assert!(refuse_a_repurpose("acme-feat-x", crate::place::Purpose::Review).is_ok());

        crate::place::record_place(
            "acme-feat-x",
            &PlaceRecord {
                purpose: crate::place::Purpose::Manual,
                ..Default::default()
            },
        )
        .expect("the fixture record is written");

        // The restart. Same box, same purpose — and this must stay allowed however clever the
        // guard gets.
        assert!(
            refuse_a_repurpose("acme-feat-x", crate::place::Purpose::Manual).is_ok(),
            "a restart at the same purpose was refused as a collision, which would break every \
             `skein start` on an existing box"
        );

        // The collision. Somebody's box, started as skein's own.
        let said = refuse_a_repurpose("acme-feat-x", crate::place::Purpose::Review)
            .expect_err("starting a manual box as a review box is a collision");
        assert!(
            said.contains("already a manual box") && said.contains("as a review one"),
            "the refusal has to name both purposes or it cannot be acted on: {said}"
        );
        assert!(
            said.contains("Nothing has been changed"),
            "a refusal that does not say the box is untouched reads as a half-done start: {said}"
        );

        crate::place::forget_place("acme-feat-x");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A box that was never created must say so, not be addressed as its own sandbox.
    ///
    /// **Two states now, and they used to be three.** **Placed** (a fleet box, attach normally) and
    /// **absent** (no placement — its start failed). The third was **legacy**: no placement, but a
    /// sandbox of its own that `sbx ls` knew about. Absent was being treated as legacy, so the
    /// terminal ran `sbx exec <name>`, sbx said it had never heard of the sandbox, the browser
    /// reconnected, and the real error scrolled away behind the repeat.
    ///
    /// The legacy arm asked the HOST's machine what sandboxes are on it, and nothing in here can
    /// (SKEIN-576) — so it went, and a foreign sandbox now reads as absent. What that costs a
    /// person is in `docs/parity.md` §7.
    ///
    /// **What would make this fail**: returning `None` for a name with no placement. That is the
    /// original defect — the caller reads `None` as "go ahead and try" — and `expect` below names
    /// it.
    #[test]
    fn a_box_that_was_never_created_says_so_instead_of_looping() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::fs::write(
            home.join("config.json"),
            serde_json::json!({ "fleet_sandbox": "skein-fleet" }).to_string(),
        )
        .unwrap();

        // Absent: no placement record, which is the whole of what skein has to go on.
        let why = absent_box_reason("PROJ-S6").expect("an absent box to be named as absent");
        assert!(why.contains("does not exist"), "{why}");
        assert!(why.contains("skein start PROJ-S6"), "no way forward: {why}");
        // sbx's own advice here is `sbx create`, which would build the per-VM box skein dropped.
        assert!(why.contains("Do not run `sbx create`"), "{why}");
        // And it does not claim to have asked something it cannot ask. The refusal used to be able
        // to cite `sbx ls`; a sentence offering evidence skein never gathered is the kind of
        // confident wrong answer this function exists to replace.
        assert!(
            !why.contains("sbx has no sandbox by that name"),
            "the refusal cites an `sbx ls` that was never run: {why}"
        );

        // Placed: a fleet box attaches through its placement.
        record_place(
            "placed-box",
            &crate::place::PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 42,
                home: "/home/agent".into(),
                tree: "/boxes/placed-box/tree".into(),
                sock: "/boxes/placed-box/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(absent_box_reason("placed-box").is_none());

        std::env::remove_var("SKEIN_HOME");
    }

    /// The reason a box was never created outlives the terminal that was told it.
    ///
    /// Creating a box from the cockpit runs `skein start` in a PTY. When it fails, that terminal
    /// closes with the error in it, the browser reconnects, and the fresh terminal knows only that
    /// there is no box — so it said "its last start failed, and the error came from that run rather
    /// than from this terminal", which is an admission that the answer existed and was discarded.
    /// Reported as: the box will not create, and the message is about a box that does not exist.
    #[test]
    fn the_reason_a_box_never_started_survives_the_terminal_that_was_told_it() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // sbx answers, and has no sandbox by that name: the "box does not exist" branch.
        std::env::set_var("SKEIN_LS_CMD", "echo '[]'");

        let bare = absent_box_reason("web-main").expect("an unplaced box has a reason");
        assert!(
            bare.contains("no record of a start"),
            "a box nobody tried to start must not claim a failure: {bare}"
        );

        remember_start_failure(
            "web-main",
            "cannot tell whether the fleet sandbox exists: `sbx` is not on this process's PATH",
        );
        let told = absent_box_reason("web-main").expect("still unplaced");
        assert!(
            told.contains("not on this process's PATH"),
            "the reason was recorded and then not said: {told}"
        );

        // Cleared when a start works, because a stale reason explains a failure that is over — and
        // it would be read the next time any box of that name is missing for an unrelated reason.
        forget_start_failure("web-main");
        assert_eq!(last_start_failure("web-main"), None);

        std::env::remove_var("SKEIN_LS_CMD");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A recorded reason comes back with its LAST clause, which is the half a person acts on.
    ///
    /// Asserted on the final clause rather than on a length, because a length is what went wrong:
    /// the cap was 400, `sandbox::launch_never_ran`'s not-found arm is 207 characters plus the
    /// server's whole `$PATH`, and what a reconnecting terminal got back ended mid-word with the
    /// cure missing. A number under some bound would have been just as true at 400.
    ///
    /// **The real sentence, not a stand-in.** It is asked of `launch_never_ran` so that a rewording
    /// of it is carried here rather than compared against a copy — and the length that makes this
    /// test worth anything comes from `$PATH`, which is asserted rather than assumed, because on a
    /// short `$PATH` this whole test would pass at a cap of 400 and prove nothing.
    ///
    /// `$PATH` is EXTENDED rather than replaced, and put back afterwards. A replaced `$PATH` that
    /// leaked — a panic between the set and the restore — would take every test that spawns a
    /// command with it; appended directories that do not exist cannot, however this test ends.
    #[test]
    fn a_recorded_start_failure_keeps_the_clause_that_says_what_to_do() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var(
            "PATH",
            format!("{path}{}", ":/opt/skein-no-such-dir".repeat(24)),
        );

        let why = crate::sandbox::launch_never_ran("web-main", 127)
            .expect("127 is a launch whose `skein` never ran");
        assert!(
            why.chars().count() > 400,
            "the sentence under test is shorter than the cap it overflowed, so nothing below \
             this line could fail — the `$PATH` arm was not the one taken: {why}"
        );

        remember_start_failure("web-main", &why);
        let kept = last_start_failure("web-main").expect("the reconnect has something to read");

        assert!(
            kept.ends_with("what matters is the PATH the server was started with."),
            "the cure is the last clause, and it is what the reader lost: {kept}"
        );
        assert!(
            !kept.contains('…'),
            "`clip` marks a cut with an ellipsis, so this reason was cut: {kept}"
        );
        assert_eq!(kept, why, "what was recorded is not what comes back");

        std::env::set_var("PATH", path);
        std::env::remove_var("SKEIN_HOME");
    }

    /// **Nothing in this module runs a tmux client on a socket a box can replace** (ISO-6).
    ///
    /// tmux honours `MSG_SHELL` and `MSG_EXEC` from the server unconditionally, so a client is a
    /// place a hostile server runs a command — and every socket skein used to name lives under a
    /// box's own root, which is bound read-write into that box. Three sites here spent that: the
    /// `kill-server` in [`stop_script`], and the two `has-session` probes. All three are now the
    /// cgroup write and [`session_socket_probe`], which sends nothing and so can be answered with
    /// nothing.
    ///
    /// **Asserted on the three scripts and not by scanning the source**, which was the first
    /// draft and was wrong twice over: it flagged `start_server` and `stop_serving`, whose socket
    /// is the fleet's own `server.tmux` and not any box's, and it flagged two error *messages*
    /// that tell a person which tmux command to run by hand. What the finding is about is which
    /// socket a client is pointed at, and that is a property of the script, not of the line.
    ///
    /// `Place::tmux` is a different thing again and stays: it builds the tmux a caller runs INSIDE
    /// the box's namespace, where execution landing in the box is what was asked for.
    #[test]
    fn nothing_here_attaches_to_a_boxs_own_tmux_socket() {
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). The two box
        // paths below are derived from it rather than spelled `/boxes/web-main`, so the fixture
        // stays one place; which socket a client is pointed at is the subject, not where it is.
        let _g = crate::testutil::env_lock();
        let dir = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", dir.join("fleet"));
        let record = crate::place::PlaceRecord {
            sandbox: "skein-fleet".into(),
            ns_pid: 4242,
            sock: box_sock("web-main"),
            generation: "boot-a".into(),
            ns_start: 900,
            ..Default::default()
        };
        for (what, script) in [
            ("the stop", stop_script("web-main", &record)),
            ("the launch probe", box_progress_script("web-main")),
            (
                "the readiness probe",
                box_ready_script_in(&box_root("web-main")),
            ),
        ] {
            assert!(
                !script.contains("tmux"),
                "{what} runs a tmux client on a socket under the box's own read-write root, so a \
                 box that puts a rogue server there runs a command at fleet scope on the next \
                 stop or probe (tmux honours MSG_SHELL/MSG_EXEC from the server):\n{script}"
            );
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **The advice matches the failure, or there is none.**
    ///
    /// One sentence used to be printed for every provisioning failure — *"if it keeps timing out,
    /// something in the sandbox's apt is stuck"* — and it is only true of a timeout. See
    /// [`what_to_look_at`] for what it cost.
    ///
    /// **What would make this fail:** going back to one sentence for everything, which passes the
    /// timeout row and fails both others; or dropping the killed case, which is the one that was
    /// wrong in the field.
    #[test]
    fn the_advice_after_a_failed_provisioning_matches_what_happened() {
        let killed =
            what_to_look_at("fleet agent: command was killed by SIGTERM — it did not fail");
        assert!(
            !killed.contains("apt"),
            "a killed script is still blamed on apt: {killed}"
        );
        assert!(
            killed.contains("stopped from outside"),
            "the one thing a signal tells the reader is not said: {killed}"
        );

        let slow = what_to_look_at("the command did not finish in time (900s)");
        assert!(
            slow.contains("apt"),
            "a real timeout lost the advice that was right for it: {slow}"
        );

        // Neither, and that is a third answer rather than a default to one of the two.
        assert_eq!(
            what_to_look_at("bwrap: setting up uid map: Permission denied"),
            "",
            "a failure this cannot classify was given advice about a mechanism with no part in it"
        );
    }

    /// **A box is "ready" only for the start it is actually on.**
    ///
    /// [`box_ready_script`] is what lets `reviewbox::open_at` skip `start_box` for a box that is
    /// already up, and the whole value of skipping is that provisioning is the cost of a start. So
    /// the two ways this can be wrong are opposite and both expensive: answering yes for a box that
    /// was never provisioned leaves a review box with no kit and no hooks, and answering yes off a
    /// marker from an EARLIER start does the same to a box that has since been restarted.
    ///
    /// Run against a real tmux session and real marker files rather than asserted on the string —
    /// the third row is the one a string test would have missed, because the id is interpolated at
    /// run time and reading the script cannot tell whether it matched.
    ///
    /// **What would make each row fail**, in order: the first is the half that must NOT refuse, and
    /// a guard that never says yes costs the whole fix; dropping the marker check says yes to an
    /// unprovisioned box; matching the marker without the id says yes to a box whose provisioning
    /// belongs to a start that is over; dropping `has-session` says yes to a box that is not
    /// running at all.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_box_is_ready_only_for_the_start_it_is_on() {
        if std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            crate::testutil::skip("no tmux here");
            return;
        }
        // **No environment at all**, which is why `box_ready_script_in` takes its root: several
        // tests in this file read `$SKEIN_FLEET_ROOT` without `env_lock`, so a test that set it
        // would be a test that fails its neighbours. Twice paid for here already.
        let dir = crate::testutil::tempdir();
        let root = dir.join("ready-probe");
        let tmp = root.join("tmp");
        std::fs::create_dir_all(&tmp).expect("box tmp");
        let id = "20260903090000-1234";
        std::fs::write(tmp.join("skein-start-id"), format!("{id}\n")).expect("start id");

        let sock = root.join("session.sock").display().to_string();
        let tmux = |args: &[&str]| {
            std::process::Command::new("tmux")
                .args(["-S", &sock])
                .args(args)
                .output()
                .expect("tmux")
        };
        let _ = tmux(&["new-session", "-d", "-s", "skein-shell", "sleep", "120"]);
        let asked = || {
            String::from_utf8_lossy(
                &std::process::Command::new("sh")
                    .arg("-c")
                    .arg(box_ready_script_in(&root.display().to_string()))
                    .output()
                    .expect("sh")
                    .stdout,
            )
            .trim()
            .to_string()
        };

        let marker = tmp.join(format!("skein-startup.ready.{id}"));
        std::fs::write(&marker, "").expect("marker");
        assert_eq!(
            asked(),
            "ready",
            "a box that is up and provisioned for this very start is not recognised, so every \
             reading goes on paying for a provisioning pass it does not need"
        );

        std::fs::remove_file(&marker).expect("drop the marker");
        assert_eq!(
            asked(),
            "",
            "a box whose provisioning never finished was called ready, which is a review box with \
             no kit and no hooks"
        );

        std::fs::write(tmp.join("skein-startup.ready.20260101000000-1"), "").expect("stale marker");
        assert_eq!(
            asked(),
            "",
            "a marker from an earlier start answered for this one — the id exists precisely so it \
             cannot"
        );

        std::fs::write(&marker, "").expect("marker again");
        let _ = tmux(&["kill-session", "-t", "skein-shell"]);
        assert_eq!(
            asked(),
            "",
            "a box with no session was called ready, so `open_at` would skip the start that is the \
             only thing able to give it one"
        );
        let _ = tmux(&["kill-server"]);
    }

    // A box's turn state describes a SESSION, and the store it is written to outlives the box. So a
    // migrated box came up reading `ended` — the old sandbox's agent died on the way out, its
    // SessionEnd hook recorded that faithfully, and the new box inherited it and looked terminated
    // while sitting there alive. What the box did stays; what it is doing right now does not.
    #[test]
    fn a_new_session_does_not_inherit_the_previous_ones_turn_state() {
        use std::fs;
        let store_tmp = tempdir();
        let store = store_tmp.join("store").join(".claude");
        for dir in ["status", "sessions", "hook-log", "telemetry"] {
            fs::create_dir_all(store.join(dir)).unwrap();
        }
        let repo = Repo {
            read_prs: false,
            id: "bridge".into(),
            source: String::new(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        };
        let write = |rel: &str, body: &str| fs::write(store.join(rel), body).unwrap();
        write(
            "status/bridge-main.json",
            r#"{"status":"ended","detail":"session ended: other"}"#,
        );
        write("status/bridge-main.agents", "2");
        write(
            "sessions/bridge-main.json",
            r#"{"lastMessage":"shipped it"}"#,
        );
        write("hook-log/bridge-main.jsonl", "{\"event\":\"ended\"}\n");
        write("telemetry/bridge-main.jsonl", "{\"total\":1}\n");
        // Another box's state must be untouched: these all live in one shared directory.
        write("status/other-box.json", r#"{"status":"working"}"#);

        forget_turn_state(&repo, "bridge-main");

        assert!(
            !store.join("status/bridge-main.json").exists(),
            "a dead session's last word must not greet the new one"
        );
        assert!(
            !store.join("status/bridge-main.agents").exists(),
            "the counter counts processes that died with the old session"
        );
        for kept in [
            "sessions/bridge-main.json",
            "hook-log/bridge-main.jsonl",
            "telemetry/bridge-main.jsonl",
            "status/other-box.json",
        ] {
            assert!(store.join(kept).exists(), "{kept} is history, not a claim");
        }

        // A box being created for the first time has none of this, and that is not an error.
        forget_turn_state(&repo, "brand-new");
    }

    // Every argument is quoted: a branch like `feat/auth` or a name with a space must reach the
    // launcher whole, and the launcher does its own refusing from there.
    #[test]
    fn starting_a_box_hands_the_launcher_quoted_arguments() {
        // Takes the env lock and pins its own SKEIN_HOME: `session_script` reads the box's cgroup
        // limits out of the config, so without this it can be handed another test's home mid-run
        // and fail on an assertion about a string it never built. Latent for a long time; it only
        // started firing once there were more env-setting tests to race with.
        let _g = env_lock();
        std::env::set_var("SKEIN_HOME", tempdir());
        // The root is pinned at the shipped default rather than at a fixture, because the three
        // assertions below quote `/boxes` paths literally — the launcher's own argv, spelled the
        // way the sandbox receives it. A fixture root would move the subject and they would all
        // have to be rewritten against `fleet_root()`, comparing the script with the function that
        // built it. `util::fleet_root` refuses an unpinned test (SKEIN-690); nothing here opens a
        // path.
        std::env::set_var("SKEIN_FLEET_ROOT", "/boxes");
        let script = session_script("web-main", "skein-agent", "claude --continue");
        assert!(
            script.contains("'/boxes/.skein/box-session.sh' 'web-main'"),
            "{script}"
        );
        // The shared ceilings lead, in the environment: an already-installed launcher that knows
        // nothing about them ignores a variable, where it would have read an argument as part of
        // the agent's command line.
        assert!(
            script.starts_with("SKEIN_FLEET_LIMITS="),
            "the ceilings must not be positional: {script}"
        );
        assert!(script.contains("'/boxes/web-main' '/boxes/web-main/anchor.pid' 'skein-agent'"));
        // The host-side state dir the box binds its conversation from — a HOST path, not a /boxes
        // one, because the point of it is to outlive the sandbox that /boxes lives in.
        assert!(
            script.contains(&format!("'{}'", box_state("web-main"))),
            "the box must be told where its durable state lives: {script}"
        );
        assert!(
            !box_state("web-main").starts_with("/boxes"),
            "box state on VM-local disk would defeat the entire point"
        );
        assert!(
            script.ends_with("bash -lc 'claude --continue'"),
            "the agent command stays one argument: {script}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// What a settings panel is told about a box's cover is what its launcher will actually do.
    ///
    /// [`box_exposure`] and [`mount_manifest`] decide the same thing in two places — the panel's
    /// word and the launcher's manifest — and the failure that matters is them disagreeing, because
    /// only one of the two is enforced. Asserted over a real `repos.json` rather than argued from
    /// the two function bodies, and in **both** directions: a matched box has to come out covered
    /// AND hold a manifest, an unmatched one uncovered AND empty. Only one direction would pass
    /// against a `box_exposure` that answered `Uncovered` for everything.
    ///
    /// **What would make this fail**: changing either side's condition without the other —
    /// `repo_for_box(name).is_none()` here, `mount_manifest`'s opening `if` there.
    #[test]
    fn what_a_panel_is_told_about_a_cover_is_what_the_launcher_will_do() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        let store = home.join("store");
        std::fs::create_dir_all(&store).unwrap();
        crate::repos::save_repos(&[crate::repos::Repo {
            read_prs: false,
            id: "web".into(),
            source: "https://example.invalid/web.git".into(),
            store: store.to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }])
        .unwrap();

        assert_eq!(
            box_exposure("web-main"),
            Exposure::Covered,
            "a box whose name matches a registered repo reads as uncovered, so the cockpit would \
             warn about every ordinary box and the warning would stop meaning anything"
        );
        assert!(
            !mount_manifest("web-main").is_empty(),
            "the panel and the launcher disagree: `covered`, over an empty manifest"
        );

        assert_eq!(
            box_exposure("adrift-main"),
            Exposure::Uncovered,
            "a box matching no repository reads as covered — which is the state SKEIN-836 found, \
             where nothing anywhere distinguished it from a covered box"
        );
        assert!(
            mount_manifest("adrift-main").is_empty(),
            "the panel and the launcher disagree: `uncovered`, over a manifest with mounts in it"
        );

        // And the switch wins over both, because that state was chosen. Written through
        // `set_box_privileged`, so this also proves the two read the same declared file.
        set_box_privileged("adrift-main", true).unwrap();
        assert_eq!(
            box_exposure("adrift-main"),
            Exposure::Workshop,
            "the workshop box is reported as an accident, which is the one distinction the two \
             banners exist to keep"
        );
        assert_eq!(box_exposure("adrift-main").spelled(), "workshop");
    }

    /// Every `timeout <n>` and `-lt <n>` in the provisioning script, summed.
    ///
    /// Bounds are written two ways and both have to be readable here. `timeout 120 sudo apt-get …`
    /// states its number; `timeout "$sync_budget" bash …` names one, because the same number is
    /// handed to the script it bounds and a second literal would be a second place to update. So the
    /// plain `name=<n>` assignments are resolved first, and a bound this cannot read is an ERROR
    /// rather than a zero — a silently unread bound is a bound that is not checked, which is the
    /// whole failure this test exists to end.
    fn bounds_in(script: &str) -> Result<u64, String> {
        let mut known: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
        for line in script.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            if let Some((name, value)) = trimmed.split_once('=') {
                let named =
                    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if named {
                    if let Ok(n) = value.trim().parse::<u64>() {
                        known.insert(name, n);
                    }
                }
            }
        }
        let mut allows = 0u64;
        for line in script.lines() {
            if line.trim().starts_with('#') {
                continue;
            }
            let mut words = line.split_whitespace().peekable();
            let mut before: Option<&str> = None;
            while let Some(word) = words.next() {
                let previous = before;
                before = Some(word);
                if word != "timeout" {
                    continue;
                }
                // `command -v timeout` asks whether the tool exists and bounds nothing. That is the
                // ONLY exemption, and it is spelled as narrowly as it is on purpose: the first
                // version of this test allowed any word that did not look like command position,
                // and quietly dropped `SKEIN_SYNC_BUDGET=$((sync_budget - 30)) timeout …` because
                // the arithmetic tokenised badly. A guard against unread bounds that skips the ones
                // it cannot parse is the bug wearing the fix's clothes. Everything else reaches the
                // error below.
                if previous == Some("-v") {
                    continue;
                }
                // `timeout -k <n> <bound>`: the kill-after is time the script may ALSO spend —
                // SIGTERM at `<bound>`, SIGKILL `<n>` later — so it is added rather than skipped.
                // Skipping it would be the same bug the exemption above is written against: a
                // guard against unread bounds that quietly drops the ones it cannot parse.
                if words.peek().copied() == Some("-k") {
                    words.next();
                    let Some(after) = words.next() else { continue };
                    match after.trim_matches('"').parse::<u64>() {
                        Ok(n) => allows += n,
                        Err(_) => {
                            return Err(format!(
                                "`timeout -k {after}` — this test cannot read that kill-after, so \
                                 the total below is short by however long it is.\n  {}",
                                line.trim()
                            ))
                        }
                    }
                }
                let Some(raw) = words.peek().copied() else {
                    continue;
                };
                let bare = raw
                    .trim_matches('"')
                    .trim_start_matches('$')
                    .trim_matches(|c| c == '{' || c == '}');
                if let Ok(n) = bare.parse::<u64>() {
                    allows += n;
                } else if let Some(n) = known.get(bare) {
                    allows += n;
                } else {
                    return Err(format!(
                        "`timeout {raw}` — this test cannot read that bound, so it is not counted \
                         and the total below is short by however long it is. Write the number, or \
                         set it as `name=<seconds>` in this script.\n  {}",
                        line.trim()
                    ));
                }
            }
            // `while apt_busy && [ "$waited" -lt 240 ]` — the wait before it tries at all.
            if line.contains("waited") {
                if let Some((_, rest)) = line.split_once("-lt ") {
                    if let Some(n) = rest
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse::<u64>().ok())
                    {
                        allows += n;
                    }
                }
            }
        }
        Ok(allows)
    }

    /// The provisioning deadline outlasts everything the provisioning script allows itself.
    ///
    /// **Read out of the script, not restated here.** A deadline shorter than the callee's own
    /// budget turns its answer into silence, and that is not hypothetical: the caller allowed 300s
    /// while `skein-startup.sh` allows itself 600 — 240 waiting for the agent image's background
    /// `apt` rather than racing it, then 120 to install, 120 to update, 120 to install again. A box
    /// that started while apt was busy waited four minutes BY DESIGN, was killed at five, and the
    /// start failed. It reads as a hung restart on a fleet where everything is working.
    ///
    /// Summing every bound is deliberately conservative — some are alternatives on one path — and
    /// conservative is the right direction: being generous costs a start that takes longer to fail,
    /// being tight costs this bug.
    #[test]
    fn the_provisioning_budget_outlasts_the_script() {
        let allows = bounds_in(KIT_STARTUP_SH).unwrap_or_else(|why| panic!("{why}"));
        assert!(
            allows > 0,
            "no bounded waits found in the provisioning script, so this test checks nothing — the \
             shapes it reads (`timeout <n>` and `-lt <n>`) must have changed"
        );
        assert!(
            PROVISION_BUDGET.as_secs() > allows,
            "provisioning is given {}s and the script allows itself {allows}s. The shorter deadline \
             wins, so the script is killed part-way through work it was told it had time for, and \
             the start fails on a fleet where nothing is wrong.",
            PROVISION_BUDGET.as_secs()
        );
    }

    /// The three deadlines a box start depends on are in the one order that works.
    ///
    /// A start runs three clocks, and any pair inverted kills a start that was going to succeed:
    ///
    ///   the script's own bounds  <  the provisioning deadline  <  the agent launch's setup wait
    ///
    /// Both inversions have happened. `PROVISION_BUDGET` was 300s against a script allowing 600, and
    /// `INITIAL_SETUP_WAIT` was 600s against a budget of 900 — and the second is worse than it looks,
    /// because that wait OPENS FIRST: `start_box_inner` starts the session, reads the anchor, records
    /// the placement, and provisions last, so the setup wait is already running down before
    /// provisioning begins. Equal is not good enough; each has to outlast the one inside it.
    #[test]
    fn each_deadline_a_start_depends_on_outlasts_the_one_inside_it() {
        let script = bounds_in(KIT_STARTUP_SH).unwrap_or_else(|why| panic!("{why}"));
        let provisioning = PROVISION_BUDGET.as_secs();
        let setup_wait = setup_wait_secs();
        assert!(
            script < provisioning && provisioning < setup_wait,
            "the script allows itself {script}s, provisioning is given {provisioning}s, and the \
             agent launch waits {setup_wait}s for the marker provisioning writes. They have to \
             increase in that order — the innermost clock is the one doing the work, and whichever \
             of the outer two fires first turns its answer into a failure on a healthy fleet."
        );
    }

    /// The launcher's start-id block, lifted rather than restated — a copy here could mint ids the
    /// real launcher does not.
    fn launcher_writes_start_id(tmp: &std::path::Path) -> String {
        let lines: Vec<&str> = BOX_SESSION_SH.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.starts_with("start_id=\"$(date"))
            .expect("the start-id block moved");
        let block = lines[at..at + 2].join("\n");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -uo pipefail; tmp={}; {block}",
                crate::util::sh_quote(&tmp.to_string_lossy())
            ))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the launcher's start-id block failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read_to_string(tmp.join("skein-start-id"))
            .expect("the launcher wrote no start id")
            .trim()
            .to_string()
    }

    /// The kit's marker preamble — the same lines the real provisioning computes its handshake
    /// paths with — run to the point of touching whichever marker the scenario needs.
    fn kit_marks(dir: &std::path::Path, then: &str) {
        let lines: Vec<&str> = KIT_STARTUP_SH.lines().collect();
        let from = lines
            .iter()
            .position(|l| l.starts_with("markers=\"$(printenv"))
            .expect("the kit's marker preamble moved");
        let to = lines[from..]
            .iter()
            .position(|l| l.starts_with("rm -f \"$markers\""))
            .map(|i| from + i)
            .expect("the kit's marker cleanup moved");
        let block = lines[from..=to].join("\n");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("set -uo pipefail; {block}\n{then}"))
            .env("SKEIN_STARTUP_MARKERS", dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the kit's marker preamble failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The agent launch's wait, run for real against `dir` standing in for the box's /tmp.
    fn setup_wait(dir: &std::path::Path, secs: u64) -> (i32, String) {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(setup_wait_script(&dir.to_string_lossy(), secs))
            .output()
            .unwrap();
        let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
        said.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), said)
    }

    /// The startup handshake guards a restart, not only a box's first start.
    ///
    /// A box's /tmp is `$root/tmp` on disk and a restart keeps it, so both markers of the previous
    /// start are still there when the next start's setup wait opens — and that wait opens BEFORE
    /// provisioning runs (see `each_deadline_a_start_depends_on_outlasts_the_one_inside_it`).
    /// Observed live as the bare-name failure this drives: a box whose provisioning timed out came
    /// up "working", its wait satisfied by the ready marker of the start before.
    ///
    /// Two consecutive starts, three parties, all running their real lines: the launcher mints the
    /// id, the kit writes the markers, the wait reads them.
    #[test]
    fn a_restarted_boxs_agent_launch_waits_for_its_own_starts_provisioning() {
        let dir = tempdir();

        // Start 1 runs to completion — and, separately, leaves a failure marker behind, which is
        // what a start whose provisioning was killed leaves via the kit's EXIT trap.
        let first = launcher_writes_start_id(&dir);
        kit_marks(&dir, "touch \"$startup_ready\" \"$startup_failed\"");
        assert!(
            dir.join(format!("skein-startup.ready.{first}")).exists(),
            "the kit did not suffix its markers with the start id"
        );

        // Start 2: the launcher has run, provisioning has not. This is the window the wait opens
        // in, with both of start 1's markers still on disk.
        let second = launcher_writes_start_id(&dir);
        assert_ne!(first, second, "two starts minted the same id");
        let (code, said) = setup_wait(&dir, 2);
        assert!(
            !said.contains("setup failed"),
            "start 1's failure marker refused a start it knows nothing about: {said}"
        );
        assert_eq!(
            (code, said.contains("timed out")),
            (1, true),
            "start 1's ready marker satisfied start 2's wait before its provisioning ran: {said}"
        );

        // Start 2's provisioning completes, and the same wait now passes.
        kit_marks(&dir, "touch \"$startup_ready\"");
        let (code, said) = setup_wait(&dir, 5);
        assert_eq!(
            code, 0,
            "this start's own ready marker was not honoured: {said}"
        );

        // A failure of THIS start still refuses — the id must not make failed markers decorative.
        kit_marks(&dir, "touch \"$startup_failed\"");
        let (code, said) = setup_wait(&dir, 5);
        assert_eq!(
            (code, said.contains("setup failed")),
            (1, true),
            "this start's own failure marker was ignored: {said}"
        );

        // And with no id at all — a per-VM sandbox, whose /tmp dies with it — the bare names still
        // carry the handshake, in both directions.
        let bare = tempdir();
        std::fs::write(bare.join("skein-startup.ready"), "").unwrap();
        let (code, _) = setup_wait(&bare, 2);
        assert_eq!(code, 0, "the bare ready marker stopped meaning ready");
        std::fs::write(bare.join("skein-startup.failed"), "").unwrap();
        let (code, said) = setup_wait(&bare, 2);
        assert_eq!(
            (code, said.contains("setup failed")),
            (1, true),
            "the bare failure marker stopped refusing: {said}"
        );
    }

    /// **A tracker install does not stop the box coming up, and does not slow it down either.**
    ///
    /// This is the last block of provisioning, and its comment has always said "a box with no
    /// tracker is not a broken box, so this can never gate startup". Nothing made that true, twice
    /// over.
    ///
    /// First it FAILED starts: the script it runs reached GitHub over ssh with no bound, on a fleet
    /// with no token that clone sat on a credential prompt, provisioning was killed at its deadline,
    /// and the kill landed BEFORE `touch /tmp/skein-startup.ready` — so the EXIT trap wrote
    /// `startup_failed` and the next agent launch read a fully provisioned box as one whose setup
    /// had failed. The box worked. Nothing could start in it. Bounding the block fixed that.
    ///
    /// Then it COST them, and the bound is why nobody saw it: this test used to shorten
    /// `sync_budget=240` to 2 before running it, so it proved the block was bounded and never once
    /// asked what the bound was worth. On a live fleet, 2026-09-03, nine boxes out of nine
    /// spent 240s here — every one within a second of the others, on trees from 130 MB to 1.5 GB —
    /// and not one of them had an artifact to show for it. Four minutes per box, for nothing.
    ///
    /// So the budget is left ALONE now and the clock is the assertion. The stand-in hangs for 45s
    /// against a 240s bound: a block that waits for it takes 45s, and a block that has been put
    /// behind the marker and detached takes none of them.
    ///
    /// **What would make this fail:** dropping the `&` (or the redirections and `setsid` that make
    /// it real — the fleet agent reads to EOF, so a child still holding the pipe blocks the create
    /// exactly as before) takes the elapsed time to 45s; moving the block back above
    /// `touch "$startup_ready"` fails the ordering assertion, which is the half that decides
    /// whether a killed tracker can still condemn a working box.
    ///
    /// Linux because `timeout(1)` is GNU coreutils — and a box is Linux, which is why the script may
    /// depend on it at all.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_tracker_install_that_hangs_does_not_hold_up_the_box() {
        // The marker first. Asserted on the real script, because the whole failure this block has
        // had twice is about which side of that line it falls on.
        // `rfind`, and the reason is a sabotage that this test survived when it should not have:
        // the direct-mode early exit near the top writes the same marker, so `find` matched THAT
        // one and the comparison held however far down the real write moved. The marker this test
        // is about is the last one — the end of provisioning.
        let ready_at = KIT_STARTUP_SH
            .rfind(r#"touch "$startup_ready""#)
            .expect("the ready marker is written somewhere");
        let sync_at = KIT_STARTUP_SH
            .rfind("sync_install=")
            .expect("the tracker block is still here");
        assert!(
            ready_at < sync_at,
            "the tracker wiring runs before the box is marked ready, so anything that kills it \
             mid-way leaves a fully provisioned box looking like a failed one"
        );

        // The block, lifted from the script rather than restated — a copy here would pass while the
        // real one hung.
        let block: String = KIT_STARTUP_SH
            .lines()
            .skip_while(|l| !l.starts_with("sync_install="))
            .take_while(|l| *l != "fi")
            .chain(std::iter::once("fi"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            block.contains("sync_budget") && block.contains("timeout"),
            "the tracker block is not the shape this test lifts:\n{block}"
        );

        let home = crate::testutil::tempdir();
        let dir = home.as_ref() as &std::path::Path;
        // Where the kit finds its helpers: skein's read-only copies, not the store (SKEIN-1149).
        let bin = dir.join("probe");
        std::fs::create_dir_all(&bin).expect("probe dir");
        // Hangs, the way a git clone waiting on a credential prompt hangs.
        std::fs::write(
            bin.join("sync-install.sh"),
            "#!/usr/bin/env bash\nsleep 45\n",
        )
        .expect("the stand-in script");

        // The budget is NOT shortened. That edit is what hid the cost for as long as it did.
        let script = format!(
            "skein_probe={p}\nmarkers={d}\n{block}\n",
            p = sh_quote(&bin.display().to_string()),
            d = sh_quote(&dir.display().to_string())
        );

        let began = std::time::Instant::now();
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("bash");
        let took = began.elapsed();

        assert!(
            out.status.success(),
            "the tracker block failed the startup it is not allowed to gate: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            took < std::time::Duration::from_secs(15),
            "the tracker block took {took:?} waiting for a stand-in that hangs for 45s, so every \
             box creation pays for it — which is what nine boxes at 240s apiece looked like"
        );
        assert!(
            dir.join("skein-sync.log").exists(),
            "no log, so the wiring was not started at all — detaching it must not become skipping \
             it, or a box silently stops coming up with a tracker"
        );
    }

    /// The repairs that asked `sbx ls` about the host stop skipping themselves inside (SKEIN-469).
    ///
    /// `sbx::fleet_boxes` answers `None` in-fleet on purpose — `sbx ls` is a question about the
    /// HOST's machine, which nothing in here can reach. Five callers read that `None` as "I could
    /// not see the fleet" and declined to act, which is the right reflex for a wedged daemon and
    /// exactly wrong for a deployment where the answer is knowable without asking: **this process is
    /// running inside the sandbox it is asking about.**
    ///
    /// What it cost, before this: `heal_fleet` skipped the launcher, the in-sandbox agent and the
    /// docker config on every server start; the agent's own repair tick retired its watcher; doctor
    /// printed its own live sandbox as "cannot tell if it exists"; and `volume::migrate`'s
    /// refusal stopped firing, so a volume could be moved out from under running boxes.
    ///
    /// The half that keeps this honest is the SECOND assertion. "It exists because I am in it" is
    /// only true of the one sandbox this process stands in; for any other name there is no way to
    /// see from in here, so `None` is the truth and `Some(false)` would be a guess dressed as an
    /// answer.
    #[test]
    fn a_repair_that_cannot_ask_sbx_asks_where_it_is_running_instead() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // An `sbx ls` that answers nothing, so anything still consulting it cannot pass by luck.
        std::env::set_var("SKEIN_LS_CMD", "exit 1");

        let mine = fleet_sandbox();
        assert_eq!(
            fleet_exists(&mine),
            Some(true),
            "in-fleet skein could not tell whether the sandbox it is running inside exists"
        );
        assert_eq!(
            fleet_exists("some-other-sandbox"),
            None,
            "in-fleet skein claimed to know about a sandbox it has no way to see"
        );

        // A box with no placement record: the failed-start case this refusal exists for. It used to
        // return `None` here, which the caller reads as "go ahead and try" — and the terminal then
        // reconnected for ever behind the real error.
        let said = absent_box_reason("never-placed")
            .expect("in-fleet, a box with no placement got no refusal at all");
        assert!(
            !said.contains("sbx has no sandbox by that name"),
            "the refusal cites an `sbx ls` that was never run: {said}"
        );

        // The host arm used to sit here: with `sbx` unable to answer, `None` had to survive, or a
        // wedged daemon would read as "absent" and skein would try to create a sandbox that already
        // existed. There is no host and no `sbx ls` to wedge (SKEIN-576) — and the `None` that arm
        // protected is now the answer for every name but this fleet's, which the second assertion
        // above holds shut.

        std::env::remove_var("SKEIN_LS_CMD");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A box's agent is handed the scratch path as a VALUE, from the one constant that defines it.
    ///
    /// The launcher joins it to the box's own HOME, which is the only thing that can: `$HOME` there
    /// is the private home bwrap binds over the sandbox's, and no shell out here can name it. So
    /// what this pins is that the path is not spelled out a second time in the shell script — the
    /// end-to-end proof that the environment really carries it is
    /// `tests/fleet_launch/lifecycle.rs::a_box_lives_and_dies_inside_the_fleet_sandbox`, under real
    /// bwrap.
    #[test]
    fn a_box_is_handed_the_scratch_path_rather_than_left_to_derive_one() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). Nothing
        // asserted below carries the root, so a fixture is the whole of what this needs.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        let script = session_script("web-main", "skein-agent", "claude");
        assert!(
            script.contains(&format!("SKEIN_MODEL_SCRATCH={}", sh_quote(MODEL_SCRATCH))),
            "a box start does not carry the scratch path, so its agent derives one from the \
             sandbox's shared /tmp: {script}"
        );
        // And the sandbox's own NAME, for the same reason and over the same channel: a box cannot
        // work it out (no `sbx` on its PATH, no sandbox config mounted in), so the blocked-egress
        // hint in the git shim can only print a pasteable `sbx policy allow network --sandbox <name>`
        // if the name travels. Pinned as the literal default rather than as
        // `sh_quote(place::fleet_sandbox())`, which would compare the script with the function that
        // built it and pass however wrong both were.
        assert!(
            script.contains("SKEIN_FLEET_NAME='skein-fleet'"),
            "a box start does not carry the sandbox's name, so the git shim cannot finish the \
             `sbx policy allow network --sandbox …` command it tells a person to run: {script}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// **The third surface of the same wall points at the same way through** (SKEIN-707).
    ///
    /// A box whose repo's store the sandbox cannot see is refused by [`start_box_inner`], and that
    /// refusal used to end "Rebuild the sandbox with the mounts it needs: `skein resize <memory>`
    /// … It carries every existing box across", above a line forbidding `sbx rm`. Every clause of
    /// that was wrong by then: `skein resize` refuses through [`fleet_lifecycle_refusal`] before it
    /// reaches any work, the phases that carried boxes across are deleted (SKEIN-679), and the
    /// command it warned against is the first half of the only thing that works. One person, one
    /// wall, three surfaces — and this was the surface still giving the old answer, found in a
    /// test's stdout rather than by anybody reading it.
    ///
    /// **Read out of the source**, because reaching this arm for real means a configured fleet and
    /// a `Place::exec` that answers — and what is being asserted is which *message* the arm
    /// composes, which is a property of what the function is allowed to contain.
    ///
    /// **What would make this fail**: putting `skein resize` back as the instruction, or writing a
    /// second way out here instead of delegating to the refusal the other two surfaces print.
    /// Proved — restoring the old three lines fired the first assertion.
    #[test]
    fn the_store_mount_refusal_points_at_the_host_rather_than_at_a_resize_that_refuses() {
        let body = code_of(fn_body(include_str!("start.rs"), "fn start_box_inner("));
        assert!(
            !body.contains("`skein resize"),
            "a box refused for a missing mount is told to run `skein resize`, which refuses \
             (SKEIN-679) and no longer carries anything across:\n{body}"
        );
        assert!(
            body.contains("fleet_lifecycle_refusal("),
            "this refusal writes its own way out instead of printing the one the cockpit and the \
             CLI print — three messages about one wall is how the stale one survived:\n{body}"
        );
        // And it must not argue with the message it just printed. The refusal hands over
        // `sbx rm -f` as step one, with what it costs and the save to take first; a "Do NOT
        // `sbx rm`" beside it steers somebody off the only act that works.
        assert!(
            !body.contains("sbx rm"),
            "the refusal forbids the command the message beside it instructs:\n{body}"
        );
    }
}
