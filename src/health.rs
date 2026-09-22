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
///
/// **The `None` arm says the sandbox did not answer, and that is now the only thing it can say**
/// (SKEIN-770). It used to say "no fleet sandbox is configured", which was false about its own
/// cause and sent a person to Settings to fix a field holding a name:
/// [`crate::fleet::fleet_resources`] had two ways to answer `None`, and
/// [`crate::config::load_config`] had already foreclosed the blank-name one (`src/config.rs:459`)
/// before SKEIN-756 deleted that arm outright. What is left is the measurement itself, and there
/// are exactly two ways for it to fail — the command did not run (it could not be spawned, it
/// outlived the 20s deadline, or it exited non-zero: `src/place.rs:1218` and `:1219`) or it ran and
/// printed nothing `fleet::parse_resources` could read (`src/fleet.rs:4539`). `Option`
/// carries no room to tell those apart, so the sentence says it cannot rather than picking one.
///
/// It also says one thing the neighbour in `disk_verdict` cannot: the [`crate::util::Gate`] keeps
/// the last good answer forever — `invalidate` expires the clock and never `good` — so `None` means
/// **nothing has arrived since this process started**, where a `disk_total` of 0 means a reading
/// arrived and carried no total.
pub fn disk_health() -> HealthCheck {
    let Some(r) = crate::fleet::fleet_resources() else {
        return HealthCheck::unknown(format!(
            "the sandbox has not answered the disk reading since skein started, and which half \
             failed cannot be told apart from here: either the measuring command did not run, or \
             it ran and printed nothing this could read. So there are no figures at all — not even \
             stale ones. Ask the same question directly with `df -Pm {root}`; skein re-asks at most \
             every 30 seconds and this clears itself the moment one reading arrives, with nothing \
             to restart",
            root = crate::fleet::fleet_root()
        ));
    };
    // The per-box figures are only READ when something is actually full: they come from their own
    // gate and a tree walk behind it, and a satisfied check has nothing to name them for. The same
    // goes for all three sweeps below, each of which walks real trees.
    //
    // The threshold is read HERE rather than inside `disk_verdict`, for the reason every other
    // reading is an argument to it: this suite runs on machines with no fleet and no settings file,
    // and a verdict that reached for `load_config` could not be driven by a test without one.
    let days = crate::config::load_config().stale_build_days;
    disk_verdict(
        &r,
        &crate::place::fleet_sandbox(),
        |_: ()| biggest_first(),
        crate::fleet::substrate_strays,
        crate::fleet::orphaned_builds,
        move || crate::fleet::stale_builds(days),
        days,
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

/// MiB as a person reads it. One place, because [`crate::announce`] prints the same figures the
/// health row does and two formatters are two ways to print one number.
pub(crate) fn gib(mib: u64) -> String {
    format!("{:.1}G", mib as f64 / 1024.0)
}

/// The fleet's disk as [`crate::announce`] needs it: how much has to go, and who is holding it.
///
/// One value rather than two calls, because the two halves have to be read of the same fleet. The
/// announcement's whole claim is that freeing *this much* *here* clears the line, and an overage
/// taken from one reading beside a ranking taken from another is a claim about no fleet that ever
/// existed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskDemand {
    /// MiB that must be freed on the boxes' filesystem to bring it back under [`DISK_FULL_PCT`].
    ///
    /// **Zero is an answer, not a gap.** [`disk_verdict`] is a fault when *either* filesystem is
    /// past the line, so a fleet whose image store is full while the boxes' disk has room is over
    /// the line with nothing for any box to free — and so is a fleet sitting exactly on it.
    /// Nothing is asked of anybody then; who is still told is [`crate::announce`]'s question.
    pub over_by: u64,
    /// Every box holding disk, largest first — [`biggest_first`] itself, never a second ordering.
    pub boxes: Vec<(String, u64)>,
}

/// [`DiskDemand`] for the live fleet.
///
/// The [`crate::fleet::fleet_resources`] read costs nothing beside [`disk_health`]'s: it is behind
/// a thirty-second gate (`src/fleet.rs:4369`), so one tick's verdict and its demand are the same
/// reading rather than two.
pub(crate) fn disk_demand() -> DiskDemand {
    DiskDemand {
        over_by: crate::fleet::fleet_resources()
            .map(|r| over_by(&r))
            .unwrap_or(0),
        boxes: biggest_first(),
    }
}

/// How far the boxes' filesystem is above the line, in MiB.
///
/// `disk_used - disk_total * DISK_FULL_PCT / 100` — the owner's own arithmetic, over the two
/// figures [`disk_verdict`] already reads, so nothing here is a number anybody invented.
///
/// **Multiplied before it is divided**, which is not a style choice: [`disk_verdict`] calls it a
/// fault when `disk_used * 100 / disk_total` reaches `DISK_FULL_PCT`, and only this order puts the
/// first MiB of overage on exactly the reading that first becomes a fault. Divide first and a
/// 60,168 MiB filesystem is 57 MiB out — a demand to free space from a fleet the same module has
/// just called satisfied.
///
/// Saturating twice over. Below the line there is nothing to free and this says zero rather than
/// wrapping into a demand for more disk than the fleet has; and a total of zero is the reading that
/// did not arrive — [`disk_verdict`] answers `Unknown` for it — so the demand it implies is zero
/// and emphatically not the whole of `disk_used`.
fn over_by(r: &crate::fleet::FleetResources) -> u64 {
    match r.disk_total {
        0 => 0,
        total => r.disk_used.saturating_sub(total * DISK_FULL_PCT / 100),
    }
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
///
/// **`orphans` and `stale` are the same shape and the same rules**, and they are here because
/// `strays`' answer moved: what it was written for was cleared by hand and the bytes went *inside*
/// the boxes, where every figure this function already prints counts them as the box's own and
/// therefore as wanted (SKEIN-975, SKEIN-974). Three readings rather than one because they are
/// cleared by three different commands and name three different sets of paths — the reason the two
/// filesystems are named apart a few lines below, applied again.
///
/// **They are ordered by how strong their evidence is, and a path named by the stronger one is
/// never named again by the weaker.** `orphans` can say a directory is dead and show its working;
/// `stale` can only say nothing has been near it. On the fleet these were measured against, the
/// single largest stale directory was *also* the single largest orphan, so without that filter the
/// same 5.9 GiB appeared twice under two reasons and two commands, and the totals a reader adds up
/// were wrong by the size of the biggest item in them.
fn disk_verdict(
    r: &crate::fleet::FleetResources,
    sandbox: &str,
    biggest: impl FnOnce(()) -> Vec<(String, u64)>,
    strays: impl FnOnce() -> Result<Vec<crate::fleet::Stray>, String>,
    orphans: impl FnOnce() -> Result<Vec<crate::fleet::OrphanedBuild>, String>,
    stale: impl FnOnce() -> Result<Vec<crate::fleet::StaleBuild>, String>,
    stale_days: u32,
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
    // Held back until every other fix is in, because each ends in a command spanning a line of its
    // own and anything joined after that reads as part of the command. They are separated from one
    // another by a NEWLINE and not by the "; " the inline fixes use, for that same reason: a second
    // offer joined onto the end of a `rm -rf` is a second offer a reader may paste as arguments to
    // the first.
    let mut offers: Vec<String> = Vec::new();
    if boxes_pct >= DISK_FULL_PCT {
        offers.extend(
            strays()
                .ok()
                .as_deref()
                .and_then(crate::fleet::stray_advice),
        );
        // Strongest evidence first, and what it names is subtracted from the weakest: the build
        // directory whose own `.d` files prove its tree is gone must not come back a second time
        // merely because nothing has touched it lately.
        let dead = orphans().ok().unwrap_or_default();
        offers.extend(crate::fleet::orphaned_build_advice(&dead));
        let mut aged = stale().ok().unwrap_or_default();
        aged.retain(|s| !dead.iter().any(|d| d.path == s.path));
        offers.extend(crate::fleet::stale_build_advice(&aged, stale_days));
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
    let fix = match (fixes.is_empty(), offers.is_empty()) {
        (_, true) => fixes.join("; "),
        (true, false) => offers.join("\n"),
        (false, false) => format!("{}\n{}", fixes.join("; "), offers.join("\n")),
    };
    let check = HealthCheck::unsatisfied(detail, fix);
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
    /// A fault here sets `ok = false`, so the banner appears and the page names it. Every check
    /// counted here must also be in the page's `CHECKED`, or `ok: false` puts a red row above the
    /// app with nothing in it to read — `every_check_the_report_carries_is_named_on_the_page`
    /// keeps the two level.
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

impl HealthReport {
    /// How many checks [`HealthReport::checks`] returns — the length of its array, named rather
    /// than written into the signature so that a test can read it without building a report
    /// (`every_health_check_field_is_named_in_the_list`).
    pub const CHECK_COUNT: usize = 14;

    /// Every check in the report, named, each with whether it turns the banner red
    /// ([`OnBanner`]). One list, so a check added to the struct and forgotten here shows up as a
    /// compile error rather than as a check nothing ever looks at.
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
    /// **And the third element is the same argument one step on** (SKEIN-1003). `ok` used to be a
    /// separate array of eleven `&field` references, so a new check was silently *excluded* from
    /// the verdict exactly as `token_expiry` had been silently excluded from this list — same
    /// hole, one function down. `ok` is derived from this array now
    /// ([`HealthReport::first_counted_fault`]), so a check cannot reach the report without
    /// somebody having written down whether it belongs on the banner and, if not, why not.
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
            logins: _,
            expired_logins: _,
            runtime_updates: _,
            models: _,
            dark_boxes: _,
            stale_boxes: _,
            uncovered_boxes: _,
            uncapped_boxes: _,
            runtimes: _,
            git_credential: _,
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
            // The one check that is a *state readout* rather than a verdict: it is the enrichment
            // toggle's own state, which the settings pane prints beside the checkbox, and it is
            // built with `HealthCheck::satisfied` on every branch of `health_report` — "off" is a
            // correct state, not a fault. Counting a value that cannot be a fault would be
            // decoration; naming it here is what makes anyone who gives it a fault arm come back
            // and decide, instead of inheriting a silence.
            ("ai", ai, NotCounted(AI_IS_A_STATE_NOT_A_VERDICT)),
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
            // Named for what it is about rather than for the field, like "isolation" below: `/v2`
            // prints this key verbatim as the row's name (`src/web/v2.html:229`), beside box names
            // and PR numbers. "expiry" on its own would be the one-word version and it is wrong
            // here — this report also carries `expired_logins`, so an unqualified "expiry" names
            // two different deadlines with different owners. "token life" is the label the
            // cockpit's own `CHECKS` gives this check, so the diagnostics pane and the queue say
            // one thing rather than two.
            ("token life", token_expiry, Counted),
            // "proxy" alone would read as "is the proxy working", which is not the question: the
            // check is about WHOSE credential the proxy answers with, and a proxy that is working
            // perfectly is exactly the case it fires on. "proxy credential" is what the same
            // `CHECKS` calls it, for the same reason.
            ("proxy credential", proxy_injection, Counted),
            ("warden", warden, Counted),
            // Named for what it is about rather than for the field: this key is what `/v2` puts
            // on the row, and "isolation" is a word somebody can act on where "cover" is jargon.
            ("isolation", cover, Counted),
        ]
    }

    /// Every check in the report, named — the list [`crate::queue::who_needs_you`] and `skein
    /// doctor` iterate, which is every check whether or not it reaches the banner.
    ///
    /// Derived from [`HealthReport::checks_with_banner`] rather than written out again: two lists
    /// of the same fourteen checks is how this file came to have three of them and no two the
    /// same set (SKEIN-1003).
    pub fn checks(&self) -> [(&'static str, &HealthCheck); Self::CHECK_COUNT] {
        self.checks_with_banner()
            .map(|(key, check, _)| (key, check))
    }

    /// **The first check that is both a fault and counted — the whole of what turns the banner
    /// red**, and `None` when there is none.
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

/// Whether a DIRECT GitHub connection lands, told apart from what GitHub answers once it does.
///
/// The distinction is the whole point (SKEIN-548, SKEIN-926): a 401/403 is GitHub answering, and a
/// blocked egress policy is GitHub never being reached. Only the second is skein's to explain with a
/// policy command; the first is an ordinary auth answer nobody should be told to change a firewall
/// over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GithubReach {
    /// The connection landed and GitHub returned some HTTP status — reachable, whatever the status.
    Reachable,
    /// The connection could not be made at all — refused, timed out, or DNS failed.
    Blocked,
}

/// Turn a direct-reachability outcome into the health line. Pure, so the blocked-vs-answer wording
/// — the user-visible half — is proven without a network.
pub(crate) fn github_reach_line(reach: GithubReach, fleet: &str) -> HealthCheck {
    match reach {
        // Not a fault: GitHub is reachable, so the scoped path presents this box's/host's own token
        // and GitHub is the one enforcing it.
        GithubReach::Reachable => HealthCheck::satisfied(
            "available — GitHub is reachable directly, so the token skein holds is the one GitHub \
             sees",
        ),
        // The approved wording (SKEIN-548), split across the diagnosis and its recipe. `{fleet}` is
        // the real sandbox name, derived from config on the host where this runs.
        GithubReach::Blocked => HealthCheck::unsatisfied(
            "GitHub is blocked by the sandbox's network policy",
            format!(
                "on your host run:  sbx policy allow network --sandbox {fleet} \
                 github.com,api.github.com"
            ),
        ),
    }
}

/// Probe `target` DIRECT (never through the proxy) and classify the outcome. A real HTTP status —
/// including 401/403 — is [`GithubReach::Reachable`]; only a failure to connect is
/// [`GithubReach::Blocked`]. `curl` without `-f` exits 0 for any response and writes `000` with a
/// non-zero exit when it could not connect, so the http_code alone decides it.
pub(crate) fn probe_github_reach_at(target: &str) -> GithubReach {
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "--noproxy",
            "*",
            "-I",
            "-o",
            "/dev/null",
            "-m",
            "5",
            "--connect-timeout",
            "3",
            "-w",
            "%{http_code}",
            target,
        ])
        .output();
    match out {
        Ok(out) => {
            let code = String::from_utf8_lossy(&out.stdout);
            let code = code.trim();
            if code.len() == 3 && code != "000" && code.bytes().all(|b| b.is_ascii_digit()) {
                GithubReach::Reachable
            } else {
                GithubReach::Blocked
            }
        }
        // curl failed to even spawn. Presence is the caller's concern; here that reads as no
        // connection.
        Err(_) => GithubReach::Blocked,
    }
}

/// The `gh` health line: curl is present (the caller has checked), so this answers reachability.
///
/// **It must not spend the box's shared api.github.com budget from a test** (SKEIN-693), and it must
/// not probe the network on a polled endpoint gratuitously — so a test that wants to exercise this
/// pins `$SKEIN_GITHUB_REACH_URL` at its own listener, and without that pin an in-test call reports
/// reachable rather than reaching out. In production it probes `github.com` — the web host, not the
/// rate-limited REST API — because all that matters is whether a connection to GitHub can be made.
fn github_reach_health(fleet: &str) -> HealthCheck {
    let pinned = std::env::var("SKEIN_GITHUB_REACH_URL")
        .ok()
        .filter(|v| !v.is_empty());
    match pinned {
        Some(target) => github_reach_line(probe_github_reach_at(&target), fleet),
        None if crate::util::in_test() => HealthCheck::satisfied("available"),
        None => github_reach_line(probe_github_reach_at("https://github.com/"), fleet),
    }
}

// ---------- what the sandbox proxy does with a credential (SKEIN-548) ----------

/// **The credential the probe sends, and the only one it ever sends.**
///
/// `skein-test-` by convention across this tree — `tests/github_reach_live.rs:47` sends the same
/// shape for the same reason — so that a copy of it in a log, a terminal or a health report is not
/// a disclosure. The probe reads a **status code and one rate-limit header** back, and discards
/// the rest of the response unread: no body, and no credential of anybody's in either direction.
/// That is not politeness, it is the property that makes this check safe to run unattended, on a
/// polled endpoint, on somebody else's fleet.
pub(crate) const PROBE_CREDENTIAL: &str = "skein-test-not-a-credential";

/// Where the probe asks, and it is `/rate_limit` for two reasons rather than one.
///
/// It discriminates as sharply as `/user` — an invalid credential is `401` on both — and **it does
/// not spend the rate limit**, so a check that runs every hour for the life of a fleet costs
/// nothing from the 60-an-hour anonymous pool that every box behind one egress IP shares. Both
/// halves of that were measured here on 2026-09-21 rather than read from documentation: the pool
/// was exhausted at the time by ordinary traffic (`x-ratelimit-used: 60`), `/user` answered `403`
/// rate-limit-exceeded in that state, and `/rate_limit` answered `200` in the same second. It also
/// carries the ceiling the arm below needs, which `/user` does not.
const PROBE_TARGET: &str = "https://api.github.com/rate_limit";

/// GitHub's hourly ceiling for a request nobody authenticated — measured through the proxy and
/// direct on 2026-09-21, `x-ratelimit-limit: 60` both ways. An authenticated one is orders above
/// it (SKEIN-927 recorded 5000 on the day injection was live), so the two never collide and the
/// exact authenticated figure does not need to be written down here.
const ANONYMOUS_CEILING: u32 = 60;

/// **What the sandbox proxy did with a credential it was handed** (SKEIN-548).
///
/// The question `NO_PROXY` cannot answer. `src/box-session.sh:2098` routes a scoped box's `git`
/// and `gh` around the proxy, and the launcher says in its own comment that this narrows the
/// normal path rather than containing anything — "a variable anything can set again is not a
/// containment". So what matters is what the proxy does to a request that *is* on it, and the only
/// way to find that out is to send something that cannot possibly be valid and see whether it
/// works anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProxyCredential {
    /// A credential that cannot be valid came back **authenticated** — accepted, and with an
    /// hourly ceiling above [`ANONYMOUS_CEILING`]. Something other than the credential that was
    /// sent answered for it, which is the injection SKEIN-548 measured on 2026-09-06.
    Injected { ceiling: u32 },
    /// **Nothing account-wide was added**, which is the claim this check actually makes and covers
    /// both ways of not adding one: the invalid credential arrived and GitHub refused it, or the
    /// request was answered anonymously.
    ///
    /// The second is not a hypothetical corner. sbx v0.43.0's note — quoted on SKEIN-548 — is that
    /// the proxy "no longer forwards a client-supplied credential the proxy did not issue" to a
    /// managed provider host, and a strip with nothing put back is exactly an anonymous `200`. A
    /// probe that read the status alone would call that injection and paint the banner red over a
    /// proxy that had added nothing at all.
    Untouched,
    /// No proxy is configured in this environment, so there is nothing in the path to inject.
    Absent,
    /// Neither an acceptance nor a refusal came back. `why` says what did.
    Unanswered(String),
}

/// Turn the reading into the line. **Pure**, so every sentence the owner sees is proven without a
/// network and without a credential of any kind.
///
/// Only [`ProxyCredential::Injected`] is a fault, and only an *authenticated* answer produces one
/// — so an `Unanswered` can never hide an injection and can never manufacture one. That ordering is
/// the same one [`token_expiry_line`] keeps, for the same reason: this is a check whose false
/// positive would put a red banner across a working fleet.
pub(crate) fn proxy_injection_line(seen: ProxyCredential, fleet: &str) -> HealthCheck {
    match seen {
        ProxyCredential::Injected { ceiling } => HealthCheck::unsatisfied(
            format!(
                "the sandbox proxy answers GitHub as the account: a deliberately invalid \
                 credential came back authenticated through it, on an hourly ceiling of {ceiling} \
                 where an unauthenticated request gets {ANONYMOUS_CEILING}. So anything in a box \
                 that routes through $HTTPS_PROXY reaches every repository the account can, \
                 whatever token that box holds"
            ),
            format!(
                "on your HOST, set what this sandbox injects — `sbx secret set github --sandbox \
                 {fleet}` — to a token bounded to the repositories the fleet should reach, or to a \
                 dummy value, which turns injection off and leaves boxes on skein's own per-repo \
                 tokens. Set it again after any fleet rebuild: `sbx rm` deletes a sandbox-scoped \
                 secret along with the sandbox"
            ),
        ),
        ProxyCredential::Untouched => HealthCheck::satisfied(
            "the sandbox proxy adds no credential of its own to GitHub — a deliberately invalid \
             one sent through it was not answered as anybody, so a box reaches GitHub as whatever \
             token it actually holds",
        ),
        ProxyCredential::Absent => HealthCheck::satisfied(
            "no proxy is configured here, so there is nothing in the path to put a credential on a \
             request that carries none",
        ),
        ProxyCredential::Unanswered(why) => HealthCheck::unknown(format!(
            "skein could not tell whether the sandbox proxy injects a credential — {why}"
        )),
    }
}

/// Send [`PROBE_CREDENTIAL`] to `target` **through `proxy`** and classify what comes back.
///
/// `--noproxy ''` empties curl's bypass list rather than inheriting it, and that is load-bearing:
/// in a scoped box `$NO_PROXY` names exactly the GitHub hosts (`src/box-session.sh:2098`), so a
/// probe that honoured it would go direct and answer a question nobody asked — "does GitHub refuse
/// a garbage token", to which the answer is always yes.
///
/// `-D -` puts the response **headers** on stdout, because the status alone is not enough to tell
/// an injected credential from a stripped one — see [`ProxyCredential::Untouched`]. The body still
/// goes to `/dev/null`: nothing this reads is anybody's secret, and nothing it does not read can
/// become one.
pub(crate) fn probe_proxy_injection_at(proxy: &str, target: &str) -> ProxyCredential {
    let header = format!("Authorization: Bearer {PROBE_CREDENTIAL}");
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "-x",
            proxy,
            "--noproxy",
            "",
            "-D",
            "-",
            "-o",
            "/dev/null",
            "-m",
            "8",
            "--connect-timeout",
            "4",
            "-H",
            header.as_str(),
            "-w",
            "\nskein-http-code %{http_code}",
            target,
        ])
        .output();
    let out = match out {
        Ok(out) => out,
        // curl is not installed, or could not be started. Presence is the `gh` line's concern; here
        // it reads as a question that was not asked rather than as an answer.
        Err(why) => return ProxyCredential::Unanswered(format!("curl did not run: {why}")),
    };
    let answer = String::from_utf8_lossy(&out.stdout);
    let code = answer
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("skein-http-code "))
        .unwrap_or("")
        .trim()
        .to_string();
    // The one header this reads, and the reason it is read at all: it is the difference between a
    // request somebody was authenticated for and one nobody was.
    let ceiling = answer
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("x-ratelimit-limit")
                .then(|| value.trim().parse::<u32>().ok())?
        })
        .next_back();
    match (code.as_str(), ceiling) {
        // Accepted, and accepted as somebody: a ceiling above the anonymous one is GitHub saying it
        // authenticated this request, and it cannot have authenticated the string that was sent.
        ("200", Some(ceiling)) if ceiling > ANONYMOUS_CEILING => {
            ProxyCredential::Injected { ceiling }
        }
        // Accepted anonymously. The credential was dropped on the way rather than replaced, so
        // nothing account-wide was added, which is what this check is about.
        ("200", Some(_)) => ProxyCredential::Untouched,
        // Accepted with no ceiling to read at all. Not an injection this can stand behind — and
        // this check does not guess, because a red banner nobody can confirm is one people learn
        // to scroll past (SKEIN-913).
        ("200", None) => ProxyCredential::Unanswered(
            "the probe was accepted but carried no x-ratelimit-limit, so whether anybody was \
             authenticated for it cannot be told from here"
                .to_string(),
        ),
        // GitHub refusing the probe's own credential is the whole point: it arrived as sent.
        ("401", _) => ProxyCredential::Untouched,
        // curl's own code for "no connection was made", and its empty output when it died first.
        ("000" | "", _) => {
            ProxyCredential::Unanswered("the probe could not reach the proxy at all".to_string())
        }
        // A rate limit is the common one, and it is genuinely not an answer to this question: an
        // unauthenticated `403` and an injected-but-throttled `403` look identical from here.
        (other, _) => ProxyCredential::Unanswered(format!(
            "the probe was answered {other}, which is neither an acceptance nor a refusal"
        )),
    }
}

/// Ask the proxy the question, and say so on the board when the answer is yes.
///
/// **Behind a gate for [`token_expiry_health`]'s reason**: `/api/health` is polled every fifteen
/// seconds by every open board, and this costs an HTTP request. **An hour**, and the interval is
/// argued from measurement rather than from taste — this is a property of the substrate and not of
/// anything skein installs, and it changed under this fleet inside six days (the dates are in
/// `docs/threat-model.md`) with nothing in the tree to say so. A day would have been wrong.
///
/// **It does not reach out from a test.** The rule [`github_reach_health`] and
/// [`token_expiry_health`] both keep: a unit test that depends on a network fails for somebody
/// else's reason, and this one would spend the box's shared api.github.com budget as well. A test
/// that wants the live path pins `$SKEIN_PROXY_PROBE_URL` at its own listener, exactly as
/// `$SKEIN_GITHUB_REACH_URL` does for the neighbour above.
fn proxy_injection_health(fleet: &str) -> HealthCheck {
    static GATE: crate::util::Gate<HealthCheck> = crate::util::Gate::new();
    let proxy = ["HTTPS_PROXY", "https_proxy"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    // No proxy is a complete answer, not a gap — and it is the answer on every deployment that is
    // not inside an sbx sandbox, which is most of them.
    let Some(proxy) = proxy else {
        return proxy_injection_line(ProxyCredential::Absent, fleet);
    };
    let pinned = std::env::var("SKEIN_PROXY_PROBE_URL")
        .ok()
        .filter(|value| !value.is_empty());
    match pinned {
        Some(target) => proxy_injection_line(probe_proxy_injection_at(&proxy, &target), fleet),
        None if crate::util::in_test() => HealthCheck::unknown("not asked from a test"),
        None => {
            let fleet = fleet.to_string();
            GATE.get(std::time::Duration::from_secs(60 * 60), move || {
                Some(proxy_injection_line(
                    probe_proxy_injection_at(&proxy, PROBE_TARGET),
                    &fleet,
                ))
            })
            .unwrap_or_else(|| HealthCheck::unknown("skein has not been able to ask the proxy yet"))
        }
    }
}

// ---------- a GitHub credential's deadline, before it is one (SKEIN-928) ----------

/// **How much notice the owner gets before a GitHub credential expires.**
///
/// Thirty days, and the number is chosen against what renewing actually costs rather than against a
/// round figure. Nothing inside the fleet can renew one of these: the token is regenerated on
/// github.com by the person whose account it is, and then re-set against the sandbox from the host.
/// So the window has to be long enough to survive the owner being away from the machine, which a
/// week is not — and short enough that the line is not permanently on the board, which a quarter
/// would be.
///
/// It is a fault rather than a note for the same reason `cover` is: an expiring credential has no
/// symptom at all until the day it has no symptoms left, and on that day every box loses GitHub at
/// once and it reads as an auth bug rather than as a date. If the banner does not say it, nothing
/// does.
pub const RENEW_WINDOW_DAYS: i64 = 30;

/// **What to do about this one**, by where it came from. Prose rather than one command for the
/// sources where a command would be a lie: a token stored in Settings is replaced in Settings, and
/// inventing a CLI for it would send somebody to a prompt that cannot help them.
///
/// The `$GH_TOKEN` arm is the fleet's own sandbox-scoped secret, and its recipe carries the two
/// halves that were learned the hard way (SKEIN-928): the permissions the replacement needs, so the
/// new token is not narrower than the one it replaces, and the fact that a fleet rebuild drops it
/// again, because `sbx rm` deletes a sandbox-scoped secret with the sandbox.
pub(crate) fn renew_recipe(source: crate::prq::GhToken, fleet: &str) -> String {
    use crate::prq::GhToken::*;
    match source {
        Environment => format!(
            "regenerate it on github.com under Settings → Developer settings → personal access \
             tokens, with the same repositories and Contents + Pull requests read/write, then on \
             your HOST run:  sbx secret set github --sandbox {fleet}   — and again after any fleet \
             rebuild, because `sbx rm` deletes a sandbox-scoped secret along with the sandbox"
        ),
        ReadToken | WritePat => "regenerate it on github.com with the same repositories and \
                                 permissions, then paste it over the old one in Settings → GitHub \
                                 & keys"
            .to_string(),
        GhCli => "run `gh auth login` again on the host that holds this login".to_string(),
        // Unreachable from `credential_lives`, which lists only credentials it found. Written out
        // rather than left to a catch-all so that adding a source to `GhToken` fails to compile
        // here instead of silently acquiring the wrong recipe.
        None => String::new(),
    }
}

/// Turn the readings into the line. **Pure**, so every sentence the owner sees — and the threshold
/// that decides whether they see one at all — is proven without a network or a credential.
///
/// The order of the three arms is the order of urgency, and it is the property worth stating: a
/// deadline inside the window is a fault, a reading skein could not take is an `unknown`, and only
/// when neither of those is true is this a pass. An `unknown` can therefore never hide a fault, and
/// a fault can never be downgraded by something else failing to answer.
pub(crate) fn token_expiry_line(lives: &[crate::prq::CredentialLife], fleet: &str) -> HealthCheck {
    use crate::prq::Life;
    if lives.is_empty() {
        return HealthCheck::satisfied(
            "no GitHub credential is stored, so there is nothing here to expire",
        );
    }
    // The nearest deadline decides the line: it is the one that will strand the fleet first, and a
    // report that leads with anything else buries it.
    let nearest = lives
        .iter()
        .filter_map(|c| match &c.life {
            Life::Expires { when, days } => Some((*days, when, c)),
            _ => None,
        })
        .min_by_key(|(days, _, _)| *days);
    if let Some((days, when, credential)) = nearest {
        if days <= RENEW_WINDOW_DAYS {
            let clock = match days {
                d if d < 0 => format!("expired {} day(s) ago, on {when}", -d),
                0 => format!("expires TODAY, on {when}"),
                d => format!("expires in {d} day(s), on {when}"),
            };
            return HealthCheck::unsatisfied(
                format!(
                    "{} {clock}. Nothing inside the fleet can renew it, and when it goes every \
                     box loses GitHub at once — API calls and `git push` alike, which reads as an \
                     auth bug rather than as a date",
                    credential.label
                ),
                renew_recipe(credential.source, fleet),
            );
        }
    }
    // Something could not be asked. Reported, never counted as a fault — and never as a pass
    // either, which is the arm that matters: GitHub answers a DEAD credential with a 401 and no
    // expiry header at all, so "skein could not tell" is exactly what the worst case looks like.
    if let Some(why) = lives.iter().find_map(|c| match &c.life {
        Life::Unanswered(why) => Some(format!("{}: {why}", c.label)),
        _ => None,
    }) {
        return HealthCheck::unknown(format!(
            "skein could not read an expiry for every credential it holds — {why}"
        ));
    }
    match nearest {
        Some((days, when, credential)) => HealthCheck::satisfied(format!(
            "the nearest deadline is {} in {days} day(s), on {when}",
            credential.label
        )),
        Option::None => HealthCheck::satisfied(format!(
            "{} credential(s), none of which GitHub gives an expiry date",
            lives.len()
        )),
    }
}

/// The line itself: ask GitHub about every credential skein holds, and say what it answered.
///
/// **Behind a gate, and the gate is not an optimisation.** `/api/health` is polled every fifteen
/// seconds by every open board, and this costs one HTTP request per stored credential — so without
/// one, a fleet with three tokens and four tabs open would spend a thousand requests an hour asking
/// a question whose answer changes once a day. **An hour, not the six that a date's own pace would
/// justify**, because the gate remembers an `unknown` exactly as readily as an answer: a moment's
/// unreachable GitHub would otherwise sit on the board saying so all afternoon.
///
/// **It does not reach out from a test.** Same rule and same reason as `github_reach_health`: a
/// test must not spend the box's shared api.github.com budget, and a unit test that depends on a
/// network is a test that fails for somebody else's reason. The sentences are proven through
/// [`token_expiry_line`], which needs neither.
pub fn token_expiry_health() -> HealthCheck {
    static GATE: crate::util::Gate<HealthCheck> = crate::util::Gate::new();
    if crate::util::in_test() {
        return HealthCheck::unknown("not asked from a test");
    }
    let fleet = crate::place::fleet_sandbox();
    GATE.get(std::time::Duration::from_secs(60 * 60), move || {
        Some(token_expiry_line(
            &crate::prq::credential_lives(chrono::Utc::now()),
            &fleet,
        ))
    })
    .unwrap_or_else(|| HealthCheck::unknown("skein has not been able to ask GitHub yet"))
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
        token_expiry,
        proxy_injection,
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
    };
    // Stale sessions are the second half and are not a check: there is no `HealthCheck` for them,
    // only a list of box names, and the banner prints the count rather than a sentence.
    report.ok = report.first_counted_fault().is_none() && report.stale_boxes.is_empty();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The build sweeps, answering "nothing", for the tests that are about something else.
    ///
    /// Named functions rather than `|| Ok(Vec::new())` written eight times: the point of every one
    /// of these sweeps being an argument is that no test on a machine with no fleet walks the
    /// owner's `/boxes`, and a spelling that has to be repeated is one somebody will eventually
    /// replace with the real function to save a line.
    fn no_orphans() -> Result<Vec<crate::fleet::OrphanedBuild>, String> {
        Ok(Vec::new())
    }

    fn no_stale() -> Result<Vec<crate::fleet::StaleBuild>, String> {
        Ok(Vec::new())
    }

    /// One credential reading, for the expiry tests below. Nothing here reaches GitHub — the whole
    /// point of [`token_expiry_line`] being pure is that the sentence and the threshold are proven
    /// without a network or a credential.
    fn reading(label: &str, life: crate::prq::Life) -> crate::prq::CredentialLife {
        crate::prq::CredentialLife {
            source: crate::prq::GhToken::Environment,
            label: label.to_string(),
            life,
        }
    }

    fn expires(label: &str, days: i64) -> crate::prq::CredentialLife {
        reading(
            label,
            crate::prq::Life::Expires {
                when: "2026-10-15 13:19:49 UTC".into(),
                days,
            },
        )
    }

    /// **The warning fires inside the window and is silent outside it** (SKEIN-928).
    ///
    /// This is the assertion the whole item is for: skein has to see the deadline coming with
    /// enough notice to be acted on, and it has to stay quiet the rest of the time, because a line
    /// that is always on the board is a line nobody reads on the day it matters.
    ///
    /// Counterfactuals, each one named before the assertion was written and each one proven by
    /// sabotage: narrowing `days <= RENEW_WINDOW_DAYS` to `days < 0` — warn only once it is already
    /// too late — makes `a deadline inside the window must be a fault` fail; widening it to
    /// `days <= RENEW_WINDOW_DAYS * 3` makes `a deadline outside the window must be silent` fail.
    /// The boundary day is asserted on purpose: an off-by-one there is a whole day of notice, and
    /// it is exactly the sort of thing that is never noticed from the outside.
    #[test]
    fn a_deadline_inside_the_window_is_a_fault_and_one_outside_it_is_silent() {
        let near = token_expiry_line(
            &[expires("$GH_TOKEN", RENEW_WINDOW_DAYS - 1)],
            "thing-fleet",
        );
        assert!(
            near.is_fault(),
            "a deadline inside the window must be a fault: {near:?}"
        );
        assert!(
            near.detail.contains("$GH_TOKEN") && near.detail.contains("2026-10-15 13:19:49 UTC"),
            "the fault must name the credential and the date: {}",
            near.detail
        );
        assert!(
            !near.fix.is_empty(),
            "an unsatisfied check must never have an empty fix"
        );

        let boundary = token_expiry_line(&[expires("$GH_TOKEN", RENEW_WINDOW_DAYS)], "thing-fleet");
        assert!(
            boundary.is_fault(),
            "the window is inclusive: {RENEW_WINDOW_DAYS} days out must still warn"
        );

        let far = token_expiry_line(
            &[expires("$GH_TOKEN", RENEW_WINDOW_DAYS + 1)],
            "thing-fleet",
        );
        assert_eq!(
            far.level,
            Level::Satisfied,
            "a deadline outside the window must be silent: {far:?}"
        );
        assert!(
            far.fix.is_empty(),
            "a satisfied check must carry no recipe, or the board shows a fix for nothing: {}",
            far.fix
        );
        assert!(
            !far.detail.contains("sbx secret set"),
            "nothing outside the window may print the renewal command: {}",
            far.detail
        );

        // Past the date entirely. Distinguished from "expires today" because they are different
        // sentences and only one of them is still a warning rather than a post-mortem.
        let gone = token_expiry_line(&[expires("$GH_TOKEN", -3)], "thing-fleet");
        assert!(
            gone.is_fault(),
            "an expired credential is a fault: {gone:?}"
        );
        assert!(
            gone.detail.contains("expired 3 day(s) ago"),
            "a credential already past its date must say so, not count down to it: {}",
            gone.detail
        );
    }

    /// **A reading skein could not take is never a pass, and never hides a fault** (SKEIN-928).
    ///
    /// GitHub answers a dead credential with a 401 and no expiry header at all, so "no header" is
    /// what the worst case looks like as well as the best. The three arms are ordered fault →
    /// unknown → pass, and both directions of that order are asserted here.
    ///
    /// Counterfactual: moving the `Unanswered` arm above the deadline arm makes
    /// `a fault outranks an unknown` fail; returning `satisfied` instead of `unknown` for an
    /// unanswered reading makes `an unanswered reading must never read as a pass` fail.
    #[test]
    fn an_unanswered_reading_is_never_a_pass_and_never_outranks_a_fault() {
        let unsure = token_expiry_line(
            &[reading(
                "$GH_TOKEN",
                crate::prq::Life::Unanswered("GitHub said 401: Bad credentials".into()),
            )],
            "thing-fleet",
        );
        assert_eq!(
            unsure.level,
            Level::Unknown,
            "an unanswered reading must never read as a pass: {unsure:?}"
        );
        assert!(
            unsure.detail.contains("Bad credentials"),
            "the reason skein could not tell is the only useful part of an unknown: {}",
            unsure.detail
        );

        let both = token_expiry_line(
            &[
                reading(
                    "the read token in Settings",
                    crate::prq::Life::Unanswered("GitHub did not answer within 20s".into()),
                ),
                expires("$GH_TOKEN", 2),
            ],
            "thing-fleet",
        );
        assert!(
            both.is_fault(),
            "a fault outranks an unknown — a credential skein could not ask about must not hide \
             one it could: {both:?}"
        );

        let endless = token_expiry_line(
            &[reading("$GH_TOKEN", crate::prq::Life::Endless)],
            "thing-fleet",
        );
        assert_eq!(
            endless.level,
            Level::Satisfied,
            "a token GitHub gives no expiry for is a supported state, not a fault: {endless:?}"
        );
        assert!(
            token_expiry_line(&[], "thing-fleet").level == Level::Satisfied,
            "holding no GitHub credential at all is not a fault either"
        );
    }

    /// **Each source gets the recipe that would actually replace it** (SKEIN-928).
    ///
    /// The fleet's own sandbox-scoped secret is renewed with a host command naming the sandbox, and
    /// a token stored in Settings is pasted back into Settings. One recipe for both would send
    /// somebody to the wrong place at the moment they are already blocked.
    ///
    /// Counterfactual: collapsing `renew_recipe` to a single string for every source makes
    /// `must not be told to run an sbx command` fail; dropping `{fleet}` from the environment arm
    /// makes `must name the sandbox` fail — and a recipe with a placeholder in it is the half of
    /// the answer nobody can copy.
    #[test]
    fn the_recipe_names_the_step_that_replaces_this_credential() {
        use crate::prq::GhToken;
        let fleet = renew_recipe(GhToken::Environment, "thing-fleet");
        assert!(
            fleet.contains("sbx secret set github --sandbox thing-fleet"),
            "the fleet secret's recipe must name the sandbox, copyable: {fleet}"
        );
        assert!(
            fleet.contains("Contents") && fleet.contains("Pull requests"),
            "a replacement narrower than what it replaces breaks pushes a week later: {fleet}"
        );
        assert!(
            fleet.contains("sbx rm"),
            "a fleet rebuild drops a sandbox-scoped secret, and that is the half people are bitten \
             by twice: {fleet}"
        );

        for stored in [GhToken::ReadToken, GhToken::WritePat] {
            let recipe = renew_recipe(stored, "thing-fleet");
            assert!(
                recipe.contains("Settings → GitHub & keys"),
                "a token stored in Settings is replaced in Settings: {recipe}"
            );
            assert!(
                !recipe.contains("sbx secret set"),
                "a token stored in Settings must not be told to run an sbx command: {recipe}"
            );
        }
    }

    /// **The blocked-egress message, and the rule that a 401/403 is not a block (SKEIN-548).** The
    /// user-visible half: only a genuine failure to reach GitHub prints the `sbx policy allow
    /// network` hint, and it carries the real fleet name. An auth answer clears the line.
    ///
    /// Counterfactual: if [`github_reach_line`] emitted the hint for `Reachable`, or dropped the
    /// fleet name, or left the fix empty for a block, an assertion here fails. Proven by sabotage —
    /// swapping the two arms made `the fleet name` / `is not a fault` fire.
    #[test]
    fn the_policy_hint_is_only_for_a_real_block() {
        let blocked = github_reach_line(GithubReach::Blocked, "skein-fleet-xyz");
        assert!(blocked.is_fault(), "a blocked GitHub must read as a fault");
        assert!(
            blocked
                .detail
                .contains("blocked by the sandbox's network policy"),
            "the diagnosis lost its wording: {}",
            blocked.detail
        );
        assert!(
            blocked.fix.contains(
                "sbx policy allow network --sandbox skein-fleet-xyz github.com,api.github.com"
            ),
            "the fix must be the exact copyable command with the real fleet name: {}",
            blocked.fix
        );

        let reachable = github_reach_line(GithubReach::Reachable, "skein-fleet-xyz");
        assert!(
            !reachable.is_fault(),
            "a reachable GitHub is not a fault, so nothing here should mention a firewall: {} / {}",
            reachable.detail,
            reachable.fix
        );
        assert!(
            !reachable.detail.contains("blocked") && !reachable.fix.contains("sbx policy"),
            "a 401/403 answer must NOT print the policy hint: {} / {}",
            reachable.detail,
            reachable.fix
        );
    }

    /// **The probe tells a connect failure apart from an HTTP answer, by sabotage of the surface it
    /// runs against.** A listener that answers 401 is [`GithubReach::Reachable`]; a dead port is
    /// [`GithubReach::Blocked`]. The counterfactual is real: if `probe_github_reach_at` treated any
    /// non-200 as blocked, the 401 case would flip; if it treated a refused connection as reachable,
    /// the dead-port case would. Both were watched to fail before this was trusted.
    #[test]
    fn a_401_is_reachable_and_a_dead_port_is_blocked() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        // Skip rather than fail where the harness has no curl — the same rule the rest of the file
        // holds; the probe is a wrapper around it. Through `testutil::skip` and not a bare `return`
        // so a run that asked for no skips refuses instead of passing in silence (SKEIN-790).
        if !crate::github::have_curl() {
            return crate::testutil::skip(
                "no curl, and the reachability probe under test is a wrapper around it",
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
                let _ = sock.flush();
            }
        });
        let reachable = probe_github_reach_at(&format!("http://127.0.0.1:{port}/"));
        let _ = handle.join();
        assert_eq!(
            reachable,
            GithubReach::Reachable,
            "an HTTP 401 is GitHub answering — reachable, not blocked"
        );

        // Port 1 on loopback refuses immediately: a connection that cannot be made at all.
        let blocked = probe_github_reach_at("http://127.0.0.1:1/");
        assert_eq!(
            blocked,
            GithubReach::Blocked,
            "a refused connection is a block, not an answer"
        );
    }

    /// **A proxy that accepts what cannot be valid is the fault; everything else is not
    /// (SKEIN-548).** The user-visible half of the detection the owner asked for: a red line only
    /// when a credential was substituted, and the recipe that clears it names the real sandbox,
    /// because `sbx secret set` without `--sandbox <name>` writes the GLOBAL secret and would widen
    /// the very thing it was run to narrow.
    ///
    /// **The concrete change that makes this fail, named before it was written:** swapping the
    /// `Injected` and `Untouched` arms of [`proxy_injection_line`]. Planted, and
    /// `an accepted garbage credential is the fault` failed. Dropping `{fleet}` from the recipe
    /// fails `the recipe must name the sandbox`; making `Absent` unsatisfied fails
    /// `no proxy is not a fault`.
    #[test]
    fn only_an_accepted_garbage_credential_reads_as_injection() {
        let injected =
            proxy_injection_line(ProxyCredential::Injected { ceiling: 5000 }, "thing-fleet");
        assert!(
            injected.is_fault(),
            "an accepted garbage credential is the fault this check exists for: {}",
            injected.detail
        );
        assert!(
            injected.detail.contains("5000") && injected.detail.contains("60"),
            "the diagnosis shows both ceilings, because the gap between them IS the evidence: {}",
            injected.detail
        );
        assert!(
            injected.detail.contains("answers GitHub as the account"),
            "the diagnosis lost its wording: {}",
            injected.detail
        );
        assert!(
            injected
                .fix
                .contains("sbx secret set github --sandbox thing-fleet"),
            "the recipe must name the sandbox, or it writes the global secret: {}",
            injected.fix
        );

        let untouched = proxy_injection_line(ProxyCredential::Untouched, "thing-fleet");
        assert!(
            !untouched.is_fault(),
            "a refused garbage credential is the proxy behaving: {}",
            untouched.detail
        );
        assert!(
            untouched.fix.is_empty(),
            "nothing to fix means no recipe: {}",
            untouched.fix
        );

        let absent = proxy_injection_line(ProxyCredential::Absent, "thing-fleet");
        assert!(
            !absent.is_fault(),
            "no proxy is not a fault — it is most deployments: {}",
            absent.detail
        );

        let unsure = proxy_injection_line(
            ProxyCredential::Unanswered("the probe was answered 403".to_string()),
            "thing-fleet",
        );
        assert_eq!(
            unsure.level,
            Level::Unknown,
            "a reading that is neither an acceptance nor a refusal must not be a fault, and must \
             not be a pass: {}",
            unsure.detail
        );
        assert!(
            unsure.detail.contains("403"),
            "an unknown says what it saw, or nobody can act on it: {}",
            unsure.detail
        );
    }

    /// **An acceptance is an injection only when somebody was authenticated for it**, against a
    /// real proxy socket. A property of the mechanism rather than of a string: curl is given `-x`
    /// and the listener answers as the proxy would, headers and all.
    ///
    /// The third case is the one worth having. An anonymous `200` — the shape sbx v0.43.0's
    /// credential-stripping produces — must NOT read as an injection, and a status-only probe
    /// cannot tell it from one. That is the false positive SKEIN-913 is about: a check that goes
    /// red for a reason the reader can see is not theirs is a check they learn to read past.
    ///
    /// **The concrete changes that make this fail, named before it was written:** returning
    /// `Untouched` from the authenticated arm of [`probe_proxy_injection_at`] — planted, and
    /// `an authenticated 200 is a substituted credential` failed; and dropping the
    /// `ceiling > ANONYMOUS_CEILING` guard so any `200` is an injection — planted, and
    /// `an anonymous 200 added no credential` failed.
    #[test]
    fn only_an_authenticated_acceptance_through_the_proxy_is_injection() {
        if !crate::github::have_curl() {
            return crate::testutil::skip(
                "no curl, and the injection probe under test is a wrapper around it",
            );
        }
        // Authenticated: GitHub's ceiling for a credential it recognised, far above the anonymous
        // one. SKEIN-927 recorded exactly this on the day injection was live.
        let (port, served) =
            fake_proxy("HTTP/1.1 200 OK\r\nx-ratelimit-limit: 5000\r\nContent-Length: 0\r\n\r\n");
        let injected =
            probe_proxy_injection_at(&format!("http://127.0.0.1:{port}"), PROBE_TARGET_FOR_TESTS);
        let _ = served.join();
        assert_eq!(
            injected,
            ProxyCredential::Injected { ceiling: 5000 },
            "an authenticated 200 is a substituted credential: the one that was sent cannot be \
             valid, so somebody else's was"
        );

        // Anonymous: accepted, and nobody was authenticated for it. A credential was dropped on the
        // way, not added — the opposite of what this check reports.
        let (port, served) = fake_proxy(format!(
            "HTTP/1.1 200 OK\r\nx-ratelimit-limit: {ANONYMOUS_CEILING}\r\nContent-Length: 0\r\n\r\n"
        ));
        let stripped =
            probe_proxy_injection_at(&format!("http://127.0.0.1:{port}"), PROBE_TARGET_FOR_TESTS);
        let _ = served.join();
        assert_eq!(
            stripped,
            ProxyCredential::Untouched,
            "an anonymous 200 added no credential, and must not paint the banner red"
        );

        let (port, served) = fake_proxy("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        let untouched =
            probe_proxy_injection_at(&format!("http://127.0.0.1:{port}"), PROBE_TARGET_FOR_TESTS);
        let _ = served.join();
        assert_eq!(
            untouched,
            ProxyCredential::Untouched,
            "a 401 is the probe's own credential arriving as sent"
        );

        // Port 1 on loopback refuses immediately: no proxy answered at all.
        let blind = probe_proxy_injection_at("http://127.0.0.1:1", PROBE_TARGET_FOR_TESTS);
        assert!(
            matches!(blind, ProxyCredential::Unanswered(_)),
            "a proxy that cannot be reached answers nothing, which is not a pass and not a fault: \
             {blind:?}"
        );
    }

    /// **The probe sends a credential that cannot be anybody's, and sends it to the proxy.**
    ///
    /// This is the security assertion of the pair, and it is about what leaves the machine rather
    /// than about what comes back. It reads the bytes the probe actually put on the socket and
    /// requires that the only `Authorization` on them is [`PROBE_CREDENTIAL`], which is prefixed
    /// `skein-test-` so that it cannot be mistaken for — or used as — a real one.
    ///
    /// **The concrete change that makes this fail, named before it was written:** dropping the
    /// `skein-test-` prefix from [`PROBE_CREDENTIAL`]. Planted, and
    /// `the probe's credential must be unmistakably not a credential` failed. Sending the request
    /// direct instead of through the proxy fails `the probe must go THROUGH the proxy`.
    #[test]
    fn the_probe_puts_nothing_but_a_marked_non_credential_on_the_wire() {
        if !crate::github::have_curl() {
            return crate::testutil::skip("no curl, and this reads what curl put on the socket");
        }
        assert!(
            PROBE_CREDENTIAL.starts_with("skein-test-"),
            "the probe's credential must be unmistakably not a credential: {PROBE_CREDENTIAL}"
        );
        let (port, served) = fake_proxy("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        let _ =
            probe_proxy_injection_at(&format!("http://127.0.0.1:{port}"), PROBE_TARGET_FOR_TESTS);
        let request = served.join().expect("the fake proxy thread");
        // First, because it is the one an empty request answers: a probe that reached the proxy at
        // all is the precondition for anything the bytes say about what it sent.
        assert!(
            request.starts_with(&format!("GET {PROBE_TARGET_FOR_TESTS} ")),
            "the probe must go THROUGH the proxy — an absolute-form request line is what a proxy \
             is asked, and a direct one would answer a different question: {request:?}"
        );
        let authorizations: Vec<&str> = request
            .lines()
            .filter(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            .collect();
        assert_eq!(
            authorizations,
            vec![format!("Authorization: Bearer {PROBE_CREDENTIAL}").as_str()],
            "exactly one Authorization, and it is the marked non-credential: {request:?}"
        );
    }

    /// A target the tests can reach a loopback listener with. `http`, because a proxy is asked for
    /// an absolute-form `GET` rather than a `CONNECT` — which is exactly the request shape under
    /// test — and a host that can never resolve, so a test that lost its `-x` fails instead of
    /// reaching out.
    const PROBE_TARGET_FOR_TESTS: &str = "http://api.github.invalid/rate_limit";

    /// A listener that answers one request as a proxy would, and hands the request back.
    ///
    /// **`accept` has a deadline, and that is not tidiness.** Written with a plain blocking
    /// `accept`, the sabotage this helper exists to catch — taking `-x` off the probe, so it never
    /// reaches the proxy at all — made the test HANG rather than fail, which is the one outcome
    /// `CONTRIBUTING.md`'s third rule says is worse than no test: a run that never finishes reports
    /// nothing, and under `--no-fail-fast` it stops the thirty-six binaries behind it too. So the
    /// wait ends, and the caller gets an empty request to assert about.
    fn fake_proxy(answer: impl Into<String>) -> (u16, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let answer = answer.into();
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
        listener
            .set_nonblocking(true)
            .expect("a loopback listener can be polled");
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut seen = String::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let _ = sock.set_nonblocking(false);
                        let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                        let mut buf = [0u8; 4096];
                        if let Ok(read) = sock.read(&mut buf) {
                            seen = String::from_utf8_lossy(&buf[..read]).to_string();
                        }
                        let _ = sock.write_all(answer.as_bytes());
                        let _ = sock.flush();
                        break;
                    }
                    Err(why) if why.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
            seen
        });
        (port, handle)
    }

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

    /// **A disk that could not be measured blames the measurement, never the settings** (SKEIN-770).
    ///
    /// The sentence this replaced said "no fleet sandbox is configured", and it was reachable and
    /// false about its own cause — the one shape `docs/recovery-survey.md`'s wording columns could
    /// not express. `load_config` repairs a blank `fleet_sandbox` (`src/config.rs:459`) and
    /// SKEIN-756 deleted the arm that read one, so a person sent to Settings by this row found a
    /// name sitting in the field.
    ///
    /// **This is the arm, and that was measured rather than assumed.** `seam::doing_nothing`
    /// substitutes `sh -c :`, so the measuring command succeeds with empty output,
    /// `parse_resources` finds no `mem_total`, and `fleet_resources` answers `None` — which is
    /// `disk_health`'s own arm and not `disk_verdict`'s `disk_total == 0` one.
    /// `a_missing_tool_is_one_fault_and_not_five` had been taking this same route while its comment
    /// named the other one; that comment is corrected too.
    ///
    /// Both halves of SKEIN-702's rule are asserted, because both were absent: the honest cause,
    /// and a next step naming the filesystem actually measured rather than a hard-coded `/boxes`.
    #[test]
    fn a_disk_that_could_not_be_measured_blames_the_sandbox_and_not_the_settings() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut env = crate::testutil::env_pins();
        // Both pins, always: `fleet_root` falls back to `/boxes` — the owner's live fleet — and it
        // is interpolated into the sentence this asserts on.
        env.set("SKEIN_HOME", &home);
        env.set("SKEIN_FLEET_ROOT", home.join("no-fleet-here"));
        // Succeeds having done nothing, which is what drives `fleet_resources` to `None`.
        let _crossing = crate::place::seam::doing_nothing();

        let blind = disk_health();
        assert_eq!(blind.level, Level::Unknown, "{}", blind.detail);
        assert!(
            blind.fix.is_empty(),
            "an unknown offers no fix, so its next step has to be in the detail: {}",
            blind.fix
        );
        // The false cause, in the words it used to reach a person in. Nothing about a *setting*
        // belongs here: there is no path left to this arm that a setting explains.
        assert!(
            !blind.detail.contains("configured"),
            "the banner blames configuration for a sandbox that did not answer: {}",
            blind.detail
        );
        assert!(
            blind.detail.contains("has not answered"),
            "the only remaining cause is unnamed: {}",
            blind.detail
        );
        // Requirement 2 — and it names the filesystem `resource_script` actually asks `df` about,
        // so a reader running it by hand gets an answer about their fleet and not about `/boxes`.
        let root = crate::fleet::fleet_root();
        assert!(
            blind.detail.contains(&format!("df -Pm {root}")),
            "no next step a person can run against the filesystem this measures ({root}): {}",
            blind.detail
        );
        // Requirement 3 — the condition is watched already, and saying so is what stops the reader
        // hunting for something to restart.
        assert!(
            blind.detail.contains("clears itself"),
            "nothing tells the reader skein is still asking: {}",
            blind.detail
        );
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
        let easy = disk_verdict(
            &full(20_000, 20_000),
            "fleet",
            boxes,
            nothing_stranded,
            no_orphans,
            no_stale,
            5,
        );
        assert_eq!(easy.level, Level::Satisfied, "{}", easy.detail);
        assert!(
            easy.detail.contains("33%") && easy.detail.contains("40%"),
            "{}",
            easy.detail
        );

        // The boxes' disk is full: the fix names the biggest, largest first, with figures — "3
        // boxes" is not something anybody can act on at the moment they read it.
        let tight = disk_verdict(
            &full(54_140, 20_000),
            "fleet",
            boxes,
            nothing_stranded,
            no_orphans,
            no_stale,
            5,
        );
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
        let images = disk_verdict(
            &full(20_000, 45_000),
            "fleet",
            boxes,
            nothing_stranded,
            no_orphans,
            no_stale,
            5,
        );
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
        let both = disk_verdict(
            &full(54_140, 45_000),
            "fleet",
            boxes,
            nothing_stranded,
            no_orphans,
            no_stale,
            5,
        );
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
        let one = disk_verdict(
            &shared,
            "fleet",
            boxes,
            nothing_stranded,
            no_orphans,
            no_stale,
            5,
        );
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
            no_orphans,
            no_stale,
            5,
        );
        assert_eq!(blind.level, Level::Unknown);
        assert!(
            blind.fix.is_empty(),
            "an unknown offers no fix: {}",
            blind.fix
        );
    }

    /// **How much must go is measured against the line the verdict itself moves on** (SKEIN-730).
    ///
    /// [`over_by`] is the one piece of arithmetic [`crate::announce`] does not do for itself, and
    /// its whole worth is that it cannot disagree with [`disk_verdict`] about where the line is. A
    /// note asking a box to free 57 MiB from a fleet this same module has just called satisfied is
    /// a warning that teaches its reader to stop reading warnings, which is the failure the
    /// announcement exists to leave behind.
    ///
    /// The sabotage each assertion was named against and proved by, in order:
    ///
    /// * *the figure is what it takes to get back under* — any percentage in [`over_by`] other
    ///   than `DISK_FULL_PCT`: 90 asks for 140 MiB where the line says 3,140.
    /// * *a total that never arrived demands nothing* — drop the `0 =>` arm, and a sandbox that
    ///   answered without disk figures asks its boxes to free every byte they are holding.
    /// * *the two agree on every reading either can see* — divide before multiplying, which moves
    ///   the demand's line 57 MiB below the verdict's. 51,141 of 60,168 MiB is then satisfied with
    ///   56 MiB to free, which is the disagreement the loop walks the boundary to find.
    #[test]
    fn how_much_must_be_freed_is_measured_against_the_line_the_verdict_uses() {
        let fleet = |disk_total, disk_used| crate::fleet::FleetResources {
            disk_total,
            disk_used,
            ..Default::default()
        };

        // 85% of 60,000 MiB is 51,000, so 54,140 is 3,140 MiB over — the figure a note names.
        assert_eq!(over_by(&fleet(60_000, 54_140)), 3_140);
        // Exactly on the line is not over it, and neither is well under.
        assert_eq!(over_by(&fleet(60_000, 51_000)), 0);
        assert_eq!(over_by(&fleet(60_000, 20_000)), 0);
        // No total is no reading, and no reading is no demand — `disk_verdict` says `Unknown` for
        // exactly these figures, and 20,000 MiB is what it would otherwise ask somebody for.
        assert_eq!(over_by(&fleet(0, 20_000)), 0);

        // The property, walked across the boundary rather than asserted beside it: on every
        // reading either half can see, a fault has something to free and a satisfied fleet has
        // nothing. 60,168 MiB is the sandbox's own figure (`src/fleet.rs`'s parser test), chosen
        // because 85% of it is not a whole MiB.
        let nothing_stranded = || -> Result<Vec<crate::fleet::Stray>, String> { Ok(Vec::new()) };
        for used in [20_000_u64, 51_141, 51_142, 51_143, 51_144, 60_168] {
            let r = fleet(60_168, used);
            let verdict = disk_verdict(
                &r,
                "fleet",
                |_: ()| Vec::new(),
                nothing_stranded,
                no_orphans,
                no_stale,
                5,
            );
            assert_eq!(
                verdict.level == Level::Unsatisfied,
                over_by(&r) > 0,
                "at {used} of 60168 MiB the verdict says {:?} and the demand is {} MiB, so one of \
                 them is measuring against a line the other does not use",
                verdict.level,
                over_by(&r)
            );
        }
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

        let v = disk_verdict(&full, "fleet", boxes, stranded, no_orphans, no_stale, 5);
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
        // Each offer ends in a command on a line of its own, and there are three of them now, so
        // the property is per LINE rather than "nothing at all follows the first command": what
        // must never happen is a second fix joined onto a command line, where a reader pasting the
        // line takes it as more arguments. A newline between two offers is what makes three of them
        // safe, so the assertion is made of every command line the report carries.
        for line in v.fix.lines() {
            let cmd = line.trim_start();
            if !cmd.starts_with("rm -rf") && !cmd.starts_with("find ") {
                continue;
            }
            assert!(
                !cmd.contains("; "),
                "another fix was joined onto a command line, so it reads as more arguments to it: \
                 {line}"
            );
        }
        assert!(
            !after.is_empty(),
            "the command has no arguments at all: {}",
            v.fix
        );

        // The sweep refusing to answer is silence. `substrate_strays` errs rather than reporting
        // "nothing is stranded" it cannot stand behind, and that reasoning is about skein — it is
        // not what somebody staring at a full disk came for.
        let refused = disk_verdict(
            &full,
            "fleet",
            boxes,
            || {
                Err(
                    "skein can see no boxes at all, so it is refusing rather than reporting"
                        .to_string(),
                )
            },
            no_orphans,
            no_stale,
            5,
        );
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
        let quiet = disk_verdict(
            &easy,
            "fleet",
            boxes,
            || panic!("the substrate was walked for a fleet that has room left"),
            || panic!("the boxes were walked for orphaned builds on a fleet with room left"),
            || panic!("the boxes were walked for stale builds on a fleet with room left"),
            5,
        );
        assert_eq!(quiet.level, Level::Satisfied, "{}", quiet.detail);
        std::env::remove_var("SKEIN_FLEET_ROOT");
    }

    /// **A full disk names the build output that can be reclaimed, and says nothing when there is
    /// none** (SKEIN-975, SKEIN-974).
    ///
    /// `crate::fleet::orphaned_builds` landed with five tests and no caller, so the 5.88 GiB it
    /// could name stayed invisible to everything the owner actually looks at. This is the reader.
    /// The sweeps are arguments for the reason every other reading here is one: they walk real box
    /// trees, and this suite runs on machines with no fleet.
    ///
    /// **What would make each assertion fail**, watched one at a time against a sabotaged
    /// `disk_verdict`: dropping the orphan offer (5.9 GiB stays invisible, which is the whole of
    /// SKEIN-975); dropping the age offer; keeping the same path in both offers, so a reader adding
    /// the totals up double-counts the largest item in them; letting either offer crowd out the
    /// boxes' own advice; turning an `Err` into a sentence rather than into silence; and reporting
    /// either of them on a fleet that has room left, which is a tree walk on every board tick.
    #[test]
    fn a_full_disk_names_build_output_that_can_be_reclaimed_and_is_silent_when_there_is_none() {
        // `orphaned_build_advice` and `stale_build_advice` print absolute paths, so nothing here
        // may resolve the owner's live fleet even to build a string (SKEIN-626).
        let _g = crate::testutil::env_lock();
        let fleet = crate::testutil::tempdir();
        // `env_pins` rather than a trailing `remove_var`: this test asserts a great many things
        // about a sentence, so the interesting run is the one that panics partway — and a
        // `remove_var` at the end never executes on that path, leaving the next test in this
        // process pinned at a temp directory that is about to be deleted (SKEIN-696/703).
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_FLEET_ROOT", &fleet);
        let full = crate::fleet::FleetResources {
            disk_total: 60_000,
            disk_used: 54_140,
            images_total: 50_000,
            images_used: 20_000,
            ..Default::default()
        };
        let boxes = |_: ()| vec![("web-main".to_string(), 512_u64)];
        let nothing_stranded = || -> Result<Vec<crate::fleet::Stray>, String> { Ok(Vec::new()) };
        let gib = |n: u64| n * 1024 * 1024 * 1024;
        let root = (fleet.as_ref() as &std::path::Path).display().to_string();
        let (dead, live) = (
            format!("{root}/web-main/target-wt1140"),
            format!("{root}/web-main/target-private"),
        );

        // Nothing to reclaim: the fleet is still full, and skein has nothing to add about builds.
        let silent = disk_verdict(
            &full,
            "fleet",
            boxes,
            nothing_stranded,
            no_orphans,
            no_stale,
            5,
        );
        assert_eq!(silent.level, Level::Unsatisfied, "{}", silent.detail);
        assert!(
            !silent.fix.contains("rm -rf") && !silent.fix.contains("find "),
            "skein offered a command over build output on a fleet where both sweeps found none — \
             an offer nobody can act on teaches a reader to stop reading the rest: {}",
            silent.fix
        );

        // Something to reclaim, by both derivations, and the dead directory is in BOTH sweeps —
        // which is what the fleet this was measured on actually looks like: its single largest
        // stale directory was also its single largest orphan.
        let orphans = || {
            Ok(vec![crate::fleet::OrphanedBuild {
                path: dead.clone(),
                bytes: gib(6),
                built_from: format!("{root}/web-main/wt-1140"),
                naming: 9,
                of: 9,
            }])
        };
        let stale = || {
            Ok(vec![
                crate::fleet::StaleBuild {
                    path: dead.clone(),
                    bytes: gib(6),
                    files: 16_416,
                    of: gib(6),
                    oldest_days: 12,
                },
                crate::fleet::StaleBuild {
                    path: live.clone(),
                    bytes: gib(4),
                    files: 13_304,
                    of: gib(4),
                    oldest_days: 12,
                },
            ])
        };
        let v = disk_verdict(&full, "fleet", boxes, nothing_stranded, orphans, stale, 5);

        assert!(
            v.fix.contains(&dead) && v.fix.contains("which is not on disk"),
            "the build output of a worktree that is gone is not named at all, so the largest thing \
             anybody can safely reclaim stays invisible on the page they opened to ask what took \
             the disk: {}",
            v.fix
        );
        assert!(
            v.fix.contains(&live) && v.fix.contains("weaker kind"),
            "the age-based offer is missing, or is stated without the caveat that makes it \
             admissible: {}",
            v.fix
        );
        assert!(
            v.fix.contains("web-main (0.5G)"),
            "the build offers crowded the boxes out of their own advice: {}",
            v.fix
        );
        // The dead directory has the stronger evidence and appears under it ONCE. Offered twice, a
        // reader adding the figures up believes 16 GiB is recoverable where 10 GiB is.
        assert_eq!(
            v.fix.matches(dead.as_str()).count(),
            2,
            "the same path is offered under two reasons and two commands, so the totals a reader \
             adds up are wrong by the size of the biggest item in them (it should appear once in \
             the orphan sentence and once in that sentence's command, and nowhere else): {}",
            v.fix
        );
        assert!(
            !v.fix.contains("find ")
                || v.fix
                    .split_once("find ")
                    .is_some_and(|(_, c)| !c.contains(&dead)),
            "the age offer's command still names the directory the orphan offer already took: {}",
            v.fix
        );
        // Every command has to survive being pasted, and there are three lines that could carry
        // one now rather than the single `rm -rf` this rule was written for.
        for line in v.fix.lines() {
            let cmd = line.trim_start();
            if !cmd.starts_with("rm -rf") && !cmd.starts_with("find ") {
                continue;
            }
            assert!(
                !cmd.contains("; "),
                "another fix was joined onto a command line, so it reads as more arguments to it: \
                 {line}"
            );
        }

        // A sweep that refuses is silence, and it does not take the other readings with it.
        let refused = disk_verdict(
            &full,
            "fleet",
            boxes,
            nothing_stranded,
            || Err("skein can see no boxes at all, so it is refusing".to_string()),
            || Err("a threshold of zero days would qualify everything".to_string()),
            0,
        );
        assert!(
            !refused.fix.contains("refusing") && !refused.fix.contains("zero days"),
            "a refusal was passed on as advice: {}",
            refused.fix
        );
        assert!(
            refused.fix.contains("web-main"),
            "one reading being unanswerable took the other fixes with it: {}",
            refused.fix
        );
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
    ///   `the_proxy_check_is_on_the_banner_and_in_the_diagnostics_pane` below is that half, for the
    ///   page; nothing checks the wording, and nothing can.
    ///
    /// **The concrete changes that make it fail, named before it was written:** putting `..` back
    /// in the pattern fails `the destructuring must not end in ..`; turning `token_expiry,` into
    /// `token_expiry: _` and deleting its array entry fails `every check field is in the list`.
    #[test]
    fn every_health_check_field_is_named_in_the_list() {
        let source = include_str!("health.rs");
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
        // `splitn(3, ", ")` and not `rsplit_once`, because the third column is now the banner
        // disposition and a `NotCounted` reason is prose that may hold a comma of its own. The
        // field binding is the second column either way.
        let entries: Vec<[&str; 3]> = source[open + 1..close]
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('('))
            .map(|line| {
                let inner = line
                    .strip_prefix('(')
                    .and_then(|rest| rest.trim_end_matches(',').strip_suffix(')'))
                    .unwrap_or_else(|| {
                        panic!("this is not a `(key, field, banner)` entry: {line}")
                    });
                let mut columns = inner.splitn(3, ", ");
                let mut next = || {
                    columns
                        .next()
                        .unwrap_or_else(|| panic!("fewer than three columns in: {line}"))
                        .trim()
                };
                [next(), next(), next()]
            })
            .collect();
        // Every entry states its disposition, and it is one of the two the type has. A third
        // variant added without a reader here would otherwise be counted as neither.
        for [key, _, banner] in &entries {
            assert!(
                *banner == "Counted" || banner.starts_with("NotCounted("),
                "`{key}` states its place on the banner as `{banner}`, which this test does not \
                 know how to read — every entry says `Counted` or `NotCounted(why)` (SKEIN-1003)"
            );
        }
        let listed: std::collections::BTreeSet<&str> =
            entries.iter().map(|[_, field, _]| *field).collect();
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

    /// **Every check the report carries is a key the banner can name** (SKEIN-1003).
    ///
    /// The page's `CHECKED` is what a raised banner filters to find something to say. It was
    /// hand-written and held twelve of the fourteen, which costs in one direction only, but that
    /// direction is the bad one: a check that `ok` counts and `CHECKED` does not know about puts a
    /// red row above the app with nothing in it to read.
    /// `the_proxy_check_is_on_the_banner_and_in_the_diagnostics_pane` below asserts that for two
    /// named keys; this is the general form, so the next check is covered before somebody
    /// remembers to add a line here.
    ///
    /// **It compares field names, not `checks_with_banner`'s keys**, because the page switches on
    /// what serde puts on the wire — `token_expiry`, not "token life". `check_fields` reads the
    /// struct, which is the same thing serde serialises.
    ///
    /// **The concrete change that makes it fail, named before it was written:** deleting `"gh"`
    /// from `CHECKED` in `src/web/index.html`, which is the state that page was in when the item
    /// was filed. Planted, and it is the count guard that fires — "found 13 keys in the page's
    /// `CHECKED` and the report has 14 checks", printing the thirteen with no `gh` among them.
    /// Said here because the guard is deliberately in front of the set comparison, so a *missing*
    /// key is reported by the first of the two and a renamed one by the second.
    #[test]
    fn every_check_the_report_carries_is_named_on_the_page() {
        let page = include_str!("web/index.html");
        let start = "const CHECKED = [";
        let from = page
            .find(start)
            .unwrap_or_else(|| panic!("the page has no `{start}` — did it get renamed?"))
            + start.len();
        let to = from
            + page[from..]
                .find(']')
                .expect("`const CHECKED` is never closed");
        let checked: std::collections::BTreeSet<&str> = page[from..to]
            .split(',')
            .map(|entry| entry.trim().trim_matches('"'))
            .filter(|key| !key.is_empty())
            .collect();
        // The extraction read the whole literal before anything is concluded from it, the way the
        // test above proves its own: a split that matched nothing would otherwise report every
        // field as missing from the page, which sends the reader to the wrong file.
        assert_eq!(
            checked.len(),
            HealthReport::CHECK_COUNT,
            "this test found {} keys in the page's `CHECKED` and the report has {} checks. If a \
             check was added, add it to `CHECKED` too; if this found the wrong number of keys in \
             a list that looks right, it is mis-parsing it: {checked:?}",
            checked.len(),
            HealthReport::CHECK_COUNT
        );
        assert_eq!(
            check_fields(include_str!("health.rs")),
            checked,
            "the report's checks and the page's `CHECKED` are not the same set. A name on the \
             left and not the right is the one that costs: if `OnBanner` counts it, `ok: false` \
             puts a red row above the app and `CHECKED.filter` finds nothing to name in it — \
             which is what a fleet that could not reach GitHub at all got, silently, for as long \
             as `gh` was in neither list (SKEIN-1003). A name on the right and not the left is a \
             key the report never sends, so the banner can never print it."
        );
    }

    /// **What turns the banner red is `OnBanner::Counted` and nothing else** (SKEIN-1003).
    ///
    /// The arithmetic on its own, against a report built here with every check satisfied, so that
    /// one check's level is the only thing moving. `health_report`'s own fixture cannot do this
    /// job: it has a fault of its own (no warden answers a test process, by design), so `ok` is
    /// already false there and an assertion that it *becomes* false could not fail.
    ///
    /// The literal below names all 26 fields, which is deliberate and costs nothing to keep: a
    /// field added to `HealthReport` stops this test compiling, in the same breath as the
    /// destructuring in `checks_with_banner`.
    ///
    /// **The concrete changes that make it fail, named before it was written:** marking `gh`
    /// `NotCounted` fails `a counted check decides it`; marking `ai` `Counted` fails `a check the
    /// banner does not count cannot raise it`.
    #[test]
    fn only_a_counted_check_turns_the_banner_red() {
        let satisfied = || HealthCheck::satisfied("nothing wrong with this one");
        let mut report = HealthReport {
            // `true` would be a lie this test then asserts around: `ok` is whatever
            // `first_counted_fault` says, and that is what is being read below.
            ok: false,
            build: BUILD_REVISION,
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
            runtimes: Vec::new(),
            git_credential: String::new(),
        };
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

    /// **The proxy check reaches the banner and the diagnostics pane, read out of the two arrays
    /// the page actually filters on** (SKEIN-548).
    ///
    /// `health_report` counts `proxy_injection` towards `ok`, so a page that does not carry the key
    /// produces the exact failure SKEIN-928's comment warns about: `ok: false` puts a red row above
    /// the app and `CHECKED.filter` finds nothing to name in it. Nothing else in this tree ties a
    /// report field to the page — `the_wire_names_are_the_names_the_page_switches_on` below covers
    /// the three *level* strings and not the keys.
    ///
    /// **It reads the array literals, not the file.** A `page.contains("proxy_injection")` would
    /// pass on the comment three lines above the array, which is SKEIN-987's lesson exactly: an
    /// HTML comment that names a thing is not a use of it. `token_expiry` is asserted alongside so
    /// that an extraction which silently matched nothing fails here rather than passing everything.
    ///
    /// **The concrete change that makes this fail, named before it was written:** deleting
    /// `"proxy_injection"` from the page's `CHECKED`. Planted, and `the banner must count it`
    /// failed; deleting the `CHECKS` row failed `the diagnostics pane must have a row for it`.
    #[test]
    fn the_proxy_check_is_on_the_banner_and_in_the_diagnostics_pane() {
        let page = include_str!("web/index.html");
        let literal = |start: &str| -> String {
            let from = page
                .find(start)
                .unwrap_or_else(|| panic!("the page has no `{start}` — did it get renamed?"))
                + start.len();
            let rest = &page[from..];
            // `];` and not `]`: `CHECKS` is an array OF arrays, and its first element closes with
            // `],` four characters in. The statement's own terminator is the only unambiguous end.
            let to = rest
                .find("];")
                .unwrap_or_else(|| panic!("`{start}` is not closed anywhere after it"));
            rest[..to].to_string()
        };
        // The banner's list, which is what decides whether a red banner has anything to say.
        let checked = literal("const CHECKED = [");
        for key in ["token_expiry", "proxy_injection"] {
            assert!(
                checked.contains(&format!("\"{key}\"")),
                "the banner must count it, or `ok: false` shows a red row with nothing in it — \
                 `{key}` is not in CHECKED: {checked}"
            );
        }
        // The diagnostics pane's rows. `CHECKS` is a list of pairs, so the first entry of the pair
        // is what has to be there — a label alone would render a row nothing fills.
        let checks = literal("const CHECKS = [");
        for key in ["token_expiry", "proxy_injection"] {
            assert!(
                checks.contains(&format!("[\"{key}\",")),
                "the diagnostics pane must have a row for it, since the banner sends a reader \
                 straight there — `{key}` is not in CHECKS: {checks}"
            );
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
        //
        // `None` rather than `crate::warden_client::sighting()`, which would ASK — and with
        // `$SKEIN_WARDEN` deliberately unset here, ask `host.docker.internal:7879`: whatever warden
        // the machine running the suite can reach, which `warden_client` refuses in a test process
        // now (SKEIN-762). It cannot be pinned away either, because an unset `$SKEIN_WARDEN` is the
        // condition `misdirected` fires on and the subject of the assertions below. `None` is
        // exactly what a warden that could not be asked gives back, so this is the same arm — and
        // now the same arm on every machine, rather than one that depends on whether whoever ran
        // the tests happens to have a warden up (SKEIN-690's shape, in the check about wardens).
        let said = warden_health(None);
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
