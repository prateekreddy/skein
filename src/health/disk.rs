//! Whether the fleet's filesystems have room left, and when they do not, how much has to go and
//! what can be cleared to free it.

use super::*;

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
/// printed nothing `fleet::parse_resources` could read (`src/fleet/resources.rs:279`). `Option`
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
/// a thirty-second gate (`src/fleet/resources.rs:219`), so one tick's verdict and its demand are the same
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
        // nothing. 60,168 MiB is the sandbox's own figure (`src/fleet/resources.rs`'s parser
        // test), chosen because 85% of it is not a whole MiB.
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
}
