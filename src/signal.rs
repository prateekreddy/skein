//! Signal — what an observation costs, and in which budget (architecture §2.2 and §10).
//!
//! **This is not `signals.rs`.** That module *computes* observations — it reads a pane, classifies a
//! screen, fuses a level with an edge. This one is the primitive's **declaration**: §2.2 says every
//! signal declares six things and that "the declaration is part of its definition, not
//! documentation", and cost is the one of the six that nothing in the code carried at all. The two
//! will meet when `signals.rs` is rewritten against §14's module; until then a declaration that
//! stands beside the code is worth more than a paragraph that stands beside neither.
//!
//! **Cost is not one number.** §10 gives five budgets with five units, and the board refresh is the
//! one with teeth: it runs for every open browser tab every two seconds, so *a signal that forks a
//! process is a different kind of thing from one that reads a file*. This module makes that
//! difference a number rather than a sentence.
//!
//! **What is declared here is what can be counted.** Forks, GitHub API units, model cents — each is
//! an integer a test can measure. Core-fraction and writes/s are §10's budgets too and they are
//! deliberately absent: they are the *observer's*, spent inside a box on its own cadence, and
//! nothing on the board tick spends them. Declaring a number nobody measured is the failure mode
//! this project has a rule about.
//!
//! **Every cost carries its basis**, and an empty one fails a test. A cost with no basis is an
//! assertion wearing a struct.
//!
//! **Which Source produced a signal** (§2.2's third facet) is [`Signal::sources`], and settling it
//! meant settling whether §2.3 was short a primitive. It was not, and the design already said so:
//! §13a puts "every `sbx exec` path" and "sandbox listing as the truth about boxes" on the delete
//! list, and says they survive only until skein moves into the fleet. `sbx exec` is the **transport**
//! around a reach, not a reach — so the liveness sweep is `file` and `socket` today and will still
//! be `file` and `socket` when the shell around it is gone.
//!
//! It is a **list**, not one Source, and that is not generality for its own sake: the liveness sweep
//! reads `/proc` for every anchored box and falls back to each undecided box's tmux socket, so which
//! one answered is per box and per tick. §2.2 says "which Source produced it", singular — right for
//! an observation, wrong for a declaration. The one that actually answered belongs on the
//! observation, beside `observed_at`.
//!
//! Depends on nothing, like `source`. A budget that had to ask `config` how much it could spend
//! would be a budget the thing being measured gets to set.

use std::fmt;
use std::time::Duration;

/// §10's budgets, each with its own unit and its own enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// The observer's core-fraction. Measured, not asserted: 0.11% of a core (§10).
    Cpu,
    /// Milliseconds per board refresh. Gates, single-flight, backoff.
    WallClock,
    /// GitHub API units. Exhaustion is a 403 with a reset time, not a slowdown.
    GitHub,
    /// Dollars. AI summaries, narrate, the resume safety gate.
    Model,
    /// Writes per second against the mounted volume.
    VolumeIo,
    /// boxes × transitions × clients — the event stream (§10.1).
    Delivery,
}

impl Budget {
    pub const ALL: [Budget; 6] = [
        Budget::Cpu,
        Budget::WallClock,
        Budget::GitHub,
        Budget::Model,
        Budget::VolumeIo,
        Budget::Delivery,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Budget::Cpu => "CPU",
            Budget::WallClock => "wall-clock",
            Budget::GitHub => "GitHub",
            Budget::Model => "model",
            Budget::VolumeIo => "volume I/O",
            Budget::Delivery => "delivery",
        }
    }

    /// §10's second column, kept here so the code and the design can be compared rather than
    /// believed. The test below reads the document and fails on either drifting.
    pub fn unit(self) -> &'static str {
        match self {
            Budget::Cpu => "core-fraction",
            Budget::WallClock => "ms per board refresh",
            Budget::GitHub => "API units",
            Budget::Model => "dollars",
            Budget::VolumeIo => "writes/s",
            Budget::Delivery => "boxes × transitions × clients",
        }
    }
}

impl fmt::Display for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What one observation spends, before any gate amortises it.
///
/// Three counters rather than five budgets, because these are the three a test can count. A signal
/// that spends nothing in all three is not undeclared — it is a file read, and saying so is the
/// distinction §10 asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    /// Processes forked per observation — the wall-clock budget, and the CPU one.
    ///
    /// A fork is the unit because it is what a board tick actually spends and what a test can
    /// count: milliseconds vary by machine, and a spawn is a spawn everywhere.
    pub spawns: u32,
    /// GitHub API units per observation.
    pub github_units: u32,
    /// Model spend per observation, in cents. An integer because a signal that costs a fraction of
    /// a cent per observation and runs every two seconds per tab is not a rounding error, and this
    /// makes anyone adding one write down a number they have to defend.
    pub model_cents: u32,
    /// How the numbers above are known — a file and line, or the measurement.
    ///
    /// **An empty basis fails a test.** §10 opens with "measured, not asserted", and this field is
    /// that sentence made into something that can fail.
    pub basis: &'static str,
}

impl Cost {
    /// Spends nothing a board tick has teeth in: no fork, no API unit, no model call.
    pub const fn free(basis: &'static str) -> Cost {
        Cost {
            spawns: 0,
            github_units: 0,
            model_cents: 0,
            basis,
        }
    }

    pub const fn forks(spawns: u32, basis: &'static str) -> Cost {
        Cost {
            spawns,
            github_units: 0,
            model_cents: 0,
            basis,
        }
    }

    pub fn plus(self, other: Cost) -> Cost {
        Cost {
            spawns: self.spawns + other.spawns,
            github_units: self.github_units + other.github_units,
            model_cents: self.model_cents + other.model_cents,
            basis: "summed",
        }
    }

    pub fn times(self, n: u32) -> Cost {
        Cost {
            spawns: self.spawns * n,
            github_units: self.github_units * n,
            model_cents: self.model_cents * n,
            basis: self.basis,
        }
    }
}

/// What a signal is about. §2.2's four, and the keying rule is that a signal is keyed by this and
/// never by the observer — turn state keyed by box but written by session let any helper process
/// overwrite the agent's state, producing seventeen spurious `ended` events in 114 seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Box,
    Module,
    PullRequest,
    Fleet,
    /// The host, and everything on it — including fleets that are not this one.
    ///
    /// **A fifth, and it is not `Fleet` stretched.** "What fleets are on this machine" is a question
    /// somebody running more than one asks, and it cannot be about *a* fleet: the answer includes
    /// ones this skein does not own and could not reach. §2.2's four are all things skein manages;
    /// this one is the ground they stand on.
    ///
    /// Nothing on the board carries it — a machine's other fleets are not the board's business,
    /// which is exactly why `sbx ls` left the tick. It is here because a subject a signal can have
    /// and the type cannot name is a subject that arrives as an unclassified call.
    Machine,
}

/// How a signal's cost multiplies across a board tick.
///
/// The distinction the board lives or dies by. A per-pass observation answers for the whole fleet in
/// one go and costs the same at one box or fifty; a per-box one is paid once per row, per tick, per
/// open tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// Once per tick, whatever the fleet size.
    PerPass,
    /// Once per box per tick.
    PerBox,
}

/// Whether the gates are cold — the first tick after skein starts, or after an Act invalidated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gates {
    /// Nothing remembered: every gated signal pays its cost.
    Cold,
    /// Within every gate's freshness window: the gated signals cost nothing and the ungated ones
    /// cost exactly what they always do.
    Warm,
}

/// The signals one board tick observes.
///
/// Read off `board::load_views` rather than imagined, and the ones that fork are the ones this
/// enum exists for. The file-read signals are listed too, because "this costs nothing" is a claim
/// worth being able to check and worth having to write down.
///
/// **Adding a variant does not compile until it declares.** Every method below is an exhaustive
/// match with no wildcard, so a new signal must say what it is about, how it scales, what it
/// spends and how that is known. And a new signal that forks without being listed in
/// [`Signal::ON_THE_BOARD`] is caught by `tests/board_cost.rs`, which counts the real thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// What sandboxes are on this machine: `sbx ls --json`.
    ///
    /// **Not on the board any more.** It rode on every tick as the source of record for which boxes
    /// exist, which was true in the per-VM model and false in this one: a box is not a sandbox, and
    /// `sbx ls` has never heard of one. The placement records answer that question without a
    /// subprocess, so the listing became what it is actually good for — what *sandboxes* are here,
    /// including another skein fleet running beside this one — and that is a question somebody asks.
    ///
    /// Kept as a variant so its cost is still declared: it did not become free, it became rare.
    FleetListing,
    /// Every box's disk use, from one `du` over the fleet root.
    FleetDisk,
    /// Which boxes have a live session, from one sweep of the sandbox.
    FleetLiveness,
    /// The registry, which enriches what `sbx` cannot say.
    Registry,
    /// Where skein placed a box, and whether it placed it at all.
    BoxPlacement,
    /// The box's own screen, as its observer last read it.
    BoxScreen,
    /// The hook edges: turn state, outcome, detail.
    BoxStatusEdge,
    /// The narrative signal the launcher writes — the last message, and why it stopped.
    BoxNarrative,
    /// What the box is doing right now.
    BoxTask,
    /// The diffstat of the box's branch.
    BoxDiff,
    /// Whether the repo's tracker docs are behind the store's copies.
    BoxDocsUpdate,
    /// The box's branch, read from `HEAD` — the fallback when the registry, the launch spec and
    /// the repo all fail to say.
    BoxBranchFromHead,
}

impl Signal {
    /// Every signal a board tick observes.
    ///
    /// Kept in step with reality by measurement rather than by care: `tests/board_cost.rs` forks a
    /// counted tick and compares it with [`board_tick`], so a signal that forks and is missing from
    /// this list makes the sum wrong and the test says so.
    ///
    /// [`Signal::FleetListing`] is deliberately **not** here. It is a real signal with a real cost;
    /// it is simply not one a board tick observes any more.
    pub const ON_THE_BOARD: [Signal; 11] = [
        Signal::FleetDisk,
        Signal::FleetLiveness,
        Signal::Registry,
        Signal::BoxPlacement,
        Signal::BoxScreen,
        Signal::BoxStatusEdge,
        Signal::BoxNarrative,
        Signal::BoxTask,
        Signal::BoxDiff,
        Signal::BoxDocsUpdate,
        Signal::BoxBranchFromHead,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Signal::FleetListing => "fleet-listing",
            Signal::FleetDisk => "fleet-disk",
            Signal::FleetLiveness => "fleet-liveness",
            Signal::Registry => "registry",
            Signal::BoxPlacement => "box-placement",
            Signal::BoxScreen => "box-screen",
            Signal::BoxStatusEdge => "box-status-edge",
            Signal::BoxNarrative => "box-narrative",
            Signal::BoxTask => "box-task",
            Signal::BoxDiff => "box-diff",
            Signal::BoxDocsUpdate => "box-docs-update",
            Signal::BoxBranchFromHead => "box-branch-from-head",
        }
    }

    /// The §2.3 Sources this signal may be produced by.
    ///
    /// **`sbx exec` is not among them and never will be.** It is how the host reaches the *sandbox*
    /// — the outer shell of a reach rather than a reach — which `docs/sources.toml` says outright
    /// and §13a schedules for deletion. Declaring `Enter` or `File` *for the shell* would put a lie
    /// in the one place the design is checkable; declaring what the script inside it actually
    /// touches is the truth both today and after the move.
    ///
    /// [`Signal::FleetListing`] is the exception and returns nothing, because nothing in §2.3
    /// reaches a sandbox *manager*. That is exactly the signal §13a deletes — and §8.3 has already
    /// decided what replaces it: a `Source: http` call to the warden. A test keeps the exception
    /// honest by requiring every signal that IS on the board to name one.
    pub fn sources(self) -> &'static [crate::source::Source] {
        use crate::source::Source::{File, Socket};
        match self {
            // Asks the sandbox manager. No §2.3 Source reaches that, and none should be invented:
            // when the observation endpoint replaces it (§8.3) this becomes `http`.
            Signal::FleetListing => &[],
            // A `du` over the fleet root. The volume, so `file`.
            Signal::FleetDisk => &[File],
            // `/proc/<pid>/stat` for every anchored box, then `tmux -S <sock> has-session` for the
            // ones the anchors could not decide — which is per box and per tick, and is why this
            // returns two.
            Signal::FleetLiveness => &[File, Socket],
            Signal::Registry
            | Signal::BoxPlacement
            | Signal::BoxScreen
            | Signal::BoxStatusEdge
            | Signal::BoxNarrative
            | Signal::BoxTask
            | Signal::BoxDiff
            | Signal::BoxDocsUpdate
            | Signal::BoxBranchFromHead => &[File],
        }
    }

    pub fn subject(self) -> Subject {
        match self {
            Signal::FleetListing | Signal::FleetDisk | Signal::FleetLiveness | Signal::Registry => {
                Subject::Fleet
            }
            Signal::BoxPlacement
            | Signal::BoxScreen
            | Signal::BoxStatusEdge
            | Signal::BoxNarrative
            | Signal::BoxTask
            | Signal::BoxDiff
            | Signal::BoxDocsUpdate
            | Signal::BoxBranchFromHead => Subject::Box,
        }
    }

    /// Whether this is paid once per tick or once per row.
    ///
    /// Three of the fleet-subject signals answer for every box in one call, which is why a
    /// twelve-box fleet forks exactly as many processes as a one-box fleet.
    pub fn scale(self) -> Scale {
        match self {
            Signal::FleetListing | Signal::FleetDisk | Signal::FleetLiveness | Signal::Registry => {
                Scale::PerPass
            }
            Signal::BoxPlacement
            | Signal::BoxScreen
            | Signal::BoxStatusEdge
            | Signal::BoxNarrative
            | Signal::BoxTask
            | Signal::BoxDiff
            | Signal::BoxDocsUpdate
            | Signal::BoxBranchFromHead => Scale::PerBox,
        }
    }

    /// The gate that mediates this observation, and how long its answer stays fresh.
    ///
    /// `None` is not "no cadence" — it is **ungated**, which for a per-box signal means the cost is
    /// paid on every tick for every row. That combination is worth being able to see in one place:
    /// [`Signal::BoxBranchFromHead`] was the only signal on the board that both forked and was
    /// ungated, it was invisible until this table existed, and it does not fork any more. Nothing
    /// on the board is in that quadrant now, and a test keeps it that way.
    pub fn gate(self) -> Option<Duration> {
        match self {
            // Gated still, because two people asking at once should not be two subprocesses — but
            // nothing asks on a schedule any more, so the gate is a courtesy rather than a budget.
            Signal::FleetListing => Some(Duration::from_millis(1500)),
            Signal::FleetLiveness => Some(Duration::from_millis(1500)),
            Signal::FleetDisk => Some(Duration::from_secs(30)),
            Signal::Registry
            | Signal::BoxPlacement
            | Signal::BoxScreen
            | Signal::BoxStatusEdge
            | Signal::BoxNarrative
            | Signal::BoxTask
            | Signal::BoxDiff
            | Signal::BoxDocsUpdate
            | Signal::BoxBranchFromHead => None,
        }
    }

    /// What one observation spends, and how that is known.
    ///
    /// Every basis names a file and line or the measurement it came from. The spawn counts are the
    /// measured ones — `tests/board_cost.rs` runs a tick with a counting `PATH` and compares.
    pub fn cost(self) -> Cost {
        match self {
            Signal::FleetListing => Cost::forks(
                1,
                "`sbx ls --json`, src/sbx.rs `fleet_boxes` — asked by `board::foreign_views` when \
                 somebody wants it, and by nothing on a tick",
            ),
            Signal::FleetDisk => {
                Cost::forks(1, "one `sbx exec` du, src/fleet.rs `fleet_disk_usage`")
            }
            Signal::FleetLiveness => {
                Cost::forks(1, "one `sbx exec` sweep, src/fleet.rs `fleet_liveness`")
            }
            Signal::Registry => Cost::free("one JSON file, src/registry.rs `all_sandboxes`"),
            Signal::BoxPlacement => {
                Cost::free("one JSON file per box, src/place.rs `placed_boxes`")
            }
            Signal::BoxScreen => Cost::free("the observer's file, src/signals.rs `read_pane_raw`"),
            Signal::BoxStatusEdge => Cost::free("the hook's file, src/signals.rs `status_edge`"),
            Signal::BoxNarrative => {
                Cost::free("the launcher's file, src/signals.rs `session_signal`")
            }
            Signal::BoxTask => Cost::free("the task file, src/signals.rs `current_task`"),
            Signal::BoxDiff => Cost::free("the diffstat file, src/diff.rs `read_diffstat_file`"),
            Signal::BoxDocsUpdate => {
                Cost::free("two files in the store, src/tracking.rs `sync_docs_available`")
            }
            Signal::BoxBranchFromHead => Cost::free(
                "`.git/HEAD` as a file, src/sbx.rs `git_branch_for` — it forked a `git rev-parse` \
                 per box per tick until the count in `tests/board_cost.rs` made that visible",
            ),
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What one board tick spends, summed from what its signals declare.
///
/// The number §10 says nobody had computed. `boxes` is how many rows the board is showing;
/// `unresolved_branches` is how many of them nothing but git can name, because that is the one
/// per-box fork and pretending it is always paid or never paid would both be wrong.
///
/// Warm means every gate is inside its freshness window. It is the honest steady state — the board
/// ticks every two seconds and the two 1.5s gates do not cover that, so a real fleet alternates —
/// but the *shape* is what matters: warm, the whole fleet costs whatever its unresolved branches
/// cost, and nothing else.
pub fn board_tick(boxes: u32, unresolved_branches: u32, gates: Gates) -> Cost {
    let mut total = Cost::free("summed from Signal::ON_THE_BOARD");
    for signal in Signal::ON_THE_BOARD {
        if gates == Gates::Warm && signal.gate().is_some() {
            continue;
        }
        let times = match (signal, signal.scale()) {
            (Signal::BoxBranchFromHead, _) => unresolved_branches,
            (_, Scale::PerBox) => boxes,
            (_, Scale::PerPass) => 1,
        };
        total = total.plus(signal.cost().times(times));
    }
    Cost {
        basis: "summed from Signal::ON_THE_BOARD",
        ..total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §10's budget table is the one people read; this enum is the one that runs. Two tables that
    /// mean the same thing, in two files, is the shape that drifts — so the test reads the design.
    #[test]
    fn the_budgets_here_are_the_budgets_the_architecture_names() {
        let doc = include_str!("../docs/architecture.md");
        let section = doc
            .split_once("## 10. Budgets")
            .expect("§10 is where the budget table lives")
            .1;
        let header = section
            .find("| budget | unit | note |")
            .expect("§10's table moved or was reworded");
        let rows: Vec<&str> = section[header..]
            .lines()
            .skip(2) // the header and its separator
            .take_while(|l| l.starts_with('|'))
            .collect();
        assert_eq!(rows.len(), Budget::ALL.len(), "rows: {rows:?}");

        let mut seen = Vec::new();
        for row in rows {
            let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
            let budget = Budget::ALL
                .into_iter()
                .find(|b| b.name() == cells[0])
                .unwrap_or_else(|| {
                    panic!(
                        "the design names a budget this code does not have: {:?}",
                        cells[0]
                    )
                });
            assert_eq!(
                budget.unit(),
                cells[1],
                "{budget} is measured in {:?} here and {:?} in the design",
                budget.unit(),
                cells[1]
            );
            seen.push(budget);
        }
        for budget in Budget::ALL {
            assert!(
                seen.contains(&budget),
                "{budget} is not in the design's table"
            );
        }
    }

    /// §10 opens with "measured, not asserted". A cost with no basis is an assertion in a struct,
    /// and this is the line that stops one being added.
    #[test]
    fn every_signal_says_how_its_cost_is_known() {
        for signal in Signal::ON_THE_BOARD {
            let cost = signal.cost();
            assert!(
                !cost.basis.trim().is_empty(),
                "{signal} declares a cost with no basis — say where the number comes from"
            );
            // A number without a file, a command or a measurement behind it is the thing the basis
            // exists to prevent, so the basis has to point at something.
            assert!(
                cost.basis.contains(".rs") || cost.basis.contains('`'),
                "{signal}'s basis names neither a file nor a command: {:?}",
                cost.basis
            );
        }
    }

    /// The property the whole fleet's affordability rests on: three of the four fleet-subject
    /// signals answer for every box in one call, so growing the fleet does not grow the tick.
    #[test]
    fn a_bigger_fleet_does_not_cost_a_bigger_tick() {
        let one = board_tick(1, 0, Gates::Cold);
        let fifty = board_tick(50, 0, Gates::Cold);
        assert_eq!(
            one.spawns, fifty.spawns,
            "a fifty-box board forks more than a one-box board, which is the budget with teeth"
        );
        assert_eq!(
            one.spawns, 2,
            "the cold tick's two: the disk walk and the liveness sweep. It was three until `sbx ls` \
             left the tick — that one answers \"what sandboxes are on this machine\", which is a \
             question somebody asks rather than one the board asks thirty times a minute."
        );
        assert_eq!(
            board_tick(50, 0, Gates::Warm).spawns,
            0,
            "warm, a board tick that can name every branch forks nothing at all"
        );
    }

    /// **No signal on the board both forks and is ungated.**
    ///
    /// The quadrant that scales badly: a per-box observation with no gate is paid on every tick of
    /// every open tab, so a fork there is a fork per row per two seconds per browser tab. The branch
    /// fallback was the one signal in it, and reading `HEAD` as a file instead of asking `git`
    /// emptied it. `unresolved_branches` stays a parameter of `board_tick` because it is still the
    /// only per-box signal whose cost is conditional — it is now conditionally *nothing*.
    #[test]
    fn nothing_on_the_board_both_forks_and_is_ungated() {
        let ungated_forkers: Vec<Signal> = Signal::ON_THE_BOARD
            .into_iter()
            .filter(|s| s.gate().is_none() && s.cost().spawns > 0)
            .collect();
        assert!(
            ungated_forkers.is_empty(),
            "an ungated per-box fork joined the board tick — it needs a gate, or to stop forking: \
             {ungated_forkers:?}"
        );
        // Which is the whole of it: a fifty-box board whose branches nothing can name still forks
        // nothing once the gates are warm.
        assert_eq!(board_tick(50, 50, Gates::Warm).spawns, 0);
        assert_eq!(board_tick(50, 50, Gates::Cold).spawns, 2);
    }

    /// Every signal the board observes names a Source, and none of them names the transport.
    ///
    /// §2.2 requires the facet; §13a is why `sbx exec` is not an answer to it. The one signal with
    /// no Source is the one §13a deletes, and it is not on the board — which is the whole of the
    /// exception, and it is checked rather than described.
    #[test]
    fn every_signal_on_the_board_says_which_source_produced_it() {
        for signal in Signal::ON_THE_BOARD {
            assert!(
                !signal.sources().is_empty(),
                "{signal} is on the board and names no Source. If it is reached by `sbx exec`, that \
                 is the transport — declare what the script inside it touches (§13a)."
            );
            for source in signal.sources() {
                assert!(
                    crate::source::Source::ALL.contains(source),
                    "{signal} names a Source §2.3 does not have: {source}"
                );
            }
        }
        // The exception, stated as a fact rather than a comment: nothing in §2.3 reaches a sandbox
        // manager, and the signal that does is the one the rewrite removes.
        assert!(Signal::FleetListing.sources().is_empty());
        assert!(
            !Signal::ON_THE_BOARD.contains(&Signal::FleetListing),
            "the one signal with no Source is back on the board, where the law applies to it"
        );

        // The liveness sweep composes two, and a declaration that could only hold one would have to
        // pick — which is how a signal starts describing half of what it does.
        assert_eq!(
            Signal::FleetLiveness.sources(),
            &[crate::source::Source::File, crate::source::Source::Socket]
        );
    }

    /// Nothing on the board tick spends the two budgets that are not counted here, which is why
    /// they are not counted here. If one ever does, this fails and the counter has to be added.
    #[test]
    fn the_board_tick_calls_no_model_and_no_api() {
        let tick = board_tick(50, 50, Gates::Cold);
        assert_eq!(tick.github_units, 0, "the board tick reached GitHub");
        assert_eq!(tick.model_cents, 0, "the board tick called a model");
    }
}
