//! The one report the cockpit and `skein doctor` render: every check it carries, which of them
//! may raise the banner, and `health_report`, which builds it.

use super::*;

/// **Whether a check's fault turns the cockpit's health banner red** — the third element of every
/// entry in [`HealthReport::checks_with_banner`], and a type rather than a second list because an
/// exclusion has to be *argued where it is made* (SKEIN-1003).
///
/// It replaces a hand-written array of eleven `&field` references in `health_report` that had
/// nothing tying it to the struct. Three of the fourteen checks were missing from it, and nothing
/// anywhere said whether that was a decision or an omission — which is the defect, not the
/// membership: `gh` was an omission, and cost a fleet that could not reach GitHub *at all* its
/// banner entirely, while `memory`'s absence was deliberate and argued in a comment above the
/// array rather than at the check it was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnBanner {
    /// A fault here sets `ok = false`, so the banner appears and the page names it — under its
    /// label from [`CHECK_LABELS`], which the report serves as `labels` and the page walks, so a
    /// counted check cannot be one the page has no name for
    /// (`every_check_has_one_label_and_every_surface_reads_it`).
    Counted,
    /// A fault here is *reported* — the diagnostics pane, the work queue, and the banner's own
    /// list once it is up for some other reason — but never raises the banner on its own. The
    /// string is why, in one line, and it is the whole point of this variant: an exclusion states
    /// itself here instead of being achieved by absence from a list somewhere else.
    ///
    /// A check that goes red for somebody else's reason teaches people to read past the banner,
    /// and the next red is read past too (SKEIN-913) — so this is a real half, not a waiting room.
    NotCounted(&'static str),
}

/// Why `ai` is [`OnBanner::NotCounted`], as a named constant so that the reason is a sentence
/// rather than whatever fits on the line of the array entry.
const AI_IS_A_STATE_NOT_A_VERDICT: &str = "the enrichment toggle's own state, which the settings \
     pane prints beside the checkbox: `HealthCheck::satisfied` on every branch, so there is no \
     fault here to count";

/// Why `warden` is [`OnBanner::NotCounted`]: the warden is optional
/// (`docs/decisions/warden-or-prompt.md`), and a fleet without one is a supported state rather
/// than a broken one — every privileged host act it would perform is shown to the person as the
/// line to run instead.
const WARDEN_IS_OPTIONAL: &str = "the warden is optional (docs/decisions/warden-or-prompt.md): \
     without one, skein shows the person the command a warden would have run, so its absence is \
     not a fault the fleet has — and a banner that is red for a state somebody chose teaches them \
     to read past the next one (SKEIN-913)";

/// A check's name on the wire and the one label a person reads for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CheckLabel {
    /// The field serde writes the check under, which is what the page indexes the report by.
    pub key: &'static str,
    /// What the banner, the diagnostics pane, `skein doctor`, the work queue and `/v2` call it.
    pub label: &'static str,
}

impl CheckLabel {
    const fn new(key: &'static str, label: &'static str) -> CheckLabel {
        CheckLabel { key, label }
    }
}

/// **The one name each check has, wherever it is shown** (SKEIN-1186).
///
/// There used to be three: the banner headlined with the wire key (`cover: …`), the diagnostics
/// pane kept its own table in the page (`box isolation`), and `skein doctor` and the work queue
/// used a third set written into [`HealthReport::checks_with_banner`] (`isolation`). The banner
/// sends a reader to the diagnostics pane, so the same fault arrived under two names one click
/// apart. The report now serves this table as `labels`, the page renders from it, and the CLI
/// asks [`label`] — so a label is changed here or nowhere.
///
/// In [`HealthReport::checks_with_banner`]'s order, which is the order the page lists them in;
/// `every_check_has_one_label_and_every_surface_reads_it` holds the two level.
pub const CHECK_LABELS: [CheckLabel; HealthReport::CHECK_COUNT] = [
    CheckLabel::new("registry", "registry"),
    CheckLabel::new("sbx", "sbx"),
    CheckLabel::new("git", "git"),
    // Not about the `gh` CLI any more: "curl is installed" and "GitHub can be connected to at
    // all" (SKEIN-548, SKEIN-926) — so it is named for the second.
    CheckLabel::new("gh", "github reach"),
    CheckLabel::new("probes", "box probes"),
    CheckLabel::new("mailbox", "mailbox"),
    CheckLabel::new("memory", "fleet memory"),
    CheckLabel::new("disk", "fleet disk"),
    // The same credential asked three questions — what it can reach, whether it will still reach
    // it next month (SKEIN-928), and whether it is the one GitHub is actually answering
    // (SKEIN-548) — so the three sit together. "token life" and not "expiry": this report also
    // carries `expired_logins`, so an unqualified "expiry" names two deadlines with two owners.
    CheckLabel::new("gitgate", "github scope"),
    CheckLabel::new("token_expiry", "token life"),
    CheckLabel::new("proxy_injection", "proxy credential"),
    CheckLabel::new("warden", "host warden"),
    // "isolation" is a word somebody can act on where "cover" is jargon.
    CheckLabel::new("cover", "box isolation"),
    CheckLabel::new("ai", "ai enrichment"),
];

/// The label for the check serde writes as `key`, from [`CHECK_LABELS`] — for a surface that
/// prints one check at a time rather than the whole report, which is `skein doctor`.
///
/// Empty for a key that is not a check, and a test holds every caller in the CLI to real keys
/// (`every_check_has_one_label_and_every_surface_reads_it`), because a label that printed nothing
/// is the quiet version of the drift this exists to stop.
pub fn label(key: &str) -> &'static str {
    CHECK_LABELS
        .iter()
        .find(|c| c.key == key)
        .map(|c| c.label)
        .unwrap_or("")
}

impl HealthReport {
    /// How many checks [`HealthReport::checks`] returns — the length of its array, named rather
    /// than written into the signature so that a test can read it without building a report
    /// (`every_health_check_field_is_named_in_the_list`).
    pub const CHECK_COUNT: usize = 14;

    /// Every check in the report, by its name on the wire, each with whether it turns the banner
    /// red ([`OnBanner`]). One list, so a check added to the struct and forgotten here shows up as
    /// a compile error rather than as a check nothing ever looks at.
    ///
    /// **That sentence was false for as long as this pattern ended in `..`** (SKEIN-1000). A `..`
    /// makes the destructuring accept whatever it has not been told about, so two checks were added
    /// to the struct and never listed here — `token_expiry` (SKEIN-928) and `proxy_injection`
    /// (SKEIN-548) — and nothing failed. `health_report`'s `ok` counts both, so a fleet inside its
    /// token's renewal window put a red row above the cockpit; and [`crate::queue::who_needs_you`]
    /// builds its rows from THIS list, so the one surface whose job is "what needs a person" could
    /// not produce a row for either. A red banner and a work queue that does not say why is the
    /// exact shape the sentence above promises cannot happen.
    ///
    /// So every field is named, including the ones that are not checks: discarding them by name
    /// costs one line each and is what makes the next addition — of any type — stop the build until
    /// somebody decides which half it belongs in.
    ///
    /// **And the last element is the same argument one step on** (SKEIN-1003). `ok` used to be a
    /// separate array of eleven `&field` references, so a new check was silently *excluded* from
    /// the verdict exactly as `token_expiry` had been silently excluded from this list — same
    /// hole, one function down. `ok` is derived from this array now
    /// ([`HealthReport::first_counted_fault`]), so a check cannot reach the report without
    /// somebody having written down whether it belongs on the banner and, if not, why not.
    ///
    /// **The first column is the check's name on the wire** (SKEIN-1013): the field serde writes
    /// it under, which is what the page indexes the report by.
    /// `every_health_check_field_is_named_in_the_list` holds it equal to the binding beside it.
    /// What a person reads is not here: it is [`CHECK_LABELS`], one table for every surface
    /// (SKEIN-1186).
    pub fn checks_with_banner(
        &self,
    ) -> [(&'static str, &HealthCheck, OnBanner); Self::CHECK_COUNT] {
        use OnBanner::{Counted, NotCounted};
        let HealthReport {
            registry,
            sbx,
            git,
            gh,
            probes,
            mailbox,
            ai,
            memory,
            disk,
            gitgate,
            token_expiry,
            proxy_injection,
            warden,
            cover,
            // Not checks, and named one by one rather than swept up by a wildcard, which is the
            // whole point: a field added to the struct fails to compile until somebody has decided
            // which of these two halves it belongs in.
            ok: _,
            build: _,
            labels: _,
            logins: _,
            expired_logins: _,
            runtime_updates: _,
            models: _,
            dark_boxes: _,
            stale_boxes: _,
            uncovered_boxes: _,
            uncapped_boxes: _,
            unowned: _,
            runtimes: _,
            git_credential: _,
            counted: _,
        } = self;
        [
            ("registry", registry, Counted),
            ("sbx", sbx, Counted),
            ("git", git, Counted),
            // **Counted since SKEIN-1003, and it is the item's whole point.** This check is not
            // about the `gh` CLI any more: it is "curl is installed" and `github_reach_health` —
            // whether GitHub can be connected to AT ALL, which a deny-by-default egress policy
            // refuses rather than answers (SKEIN-548, SKEIN-926). Both arms produce `unsatisfied`
            // with a fix, and while `ok` was a separate list that did not name this field, a fleet
            // with no route to GitHub had `ok == true`, no banner, and therefore nothing sending
            // anybody to the diagnostics pane that would have shown it.
            ("gh", gh, Counted),
            ("probes", probes, Counted),
            ("mailbox", mailbox, Counted),
            // **Counted since SKEIN-1003, and this one overturns a stated exclusion, so the
            // argument is here rather than in the commit.** The comment that excluded it read
            // "being at the ceiling is the fleet working as configured" — and that is true of the
            // *throttling* arm, which returns `satisfied` and so could never have raised the
            // banner anyway. The two arms that do return `unsatisfied` are not covered by it: the
            // kernel having killed something for memory, whose own comment reads "a fault, not a
            // note: whatever it was did not finish", and no memory ceiling anywhere, where one
            // build can reach the VM's memory and the kernel picks a victim by badness rather than
            // by blame. Both are faults the check's own author named as faults.
            ("memory", memory, Counted),
            // A filesystem past its threshold is not a ceiling being used, it is a wall being
            // approached, and the only warning anyone gets before a build dies somewhere in the
            // middle — in whichever box happened to ask for the next block, usually not the one
            // that took the space. It can only be a fault past the threshold: an unknown disk (no
            // sandbox, no answer) is never one.
            ("disk", disk, Counted),
            ("gitgate", gitgate, Counted),
            ("token_expiry", token_expiry, Counted),
            ("proxy_injection", proxy_injection, Counted),
            // **Not counted since SKEIN-1184**, and this overturns a stated inclusion, so the
            // argument is here. It was counted on the reasoning that fleet create and destroy go
            // only through the warden, so a missing one was skein unable to do something it
            // offers. The owner's decision says otherwise (warden-or-prompt): with no warden,
            // skein shows the person the line, so nothing is unavailable — and a first run that
            // followed the README met a red banner whose fix was compiling Rust on the host.
            ("warden", warden, NotCounted(WARDEN_IS_OPTIONAL)),
            ("cover", cover, Counted),
            // The one check that is a *state readout* rather than a verdict: it is the enrichment
            // toggle's own state, which the settings pane prints beside the checkbox, and it is
            // built with `HealthCheck::satisfied` on every branch of `health_report` — "off" is a
            // correct state, not a fault. Counting a value that cannot be a fault would be
            // decoration; naming it here is what makes anyone who gives it a fault arm come back
            // and decide, instead of inheriting a silence. Last, as the diagnostics pane has
            // always listed it: it is about a switch, not about the fleet.
            ("ai", ai, NotCounted(AI_IS_A_STATE_NOT_A_VERDICT)),
        ]
    }

    /// Every check in the report, by the label a person reads — the list
    /// [`crate::queue::who_needs_you`] iterates, which is every check whether or not it reaches
    /// the banner.
    ///
    /// Derived from [`HealthReport::checks_with_banner`] rather than written out again: two lists
    /// of the same fourteen checks is how this file came to have three of them and no two the
    /// same set (SKEIN-1003).
    pub fn checks(&self) -> [(&'static str, &HealthCheck); Self::CHECK_COUNT] {
        self.checks_with_banner()
            .map(|(key, check, _)| (label(key), check))
    }

    /// **The first check that is both a fault and counted — the whole of what turns the banner
    /// red**, by its name on the wire, and `None` when there is none.
    ///
    /// Returns the key rather than a bool so that a caller, a test or a person reading a failure
    /// message learns *which* check decided it. `ok` is this plus "no stale sessions"; nothing
    /// else contributes to it.
    ///
    /// `is_fault` is what "fault" means here, and it declines `Unknown` — telling somebody their
    /// fleet is broken because skein could not reach it for two seconds is the false alarm the
    /// third state exists to stop. The cockpit reports the unknowns beside the faults either way,
    /// in the mark it already has for "look at this but nothing is wrong".
    pub fn first_counted_fault(&self) -> Option<&'static str> {
        self.checks_with_banner()
            .into_iter()
            .find(|(_, check, banner)| *banner == OnBanner::Counted && check.is_fault())
            .map(|(key, _, _)| key)
    }

    /// **The checks whose fault raises the banner, by the names the page reads them under**
    /// (SKEIN-1013) — what goes out as `counted`.
    ///
    /// The page's banner has one sentence a person can read and copy, and it has to be the reason
    /// the banner is up. The page cannot know which checks those are: that is decided here, by
    /// [`OnBanner`], and a page that kept its own list of them would be a second place deciding it
    /// — which is SKEIN-1003's failure exactly, a check reaching one list and not the other. So
    /// the report says. Every check is still in the report either way; this only says which of
    /// them may speak for the banner.
    ///
    /// In [`HealthReport::checks_with_banner`]'s order, which is the order of the `labels` the page
    /// walks, so the first counted fault the page finds is the one
    /// [`HealthReport::first_counted_fault`] finds.
    pub fn counted_on_the_wire(&self) -> Vec<&'static str> {
        self.checks_with_banner()
            .into_iter()
            .filter(|(_, _, banner)| *banner == OnBanner::Counted)
            .map(|(key, _, _)| key)
            .collect()
    }
}

/// Throttles a minute above which the fleet is worth mentioning as busy.
///
/// A handful is ordinary — a build briefly overshooting and the kernel reclaiming, which is what
/// `memory.high` is for. Sixty a minute is one a second, sustained, which is the shape that gets
/// remembered as "it felt slow" and never reported.
const THROTTLE_NOTICEABLE: f64 = 60.0;

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub ok: bool,
    /// **Which checks may raise the banner**, by their names on the wire:
    /// [`HealthReport::counted_on_the_wire`], written beside `ok` because it is the other half of
    /// the same verdict. `ok` says whether the banner is up; this says which checks can be the
    /// reason, so the page's headline is always one of them (SKEIN-1013).
    pub counted: Vec<&'static str>,
    /// **Every check's key and the one label a person reads for it** — [`CHECK_LABELS`], served
    /// so that the page renders the banner and the diagnostics pane from the same names `skein
    /// doctor` and the work queue print, instead of keeping its own (SKEIN-1186). In the order the
    /// page lists the checks.
    pub labels: &'static [CheckLabel],
    /// Which build is answering: [`BUILD_REVISION`]. On the report because /api/health is the one
    /// surface every deployment serves — the cockpit, curl, and a box all reach it — so it is where
    /// "is the fix deployed" gets answered without grepping HTML for marker strings.
    pub build: &'static str,
    pub registry: HealthCheck,
    pub sbx: HealthCheck,
    pub git: HealthCheck,
    pub gh: HealthCheck,
    pub probes: HealthCheck,
    pub mailbox: HealthCheck,
    /// Whether AI enrichment is on and can actually run. Never `ok: false` — it is opt-in, so
    /// "off" is a correct state, not a fault; the detail says what turning it on would buy.
    pub ai: HealthCheck,
    /// How the fleet sandbox's memory is divided between the boxes, its inner Docker daemon, and
    /// the reserve that keeps the sandbox itself answering. Worth a line of its own because when
    /// this is wrong the symptom is not a message — it is a sandbox that stops responding.
    pub memory: HealthCheck,
    /// How full the fleet's two filesystems are — the boxes' and Docker's image store.
    ///
    /// Beside memory rather than inside it because they fail differently: memory is divided by a
    /// plan and enforced by cgroups, while one filesystem serves every box with nothing enforcing
    /// anything. This is the resource the fleet actually runs out of, and it was the one nothing
    /// mentioned until asked (SKEIN-133).
    pub disk: HealthCheck,
    /// Whether a box's GitHub credential is actually scoped, and why not when it isn't.
    ///
    /// Never `ok: false` for being switched off — scoping is opt-in and "off" is a correct state.
    /// It reports `false` only when the fleet is *trying* to scope and cannot: an App ID that
    /// GitHub rejects, a key for a different App, an App installed on none of the repos in use.
    /// Those failures were previously invisible — `refresh_tokens` produced exact, useful errors
    /// and the server printed them to a detached process's stderr, so the first place anyone
    /// learned of one was a 403 inside a box some minutes later.
    pub gitgate: HealthCheck,
    /// **How long the GitHub credentials skein holds have left** (SKEIN-928).
    ///
    /// Beside `gitgate` rather than inside it because the two answer different questions and fail
    /// on different days: `gitgate` says whether a box's access is *scoped*, and this says whether
    /// it will still work next month. A fleet can be perfectly scoped and three days from losing
    /// GitHub entirely.
    ///
    /// `unsatisfied` inside [`RENEW_WINDOW_DAYS`], because only the owner can renew one of these
    /// and an expiry has no symptom until it has no symptoms left. `unknown` when GitHub could not
    /// be asked — never `satisfied`, since a dead credential and a credential with no expiry are
    /// the same silence on the wire.
    pub token_expiry: HealthCheck,
    /// **Whether the sandbox proxy is answering GitHub as the account** (SKEIN-548).
    ///
    /// skein cannot stop this — it is the substrate's, set on the host with `sbx secret set` — so
    /// the whole of skein's answer is to notice and say so. It is a banner rather than a refusal to
    /// start, because a false positive here would lock the owner out of their own fleet; and it is
    /// a banner rather than a `skein doctor` line, because a boundary nobody is looking at is a
    /// boundary nobody knows has gone.
    ///
    /// **Why it is a check rather than a sentence in a document.** It has flipped under this fleet
    /// twice in a fortnight in opposite directions, silently: injecting on 2026-09-06, injecting on
    /// 2026-09-15, and not injecting on 2026-09-21 (`docs/threat-model.md`). A document records the
    /// day it was written; only a check records today.
    ///
    /// Never `ok: false` for having no proxy — that is most deployments, and it is a correct state.
    pub proxy_injection: HealthCheck,
    /// Whether the host warden is answering, and what it says it can do.
    ///
    /// A fault when it is not: fleet create and destroy go only through it and there is no
    /// fallback, so without it two lifecycle operations are simply unavailable. Said here so that
    /// is learned at a glance rather than at the moment somebody presses Launch.
    pub warden: HealthCheck,

    /// Whether every running box is under the isolation this skein installs.
    ///
    /// The check that cannot be answered by looking at anything on the host: `install_launcher`
    /// refreshes `box-session.sh` at every start and every heal, so the copy on disk is always
    /// current and always says nothing about the boxes already running. The answer travels with
    /// each box instead, in its placement record.
    pub cover: HealthCheck,
    /// Which agent runtimes have a login every new box will inherit **and can still use**. Empty
    /// means `skein login` has not been run — the single most common way a first run goes quiet,
    /// since each box then comes up sitting at a sign-in prompt doing nothing. A credential whose
    /// refresh token has died is deliberately not in this list: it used to be, and on a fleet-wide
    /// logout every surface then said "signed in", so the symptom read as "each box needs a login"
    /// instead of "the fleet's credential is dead".
    pub logins: Vec<String>,
    /// Runtimes holding a credential whose refresh token has already died, and when it died.
    /// Beside `logins` rather than folded into it because the two states need different sentences:
    /// absent is "run `skein login`", expired is "one login heals every box — they all hold the
    /// same dead token". The dead token still seeds and heals boxes (reported here, never removed:
    /// a box with nothing is worse off than a box with a token a heal can replace).
    pub expired_logins: Vec<crate::fleet::ExpiredLogin>,
    /// Agent CLIs the sandbox could be running a newer version of (SKEIN-405).
    ///
    /// Beside `expired_logins` because it is the same kind of thing — a fact about the fleet's
    /// tooling that the bar says out loud — and for the same reason it needs its own sentence: a
    /// dead login stops work, an old CLI does not. One is a fault, the other is an offer.
    ///
    /// **Empty means nothing to say**, for every reason at once: nothing checked yet, the check
    /// failed, or everything is current. `fleet::runtime_updates` never blocks to find out, which
    /// is the rule this whole report already keeps — see the `ai` field's note about a polled
    /// endpoint being the wrong place to spawn a process.
    #[serde(default)]
    pub runtime_updates: Vec<crate::fleet::RuntimeUpdate>,
    /// **Which models this `claude` will accept** (SKEIN-451) — for the review-model setting's
    /// dropdown, so the choices offered are the ones that exist rather than a list written down in
    /// skein that goes stale the week a model ships. Parsed out of `claude --help`; see
    /// [`crate::ai::parse_model_aliases`].
    ///
    /// Empty means skein could not ask, and the setting stays the free-text box it has always been
    /// — which is also why the control is a `datalist` rather than a `select`: an exact build name
    /// must still be typeable when the list is short, wrong, or missing.
    #[serde(default)]
    pub models: Vec<String>,
    pub dark_boxes: Vec<String>,
    pub stale_boxes: Vec<String>,
    /// Running boxes whose mount namespace was built by an older `box-session.sh`.
    ///
    /// Named rather than counted because the fix is per box and costs the agent's unfinished work:
    /// "3 boxes" is not something anybody can act on at the moment they read it.
    #[serde(default)]
    pub uncovered_boxes: Vec<String>,
    /// Running boxes with no memory ceiling on them at all.
    ///
    /// Named rather than counted, because the two reasons need different people: skein's own plan
    /// producing nothing for a box is a restart, and a sandbox that will not delegate cgroups is a
    /// different fleet.
    #[serde(default)]
    pub uncapped_boxes: Vec<String>,
    /// Running containers and cgroups that no box owns, as the Settings → Diagnostics row draws
    /// them ([`crate::fleet::unowned_row`]).
    ///
    /// Not a `HealthCheck`, for two reasons. The row is marked `!` and carries commands, while a
    /// check that is not a fault must offer no fix. And nothing it finds is a fault: skein did not
    /// start these containers and does not remove them. `None` until the server's own pass has run
    /// once, because the report is polled and must not ask docker itself; the page draws no row
    /// until then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unowned: Option<crate::fleet::UnownedRow>,
    pub runtimes: Vec<RuntimeInfo>,
    /// How boxes get GitHub credentials, named — or empty when nobody has chosen.
    ///
    /// Empty is a real state now, not a theoretical one. All three paths are opt-in, so a fresh fleet
    /// has no way to push until someone picks one, and the first-run checklist asks on the strength of
    /// this field. It used to be unaskable: the account token was seeded by default, so the answer was
    /// always "the account token" and the question would have been noise.
    pub git_credential: String,
}

/// Read-only environment diagnosis for detached server deployments. Unlike startup `eprintln!`,
/// this remains inspectable from the cockpit and makes a missing box-side jq dependency explicit.
pub fn health_report() -> HealthReport {
    report_with(crate::fleet::runtime_updates)
}

/// [`health_report`] with the runtime-update reading as an argument, the way
/// `fleet::substrate::updates_from` takes its check — so the tests that build the real report can
/// stand in for it (SKEIN-1121). Called cold, `fleet::runtime_updates` starts the real version
/// check on a thread nobody joins, which outlives the test's `env_lock` and `place::seam`: after
/// the test unpins `$SKEIN_HOME` it panics on `config::skein_home`'s guard, and after the seam guard
/// drops it crosses through whichever test's stand-in is installed next (SKEIN-1087).
fn report_with(runtime_updates: impl FnOnce() -> Vec<crate::fleet::RuntimeUpdate>) -> HealthReport {
    // **This field is the toggle's own state**, and deliberately stays that way. The page builds
    // `#set-ainote` from it and the settings pane reads "off — …" beside the checkbox, so widening
    // it to mean "anything that wants the model" broke the sentence next to the control it
    // describes. It must also never go unsatisfied: opting out is not a fault, and a polled endpoint
    // is the wrong place to spawn a process to find out whether a binary runs.
    //
    // Where the other half went: `skein doctor` has a `model` line that asks about BOTH switches and
    // actually tries the binary. That is a command a person runs, so it can afford the subprocess
    // and the answer arrives when somebody is asking the question.
    let ai = HealthCheck::satisfied(if !crate::ai::ai_enabled() {
        "off — Settings → Boxes turns it on: a one-line summary for boxes with no journal, and a \
         second opinion before Continue N resumes anything"
            .to_string()
    } else if !program_on_path("claude") {
        "on, but `claude` is not on PATH — every call falls back to the free signals".to_string()
    } else {
        "on — rationed Haiku over your subscription, on demand and cached per turn-end".to_string()
    });
    // Named in GiB rather than MiB: these are numbers a person compares against how much memory the
    // Mac has, and 15975 does not read as "about sixteen gigabytes" at a glance.
    let gib = |mib: u64| format!("{:.1}G", mib as f64 / 1024.0);
    // Every box shares one sandbox, so there is always a division to report. This used to have a
    // "one sandbox per box — nothing to divide" arm for a fleet whose name was cleared; that model is
    // gone, and with it the only way to reach it.
    // How the memory is divided, and — the part that used to be missing — whether the division is
    // actually being *hit*. A plan is a claim about what should happen; the kernel's counters are
    // what did. The fleet had been throttling ninety thousand times an hour and the only place that
    // showed was a file nobody read.
    let squeeze = crate::fleet::pressure();
    let mut memory = match crate::fleet::memory_plan() {
        Some(plan) => {
            let divided = format!(
                "{} across all boxes and the containers they start, {} for the sandbox's own \
                 daemons, {} kept back for the VM's services and the kernel",
                gib(plan.boxes),
                gib(plan.plumbing),
                gib(plan.reserve)
            );
            match squeeze {
                // Something was killed for memory. **A fault, not a note**: whatever it was did not
                // finish, and the fix is a real one rather than advice to watch it.
                p if p.killed > 0 => HealthCheck::unsatisfied(
                    format!(
                        "{divided}. The kernel has killed {} process(es) for memory since skein \
                         last looked{}",
                        p.killed,
                        match p.docker_restarts {
                            0 => String::new(),
                            n => format!(", and the Docker daemon has been restarted {n} time(s)"),
                        }
                    ),
                    "give the fleet more memory (Settings → Fleet), or stop a box you are not \
                     using — `skein ls` shows what is holding it",
                ),
                // Sustained throttling is not a kill and is not nothing: it is every box getting
                // slower together, which is exactly what gets remembered as "skein felt slow" and
                // never reported. Said, and not raised to a fault, because the fleet is working.
                p if p.rated && p.throttled_per_min > THROTTLE_NOTICEABLE => {
                    HealthCheck::satisfied(format!(
                        "{divided}. It is at that ceiling now — {:.0} throttles a minute{}",
                        p.throttled_per_min,
                        match p.containers_throttled_per_min > THROTTLE_NOTICEABLE {
                            true => ", mostly from containers a box started",
                            false => "",
                        }
                    ))
                }
                _ => HealthCheck::satisfied(divided),
            }
        }
        // A fleet whose total is unset has no ceiling anywhere: not per box, not on the boxes
        // together, not on Docker. One build can then reach the VM's memory, and with no swap the
        // kernel's global OOM killer picks a victim by badness rather than by blame.
        // The fix REBUILDS the sandbox — sbx has no resize, so changing the size means a new
        // sandbox — which is why it is marked destructive even though `skein resize` carries every
        // box across. Nothing may drive this on its own.
        None => HealthCheck::unsatisfied(
            "no memory ceiling anywhere: not per box, not on the boxes together, not on Docker. \
             One build can reach the VM's memory, and with no swap the kernel picks a victim by \
             badness rather than by blame",
            "skein resize 26g   (or Settings → Fleet → Fleet memory; it rebuilds the sandbox and carries \
             every box's work across)",
        )
        .destroys(),
    };
    // The legacy single-repo registry. Managed repos are the supported path and the board does not
    // read this at all — it aggregates per-repo stores via `all_stores` — so a fleet with repos
    // registered is healthy whether or not a `sandboxes.json` exists anywhere.
    //
    // It used to be a hard failure, and on a clean install it failed *by construction*: with no
    // `$SKEIN_REGISTRY`, `locate_registry` falls back to a sibling `skein-shared/` directory named
    // after a different project, which no new user has. So the first thing anyone saw was the
    // product declaring itself broken, permanently, over a file it no longer needs — and every real
    // fault afterwards was noise in a banner that never cleared.
    let repos_registered = !crate::repos::load_repos().is_empty();
    // An unset registry is not a broken registry. It is a fault only when someone has *named* one —
    // `$SKEIN_REGISTRY` or `$SKEIN_SHARED` — and it cannot be read. With neither set and no repos
    // yet, the honest report is "nothing here yet"; the empty state already says to add a repo, and
    // a red banner repeating it is noise on the one screen that should be welcoming.
    let registry_named = std::env::var_os("SKEIN_REGISTRY")
        .or_else(|| std::env::var_os("SKEIN_SHARED"))
        .is_some_and(|v| !v.is_empty());
    let registry = match load_registry() {
        Ok((boxes, path)) => {
            HealthCheck::satisfied(format!("{} ({} boxes)", path.display(), boxes.len()))
        }
        Err(error) if repos_registered => HealthCheck::satisfied(format!(
            "not in use — {} repos are managed directly ({error})",
            crate::repos::load_repos().len()
        )),
        Err(error) if !registry_named => HealthCheck::satisfied(format!(
            "not in use — add a repository with `skein add <url>` ({error})"
        )),
        Err(error) => HealthCheck::unsatisfied(
            error.to_string(),
            "it is named by $SKEIN_REGISTRY or $SKEIN_SHARED — unset whichever is set, or point \
             it at a readable file",
        ),
    };
    let fleet = fleet_boxes();
    let fleet_degraded = fleet_degraded();
    let sbx = sbx_health(&fleet, fleet_degraded);
    let tool = |name: &str, required: bool| match (program_on_path(name), required) {
        (true, _) => HealthCheck::satisfied("available"),
        (false, true) => HealthCheck::unsatisfied(
            format!("`{name}` is not on PATH, and skein needs it"),
            format!("install {name}, or start the server from a shell whose PATH has it"),
        ),
        // Optional means optional: absent is a correct state, so it is not a fault and there is
        // nothing to fix.
        (false, false) => HealthCheck::satisfied("not found (optional)"),
    };
    let git = tool("git", true);
    // What actually reads GitHub. It used to be `gh`, which made a third-party CLI a hard
    // requirement of a default-on feature and dragged its keyring in with it; the queue now talks to
    // the API with a token skein already has. curl is what carries that, and gitgate has always
    // needed it to mint App tokens.
    //
    // And — since the scoped path reaches GitHub DIRECT (SKEIN-548) — whether GitHub is reachable at
    // all. A deny-by-default egress policy (SKEIN-926) that blocks GitHub does not answer a request;
    // it refuses the connection, and `crate::github::call` then fails as a transport error rather
    // than falling back to the proxy. So this line reports the block and the one command that clears
    // it, and clears ITSELF the next time the probe connects — a 401/403 is an answer, so only a
    // failure to connect at all counts as blocked.
    let gh = match crate::github::have_curl() {
        false => HealthCheck::unsatisfied(
            "curl is not installed, and skein reads GitHub with it — pull requests, diffs, merges, \
             and minting App tokens",
            "install curl",
        ),
        true => github_reach_health(&crate::place::fleet_sandbox()),
    };

    let repos = load_repos();
    let fleet_names = fleet.as_ref().map(|boxes| {
        boxes
            .iter()
            .map(|box_| box_.name.as_str())
            .collect::<BTreeSet<_>>()
    });
    let mut probe_errors = Vec::new();
    let mut mailbox_errors = Vec::new();
    for repo in &repos {
        let store = Path::new(&repo.store);
        // ONE cause, one line. A store that was never made is not eight missing probes and a
        // missing mailbox — it is a repo that never finished being added, and listing its
        // consequences separately buries the one fact that would fix all of them. Nine complaints
        // across two checks was the measured shape.
        if !store.is_dir() {
            probe_errors.push(format!(
                "{}: its store does not exist at {} — nothing is installed there because there is \
                 no there",
                repo.id,
                store.display()
            ));
            continue;
        }
        for relative in [
            "skein/probe-revision",
            "skein/runtimes.tsv",
            "skein/bin/box-status.sh",
            "skein/bin/mailbox.sh",
            "skein/bin/shared-home.sh",
            "skein/bin/agent-guide.sh",
            "skein/bin/install-codex-hooks.sh",
        ] {
            if !store.join(relative).is_file() {
                probe_errors.push(format!("{} missing {relative}", repo.id));
            }
        }
        if !store.join("mailbox").is_dir() {
            mailbox_errors.push(format!("{} mailbox directory missing", repo.id));
        }
        if !store.join("shared-home").is_dir() {
            probe_errors.push(format!("{} shared-home directory missing", repo.id));
        }
        let boot_dir = store.join("skein/boot");
        if let Ok(entries) = fs::read_dir(boot_dir) {
            for path in entries.flatten().map(|entry| entry.path()) {
                let box_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("box");
                if fleet_names
                    .as_ref()
                    .is_some_and(|names| !names.contains(box_name))
                {
                    continue;
                }
                let boot = fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
                let jq_available = boot.as_ref().and_then(|value| value.get("jq")?.as_bool());
                if jq_available == Some(false) {
                    mailbox_errors.push(format!("{box_name} is missing required jq"));
                }
                let tmux_available = boot.as_ref().and_then(|value| value.get("tmux")?.as_bool());
                if tmux_available == Some(false) {
                    probe_errors.push(format!("{box_name} is missing required tmux"));
                }
                if boot
                    .as_ref()
                    .and_then(|value| value.get("shared_home")?.as_str())
                    .is_some_and(|state| state != "linked")
                {
                    probe_errors.push(format!("{box_name} shared home is unavailable"));
                }
                if boot
                    .as_ref()
                    .and_then(|value| value.get("agent_guide")?.as_str())
                    .is_some_and(|state| state != "installed")
                {
                    probe_errors.push(format!("{box_name} durable agent guidance is unavailable"));
                }
            }
        }
    }
    let mut probes = match probe_errors.is_empty() {
        true => HealthCheck::satisfied(format!("installed for {} managed repos", repos.len())),
        false => HealthCheck::unsatisfied(
            probe_errors.join("; "),
            "restart the server, which recreates every repo's store and reinstalls the probes into \
             it; a box that is missing tmux or jq needs `skein restart <box>` after that",
        ),
    };
    let mailbox = match mailbox_errors.is_empty() {
        true => {
            HealthCheck::satisfied("shared stores and required jq available in reporting boxes")
        }
        false => HealthCheck::unsatisfied(
            mailbox_errors.join("; "),
            "a missing mailbox directory is created by restarting the server; a box missing jq \
             needs `skein restart <box>`, which reprovisions it",
        ),
    };
    let views = load_views().unwrap_or_default();
    let dark_boxes = views
        .iter()
        .filter(|view| view.hook_health == "never")
        .map(|view| view.name.clone())
        .collect::<Vec<_>>();
    let stale_boxes = views
        .iter()
        .filter(|view| view.hook_health == "stale")
        .map(|view| view.name.clone())
        .collect::<Vec<_>>();
    // Boxes holding a hook signal that says it is a different box's. See
    // [`crate::signals::hook_health`]: the file is in the store, well-formed and fresh, and it is
    // refused — so the box reports nothing while looking exactly like one that has nothing to say.
    //
    // Here rather than only on the row, and NOT folded into `dark_boxes`, because the two answers
    // send a person somewhere different: `dark_boxes` carries "`skein restart <box>`", and
    // restarting a box does not remove a file that is already on disk under the wrong name. This
    // one is a store to clean. Folding them would have given every misfiled box the recipe that
    // cannot fix it, which is worse than the silence it replaces.
    let misfiled_boxes = views
        .iter()
        .filter(|view| view.hook_health == "misfiled")
        .map(|view| view.name.clone())
        .collect::<Vec<_>>();
    // Boxes still living in the namespace an older `box-session.sh` built for them. See
    // [`crate::board::BoxView::cover`]: everything skein does about isolation it does at box start,
    // so a cover that lands in a new release reaches new boxes and no running one.
    let uncovered_boxes = views
        .iter()
        .filter(|view| view.cover == "older")
        .map(|view| view.name.clone())
        .collect::<Vec<_>>();
    // Running boxes nothing bounds. See [`crate::board::BoxView::ceiling`]: the launcher records
    // this in the box's own root, inside the sandbox, so until it started reporting it there was no
    // surface on which an uncapped box looked different from a capped one.
    let uncapped: Vec<(String, String)> = views
        .into_iter()
        .filter(|view| !view.ceiling.is_empty() && !crate::fleet::is_capped(&view.ceiling))
        .map(|view| (view.name, view.ceiling))
        .collect();
    let uncapped_boxes: Vec<String> = uncapped.iter().map(|(name, _)| name.clone()).collect();
    if !uncapped_boxes.is_empty() {
        // **A fault, and it belongs on the memory line rather than beside it.** The plan above can
        // be perfectly good and still not reach a box that never joined a cgroup — which is the box
        // that can take the sandbox down, since the ceiling is what "keeps one box's runaway build
        // from killing every other box" (`box-session.sh`). Reading "3.0 GiB across all boxes" with
        // no mention that one of them is outside that number is the reassuring half of the truth.
        memory.level = Level::Unsatisfied;
        memory.detail.push_str(&format!(
            ". {} running outside that ceiling entirely: {}",
            match uncapped_boxes.len() {
                1 => "One box is".to_string(),
                n => format!("{n} boxes are"),
            },
            uncapped_boxes.join(", ")
        ));
        // The two causes need different people. `no-limit-computed` is skein's own plan producing
        // nothing for this box; the other two are the sandbox refusing to delegate cgroups, which no
        // setting here fixes.
        // `no-limit-computed` means the box IS in a cgroup and skein wrote no ceiling onto it —
        // a restart puts it under the current plan. The other two mean it is in no cgroup at all,
        // which is the sandbox's answer and no setting here changes it.
        let skeins_own = uncapped
            .iter()
            .any(|(_, state)| state.contains("no-limit-computed"));
        memory.fix = match skeins_own {
            true => format!(
                "`skein restart {}` — it started before this fleet had a memory plan, and a \
                 restart puts it under the current one",
                uncapped_boxes
                    .first()
                    .map(String::as_str)
                    .unwrap_or("<box>")
            ),
            false => "this sandbox does not delegate cgroups, so skein cannot bound a box in it — \
                      the ceilings on the fleet as a whole still hold, but one box's build \
                      can reach all of them"
                .to_string(),
        };
    }
    if !dark_boxes.is_empty() {
        probes.level = Level::Unsatisfied;
        probes.detail.push_str(&format!(
            "; no signals from running boxes: {}",
            dark_boxes.join(", ")
        ));
        // The check may already have carried a fix for a missing probe file; this reason has its
        // own, and a fault must never be left with an empty one.
        probes.fix = format!(
            "`skein restart {}` — a box whose probes have never reported was started before they \
             were installed",
            dark_boxes.first().map(String::as_str).unwrap_or("<box>")
        );
    }
    if !misfiled_boxes.is_empty() {
        probes.level = Level::Unsatisfied;
        probes.detail.push_str(&format!(
            "; hook signals filed under the wrong box's name, so they are refused: {}",
            misfiled_boxes.join(", ")
        ));
        // Only when nothing else has already claimed the fix line: a dark box's restart is the
        // more urgent of the two, and a check may carry exactly one recipe.
        if probes.fix.is_empty() || dark_boxes.is_empty() {
            probes.fix = format!(
                "remove the misfiled signal — `ls ~/.skein/repos/*/store/.claude/{{status,sessions,\
                 tasks}}/{}.json` and delete the one whose `box` field names a different box — then \
                 reattach the box, since the attach is what exports SKEIN_BOX to its probes",
                misfiled_boxes.first().map(String::as_str).unwrap_or("<box>")
            );
        }
    }
    // Deliberately NOT reported here: a box on hook-only turn state (see `screen_health`) is not
    // unhealthy — it degrades to exactly its pre-observer behaviour. Nagging in the environment
    // banner would be crying wolf; the caveat belongs on the row and tab it applies to.
    let cover = cover_health(&uncovered_boxes);
    // Behind the same 30s gate the resources overlay reads, so a doctor run and an open cockpit
    // cost one measurement between them.
    let disk = disk_health();
    let gitgate = git_scope_health();
    // Asked through the gate rather than directly, so as many open tabs as you like cost one probe
    // per ten seconds between them, and a warden that has gone slow is asked progressively less
    // often instead of being handed a fresh connection every fifteen.
    let warden = warden_health(crate::warden_client::sighting());
    // `token_expiry` is `Counted` on the banner for the reason `cover` is: a credential inside its
    // renewal window has no symptom whatsoever until the day it stops working, and on that day the
    // symptom is every box at once. An `unknown` here — GitHub unreachable, no credential answered
    // — is not a fault and `is_fault` already says so.
    let token_expiry = token_expiry_health();
    // `proxy_injection` is `Counted` for the reason `token_expiry` is, one step further on: an
    // injected account credential has no symptom at all from inside a box — every request simply
    // works — so the first sign of it is somebody else's repository in a diff. skein cannot close
    // it, which is exactly why it has to be the thing that says it is open (SKEIN-548). An
    // `unknown` here is a rate limit or an unreachable proxy and `is_fault` already declines it.
    let proxy_injection = proxy_injection_health(&crate::place::fleet_sandbox());

    // **`ok` is derived from the one list and no longer written out beside it** (SKEIN-1003).
    // What stood here was a second array — eleven `&field` references with nothing tying them to
    // the struct — and it had exactly the hole SKEIN-1000 closed in `checks_with_banner` above: a
    // check could be added to the report and never reach the verdict, with nothing anywhere saying
    // whether that was a decision. Three had. `gh` was the one that cost something: it is
    // "curl is installed" and "GitHub can be reached at all", so a fleet with no route to GitHub
    // had `ok == true` and showed no banner whatever.
    //
    // The report is built first and its verdict written onto it, because the verdict is now a
    // function of the report. `ok: false` here is not a default that could survive — the next
    // statement overwrites it unconditionally.
    let mut report = HealthReport {
        ok: false,
        // Overwritten beside `ok`, for the same reason: it is read off the report once it exists.
        counted: Vec::new(),
        build: BUILD_REVISION,
        labels: &CHECK_LABELS,
        registry,
        sbx,
        git,
        gh,
        probes,
        mailbox,
        ai,
        memory,
        disk,
        gitgate,
        token_expiry,
        proxy_injection,
        warden,
        cover,
        logins: crate::fleet::signed_in_runtimes(),
        expired_logins: crate::fleet::expired_logins(),
        runtime_updates: runtime_updates(),
        models: crate::ai::model_choices(),
        dark_boxes,
        stale_boxes,
        uncovered_boxes,
        uncapped_boxes,
        unowned: crate::fleet::unowned_report(),
        runtimes: supported_runtimes(),
        git_credential: crate::gitgate::box_credential().label(),
    };
    // Stale sessions are the second half and are not a check: there is no `HealthCheck` for them,
    // only a list of box names, and the banner prints the count rather than a sentence.
    report.ok = report.first_counted_fault().is_none() && report.stale_boxes.is_empty();
    report.counted = report.counted_on_the_wire();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One cause, one line — measured, because the alternative is nine.
    ///
    /// A repo whose store does not exist produced eight "missing" complaints from the probe check
    /// and one from the mailbox check: nine symptoms of a repo that never finished being added, and
    /// no way for a reader to see that they were one thing. Every one of them clears when the store
    /// is made, and none of them is separately actionable.
    #[test]
    fn a_repo_with_no_store_is_one_fault_and_not_nine() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        // A stand-in for the crossing. `health_report` reads the machine's facts through a
        // fleet-scope command, and `Place::spawning` refuses a test process that installed no
        // stand-in rather than running one for real (SKEIN-530). It is also what makes the
        // disk verdict below the same on every machine — see the note there.
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // A fleet root with nothing at it, for the reason spelled out in
        // `a_missing_tool_is_one_fault_and_not_five`: unpinned, `health_report` measures the live
        // fleet at `/boxes` and walks the boxes it finds there. This test counts complaints about
        // one repo's missing store, and it should count the same number on every machine.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("no-fleet-here"));
        // A repo registered against a store nobody made — `skein add` interrupted, or a volume
        // mounted somewhere else since.
        crate::repos::save_repos(&[crate::repos::Repo {
            read_prs: false,
            id: "orphan".into(),
            source: "https://github.com/a/b".into(),
            store: home.join("gone/.claude").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }])
        .unwrap();

        let report = report_with(Vec::new);
        let complaints = report.probes.detail.matches(';').count() + 1;
        assert_eq!(
            complaints, 1,
            "one missing store produced {complaints} complaints: {}",
            report.probes.detail
        );
        assert!(
            report.probes.detail.contains("its store does not exist"),
            "the one complaint must name the cause rather than a symptom: {}",
            report.probes.detail
        );
        assert!(
            !report.mailbox.is_fault(),
            "the mailbox check repeated the same cause: {}",
            report.mailbox.detail
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// A missing tool is one fault, and does not make its dependents look broken too.
    ///
    /// The property the tri-state bought, pinned so it cannot be lost. `sbx` is how skein reaches
    /// every box, so the intuition is that losing it should light up the whole report — and the
    /// intuition is wrong, which is exactly why this is worth asserting: the other checks are
    /// answered from the host, and the ones that would need the fleet report `unknown` rather than
    /// inventing a fault. Five red cards for one cause is the failure this rules out.
    ///
    /// `warden` is in the allowed list beside the three tools, and it is not one: it is a service on
    /// a port. Same category all the same — an absent dependency skein needs, reported once, with
    /// one command that clears it — and the property being pinned is unchanged, that its absence
    /// must not make anything downstream of it look broken too.
    ///
    /// It reads the machine's own PATH rather than blanking it, and that is not laziness. `PATH` is
    /// process-global and the suite runs in parallel: an earlier version set it to a directory that
    /// does not exist, and a sibling test that shells out failed while it held it. A test that makes
    /// other tests fail is worse than one that is only sharp on some machines — and it is sharp
    /// wherever a tool is genuinely absent.
    ///
    /// It used to force the host deployment before reading the report, because `sbx` was the
    /// missing tool it counted on: in-fleet `sbx_health` is satisfied by construction, so running
    /// the suite from inside the fleet flipped its subject out from under it (SKEIN-471). With one
    /// deployment left (SKEIN-576) there is nothing to force and `sbx` is no longer one of the
    /// tools that can be missing — so it comes off both lists below, and `git` carries the
    /// "is it sharp at all" half.
    ///
    /// The same thing happened a second time and from the other side: with `$SKEIN_FLEET_ROOT`
    /// unset the report measured the disk of the fleet this suite runs on, so the verdict became a
    /// reading of somebody's free space (SKEIN-690). What the fixture has to be, and why an
    /// ordinary temp directory is not enough, is in the test.
    #[test]
    fn a_missing_tool_is_one_fault_and_not_five() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        // A stand-in for the crossing. `health_report` reads the machine's facts through a
        // fleet-scope command, and `Place::spawning` refuses a test process that installed no
        // stand-in rather than running one for real (SKEIN-530). It is also what makes the
        // disk verdict below the same on every machine — see the note there.
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // **A fleet root that does not exist, and the "does not exist" is the load-bearing half.**
        // Unpinned, `fleet_root()` is `/boxes` — the live fleet on any machine running skein — and
        // `disk_health` measures it with `df`. That is what made this test's verdict track the
        // machine's free space: at 91% used it FAILED 3 of 3 runs and at 74% it passed 3 of 3, same
        // binary, and the message accused `health.rs` of inventing a fault (SKEIN-690).
        //
        // A pin at an ordinary fixture directory does not fix that, and this was measured rather
        // than assumed: pointed at the tempdir above, the check came back "the boxes' disk is 77%
        // full" — the same overlay, because `/tmp` and `/boxes` are one filesystem here. The
        // threshold is 85%, so the test would still fail on a full machine.
        //
        // The check is `Unknown` here — which is the state this test's own message describes as "a
        // machine with no fleet", and the state the tri-state exists to have. It is asserted below
        // rather than left as a happy accident.
        //
        // **By which route was wrong here until SKEIN-770 measured it.** This said `df` prints no
        // row and the sandbox answers without disk figures — `disk_verdict`'s `disk_total == 0`
        // arm. It never gets that far: `seam::doing_nothing` above substitutes `sh -c :`, so the
        // measuring command succeeds with EMPTY output, `parse_resources` finds no `mem_total` and
        // `fleet_resources` answers `None`, which is `disk_health`'s own arm. The assertion below
        // holds either way, which is exactly why the wrong route went unnoticed; the pin on
        // `SKEIN_FLEET_ROOT` is still load-bearing for every other check in the report.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("no-fleet-here"));
        let report = report_with(Vec::new);
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");

        assert_eq!(
            report.disk.level,
            Level::Unknown,
            "the disk check answered from a real filesystem, so this test's verdict is again a \
             reading of how full the machine running it happens to be: {}",
            report.disk.detail
        );

        let faults: Vec<&str> = report
            .checks()
            .into_iter()
            .filter(|(_, check)| check.is_fault())
            .map(|(name, _)| name)
            .collect();
        assert!(
            faults
                .iter()
                .all(|name| [label("git"), label("gh"), label("warden")].contains(name)),
            "something that is not a tool is reported broken, which on a machine with no fleet \
             means a check invented a fault out of a question it could not put: {faults:?}"
        );
        // A missing `sbx` is not among them, and that is asserted rather than left to the list
        // above — the list is a permission and this is the specific thing it must not permit.
        assert!(
            !faults.contains(&"sbx"),
            "a host-only tool skein does not use was reported as broken: {faults:?}"
        );
        // And where a tool IS missing it is named, so this is not passing by finding nothing.
        if !program_on_path("git") {
            assert!(
                faults.contains(&"git"),
                "git is not on this PATH and the report does not say so: {faults:?}"
            );
        }
    }

    /// **No fault without a way out.** The parent property, in the only form that can be enforced.
    ///
    /// A recipe written by hand per check is right where somebody thought of it, and absent where
    /// they did not — and the check that nobody thought about is the one somebody is staring at.
    /// This walks the whole report, so a check added later with no fix fails here rather than in
    /// front of a person who is stuck. "The whole report" and "this machine's report" used to be
    /// the same sentence, and they are not: the property is about every check having a recipe, and
    /// it holds for any home and any fleet. What the second reading cost was real — unpinned, this
    /// resolved the owner's live `~/.skein`, recursively stat'd every box tree under `/boxes`, and
    /// opened a session socket per running box, all to assert something about strings (SKEIN-530,
    /// SKEIN-646). It passed only because a neighbour in this process had left `$SKEIN_HOME` set;
    /// alone, the guard from SKEIN-626 refuses it.
    #[test]
    fn every_fault_says_what_would_fix_it() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        // A stand-in for the crossing. `health_report` reads the machine's facts through a
        // fleet-scope command, and `Place::spawning` refuses a test process that installed no
        // stand-in rather than running one for real (SKEIN-530). It is also what makes the
        // disk verdict below the same on every machine — see the note there.
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // Both, because this reaches a fleet path as well as a home: `$SKEIN_FLEET_ROOT` unset is
        // `/boxes`, and the disk and liveness checks act on what they find there.
        let fleet = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &fleet);
        let report = report_with(Vec::new);
        // The one test that builds the real report, so the one place that can see `health_report`
        // write `counted` at all: a report that left it at its placeholder would send the page an
        // empty list, and every fault would lose its headline to the stale-session line
        // (SKEIN-1013). What the list holds is `the_report_tells_the_page_which_checks_count`'s.
        assert_eq!(
            report.counted,
            report.counted_on_the_wire(),
            "`health_report` sends `counted` as something other than what `OnBanner` says, so the \
             page cannot tell which fault is the reason its banner is up (SKEIN-1013)"
        );
        for (name, check) in report.checks() {
            if check.is_fault() {
                assert!(
                    !check.fix.trim().is_empty(),
                    "`{name}` is a fault with no way out: {}",
                    check.detail
                );
            } else {
                assert!(
                    check.fix.is_empty(),
                    "`{name}` is not a fault and offers a fix anyway: {}",
                    check.fix
                );
            }
        }
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The `HealthCheck` fields of [`HealthReport`], read out of this file's own source text.
    ///
    /// Shared by the two tests below because they are two halves of one question — the struct, the
    /// list and the page all naming the same set — and a second copy of this extraction is exactly
    /// the shape of defect they exist to catch.
    fn check_fields(source: &str) -> std::collections::BTreeSet<&str> {
        let struct_at = source
            .find("pub struct HealthReport {")
            .expect("`HealthReport` is not declared the way this test finds it");
        let struct_end = struct_at
            + source[struct_at..]
                .find("\n}\n")
                .expect("`HealthReport` is never closed at column 0");
        source[struct_at..struct_end]
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub "))
            .filter_map(|rest| rest.split_once(": "))
            .filter(|(_, ty)| *ty == "HealthCheck,")
            .map(|(name, _)| name)
            .collect()
    }

    /// **The one list is the whole list**: every `HealthCheck` field on [`HealthReport`] is named in
    /// [`HealthReport::checks_with_banner`], read out of this file's own source text (SKEIN-1000).
    ///
    /// **Be exact about what restores the promise and what this adds, because they are not the same
    /// thing.** That function's doc comment promises that a forgotten check is a compile error. What
    /// delivers that is the *pattern* — with no `..`, a field added to the struct does not compile
    /// until it is named on one side or the other — and no `#[test]` can assert it, because a
    /// compile error is not a test outcome. This test does not restore that property and must not
    /// be read as evidence of it; there is no compile-fail harness in this tree to assert it with.
    ///
    /// What it does is cover the two ways the promise goes quiet again:
    ///
    /// * **The `..` comes back**, and with it the silence this item is about. The first assertion
    ///   is about the source construct because the promise is made of the source construct.
    /// * **A check is named and then thrown away.** `token_expiry: _` in the discard block compiles
    ///   perfectly, satisfies the pattern, and puts the field back in the dark. The set comparison
    ///   catches that by name — which is why it compares names and not just [`CHECK_COUNT`].
    ///
    /// [`CHECK_COUNT`]: HealthReport::CHECK_COUNT
    ///
    /// Two things it does **not** see, said plainly so nobody reads more into a green run:
    ///
    /// * a check whose field is not spelled `HealthCheck` — a type alias, or an
    ///   `Option<HealthCheck>` — since the struct half matches that type literally. Every check
    ///   field is a bare `HealthCheck` today; one that is not would be invisible here while still
    ///   failing to compile in the pattern, so the half that is missing is the cheaper half.
    /// * whether a key is a name anybody can act on, or whether the check reaches a surface.
    ///   `every_check_has_one_label_and_every_surface_reads_it` below is that half, for the page
    ///   and the CLI; nothing checks the wording, and nothing can.
    ///
    /// **The concrete changes that make it fail, named before it was written:** putting `..` back
    /// in the pattern fails `the destructuring must not end in ..`; turning `token_expiry,` into
    /// `token_expiry: _` and deleting its array entry fails `every check field is in the list`.
    #[test]
    fn every_health_check_field_is_named_in_the_list() {
        let source = include_str!("report.rs");
        // The first occurrence of each anchor is the definition, which is above this test module —
        // so the copies of these strings in this function's own body are never what gets read.
        let fn_at = source.find("pub fn checks_with_banner(").expect(
            "`checks_with_banner` is not declared the way this test finds it — did it get renamed?",
        );
        let pattern_end = fn_at
            + source[fn_at..].find("} = self;").expect(
                "`checks_with_banner` no longer destructures `self`, so this test reads nothing",
            );
        assert!(
            !source[fn_at..pattern_end].contains(".."),
            "the destructuring must not end in `..`: that is the whole of what makes a forgotten \
             check a compile error, and while it was there `token_expiry` and `proxy_injection` \
             both reached the struct without reaching this list, so the cockpit went red over a \
             credential the work queue could not name (SKEIN-1000)"
        );

        // The array literal, by bracket depth rather than by a closing spelling — a `];` or an
        // indent is a guess about rustfmt, and this is not.
        let open = pattern_end
            + source[pattern_end..]
                .find('[')
                .expect("`checks_with_banner` returns no array literal");
        let mut depth = 0usize;
        let mut close = open;
        for (offset, ch) in source[open..].char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        close = open + offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        // An entry is a top-level `( … )` group, found by paren depth rather than one per line:
        // rustfmt breaks a tuple across lines once it is wider than its limit, and the wire-name
        // column put `("proxy credential", "proxy_injection", …)` over it (SKEIN-1013). Comments
        // go first, because the ones between entries carry parentheses of their own.
        let code: String = source[open + 1..close]
            .lines()
            .map(|line| line.split("//").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" ");
        let mut groups = Vec::new();
        let (mut depth, mut from) = (0usize, 0usize);
        for (at, ch) in code.char_indices() {
            match ch {
                '(' => {
                    if depth == 0 {
                        from = at + 1;
                    }
                    depth += 1;
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        groups.push(
                            code[from..at]
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" "),
                        );
                    }
                }
                _ => {}
            }
        }
        // `splitn(3, ", ")` and not `rsplit_once`, because the last column is the banner
        // disposition and a `NotCounted` reason is prose that may hold a comma of its own. The
        // field binding is the second column; the first is its name on the wire (SKEIN-1013).
        let entries: Vec<[String; 3]> = groups
            .iter()
            .map(|group| {
                let inner = group.trim_end_matches(',');
                let mut columns = inner.splitn(3, ", ");
                let mut next = || {
                    columns
                        .next()
                        .unwrap_or_else(|| {
                            panic!(
                                "fewer than three columns in `({group})`, so it is not a \
                                    `(wire, field, banner)` entry"
                            )
                        })
                        .trim()
                        .to_string()
                };
                [next(), next(), next()]
            })
            .collect();
        // The wire name is the binding beside it, spelled as a string. The binding is the field
        // (the destructuring above makes that a compile error otherwise) and serde writes a field
        // under its own name, so this equality is what makes `counted` a list of keys the page can
        // actually find in the report rather than labels it cannot (SKEIN-1013).
        for [wire, field, _] in &entries {
            assert_eq!(
                wire.trim_matches('"'),
                field,
                "an entry says its name on the wire is {wire}, and serde writes it as `{field}` — \
                 the page would look for a key the report never sends, so a counted fault there \
                 could never be the banner's headline (SKEIN-1013)"
            );
        }
        // Every entry states its disposition, and it is one of the two the type has. A third
        // variant added without a reader here would otherwise be counted as neither.
        for [key, _, banner] in &entries {
            assert!(
                banner == "Counted" || banner.starts_with("NotCounted("),
                "{key} states its place on the banner as `{banner}`, which this test does not \
                 know how to read — every entry says `Counted` or `NotCounted(why)` (SKEIN-1003)"
            );
        }
        let listed: std::collections::BTreeSet<&str> =
            entries.iter().map(|[_, field, _]| field.as_str()).collect();
        // Proves the extraction read the WHOLE array before anything is concluded from it: a scan
        // that stopped early would otherwise report the fields it never reached as missing, which
        // is a red that sends the reader to the wrong file.
        assert_eq!(
            listed.len(),
            HealthReport::CHECK_COUNT,
            "this test found {} of `checks_with_banner`'s {} entries — it is mis-parsing the \
             array, not finding a bug in it: {listed:?}",
            listed.len(),
            HealthReport::CHECK_COUNT
        );

        assert_eq!(
            check_fields(source),
            listed,
            "every check field is in the list, and these two are not the same set. A field on the \
             left and not the right is a check nothing looks at — `who_needs_you` builds the work \
             queue from `checks()` alone, so it cannot produce a row for one (SKEIN-1000). A name \
             on the right and not the left is a binding this test cannot match to a field."
        );
    }

    /// **Every check has one label, and the banner, the diagnostics pane and `skein doctor` all
    /// read it from [`CHECK_LABELS`]** (SKEIN-1186).
    ///
    /// The same check used to have up to three names one click apart: the banner headlined with
    /// the wire key, the diagnostics pane kept a table of its own in the page, and the CLI and the
    /// work queue used a third. What holds them together now is that there is one table and each
    /// surface reads it — the page from the report's `labels`, the CLI through [`label`] — so this
    /// asserts the table is complete and in the report's order, that it goes out on the wire, that
    /// the page keeps no table of its own, and that `doctor` asks for the label of a real check
    /// every time it prints one. `tests/ui/healthban.mjs` drives the banner and the pane from the
    /// served labels, which is the half a source-reading test cannot see.
    ///
    /// **The concrete changes that make it fail, named before they were written:** swapping two
    /// rows of `CHECK_LABELS` fails `in the report's order`; writing `"{mark} git scope     "`
    /// back into `skein doctor` fails `doctor prints every check it shows under the report's
    /// label`; putting `const CHECKS = [` back into `panels.js` fails `the page keeps no label
    /// table of its own`. The first two planted.
    #[test]
    fn every_check_has_one_label_and_every_surface_reads_it() {
        let report = every_check_satisfied();
        let order: Vec<&str> = report
            .checks_with_banner()
            .iter()
            .map(|(key, _, _)| *key)
            .collect();
        let labelled: Vec<&str> = CHECK_LABELS.iter().map(|c| c.key).collect();
        assert_eq!(
            labelled, order,
            "`CHECK_LABELS` must name every check exactly once, in the report's order — the \
             order is the page's, so the first counted fault it finds is `first_counted_fault`'s"
        );
        let names: std::collections::BTreeSet<&str> =
            CHECK_LABELS.iter().map(|c| c.label).collect();
        assert_eq!(
            names.len(),
            HealthReport::CHECK_COUNT,
            "two checks share a label, so a reader cannot tell which one is failing: {:?}",
            CHECK_LABELS
        );
        assert!(
            CHECK_LABELS.iter().all(|c| !c.label.trim().is_empty()),
            "a check with an empty label prints nothing where its name should be"
        );

        // On the wire, where the page reads it.
        let wire = serde_json::to_value(&report).expect("a health report serialises");
        let served: Vec<(&str, &str)> = wire["labels"]
            .as_array()
            .expect("`labels` goes out as a list")
            .iter()
            .map(|l| {
                (
                    l["key"].as_str().expect("a label has a key"),
                    l["label"].as_str().expect("a label has a label"),
                )
            })
            .collect();
        assert_eq!(
            served,
            CHECK_LABELS
                .iter()
                .map(|c| (c.key, c.label))
                .collect::<Vec<_>>(),
            "the report does not serve the labels the CLI prints"
        );

        // The page renders from them, and keeps no table of its own to drift from them.
        let page = include_str!("../web/index.html");
        for own in ["const CHECKS = [", "const CHECKED = ["] {
            assert!(
                !page.contains(own),
                "the page keeps no label table of its own, and `{own}` is one: the banner and the \
                 diagnostics pane render from the report's `labels`"
            );
        }

        // `skein doctor` prints five of the checks one at a time. Each asks for the label.
        let cli = include_str!("../bin/skein.rs");
        let asked: std::collections::BTreeSet<&str> = cli
            .split("skein::health::label(\"")
            .skip(1)
            .filter_map(|rest| rest.split_once('"').map(|(key, _)| key))
            .collect();
        for key in &asked {
            assert!(
                !label(key).is_empty(),
                "`skein doctor` asks for the label of `{key}`, which is not a check"
            );
        }
        assert_eq!(
            asked,
            ["cover", "disk", "gitgate", "token_expiry", "warden"]
                .into_iter()
                .collect(),
            "doctor prints every check it shows under the report's label — a check it prints with \
             a name typed into the CLI is a second name for it"
        );
    }

    /// **A fleet with no warden is not a fault and cannot turn the banner red** (SKEIN-1184).
    ///
    /// The owner's decision (`docs/decisions/warden-or-prompt.md`) makes the warden optional, and
    /// the first screen used to disagree twice: the checklist waited on it and the banner went red
    /// over it, with a fix that compiled Rust on the host. This is the banner half; the checklist
    /// half is `tests/ui/onboarding.mjs`, against the real page.
    ///
    /// **The concrete change that makes it fail, named before it was written:** marking `warden`
    /// `Counted` again fails `even a warden that answers wrongly does not raise the banner`.
    /// Planted.
    #[test]
    fn a_missing_warden_does_not_turn_the_banner_red() {
        let _g = crate::testutil::env_lock();
        let _warden = crate::testutil::no_warden();
        let mut report = every_check_satisfied();
        // The real line for a warden nobody started, not a fixture of one.
        report.warden = warden_health(crate::warden_client::sighting());
        assert!(
            !report.warden.is_fault(),
            "no warden answering is reported as a fault: {:?}",
            report.warden
        );
        assert_eq!(report.first_counted_fault(), None);
        // And the stronger half, which is the disposition itself: even the one warden state that
        // IS a fault — something answering on its port that is not a warden — is for the
        // diagnostics pane, not for the banner.
        report.warden = HealthCheck::unsatisfied("something else is on that port", "check it");
        assert_eq!(
            report.first_counted_fault(),
            None,
            "even a warden that answers wrongly does not raise the banner: the warden is optional, \
             and a banner red over a part somebody chose not to run is read past next time"
        );
        assert!(
            !report.counted_on_the_wire().contains(&"warden"),
            "the page is told the warden may headline the banner"
        );
        // Non-vacuity: the same report does go red for a counted check.
        report.disk = HealthCheck::unsatisfied("the disk is full", "clear it");
        assert_eq!(report.first_counted_fault(), Some("disk"));
    }

    /// A report built here with every check satisfied, so that one check's level is the only
    /// thing a test moves. `health_report`'s own fixture cannot do this job: it has a fault of its
    /// own (no warden answers a test process, by design), so `ok` is already false there.
    ///
    /// The literal names all 28 fields, which is deliberate and costs nothing to keep: a field
    /// added to `HealthReport` stops this compiling, in the same breath as the destructuring in
    /// `checks_with_banner`.
    fn every_check_satisfied() -> HealthReport {
        let satisfied = || HealthCheck::satisfied("nothing wrong with this one");
        HealthReport {
            // `true` would be a lie the tests then assert around: `ok` is whatever
            // `first_counted_fault` says, and that is what they read.
            ok: false,
            counted: Vec::new(),
            build: BUILD_REVISION,
            labels: &CHECK_LABELS,
            registry: satisfied(),
            sbx: satisfied(),
            git: satisfied(),
            gh: satisfied(),
            probes: satisfied(),
            mailbox: satisfied(),
            ai: satisfied(),
            memory: satisfied(),
            disk: satisfied(),
            gitgate: satisfied(),
            token_expiry: satisfied(),
            proxy_injection: satisfied(),
            warden: satisfied(),
            cover: satisfied(),
            logins: Vec::new(),
            expired_logins: Vec::new(),
            runtime_updates: Vec::new(),
            models: Vec::new(),
            dark_boxes: Vec::new(),
            stale_boxes: Vec::new(),
            uncovered_boxes: Vec::new(),
            uncapped_boxes: Vec::new(),
            unowned: None,
            runtimes: Vec::new(),
            git_credential: String::new(),
        }
    }

    /// **What turns the banner red is `OnBanner::Counted` and nothing else** (SKEIN-1003).
    ///
    /// The arithmetic on its own, against [`every_check_satisfied`], so that one check's level is
    /// the only thing moving — an assertion that `ok` *becomes* false could not fail against
    /// `health_report`'s own fixture, which is false already.
    ///
    /// **The concrete changes that make it fail, named before it was written:** marking `gh`
    /// `NotCounted` fails `a counted check decides it`; marking `ai` `Counted` fails `a check the
    /// banner does not count cannot raise it`.
    #[test]
    fn only_a_counted_check_turns_the_banner_red() {
        let mut report = every_check_satisfied();
        // The absence has to have been a presence: a report that was never clean would make every
        // assertion below unfalsifiable.
        assert_eq!(
            report.first_counted_fault(),
            None,
            "the fixture starts with every check satisfied, so nothing can be a fault in it yet"
        );

        report.ai = HealthCheck::unsatisfied("the toggle is off", "turn it on");
        assert_eq!(
            report.first_counted_fault(),
            None,
            "a check the banner does not count cannot raise it — `ai` is `NotCounted`, and a \
             banner that goes red for a reason its owner knows is fine is a banner people learn \
             to read past (SKEIN-913)"
        );

        report.gh = HealthCheck::unsatisfied(
            "GitHub could not be reached at all",
            "allow egress to github.com",
        );
        assert_eq!(
            report.first_counted_fault(),
            Some("gh"),
            "a counted check decides it, and `gh` is the case the item was filed for: it is \
             \"curl is installed\" and \"GitHub can be reached at all\", and while `ok` was a \
             hand-written list that did not name it, a fleet with no route to GitHub showed no \
             banner whatever (SKEIN-1003)"
        );
    }

    /// **The report tells the page which checks may raise its banner, under the names the page
    /// reads them by** (SKEIN-1013).
    ///
    /// The banner's headline is one sentence, and the owner's rule is that it is always the reason
    /// the banner is up — so the page takes it from the first failing check that is `Counted`. It
    /// cannot know which those are unless the report says: `OnBanner` is the one place that
    /// decides, and a page with its own list would be the second (SKEIN-1003). This reads the
    /// report *as serde writes it*, because the property is about the wire and not about a Rust
    /// value: a name in `counted` that is not a key of the serialised report is a check the page
    /// can never find, and its fault could never be the headline.
    ///
    /// **The concrete changes that make it fail, named before it was written:** mapping
    /// `counted_on_the_wire` to the first column (the label) instead of the second fails
    /// `every name counted sends is a check the report carries`, on "token life", "proxy
    /// credential" and "isolation"; leaving `disk` out of `counted_on_the_wire` fails `the checks
    /// the report does not offer are exactly the NotCounted ones`; marking `ai` `Counted` empties
    /// both sides of that comparison, so it is the last assertion that fails — `ai` is
    /// `NotCounted` today — which is what that assertion is there for. All three planted.
    #[test]
    fn the_report_tells_the_page_which_checks_count() {
        let mut report = every_check_satisfied();
        report.counted = report.counted_on_the_wire();
        let wire = serde_json::to_value(&report).expect("a health report serialises");
        let wire = wire.as_object().expect("a health report is a JSON object");
        // Every key serde wrote that is shaped like a check. Read off the wire rather than off
        // `check_fields`, so a rename on either side is a mismatch here rather than a match on
        // two copies of the same mistake.
        let checks: std::collections::BTreeSet<&str> = wire
            .iter()
            .filter(|(_, value)| value.get("level").is_some())
            .map(|(key, _)| key.as_str())
            .collect();
        // The whole report was read before anything is concluded from it: an empty set would make
        // both assertions below true of nothing.
        assert_eq!(
            checks.len(),
            HealthReport::CHECK_COUNT,
            "found {} check-shaped keys on the wire and the report has {} checks: {checks:?}",
            checks.len(),
            HealthReport::CHECK_COUNT
        );
        let counted: std::collections::BTreeSet<&str> = wire["counted"]
            .as_array()
            .expect("`counted` goes out as a list")
            .iter()
            .map(|name| name.as_str().expect("`counted` is a list of names"))
            .collect();
        let strangers: Vec<&&str> = counted.difference(&checks).collect();
        assert!(
            strangers.is_empty(),
            "every name counted sends is a check the report carries, and these are not keys of \
             the report at all: {strangers:?}. The page looks a check up by the name serde gives \
             it, so a fault under any of these could never be the banner's headline (SKEIN-1013)"
        );
        let not_counted: std::collections::BTreeSet<&str> = report
            .checks_with_banner()
            .into_iter()
            .filter(|(_, _, banner)| matches!(banner, OnBanner::NotCounted(_)))
            .map(|(wire, _, _)| wire)
            .collect();
        assert_eq!(
            checks
                .difference(&counted)
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            not_counted,
            "the checks the report does not offer are exactly the NotCounted ones. One missing \
             from `counted` loses the headline to a check that cannot raise the banner; one extra \
             hands the headline to a check whose fault is not why the banner is up"
        );
        assert!(
            !not_counted.is_empty() && !counted.contains("ai"),
            "`ai` is `NotCounted` today, so this fixture has something to leave out — if that \
             changed, the assertion above compares two empty sets and says nothing"
        );
    }
}
