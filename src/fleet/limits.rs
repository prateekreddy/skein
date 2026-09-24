//! Memory ceilings and guarantees: the plan, each box's and the fleet's cgroup limits, how
//! they are read back, and pressure as a rate.

use super::*;

/// How the sandbox's memory is divided, in MiB.
///
/// Three claims on one VM, and until this existed only the first was written down:
///
/// * **boxes** — the whole workload, and all of it under `/sys/fs/cgroup/skein`: everything the
///   agents and their builds run, *and* the containers they start, which dockerd is pointed at
///   [`CONTAINER_CGROUP`] so that they land inside the same parent rather than beside it;
/// * **plumbing** — the sandbox's own container: its init, the ssh-agent forwarder, dockerd and
///   containerd themselves;
/// * **reserve** — everything outside the workload: the kernel, and the VM-level services that
///   answer the host. Capped by nobody, because it is what everything else is measured against.
///
/// The workload is **one** share rather than a boxes half and a Docker half, and that is the whole
/// of what "bounded together" means here. Splitting it read like two protections and was one and a
/// half: only `skein` can actually be capped ([`fleet_limits`] says why `docker` cannot), so the
/// Docker half was never a ceiling on Docker — it was memory withheld from the boxes on Docker's
/// behalf. A fleet whose boxes wanted 20 GB with no container running was told no, and the third
/// held back for `docker build` protected nothing, because nothing was written on that cgroup.
///
/// So the pool is shared and taken first-come: a box may fill it when Docker is idle, and a build
/// may fill it when the boxes are. Sharing it does not mean giving up the bound. The containers are
/// nested *inside* the cgroup that carries the ceiling, so the two are held to the total between
/// them by the same one limit that holds the boxes — and an overshoot is an OOM in whichever of
/// them caused it, never in the sandbox's own processes. The reserve and plumbing shares, the ones
/// that keep the sandbox answering at all, are untouched by the merge.
///
/// The reserve is the point of the whole exercise. There is no swap in the sandbox, so reaching the
/// VM's memory is not a slowdown, it is the kernel's global OOM killer choosing a victim — and it
/// picks by badness, not by blame, so the process it kills is as likely to be what answers the host
/// as the build that caused it. A sandbox whose plumbing was killed is exactly a sandbox that
/// "stops responding" and only comes back when it is cycled. Keeping the shares' sum below the
/// total converts that into an OOM *inside* the offending cgroup, which kills a build.
pub struct MemoryPlan {
    pub boxes: u64,
    pub plumbing: u64,
    pub reserve: u64,
}

/// The division above, or `None` when [`Config::fleet_memory`] names no number to divide.
pub fn memory_plan() -> Option<MemoryPlan> {
    let total = parse_mib(&load_config().fleet_memory)?;
    // A fixed gigabyte plus 2%, because what this covers is mostly *fixed*: the VM's own services
    // do not grow with the size of the VM, and only the kernel's own structures (page tables,
    // per-cpu areas, slab) scale at all. A flat percentage therefore reserves far too much of a big
    // fleet and, at 10%, was 4.6× what a live 26 GiB sandbox actually had outside both cgroups —
    // measured at 574 MiB, of which 191 MiB was unreclaimable kernel memory.
    //
    // It is not tighter than that because the failure it prevents is not graceful. With no swap,
    // overshooting is an instant kill rather than a slowdown, and the victim is chosen across the
    // whole VM — so the cost of being wrong is a dead sandbox, not a slow one. Never more than half
    // either, so a tiny configured total still leaves something to work in.
    let reserve = (1024 + total / 50).min(total / 2);
    // Small and flat: the sandbox's own container holds 70 MiB of anonymous memory on a live fleet.
    // The ~1.7 GiB beside it is page cache and dentry slab, which reclaims under pressure rather
    // than needing to be owned. This is headroom for dockerd and containerd growing with the number
    // of containers, not a share of the workload.
    let plumbing = 512.min(total / 8);
    // Everything left over is the workload's, in one share. A third of it used to be set aside for
    // Docker; see [`MemoryPlan`] for why holding it back protected nothing and cost the boxes a
    // third of the fleet whenever no container was running.
    let boxes = total.saturating_sub(reserve + plumbing);
    Some(MemoryPlan {
        boxes,
        plumbing,
        reserve,
    })
}

/// The per-box cgroup limits, as the `key=value,…` spec `box-session.sh` applies.
///
/// Memory only. **CPU is deliberately not capped**: `cpu.weight` is already equal for every box, so
/// they fair-share under contention and a lone box still gets every core — and capping it would
/// leave cores idle while a box waits, which is the exact waste the shared sandbox exists to end.
/// Memory is different because it is not reclaimable on demand: two boxes wanting 20 GB do not each
/// get 13 slowly, they hit the wall and the kernel starts killing things.
///
/// `max` is what stops one box taking the rest of the boxes down with it. `high` sits below it so
/// the kernel throttles and reclaims first — a box that briefly overshoots gets slower rather than
/// losing its turn.
///
/// Defaults are 70% and 55% **of the boxes' share** ([`memory_plan`]), not of the whole VM. They
/// were once fractions of the fleet total, which read like a protection and was not one: 70% of the
/// VM each, with nothing capping the sum, meant any two boxes could exhaust it between them. What
/// bounds the fleet is the ceiling on their shared parent; this bounds one box against the others.
///
/// `pids.max` is the fork-bomb guard; a runaway spawn loop in one box would otherwise exhaust the
/// VM's pid space and no box could start a process.
pub fn box_limits() -> String {
    let config = load_config();
    let share = memory_plan().map(|plan| plan.boxes);
    let pick = |explicit: &str, fraction: u64| -> Option<String> {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Some(explicit.to_string());
        }
        share.map(|boxes| format!("{}M", (boxes * fraction / 100).max(512)))
    };
    let mut parts = Vec::new();
    if let Some(max) = pick(&config.box_memory_max, 70) {
        parts.push(format!("max={max}"));
    }
    if let Some(high) = pick(&config.box_memory_high, 55) {
        parts.push(format!("high={high}"));
    }
    parts.push("pids=8192".to_string());
    parts.join(",")
}

/// The ceilings on the two cgroups that hold everything a box can cause, as the `<cgroup>=max/high`
/// spec `box-session.sh` applies. Empty when no fleet total is configured to divide.
///
/// `skein` is the parent of every box's cgroup, so it is the only place the boxes' *sum* can be
/// bounded — a per-box ceiling never could be. It is the one ceiling here, and it is a number.
///
/// `docker` is named too, but only to be handed `max/max` — an explicit *absence* of a ceiling.
/// Earlier versions capped it, on the reasoning that a box's `docker build` runs there and no
/// per-box limit reaches it. That reasoning was right about the hole and wrong about the patch,
/// because of what else lives in that cgroup: the sandbox's own container is a child of it, so
/// `/sys/fs/cgroup/docker` holds init, `socat`, dockerd and containerd — the machinery that answers
/// `sbx exec`. Adding the plumbing share to its ceiling was an attempt to leave that machinery
/// room, and it does not work, because the failure is not about the size of the number.
///
/// Measured on this fleet while it was wedged, with `memory.high` at 7.57 GiB and the cgroup at
/// 7.73 GiB of *anonymous* memory behind 73 MiB of page cache: `pgscan` 43,232 MiB against
/// `pgsteal` 45 MiB. The kernel scanned 43 GB to recover 45 MB — 1,695 throttle events a second,
/// ten of eleven cores, indefinitely. `memory.high` throttles by stalling the allocator until
/// reclaim catches up, which is humane when the overshoot is brief and there is cache to give back.
/// A linker holding 3.4 GB for ten minutes with no swap satisfies neither: there is nothing to
/// reclaim, so the stall never ends. And because init and `socat` share the cgroup, the stall lands
/// on the sandbox's own service path — new `sbx exec` calls hang while established streams, already
/// faulted in, keep flowing. The VM had 16 GB free throughout.
///
/// `memory.max` is no better placed. It kills rather than stalls, and the OOM it would trigger
/// picks its victim from a cgroup containing pid 1 — trading a stuck build for a dead sandbox.
///
/// So the containers are moved instead of the ceiling. [`install_docker_config`] points dockerd at
/// [`CONTAINER_CGROUP`] — `skein/containers`, a child of the boxes' own parent — and what stays
/// behind in `/docker` is the sandbox itself, which nothing caps and nothing should.
///
/// **A ceiling on `skein/containers` is not the reservation this argues against**, and the
/// difference is worth being exact about because the two look alike written down. A reservation is
/// memory *withheld from the boxes* so that Docker can have it — it idles real memory every hour no
/// container runs, which is why a third set aside for Docker was removed. A ceiling withholds
/// nothing: when no container is running the boxes have the whole share, and when one is running it
/// bounds what that one can take. What it buys is where an overshoot lands. Under one shared
/// ceiling a runaway container stalls every box — measured, `high 5551` on this fleet — and at the
/// hard limit the kill is chosen by badness across the whole workload, as readily a box's agent as
/// the container that caused it. Under its own, the container throttles itself and dies in its own
/// cgroup.
///
/// That is what makes `skein` the one ceiling *and* a real one. It is sized to the whole workload,
/// and once dockerd has been pointed inside it, the whole workload is what it actually holds:
/// boxes and containers under one limit, taken first-come, with an overshoot killed in whichever
/// of them caused it and never in the sandbox's own processes. [`MemoryPlan`] keeps no third back
/// for Docker, because a reservation is the opposite of a shared pool — it was memory withheld
/// from the boxes on behalf of a cgroup nothing was written on, buying no protection and idling
/// real memory every hour no container ran.
///
/// Until a fleet has cycled, its containers are still in `/docker` and outside this ceiling: the
/// setting only decides where the *next* dockerd puts them, and restarting dockerd to hurry it
/// would stop every running container. Uncapped for one more boot is the cheaper wrong.
///
/// Applied on every box start rather than once, because dockerd recreates `/sys/fs/cgroup/docker`
/// from scratch when the sandbox cycles, taking any limit written on it with it. That is also why
/// `max/max` is written rather than simply omitted: a fleet an older skein already capped keeps
/// that cap until something writes over it.
pub fn fleet_limits() -> String {
    let Some(plan) = memory_plan() else {
        return String::new();
    };
    // `high` below `max` for the same reason it is per box: past it the kernel reclaims and
    // throttles, so a fleet that briefly overshoots gets slower instead of losing a box.
    let ceiling = |mib: u64| format!("{mib}M/{}M", (mib * 9 / 10).max(512));
    // `total` is what these are a share OF, and it travels with them because only the sandbox can
    // check it. sbx fixes a sandbox's memory when it is created, so editing Fleet memory without
    // rebuilding leaves this describing a VM that does not exist — and a ceiling worked out for a
    // machine twice the real size is not a ceiling. The launcher scales by what it actually finds.
    // `skein/containers` is bounded as one more box-sized claimant, at the same fractions a box
    // gets. **This is a ceiling, not the reservation the plan argues against** — see [`MemoryPlan`]:
    // a reservation withholds memory from the boxes whether or not a container is running, while a
    // ceiling withholds nothing when containers are idle and bounds them when they are not. The
    // measurement that made the case: `skein`'s `memory.events` on the live fleet read `high 5551`,
    // which is one container's overshoot stalling every box, five and a half thousand times.
    //
    // The name carries the slash because the cgroup is nested; the launcher builds the path from it
    // and splits the *value* on `/`, so the two never meet.
    let containers = memory_plan()
        .map(|p| ceiling(p.boxes * 70 / 100))
        .unwrap_or_else(|| "max/max".into());
    format!(
        "total={}M,skein={},skein/containers={containers},docker=max/max",
        plan.boxes + plan.plumbing + plan.reserve,
        ceiling(plan.boxes),
    )
}

/// The cgroups worth reading back from the kernel, and what each one being uncapped MEANS.
///
/// Read back rather than reported from config, because a ceiling skein computed and failed to write
/// looks identical from the host until the sandbox stops answering.
///
/// The list carries `capped_is_the_goal` because it is not the same answer for all three, and
/// assuming it was is how `skein doctor` came to print a red ✗ for something [`fleet_limits`] writes
/// on purpose: `docker=max/max`, uncapped deliberately and for forty lines of measured reasons. That
/// ✗ was the only one in the fleet section and the only one anywhere with no fix under it — because
/// there is no fix, because nothing is wrong. A warning that fires on the ordinary case is one
/// nobody reads, and this one said the fleet was unsafe to depend on.
pub const CEILINGS: &[(&str, &str, bool)] = &[
    ("skein", "all boxes together", true),
    (
        "skein/containers",
        "the containers boxes start, inside the ceiling above",
        true,
    ),
    ("docker", "the sandbox's own init, socat and dockerd", false),
];

/// What one cgroup's `memory.max` means, in the words somebody reading `doctor` needs.
///
/// A function rather than a `match` inside the printer so it can be driven by a test: the wrong
/// version of this shipped and was read by a person deciding whether to trust their fleet.
pub enum Ceiling {
    /// As it should be. The string says what it is bounded to, or why being unbounded is right.
    Good(String),
    /// Wrong, and worth a red mark.
    Bad(String),
    /// Neither — there is no such cgroup to have an opinion about.
    Absent(String),
}

pub fn ceiling_reading(what: &str, capped_is_the_goal: bool, live: &str) -> Ceiling {
    match live.trim() {
        "" => Ceiling::Absent(format!("no such cgroup ({what})")),
        "max" if capped_is_the_goal => Ceiling::Bad(format!(
            "UNBOUNDED — {what} can reach the VM's memory, and with no swap that ends the sandbox \
             rather than the build"
        )),
        // The designed state. Named as such rather than merely tolerated, because "uncapped" on its
        // own is the word somebody is scanning for when they are looking for what is wrong.
        "max" => Ceiling::Good(format!("uncapped by design ({what})")),
        bytes if !capped_is_the_goal => Ceiling::Bad(format!(
            "capped at {} — {what} must not be, and an OOM here picks its victim from a cgroup \
             holding pid 1. An older skein wrote this; starting any box rewrites it",
            gib(bytes)
        )),
        bytes => Ceiling::Good(format!("capped at {} ({what})", gib(bytes))),
    }
}

/// `memory.max`'s bytes as a person reads them, or the raw string when it is not a number.
fn gib(bytes: &str) -> String {
    bytes
        .parse::<u64>()
        .map(|b| format!("{:.1}G", b as f64 / 1024.0 / 1024.0 / 1024.0))
        .unwrap_or_else(|_| bytes.to_string())
}

/// How hard the fleet is being squeezed, as a rate rather than a total.
///
/// **A counter is not a number a person can act on.** Everything the kernel keeps here is monotonic
/// since boot: `memory.events`' `high` on this fleet read 5,551 one hour and 98,305 the next, and
/// shown raw it says the same enormous thing for ever while telling nobody whether it is happening
/// *now*. So two readings are kept and what is reported is the difference over the time between
/// them.
///
/// **The baseline is in memory, and lost on restart.** A file would survive it, and would then have
/// to distinguish a counter that went backwards because the sandbox rebooted from one that went
/// backwards because the file is stale — for a number that re-establishes itself within a minute of
/// asking twice. The cost is stated rather than paid: after a skein restart the first answer has no
/// rate, and says so.
///
/// It costs no subprocess: the agent reads `/sys/fs/cgroup` and `/proc/vmstat`, which are files, and
/// this is one HTTP call to it. That is what keeps it off the board's tick — see
/// `tests/board_cost.rs`, which measures the forks a tick makes.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Pressure {
    /// Times the whole workload was throttled at its ceiling, per minute, since the last reading.
    pub throttled_per_min: f64,
    /// The same for containers alone — which is a runaway container rather than a busy fleet.
    pub containers_throttled_per_min: f64,
    /// Anything the kernel killed for memory, anywhere in the VM, since the last reading. Not a
    /// rate: one is already too many, and the number is what says how bad it got.
    pub killed: u64,
    /// How many times the watchdog has had to restart the Docker daemon.
    pub docker_restarts: u64,
    /// `false` on the first answer after a restart, when there is nothing to have changed since.
    pub rated: bool,
}

/// The previous reading, so a rate can be worked out from the next one.
static LAST_PRESSURE: std::sync::Mutex<Option<(std::time::Instant, serde_json::Value)>> =
    std::sync::Mutex::new(None);

/// How hard the fleet is being squeezed.
///
/// **Always an answer now, which is why there is no `Option`.** It used to be one HTTP call to the
/// in-sandbox agent, and `None` meant "no agent answered". The counters are files under
/// `/sys/fs/cgroup` and `/proc`, skein is in the sandbox that owns them, and
/// [`crate::dockerd::counters`] reads them directly (SKEIN-521) — so "could not be asked" has
/// stopped being a state. A cgroup that is not there contributes nothing rather than failing the
/// reading, which is the same tolerance the agent had.
pub fn pressure() -> Pressure {
    let now = crate::dockerd::reading();
    let mut held = LAST_PRESSURE.lock().unwrap_or_else(|e| e.into_inner());
    let out = rate_between(
        held.as_ref()
            .map(|(when, before)| (when.elapsed(), before.clone())),
        &now,
    );
    *held = Some((std::time::Instant::now(), now));
    out
}

/// How much a since-boot counter moved between two readings.
///
/// **A counter that went backwards is a sandbox that rebooted**, not a negative rate — and what it
/// reads now is, exactly, everything that has happened since skein last looked. Subtracting anyway
/// would report zero for a fleet that had just been killed and restarted, which is the one moment
/// the number matters most.
fn grew_by(then: u64, read: u64) -> u64 {
    match read < then {
        true => read,
        false => read - then,
    }
}

/// The arithmetic, with the clock and the agent taken out of it.
///
/// Its own function because the two cases that are not arithmetic — a first reading, and a counter
/// that went backwards — cannot be reached from a test that has to arrange a real sandbox reboot.
fn rate_between(
    before: Option<(Duration, serde_json::Value)>,
    now: &serde_json::Value,
) -> Pressure {
    let restarts = now["docker"]["restarts"].as_u64().unwrap_or(0);
    match before {
        None => Pressure {
            docker_restarts: restarts,
            ..Default::default()
        },
        Some((elapsed, before)) => {
            let minutes = (elapsed.as_secs_f64() / 60.0).max(1.0 / 60.0);
            let since = |what: &str, field: &str| -> u64 {
                let then = before["pressure"][what][field].as_u64().unwrap_or(0);
                let read = now["pressure"][what][field].as_u64().unwrap_or(0);
                grew_by(then, read)
            };
            Pressure {
                throttled_per_min: since("skein", "high") as f64 / minutes,
                containers_throttled_per_min: since("skein/containers", "high") as f64 / minutes,
                // **One counter, not four added together** (FLEET-9). `memory.events` in cgroup v2
                // is hierarchical — every field counts the subtree — so a container's OOM lands in
                // `skein/containers`, again in `skein`, and again in the VM-wide `/proc/vmstat`
                // that the agent also reads. Summing them reported one kill as three, and the
                // number is what the board shows a person deciding whether to raise a ceiling.
                //
                // `vmstat_oom_kill` is the one that needs no assumption about which cgroup a
                // process was in when the kernel took it: it is "anything the kernel killed in
                // this VM", which is exactly what `killed` claims to be. The per-cgroup split is
                // recoverable from the agent's own payload when somebody wants it — reading
                // `memory.events.local` there is the other half of this fix and is the agent's.
                killed: grew_by(
                    before["pressure"]["vmstat_oom_kill"].as_u64().unwrap_or(0),
                    now["pressure"]["vmstat_oom_kill"].as_u64().unwrap_or(0),
                ),
                docker_restarts: restarts,
                rated: true,
            }
        }
    }
}

/// What the sandbox's own plumbing is **guaranteed**, as the `key=value,…` spec the launcher applies
/// to `memory.min`.
///
/// **Uncapped and unprotected are different things, and only the first was a decision.** `/docker`
/// is deliberately without a ceiling — see [`MemoryPlan`], "what stays behind in `/docker` is the
/// sandbox itself, which nothing caps and nothing should". Nothing followed from that about
/// *protection*, so the kernel reclaims dockerd's working set like anywhere else, and when the VM
/// runs short — the boxes' ceiling plus an uncapped `/docker` is about the whole of a sandbox with
/// no swap — the global killer picks by badness, where a daemon holding many containers scores well.
///
/// `memory.min` is a hard guarantee: memory under it is never reclaimed, and a process in that
/// cgroup is not what the kernel reaches for first. **Half the plumbing share**, and the halving is
/// the point: `memory.min` is taken from everybody else, so a promise larger than the budget it
/// comes from turns this fix into the next problem. Measured on a live fleet, dockerd and containerd
/// hold 319 MiB of *anonymous* memory against a 512 MiB plumbing share — the rest is page cache and
/// slab, which is exactly what should still be reclaimable.
///
/// Its own variable rather than a third field on `docker=max/max`: that spec is `max/high`, split on
/// one `/`, and a launcher older than this skein reads whatever it is given. A ceiling it cannot
/// parse it skips loudly; a *grammar* it cannot parse it would misread as a ceiling. A new name is
/// invisible to an old launcher, which is the failure mode worth having.
pub fn fleet_guarantees() -> String {
    let Some(plan) = memory_plan() else {
        return String::new();
    };
    let mut pairs = Vec::new();
    // Nothing at all rather than a token guarantee: below this the promise is not worth the
    // arithmetic, and `memory.min` on a cgroup that cannot hold its own working set inside it is a
    // number that reads as protection and is not one.
    let min = plan.plumbing / 2;
    if min >= 128 {
        pairs.push(format!("docker={min}M"));
    }
    // The boxes' floors, as ONE budget the launcher divides between the box cgroups it finds.
    // Under `boxes`, which names no cgroup: a launcher older than this looks for
    // `/sys/fs/cgroup/boxes`, finds no directory and skips the pair without a word, which is the
    // failure mode the separate variable was chosen for.
    let floors = box_floor_budget();
    if floors > 0 {
        pairs.push(format!("boxes={floors}M"));
    }
    pairs.join(",")
}

/// How much memory every box's floor adds up to, in MiB, before the launcher divides it between
/// the boxes it finds. Zero when there is no plan, or no room for a floor.
///
/// **What a floor is for.** When one box bursts, the kernel reclaims from whatever sits under the
/// ceiling that box hit. If that is its own `memory.max`, it reclaims only from itself. If it is
/// the shared ceiling on `skein`, it reclaims from every box, and the quiet one somebody is
/// working in loses its working set to another box's build. `memory.min` on each box's cgroup is
/// what that box keeps through it. Reclaim that starts at `skein` measures each child against the
/// child's own `memory.min`, so `skein` needs no floor of its own for this, and has none: see
/// `a_guarantee_is_written_to_memory_min_and_scaled_like_a_ceiling`.
///
/// **Why the budget is the gap between two ceilings.** The shared ceiling throttles at `high`, 90%
/// of the boxes' share ([`fleet_limits`]). One box may use up to its own `max`, 70% of it by
/// default ([`box_limits`]). A box at its own `max` plus every floor still fits under the shared
/// `high` when the floors add up to no more than the difference:
///
/// ```text
///   floors ≤ skein high − one box's max = 90% − 70% = 20% of the boxes' share
/// ```
///
/// So the box that is bursting reaches its own ceiling, and reclaims from itself, before the
/// fleet's line is crossed. A bigger budget would do worse than nothing: with everything under
/// `skein` protected, a burst has nothing left to reclaim and meets the hard limit, where the
/// kernel picks a victim from the whole workload. The owner's answer was to throttle and never
/// kill (box-plugin.md, answer 3), and this floor kills nothing.
///
/// An explicit `box_memory_max` is taken as it is. If it reaches the shared `high` there is no gap,
/// and no floor.
pub fn box_floor_budget() -> u64 {
    let Some(plan) = memory_plan() else {
        return 0;
    };
    // Both ceilings from the arithmetic that writes them: `fleet_limits`' `ceiling` and
    // `box_limits`' `pick`.
    let skein_high = (plan.boxes * 9 / 10).max(512);
    let one_box_max = match load_config().box_memory_max.trim() {
        "" => (plan.boxes * 70 / 100).max(512),
        explicit => match parse_mib(explicit) {
            Some(mib) => mib,
            // A size this skein cannot read is not one it can promise underneath.
            None => return 0,
        },
    };
    skein_high.saturating_sub(one_box_max)
}

/// Why a box has no memory ceiling, or `None` when it has one.
///
/// Read from the file `box-session.sh` leaves behind rather than from its stderr: skein keeps a
/// command's stdout and discards stderr on success, so a warning about a box that started *fine*
/// would be dropped exactly when nothing looked wrong. The fact outlives the launch that produced
/// it, which is what lets anything later — a doctor check, a row on the board — still ask.
pub fn uncapped_reason(name: &str) -> Option<String> {
    let sandbox = fleet_sandbox();
    let path = format!("{}/limits.state", box_root(name));
    let state = own_sandbox(&sandbox)
        .exec(&format!("cat {}", sh_quote(&path)), Duration::from_secs(15))
        .ok()?;
    let mut parts = state.split_whitespace();
    match parts.next()? {
        "uncapped" => Some(parts.next().unwrap_or("reason not recorded").to_string()),
        _ => None,
    }
}

/// Apply the current per-box ceilings to every box that is already running.
///
/// Unlike the fleet's own memory, a cgroup limit is **live**: writing `memory.max` changes the cap
/// on a running box immediately, with no restart and nothing to save or restore. So a tighter or
/// looser per-box ceiling is a setting you can simply change, and it would be wrong to make the user
/// rebuild the fleet for it — that is the expensive path, and this is not.
///
/// Returns the boxes whose limits could not be written. Best-effort per box on purpose: one box
/// missing its cgroup (started before delegation existed, say) must not stop the others being
/// corrected.
pub fn apply_box_limits() -> Result<Vec<String>, String> {
    let sandbox = fleet_sandbox();
    let limits = box_limits();
    let fleet = own_sandbox(&sandbox);
    let mut failed = Vec::new();
    // The shared ceilings first, and reported under a name no box answers to. They are what keeps
    // the sandbox itself alive (see `fleet_limits`), so a run that fixed every box and silently
    // left these unwritten would have skipped the important half.
    //
    // Handed back to the launcher rather than written from here, because the half that matters can
    // only be done inside: these numbers are a share of the *configured* fleet size, and the
    // sandbox is the only thing that knows what it really got. Reinstalled first, so a sandbox
    // built before `--ceilings` existed gets the copy that has it.
    install_launcher(&sandbox)?;
    let ceilings = format!(
        "SKEIN_FLEET_LIMITS={} SKEIN_FLEET_GUARANTEES={} {} --ceilings",
        sh_quote(&fleet_limits()),
        sh_quote(&fleet_guarantees()),
        sh_quote(&box_session_path())
    );
    if fleet.exec(&ceilings, Duration::from_secs(30)).is_err() {
        failed.push("the fleet's shared ceilings".to_string());
    }
    for (name, _) in placed_boxes(&sandbox) {
        let mut writes = Vec::new();
        for kv in limits.split(',') {
            let Some((key, value)) = kv.split_once('=') else {
                continue;
            };
            let file = match key {
                "max" => "memory.max",
                "high" => "memory.high",
                "pids" => "pids.max",
                _ => continue,
            };
            // Both halves quoted. `value` always was; `name` was not, and this is the one place in
            // the crate where an unquoted box name reached a `sudo` pipeline — so it was the most
            // expensive of the nine sites `valid_name`'s allow-list now covers. Quoted here as well,
            // because the guard and the escaping must be able to fail independently.
            writes.push(format!(
                "printf '%s\\n' {} | sudo tee {} >/dev/null",
                sh_quote(value),
                sh_quote(&format!("{}/{file}", box_cgroup(&name))),
            ));
        }
        // `test -d` first, so a box with no cgroup is reported rather than counted as adjusted.
        let script = format!(
            "test -d {} || exit 1; {}",
            sh_quote(&box_cgroup(&name)),
            writes.join(" && ")
        );
        if fleet.exec(&script, Duration::from_secs(30)).is_err() {
            failed.push(name);
        }
    }
    Ok(failed)
}

/// The cgroup one box's processes live in.
///
/// `box-session.sh` puts the *session shell* in it before exec'ing bwrap, so the tmux server, the
/// agent, and every compiler they fork start there and stay there — a process's children begin in
/// its cgroup and cannot leave by forking.
pub fn box_cgroup(name: &str) -> String {
    format!("/sys/fs/cgroup/skein/{name}")
}

/// Shell that ends every process in a box, reparented or not.
///
/// **`tmux kill-server` is not "stop the box".** It reaches the processes in the server's panes and
/// nothing else, so anything that double-forked away — an agent restarted under `setsid`, a daemon a
/// build left behind — survives a stop and goes on holding the box's memory and its cgroup. Seen on
/// a live fleet: a box the operator had closed still had `claude --name <box> --continue` running,
/// **with PPID 1**, which is the whole diagnosis. It had been reparented out of the tree the kill
/// walked.
///
/// Cgroup membership is exactly the property `kill-server` lacks, which is why the handle was
/// already there: `cgroup.kill` (v2) SIGKILLs every member, and membership is not something a
/// process can shed. It is write-only and root-owned, which is fine here — this runs in the sandbox,
/// where `sudo` works, the same place and for the same reason the ceiling is applied.
///
/// **Best-effort, and quiet about it.** A box that never got a cgroup — delegation missing, which
/// the launcher records as `uncapped no-cgroup-delegation` — must still stop exactly as it did
/// before, and the `kill-server` before this is that path. Failing loudly here would turn a box that
/// stops into a box that reports an error while stopping.
pub fn box_cgroup_kill(name: &str) -> String {
    format!(
        "sudo sh -c 'echo 1 > \"$1\"' _ {} 2>/dev/null || true",
        sh_quote(&format!("{}/cgroup.kill", box_cgroup(name)))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// The ceiling exists so ONE runaway box cannot take the fleet down with it. That means max sits
    /// below the fleet total (or it protects nothing) and high sits below max (or the kernel kills
    /// the box instead of throttling it, turning a slow build into a lost turn).
    #[test]
    fn a_boxs_ceiling_protects_the_fleet_and_throttles_before_it_kills() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        save_config(&Config {
            fleet_memory: "26g".into(),
            ..Config::default()
        })
        .unwrap();
        let spec = box_limits();
        let get = |k: &str| -> u64 {
            let raw = spec
                .split(',')
                .find_map(|p| p.strip_prefix(&format!("{k}=")))
                .unwrap_or_else(|| panic!("{k} missing from {spec}"));
            parse_mib(raw).unwrap()
        };
        let (max, high) = (get("max"), get("high"));
        let share = memory_plan().unwrap().boxes;
        assert!(
            max < share,
            "a cap at or above what all the boxes share protects nothing: {spec}"
        );
        assert!(high < max, "high must throttle before max kills: {spec}");
        assert!(
            max > share / 2,
            "a cap this tight makes a normal build fail; the point is one box CAN be big: {spec}"
        );
        assert!(
            spec.contains("pids="),
            "a fork bomb in one box starves every other: {spec}"
        );
        // CPU is deliberately absent — see box_limits.
        assert!(
            !spec.contains("cpu"),
            "capping CPU idles cores while a box waits, which is the waste this design ends: {spec}"
        );

        // An explicit value always wins over the derivation.
        save_config(&Config {
            fleet_memory: "26g".into(),
            box_memory_max: "4g".into(),
            ..Config::default()
        })
        .unwrap();
        assert!(box_limits().contains("max=4g"), "{}", box_limits());
        std::env::remove_var("SKEIN_HOME");
    }

    /// The invariant a per-box ceiling never expressed and could not: what everything adds up to.
    ///
    /// Boxes were capped at 70% of the VM *each* with nothing capping their sum, and the sandbox's
    /// Docker daemon — where a box's `docker build` actually runs — was capped at nothing at all.
    /// Two busy boxes, or one docker-heavy one, could reach the VM's memory; with no swap that is
    /// the global OOM killer choosing a victim by badness rather than by blame, and what it kills
    /// is as readily the thing that answers the host as the build that caused it. That is the
    /// sandbox "not responding" until someone cycles it.
    #[test]
    fn every_claim_on_the_sandbox_together_leaves_it_room_to_answer() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        for total in ["4g", "8g", "26g", "64g"] {
            save_config(&Config {
                fleet_memory: total.into(),
                ..Config::default()
            })
            .unwrap();
            let total_mib = parse_mib(total).unwrap();
            let plan = memory_plan().unwrap();
            assert_eq!(
                plan.boxes + plan.plumbing + plan.reserve,
                total_mib,
                "the shares must account for the whole VM at {total}"
            );
            // Measured at 574 MiB outside both cgroups on a live 26 GiB fleet. A gigabyte is the
            // floor because what it covers barely scales with the size of the VM.
            assert!(
                plan.reserve >= (1024).min(total_mib / 2),
                "the VM's own services are what stop answering first at {total}"
            );

            // The spec the launcher applies has to name both cgroups, but it caps only one of them.
            let spec = fleet_limits();
            let pair = |cgroup: &str| -> &str {
                spec.split(',')
                    .find_map(|p| p.strip_prefix(&format!("{cgroup}=")))
                    .unwrap_or_else(|| panic!("{cgroup} missing from {spec}"))
            };
            let (boxes_max, boxes_high) = pair("skein").split_once('/').expect("max/high");
            let (boxes_max, boxes_high) = (
                parse_mib(boxes_max).unwrap(),
                parse_mib(boxes_high).unwrap(),
            );
            assert_eq!(boxes_max, plan.boxes);
            assert!(
                boxes_high < boxes_max,
                "throttle before killing, the same way a box does: {spec}"
            );
            // One ceiling over the whole workload — the boxes and the containers they start share a
            // pool now rather than each being handed a slice. What it must still leave untouched is
            // the plumbing and the reserve, because those are what answers the host: the merge is
            // between the two workload shares, never into the sandbox's own.
            assert!(
                boxes_max + plan.plumbing < total_mib,
                "the workload's ceiling has to leave the sandbox its own share: {spec}"
            );
            assert!(
                total_mib - boxes_max >= plan.reserve,
                "the reserve survives the merge, or the VM has nothing to answer with: {spec}"
            );
            // Docker is named in order to be left uncapped, and `max` is the only value that says
            // so — a number here throttles the sandbox's own init and socat, which share that
            // cgroup, and hangs `sbx exec` while established streams keep flowing. Withholding the
            // ceiling has to be written rather than omitted, or a fleet an older skein capped keeps
            // that cap for as long as it lives.
            assert_eq!(
                pair("docker"),
                "max/max",
                "a bounded docker cgroup throttles the machinery that answers sbx: {spec}"
            );
            // And what they are a share OF, so the sandbox can check the share against itself.
            assert!(
                spec.starts_with(&format!("total={total_mib}M,")),
                "without the total, a sandbox smaller than the config says gets ceilings that \
                 cannot bound it: {spec}"
            );
        }
        std::env::remove_var("SKEIN_HOME");
    }

    /// A counter is not a rate, and the difference is what a person can act on.
    ///
    /// Everything the kernel keeps here is monotonic since boot: this fleet's `high` read 5,551 one
    /// hour and 98,305 the next. Shown raw it says the same enormous thing for ever and never says
    /// whether it is happening *now*.
    ///
    /// Two readings and the time between them, with two cases that are not arithmetic: the **first**
    /// answer after a restart has nothing to have changed since and says so rather than reporting
    /// zero, and a counter that went **backwards** is a sandbox that rebooted rather than a negative
    /// rate — everything since its boot is what has happened since skein last looked.
    #[test]
    fn pressure_is_reported_as_a_rate_and_survives_a_sandbox_reboot() {
        // **One kill, counted by every counter that can see it** — which is what the fixture has to
        // look like, because `memory.events` in cgroup v2 is hierarchical: a container's OOM is in
        // `skein/containers`, in `skein` above it, in `docker`, and in the VM-wide vmstat. Adding
        // them up reported one kill as four (FLEET-9), and a fixture that put the count in one
        // field could not tell the sum from the truth.
        let reading = |high: u64, kills: u64, restarts: u64| {
            serde_json::json!({
                "docker": { "restarts": restarts },
                "pressure": {
                    "skein": { "high": high, "oom_kill": kills },
                    "skein/containers": { "high": 0, "oom_kill": kills },
                    "docker": { "oom_kill": kills },
                    "vmstat_oom_kill": kills,
                }
            })
        };
        // The first answer: nothing to compare against, and it does not pretend otherwise.
        let first = rate_between(None, &reading(5551, 0, 0));
        assert!(
            !first.rated,
            "a first reading reported a rate it could not have"
        );
        assert_eq!(first.throttled_per_min, 0.0);
        assert_eq!(
            first.docker_restarts, 0,
            "the count is a total, not a delta"
        );

        // A minute later, ninety thousand more throttles: about 1,500 a minute, not 98,305.
        let second = rate_between(
            Some((Duration::from_secs(60), reading(5551, 0, 0))),
            &reading(95_551, 0, 2),
        );
        assert!(second.rated);
        assert_eq!(second.throttled_per_min.round(), 90_000.0);
        assert_eq!(second.docker_restarts, 2);

        // The sandbox rebooted: the counter is lower than it was. Everything it now reads has
        // happened since skein last looked, which is the only honest reading of it.
        let after = rate_between(
            Some((Duration::from_secs(60), reading(95_551, 4, 2))),
            &reading(120, 1, 0),
        );
        assert_eq!(
            after.throttled_per_min.round(),
            120.0,
            "a reboot read as a negative rate"
        );
        assert_eq!(after.killed, 1, "the kills since the reboot were lost");

        // And one kill is one kill. Every counter above moved by four, and the answer is four —
        // not sixteen, which is what summing the hierarchy gives.
        let killed = rate_between(
            Some((Duration::from_secs(60), reading(0, 1, 0))),
            &reading(0, 5, 0),
        );
        assert_eq!(
            killed.killed, 4,
            "the same kill was counted once per cgroup that contains it"
        );
    }

    /// A runaway container throttles itself instead of every box.
    ///
    /// `skein/containers` had no ceiling of its own, so containers were bounded only by the shared
    /// one they sit inside — and the live fleet's `memory.events` read `high 5551`, which is one
    /// container's overshoot stalling every box, five and a half thousand times. Under its own
    /// ceiling the container throttles itself and, past the hard limit, dies in its own cgroup
    /// rather than handing the kernel a choice across the whole workload.
    ///
    /// **A ceiling is not the reservation `MemoryPlan` argues against.** A reservation withholds
    /// memory from the boxes whether or not a container is running; this withholds nothing when
    /// containers are idle, which is the assertion below.
    #[test]
    fn containers_are_bounded_as_one_more_box_sized_claimant() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        save_config(&Config {
            fleet_memory: "26g".into(),
            ..Default::default()
        })
        .unwrap();
        let plan = memory_plan().expect("a plan");
        let spec = fleet_limits();
        std::env::remove_var("SKEIN_HOME");

        let pair = spec
            .split(',')
            .find(|p| p.starts_with("skein/containers="))
            .unwrap_or_else(|| panic!("containers have no ceiling: {spec}"));
        let value = pair.trim_start_matches("skein/containers=");
        let (max, high) = value.split_once('/').expect("max/high");
        let max: u64 = max.trim_end_matches('M').parse().expect("a size");
        let high: u64 = high.trim_end_matches('M').parse().expect("a size");

        // The same fraction a box gets, because that is what "one more claimant" means.
        assert_eq!(
            max,
            plan.boxes * 70 / 100,
            "containers are not sized like a box"
        );
        // `high` below `max`, for the reason it is below per box: an overshoot should be slow
        // before it is fatal.
        assert!(
            high < max,
            "the soft limit is not below the hard one: {value}"
        );
        // **Nothing is withheld.** The boxes' own ceiling is untouched by the containers' one, which
        // is the whole difference between this and the reservation `MemoryPlan` removed.
        assert!(
            spec.contains(&format!("skein={}M/", plan.boxes)),
            "the boxes' share shrank to make room for the containers': {spec}"
        );
        // And the nesting travels in the NAME rather than the value, so the launcher's `max/high`
        // split still sees two halves.
        assert_eq!(
            value.matches('/').count(),
            1,
            "the value grew a third field: {value}"
        );
    }

    /// The plumbing is guaranteed memory the kernel may not reclaim, and the number comes from the
    /// plan rather than from a preference.
    ///
    /// `/docker` is uncapped **by decision** and was unprotected **by omission**, and the two are
    /// not the same thing. `memory.min` is what says "do not reclaim this and do not reach for it
    /// first" — which is the difference between a daemon that survives a container's overshoot and
    /// one the global killer picks because it is the biggest thing in sight.
    ///
    /// Half the plumbing share, and the halving is the assertion: `memory.min` is taken from
    /// everybody else, so a promise larger than the budget it comes from would turn this into the
    /// next problem.
    #[test]
    fn the_plumbing_is_guaranteed_half_of_what_the_plan_set_aside_for_it() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        save_config(&Config {
            fleet_memory: "26g".into(),
            ..Default::default()
        })
        .unwrap();
        let plan = memory_plan().expect("a plan");
        let said = fleet_guarantees();
        std::env::remove_var("SKEIN_HOME");
        assert_eq!(
            said.split(',').find(|p| p.starts_with("docker=")),
            Some(format!("docker={}M", plan.plumbing / 2).as_str()),
            "the guarantee is not half the plumbing share: {said}"
        );
        assert!(
            plan.plumbing / 2 < plan.plumbing,
            "a guarantee the size of the whole share leaves the rest of the plumbing nothing"
        );
    }

    /// Every box's floor together is the gap between one box's ceiling and the fleet's throttle
    /// line, so a box bursting to its own `max` never has to reclaim from another box's floor.
    ///
    /// What makes it fail: a budget taken as a plain share of the boxes' memory (a third, say)
    /// rather than as the gap. A box at its own `max` plus the floors then crosses the shared
    /// `high`, which is the first assertion. A budget that ignores an explicit `box_memory_max`
    /// fails the second half.
    #[test]
    fn the_boxes_floors_fit_between_one_box_at_its_ceiling_and_the_fleets_throttle() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        for total in ["4g", "8g", "26g", "64g"] {
            save_config(&Config {
                fleet_memory: total.into(),
                ..Config::default()
            })
            .unwrap();
            let plan = memory_plan().unwrap();
            let floors = box_floor_budget();
            let spec = fleet_limits();
            let skein_high = spec
                .split(',')
                .find_map(|p| p.strip_prefix("skein="))
                .and_then(|v| v.split_once('/'))
                .and_then(|(_, high)| parse_mib(high))
                .unwrap_or_else(|| panic!("no shared high in {spec}"));
            let one_box_max = box_limits()
                .split(',')
                .find_map(|p| p.strip_prefix("max="))
                .and_then(parse_mib)
                .unwrap_or_else(|| panic!("no box max in {}", box_limits()));
            assert!(floors > 0, "no floor at all at {total}");
            assert!(
                one_box_max + floors <= skein_high,
                "a box at its own ceiling plus every floor crosses the fleet's throttle at {total}: \
                 {one_box_max}M + {floors}M > {skein_high}M"
            );
            let docker_min = fleet_guarantees()
                .split(',')
                .find_map(|p| p.strip_prefix("docker="))
                .and_then(parse_mib)
                .unwrap_or(0);
            assert!(
                floors + docker_min <= plan.boxes,
                "the floors and the plumbing's guarantee add up to more than the workload's share \
                 at {total}"
            );
            assert!(
                fleet_guarantees().contains(&format!("boxes={floors}M")),
                "the launcher is never handed the budget: {}",
                fleet_guarantees()
            );
        }
        // One box allowed as much as the fleet's throttle line leaves no gap, and no floor.
        save_config(&Config {
            fleet_memory: "26g".into(),
            box_memory_max: "30g".into(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(box_floor_budget(), 0);
        assert!(
            !fleet_guarantees().contains("boxes="),
            "a floor with no room for it: {}",
            fleet_guarantees()
        );
    }

    /// The launcher gives every box cgroup its share of the budget and adds up to no more than it,
    /// on a fake cgroup tree with the real launcher's shell.
    ///
    /// What makes it fail: skipping a box (the `box-a has no floor` assertion), writing the whole
    /// budget to each box instead of dividing it (the sum assertion), or counting `containers` as
    /// a box (its own assertion). The second pass adds a box, which is how a fleet grows. Every
    /// floor has to shrink so the sum still fits. A launcher that wrote only the new box fails
    /// the sum.
    #[test]
    fn every_box_gets_a_floor_and_the_floors_never_add_up_to_more_than_the_budget() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        save_config(&Config {
            fleet_memory: "26g".into(),
            ..Config::default()
        })
        .unwrap();
        let plan = memory_plan().unwrap();
        let (limits, guarantees, budget) = (fleet_limits(), fleet_guarantees(), box_floor_budget());
        drop(env);

        let dir = tempdir();
        let root = std::path::Path::new(&dir);
        for cgroup in [
            "skein",
            "skein/containers",
            "skein/box-a",
            "skein/box-b",
            "skein/box-c",
            "docker",
        ] {
            std::fs::create_dir_all(root.join("cgroup").join(cgroup)).unwrap();
        }
        // Exactly the configured size, so nothing is scaled and the arithmetic below is exact.
        std::fs::write(
            root.join("meminfo"),
            format!("MemTotal:       {} kB\n", 26 * 1024 * 1024),
        )
        .unwrap();
        let harness = format!(
            "set -uo pipefail\n\
             {sudo}\n\
             {body}\n\
             fleet_limits={limits}\n\
             SKEIN_FLEET_GUARANTEES={guarantees}\n\
             apply_fleet_ceilings\n",
            sudo = SUDO_WRITES_A_VALUE_TO_A_PATH,
            limits = sh_quote(&limits),
            guarantees = sh_quote(&guarantees),
            body = BOX_SESSION_SH
                .lines()
                .skip_while(|l| !l.starts_with("apply_fleet_ceilings() {"))
                .take_while(|l| *l != "}")
                .collect::<Vec<_>>()
                .join("\n")
                .replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()))
                .replace("/proc/meminfo", &root.join("meminfo").to_string_lossy())
                + "\n}",
        );
        let floor = |cgroup: &str| -> Option<u64> {
            std::fs::read_to_string(root.join("cgroup").join(cgroup).join("memory.min"))
                .ok()
                .and_then(|s| parse_mib(s.trim()))
        };
        let docker_min = guarantees
            .split(',')
            .find_map(|p| p.strip_prefix("docker="))
            .and_then(parse_mib)
            .expect("the plumbing's guarantee");
        for boxes in [
            &["box-a", "box-b", "box-c"][..],
            &["box-a", "box-b", "box-c", "box-d"],
        ] {
            std::fs::create_dir_all(root.join("cgroup/skein").join(boxes[boxes.len() - 1]))
                .unwrap();
            let out = run_harness(root, &harness);
            let floors: Vec<u64> = boxes
                .iter()
                .map(|name| {
                    floor(&format!("skein/{name}")).unwrap_or_else(|| {
                        panic!(
                            "{name} has no floor: {}",
                            String::from_utf8_lossy(&out.stderr)
                        )
                    })
                })
                .collect();
            let sum: u64 = floors.iter().sum();
            assert!(
                sum <= budget && sum + docker_min <= plan.boxes,
                "{} boxes' floors add up to {sum}M against a budget of {budget}M",
                boxes.len()
            );
            assert!(
                floors.iter().all(|f| *f == budget / boxes.len() as u64),
                "the boxes did not get equal shares of the budget: {floors:?}"
            );
            assert_eq!(
                floor("skein/containers"),
                None,
                "the containers were counted as a box and given a floor"
            );
            assert_eq!(
                floor("skein"),
                None,
                "the workload's parent was given a floor"
            );
        }
    }

    /// The `sudo` stand-ins these tests run the launcher's own shell against.
    ///
    /// Written down once, and both begin by dropping sudo's OWN options — the one line every sudo
    /// stand-in in the crate shares (`testutil::sudo_drops_its_own_options`, SKEIN-811) — which is
    /// the whole reason they are here rather than inline at each call. They used to read their arguments purely by
    /// position — `shift 4`, and `case "$1" in mkdir)` — which is correct only for the exact argv
    /// the launcher happened to send on the day each was written. SKEIN-555 put `-n` in front of
    /// all twelve `sudo` calls in `box-session.sh`, and both misread it: the first shifted one
    /// argument early and wrote a file named after the VALUE it was passed, and the second matched
    /// neither arm, fell through to `*) "$@"` and tried to run a command called `-n`. Six tests
    /// went red for a change that was right.
    ///
    /// A stub that only survives one spelling of a call is a stub that makes every future flag look
    /// unsafe to add. These skip options and then read positions, so the launcher can gain or lose
    /// one without the shim quietly meaning something else.
    ///
    /// `while [ $# -gt 0 ]` before the test, so this is still correct under the `set -u` that
    /// `an_unreadable_ceiling_is_skipped_rather_than_fatal` runs it with.
    const SUDO_WRITES_A_VALUE_TO_A_PATH: &str = concat!(
        "sudo() { ",
        crate::testutil::sudo_drops_its_own_options!(),
        " shift 4; sh -c 'echo \"$1\" > \"$2\"' _ \"$1\" \"$2\"; }"
    );

    /// The other shape: dispatches on the command sudo was asked to run, both ways the per-box
    /// cgroup block spells it — `mkdir -p`, and `sh -c` with and without positionals after the
    /// script.
    const SUDO_DISPATCHES_ON_THE_COMMAND: &str = concat!(
        "sudo() { ",
        crate::testutil::sudo_drops_its_own_options!(),
        " case \"$1\" in \
           mkdir) shift; mkdir \"$@\" ;; \
           sh) shift; if [ \"$#\" = 2 ]; then sh -c \"$2\"; else sh \"$@\"; fi ;; \
           *) \"$@\" ;; \
         esac; }"
    );

    /// Run a block of the launcher's shell, with the fixture as its working directory.
    ///
    /// **The `current_dir` is containment, not tidiness.** A stub that misreads its arguments
    /// writes a file named after whatever it took for the path, and a bash with no working
    /// directory of its own inherits the test process's — the repository root. Ten such files
    /// landed there (`128M`, `max`, `50`, `15975M`, …) when the stubs above were still positional,
    /// and because `tools/gates.sh` compares `git status --porcelain` across its run, the failing
    /// tests dirtied the very tree being measured: the runner refused its own results, and thirteen
    /// green gates described a tree that no longer existed.
    ///
    /// So it is applied here, to every harness, rather than in any one stub — a test that can write
    /// outside its fixture can spoil any gate run, not only the one that catches it, and the next
    /// harness must not be able to reopen that by bringing a stub of its own.
    fn run_harness(root: &std::path::Path, script: &str) -> std::process::Output {
        std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .current_dir(root)
            .output()
            .expect("run a block of the launcher's shell")
    }

    /// A harness that writes where it should not leaves the working tree alone.
    ///
    /// This is the assertion that stops the litter coming back, and it is about `run_harness`
    /// rather than about any stub: the stubs were only the misfire that happened to be found. Any
    /// shell these tests run can write a relative path — a typo, a `>` with an unset variable, the
    /// next positional shim — and without a working directory of its own it lands in the
    /// repository root, where `tools/gates.sh` sees it as the tree changing under the run and
    /// refuses its own results. That refusal is correct and it is expensive: it makes thirteen
    /// green gates unquotable.
    ///
    /// What makes it fail: take `.current_dir(root)` off `run_harness`. The probe then lands in the
    /// test process's own directory, which is the repository root — so the check reads that
    /// location, **removes anything it finds there**, and only then asserts. A test that forbids
    /// dirtying the tree must not dirty the tree on its way to failing.
    #[test]
    fn a_harness_that_writes_where_it_should_not_cannot_dirty_the_working_tree() {
        let dir = tempdir();
        let root = std::path::Path::new(&dir);
        // Named for this process, so two of these running at once cannot read each other's probe.
        let probe = format!("skein-containment-probe-{}", std::process::id());

        let out = run_harness(root, &format!("echo contained > {probe}"));
        assert!(
            out.status.success(),
            "the probe harness did not run at all: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Where it would have gone with no working directory of its own.
        let escape = std::env::current_dir()
            .expect("a working directory")
            .join(&probe);
        let escaped = escape.exists();
        if escaped {
            std::fs::remove_file(&escape).expect("clear the probe this test just leaked");
        }
        assert!(
            !escaped,
            "a harness wrote {probe} into {} — the working tree, which a gate run reads as the \
             tree changing underneath it",
            escape.parent().unwrap_or(&escape).display()
        );
        assert!(
            root.join(&probe).exists(),
            "the probe landed neither in the fixture nor in the working tree, so this test no \
             longer demonstrates anything about where a stray write goes"
        );
    }

    /// And the launcher writes it, on the same cgroup and under the same rules as a ceiling.
    #[test]
    fn a_guarantee_is_written_to_memory_min_and_scaled_like_a_ceiling() {
        let dir = tempdir();
        let root = std::path::Path::new(&dir);
        for cgroup in ["skein", "docker"] {
            std::fs::create_dir_all(root.join("cgroup").join(cgroup)).unwrap();
        }
        // Half the size skein was configured for, so the scaling applies to the guarantee exactly as
        // it does to the ceilings — a promise worked out for a machine twice the real size is not a
        // promise, it is an over-commitment.
        std::fs::write(root.join("meminfo"), "MemTotal:       13631488 kB\n").unwrap();
        let harness = format!(
            "{sudo}\n\
             {body}\n\
             fleet_limits='total=26624M,skein=15975M/14377M,docker=max/max'\n\
             SKEIN_FLEET_GUARANTEES='docker=256M'\n\
             apply_fleet_ceilings\n",
            sudo = SUDO_WRITES_A_VALUE_TO_A_PATH,
            body = BOX_SESSION_SH
                .lines()
                .skip_while(|l| !l.starts_with("apply_fleet_ceilings() {"))
                .take_while(|l| *l != "}")
                .collect::<Vec<_>>()
                .join("\n")
                .replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()))
                .replace("/proc/meminfo", &root.join("meminfo").to_string_lossy())
                + "\n}",
        );
        let out = run_harness(root, &harness);
        let read = |cgroup: &str, file: &str| -> String {
            std::fs::read_to_string(root.join("cgroup").join(cgroup).join(file))
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        assert_eq!(
            read("docker", "memory.min"),
            "128M",
            "half a sandbox gets half the guarantee: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // And nothing is promised to the workload: a guarantee there would be taken from the
        // plumbing it exists to protect.
        assert_eq!(
            read("skein", "memory.min"),
            "",
            "the boxes were promised memory nobody meant"
        );
    }

    /// The launcher writes the nested ceiling to the nested cgroup, not to one named after it.
    ///
    /// `skein/containers=…` carries a slash in the *name*, and the launcher builds a path from the
    /// name and splits the *value* on `/`. If those two ever met, the ceiling would land on a
    /// cgroup called `containers` at the root — a directory that does not exist, so the write would
    /// vanish and the containers would stay unbounded with nothing said.
    #[test]
    fn a_nested_ceiling_lands_on_the_nested_cgroup() {
        let dir = tempdir();
        let root = std::path::Path::new(&dir);
        for cgroup in ["skein", "skein/containers", "docker"] {
            std::fs::create_dir_all(root.join("cgroup").join(cgroup)).unwrap();
        }
        std::fs::write(root.join("meminfo"), "MemTotal:       27262976 kB\n").unwrap();
        let harness = format!(
            "{sudo}\n\
             {body}\n\
             fleet_limits='total=26624M,skein=15975M/14377M,skein/containers=11182M/10063M,docker=max/max'\n\
             apply_fleet_ceilings\n",
            sudo = SUDO_WRITES_A_VALUE_TO_A_PATH,
            body = BOX_SESSION_SH
                .lines()
                .skip_while(|l| !l.starts_with("apply_fleet_ceilings() {"))
                .take_while(|l| *l != "}")
                .collect::<Vec<_>>()
                .join("\n")
                .replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()))
                .replace("/proc/meminfo", &root.join("meminfo").to_string_lossy())
                + "\n}",
        );
        let out = run_harness(root, &harness);
        let read = |cgroup: &str, file: &str| -> String {
            std::fs::read_to_string(root.join("cgroup").join(cgroup).join(file))
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        assert_eq!(
            read("skein/containers", "memory.max"),
            "11182M",
            "the containers' ceiling did not reach their cgroup: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(read("skein/containers", "memory.high"), "10063M");
        // And the CPU share, which is a weight rather than a cap: a cap would idle cores while a
        // container waits, and half a box is what it is worth when both want the machine.
        assert_eq!(
            read("skein/containers", "cpu.weight"),
            "50",
            "containers weigh the same as a box, so one can starve the daemon they depend on"
        );
        // The boxes' own ceiling is untouched by it.
        assert_eq!(read("skein", "memory.max"), "15975M");
        assert!(
            !root.join("cgroup/containers").exists(),
            "a cgroup was created at the root from the name's second half"
        );
    }

    /// The gap between what skein is *configured* for and what the sandbox actually got. sbx fixes
    /// a sandbox's memory when it is created, so editing Fleet memory without rebuilding leaves the
    /// config describing a VM that does not exist — and ceilings worked out for a machine twice the
    /// real size bound nothing at all. Run against the real launcher, with a fake cgroup tree and a
    /// fake `/proc/meminfo`, because the scaling lives in shell and an assertion about the Rust
    /// half would prove nothing about it.
    #[test]
    fn ceilings_shrink_to_the_memory_the_sandbox_really_has() {
        let dir = tempdir();
        let root = std::path::Path::new(&dir);
        for cgroup in ["skein", "docker"] {
            std::fs::create_dir_all(root.join("cgroup").join(cgroup)).unwrap();
        }
        // A sandbox with MORE than skein was told about keeps the ceilings as computed: the reserve
        // is deliberate, and a surplus nobody configured is not an invitation to spend it.
        std::fs::write(root.join("meminfo"), "MemTotal:       41943040 kB\n").unwrap();
        // The launcher's own function, with `sudo` and the cgroup root redirected at the fixture.
        let harness = format!(
            "{sudo}\n\
             {body}\n\
             fleet_limits='total=26624M,skein=15975M/14377M,docker=max/max'\n\
             apply_fleet_ceilings\n",
            sudo = SUDO_WRITES_A_VALUE_TO_A_PATH,
            body = BOX_SESSION_SH
                .lines()
                .skip_while(|l| !l.starts_with("apply_fleet_ceilings() {"))
                .take_while(|l| *l != "}")
                .collect::<Vec<_>>()
                .join("\n")
                .replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()))
                .replace("/proc/meminfo", &root.join("meminfo").to_string_lossy())
                + "\n}",
        );
        let run = || -> String {
            let out = run_harness(root, &harness);
            String::from_utf8_lossy(&out.stderr).into_owned()
        };
        let read = |cgroup: &str, file: &str| -> String {
            std::fs::read_to_string(root.join("cgroup").join(cgroup).join(file))
                .unwrap_or_else(|e| panic!("{cgroup}/{file}: {e}"))
                .trim()
                .to_string()
        };
        let quiet = run();
        assert_eq!(read("skein", "memory.max"), "15975M");
        // Written, not left alone: this is how a fleet an older skein capped gets uncapped.
        assert_eq!(read("docker", "memory.max"), "max");
        assert_eq!(read("docker", "memory.high"), "max");
        assert!(
            !quiet.contains("scaling"),
            "nothing to scale, so nothing to say: {quiet}"
        );

        // Half the configured size — every ceiling comes out halved, and says so.
        std::fs::write(root.join("meminfo"), "MemTotal:       13631488 kB\n").unwrap();
        let noisy = run();
        assert_eq!(read("skein", "memory.max"), "7987M", "half of 15975");
        assert_eq!(read("skein", "memory.high"), "7188M");
        // Half of no ceiling is still no ceiling — scaling must not turn `max` into a number.
        assert_eq!(read("docker", "memory.max"), "max");
        assert!(
            noisy.contains("not the 26624M"),
            "a sandbox smaller than skein was told must say so, not silently differ: {noisy}"
        );
    }

    /// A ceiling this launcher cannot read costs the ceiling, not the box.
    ///
    /// The sandbox keeps whichever `box-session.sh` it was last given, so the launcher applying a
    /// spec is routinely OLDER than the skein that sent it. When that gap first opened it took the
    /// whole fleet down: skein began sending `docker=max/max`, the installed launcher fed `max` to
    /// `$(( ))`, and `set -u` aborted the shell before it reached tmux — so every box stopped
    /// starting and each reconnect reported `nsenter: cannot open /proc/<pid>/ns/user`, an error
    /// about namespaces for a fleet that needed a file copied.
    ///
    /// `heal_fleet` narrows that window; it cannot close it, because the next unfamiliar token will
    /// reach some sandbox before the launcher that understands it does. So the launcher has to
    /// degrade rather than die, and this asserts the three properties that means: an unreadable
    /// cgroup is skipped and says so, a readable one beside it is still applied, and neither half
    /// of an unreadable pair is written — a `high` with no `max` above it is the throttle-forever
    /// shape these two limits exist together to avoid.
    ///
    /// Run against the real launcher for the same reason as the test above: the logic is shell, and
    /// a Rust assertion about it would prove nothing.
    #[test]
    fn an_unreadable_ceiling_is_skipped_rather_than_fatal() {
        let dir = tempdir();
        let root = std::path::Path::new(&dir);
        for cgroup in ["skein", "docker"] {
            std::fs::create_dir_all(root.join("cgroup").join(cgroup)).unwrap();
        }
        std::fs::write(root.join("meminfo"), "MemTotal:       27262976 kB\n").unwrap();
        let body = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("apply_fleet_ceilings() {"))
            .take_while(|l| *l != "}")
            .collect::<Vec<_>>()
            .join("\n")
            .replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()))
            .replace("/proc/meminfo", &root.join("meminfo").to_string_lossy())
            + "\n}";
        // `set -uo pipefail` as the real launcher has it — without it this proves nothing, since
        // the failure being guarded against is precisely what `set -u` does to an unread word.
        let run = |spec: &str| -> (String, bool) {
            let out = run_harness(
                root,
                &format!(
                    "set -uo pipefail\n\
                     {sudo}\n\
                     {body}\n\
                     fleet_limits='total=26624M,skein=15975M/14377M,{spec}'\n\
                     apply_fleet_ceilings\n\
                     echo REACHED-THE-END\n",
                    sudo = SUDO_WRITES_A_VALUE_TO_A_PATH,
                ),
            );
            (
                String::from_utf8_lossy(&out.stderr).into_owned(),
                String::from_utf8_lossy(&out.stdout).contains("REACHED-THE-END"),
            )
        };
        let wrote = |cgroup: &str, file: &str| -> Option<String> {
            std::fs::read_to_string(root.join("cgroup").join(cgroup).join(file))
                .ok()
                .map(|s| s.trim().to_string())
        };

        // A word no launcher of this vintage knows — `max` was one of these once.
        let (said, finished) = run("docker=somethingnew/somethingnew");
        assert!(
            finished,
            "the launcher died on a ceiling it could not read, so no box in this fleet starts"
        );
        assert!(
            said.contains("somethingnew"),
            "a skipped ceiling has to name itself, or the fleet runs unbounded and silently: {said}"
        );
        assert_eq!(
            wrote("docker", "memory.max"),
            None,
            "a ceiling that could not be read must leave the cgroup as it found it"
        );
        assert_eq!(
            wrote("skein", "memory.max").as_deref(),
            Some("15975M"),
            "one unreadable cgroup must not cost the others theirs — skein is the ceiling that \
             actually bounds the workload"
        );

        // Half-readable is the dangerous one: `high` alone throttles against a ceiling that is not
        // there, which is the wedge that started all of this.
        for cgroup in ["skein", "docker"] {
            for file in ["memory.max", "memory.high"] {
                let _ = std::fs::remove_file(root.join("cgroup").join(cgroup).join(file));
            }
        }
        let (_, finished) = run("docker=notasize/7188M");
        assert!(finished, "still not fatal when only one half is unreadable");
        assert_eq!(
            wrote("docker", "memory.high"),
            None,
            "a `high` written without the `max` above it is the throttle-forever shape: both halves \
             are read before either is written"
        );
    }

    /// The launcher is the only thing that runs on every box start, which is what the Docker
    /// ceiling needs: dockerd rebuilds its cgroup from scratch when the sandbox cycles and takes
    /// any limit written on it along with it.
    #[test]
    fn the_launcher_is_handed_the_shared_ceilings_as_well_as_the_boxs_own() {
        let _g = env_lock();
        let home = tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // A fixture fleet root: `util::fleet_root` refuses an unpinned test rather than answering
        // `/boxes`, which on any machine running skein is the live fleet (SKEIN-690). Nothing
        // asserted below carries the root, so a fixture is the whole of what this needs.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("fleet"));
        save_config(&Config {
            fleet_memory: "26g".into(),
            ..Config::default()
        })
        .unwrap();
        let script = session_script("web-main", "skein-agent", "claude");
        assert!(
            script.contains(&fleet_limits()),
            "the box would start under a ceiling nobody had applied: {script}"
        );
        assert!(
            script.contains(&box_limits()),
            "and its own ceiling still has to get there: {script}"
        );
        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// `doctor` does not call skein's own deliberate ceiling a failure.
    ///
    /// It did, and it was the only ✗ in the fleet section — the one line telling somebody their
    /// fleet was unsafe to depend on, for a value `fleet_limits` writes on purpose. Driven through
    /// the reading rather than by matching doctor's output, so the judgement is tested where it is
    /// made and the printer stays a printer.
    #[test]
    fn a_ceiling_reads_as_what_the_design_wanted_and_not_as_the_word_uncapped() {
        let says = |cgroup: &str, live: &str| {
            let (_, what, goal) = CEILINGS
                .iter()
                .find(|(name, _, _)| *name == cgroup)
                .unwrap_or_else(|| panic!("{cgroup} is not one of the ceilings doctor reads"));
            ceiling_reading(what, *goal, live)
        };

        // The one that shipped wrong. `fleet_limits` writes `docker=max/max`; doctor called it BAD.
        match says("docker", "max") {
            Ceiling::Good(said) => assert!(
                said.contains("by design"),
                "an uncapped `docker` is right, and has to READ as right: {said}"
            ),
            Ceiling::Bad(said) => panic!(
                "doctor calls skein's own `docker=max/max` a failure: {said}\n\
                 `fleet_limits` writes that value deliberately — capping the cgroup that holds init \
                 and socat stalls the sandbox's service path, and its OOM picks from a cgroup with \
                 pid 1 in it. The containers are moved instead, into `skein/containers`."
            ),
            Ceiling::Absent(said) => panic!("{said}"),
        }
        // And the value that IS wrong there — written by an older skein — says so, and says that
        // starting a box undoes it.
        assert!(
            matches!(says("docker", "8589934592"), Ceiling::Bad(said) if said.contains("older skein")),
            "a `docker` an older skein capped reads as fine, so nobody ever clears it"
        );

        // The real ceiling, both ways round.
        assert!(
            matches!(says("skein", "max"), Ceiling::Bad(said) if said.contains("UNBOUNDED")),
            "an uncapped `skein` is one runaway box taking every other box down, and must be loud"
        );
        assert!(
            matches!(says("skein", "25566023680"), Ceiling::Good(said) if said.contains("23.8G")),
            "a capped `skein` should read back the size it is capped to"
        );

        // Reported at all, which it was not: the whole argument for `docker=max/max` is that the
        // containers moved somewhere that IS capped, and doctor never showed that place.
        assert!(
            CEILINGS
                .iter()
                .any(|(name, _, _)| *name == "skein/containers"),
            "doctor does not report the cgroup the containers were moved INTO, so the reason the \
             one above it is uncapped cannot be checked from the report"
        );

        // Nothing there to have an opinion about is its own answer — not a failure, and not a pass.
        assert!(matches!(says("docker", ""), Ceiling::Absent(_)));
    }

    /// The value doctor reads back is the value skein writes.
    ///
    /// The two drifting apart is exactly what this pair of changes was about, and they live in
    /// different functions a hundred lines apart.
    #[test]
    fn every_ceiling_doctor_reads_is_one_the_fleet_actually_sets() {
        // `fleet_limits` works the plan out from the config, so it resolves `config::skein_home` —
        // which refuses an unpinned test rather than answering with the real `~/.skein` (SKEIN-626).
        // Unpinned it read the owner's live `~/.skein/config.json`; it only ever passed because a
        // neighbour in this process had left `$SKEIN_HOME` set. The default config is the right
        // fixture: what is compared is two lists of cgroup names, not a tuning.
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let written = fleet_limits();
        for (cgroup, _, _) in CEILINGS {
            assert!(
                written.contains(&format!("{cgroup}=")),
                "doctor reports `{cgroup}` and `fleet_limits` never sets it: {written}"
            );
        }
        std::env::remove_var("SKEIN_HOME");
    }

    /// Run the launcher's per-box cgroup block for real, against a scratch cgroup tree.
    ///
    /// Extracted rather than restated, and by its first and last lines rather than by a function
    /// name, because this block is top-level: it runs once per launch, before `exec bwrap`, and
    /// making it a function to be testable would move code for the test's benefit.
    ///
    /// The scratch directory's guard comes back with the answer, and that is not tidiness: dropping
    /// it removes the tree, so a helper that returned only the path handed the caller a directory
    /// that no longer existed and every assertion read an empty file.
    fn box_cgroup_block(limits: &str) -> (String, crate::testutil::TempDir) {
        let dir = crate::testutil::tempdir();
        let root = std::path::PathBuf::from(dir.as_ref() as &std::path::Path);
        std::fs::create_dir_all(root.join("boxroot")).unwrap();
        let body: Vec<&str> = BOX_SESSION_SH
            .lines()
            .skip_while(|l| !l.starts_with("cgroup_root=\"/sys/fs/cgroup/skein\""))
            .take_while(|l| !l.starts_with("printf \"SKEIN_LIMITS"))
            .collect();
        assert!(
            body.len() > 20,
            "the per-box cgroup block was not found in box-session.sh; this test would prove \
             nothing: {body:?}"
        );
        let block = body
            .join("\n")
            .replace("/sys/fs/cgroup/", &format!("{}/cgroup/", root.display()));
        let harness = format!(
            "{sudo}\n\
             apply_fleet_ceilings() {{ :; }}\n\
             box=demo\n\
             root={root}/boxroot\n\
             limits={limits}\n\
             {block}\n\
             printf 'SKEIN_LIMITS %s\\n' \"$limits_state\"\n",
            sudo = SUDO_DISPATCHES_ON_THE_COMMAND,
            root = root.display(),
            limits = crate::util::sh_quote(limits),
        );
        let out = run_harness(&root, &harness);
        assert!(
            out.status.success(),
            "the block failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (String::from_utf8_lossy(&out.stdout).trim().to_string(), dir)
    }

    /// The gap: a box with no computed ceiling got no cgroup **either**.
    ///
    /// The cgroup does two jobs and only one is the ceiling — it is also the box's identity as a set
    /// of processes, which is what `cgroup.kill` needs at stop and what the fleet's accounting rests
    /// on. Gating the whole block on `$limits` gave the box that most needed containing neither, and
    /// `2>/dev/null || true` on the kill made that silent.
    #[test]
    fn a_box_with_no_ceiling_still_gets_a_cgroup_to_be_contained_by() {
        let (said, dir) = box_cgroup_block("");
        let cg = dir.join("cgroup/skein/demo");
        assert!(
            cg.join("cgroup.procs").is_file(),
            "the box joined no cgroup, so nothing at stop can reach what it started"
        );
        assert!(
            !cg.join("memory.max").exists(),
            "a ceiling was invented for a box skein computed none for"
        );
        assert_eq!(
            said, "SKEIN_LIMITS uncapped no-limit-computed",
            "the box is contained but unbounded, and it did not say so"
        );
    }

    /// And the ordinary case still writes the ceiling it was given, and says it did.
    #[test]
    fn a_box_with_a_ceiling_reports_the_one_it_got() {
        let (said, dir) = box_cgroup_block("max=1G,high=800M,pids=512");
        let cg = dir.join("cgroup/skein/demo");
        let read = |f: &str| {
            std::fs::read_to_string(cg.join(f))
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        assert_eq!(read("memory.max"), "1G");
        assert_eq!(read("memory.high"), "800M");
        assert_eq!(read("pids.max"), "512");
        assert_eq!(said, "SKEIN_LIMITS capped max=1G,high=800M,pids=512");
    }
}
