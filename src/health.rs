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
/// The revision this binary was built from: `git describe --always --dirty`, stamped by build.rs.
///
/// This is the answer to "which build is serving?", and it exists because the question was
/// unanswerable twice at real cost: a restart mis-diagnosed as a stale fleet agent because nothing
/// could name the binary, and "is the fix deployed" settled only by grepping served HTML for marker
/// strings. `--dirty` is load-bearing — a binary from an edited tree is the other thing that looks
/// like a clean deploy and is not. "unknown" when git was absent at build time; never the package
/// version, which is 0.1.0 forever and answers a different question.
pub const BUILD_REVISION: &str = env!("SKEIN_BUILD_REVISION");

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

/// Past this share of a filesystem, skein says so. Below it, nothing is said.
///
/// 85 rather than 95 because the fix takes minutes and the failure takes an afternoon: a build that
/// runs out of space fails somewhere in the middle, and the box it failed in is usually not the box
/// that took the space. The per-box chip marks at 80% of a box's own allowance, which is a
/// different question — that one is "who is taking it", this one is "is there any left".
const DISK_FULL_PCT: u64 = 85;

/// Is there room left on the fleet's filesystems — the one the boxes are on, and the one Docker
/// keeps its images on?
///
/// Disk is the resource this fleet actually runs out of (`box-session.sh` and `BoxLoad::disk_mb`
/// both say so, and the sandbox has hit 100% mid-build), and until SKEIN-133 nothing said a word
/// about it unprompted: the figures existed only inside the resources overlay, which you have to
/// already suspect something to open.
///
/// **Two filesystems, told apart.** sbx gives a sandbox a root sized by
/// `DOCKER_SANDBOXES_ROOT_SIZE` and an image store sized by `DOCKER_SANDBOXES_DOCKER_SIZE`, so one
/// being full says nothing about the other — and they are cleared by different actions, which is
/// the whole reason for naming them separately rather than summing them. The boxes' disk is
/// cleared by stopping or clearing a box; the image store by pruning what Docker is keeping.
pub fn disk_health() -> HealthCheck {
    let Some(r) = crate::fleet::fleet_resources() else {
        return HealthCheck::unknown(
            "no fleet sandbox is configured, so there is no filesystem to measure",
        );
    };
    // The per-box figures are only READ when something is actually full: they come from their own
    // gate and a tree walk behind it, and a satisfied check has nothing to name them for.
    disk_verdict(
        &r,
        &crate::place::fleet_sandbox(),
        |_: ()| biggest_first(),
        crate::fleet::substrate_strays,
    )
}

/// Which box is holding the most of the fleet's disk, largest first.
///
/// Its own function rather than the closure it used to be, because [`crate::announce`] asks the
/// same question of the same map and a second ordering is a second answer: the fix line would name
/// one box while the agent that got interrupted was another, with nothing to say which was right.
/// Ties are broken by name so the order is total and the two cannot disagree on equal figures
/// either.
pub(crate) fn biggest_first() -> Vec<(String, u64)> {
    let mut all: Vec<(String, u64)> = crate::fleet::fleet_disk_usage().into_iter().collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    all
}

/// The verdict itself, over figures already in hand — so the thresholds can be driven in a test on
/// a machine with no fleet, which is every machine this suite runs on.
///
/// `strays` is the second reading, and it is an argument for the same reason `biggest` is: it walks
/// the real `<fleet root>/.skein`, so a test that could not choose it would be reading the owner's
/// live fleet. Both are `FnOnce`, so neither walk happens on a fleet with room left.
///
/// **The two are one answer.** `biggest` names boxes and only boxes — `local_disk_usage` stopped
/// keeping `.skein` when this landed (SKEIN-735), because offering `skein stop .skein` for 19.2 GB
/// is a command that cannot work on a thing that is not a box. Removing it from that list alone
/// would have made skein quieter about a real consumer, so `strays` says the true thing about the
/// same bytes: what is in there that is neither skein's own install nor any live box's, and the
/// `rm -rf` for it.
///
/// **An `Err` from `strays` is silence, not a sentence.** [`crate::fleet::substrate_strays`]
/// deliberately errs whenever one of its derivations came up empty rather than reporting "nothing
/// is stranded" it could not stand behind; passing that reasoning on to somebody looking at a full
/// disk would be a paragraph about skein in the middle of the advice they came for, and it is
/// already in `skein doctor`'s reach by other means.
fn disk_verdict(
    r: &crate::fleet::FleetResources,
    sandbox: &str,
    biggest: impl FnOnce(()) -> Vec<(String, u64)>,
    strays: impl FnOnce() -> Result<Vec<crate::fleet::Stray>, String>,
) -> HealthCheck {
    if r.disk_total == 0 {
        return HealthCheck::unknown(match r.stale {
            true => {
                "the sandbox is not answering, so its disk figures are the last ones that \
                     arrived — and they carry no total"
            }
            false => "the sandbox answered without disk figures, so how full it is cannot be said",
        });
    }
    let gib = |mib: u64| format!("{:.1}G", mib as f64 / 1024.0);
    let pct = |used: u64, total: u64| match total {
        0 => 0,
        _ => used * 100 / total,
    };
    let boxes_pct = pct(r.disk_used, r.disk_total);
    // Zero total means Docker shares the boxes' filesystem — the same bytes, already counted.
    let images_pct = pct(r.images_used, r.images_total);
    let boxes_line = format!(
        "the boxes' disk is {}% full ({} of {})",
        boxes_pct,
        gib(r.disk_used),
        gib(r.disk_total)
    );
    let images_line = match r.images_total {
        0 => "Docker shares that filesystem, so there is no separate image store".to_string(),
        _ => format!(
            "the image store is {}% full ({} of {})",
            images_pct,
            gib(r.images_used),
            gib(r.images_total)
        ),
    };
    let detail = format!("{boxes_line}; {images_line}");
    if boxes_pct < DISK_FULL_PCT && images_pct < DISK_FULL_PCT {
        return HealthCheck::satisfied(detail);
    }
    // What to clear, named per filesystem, because the two are cleared by different actions and a
    // combined sentence leaves the reader to work out which half applies to them.
    let mut fixes: Vec<String> = Vec::new();
    // Held back until every other fix is in, because it ends in a `rm -rf` spanning a line of its
    // own and anything joined after that reads as part of the command.
    let mut substrate: Option<String> = None;
    if boxes_pct >= DISK_FULL_PCT {
        substrate = strays()
            .ok()
            .as_deref()
            .and_then(crate::fleet::stray_advice);
        let named = biggest(())
            .iter()
            .take(3)
            .map(|(name, mb)| format!("{name} ({})", gib(*mb)))
            .collect::<Vec<_>>()
            .join(", ");
        fixes.push(match named.is_empty() {
            true => "stop a box you are not using (`skein ls` shows what is running) or clear \
                     its build output — one filesystem serves every box"
                .to_string(),
            false => format!(
                "the largest boxes are {named} — `skein stop <box>` keeps its checkout, branch \
                 and conversation, or clear its build output in place"
            ),
        });
    }
    if images_pct >= DISK_FULL_PCT {
        fixes.push(format!(
            "the image store is Docker's: `sbx exec {sandbox} docker system prune -af` frees it"
        ));
    }
    fixes.extend(substrate);
    let check = HealthCheck::unsatisfied(detail, fixes.join("; "));
    // The prune deletes images and build cache that nothing is using *now* — recoverable, but it
    // is a delete, and §2.4 says a recipe that destroys is printed rather than driven.
    match images_pct >= DISK_FULL_PCT {
        true => check.destroys(),
        false => check,
    }
}

/// What the shared `/tmp` is asked, where the fleet's `claude` actually runs.
///
/// Derives the path rather than assuming it: `${TMPDIR:-/tmp}/claude-$(id -u)` is the rule Claude
/// Code applies, and both halves are answered by the machine being asked — a fleet's uid is not the
/// host's, and `TMPDIR` is set on macOS and unset in the sandbox. `stat -c` is GNU and `stat -f` is
/// BSD, so both are tried and whichever exists answers.
///
/// Prints one line: `clear <path>` when nothing is there, or `<owner-uid> <our-uid> <path>` when
/// something is. Never deletes, never creates — see [`scratch_verdict`] for why that is a rule and
/// not an omission.
const SCRATCH_PROBE: &str = "d=\"${TMPDIR:-/tmp}/claude-$(id -u)\"\n\
     if [ ! -e \"$d\" ]; then printf 'clear %s\\n' \"$d\"; exit 0; fi\n\
     owner=\"$(stat -c %u \"$d\" 2>/dev/null || stat -f %u \"$d\" 2>/dev/null)\"\n\
     printf '%s %s %s\\n' \"${owner:-unreadable}\" \"$(id -u)\" \"$d\"\n";

/// Has somebody else's directory taken the temp path the model CLI derives for itself?
///
/// **Why this is skein's business at all.** Claude Code puts its temp directory at
/// `${os.tmpdir()}/claude-<uid>` and refuses to start when that path exists and belongs to another
/// uid — a deliberate guard against a directory somebody planted. In a fleet that path is the
/// sandbox's SHARED `/tmp`, and on the owner's fleet something running as root got there first.
/// Every model call skein makes, every box session and the login terminal now carry
/// `CLAUDE_CODE_TMPDIR` past it ([`crate::fleet::MODEL_SCRATCH`]), so this is no longer how skein
/// fails — but the directory outlives every call, and a `claude` anybody starts by hand still meets
/// it. The CLI's own message is a good one; the trouble was that it landed on whoever happened to be
/// typing rather than in the one place that reports the fleet's health.
///
/// **A `doctor` line and not part of [`health_report`]**, for the same reason `skein doctor`'s model
/// line is not: this spawns a process in the sandbox, and the health endpoint is polled every
/// fifteen seconds by every open board.
pub fn model_scratch_health() -> HealthCheck {
    // The question is about the /tmp the fleet's `claude` runs in, and **this process is standing
    // in it** — one arm now (SKEIN-576). The other asked the sandbox through `sbx exec`, a hop that
    // no longer exists in either direction; `crate::fleet::model_call_in_box` is the only crossing
    // a model call still makes, and a box has a /tmp of its own that this check is not about.
    let (reported, whose) = (run_here(SCRATCH_PROBE), "this fleet's shared /tmp");
    scratch_verdict(reported, whose)
}

/// The probe, run on this machine.
fn run_here(script: &str) -> Result<String, String> {
    let out = std::process::Command::new("bash")
        .arg("-lc")
        .arg(script)
        .output()
        .map_err(|e| format!("bash could not be run here ({e})"))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// The verdict over [`SCRATCH_PROBE`]'s answer — split from the call so every arm is testable on a
/// machine with no fleet, which is every machine this suite runs on.
///
/// **The fix is words, and it is marked destructive so nothing drives it.** Deleting a directory in
/// a shared `/tmp` that skein does not own is precisely the attack the CLI's guard exists to stop,
/// and skein would be doing it with more privilege than whoever planted it. So the recipe names the
/// path and the uid and stops there: a person decides, on a machine where they can see what it is.
pub fn scratch_verdict(reported: Result<String, String>, whose: &str) -> HealthCheck {
    let line = match reported {
        Ok(out) => out.lines().last().unwrap_or_default().trim().to_string(),
        Err(why) => {
            return HealthCheck::unknown(format!(
                "{whose} could not be asked whether anything has taken the model's temp directory \
                 ({why})"
            ))
        }
    };
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["clear", path] => HealthCheck::satisfied(format!(
            "nothing has taken {path} in {whose}, and skein's own calls carry their own scratch \
             directory either way"
        )),
        [owner, mine, path] if owner == mine => HealthCheck::satisfied(format!(
            "{path} in {whose} is this fleet's own (uid {mine})"
        )),
        [owner, mine, path] => HealthCheck::unsatisfied(
            format!(
                "{path} in {whose} belongs to uid {owner}, and the fleet runs as uid {mine} — a \
                 `claude` that derives its own temp directory refuses to start there, whatever the \
                 login says"
            ),
            format!(
                "skein's model calls, every box session and the login terminal carry \
                 CLAUDE_CODE_TMPDIR past it, so this reaches only a `claude` somebody starts by \
                 hand. Clearing it takes uid {owner} — `rm -rf {path}` on that machine, by \
                 somebody who can see what is in it. skein will not: deleting a directory in a \
                 shared /tmp it does not own is the thing the CLI's guard exists to stop"
            ),
        )
        .destroys(),
        _ => HealthCheck::unknown(format!(
            "{whose} answered something this cannot read ({line:?}), so whether anything has taken \
             the model's temp directory is unknown"
        )),
    }
}

impl HealthReport {
    /// Every check in the report, named. One list, so a check added to the struct and forgotten
    /// here shows up as a compile error rather than as a check nothing ever looks at.
    pub fn checks(&self) -> [(&'static str, &HealthCheck); 12] {
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
            warden,
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
            ("disk", disk),
            ("gitgate", gitgate),
            ("warden", warden),
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
    pub runtimes: Vec<RuntimeInfo>,
    /// How boxes get GitHub credentials, named — or empty when nobody has chosen.
    ///
    /// Empty is a real state now, not a theoretical one. All three paths are opt-in, so a fresh fleet
    /// has no way to push until someone picks one, and the first-run checklist asks on the strength of
    /// this field. It used to be unaskable: the account token was seeded by default, so the answer was
    /// always "the account token" and the question would have been noise.
    pub git_credential: String,
}

/// The `sbx` line, extracted so it can be read without building a whole report — which is why
/// `git_scope_health` sits beside it.
///
/// **Whether `sbx` is on `$PATH` decides nothing, and that is the change** (SKEIN-576). It used to
/// be the first question: a missing `sbx` was a fault with a fix, because `sbx` was how skein
/// reached every box. Skein runs inside the fleet sandbox now — it enters a box by its namespace,
/// and `sbx ls` is a question about the HOST's machine, which this process is not standing on. So
/// the binary's presence became a fact about nothing, while still flipping this row from
/// *satisfied* to *unknown* when it happened to be installed. That is a row that changes for a
/// reason the reader cannot act on, which is worse than one that says the same thing every time.
///
/// A listing still ANSWERS when something can answer it — `$SKEIN_LS_CMD`, a test or a proxy — and
/// those two arms are kept for that: skein reporting from a picture it took a moment ago is neither
/// current nor wrong, which is what `unknown` is for. What is gone is the fault.
///
/// `docs/parity.md` §7 records what a person stops being told.
fn sbx_health(fleet: &Option<Vec<crate::sbx::SbxBox>>, degraded: bool) -> HealthCheck {
    match (fleet, degraded) {
        (Some(boxes), true) => HealthCheck::unknown(format!(
            "`sbx ls` did not answer just now; showing the last successful snapshot ({} boxes)",
            boxes.len()
        )),
        (Some(boxes), false) => {
            HealthCheck::satisfied(format!("available ({} boxes)", boxes.len()))
        }
        // Nothing asked, which is the ordinary state rather than a failure to get an answer.
        // `unknown` here would report a question skein deliberately does not put, and on the
        // first-run checklist that reads as a step somebody has to go and fix — the one step a new
        // person cannot fix from inside the cockpit.
        (None, _) => HealthCheck::satisfied(
            "not asked, and not needed: skein is inside the fleet sandbox, so it enters a box by \
             its namespace rather than through sbx, and which boxes exist is read from their \
             placement records",
        ),
    }
}

/// Whether the host warden is answering — because fleet create and destroy go only through it.
///
/// **The whole point is that this is said BEFORE something needs it.** `create_through_warden`
/// refuses rather than falling back, deliberately: a fallback that ran `sbx` here would be taken on
/// exactly the day something was wrong. But until this line existed, that refusal was the first
/// anybody heard of it, and the sequence was: build, start the server, watch every check go green,
/// press Launch, get a 500. Worse on an *upgrade* than on a fresh install — an existing fleet keeps
/// running, so the failure surfaces weeks later on the first resize, by which time nobody connects
/// it to having upgraded skein.
///
/// **A fault, not a note.** Two of the fleet's five lifecycle operations are unavailable without it,
/// there is one command that fixes it, and this is skein unable to do something it offers — which is
/// what every other fault on this panel is. The argument against, and it is real: somebody who never
/// resizes would carry a red mark for a capability they do not use, and a banner that is red for a
/// state you have chosen is how the next real fault gets read as noise. It loses to the sentence
/// above — the cost of finding out late is a fleet you cannot resize at the moment you need to.
///
/// **What it does not do is trust the answer.** `capabilities` is what the far end SAYS it can do,
/// and §8.3 is blunt that this is never evidence — a malicious endpoint advertises whatever makes
/// skein show a button. So it is reported, in the warden's own words, and nothing here decides
/// anything from it.
fn warden_health(seen: Option<crate::warden_client::Sighting>) -> HealthCheck {
    // Said whichever way the check goes, because setting the warden's own variable on a client is a
    // mistake even when something happens to answer: it means this process is not asking where the
    // person thinks it is. It rides on the check rather than being a line of its own — a reader
    // looking at the warden is exactly the reader who needs it.
    let misdirected = crate::warden_client::misdirected();
    let note = |text: String| match &misdirected {
        Some(said) => format!("{text}\n{said}"),
        None => text,
    };
    match seen {
        Some(sighting) => {
            let doers = match sighting.capabilities.is_empty() {
                true => "it advertises no doers, so it can report but not create or destroy".into(),
                false => format!("it says it can {}", sighting.capabilities.join(" and ")),
            };
            HealthCheck::satisfied(note(format!(
                "answering on {}, and {doers} ({} sandbox(es) in view)",
                crate::warden_client::where_it_asks(),
                sighting.sandboxes.len()
            )))
        }
        // **The advice depends on which failure it was**, and getting that wrong is worse than
        // saying nothing. Every unsatisfied arm printed the build command, so this told somebody to
        // build a warden they were plainly running. Something answering and refusing is not
        // something missing, and the two send a reader to opposite places.
        //
        // The DETAIL stays the client's own words either way — not running, refusing the secret,
        // unreadable — because "the warden is not available" sends nobody anywhere.
        //
        // **This was an `unknown` in-fleet and is a fault again**, because the reason for the
        // exemption is gone. It read: the warden binds loopback, a loopback listener answers
        // nothing inside the sandbox, so no warden a person starts would help — and a banner
        // nobody can clear is how the next real fault gets read as noise. The premise was
        // measured and is false on Docker Desktop, which proxies the gateway address from the
        // host side; the bind has since widened for the hosts where it was true (SKEIN-475).
        // In-fleet skein reaches the host warden, so an unreachable one is again what the
        // unsatisfied arm has always been for: something a person can start.
        None => HealthCheck::unsatisfied(
            note(crate::warden_client::sighting_failure().unwrap_or_else(|| {
                "the host warden did not answer, and no reason was recorded".into()
            })),
            match crate::warden_client::sighting_trouble() {
                // It answered. Do not send anybody to a compiler.
                Some(crate::warden_client::Unseen::Answered) => format!(
                    "something is answering on {} and it is not a warden this skein can use. Check \
                     what is on that port, and that both ends agree about which one it is: \
                     `$SKEIN_WARDEN` moves the client, `$SKEIN_WARDEN_PORT` moves the warden, and \
                     setting only one of them aims skein at whatever else happens to be listening.",
                    crate::warden_client::where_it_asks()
                ),
                // Nothing there — and in-fleet the crossing is part of the answer, so the advice
                // names it. The address is now the host's rather than the sandbox's, which leaves
                // two candidates rather than the old four, and they are checked in different
                // places: a warden that is not running on the host, or a host whose warden cannot
                // be reached at the address this asked.
                // **One arm** (SKEIN-576): skein is in the fleet and the warden is on the host,
                // always. The other arm was the host-driven one, and what it knew that this did
                // not is folded in rather than deleted with it — "check a warden is running there"
                // is unhelpful to somebody who never had the binary, and a plain `cargo build`
                // does not make it.
                _ => format!(
                    "the warden runs on the host, and this asked it at {} \u{2014} the alias every \
                     sandbox has for its host. Check a `skein-warden` is running there \u{2014} and \
                     that there is one to run: `cargo build --release --workspace` makes it, while \
                     a plain `cargo build` makes `skein` and `skein-server` only. Run it where you \
                     will see it: it puts each create and destroy to a person, and nothing happens \
                     until somebody answers. If that host is Linux, it also has to be a build that \
                     binds the Docker bridge (architecture \u{a7}9.5): an older one binds loopback, \
                     which answers host processes and nothing in here. `$SKEIN_WARDEN` moves this \
                     end.",
                    crate::warden_client::where_it_asks()
                ),
            },
        ),
    }
}

/// The warden line on its own, for `skein doctor`.
///
/// Public for the same reason [`health_report_gitgate`] is: the CLI builds its own list rather than
/// rendering the whole report, so a check that is only reachable through `health_report` is one the
/// terminal never shows.
pub fn warden_report() -> HealthCheck {
    warden_health(crate::warden_client::sighting())
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
    //
    // Every line here says what a box HOLDS, and none of them says what a box can REACH. They used
    // to — "cannot push", "boxes write only their own repo" without the token named — and that was
    // false: the sandbox proxy answers a request carrying no credential as the account
    // (SKEIN-548, open; `gitgate`'s module note has the measurement).
    let unscoped_holds = match crate::gitgate::box_credential() {
        crate::gitgate::BoxCredential::None => {
            "boxes hold no GitHub credential of their own".to_string()
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
            "on — a box's own token writes only its own repo.{}{}",
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
    // A fault, and only a fault. An `Unknown` check must not turn the banner red: telling somebody
    // their fleet is broken because skein could not reach it for two seconds is the false alarm the
    // third state exists to stop. The cockpit reports the unknowns beside the faults, in the mark
    // it already has for "look at this but nothing is wrong".
    // `disk` is in this list and `memory` is not, deliberately. The memory check reports a plan
    // and its pressure — being at the ceiling is the fleet working as configured. A filesystem past
    // 85% is not a ceiling being used, it is a wall being approached, and the only warning anyone
    // gets before a build dies somewhere in the middle. It can only be a fault past the threshold:
    // an unknown disk (no sandbox, no answer) is never one.
    let ok = ![
        &registry, &sbx, &git, &probes, &mailbox, &gitgate, &warden, &cover, &disk,
    ]
    .iter()
    .any(|check| check.is_fault())
        && stale_boxes.is_empty();

    HealthReport {
        ok,
        build: BUILD_REVISION,
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
        warden,
        cover,
        logins: crate::fleet::signed_in_runtimes(),
        expired_logins: crate::fleet::expired_logins(),
        runtime_updates: crate::fleet::runtime_updates(),
        models: crate::ai::model_choices(),
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

    /// A running skein must say which build it is — with a revision, not a version number.
    ///
    /// The package version is 0.1.0 forever, so a `--version` or health field carrying it answers
    /// nothing; and "unknown" is the honest fallback for a build outside git, which this repo is
    /// not. Both mis-answers cost real time: a restart mis-diagnosed as a stale fleet agent, and
    /// "is the fix deployed" settled by grepping served HTML for marker strings. This test runs in
    /// a git checkout by construction, so a placeholder here means the stamp in build.rs broke.
    #[test]
    fn the_build_names_a_real_revision() {
        assert!(
            !BUILD_REVISION.trim().is_empty(),
            "the build stamp is empty — nothing skein serves can say which build it is"
        );
        assert_ne!(
            BUILD_REVISION, "unknown",
            "built inside a git checkout, yet the stamp is the no-git fallback"
        );
        assert_ne!(
            BUILD_REVISION,
            env!("CARGO_PKG_VERSION"),
            "the package version masquerading as a revision — it is 0.1.0 forever and identifies \
             nothing"
        );
    }

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
        // Pointed at a path with nothing at it, `df` prints no row, the sandbox answers without
        // disk figures, and the check is `Unknown` — which is the state this test's own message
        // describes as "a machine with no fleet", and the state the tri-state exists to have. It is
        // asserted below rather than left as a happy accident.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("no-fleet-here"));
        let report = health_report();
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
                .all(|name| ["git", "gh", "warden"].contains(name)),
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

    /// A filesystem past the threshold is a fault that names what to clear — and the two
    /// filesystems are cleared by different actions, so they are named apart (SKEIN-133).
    #[test]
    fn a_full_fleet_disk_says_so_and_says_what_to_clear() {
        let full = |disk_used, images_used| crate::fleet::FleetResources {
            disk_total: 60_000,
            disk_used,
            images_total: 50_000,
            images_used,
            ..Default::default()
        };
        let boxes = |_: ()| {
            vec![
                ("proj-s6".to_string(), 14_336_u64),
                ("example-work".to_string(), 10_650),
                ("web-main".to_string(), 512),
            ]
        };
        // This test is about the thresholds and the two filesystems. The substrate sweep has its
        // own, below, and here it finds nothing so it says nothing.
        let nothing_stranded = || -> Result<Vec<crate::fleet::Stray>, String> { Ok(Vec::new()) };

        // Room left: said, and nothing to do about it.
        let easy = disk_verdict(&full(20_000, 20_000), "fleet", boxes, nothing_stranded);
        assert_eq!(easy.level, Level::Satisfied, "{}", easy.detail);
        assert!(
            easy.detail.contains("33%") && easy.detail.contains("40%"),
            "{}",
            easy.detail
        );

        // The boxes' disk is full: the fix names the biggest, largest first, with figures — "3
        // boxes" is not something anybody can act on at the moment they read it.
        let tight = disk_verdict(&full(54_140, 20_000), "fleet", boxes, nothing_stranded);
        assert_eq!(tight.level, Level::Unsatisfied, "{}", tight.detail);
        assert!(
            tight.detail.contains("90%"),
            "the share is not stated: {}",
            tight.detail
        );
        assert!(
            tight.fix.contains("proj-s6 (14.0G)") && tight.fix.contains("example-work (10.4G)"),
            "the fix does not name what is taking the space: {}",
            tight.fix
        );
        assert!(
            !tight.fix.contains("prune"),
            "the image store is not full and the fix offers to prune it anyway: {}",
            tight.fix
        );
        assert!(!tight.destructive, "stopping a box destroys nothing");

        // Docker's store is the other filesystem and the other action — and it deletes, so the
        // recipe is printed rather than driven.
        let images = disk_verdict(&full(20_000, 45_000), "fleet", boxes, nothing_stranded);
        assert_eq!(images.level, Level::Unsatisfied, "{}", images.detail);
        assert!(
            images
                .fix
                .contains("sbx exec fleet docker system prune -af"),
            "the image store's fix is not the one that clears it: {}",
            images.fix
        );
        assert!(
            !images.fix.contains("proj-s6"),
            "the boxes' disk has room and the fix asks somebody to stop a box: {}",
            images.fix
        );
        assert!(
            images.destructive,
            "a prune deletes; §2.4 says such a recipe is never driven"
        );

        // Both, and both sentences.
        let both = disk_verdict(&full(54_140, 45_000), "fleet", boxes, nothing_stranded);
        assert!(
            both.fix.contains("proj-s6") && both.fix.contains("prune"),
            "{}",
            both.fix
        );

        // Docker sharing the boxes' filesystem: the same bytes are never counted twice, and there
        // is no second thing to clear.
        let shared = crate::fleet::FleetResources {
            disk_total: 60_000,
            disk_used: 54_140,
            images_total: 0,
            images_used: 0,
            ..Default::default()
        };
        let one = disk_verdict(&shared, "fleet", boxes, nothing_stranded);
        assert!(
            one.detail.contains("no separate image store"),
            "{}",
            one.detail
        );
        assert!(!one.fix.contains("prune"), "{}", one.fix);

        // Asked and not answered is not a fault — the third state exists for exactly this.
        let blind = disk_verdict(
            &crate::fleet::FleetResources::default(),
            "fleet",
            boxes,
            nothing_stranded,
        );
        assert_eq!(blind.level, Level::Unknown);
        assert!(
            blind.fix.is_empty(),
            "an unknown offers no fix: {}",
            blind.fix
        );
    }

    /// A full fleet is told about the substrate as the substrate — not as a box called `.skein`
    /// that it is invited to stop (SKEIN-735).
    ///
    /// The two halves are one change and are asserted together, because either alone is worse than
    /// neither. `local_disk_usage` keeping `.skein` put 19.2 GB at the top of "the largest boxes
    /// are" beside a `skein stop <box>` that cannot be pointed at it; dropping it and saying
    /// nothing else would make skein quieter about the same 19.2 GB, on the page somebody opens
    /// precisely because they are asking what took the disk.
    ///
    /// Both readings are arguments here for the reason the rest of this suite's are: this machine
    /// has no fleet, and a `substrate_strays` that resolved one would be walking the owner's.
    ///
    /// **What would make each assertion fail**, watched one at a time against a sabotaged
    /// `disk_verdict`: dropping `fixes.extend(substrate)` (the substrate is never named); joining
    /// the substrate line before the image store's rather than after (something follows the
    /// `rm -rf` and reads as more arguments to it); returning the line's sentence without its
    /// command; letting the substrate line replace the boxes' own advice; turning the `Err` into a
    /// fix rather than into silence, and turning it into an early return that takes the other
    /// fixes with it; and taking the sweep before the healthy-fleet return, which is the one that
    /// costs a tree walk on every board tick of a fleet with room to spare.
    #[test]
    fn a_full_disk_names_what_is_stranded_in_the_substrate_rather_than_a_box_called_dot_skein() {
        // `stray_advice` spells the substrate's real path into the sentence it writes, so this
        // test resolves a fleet path — and unpinned that is `/boxes`, the owner's live fleet
        // (SKEIN-626's guard refuses it outright). Nothing here reads the directory; it is the
        // name that has to be somebody else's.
        let _g = crate::testutil::env_lock();
        let fleet = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &fleet);
        let full = crate::fleet::FleetResources {
            disk_total: 60_000,
            disk_used: 54_140,
            images_total: 50_000,
            images_used: 45_000,
            ..Default::default()
        };
        // What the per-box map answers now that the substrate is not one of its keys: boxes, and
        // the biggest of them holding a fraction of what is stranded beside them.
        let boxes = |_: ()| vec![("web-main".to_string(), 512_u64)];
        let gib = |n: u64| n * 1024 * 1024 * 1024;
        let stranded = || {
            Ok(vec![
                crate::fleet::Stray {
                    name: "target-phase3-agent".to_string(),
                    bytes: gib(12),
                },
                crate::fleet::Stray {
                    name: "target-wave2b-queues".to_string(),
                    bytes: gib(4),
                },
            ])
        };

        let v = disk_verdict(&full, "fleet", boxes, stranded);
        assert_eq!(v.level, Level::Unsatisfied, "{}", v.detail);
        assert!(
            v.fix.contains("target-phase3-agent (12.0G)")
                && v.fix.contains("target-wave2b-queues (4.0G)"),
            "the biggest thing on the disk is not named at all, so the person who opened this \
             page to ask what took the space leaves without the answer: {}",
            v.fix
        );
        assert!(
            v.fix.contains("web-main"),
            "the substrate crowded the boxes out of their own advice: {}",
            v.fix
        );
        // The command is the reader's to run, so it has to survive being read: `fixes` are joined
        // with "; ", and the `rm -rf` spans a line of its own.
        let (_, after) = v
            .fix
            .split_once("rm -rf")
            .unwrap_or_else(|| panic!("no command anybody can copy: {}", v.fix));
        assert!(
            after.contains("target-phase3-agent") && after.contains("target-wave2b-queues"),
            "the command does not name what the sentence above it named: {}",
            v.fix
        );
        assert!(
            !after.contains("; "),
            "another fix was joined on after the `rm -rf`, so it reads as more arguments to it: {}",
            v.fix
        );

        // The sweep refusing to answer is silence. `substrate_strays` errs rather than reporting
        // "nothing is stranded" it cannot stand behind, and that reasoning is about skein — it is
        // not what somebody staring at a full disk came for.
        let refused = disk_verdict(&full, "fleet", boxes, || {
            Err(
                "skein can see no boxes at all, so it is refusing rather than reporting"
                    .to_string(),
            )
        });
        assert!(
            !refused.fix.contains("rm -rf") && !refused.fix.contains("refusing"),
            "a refusal was passed on as advice: {}",
            refused.fix
        );
        assert!(
            refused.fix.contains("web-main") && refused.fix.contains("prune"),
            "one reading being unanswerable took the other fixes with it: {}",
            refused.fix
        );

        // And on a fleet with room left neither reading is taken at all — both walk a tree, and
        // this verdict is computed on every board tick.
        let easy = crate::fleet::FleetResources {
            disk_total: 60_000,
            disk_used: 20_000,
            images_total: 50_000,
            images_used: 20_000,
            ..Default::default()
        };
        let quiet = disk_verdict(&easy, "fleet", boxes, || {
            panic!("the substrate was walked for a fleet that has room left")
        });
        assert_eq!(quiet.level, Level::Satisfied, "{}", quiet.detail);
        std::env::remove_var("SKEIN_FLEET_ROOT");
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
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // Both, because this reaches a fleet path as well as a home: `$SKEIN_FLEET_ROOT` unset is
        // `/boxes`, and the disk and liveness checks act on what they find there.
        let fleet = crate::testutil::tempdir();
        std::env::set_var("SKEIN_FLEET_ROOT", &fleet);
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
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
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

    /// The warden line, against a warden rather than by reading the code.
    ///
    /// Both arms matter and they fail differently. A warden that is not there has to produce a
    /// **fault with a fix** — that is the whole item: without this line the first anybody heard of a
    /// missing warden was a 500 from pressing Launch, weeks after the upgrade that caused it. A
    /// warden that IS there has to be believed about being reachable and quoted, never trusted,
    /// about what it can do: §8.3 says the advertised capability set may decide what skein offers
    /// and may never stand in for a check.
    #[test]
    fn a_warden_that_is_not_answering_is_a_fault_that_says_how_to_start_one() {
        let _g = crate::testutil::env_lock();

        // A port nothing is listening on. Bound and dropped, so the number is real and free —
        // picking one out of the air races another test that happens to have bound it.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = free.local_addr().unwrap().port();
        drop(free);
        std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{dead}"));
        let missing = warden_health(crate::warden_client::sighting());
        assert!(
            missing.is_fault(),
            "a warden that is not there read as fine"
        );
        // Both fields, because both are shown: `skein doctor` prints the detail and the fix on
        // consecutive lines and the diagnostics pane puts one under the other. What has to be true
        // is that between them a reader is told the name of the thing to start AND the command that
        // produces it — the second is the half that was missing, since a plain `cargo build` never
        // built it and "start the warden" is useless advice about a binary you do not have.
        assert!(!missing.fix.is_empty(), "a fault with no way out");
        let shown = format!("{} {}", missing.detail, missing.fix);
        for needed in ["skein-warden", "--workspace"] {
            assert!(
                shown.contains(needed),
                "nothing a reader sees mentions {needed}: {shown:?}"
            );
        }
        // The reason has to be the client's own. "not available" sends nobody anywhere; the address
        // it tried is the thing somebody acts on.
        assert!(
            missing.detail.contains(&dead.to_string()),
            "the fault does not say where it looked: {:?}",
            missing.detail
        );

        // And one that answers, advertising both doers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut raw = [0u8; 4096];
                let _ = stream.read(&mut raw);
                let body = r#"{"sandboxes":["skein-fleet"],"capabilities":["create","destroy"]}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
        let answering = warden_health(crate::warden_client::sighting());
        std::env::remove_var("SKEIN_WARDEN");

        assert!(
            !answering.is_fault(),
            "a warden that answered was still reported broken: {answering:?}"
        );
        assert!(
            answering.fix.is_empty(),
            "a satisfied check carries a fix for a problem it does not have: {:?}",
            answering.fix
        );
        // Quoted, not believed. What it says it can do is in the sentence because a person deciding
        // whether to trust a Launch button wants to see it — and nothing in `health` reads it.
        for said in ["create", "destroy"] {
            assert!(
                answering.detail.contains(said),
                "the report does not pass on what the warden said it can do: {:?}",
                answering.detail
            );
        }
    }

    /// Something answering on the warden's port is not the same fault as nothing being there.
    ///
    /// Written because the first version of this check got it wrong in the way that wastes somebody's
    /// afternoon: every unsatisfied arm printed `cargo build --release --workspace`, so a person
    /// looking at a warden they had just started and were watching log to their terminal was told to
    /// go and build one. The two failures send a reader to opposite places — a compiler, or the
    /// question of what is actually on that port — and the advice has to know which it is looking at.
    #[test]
    fn a_warden_that_answers_and_refuses_is_not_told_to_go_and_build_one() {
        let _g = crate::testutil::env_lock();

        // Something on the port that is not a warden: answers, refuses, says nothing useful. That is
        // exactly the shape a wrong port produces, which is now a thing somebody can arrange by
        // setting `$SKEIN_WARDEN_PORT` without `$SKEIN_WARDEN`.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut raw = [0u8; 2048];
                let _ = stream.read(&mut raw);
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        std::env::set_var("SKEIN_WARDEN", format!("127.0.0.1:{port}"));
        let refused = warden_health(crate::warden_client::sighting());
        std::env::remove_var("SKEIN_WARDEN");

        assert!(
            refused.is_fault(),
            "a warden that would not answer read as fine"
        );
        for absent in ["cargo build", "--workspace"] {
            assert!(
                !refused.fix.contains(absent),
                "the advice tells somebody to build a warden that is plainly running: {:?}",
                refused.fix
            );
        }
        // And it names where it looked, because a wrong port is the likeliest cause and the reader
        // cannot check a number nothing printed.
        assert!(
            refused.fix.contains(&port.to_string()),
            "the advice does not say which address was asked: {:?}",
            refused.fix
        );
        // Both variables, because setting one without the other is how somebody gets here.
        for named in ["$SKEIN_WARDEN", "$SKEIN_WARDEN_PORT"] {
            assert!(
                refused.fix.contains(named),
                "the advice does not name {named}, and the two ends have to agree: {:?}",
                refused.fix
            );
        }
    }

    /// Setting the WARDEN's variable on a CLIENT is said, whichever way the check goes.
    ///
    /// The mistake is invisible from where somebody makes it: `$SKEIN_WARDEN_PORT` on a `skein`
    /// command looks like it moves where skein asks, and moves nothing — skein keeps asking the
    /// default, where something else may well answer. The failure that follows is a refusal from a
    /// stranger, which reads as the warden being broken rather than as being asked the wrong place.
    ///
    /// Said on the satisfied arm too, deliberately. Something answering does not mean it is the
    /// warden the person just started, and "it works" is the reading this has to prevent.
    #[test]
    fn the_wardens_own_variable_set_on_a_client_is_pointed_out() {
        let _g = crate::testutil::env_lock();
        std::env::remove_var("SKEIN_WARDEN");
        std::env::set_var("SKEIN_WARDEN_PORT", "7880");

        // The address it offers is the one this process would have used, not a fixed string: the
        // warden is on the host and skein is not, so that is `host.docker.internal` and a note
        // offering `127.0.0.1` would name the sandbox somebody is already inside.
        let said = warden_health(crate::warden_client::sighting());
        for needed in [
            "SKEIN_WARDEN_PORT",
            "SKEIN_WARDEN=host.docker.internal:7880",
            "7879",
        ] {
            assert!(
                said.detail.contains(needed),
                "the note does not mention {needed}: {:?}",
                said.detail
            );
        }

        // Both set is somebody who meant it, and the note goes away — otherwise it becomes noise on
        // every run of a fleet that has deliberately moved its warden.
        std::env::set_var("SKEIN_WARDEN", "127.0.0.1:7880");
        let quiet = warden_health(crate::warden_client::sighting());
        assert!(
            !quiet.detail.contains("is the WARDEN's variable"),
            "the note fires at somebody who set both: {:?}",
            quiet.detail
        );

        std::env::remove_var("SKEIN_WARDEN_PORT");
        std::env::remove_var("SKEIN_WARDEN");
    }

    /// A missing `sbx` is the normal state, and never a fault.
    ///
    /// Reporting it red would hand somebody a fault they cannot clear — `sbx` is host-only and
    /// cannot be installed into the sandbox — and, worse, would hide behind a false alarm the one
    /// thing they wanted to know: that skein reaches boxes another way. A banner that is red for a
    /// correct state is how the next real fault gets read as noise too.
    ///
    /// This had a second arm: on a host, no `sbx` meant no box could be created, started or
    /// entered, and that was a fault with `PATH` in the fix. There is no such host any more
    /// (SKEIN-576), so the arm that was true for it went with it — recorded in `docs/parity.md`
    /// §7, because the row it produced is one a person used to be able to see.
    ///
    /// **What would make this fail**: making the `(false, _, _)` arm of `sbx_health` unsatisfied
    /// again, which is precisely the deleted arm coming back.
    #[test]
    fn a_missing_sbx_is_the_normal_state_and_never_a_fault() {
        let _g = crate::testutil::env_lock();

        // **Nothing asked**, which is the production state: `fleet_boxes` returns `None` unless
        // something can answer for the host's machine. Satisfied, not unknown — the first-run
        // checklist reads `unknown` as a step somebody must go and fix, and this is the one step a
        // new person cannot fix from inside the cockpit (`tests/ui/onboarding.mjs` asserts it).
        let absent = sbx_health(&None, false);
        assert_eq!(
            absent.level,
            Level::Satisfied,
            "skein reported a question it deliberately does not put as one it could not get an \
             answer to: {}",
            absent.detail
        );
        assert!(
            absent.detail.contains("namespace"),
            "it says sbx is not asked without saying how boxes are reached instead: {}",
            absent.detail
        );

        // And a listing that DID answer still reports what it saw, so the seam that lets something
        // answer for the host has not been collapsed away with the fault.
        let answered = sbx_health(&Some(Vec::new()), false);
        assert_eq!(answered.level, Level::Satisfied);
        assert!(
            answered.detail.contains("available"),
            "a listing that answered stopped saying so: {}",
            answered.detail
        );
        // A stale snapshot is the one case that is neither current nor wrong, which is what the
        // third state is for — and it is not a fault either.
        let stale = sbx_health(&Some(Vec::new()), true);
        assert_eq!(stale.level, Level::Unknown, "{}", stale.detail);
        assert!(!stale.is_fault(), "{}", stale.detail);
    }

    /// The probe finds the path the CLI would derive, on the machine being asked — not one skein
    /// worked out for it.
    ///
    /// `${TMPDIR:-/tmp}/claude-$(id -u)` is the rule, and both halves belong to the other machine:
    /// a fleet's uid is not the host's, and `TMPDIR` is set on macOS and unset in a sandbox. So the
    /// probe is run here against a `TMPDIR` this test controls, and asked what it found.
    #[cfg(unix)]
    #[test]
    fn the_scratch_probe_reads_the_directory_the_runtime_would_derive() {
        let dir = crate::testutil::tempdir();
        let dir = dir.as_ref() as &std::path::Path;
        let ask = |tmp: &std::path::Path| {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(SCRATCH_PROBE)
                .env("TMPDIR", tmp)
                .output()
                .expect("bash");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(dir).unwrap());
        let derived = dir.join(format!("claude-{uid}"));

        // Nothing there: `clear`, and the path it looked at, so a reader can check the derivation
        // rather than take it on trust.
        let empty = ask(dir);
        assert_eq!(
            empty,
            format!("clear {}", derived.display()),
            "the probe looked somewhere other than the path the runtime derives"
        );
        assert!(
            !derived.exists(),
            "the probe CREATED the directory it was asked about, so it answers about itself"
        );
        assert_eq!(
            scratch_verdict(Ok(empty), "the fleet").level,
            Level::Satisfied
        );

        // And ours: reported as ours, with both uids, so the verdict never has to assume which one
        // it is looking at.
        std::fs::create_dir_all(&derived).unwrap();
        let mine = ask(dir);
        assert_eq!(
            mine,
            format!("{uid} {uid} {}", derived.display()),
            "the probe could not say who owns a directory that is there"
        );
        assert_eq!(
            scratch_verdict(Ok(mine), "the fleet").level,
            Level::Satisfied,
            "the fleet's own scratch directory was reported as somebody else's"
        );
    }

    /// A poisoned directory is NAMED, with a way out that is words — never a delete skein runs.
    ///
    /// The owner met this in the middle of a login that had otherwise worked: OAuth completed, and
    /// the CLI then refused because `/tmp/claude-1000` in the fleet's shared /tmp belonged to root.
    /// The CLI's message is a good one; the trouble was that it landed on whoever happened to be
    /// typing. The uid arm cannot be built without root — a test cannot plant a directory it does
    /// not own — so it is driven on the probe's own answer, which the test above pins to the real
    /// thing.
    #[test]
    fn a_poisoned_shared_tmp_is_named_and_its_removal_is_left_to_a_person() {
        let poisoned = scratch_verdict(
            Ok("0 1000 /tmp/claude-1000\n".into()),
            "the fleet sandbox's shared /tmp",
        );
        assert_eq!(
            poisoned.level,
            Level::Unsatisfied,
            "a directory the runtime refuses to start beside is reported as fine: {}",
            poisoned.detail
        );
        for said in ["/tmp/claude-1000", "uid 0", "1000"] {
            assert!(
                poisoned.detail.contains(said),
                "the fault does not name {said}, so nobody can act on it: {}",
                poisoned.detail
            );
        }
        assert!(
            poisoned.fix.contains("/tmp/claude-1000") && poisoned.fix.contains("uid 0"),
            "the way out names neither the path nor the uid that can clear it: {}",
            poisoned.fix
        );
        assert!(
            poisoned.destructive,
            "a recipe that deletes a directory in a shared /tmp is drivable — §2.4 says printed, \
             never run, and this is the exact shape of the guard the runtime applies"
        );

        // Asked and not answered is the third state, not a fault: a sandbox that will not answer
        // is not evidence that anything is wrong in it.
        let silent = scratch_verdict(Err("sbx did not answer".into()), "the fleet");
        assert_eq!(silent.level, Level::Unknown);
        assert!(
            silent.fix.is_empty(),
            "an unknown offers a fix: {}",
            silent.fix
        );
        let garbled = scratch_verdict(Ok("what\n".into()), "the fleet");
        assert_eq!(
            garbled.level,
            Level::Unknown,
            "an answer this cannot read was turned into a claim about the fleet"
        );
    }
}
