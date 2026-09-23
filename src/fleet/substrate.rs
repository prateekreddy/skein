//! The sandbox's substrate: its base packages, the runtimes on it, and updating them.

use super::*;

/// The provisioning script itself, at module scope so it can be asserted on without a sandbox.
///
/// apt's output is kept, not discarded: when this step fails it is the only thing that says whether
/// the mirror was unreachable, sudo refused, or the package simply isn't there — and "missing
/// required tools: tmux" with the reason thrown away is a dead end.
const SUBSTRATE_SCRIPT: &str = r#"need='';
         command -v tmux >/dev/null 2>&1 || need="$need tmux";
         command -v jq   >/dev/null 2>&1 || need="$need jq";
         # What this fleet's owner has approved, replayed. A sandbox is rebuilt from an image that
         # knows nothing about it, so without this a rebuild silently comes back missing packages
         # someone already said yes to — and every box starts asking for them again.
         #
         # Kept apart from `need` deliberately: `need` is command-checked at the end, and an
         # approved package need not be a command at all. `libnss3` installs perfectly and would
         # still read as missing, failing a launch over a package that is actually there.
         extra='';
         for p in $SKEIN_APPROVED_APT; do
           dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'ok installed' || extra="$extra $p";
         done;
         # The agent runtimes are substrate too. The `shell` image has neither, and an agent image
         # would only ever carry one of them — so they are installed once, into the sandbox, and
         # every box in it shares them. Measured on a real sandbox: 6s and 3s. What stays per box is
         # the state, which box-session.sh keeps private; only the binaries are shared.
         npm="$SKEIN_RUNTIME_PACKAGES";
         command -v bwrap >/dev/null 2>&1 || { echo 'skein: this sandbox image has no bwrap; boxes cannot be isolated in it' >&2; exit 1; };
         log=/tmp/skein-substrate.log;
         want=""; for p in $npm; do
           case "$p" in
             *claude-code) command -v claude >/dev/null 2>&1 || want="$want $p" ;;
             *codex)       command -v codex  >/dev/null 2>&1 || want="$want $p" ;;
             *)            want="$want $p" ;;
           esac;
         done;
         # Approved npm packages, asked of npm itself rather than of $PATH: a global package need
         # not put a command on it, so `command -v` would reinstall it on every single launch.
         for p in $SKEIN_APPROVED_NPM; do
           npm ls -g --depth=0 "$p" >/dev/null 2>&1 || want="$want $p";
         done;
         npm="$want";
         apt_want="$need$extra";
         [ -n "$apt_want" ] || [ -n "$npm" ] || exit 0;
         [ -n "$apt_want" ] || { timeout 300 sudo npm install -g $npm >>"$log" 2>&1 || true; exit 0; };
         # A freshly created sandbox is still running its own first-boot apt, and apt refuses to run
         # twice. Outlast it rather than failing the launch on a race: measured on a real rebuild,
         # where the retry landed on "Could not get lock ... held by process 281 (apt-get)".
         waited=0;
         while [ "$waited" -lt 120 ]; do
           if sudo fuser /var/lib/dpkg/lock-frontend /var/lib/apt/lists/lock >/dev/null 2>&1; then
             sleep 3; waited=$((waited + 3));
           else break; fi;
         done;
         # update FIRST. A fresh image ships an empty index, where install reports "Package 'tmux'
         # has no installation candidate" — which reads as a missing package and is a missing index.
         { timeout 180 sudo apt-get update -qq; \
           timeout 240 sudo apt-get install -y -qq $apt_want \
             || { sleep 5; timeout 180 sudo apt-get update -qq; \
                  timeout 240 sudo apt-get install -y -qq $apt_want; }; } >"$log" 2>&1;
         if [ -n "$npm" ] && command -v npm >/dev/null 2>&1; then
           timeout 300 sudo npm install -g $npm >>"$log" 2>&1 || true;
         fi;
         missing=''; for t in $need; do command -v "$t" >/dev/null 2>&1 || missing="$missing $t"; done;
         [ -z "$missing" ] || {
             echo "skein: the fleet sandbox is missing required tools:$missing";
             echo "skein: apt said (tail of $log inside the sandbox):";
             tail -n 25 "$log" | sed 's/^/  | /';
             exit 1;
         } >&2"#;

/// An agent CLI the sandbox could be running a newer version of.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeUpdate {
    /// `claude`, `codex` — the runtime as a person names it.
    pub runtime: String,
    /// What the sandbox is running now.
    pub have: String,
    /// What npm would install.
    pub latest: String,
}

/// Which agent CLIs are behind — **read, never asked** (SKEIN-405).
///
/// Asked for in these words: *"show that in the bar when there is an update. You check if new
/// version is out regularly."* Both halves are here: this is the reading, and it is free; the
/// asking happens on skein's own clock, behind whoever called.
///
/// **It must not spawn, and that rule is older than this function.** [`crate::health::health_report`]
/// says so about its own AI field — "a polled endpoint is the wrong place to spawn a process to find
/// out whether a binary runs" — and this is polled from the same report. So a caller gets what is
/// remembered, immediately, including nothing at all on the first call; the refresh runs on a thread
/// and the next caller finds an answer. A cockpit that stalls on an npm round trip is a worse
/// outcome than a bar that says nothing for one tick.
///
/// Empty means "nothing to say", and it means it for every reason: nothing checked yet, the check
/// failed, or everything is current. That is deliberate — the bar's job is to speak when there is
/// something to install, and "skein could not find out" is not something a person can act on.
pub fn runtime_updates() -> Vec<RuntimeUpdate> {
    let known = READINGS.known();
    if READINGS.claim_if_due(std::time::Instant::now()) {
        std::thread::spawn(|| READINGS.take(std::time::Instant::now, look_for_newer_runtimes));
    }
    known.map(|(_, found)| found).unwrap_or_default()
}

/// File a reading as the current one, whoever took it.
///
/// Its own function because the callers must not drift: the timed check — behind
/// [`runtime_updates`] and the server's own loop, [`watch_runtime_updates`] — and
/// [`update_runtimes`] refreshing it the moment an install makes it wrong. A reading stored by one
/// and not the other is exactly the bug this exists to stop.
fn remember_updates(found: Vec<RuntimeUpdate>) {
    READINGS.file(std::time::Instant::now(), found);
}

/// Six hours. An agent CLI ships a few times a week, and the answer is only ever used to draw a
/// line in a bar — so this is about being told within a working day, not about being current to the
/// minute. It is also a network call per fleet, which is the thing to be sparing with.
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The remembered reading and the flag that says one is being taken — one value rather than two
/// statics, so the loop's test can hand [`watch_updates`] a store of its own. Two statics were
/// shared with every other test in the process, and one of those calls [`runtime_updates`] cold,
/// which starts a real check that files over whatever the loop's test had arranged.
struct Readings {
    last: std::sync::Mutex<Option<(std::time::Instant, Vec<RuntimeUpdate>)>>,
    /// Held while a check runs, so ten pollers arriving during one — or the loop arriving during a
    /// poller's — do not each start another.
    checking: std::sync::atomic::AtomicBool,
}

static READINGS: Readings = Readings::new();

impl Readings {
    const fn new() -> Readings {
        Readings {
            last: std::sync::Mutex::new(None),
            checking: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn known(&self) -> Option<(std::time::Instant, Vec<RuntimeUpdate>)> {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Is a reading due at `now`, and if so, is it this caller's to take? **One rule for both ways
    /// in** — the polled one and the loop — so neither can drift into checking more often than
    /// [`UPDATE_CHECK_EVERY`]. A `true` must be answered by [`Readings::take`], which is what gives
    /// the flag back.
    fn claim_if_due(&self, now: std::time::Instant) -> bool {
        let due = match self.known() {
            Some((at, _)) => now.saturating_duration_since(at) >= UPDATE_CHECK_EVERY,
            None => true,
        };
        due && !self
            .checking
            .swap(true, std::sync::atomic::Ordering::SeqCst)
    }

    /// Take the reading [`Readings::claim_if_due`] said was due, file it at the time it finished,
    /// and give the flag back. Blocking: `check` is an exec into the sandbox and an npm round trip.
    fn take(
        &self,
        now: impl Fn() -> std::time::Instant,
        check: impl FnOnce() -> Vec<RuntimeUpdate>,
    ) {
        let found = check();
        self.file(now(), found);
        self.checking
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn file(&self, at: std::time::Instant, found: Vec<RuntimeUpdate>) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((at, found));
    }
}

/// How often the server's loop ASKS whether a reading is due, which is not how often it takes one.
///
/// **Not [`UPDATE_CHECK_EVERY`] itself.** A reading is filed when the check finishes, up to two
/// minutes after the tick that started it, so a loop ticking every six hours would find the last
/// reading a little under six hours old on every tick, skip it, and leave the check to whoever
/// polls next — the gap this loop exists to close. Asking once a minute costs a mutex read; the
/// npm pair still runs once per [`UPDATE_CHECK_EVERY`].
const UPDATE_LOOK_EVERY: Duration = Duration::from_secs(60);

/// The loop the server runs, so that "every six hours" is true of a fleet nobody is watching
/// (SKEIN-1068). Never returns. **It only checks; installing stays a person's press**
/// ([`update_runtimes`]).
///
/// Before this the check sat behind [`runtime_updates`] alone, whose only production callers are
/// the health report and the Update route — so a fleet with no cockpit open never checked at all.
/// Its own loop for the reason `announce::watch_fleet_disk` gives about itself: `crate::stream`
/// does no work for a server nobody is watching, and this has to happen anyway.
pub async fn watch_runtime_updates() {
    watch_updates(
        UPDATE_LOOK_EVERY,
        std::time::Instant::now,
        look_for_newer_runtimes,
        &READINGS,
    )
    .await
}

/// [`watch_runtime_updates`] with the period, the clock, the check and the store as arguments — the
/// seam the loop is tested through, the way `announce::watch_disk` is. Six hours is untestable by
/// waiting, so the test moves the clock instead. Private, so the choosing stops at this file's edge.
///
/// **`spawn_blocking`, not a bare call**: the check is an exec into the sandbox and an npm round
/// trip, and running it on a runtime thread would stall every cockpit connection the server holds.
async fn watch_updates<N, C>(every: Duration, now: N, check: C, readings: &'static Readings)
where
    N: Fn() -> std::time::Instant + Clone + Send + 'static,
    C: Fn() -> Vec<RuntimeUpdate> + Clone + Send + 'static,
{
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        if !readings.claim_if_due(now()) {
            continue;
        }
        let (now, check) = (now.clone(), check.clone());
        if let Err(e) = tokio::task::spawn_blocking(move || readings.take(now, check)).await {
            // A check that panicked never gave the flag back. Give it back here, or no reading
            // would ever be taken again, by this loop or by anybody polling.
            readings
                .checking
                .store(false, std::sync::atomic::Ordering::SeqCst);
            eprintln!("skein: agent CLI update check did not run: {e}");
        }
    }
}

/// Ask npm what it would install, and the sandbox what it is running. One exec, both answers.
///
/// **In the sandbox, because that is where the CLIs are** — a box shares them and skein's own host
/// may not have them at all. `npm view` is the network call; `--version` is local to the sandbox.
///
/// A runtime that answers neither is left out rather than reported as "unknown": the bar exists to
/// say there is something to install, and a row that cannot say what it would move to is not that.
fn look_for_newer_runtimes() -> Vec<RuntimeUpdate> {
    let sandbox = fleet_sandbox();
    check_runtimes(&sandbox)
}

/// The same check, against a sandbox somebody already has in hand — so an update can re-read the
/// fleet it just installed into without going back to the environment for its name.
fn check_runtimes(sandbox: &str) -> Vec<RuntimeUpdate> {
    let packages = std::env::var("SKEIN_RUNTIME_PACKAGES")
        .unwrap_or_else(|_| "@anthropic-ai/claude-code @openai/codex".to_string());
    let script = format!(
        "SKEIN_RUNTIME_PACKAGES={}; {RUNTIME_VERSIONS_SCRIPT}",
        sh_quote(packages.trim())
    );
    let Ok(out) = own_sandbox(sandbox).exec(&script, Duration::from_secs(120)) else {
        return Vec::new();
    };
    parse_runtime_versions(&out)
}

/// `<runtime> <have> <latest>` a line, and nothing else on stdout.
const RUNTIME_VERSIONS_SCRIPT: &str = r#"
         command -v npm >/dev/null 2>&1 || exit 0;
         for p in $SKEIN_RUNTIME_PACKAGES; do
           case "$p" in
             *claude-code) r=claude ;;
             *codex)       r=codex  ;;
             *)            continue ;;
           esac;
           command -v "$r" >/dev/null 2>&1 || continue;
           have="$($r --version 2>/dev/null | head -n 1 | tr -d '
')";
           latest="$(timeout 60 npm view "$p" version 2>/dev/null | tr -d '
')";
           [ -n "$have" ] && [ -n "$latest" ] && printf '%s %s %s
' "$r" "$have" "$latest";
         done"#;

/// What the script said, keeping only the runtimes that are genuinely behind.
///
/// **`have` is not a bare version.** `claude --version` answers `1.2.3 (Claude Code)`, so the
/// comparison is "does what we have CONTAIN what npm offers" rather than string equality — which
/// also means a version scheme skein has never seen still compares correctly, because the only
/// thing being asked is whether npm's number is already in the sandbox's answer.
///
/// Separate from the exec so it can be tested without a sandbox, which is the whole reason the
/// script writes a format rather than being read for its effect.
fn parse_runtime_versions(out: &str) -> Vec<RuntimeUpdate> {
    out.lines()
        .filter_map(|line| {
            // **First and LAST, with everything between them as `have`.** `claude --version`
            // answers `1.2.3 (Claude Code)`, so the middle field has spaces in it and taking the
            // second token gives `1.2.3` while the third gives `(Claude` — which then compares
            // against npm's number and reports every current runtime as behind. Caught by the test
            // below rather than in the fleet, which is the only reason this reads correctly.
            let parts: Vec<&str> = line.split_whitespace().collect();
            let [runtime, rest @ .., latest] = parts.as_slice() else {
                return None;
            };
            if rest.is_empty() {
                return None;
            }
            // **Only runtimes skein actually has an adapter for.** Anything else on that stream is
            // not an answer — `npm ERR! code E404` parses as cleanly as a real line and would put
            // "npm ERR! code → E404" in the bar. Asked of the runtime registry rather than written
            // down here, so a runtime added there is covered without anyone remembering this.
            if !crate::runtime::valid_runtime(runtime) {
                return None;
            }
            let runtime = runtime.to_string();
            // Compared against the WHOLE of what the runtime said, shown as just the number.
            // `claude --version` answers `1.2.3 (Claude Code)`: the trailing words are noise on a
            // bar and are exactly what makes the comparison safe, since the only question is
            // whether npm's number is already somewhere in that answer.
            let said = rest.join(" ");
            // **The first token that starts with a digit, not simply the first token.** The version
            // does not always come first: `claude --version` answers `2.1.247 (Claude Code)`, but
            // `codex --version` answers `codex-cli 0.150.1` — so taking `rest[0]` put the package's
            // NAME in the bar, and the live fleet drew `codex codex-cli -> 0.150.1` (read from
            // /api/health, 2026-08-27). A runtime whose answer holds no number at all is dropped:
            // the bar's whole sentence is `have -> latest`, and a row that cannot say what it has
            // cannot make it.
            let have = rest
                .iter()
                .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))?
                .to_string();
            let latest = latest.to_string();
            // Already on it. `contains` rather than `==` for the reason above.
            if said.contains(&latest) {
                return None;
            }
            Some(RuntimeUpdate {
                runtime,
                have,
                latest,
            })
        })
        .collect()
}

/// Bring the sandbox's agent CLIs up to date — **the only thing in skein that ever does**
/// (SKEIN-404).
///
/// [`SUBSTRATE_SCRIPT`] installs a runtime only when its command is missing, which is right for a
/// launch (reinstalling two npm packages on every box start is minutes nobody asked for) and means
/// the version that first landed is the version that stays, for ever.
///
/// **The `claude update` skein used to run before every agent session looked like the answer and
/// could not be.** It ran INSIDE a box, and a box is a user namespace mapping only your own uid —
/// the CLI lives under a root-owned `/usr/local/lib/node_modules`, put there by the `sudo npm` in
/// the script above, so it reads as `nobody` and npm cannot write it. Measured 2026-08-26: 1.9-3.2s
/// per session start and `Error: Failed to install update` every single time, swallowed by the
/// `|| echo` on the same line. It has been deleted (SKEIN-403); this is what replaces it.
///
/// **Fleet-wide, because there is nowhere else it could be.** The runtimes are installed once into
/// the sandbox and every box shares them — see the comment at the top of [`SUBSTRATE_SCRIPT`] — so
/// a box has no CLI of its own to update. A box already running keeps the binary it started with
/// until its next session, which is simply what a running process does with a file replaced under
/// it; nothing is restarted here, because restarting somebody's agent to install an update is not
/// a decision this should be making on its own.
///
/// Returns what moved, per runtime, so the caller can say so rather than say "done".
pub fn update_runtimes(sandbox: &str) -> Result<String, String> {
    // The same seam `ensure_substrate` has, for the same reason: a harness must be able to ask for
    // no runtimes rather than npm-install an agent onto whoever is running the tests.
    let packages = std::env::var("SKEIN_RUNTIME_PACKAGES")
        .unwrap_or_else(|_| "@anthropic-ai/claude-code @openai/codex".to_string());
    let script = format!(
        "SKEIN_RUNTIME_PACKAGES={}; {RUNTIME_UPDATE_SCRIPT}",
        sh_quote(packages.trim())
    );
    let said = own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(900))
        .map(|out| out.trim().to_string())?;
    // **Re-read before returning, because the bar answers from a remembered reading** (SKEIN-441).
    // Without this the install is invisible to the thing that offered it: `runtime_updates` keeps
    // the answer it took up to `UPDATE_CHECK_EVERY` ago, so the next `/api/health` poll — two
    // seconds later — hands the page the same "behind" reading and the bar comes straight back.
    // Measured on the live fleet 2026-08-27: the bar offered `claude 2.1.221 -> 2.1.247` while the
    // sandbox's own tree was already at 2.1.247, so pressing update correctly reported nothing to
    // do and correctly changed nothing anybody could see.
    //
    // Done here rather than by the caller so every route in gets it — the cockpit's button and
    // `skein update-agents` both — and done inline rather than by invalidating, because this has already
    // spent minutes in npm and a reading the very next poll can use beats a gap it cannot.
    remember_updates(check_runtimes(sandbox));
    Ok(said)
}

/// **Asked for by name, never by `command -v`.** That guard is the whole defect this exists for: a
/// runtime that is present is a runtime that is never upgraded, so a path that consulted it would
/// be the same bug wearing a different function.
///
/// Versions are read before and after and reported as a change, because "updated" is not a fact
/// anybody can check and `1.2.3 -> 1.2.9` is. A runtime that was already current says so rather
/// than claiming to have moved.
///
/// **It installs into the prefix a BOX executes from, which is not the one `sudo` writes**
/// (SKEIN-968). This said `sudo npm install -g` and nothing else for as long as it existed, and
/// `sudo npm` is npm running as root, whose global prefix is `/usr/local`. Every box's PATH begins
/// `$HOME/.local/bin:/usr/local/share/npm-global/bin:…` (`box-session.sh`'s `box_path`, and
/// `BOX_PATH_HEAD` in `src/place.rs` for the crossing), and that npm-global prefix belongs to uid
/// 1000. Measured on the live fleet, 2026-09-19: `/usr/local/share/npm-global/bin/claude` was
/// 2.1.278 and the root-owned `/usr/local/bin/claude` this script had been writing was 2.1.272. So
/// the update installed, correctly reported that it had, and changed nothing any box ran — the
/// shape of SKEIN-441, one prefix further down.
///
/// Unprivileged when the configured prefix is ours, `sudo` when it is not, because both substrates
/// are real: a per-user npm prefix (this fleet) and a root-owned `/usr/local` (a plain image).
///
/// And it SAYS when the copy it installed is not the copy on the PATH, rather than leaving that to
/// be inferred from a version that did not move. A shadowed install is the one failure here that
/// looks exactly like success.
const RUNTIME_UPDATE_SCRIPT: &str = r#"
         set -- $SKEIN_RUNTIME_PACKAGES;
         [ "$#" -gt 0 ] || { echo 'no agent runtimes are configured here, so there is nothing to update'; exit 0; };
         command -v npm >/dev/null 2>&1 || { echo 'this sandbox has no npm, so the agent CLIs cannot be updated in it' >&2; exit 1; };
         was_claude="$(claude --version 2>/dev/null | head -n 1)";
         was_codex="$(codex --version 2>/dev/null | head -n 1)";
         prefix="$(npm config get prefix 2>/dev/null)";
         case "$prefix" in undefined|null) prefix="";; esac;
         if [ -n "$prefix" ] && [ -w "$prefix" ]; then as=""; else as="sudo"; fi;
         log=/tmp/skein-runtime-update.log;
         timeout 600 $as npm install -g "$@" >"$log" 2>&1 || {
             echo 'npm could not install the agent runtimes. It said:' >&2;
             tail -n 15 "$log" | sed 's/^/  | /' >&2;
             exit 1;
         };
         moved=0;
         for r in claude codex; do
           case "$r" in claude) was="$was_claude";; codex) was="$was_codex";; esac;
           now="$($r --version 2>/dev/null | head -n 1)";
           if [ -z "$now" ]; then continue; fi;
           if [ -z "$was" ]; then echo "$r: installed, now $now"; moved=1;
           elif [ "$was" = "$now" ]; then echo "$r: $now (already current)";
           else echo "$r: $was -> $now"; moved=1; fi;
           at="$(command -v "$r" 2>/dev/null)";
           if [ -n "$prefix" ] && [ -n "$at" ] && [ "$at" != "$prefix/bin/$r" ] && [ -x "$prefix/bin/$r" ]; then
             echo "$r: installed into $prefix but this sandbox runs $at, which every box runs too — the install is shadowed" >&2;
           fi;
         done;
         [ "$moved" = 1 ] || echo 'nothing moved — every runtime here was already the newest npm has.'"#;

/// Install the tools a box needs in order to exist at all.
///
/// Measured in a real sandbox: the `shell` image ships `bwrap` and `git` but **not `tmux`**, and a
/// box without tmux cannot start — `box-session.sh` refuses, because the session *is* the box.
///
/// skein's kit installs jq and tmux for ordinary boxes, but it cannot serve this one: its startup
/// hook returns early in non-clone mode ("already has an in-repo .claude"), and a fleet sandbox is
/// neither a clone nor a mounted repo. So it provisions its own substrate rather than bending a hook
/// written for a different shape. jq comes along because the store probes that run inside boxes need it.
///
/// bwrap is checked but never installed: without it there is no isolation to be had, and quietly
/// continuing would give every box the sandbox's own `/tmp` and `$HOME` — the exact collision this
/// design exists to prevent.
pub fn ensure_substrate(sandbox: &str) -> Result<(), String> {
    let script = SUBSTRATE_SCRIPT;
    // The packages are named by the caller, not by the script, so a harness can ask for none.
    // Without that seam the integration test — whose `sbx exec` runs on the developer's own machine
    // — npm-installs an agent runtime onto it, which is both a 50s test and software nobody asked
    // for. $SKEIN_RUNTIME_PACKAGES set to empty means "install no runtimes".
    let packages = std::env::var("SKEIN_RUNTIME_PACKAGES")
        .unwrap_or_else(|_| "@anthropic-ai/claude-code @openai/codex".to_string());
    // Read on the host, because the record of what was approved lives there — see
    // `substrate::manifest_path` for why keeping it in the sandbox would defeat the whole point.
    let (apt, npm) = crate::substrate::approved_packages();
    let script = format!(
        "SKEIN_RUNTIME_PACKAGES={}; SKEIN_APPROVED_APT={}; SKEIN_APPROVED_NPM={}; {script}",
        sh_quote(packages.trim()),
        sh_quote(&apt.join(" ")),
        sh_quote(&npm.join(" ")),
    );
    own_sandbox(sandbox)
        .exec(&script, Duration::from_secs(900))
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The bar speaks only when there is something to install** (SKEIN-405), and it says what it
    /// would move to.
    ///
    /// Asked for as a check on skein's own clock rather than on every session start, so what
    /// matters here is the answer's SHAPE: a runtime that is behind is reported with both versions,
    /// and one that is current is not reported at all. A bar that said "up to date" would be a bar
    /// somebody has to dismiss.
    #[test]
    fn only_a_runtime_that_is_actually_behind_is_offered_an_update() {
        let found = super::parse_runtime_versions(
            "claude 1.2.3 (Claude Code) 1.2.9\n\
             codex 0.4.1 0.4.1\n",
        );
        assert_eq!(
            found,
            vec![super::RuntimeUpdate {
                runtime: "claude".into(),
                have: "1.2.3".into(),
                latest: "1.2.9".into(),
            }],
            "either a runtime that is behind was not offered an update — which is the whole of \
             what the owner asked to see in the bar — or one that is already current was, which \
             makes the bar something to dismiss rather than something to act on"
        );

        // `claude --version` answers `1.2.3 (Claude Code)`, so the sandbox's answer is not a bare
        // version and never was. Compared by containment, which also means a version scheme skein
        // has never seen still compares correctly: the only question is whether npm's number is
        // already in what the sandbox reported.
        assert!(
            super::parse_runtime_versions("claude 1.2.9 (Claude Code) 1.2.9\n").is_empty(),
            "a runtime already on the newest version was offered an update, because its own \
             `--version` prints more than a number"
        );

        // A line that cannot say what it would move to is not an offer. Silence beats a row with a
        // blank in it.
        for half in ["claude 1.2.3\n", "claude\n", "\n", "npm ERR! code E404\n"] {
            assert!(
                super::parse_runtime_versions(half).is_empty(),
                "an answer skein could not read was turned into an update offer: {half:?}"
            );
        }
    }

    /// **Reading the answer never waits for it** (SKEIN-405). This is polled from
    /// [`crate::health::health_report`], whose own `ai` field says why in as many words: "a polled
    /// endpoint is the wrong place to spawn a process to find out whether a binary runs". The check
    /// behind this makes a network call, so a version of it that asked inline would make the whole
    /// board wait on npm.
    ///
    /// A cold call answers "nothing to say" — which is the honest answer, since nothing has been
    /// checked — and starts the checking behind the caller. Timed rather than asserted structurally
    /// because the failure is a wait: an inline `npm view` is seconds, and a second is a hundred
    /// times this bound. That is not a tight measurement and does not need to be.
    #[test]
    fn asking_whether_a_runtime_is_behind_answers_now_and_finds_out_later() {
        let began = std::time::Instant::now();
        let said = super::runtime_updates();
        let took = began.elapsed();
        assert!(
            took < Duration::from_millis(500),
            "reading the update offer took {took:?} — it is doing the network check inline, and \
             the whole board is polled through it"
        );
        // Whatever it found (nothing here — no sandbox), it must be a list rather than a refusal:
        // "skein could not find out" is not something a person can act on, so it is not said.
        assert!(
            said.len() < 100,
            "the remembered answer is implausible, so this is not reading what it thinks it is"
        );
    }

    /// **Six hours pass with nobody polling, and a fresh reading is taken anyway** (SKEIN-1068).
    ///
    /// Every other test of the check goes through [`super::runtime_updates`], so all of them pass on
    /// a skein whose only trigger is a health poll or the Update route — a fleet with no cockpit
    /// open, never checking. Nothing here calls either: what is under test is the server's own
    /// loop, [`super::watch_updates`], driven at a 20ms tick against a clock this test moves.
    ///
    /// Real time for the tick and a moved clock for the age, for `announce::watch_disk`'s reason:
    /// `spawn_blocking` leaves the runtime idle while the check is in flight, so a paused tokio
    /// clock could run the ticks out from under it. The store is the test's own [`super::Readings`],
    /// because the process-wide one is also written by a cold [`super::runtime_updates`] in the
    /// test above. `npm` is stubbed at `place::seam`, the one place the real check crosses into the
    /// sandbox.
    ///
    /// The sabotage each assertion was named against:
    ///
    /// * *not before it is due* — drop the due test from [`super::Readings::claim_if_due`], so
    ///   every tick checks.
    /// * *a fresh reading was taken with nobody polling* — make [`super::watch_updates`]'s body
    ///   `continue` instead of checking, leaving the polled path as the only way in.
    /// * *and only one* — stop [`super::Readings::take`] filing what it found, so every tick finds
    ///   the old reading still due and checks again.
    /// * *off the runtime's thread* — call `readings.take` directly instead of `spawn_blocking`.
    #[test]
    fn the_server_checks_for_newer_agent_clis_every_six_hours_with_nobody_polling() {
        use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Instant;

        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", home.join("fleet"));
        assert!(
            crate::util::fleet_root().starts_with(home.to_str().expect("a utf-8 fixture path")),
            "the fleet root this test resolves is not the fixture's: {}",
            crate::util::fleet_root()
        );
        // npm stubbed where it is actually reached: the version check is a fleet-scope crossing
        // (`own_sandbox(..).exec`), and `place::seam` is where a test stands in for one — a `$PATH`
        // stub would not be, since the crossing runs under a PATH of its own. It does nothing, and
        // it is not asserted on: the seam is process-wide, and the cold `runtime_updates()` in the
        // test above takes no lock and crosses from its own thread, so a count here would be that
        // test's as often as this loop's (it was, in the first gate run).
        let _npm = crate::place::seam::doing_nothing();

        static STORE: super::Readings = super::Readings::new();
        let stale = super::RuntimeUpdate {
            runtime: "claude".into(),
            have: "1.0.0".into(),
            latest: "1.0.1".into(),
        };
        let fresh = super::RuntimeUpdate {
            runtime: "claude".into(),
            have: "1.0.0".into(),
            latest: "2.0.0".into(),
        };
        let t0 = Instant::now();
        STORE.file(t0, vec![stale.clone()]);

        // The clock the loop reads: the real one at t0, plus however far this test has moved it.
        let moved = Arc::new(AtomicU64::new(0));
        let clock = {
            let moved = Arc::clone(&moved);
            move || t0 + Duration::from_secs(moved.load(Ordering::SeqCst))
        };
        let runtime_thread = std::thread::current().id();
        let checks = Arc::new(AtomicUsize::new(0));
        let on_the_runtime_thread = Arc::new(AtomicBool::new(false));
        let check = {
            let (checks, on_the_runtime_thread) =
                (Arc::clone(&checks), Arc::clone(&on_the_runtime_thread));
            let fresh = fresh.clone();
            move || {
                checks.fetch_add(1, Ordering::SeqCst);
                if std::thread::current().id() == runtime_thread {
                    on_the_runtime_thread.store(true, Ordering::SeqCst);
                }
                vec![fresh.clone()]
            }
        };

        let six_hours = 6 * 60 * 60;
        let (before_due, after_due) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a tokio runtime for this test's body")
            .block_on(async {
                let watching = tokio::spawn(super::watch_updates(
                    Duration::from_millis(20),
                    clock,
                    check,
                    &STORE,
                ));
                // A minute short of six hours: ten ticks, and none of them may check.
                moved.store(six_hours - 60, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(200)).await;
                let before_due = checks.load(Ordering::SeqCst);
                // Past six hours. Bounded, so a loop that never checks fails below, not hangs.
                moved.store(six_hours + 1, Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(10);
                while checks.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                // Ten more ticks on the same clock: the reading just filed is not due again.
                tokio::time::sleep(Duration::from_millis(200)).await;
                watching.abort();
                (before_due, checks.load(Ordering::SeqCst))
            });

        assert_eq!(
            before_due, 0,
            "the loop checked a reading that was not yet six hours old — it is spending an npm \
             round trip per tick rather than one per six hours"
        );
        assert!(
            after_due >= 1,
            "six hours passed with nobody polling and no reading was taken — the check is still \
             only reachable through runtime_updates' callers, so an unwatched fleet never checks"
        );
        assert_eq!(
            after_due, 1,
            "the loop took {after_due} readings for one six-hour crossing, so what it takes is not \
             what it files as current"
        );
        let (at, found) = STORE.known().expect("a reading");
        assert_eq!(
            found,
            vec![fresh],
            "the fresh reading was not filed as the current one, so the bar still answers from the \
             stale one"
        );
        assert!(
            at >= t0 + Duration::from_secs(six_hours),
            "the reading was filed at the wrong time, so its age is not measured from when it was \
             taken"
        );
        assert!(
            !on_the_runtime_thread.load(Ordering::SeqCst),
            "the check ran on the runtime's own thread — it is an npm round trip, and every cockpit \
             connection the server holds would stall on it"
        );
    }

    /// Something in the server starts [`super::watch_runtime_updates`]. A source read, and the one
    /// claim a source read is the right tool for; the test above is what shows the loop ticks.
    #[test]
    fn the_server_is_what_runs_the_update_check() {
        let server = include_str!("../bin/skein-server.rs");
        assert!(
            server.contains("tokio::spawn(skein::fleet::watch_runtime_updates())"),
            "nothing in skein-server.rs starts the update-check loop, so a fleet nobody watches \
             never checks for a newer agent CLI"
        );
    }

    /// The check runs where the CLIs are, asks npm what it would install, and asks each runtime
    /// what it is running — and it never reports a runtime the sandbox does not have.
    ///
    /// Asserted on the script's text rather than by running it: `sbx` does not exist inside a box,
    /// so nothing here can reach a sandbox. What this holds is the shape that would silently stop
    /// the bar working.
    #[test]
    fn the_version_check_asks_npm_and_the_sandbox_and_skips_what_is_not_installed() {
        let script = super::RUNTIME_VERSIONS_SCRIPT;
        assert!(
            script.contains("npm view"),
            "nothing asks npm what it would install, so the bar can never know there IS a newer \
             version — which is the entire question"
        );
        assert!(
            script.contains("--version"),
            "nothing asks the sandbox what it is running, so skein cannot tell behind from current"
        );
        assert!(
            script.contains("command -v \"$r\" >/dev/null 2>&1 || continue"),
            "a runtime the sandbox does not have would be reported anyway — offering to update \
             something that is not installed"
        );
        assert!(
            script.contains("timeout 60 npm view"),
            "the network call is unbounded, and this runs behind a poll that must never hang"
        );
    }

    /// **The version is not always the first thing a runtime says.** `claude --version` answers
    /// `2.1.247 (Claude Code)` and `codex --version` answers `codex-cli 0.150.1` — the number
    /// leads in one and trails in the other. Taking the first token put the package's NAME in the
    /// bar, and the live fleet drew `codex codex-cli -> 0.150.1` (read from /api/health,
    /// 2026-08-27). Both shapes are here because a fix that only handles one is the same bug.
    #[test]
    fn a_runtime_that_names_itself_before_its_version_shows_the_version_and_not_its_name() {
        let found = super::parse_runtime_versions(
            "claude 2.1.221 (Claude Code) 2.1.247\ncodex codex-cli 0.149.0 0.150.1\n",
        );
        let saw: Vec<(&str, &str)> = found
            .iter()
            .map(|u| (u.runtime.as_str(), u.have.as_str()))
            .collect();
        assert_eq!(
            saw,
            vec![("claude", "2.1.221"), ("codex", "0.149.0")],
            "the bar is naming something that is not a version — a reader cannot tell what they \
             have, which is half of the only sentence the bar says"
        );
    }

    /// A runtime whose answer carries no number at all is left out rather than half-drawn. The
    /// bar's whole sentence is `have -> latest`; a row that cannot say what it has cannot make it,
    /// and the module already holds that rule for a runtime that answers nothing.
    #[test]
    fn a_runtime_whose_answer_carries_no_version_is_left_out_of_the_bar() {
        assert!(
            super::parse_runtime_versions("claude unreleased 2.1.247\n").is_empty(),
            "an answer with no version in it was turned into an update offer"
        );
    }

    /// **Installing refreshes the reading the bar answers from** (SKEIN-441).
    ///
    /// The reported bug, exactly: update pressed, the answer "the runtimes are already current",
    /// and the bar straight back. `runtime_updates` answers from a remembered reading refreshed
    /// every `UPDATE_CHECK_EVERY` — six hours — so an install that does not refresh it leaves the
    /// offer standing until the clock comes round, whatever it did.
    ///
    /// Asserted on the call rather than by running it, and the limit is real: `sbx` does not exist
    /// inside a box, so nothing here can reach a sandbox to install into. What this holds is the
    /// one line whose deletion brings the whole symptom back, and it is scoped to the function's
    /// own body so that the call moving somewhere else still fails it.
    #[test]
    fn installing_an_update_refreshes_the_reading_the_bar_answers_from() {
        let me = include_str!("substrate.rs");
        let at = me
            .find("pub fn update_runtimes")
            .expect("update_runtimes has been renamed; this test can no longer see it");
        let body = &me[at..];
        let body = &body[..body
            .find("\n}\n")
            .expect("update_runtimes has no end, so this is not reading a function body")];
        assert!(
            body.contains("remember_updates("),
            "an install no longer refreshes the remembered reading, so the bar will keep offering \
             an update that has already been done — for up to UPDATE_CHECK_EVERY"
        );
        assert!(
            body.contains("check_runtimes("),
            "the reading filed after an install is not a fresh one, so the bar would be refreshed \
             with the same stale answer it already had"
        );
    }

    /// **The update asks for the runtimes by name; the launch asks whether they are there.** That
    /// difference is the whole of SKEIN-404: `SUBSTRATE_SCRIPT`'s `command -v` guard is correct for
    /// a launch and is exactly why a runtime that is present is a runtime that is never upgraded. A
    /// path that consulted it would be the same defect wearing a new function name.
    ///
    /// Asserted against the script's text rather than by running it, and that limit is real: `sbx`
    /// does not exist in a box, so nothing here can create a sandbox. What this CAN hold is the one
    /// property that would silently undo the fix.
    #[test]
    fn updating_the_runtimes_asks_for_them_by_name_and_not_by_whether_they_are_installed() {
        assert!(
            super::SUBSTRATE_SCRIPT.contains("command -v claude")
                && super::SUBSTRATE_SCRIPT.contains("command -v codex"),
            "the launch path stopped skipping runtimes that are already installed, so every box \
             start now waits on two npm installs"
        );
        for guard in ["command -v claude", "command -v codex"] {
            assert!(
                !super::RUNTIME_UPDATE_SCRIPT.contains(guard),
                "the update path consults {guard:?} — so a runtime that is present is skipped, \
                 which is the exact reason the sandbox's CLI was frozen at whatever version first \
                 landed and the update does nothing at all"
            );
        }
        assert!(
            super::RUNTIME_UPDATE_SCRIPT.contains("npm install -g \"$@\""),
            "the update does not install the packages it was given"
        );
        assert!(
            super::RUNTIME_UPDATE_SCRIPT.contains("already current")
                && super::RUNTIME_UPDATE_SCRIPT.contains("->"),
            "the update reports that it ran rather than what moved — \"done\" is not a fact \
             anybody can check, and a version that did not move must say so"
        );
    }

    /// An approved package is installed but never command-checked, and the distinction is the
    /// difference between a fleet that starts and one that does not.
    ///
    /// The provisioning script ends by proving its work: for every name it asked apt for, it checks
    /// `command -v` and fails the launch if the name is not on `$PATH`. That is exactly right for
    /// tmux and jq, which are commands. It is exactly wrong for the packages this gate exists to
    /// install — `libnss3` is the reason chromium cannot start in a box, it installs perfectly, and
    /// it puts no command anywhere. Folding approved packages into `$need` would therefore have
    /// taken down every fleet launch after the first approval, reporting a package as missing while
    /// it sat installed. So they go in `$extra`, and only `$need` is ever verified by command.
    #[test]
    fn an_approved_package_is_installed_without_being_mistaken_for_a_command() {
        let s = SUBSTRATE_SCRIPT;
        assert!(
            s.contains("apt-get install -y -qq $apt_want"),
            "approved packages are never installed: {s}"
        );
        assert!(
            s.contains(r#"missing=''; for t in $need;"#),
            "the command check must iterate $need alone"
        );
        assert!(
            !s.contains("for t in $apt_want") && !s.contains("for t in $extra"),
            "a library package would be reported missing and fail the launch"
        );
        // And it must not reinstall on every launch: a fleet start that always runs apt is a fleet
        // start that always waits for the dpkg lock.
        assert!(
            s.contains("dpkg-query -W") && s.contains("npm ls -g"),
            "already-installed approved packages are re-installed on every launch: {s}"
        );
    }

    /// The approved list reaches the script as one quoted value, whatever is in it.
    #[test]
    fn the_approved_packages_cannot_break_out_of_the_assignment() {
        // `sh_quote` is what stands between the manifest — an ordinary file on the host — and a
        // root command line, so this asserts the join is quoted rather than interpolated bare.
        let quoted = sh_quote("libnss3 libatk1.0-0");
        assert!(
            quoted.starts_with('\'') && quoted.ends_with('\''),
            "{quoted}"
        );
        assert_eq!(sh_quote("a'; rm -rf /; '"), r#"'a'\''; rm -rf /; '\'''"#);
    }
}
