//! Is skein's own environment sound? One report the cockpit and `skein doctor` both render, so a
//! misconfiguration is diagnosed in one place rather than guessed at from a failure downstream.

use crate::board::load_views;
use crate::registry::load_registry;
use crate::repos::load_repos;
use crate::runtime::*;
use crate::sbx::{fleet_boxes, fleet_degraded};
use crate::util::*;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// What a check answered. **Three states, and the third is the point.**
///
/// A binary check makes "the daemon is wedged" and "the fleet is absent" indistinguishable, and
/// anything that reconciles answers that ambiguity by doing the work again — creating a fleet that
/// already exists. The codebase already knew this in one place and said so:
/// [`crate::fleet::fleet_exists`] returns `Option<bool>` with exactly this comment. This is that
/// knowledge, everywhere a check is made.
///
/// **`Unknown` may never drive a doer.** It may only be reported. Whatever would act on
/// `Unsatisfied` must do nothing at all on `Unknown` — the honest response to "I could not tell" is
/// to say so and wait, never to guess in the direction that happens to be cheap to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Checked, and it holds.
    Satisfied,
    /// Checked, and it does not. This is the only state that is a fault.
    Unsatisfied,
    /// Could not be checked. Not a fault, and not a pass either.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    pub level: Level,
    /// What is true. The diagnosis, and only the diagnosis.
    pub detail: String,
    /// **What would clear it** — architecture §2.4's `recipe`, and the reason the parent property
    /// holds at all: skein can only be blocked in a way it can explain if the check that found the
    /// block carries the way out with it.
    ///
    /// A command where there is one, so it can be copied rather than transcribed. Prose where the
    /// answer is a place in the UI rather than a command, because "Settings → Fleet → memory" is
    /// the honest recipe for a setting and inventing a CLI for it would not be.
    ///
    /// Empty for a satisfied or unknown check — there is nothing to fix, and nothing known to be
    /// wrong. **Never empty for an unsatisfied one**, which the tests enforce rather than trust.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fix: String,
    /// Would running the fix destroy something? §2.4's `destructive` class.
    ///
    /// A destructive recipe is **printed and never run**. Nothing auto-drives one however
    /// unsatisfied its check is, because the cost of being wrong is not a wasted minute — it is a
    /// sandbox with every box's unpushed work on it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
}

impl HealthCheck {
    pub fn satisfied(detail: impl Into<String>) -> HealthCheck {
        HealthCheck {
            level: Level::Satisfied,
            detail: detail.into(),
            fix: String::new(),
            destructive: false,
        }
    }

    /// A fault, and what would clear it. Both, always — the second argument exists so that a fault
    /// with no way out cannot be written without noticing.
    pub fn unsatisfied(detail: impl Into<String>, fix: impl Into<String>) -> HealthCheck {
        HealthCheck {
            level: Level::Unsatisfied,
            detail: detail.into(),
            fix: fix.into(),
            destructive: false,
        }
    }

    /// Could not be answered — `detail` says why it could not, not what is wrong.
    pub fn unknown(detail: impl Into<String>) -> HealthCheck {
        HealthCheck {
            level: Level::Unknown,
            detail: detail.into(),
            fix: String::new(),
            destructive: false,
        }
    }

    /// Mark the fix as one that destroys something, so nothing drives it.
    pub fn destroys(mut self) -> HealthCheck {
        self.destructive = true;
        self
    }

    /// Is this a fault? `Unknown` is not one — see [`Level`].
    pub fn is_fault(&self) -> bool {
        self.level == Level::Unsatisfied
    }
}

impl HealthReport {
    /// Every check in the report, named. One list, so a check added to the struct and forgotten
    /// here shows up as a compile error rather than as a check nothing ever looks at.
    pub fn checks(&self) -> [(&'static str, &HealthCheck); 10] {
        let HealthReport {
            registry,
            sbx,
            git,
            gh,
            probes,
            mailbox,
            ai,
            memory,
            gitgate,
            cover,
            ..
        } = self;
        [
            ("registry", registry),
            ("sbx", sbx),
            ("git", git),
            ("gh", gh),
            ("probes", probes),
            ("mailbox", mailbox),
            ("ai", ai),
            ("memory", memory),
            ("gitgate", gitgate),
            // Named for what it is about rather than for the field: this key is what `/v2` puts
            // on the row, and "isolation" is a word somebody can act on where "cover" is jargon.
            ("isolation", cover),
        ]
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
    /// Whether a box's GitHub credential is actually scoped, and why not when it isn't.
    ///
    /// Never `ok: false` for being switched off — scoping is opt-in and "off" is a correct state.
    /// It reports `false` only when the fleet is *trying* to scope and cannot: an App ID that
    /// GitHub rejects, a key for a different App, an App installed on none of the repos in use.
    /// Those failures were previously invisible — `refresh_tokens` produced exact, useful errors
    /// and the server printed them to a detached process's stderr, so the first place anyone
    /// learned of one was a 403 inside a box some minutes later.
    pub gitgate: HealthCheck,
    /// Whether every running box is under the isolation this skein installs.
    ///
    /// The check that cannot be answered by looking at anything on the host: `install_launcher`
    /// refreshes `box-session.sh` at every start and every heal, so the copy on disk is always
    /// current and always says nothing about the boxes already running. The answer travels with
    /// each box instead, in its placement record.
    pub cover: HealthCheck,
    /// Which agent runtimes have a login every new box will inherit. Empty means `skein login` has
    /// not been run — the single most common way a first run goes quiet, since each box then comes
    /// up sitting at a sign-in prompt doing nothing.
    pub logins: Vec<String>,
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
    pub runtimes: Vec<RuntimeInfo>,
    /// How boxes get GitHub credentials, named — or empty when nobody has chosen.
    ///
    /// Empty is a real state now, not a theoretical one. All three paths are opt-in, so a fresh fleet
    /// has no way to push until someone picks one, and the first-run checklist asks on the strength of
    /// this field. It used to be unaskable: the account token was seeded by default, so the answer was
    /// always "the account token" and the question would have been noise.
    pub git_credential: String,
}

/// The `sbx` line, extracted so both deployments' answers can be read without building a
/// whole report. `git_scope_health` is here for the same reason.
///
/// Three answers, and this is the check that most needed them. `sbx` missing from PATH is a
/// fault with a fix. A listing that timed out is NOT a fault — it is skein unable to ask, and
/// reporting it as "sbx is broken" sent people to reinstall a working tool. The snapshot case is
/// the same shape one step further on: skein is answering from a picture it took a moment ago,
/// which is neither current nor wrong.
fn sbx_health(
    on_path: bool,
    fleet: &Option<Vec<crate::sbx::SbxBox>>,
    degraded: bool,
) -> HealthCheck {
    match (on_path, fleet, degraded) {
        // In-fleet its absence is correct, not a fault. `sbx` is host-only, and a check that turned
        // the banner red for it would be telling somebody to install a tool that cannot run where
        // they are — and hiding, behind a false alarm, the one thing they would want to know: that
        // this deployment reaches the fleet a different way.
        (false, _, _) if crate::deployment::in_fleet() => HealthCheck::satisfied(
            "not here, and not needed: skein is inside the fleet, so it enters a box by its \
             namespace rather than through sbx",
        ),
        (false, _, _) => HealthCheck::unsatisfied(
            "`sbx` is not on PATH, and it is how skein reaches the fleet — no box can be created, \
             started or entered without it",
            "install Docker Sandboxes, or start the server from a shell whose PATH has `sbx` on it",
        ),
        (true, Some(boxes), true) => HealthCheck::unknown(format!(
            "`sbx ls` did not answer just now; showing the last successful snapshot ({} boxes)",
            boxes.len()
        )),
        (true, Some(boxes), false) => {
            HealthCheck::satisfied(format!("available ({} boxes)", boxes.len()))
        }
        // The failure in its own words. "installed, but `sbx ls` failed or timed out" is what this
        // said, and it is four different faults wearing one coat — the reader's next move is
        // different for each. Unknown rather than a fault: sbx is installed and did not answer,
        // which is a question skein could not put, not an answer it got.
        (true, None, _) => HealthCheck::unknown(
            crate::sbx::fleet_failure()
                .unwrap_or_else(|| "no fleet listing, and no reason recorded".into()),
        ),
    }
}

/// The isolation line: whether every running box is under the cover this skein installs.
///
/// A fault rather than a note, and the argument had two sides. Against: the fix costs whatever the
/// agent in that box had half-finished, so somebody may reasonably put it off, and a red mark they
/// cannot clear without losing work is the shape of an alarm people learn to ignore. For, and it
/// wins: every other thing on this panel is skein failing at something, and this is the fleet being
/// less isolated than the person running it believes. That belief is exactly what a per-box cover
/// was built to make safe, and a quiet note is how the gap went unnoticed long enough to be found
/// by looking at a box rather than by reading the board.
///
/// The wording says what a restart BUYS. "Stale" describes a file and leaves the reader to work out
/// why they should care; the cover is the reason, so the cover is what the sentence names — and it
/// says what a restart costs too, because this is a decision about somebody's unfinished work
/// rather than an instruction.
pub fn cover_health(uncovered: &[String]) -> HealthCheck {
    match uncovered.is_empty() {
        true => HealthCheck::satisfied("every running box is under the current isolation"),
        false => HealthCheck::unsatisfied(
            format!(
                "started before the current isolation and still running under the old one: {}",
                uncovered.join(", ")
            ),
            format!(
                "`skein restart {}` — restarting it rebuilds the box's namespace with the covers \
                 this skein installs. Its checkout and its branch are untouched; whatever the \
                 agent was part-way through is not, so pick the moment",
                uncovered.first().map(String::as_str).unwrap_or("<box>")
            ),
        ),
    }
}

/// Running boxes with no memory ceiling on them, for the CLI, which prints its lines one at a time
/// rather than from a report.
pub fn uncapped_boxes() -> Vec<String> {
    crate::board::load_views()
        .unwrap_or_default()
        .into_iter()
        .filter(|view| !view.ceiling.is_empty() && !crate::fleet::is_capped(&view.ceiling))
        .map(|view| view.name)
        .collect()
}

/// The boxes that line is about: running, and placed by a launcher that is not the current one.
///
/// A separate entry point because `skein doctor` prints its lines one at a time rather than from a
/// report, and the CLI is where somebody looks when the cockpit is the thing that is not running.
pub fn uncovered_boxes() -> Vec<String> {
    crate::board::load_views()
        .unwrap_or_default()
        .into_iter()
        .filter(|view| view.cover == "older")
        .map(|view| view.name)
        .collect()
}

/// Render [`crate::gitgate::ScopeStatus`] as a health line.
///
/// Presentation only — the states themselves belong to `gitgate`, which is the module that knows
/// what they mean. This file used to derive them by reaching into five of its internals, which is
/// how two callers of the same question end up disagreeing.
fn git_scope_health() -> HealthCheck {
    use crate::gitgate::ScopeStatus::*;
    // What a box holds when scoping is *not* in force is no longer a fixed sentence. It used to be —
    // the account token was seeded by default, so "not scoped" always meant "every box holds your
    // whole account". Now that all three credential paths are chosen, an unscoped box may hold nothing
    // at all, and telling someone their boxes carry a credential they never picked sends them hunting
    // the wrong problem the first time a push fails.
    let unscoped_holds = match crate::gitgate::box_credential() {
        crate::gitgate::BoxCredential::None => {
            "boxes have no GitHub credential at all and cannot push".to_string()
        }
        other => format!("every box holds {}", other.label()),
    };
    match crate::gitgate::scope_status() {
        Off => HealthCheck::satisfied(format!(
            "off — {unscoped_holds}. Settings → scope each box's access to its own repo"
        )),
        NotConfigured => HealthCheck::satisfied(format!(
            "not set up — {unscoped_holds}. Settings → GitHub & keys → add a GitHub App or a \
             per-repo token to scope them"
        )),
        Unusable { why, refused } => HealthCheck::unsatisfied(
            format!(
                "ON but nothing is scoped, so {unscoped_holds}: {why}.{}",
                match refused.is_empty() {
                    true => String::new(),
                    false => format!(" Stored tokens refused — {}.", refused.join("; ")),
                }
            ),
            "Settings → GitHub & keys → add a GitHub App, or a per-repo token for each repo in use",
        ),
        Active { app, tokens } => HealthCheck::satisfied(format!(
            "on — boxes write only their own repo.{}{}",
            match app.is_empty() {
                true => String::new(),
                false => format!(" App {app}"),
            },
            match tokens {
                0 => String::new(),
                n => format!(" {n} stored repo token(s)"),
            }
        )),
    }
}

/// The git-scope check alone, so `skein doctor` can print the one line without building the whole
/// report — which probes the sandbox and takes seconds.
pub fn health_report_gitgate() -> HealthCheck {
    git_scope_health()
}

/// Read-only environment diagnosis for detached server deployments. Unlike startup `eprintln!`,
/// this remains inspectable from the cockpit and makes a missing box-side jq dependency explicit.
pub fn health_report() -> HealthReport {
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
                Some(p) if p.killed > 0 => HealthCheck::unsatisfied(
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
                Some(p) if p.rated && p.throttled_per_min > THROTTLE_NOTICEABLE => {
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
            "skein resize 26g   (or Settings → Fleet → memory; it rebuilds the sandbox and carries \
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
    let sbx = sbx_health(program_on_path("sbx"), &fleet, fleet_degraded);
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
    let gh = match crate::github::have_curl() {
        true => HealthCheck::satisfied("available"),
        false => HealthCheck::unsatisfied(
            "curl is not installed, and skein reads GitHub with it — pull requests, diffs, merges, \
             and minting App tokens",
            "install curl",
        ),
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
                "`skein restart {}` — it started before this fleet had a memory plan, and a restart                  puts it under the current one",
                uncapped_boxes.first().map(String::as_str).unwrap_or("<box>")
            ),
            false => "this sandbox does not delegate cgroups, so skein cannot bound a box in it —                       the ceilings on the fleet as a whole still hold, but one box's build can                       reach all of them"
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
    // Deliberately NOT reported here: a box on hook-only turn state (see `screen_health`) is not
    // unhealthy — it degrades to exactly its pre-observer behaviour. Nagging in the environment
    // banner would be crying wolf; the caveat belongs on the row and tab it applies to.
    let cover = cover_health(&uncovered_boxes);
    let gitgate = git_scope_health();
    // A fault, and only a fault. An `Unknown` check must not turn the banner red: telling somebody
    // their fleet is broken because skein could not reach it for two seconds is the false alarm the
    // third state exists to stop. The cockpit reports the unknowns beside the faults, in the mark
    // it already has for "look at this but nothing is wrong".
    let ok = ![&registry, &sbx, &git, &probes, &mailbox, &gitgate, &cover]
        .iter()
        .any(|check| check.is_fault())
        && stale_boxes.is_empty();

    HealthReport {
        ok,
        registry,
        sbx,
        git,
        gh,
        probes,
        mailbox,
        ai,
        memory,
        gitgate,
        cover,
        logins: crate::fleet::signed_in_runtimes(),
        dark_boxes,
        stale_boxes,
        uncovered_boxes,
        uncapped_boxes,
        runtimes: supported_runtimes(),
        git_credential: crate::gitgate::box_credential().label(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three states, and what each one is allowed to cause.
    ///
    /// `Unknown` is the whole point of the type: a check that could not be answered is not a pass
    /// and not a fault, and treating it as either is a bug with a name. As a fault it cries wolf —
    /// telling somebody `sbx` is broken because a listing timed out once sends them to reinstall a
    /// working tool. As a pass it is worse: whatever would have acted on `Unsatisfied` does nothing,
    /// silently, and the thing that was actually wrong is never reported.
    #[test]
    fn only_a_fault_is_a_fault() {
        assert!(HealthCheck::unsatisfied("x", "do y").is_fault());
        assert!(!HealthCheck::satisfied("x").is_fault());
        assert!(
            !HealthCheck::unknown("x").is_fault(),
            "a question skein could not put is not an answer it got"
        );
        // Only a fault carries a way out. A satisfied check has nothing to fix, and an unknown one
        // has nothing KNOWN to fix — offering a remedy for a question skein could not put is how a
        // diagnostic sends somebody to change a working setting.
        assert!(HealthCheck::satisfied("x").fix.is_empty());
        assert!(HealthCheck::unknown("x").fix.is_empty());
        assert_eq!(HealthCheck::unsatisfied("x", "do y").fix, "do y");
        // Destructive is off unless said, and saying it does not change the level: a destructive
        // fix is still the fix, it just may not be driven.
        let destructive = HealthCheck::unsatisfied("x", "do y").destroys();
        assert!(destructive.destructive && destructive.is_fault());
        assert!(!HealthCheck::unsatisfied("x", "do y").destructive);
    }

    /// One cause, one line — measured, because the alternative is nine.
    ///
    /// A repo whose store does not exist produced eight "missing" complaints from the probe check
    /// and one from the mailbox check: nine symptoms of a repo that never finished being added, and
    /// no way for a reader to see that they were one thing. Every one of them clears when the store
    /// is made, and none of them is separately actionable.
    #[test]
    fn a_repo_with_no_store_is_one_fault_and_not_nine() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");
        // A repo registered against a store nobody made — `skein add` interrupted, or a volume
        // mounted somewhere else since.
        crate::repos::save_repos(&[crate::repos::Repo {
            id: "orphan".into(),
            source: "https://github.com/a/b".into(),
            source_tree: String::new(),
            store: home.join("gone/.claude").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        }])
        .unwrap();

        let report = health_report();
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
        std::env::remove_var("SKEIN_NO_GH_SECRET");
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
    /// It reads the machine's own PATH rather than blanking it, and that is not laziness. `PATH` is
    /// process-global and the suite runs in parallel: an earlier version set it to a directory that
    /// does not exist, and a sibling test that shells out failed while it held it. A test that makes
    /// other tests fail is worse than one that is only sharp on some machines — and it is sharp
    /// wherever a tool is genuinely absent, which is every machine without `sbx`.
    #[test]
    fn a_missing_tool_is_one_fault_and_not_five() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let report = health_report();
        std::env::remove_var("SKEIN_HOME");

        let faults: Vec<&str> = report
            .checks()
            .into_iter()
            .filter(|(_, check)| check.is_fault())
            .map(|(name, _)| name)
            .collect();
        assert!(
            faults
                .iter()
                .all(|name| ["sbx", "git", "gh"].contains(name)),
            "something that is not a tool is reported broken, which on a machine with no fleet \
             means a check invented a fault out of a question it could not put: {faults:?}"
        );
        // And where a tool IS missing it is named, so this is not passing by finding nothing.
        for tool in ["sbx", "git"] {
            if !program_on_path(tool) {
                assert!(
                    faults.contains(&tool),
                    "{tool} is not on this PATH and the report does not say so: {faults:?}"
                );
            }
        }
    }

    /// **No fault without a way out.** The parent property, in the only form that can be enforced.
    ///
    /// A recipe written by hand per check is right where somebody thought of it, and absent where
    /// they did not — and the check that nobody thought about is the one somebody is staring at.
    /// This walks the real report on this machine, so a check added later with no fix fails here
    /// rather than in front of a person who is stuck.
    #[test]
    fn every_fault_says_what_would_fix_it() {
        let report = health_report();
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
    }

    /// The three states reach the cockpit under the names it renders.
    ///
    /// The page switches on this string. A rename here that the page does not follow shows every
    /// check as unknown, which is the one failure mode that looks like a working screen.
    #[test]
    fn the_wire_names_are_the_names_the_page_switches_on() {
        let page = include_str!("web/index.html");
        for (level, name) in [
            (Level::Satisfied, "satisfied"),
            (Level::Unsatisfied, "unsatisfied"),
            (Level::Unknown, "unknown"),
        ] {
            let json = serde_json::to_string(&HealthCheck {
                level,
                detail: String::new(),
                fix: String::new(),
                destructive: false,
            })
            .unwrap();
            assert!(
                json.contains(&format!("\"level\":\"{name}\"")),
                "{level:?} does not serialise as {name}: {json}"
            );
            assert!(
                page.contains(&format!("{name}:")) || page.contains(&format!("\"{name}\"")),
                "the cockpit does not mention the `{name}` level at all"
            );
        }
    }

    /// A missing `sbx` is a fault on a host and correct in the fleet.
    ///
    /// Reporting it red in-fleet would hand somebody a fault they cannot clear — `sbx` is host-only
    /// and cannot be installed into the sandbox — and, worse, would hide behind a false alarm the
    /// one thing they wanted to know: that this deployment reaches boxes another way. A banner that
    /// is red for a correct state is how the next real fault gets read as noise too.
    #[test]
    fn a_missing_sbx_is_a_fault_on_a_host_and_the_normal_state_in_the_fleet() {
        let _g = crate::testutil::env_lock();

        std::env::remove_var(crate::deployment::IN_FLEET);
        let on_host = sbx_health(false, &None, false);
        assert!(
            on_host.is_fault(),
            "a host with no sbx cannot create, start or enter a box, and that is a fault"
        );
        assert!(on_host.fix.contains("PATH"), "{}", on_host.fix);

        std::env::set_var(crate::deployment::IN_FLEET, "1");
        let in_fleet = sbx_health(false, &None, false);
        assert!(
            !in_fleet.is_fault(),
            "the fleet was told to install a host-only tool it cannot run: {}",
            in_fleet.detail
        );
        assert!(
            in_fleet.detail.contains("namespace"),
            "it says sbx is missing without saying how boxes are reached instead: {}",
            in_fleet.detail
        );

        // And the deployment does not touch the other three arms: a present `sbx` that will not
        // answer is the same unknown either way, because that is a question skein could not put
        // rather than an answer about where it is standing.
        let silent = sbx_health(true, &None, false);
        std::env::remove_var(crate::deployment::IN_FLEET);
        assert_eq!(silent.level, sbx_health(true, &None, false).level);
        assert!(!silent.is_fault());
    }
}
