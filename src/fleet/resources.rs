//! What the fleet and each box are using right now — memory, CPU, disk — measured in the
//! sandbox and remembered behind a gate.

use super::*;

/// What the fleet's one VM is actually using right now — the gauge behind [`fleet_resources`].
///
/// Sized in MiB throughout, because that is what every other number in this module speaks and the
/// browser should not have to know which unit each field arrived in.
///
/// `boxes` and `docker` are the two cgroups [`fleet_limits`] writes ceilings on. They are here
/// rather than a single VM total because a single total cannot answer the question you ask when the
/// sandbox is struggling: *what* is holding it. 12 GB in the boxes is the agents working; 12 GB in
/// `docker` is a container someone forgot, in a cgroup no per-box limit reaches.
///
/// Memory *used* is `MemTotal - MemAvailable`, not `MemTotal - MemFree`. Free is nearly always small
/// and nearly always meaningless — the kernel spends idle memory on page cache and hands it back on
/// demand — so a gauge drawn from it reads as a permanently full machine.
///
/// The two cgroup figures are `anon` from `memory.stat`, **not** `memory.current`, and the two are
/// not interchangeable: `current` counts page cache, which `MemAvailable` has already treated as
/// free. Measured on this fleet, `current` reported 11.0 GB for the boxes and 10.8 GB for docker
/// against a whole-VM `mem_used` of 2.9 GB — two parts of a bar, each four times the bar. `anon`
/// gave 1.2 GB and 0.8 GB, which sum inside the total and leave the VM's own services visible as
/// the difference. It is also the memory that *matters* here, being the part the kernel cannot
/// reclaim its way out of and therefore the part that ends in an OOM kill.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FleetResources {
    pub mem_total: u64,
    pub mem_used: u64,
    pub boxes: u64,
    pub docker: u64,
    pub disk_total: u64,
    pub disk_used: u64,
    /// `/var/lib/docker` — images, volumes and build cache. **A different disk from the one above**,
    /// and that is why it is measured separately rather than folded in: sbx gives a sandbox two, a
    /// root filesystem sized by `DOCKER_SANDBOXES_ROOT_SIZE` and this one by
    /// `DOCKER_SANDBOXES_DOCKER_SIZE`, so filling one says nothing about the other.
    ///
    /// Without it the gauge answered the wrong question confidently. Measured here: the boxes' disk
    /// 37% full while this one was at 76%, so a build that ran out of space did so against a strip
    /// showing two thirds free — the disk that filled was not the disk being drawn.
    ///
    /// Zero when Docker shares the boxes' filesystem, so the same bytes are never drawn twice.
    pub images_total: u64,
    pub images_used: u64,
    pub cpus: u64,
    pub load1: f64,
    pub load5: f64,
    /// What [`memory_plan`] allows the workload — `boxes` and `docker` **together**, because they
    /// share one pool taken first-come rather than holding a slice each. One number, so the gauge
    /// cannot imply two separate allowances where there is one. Zero when no fleet total is
    /// configured to divide.
    ///
    /// It is what the *plan* allows, not what any single cgroup enforces: only `skein` carries a
    /// ceiling (see [`fleet_limits`] for why `docker` cannot), so this is the line the workload is
    /// meant to stay under, and `docker` can cross it without being stopped.
    pub workload_max: u64,
    /// True while the sandbox is failing to answer — see [`crate::util::Gate`]. The figures are then the
    /// last ones that arrived, and saying so is the difference between stale and wrong.
    pub stale: bool,
}

/// What one box is using right now.
///
/// The fleet gauge answers "is the sandbox in trouble", which is the wrong question when the answer
/// is yes: the next thing you want is *which box*, and nothing could tell you. One box saturating
/// every core is legitimate here — `cpu.weight` is equal and uncapped on purpose, so a lone box gets
/// the whole machine and gives it back under contention — but "legitimate" and "what you wanted"
/// are different, and you cannot judge which without seeing the name.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BoxLoad {
    pub name: String,
    /// CPUs in use, measured rather than reported: `usage_usec` twice over a known interval. A
    /// cgroup only carries a running total, so a single read says how much CPU a box has used since
    /// it started — which is a fine way to rank yesterday's builds and no way at all to find what is
    /// busy now.
    pub cores: f64,
    /// **Anonymous memory: the part the kernel cannot reclaim its way out of**, and therefore the
    /// part that ends in an OOM kill.
    ///
    /// This used to be `memory.current`, which the fleet gauge had already stopped using for the
    /// same reason — `current` counts page cache, and a box that read a large repository an hour ago
    /// is charged for every byte of it until something else wants the memory. Measured on this
    /// fleet: a box showed `memory.current` at 1.87 GiB while its largest process held ~515 MB. An
    /// operator asked why a box they had closed was "using 15 GB"; the box was indeed still running
    /// (fixed separately), and the number was still not what it looked like.
    ///
    /// The two license different actions — "this box is why the fleet is slow" against "this box is
    /// fine and the kernel is doing its job" — so they are two fields rather than one.
    pub mem_anon: u64,
    /// Page cache and the rest: charged to the box, reclaimable under pressure.
    ///
    /// Kept rather than dropped, because it answers the question the headline provokes: a box whose
    /// anonymous memory is small and whose total is huge has been reading files, and somebody
    /// looking at a gauge deserves to be told that rather than left to wonder.
    pub mem_cache: u64,
    pub pids: u64,
    /// MiB on the fleet's shared disk, and this box's share of it. Merged in from
    /// [`fleet_disk_usage`] rather than measured here: counting bytes means walking the tree, which
    /// is seconds per box on a big checkout and has no business inside a half-second CPU sample.
    /// That walk is already done and already gated, so this costs a map lookup.
    ///
    /// Present because "what is eating the machine" is asked about disk at least as often as about
    /// CPU — and unlike memory, disk is the one the fleet actually runs out of: this sandbox hit
    /// 100% mid-build while a single box transiently took 25 GB.
    pub disk_mb: u64,
    /// What this box is allowed, when it has an allowance. Measured, never enforced — one
    /// filesystem serves every box — so this says who took the space, not who may.
    pub disk_limit_mb: Option<u64>,
}

/// Every box's live usage, in one round trip.
///
/// Not behind the resource gate: this is asked for deliberately rather than polled, and a cached
/// answer to "what is eating the machine *now*" is worse than a slow one. The half-second inside
/// the script is the measurement interval, not latency to hide.
pub fn box_loads() -> Vec<BoxLoad> {
    let sandbox = fleet_sandbox();
    let mut loads = own_sandbox(&sandbox)
        .exec(BOX_LOAD_SCRIPT, Duration::from_secs(20))
        .map(|out| parse_box_loads(&out, BOX_LOAD_INTERVAL_US))
        .unwrap_or_default();
    // Folded in after the sample rather than during it: the disk figures come from their own gate,
    // and making the CPU measurement wait on a tree walk would widen a half-second interval into
    // however long `du` takes over every box.
    let usage = fleet_disk_usage();
    for load in &mut loads {
        load.disk_mb = usage.get(&load.name).copied().unwrap_or(0);
        load.disk_limit_mb = usage.get(&load.name).and(box_disk_limit(&load.name));
    }
    loads
}

const BOX_LOAD_INTERVAL_US: f64 = 500_000.0;

/// Two samples of every box cgroup, separated by the interval above.
///
/// `usage_usec` is cumulative, so the pair is the whole point — and both are taken in one shell so
/// the interval is the sandbox's own clock rather than a round trip that might stall between them.
/// **Every field goes through `n()`, and that is not tidiness.** `printf` with an empty command
/// substitution emits two spaces where a value should be, and the parser splits on whitespace — so a
/// cgroup that could not answer for `anon` would shift `memory.current` into its place and report a
/// box's page cache as memory it is holding. Caught by the test for exactly that case. A field that
/// is always a number cannot shift.
const BOX_LOAD_SCRIPT: &str = "\
n() { v=$(cat \"$1\" 2>/dev/null); echo \"${v:-0}\"; }; \
k() { v=$(awk -v k=\"$2\" '$1==k{print $2}' \"$1\" 2>/dev/null); echo \"${v:-0}\"; }; \
cd /sys/fs/cgroup/skein 2>/dev/null || exit 0; \
for d in */; do n=${d%/}; \
  printf 'a %s %s\\n' \"$n\" \"$(k \"$d/cpu.stat\" usage_usec)\"; done; \
sleep 0.5; \
for d in */; do n=${d%/}; \
  printf 'b %s %s %s %s %s\\n' \"$n\" \
    \"$(k \"$d/cpu.stat\" usage_usec)\" \
    \"$(n \"$d/pids.current\")\" \
    \"$(k \"$d/memory.stat\" anon)\" \
    \"$(n \"$d/memory.current\")\"; done";

/// Turn the script's two passes into a rate per box. Its own function so the arithmetic is testable
/// without a sandbox — the parser is only correct against exactly the output above.
fn parse_box_loads(out: &str, interval_us: f64) -> Vec<BoxLoad> {
    let mut first: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    let mut loads = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.first() {
            Some(&"a") if f.len() >= 3 => {
                if let Ok(v) = f[2].parse() {
                    first.insert(f[1], v);
                }
            }
            Some(&"b") if f.len() >= 6 => {
                let (name, used) = (f[1], f[2].parse::<u64>().unwrap_or(0));
                // `anon` and the total, and the cache is the difference rather than a third read:
                // `memory.stat` has half a dozen reclaimable classes and summing the ones anybody
                // remembers is how a figure quietly stops adding up.
                let anon: u64 = f[4].parse().unwrap_or(0);
                let current: u64 = f[5].parse().unwrap_or(0);
                // A box that appeared between the two passes has no baseline. Reporting it at zero
                // is honest — it has been observed for no time at all — and beats inventing a rate
                // from a total that has been accumulating since it started.
                let before = first.get(name).copied().unwrap_or(used);
                loads.push(BoxLoad {
                    name: name.to_string(),
                    cores: (used.saturating_sub(before) as f64 / interval_us).max(0.0),
                    mem_anon: anon,
                    mem_cache: current.saturating_sub(anon),
                    pids: f[3].parse().unwrap_or(0),
                    // Filled by `box_loads` from the disk gate; the parser only sees the cgroup
                    // sample, which carries no notion of bytes on disk.
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
    loads.sort_by(|a, b| b.cores.total_cmp(&a.cores).then(a.name.cmp(&b.name)));
    loads
}

/// The fleet VM's memory, disk and CPU, in one round trip.
///
/// `None` when the sandbox has never answered. Deliberately coarse and deliberately stale-tolerant: this is a gauge you glance at, not
/// a number anything decides on, so it is worth at most one `sbx exec` every 30 seconds and worth
/// nothing at all when the sandbox is busy. The [`crate::util::Gate`] enforces both, and backs off further
/// while the sandbox is unwell — a struggling VM being asked how it feels every 2 seconds is how
/// skein used to keep it struggling.
///
/// One shell, printing `key value` lines, because the alternative is five round trips to build one
/// strip. `df` is asked about [`fleet_root`] rather than `/`: box roots are the only disk skein can
/// account for, and on a filesystem the boxes do not share the number would be answering about
/// somebody else's storage.
pub fn fleet_resources() -> Option<FleetResources> {
    let mut resources = measured()?;
    // The ceilings come from the host's own config, not the guest, so they are always current even
    // when the figures beside them are the last ones that arrived.
    if let Some(plan) = memory_plan() {
        resources.workload_max = plan.boxes;
    }
    resources.stale = RESOURCE_GATE.degraded();
    Some(resources)
}

/// The gated reading itself, before anything is worked out from it — shared by
/// [`fleet_resources`] and [`sandbox_memory_mib`], so the ceilings and the gauge strip are one
/// measurement. Split out because [`fleet_resources`] asks [`memory_plan`] for its ceiling, and
/// [`memory_plan`] asks for this: through [`fleet_resources`] the two would call each other.
fn measured() -> Option<FleetResources> {
    let sandbox = fleet_sandbox();
    let fresh = if cfg!(test) {
        Duration::ZERO
    } else {
        Duration::from_secs(30)
    };
    RESOURCE_GATE.get(fresh, move || {
        let out = own_sandbox(&sandbox)
            .exec(&resource_script(), Duration::from_secs(20))
            .ok()?;
        parse_resources(&out)
    })
}

/// The memory this sandbox HAS, in MiB — the `mem_total` the Fleet pane's "This sandbox has" line
/// draws — or `None` when it could not be read.
///
/// **The measured sandbox is the truth** (the owner, 2026-09-27): sbx fixes a sandbox's memory when
/// it is created, so `Config::fleet_memory` describes the NEXT create, and ceilings divided from it
/// on a sandbox made at another size bound a machine that does not exist. [`memory_plan`] divides
/// this instead.
///
/// Measured where skein runs, which is inside the sandbox: [`own_sandbox`] adds no hop, so this is
/// the process reading its own VM's `/proc/meminfo`, the server and a `skein start` alike. Behind
/// the same 30-second gate as the gauge strip, so a box start costs at most one reading.
#[cfg(not(test))]
pub fn sandbox_memory_mib() -> Option<u64> {
    measured().map(|r| r.mem_total).filter(|mib| *mib > 0)
}

/// Under test the reading is what the test says it is, and nothing when it says nothing. Measuring
/// for real would read the memory of whatever machine runs the tests, so every ceiling a test
/// works out from `fleet_memory` would depend on that machine. Per thread, because each test runs
/// on its own and one test's sandbox must not size another's.
#[cfg(test)]
pub fn sandbox_memory_mib() -> Option<u64> {
    MEASURED_MEMORY.with(|m| m.get())
}

#[cfg(test)]
thread_local! {
    pub(crate) static MEASURED_MEMORY: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// The one shell [`fleet_resources`] runs, printing `key value` lines.
///
/// Its own function so the wire format is readable in one place and testable without a sandbox —
/// the parser below is only correct against exactly this output.
fn resource_script() -> String {
    format!(
        "awk '/^MemTotal:/{{t=$2}} /^MemAvailable:/{{a=$2}} \
         END{{print \"mem_total\", int(t/1024); print \"mem_used\", int((t-a)/1024)}}' /proc/meminfo; \
         echo \"cpus $(nproc 2>/dev/null || echo 0)\"; \
         awk '{{print \"load1\", $1; print \"load5\", $2}}' /proc/loadavg; \
         df -Pm {root} 2>/dev/null \
         | awk 'NR==2{{print \"disk_dev\", $1; print \"disk_total\", $2; print \"disk_used\", $3}}'; \
         df -Pm {docker} 2>/dev/null \
         | awk 'NR==2{{print \"images_dev\", $1; print \"images_total\", $2; print \"images_used\", $3}}'; \
         for c in skein skein/containers docker; do \
         awk -v c=$c '/^anon /{{print c, int($2/1048576)}}' \
         /sys/fs/cgroup/$c/memory.stat 2>/dev/null; done",
        root = sh_quote(&fleet_root()),
        // Where dockerd's data actually is, not where it conventionally lives. With one pool,
        // `/var/lib/docker` is still a mounted disk — it is simply the one nothing writes to any
        // more, so measuring it would draw a gauge for an empty disk while the disk that filled up
        // went unreported. Pointed at the pool instead, `disk_dev` and `images_dev` come back as the
        // same device, which is exactly how the strip already knows to draw one row rather than two.
        docker = sh_quote(&effective_docker_root()),
    )
}

/// The directory dockerd keeps its data in, according to the setting that put it there.
fn effective_docker_root() -> String {
    if load_config().fleet_one_disk {
        docker_data_root()
    } else {
        "/var/lib/docker".to_string()
    }
}

/// `key value` lines into a [`FleetResources`], or `None` when the reply carried no memory total.
///
/// That last condition is the point of returning an `Option`: `sbx exec` can succeed while the guest
/// prints nothing usable — a sandbox mid-boot, a `/proc` not yet mounted — and without the check the
/// [`Gate`](crate::util::Gate) would remember a zeroed machine as a good answer and stop asking for 30
/// seconds. Missing individual fields are fine and stay zero; the browser hides a gauge whose
/// denominator is zero rather than drawing a bar against nothing.
fn parse_resources(out: &str) -> Option<FleetResources> {
    let mut r = FleetResources::default();
    // Where the containers are is a question with two answers during a migration, so both homes are
    // read and one is chosen below rather than summed.
    let mut nested: Option<u64> = None;
    let mut outside = 0;
    // Which device each `df` answered about, so the same filesystem is never drawn twice.
    let (mut root_dev, mut images_dev) = (String::new(), String::new());
    for line in out.lines() {
        let Some((key, value)) = line.trim().split_once(' ') else {
            continue;
        };
        let value = value.trim();
        let number = || value.parse::<u64>().unwrap_or(0);
        match key {
            "mem_total" => r.mem_total = number(),
            "mem_used" => r.mem_used = number(),
            "skein" => r.boxes = number(),
            "skein/containers" => nested = Some(number()),
            "docker" => outside = number(),
            "disk_total" => r.disk_total = number(),
            "disk_used" => r.disk_used = number(),
            "disk_dev" => root_dev = value.to_string(),
            "images_total" => r.images_total = number(),
            "images_used" => r.images_used = number(),
            "images_dev" => images_dev = value.to_string(),
            "cpus" => r.cpus = number(),
            "load1" => r.load1 = value.parse().unwrap_or(0.0),
            "load5" => r.load5 = value.parse().unwrap_or(0.0),
            _ => {}
        }
    }
    // Containers live *inside* `skein` once dockerd has been pointed at them, so `skein` counts them
    // and the two figures have to be separated rather than added — added, the strip would draw the
    // same memory twice and a bar of parts would exceed the whole it is drawn against.
    //
    // Chosen on whether the nested cgroup EXISTS, not on whether it holds anything: an empty one is
    // a fleet that has cycled and simply has no container running, and falling back then would put
    // the sandbox's own daemons under the `docker` label. `/sys/fs/cgroup/docker` is not a synonym
    // for the old home — after the move it holds only the sandbox's own container, which belongs to
    // the plumbing share and is counted in `other`.
    match nested {
        Some(containers) => {
            r.docker = containers;
            r.boxes = r.boxes.saturating_sub(containers);
        }
        None => r.docker = outside,
    }
    // sbx gives `/var/lib/docker` a disk of its own, but it does not have to: a sandbox built
    // without one has Docker on the same filesystem as the boxes, and drawing that as a second
    // gauge would show the same bytes twice under two names. Compared by device rather than by
    // path, which is the only comparison that answers "is this the same storage".
    // Both empty means neither `df` answered, which is not the two being the same device — the
    // figures are already zero there, and treating "unknown" as "matched" would be a coincidence
    // waiting to be relied on.
    if !images_dev.is_empty() && images_dev == root_dev {
        r.images_total = 0;
        r.images_used = 0;
    }
    (r.mem_total > 0).then_some(r)
}

/// See [`crate::util::Gate`]. Asked rarely and backed off hard: nothing depends on this answer, so it must
/// never be a reason the sandbox is busy.
pub(super) static RESOURCE_GATE: crate::util::Gate<FleetResources> = crate::util::Gate::new();

#[cfg(test)]
mod tests {
    use super::*;

    /// A box's CPU is a *rate*, and the cgroup only offers a running total.
    ///
    /// Reading `usage_usec` once and reporting it ranks boxes by how much CPU they have burned since
    /// they started, which puts yesterday's long build permanently at the top and never shows what
    /// is busy now. The difference between two samples over a known interval is the whole
    /// measurement.
    #[test]
    fn a_boxs_cpu_is_the_difference_between_two_samples() {
        // Half a second of wall clock; `busy` burns two full cores in it, `idle` none.
        // `b <name> <usage_usec> <pids> <anon> <memory.current>`. `busy` is charged 4 GiB and only
        // 1 GiB of it is anonymous — a box that has been compiling, which is the shape that made an
        // operator ask why a box was "using 15 GB".
        let out = "\
a busy 1000000
a idle 5000000
b busy 2000000 312 1073741824 4294967296
b idle 5000000 4 1048576 1048576
";
        let loads = parse_box_loads(out, 500_000.0);
        assert_eq!(loads.len(), 2);
        // Sorted by what you opened this to find out.
        assert_eq!(loads[0].name, "busy");
        assert_eq!(loads[0].cores, 2.0);
        assert_eq!(loads[0].pids, 312);
        assert_eq!(loads[1].name, "idle");
        assert_eq!(loads[1].cores, 0.0);
    }

    /// The parser against the script's real output, on real cgroups.
    ///
    /// Every other test here feeds `parse_box_loads` a string somebody typed, and the doc on it says
    /// it "is only correct against exactly the output above" — which nothing checked. The two drift
    /// the moment a field is added, and the failure is silent: fields shift and a box's page cache
    /// is reported as memory it holds. That is not hypothetical; it happened while this was written.
    ///
    /// Skipped where there are no box cgroups to sample, which is most machines — and it runs here,
    /// inside a fleet, where `/sys/fs/cgroup/skein` has one directory per box.
    #[test]
    fn the_load_script_and_its_parser_agree_on_real_cgroups() {
        if !std::path::Path::new("/sys/fs/cgroup/skein").is_dir() {
            crate::testutil::skip("no box cgroups on this machine to sample");
            return;
        }
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(BOX_LOAD_SCRIPT)
            .output()
            .expect("run the load script");
        let text = String::from_utf8_lossy(&out.stdout);
        let loads = parse_box_loads(&text, BOX_LOAD_INTERVAL_US);
        assert!(
            !loads.is_empty(),
            "the script produced nothing the parser recognised:\n{text}"
        );
        for load in &loads {
            assert!(!load.name.is_empty(), "a box with no name: {text}");
            // Every `b` line has six fields, so nothing shifted — which is what the `${v:-0}`
            // substitution buys and what a missing `memory.stat` would otherwise cost.
            let line = text
                .lines()
                .find(|l| l.starts_with(&format!("b {} ", load.name)))
                .unwrap_or_else(|| panic!("no sample line for {}: {text}", load.name));
            assert_eq!(
                line.split_whitespace().count(),
                6,
                "a sample line is not six fields, so the parser is reading one out of step: {line}"
            );
            // The relationship that makes the split meaningful: `anon` is part of the charge, never
            // more than it. If this ever inverts, the two fields have been read in the wrong order.
            assert!(
                load.mem_anon <= load.mem_anon + load.mem_cache,
                "{} holds more than it is charged: {line}",
                load.name
            );
        }
    }

    /// And the cockpit renders the one that means something.
    ///
    /// A field that is split in the API and rendered as the old total in the page is the same bug
    /// with an extra step, and it is invisible from the Rust side — which is exactly how a figure
    /// stops meaning what its name says.
    #[test]
    fn the_cockpit_shows_what_a_box_holds_rather_than_what_it_is_charged() {
        let page = include_str!("../web/index.html");
        assert!(
            page.contains("r.mem_anon"),
            "the page still renders the charge, which counts page cache"
        );
        assert!(
            !page.contains("fmtGB(r.mem)"),
            "the old total is still on screen somewhere"
        );
        assert!(
            page.contains("r.mem_cache"),
            "the cache is not shown at all, so a box whose two figures differ by an order of \
             magnitude explains itself to nobody"
        );
    }

    /// The headline is the memory a box cannot give back, and the rest is shown as what it is.
    ///
    /// The two are not interchangeable and this is where they stop being one number. A box charged
    /// 4 GiB of which 1 GiB is anonymous is a box holding 1 GiB — the other three are page cache the
    /// kernel hands back the moment anything asks. Reported as one figure, the same box reads as
    /// four times the problem it is, and the fleet gauge already stopped doing that for exactly this
    /// reason (see `FleetResources`).
    #[test]
    fn a_boxs_memory_is_what_it_holds_and_what_it_is_merely_charged_for() {
        let loads = parse_box_loads("b busy 0 9 1073741824 4294967296\n", 500_000.0);
        assert_eq!(loads[0].mem_anon, 1_073_741_824, "the part an OOM turns on");
        assert_eq!(
            loads[0].mem_cache,
            3 * 1_073_741_824,
            "the part the kernel reclaims, reported separately rather than added to the headline"
        );

        // A cgroup that answers for `memory.current` and not for `anon` — an older kernel, or a
        // read that raced a box being destroyed. Zero anonymous and the whole charge as cache is
        // wrong in the safe direction: it under-reports a box's hold rather than inventing one.
        let partial = parse_box_loads("b odd 0 9 0 4294967296\n", 500_000.0);
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].mem_anon, 0);
        assert_eq!(partial[0].mem_cache, 4_294_967_296);

        // And the shape that made this a real bug rather than a hypothetical: a missing value left
        // an empty field, `split_whitespace` closed the gap, and `memory.current` slid into `anon` —
        // reporting a box's page cache as memory it was holding. The script substitutes zero, so
        // the line can never be short, and a line that IS short is not read as a box.
        // **Every** substitution in the sample line goes through a helper, not just one of them.
        // The first version of this asserted the helper merely existed, which passed while the
        // `anon` read was reverted to a bare `awk` — the exact shift it was written to prevent.
        // The helpers themselves read directly, of course — it is the *sampling loops* that must
        // not, so the check starts after the definitions.
        let sampling = BOX_LOAD_SCRIPT
            .split_once("cd /sys/fs/cgroup/skein")
            .expect("the script still samples the box cgroups")
            .1;
        for bare in ["$(cat ", "$(awk "] {
            assert!(
                !sampling.contains(bare),
                "`{bare}` reads a field directly, and an empty result shifts every field after it \
                 — put it behind `n` or `k`, which substitute zero"
            );
        }
        assert!(BOX_LOAD_SCRIPT.contains("${v:-0}"));
        assert!(
            parse_box_loads("b odd 0 9 4294967296\n", 500_000.0).is_empty(),
            "a short line must be dropped rather than read one field out of step"
        );
    }

    /// A box that appears between the two passes has no baseline, and must not be handed one.
    ///
    /// Treating a missing first sample as zero would subtract from it — reporting a box's entire
    /// lifetime of CPU as if it had all happened in half a second, which is both enormous and
    /// exactly the box that just started doing nothing.
    #[test]
    fn a_box_that_arrives_mid_measurement_is_reported_at_zero() {
        let loads = parse_box_loads("b newcomer 900000000 3 1048576 1048576\n", 500_000.0);
        assert_eq!(loads.len(), 1);
        assert_eq!(loads[0].cores, 0.0, "not 1800 cores");
    }

    /// Verbatim output of [`resource_script`] on a live fleet, so the parser is tested against what
    /// the guest actually prints rather than against what this file assumes it prints.
    const LIVE_REPLY: &str = "mem_total 26377\nmem_used 11215\ncpus 11\nload1 6.89\nload5 5.48\n\
                              disk_dev overlay\ndisk_total 60168\ndisk_used 20986\n\
                              images_dev /dev/vdd\nimages_total 50089\nimages_used 45433\n\
                              skein 1440\ndocker 8880\n";

    #[test]
    fn the_gauge_reads_a_live_reply_and_the_shares_fit_inside_the_total() {
        let r = parse_resources(LIVE_REPLY).expect("a reply with a memory total is a reply");
        assert_eq!((r.mem_total, r.mem_used), (26377, 11215));
        assert_eq!((r.boxes, r.docker), (1440, 8880));
        assert_eq!((r.disk_total, r.disk_used, r.cpus), (60168, 20986, 11));
        assert_eq!((r.load1, r.load5), (6.89, 5.48));
        // Docker's own disk, which the strip did not draw at all until this: the reply above is a
        // fleet whose boxes' disk is a third full while the one Docker writes to is at 91%. A build
        // that ran out of space there did so against a gauge showing two thirds free.
        assert_eq!((r.images_total, r.images_used), (50089, 45433));
        // The stacked memory bar draws boxes + docker + everything-else against the total, so a
        // reading where the parts exceed the whole is one that renders as a bar past its own end.
        // This is exactly what `memory.current` produced — 11.0 GB and 10.8 GB against 2.9 GB used —
        // and the reason those two figures are `anon` from `memory.stat` instead.
        assert!(
            r.boxes + r.docker <= r.mem_used,
            "the cgroups' share must fit inside what the VM is using: \
             {} + {} against {}",
            r.boxes,
            r.docker,
            r.mem_used
        );
    }

    /// Two disks or one, drawn honestly either way.
    ///
    /// sbx gives a sandbox a root filesystem and a separate `/var/lib/docker`, sized by two
    /// different create-time variables — so filling one says nothing about the other, and a single
    /// disk gauge answered the wrong question confidently. But it does not *have* to be two: a
    /// sandbox built without the second has Docker on the boxes' own filesystem, and drawing that
    /// as a second gauge would show the same bytes twice under two names. Told apart by device,
    /// which is the only comparison that answers "is this the same storage".
    #[test]
    fn dockers_disk_is_drawn_when_it_is_its_own_and_never_drawn_twice() {
        let head = "mem_total 26377\nmem_used 900\n";

        let two = parse_resources(&format!(
            "{head}disk_dev overlay\ndisk_total 60168\ndisk_used 20986\n\
             images_dev /dev/vdd\nimages_total 50089\nimages_used 45433\n"
        ))
        .unwrap();
        assert_eq!((two.disk_used, two.images_used), (20986, 45433));

        // One filesystem answering both questions: the boxes' gauge already counts these bytes.
        let one = parse_resources(&format!(
            "{head}disk_dev overlay\ndisk_total 60168\ndisk_used 20986\n\
             images_dev overlay\nimages_total 60168\nimages_used 20986\n"
        ))
        .unwrap();
        assert_eq!(one.disk_used, 20986);
        assert_eq!(
            (one.images_total, one.images_used),
            (0, 0),
            "a zero denominator is how the strip drops a row, which is what one disk should draw"
        );
    }

    /// Once dockerd is pointed at [`CONTAINER_CGROUP`], the containers are counted *inside* `skein`
    /// — so reading both cgroups and adding them would draw the same memory twice and put the parts
    /// of the stacked bar past the whole they are drawn against, which is the exact fault the
    /// `anon`-instead-of-`current` fix was for.
    ///
    /// Both layouts are live at once during a migration, because the setting only takes effect at
    /// the next dockerd start: a fleet that has not cycled still has its containers in `/docker`.
    /// So the choice is made on whether the nested cgroup EXISTS, not on whether it holds anything.
    /// An empty one means a fleet that has cycled and has no container running — falling back then
    /// would label the sandbox's own daemons, which is all that is left in `/docker`, as Docker.
    #[test]
    fn containers_are_counted_once_wherever_dockerd_has_put_them() {
        let head = "mem_total 26377\nmem_used 2947\n";

        // Cycled: `skein` is boxes AND containers, and the nested figure separates them.
        let moved = parse_resources(&format!(
            "{head}skein 2100\nskein/containers 855\ndocker 106\n"
        ))
        .unwrap();
        assert_eq!(
            (moved.boxes, moved.docker),
            (1245, 855),
            "the containers' share belongs to them, not to the boxes that started them"
        );
        assert!(
            moved.boxes + moved.docker <= moved.mem_used,
            "counted twice, the parts of the bar exceed the whole"
        );

        // Cycled, nothing running: the empty nested cgroup is still the answer. Falling back here
        // would report the sandbox's own init and dockerd — all `/docker` holds now — as Docker.
        let idle = parse_resources(&format!(
            "{head}skein 1252\nskein/containers 0\ndocker 106\n"
        ))
        .unwrap();
        assert_eq!((idle.boxes, idle.docker), (1252, 0));

        // Not yet cycled: no nested cgroup at all, so the old home is where they still are.
        let legacy = parse_resources(&format!("{head}skein 1252\ndocker 855\n")).unwrap();
        assert_eq!((legacy.boxes, legacy.docker), (1252, 855));
    }

    #[test]
    fn a_reply_that_names_no_memory_is_not_remembered_as_a_zeroed_machine() {
        // A sandbox mid-boot answers `sbx exec` successfully and prints nothing useful. Taking that
        // as an answer would park a machine of zero bytes behind the Gate for the next 30 seconds.
        assert!(parse_resources("").is_none());
        assert!(parse_resources("cpus 8\nload1 0.10\n").is_none());
        // Missing pieces of a real reply are fine — a fleet root on a filesystem `df` cannot see
        // loses the disk gauge, not the memory one.
        let partial = parse_resources("mem_total 4096\nmem_used 900\n").unwrap();
        assert_eq!((partial.mem_total, partial.disk_total), (4096, 0));
    }
}
