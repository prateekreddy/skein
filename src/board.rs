//! The fleet, enriched and sorted so the first row is the one that needs you.
//!
//! One projection, rendered by both surfaces: the CLI table and the cockpit board are the same
//! `BoxView` list, which is why they cannot disagree about what a box is doing.
//!
//! **The placements are the source of record, and the registry only enriches.** A box exists
//! because skein placed it in the fleet sandbox; the registry carries the one datum the placement
//! cannot — the agent's turn state. Getting that the wrong way round is how a board shows boxes
//! that were destroyed, and keeps showing them.
//!
//! `sbx ls` is asked nowhere here (SKEIN-484). A box is not a sandbox — `sbx ls` has never heard of
//! one — so the listing served only the per-VM model, at one subprocess every two seconds for every
//! open browser tab, which was the most expensive thing a board tick did. It reached the board only
//! when `fleet_sandbox` was blank, and since SKEIN-484 `load_config` will not hand anybody a blank
//! one: a fleet always has a name, so the placements always have a register to be.
//!
//! The order is "who needs me first" rather than alphabetical or newest-first, because a board that
//! is read at a glance answers exactly one question and the answer has to be the top row.

use crate::diff::{read_diffstat_file, DiffStat};
use crate::fleet::{box_disk_limit, fleet_disk_usage};
use crate::place::{fleet_sandbox, placed_boxes, shared_record};
use crate::registry::{all_sandboxes, Sandbox};
use crate::repos::{branch_from_box, launch_spec_agent, launch_spec_branch, repo_for_box};
use crate::runtime::{default_agent, valid_runtime};
use crate::sbx::{box_liveness, git_branch_for, Liveness};
use crate::signals::{
    classify_message, classify_pane, current_status_detail, current_task, fuse_status, hook_health,
    is_generic_wait, pane_usable, read_pane_raw, screen_health, session_signal, status_edge,
    title_activity, title_is_fresh, Pause, Screen,
};
use crate::tracking::sync_docs_available;
use crate::util::{first_line, shorten};
use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::path::Path;

/// The fleet, enriched and sorted "who needs me first" (tier asc, then name).
pub fn load_views() -> Result<Vec<BoxView>, String> {
    // **Which boxes exist is a question the placements answer, and `sbx ls` cannot** — see the
    // module note. `fleet` is never empty: `load_config` repairs a blank name, which is what makes
    // `placed_boxes` unconditional here rather than one arm of a fork.
    let fleet = fleet_sandbox();
    let reg = all_sandboxes();
    // skein-server may run *inside* one box; that box is provably up, so keep it live even when sbx
    // can't confirm it. Set $SKEIN_SELF to override the detected vmid.
    let self_box = env::var("SKEIN_SELF")
        .or_else(|_| env::var("SANDBOX_VM_ID"))
        .ok()
        .filter(|s| !s.is_empty());

    // Whichever register applies. In the fleet the placement records ARE it — a box exists because
    // skein placed it — and a registry entry for a destroyed box must not resurrect one, which is
    // why the registry is not consulted for existence here at all.
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.extend(placed_boxes(&fleet).into_iter().map(|(name, _)| name));
    // skein-server may run inside one box, which is provably up whatever any register says.
    if let Some(self_name) = &self_box {
        names.insert(self_name.clone());
    }
    // Last, and after the self-box: the sandbox that hosts the boxes is not one of them, and it
    // would otherwise appear as a box that is permanently stale with no repo and no branch. skein
    // running *in* the fleet sandbox is the case that reaches this by the other route.
    names.remove(&fleet);
    // Hoisted: this reads the config and the stored-credential list, and `load_views` runs on every
    // 2s tick for every box. Asked once per pass rather than once per box per pass.
    let scopable = crate::gitgate::can_issue_write_tokens();

    // One measurement for the whole board, not one per row: it is a single `du` in the sandbox, and
    // asking per box would be one round trip each for a number they all read from the same walk.
    let usage = fleet_disk_usage();

    let mut views: Vec<BoxView> = names
        .into_iter()
        .map(|name| {
            let r = reg.get(&name);
            // The registry's known-good directory. The `sbx ls` row used to stand in behind it and
            // no longer can: a placed box has no sandbox of its own for sbx to have a workspace for.
            let dir = r
                .map(|x| x.dir.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or_default();
            // The repo this box belongs to (if any), used for grouping + branch fallback.
            let repo = repo_for_box(&name);
            // Runtime resolution mirrors branch resolution: the launch spec preserves an explicit
            // per-box override, and the repo is the default. sbx used to lead this list with the
            // agent that created the box's own sandbox — a box does not have one.
            let agent = repo
                .as_ref()
                .and_then(|rp| launch_spec_agent(rp, &name))
                .or_else(|| repo.as_ref().map(|rp| rp.agent.clone()))
                .filter(|a| valid_runtime(a))
                .unwrap_or_else(default_agent);
            // Branch resolution, most-authoritative first. Lifecycle probes refresh the per-store
            // registry after an in-box checkout. The launch spec is only the creation fallback; the
            // host clone is a different worktree (often on main) and must never override box state.
            let branch = r
                .map(|x| x.branch.clone())
                .filter(|b| !b.is_empty() && b != "?")
                .or_else(|| repo.as_ref().and_then(|rp| launch_spec_branch(rp, &name)))
                .or_else(|| repo.as_ref().map(|rp| branch_from_box(&name, rp)))
                .or_else(|| git_branch_for(&dir))
                .unwrap_or_default();
            // Reuse the registry-derived state logic; status (turn-state) is the registry's specific
            // datum, lastSeen is only a fallback when sbx liveness is absent.
            // Turn-state: the level observation of the box's own screen, fused with the hook edges
            // (docs/turn-state.md §4.3). With no observation this is exactly the edge signal, so a
            // box running an older probe behaves as it always did.
            //
            // Every reason `screen_health` can give for the screen half not contributing has to be
            // applied HERE too, or the row says "not reading the screen" in the badge and renders
            // the screen's verdict in the status anyway. This spelled out one of the three by hand
            // and so was missing the other two, which is why it is now `pane_usable` — the same
            // predicate `read_pane` applies, named once so the two cannot drift again:
            //   · `pane_is_ours` — the filename is a claim about whose screen this is, and until
            //     the probe wrote the box name into the observation it was one nothing could
            //     check. A misfiled observation classifies perfectly, which is what makes it bad.
            //   · `pane_is_readable` — PANE_CONTRACT's own doc says a newer observation "is
            //     treated as no observation, the board falls back to hook edges exactly as it does
            //     for a box with no observer". The board disclosed it and then classified it.
            // The raw observation is kept beside it because `screen_health` needs what was on disk
            // to say WHICH of the three refused it.
            let raw_pane = read_pane_raw(&name);
            let pane = raw_pane.clone().filter(|obs| pane_usable(obs, &name));
            let level = pane
                .as_ref()
                .map(|obs| (classify_pane(&agent, obs), obs.ts));
            let (fused, blocked_kind, status_from) = fuse_status(status_edge(&name), level.clone());
            let sb = Sandbox {
                branch: branch.clone(),
                dir: dir.clone(),
                last_seen: r.map(|x| x.last_seen.clone()).unwrap_or_default(),
                // the registry's own status remains the transitional fallback for unprobed boxes.
                status: fused
                    .or_else(|| r.map(|x| x.status.clone()))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_default(),
            };
            // Liveness through `box_liveness`, never straight off the `sbx ls` row — for a box in
            // the fleet the two disagree, and the row wins in the worst possible way.
            //
            // A migrated box's old sandbox is STOPPED, not destroyed (that is deliberate: it is the
            // undo). It keeps the box's name, so `sbx ls` still lists it, `s` is `Some`, and its
            // `live` is `Stopped` — which `state_with` turns into `stale`, discarding a perfectly
            // fresh `waiting` the box's own hooks just wrote. Every migrated box read `stale` on the
            // board while working, and the per-box session view — which already went through
            // `box_liveness` — disagreed with the board about the same box at the same moment.
            //
            // `box_liveness` knows a placed box's liveness is its tmux server in the shared sandbox,
            // and falls back to exactly this row for a box that is still its own VM.
            let live = box_liveness(&name);
            let (mut state, mut tier) = sb.state_with(live);
            // The cold-start demotion that used to sit here — heartbeat older than half an hour
            // reads as "stale" — was the per-VM path's only defence against an old outcome file
            // resurrecting a destroyed box. It went with `sbx ls` (SKEIN-484). In the fleet a box
            // exists because a placement says so, liveness comes from the sweep through
            // `box_liveness` below, and a quiet heartbeat means a box that is not talking rather
            // than one that is gone — so demoting on age here would have been wrong anyway.
            // Self-box stays live when sbx can't confirm it (e.g. skein running outside sbx).
            if live.is_none()
                && sb.status.is_empty()
                && self_box.as_deref() == Some(name.as_str())
                && tier > 3
            {
                state = "live".into();
                tier = 3;
            }
            // The narrative signal (box-session.sh): a cheap per-box file read, no model call.
            // Headline = the blocking prompt when waiting on you, else the gist of the last
            // message; pause classifies *why* it stopped so the inbox can rank and batch.
            let sig = session_signal(&name);
            let blocked = state == "needs-input";
            let signal_text = sig.as_ref().map(|s| {
                if s.kind == "notification" && !s.prompt.trim().is_empty() {
                    s.prompt.clone()
                } else {
                    s.last_message.clone()
                }
            });
            // The live "what's it doing now" signal (box-task.sh / journal `next`), with the
            // terminal title's activity text as a last resort — Claude Code writes the running tool
            // there ("Run bash command true"), which is fresher and free. Only while the box is
            // actually busy: the title keeps the finished tool's text, so on an idle box it lies.
            let task = current_task(&name).or_else(|| {
                let obs = pane.as_ref()?;
                // Busy *and* a title this observer watched change recently: the text is a live tool
                // description only under both conditions (see PaneObs::title_age). Claude only —
                // Codex's title is the working directory (`⠧ skein`), which names no activity, so
                // reading it as one would put the box's own folder name in the task column.
                //
                // `title_is_fresh` rather than the bound spelled out here: the glyph half of the
                // title is gated on the same predicate (`title_is_spinning`), and two spellings of
                // one rule is how the glyph came to have no gate at all (SKEIN-321).
                (agent == "claude"
                    && title_is_fresh(obs)
                    && level.as_ref().map(|(s, _)| s) == Some(&Screen::Busy))
                .then(|| title_activity(&obs.title))
                .flatten()
            });
            let mut headline = signal_text.as_deref().and_then(first_line);
            // When the signal is absent or just the generic "waiting for your input", surface the
            // current task instead — so even a tier-0 needs-input row says what it was working on.
            if headline.as_deref().is_none_or(is_generic_wait) {
                if let Some(t) = task.clone() {
                    headline = Some(t);
                }
            }
            // error/ended carry their own reason from the probe (error_type / end reason) — that's the
            // headline that matters for those rows, so it wins over the stale narrative/task signal.
            let is_outcome = state == "error" || state == "ended";
            if is_outcome {
                if let Some(d) = current_status_detail(&name) {
                    headline = Some(d);
                }
            }
            let pause = if tier == 3 || is_outcome {
                Pause::None // still working, or a terminal outcome whose pill speaks for itself
            } else {
                classify_message(signal_text.as_deref().unwrap_or(""), blocked)
            };
            // Hook-health: never-wired probes, a signal that turned out to be another box's, and
            // sessions still running an older box-side contract, told apart. This used to be
            // spelled out here while the refusal it discloses lived in `signals.rs`, which is
            // exactly the drift `pane_usable` was named to stop on the screen half: a signal could
            // be refused there and announced as healthy here. It is `hook_health` now, beside
            // `signal_is_ours`, so the disclosure cannot be one reason short of the refusal again.
            let hook_health = hook_health(&name, live == Some(Liveness::Running)).to_string();
            // The other half's health: a box can be perfectly wired for hooks and still be blind to
            // its own screen (no observer, an observer that stopped, a screen we can't parse), which
            // is invisible unless we say it.
            let screen = screen_health(
                &agent,
                &name,
                raw_pane.as_ref(),
                live == Some(Liveness::Running),
            );
            // Read once and asked twice below: whether skein placed this box at all, and which
            // cover it was placed under.
            let record = shared_record(&name);
            BoxView {
                name: name.clone(),
                state,
                tier,
                branch,
                age: sb.age(),
                age_secs: sb.age_secs(),
                dir: shorten(&dir),
                repo: repo.as_ref().map(|rp| rp.id.clone()).unwrap_or_default(),
                agent,
                diff: read_diffstat_file(&name),
                headline,
                task,
                pause,
                blocked_kind: blocked_kind.to_string(),
                hook_health,
                screen_health: screen.to_string(),
                status_from: status_from.key().to_string(),
                // Two file reads against the repo's store — no box is woken to answer this, which is
                // what makes it affordable on a signal computed for every row on every snapshot.
                docs_update: repo
                    .as_ref()
                    .is_some_and(|rp| sync_docs_available(Path::new(&rp.store))),
                // The placement record is the whole test: writing one is what makes a sandbox a box
                // skein owns. This used to be conditional on a fleet being configured, because an
                // unconfigured host gave every box its own VM and labelling all of them would have
                // marked the normal case as the odd one. There is no such host now.
                foreign: record.is_none(),
                // The same record again, and the same shape of question as `foreign` — who this box
                // belongs to — read off the placement record because that is where the intention was
                // written down. No record ⇒ not managed: a sandbox skein never placed is nobody's
                // job of skein's, whatever else it is.
                managed: record.as_ref().is_some_and(|rec| rec.purpose.managed()),
                // The same record, read once. A box carries the cover the launcher gave it at
                // start and nothing later changes that, so this is the only place the answer is.
                //
                // Only while it is running, and that is not a shortcut: a stopped box has no
                // namespace to be uncovered in, and starting it runs the current launcher. Saying
                // "older" of one would be asking somebody to restart a box that is already going to
                // get the current cover the moment it exists.
                // Same record, same tick. `uncapped …` is the launcher's own word for it and the
                // rest of the line is the reason; empty means no launcher answered, which is not
                // the same as capped and must not read as it.
                ceiling: match live == Some(Liveness::Running) {
                    false => String::new(),
                    true => record
                        .as_ref()
                        .map(|rec| rec.ceiling.clone())
                        .unwrap_or_default(),
                },
                cover: match live == Some(Liveness::Running) {
                    false => String::new(),
                    true => match record.as_ref() {
                        Some(rec) if crate::fleet::cover_is_current(&name, rec) => String::new(),
                        Some(_) | None => "older".to_string(),
                    },
                },
                scoped: scopable.then(|| crate::gitgate::box_is_scoped(&name)),
                disk_mb: usage.get(&name).copied(),
                disk_limit_mb: usage.get(&name).and(box_disk_limit(&name)),
            }
        })
        .collect();
    views.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then(a.pause.rank().cmp(&b.pause.rank()))
            .then(a.name.cmp(&b.name))
    });
    Ok(views)
}

/// A registry entry enriched for display — what the CLI table and the web API both render.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct BoxView {
    pub name: String,
    pub state: String,
    /// 0 waiting/done-attention, sorted up; higher = quieter. See [`Sandbox::state`].
    pub tier: u8,
    pub branch: String,
    pub age: String,
    /// The same age in seconds, when it is knowable.
    ///
    /// `age` is for reading and this is for comparing. The unified queue orders equally-urgent rows
    /// by how long they have been waiting, and a string like "2m" cannot be compared with a pull
    /// request's timestamp — so the number travels beside the words rather than being parsed back
    /// out of them.
    #[serde(default)]
    pub age_secs: Option<i64>,
    pub dir: String,
    /// the registered repo this box belongs to (`<repo>-<branch>`), empty if it matches none.
    /// Lets the cockpit group rows by repo once more than one is managed.
    #[serde(default)]
    pub repo: String,
    /// Runtime configured for this sandbox (`claude` or `codex`). The cockpit uses this as the
    /// default agent and offers the other runtime as a replacement-box takeover target.
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
    /// one-line gist of the box's last reported signal (the inbox headline) — the blocking
    /// prompt when it's waiting on you, else the first line of its last message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    /// what the box is doing *right now* — the in-progress TodoWrite item (or journal `next`).
    /// The peripheral "what's happening in the other tabs" signal; shown subtly on every row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// why the turn ended (the fork-detector) — lets the inbox label & batch the trivial asks.
    pub pause: Pause,
    /// which dialog is blocking, when the box's own screen says one is: `permission` | `question` |
    /// `trust` | `auth`. Empty when nothing blocks, or when no screen observation was available —
    /// each wants a different move from you, so the row names it instead of saying "decision".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub blocked_kind: String,
    /// probe wiring health, and the hook half's answer to [`screen_health`](crate::signals::screen_health):
    /// "" = fine; "never" = the sandbox is Running but no probe has EVER reported (no heartbeat, no
    /// status file) — hooks dark for this box; "misfiled" = a signal IS there under this box's name
    /// and says it is a different box's, so it was refused; "stale" = the session predates the
    /// installed probe contract. See [`crate::signals::hook_health`] for what each one asks of a
    /// person; the cockpit badges them.
    ///
    /// `misfiled` is a separate value rather than folded into `never` because the two are the same
    /// row and different jobs: `never` is a box to reattach, `misfiled` is a store to clean, and
    /// until this existed the second rendered as the first.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hook_health: String,
    /// the *other* half's health — whether this box's own screen is being read, and if not why:
    /// Where the displayed state came from: `screen`, `edge`, or `edge-ahead`. See
    /// [`crate::signals::StatusFrom`] — the last one is a healthy observer whose reading lost to a
    /// newer edge, which nothing disclosed before it existed.
    pub status_from: String,
    /// "" | "none" | "stale" | "unreadable" | "unsupported" | "newer" | "misfiled". See
    /// [`screen_health`]. Without it,
    /// falling back to hook-only turn state looks exactly like everything working.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub screen_health: String,
    /// this repo's store holds work-tracking documents newer than the ones installed from it, so a
    /// re-apply has something to deliver. Repo-scoped and host-side, because that half of the answer
    /// is free; whether *this box's* CLAUDE.md is stale can only be read inside the box, and is not
    /// worth waking one on every snapshot to find out. See [`sync_docs_available`].
    #[serde(default)]
    pub docs_update: bool,
    /// This sandbox is not a box skein placed — so skein can see it, and can do nothing with it.
    ///
    /// Two kinds land here and they need no telling apart: a sandbox someone created with `sbx`
    /// directly, and a box from a skein old enough to give every box its own microVM. Neither has a
    /// placement record, a store skein provisioned, or the tmux contract the cockpit attaches through.
    ///
    /// The board *hides* these by default and reveals them on the `foreign:` filter, because they are
    /// on the list only as an artefact of how the list is built: `sbx ls` is authoritative for which
    /// sandboxes exist, and it does not know which of them are skein's. Showing them made a first run
    /// on a machine with other sandboxes look like a fleet full of broken boxes.
    #[serde(default)]
    pub foreign: bool,
    /// skein started this box itself, to do a job of its own — see [`crate::place::Purpose`].
    ///
    /// **Grouped apart, and deliberately NOT hidden**, which is where this parts company with
    /// [`Self::foreign`] even though it is carried the same way. Foreign rows are hidden because
    /// they are on the list only as an artefact of how the list is built and nothing on the row
    /// works: no checkout, no store, no session to attach to. A managed box is the opposite of
    /// that on every count — it is skein's own box, in skein's own sandbox, spending skein's model
    /// calls, and it can get stuck or ask a question exactly like any other. Hiding it by default
    /// would mean a box burning tokens where nobody can see it, and the first sight of it would be
    /// the bill.
    ///
    /// So the board draws these in a section of their own and `managed:` *narrows* to them rather
    /// than revealing them. The grouping is the whole point: the owner asked for these "grouped
    /// separately from manual boxes", because a board where a person cannot tell at a glance which
    /// rows are their own work is a board that has stopped answering "what needs me".
    #[serde(default)]
    pub managed: bool,
    /// `"older"` when this box was started by a launcher that is not the one skein installs now,
    /// and empty when it was started by the current one.
    ///
    /// A box keeps the mount namespace it was born with, and everything skein does about isolation
    /// it does in `box-session.sh` at box start. So a launcher that gains a cover — `/run` going
    /// private, the fleet root going out of sight — reaches new boxes and no running one, and the
    /// running ones look exactly like the covered ones from every angle. That is what this says.
    ///
    /// The whole argument for a cover derived per box is that it cannot be forgotten; a fleet where
    /// half the boxes predate it is the state the mechanism was meant to make impossible to be in
    /// unknowingly. This is the part that makes it *known*, and no more: skein must not restart a
    /// box for it. A restart loses whatever the agent had half-finished, and the board's job is to
    /// surface what needs a person (architecture §11), not to act on their behalf.
    ///
    /// Empty means checked and current, not unchecked — a box with no placement record at all
    /// reads `"older"`, since nothing said otherwise and this is not a direction to guess in.
    #[serde(default)]
    pub cover: String,
    /// What bounds this box's memory — `capped <limits>`, `uncapped <reason>`, or empty for a box
    /// that is not running or whose launcher never said.
    ///
    /// On the row because it was nowhere else. The launcher records it in `limits.state` under the
    /// box's own root, which is inside the sandbox, so no surface skein has could read it — and an
    /// uncapped box looks identical to a capped one right up until a build in it takes the sandbox
    /// with it. `box-session.sh`'s own comment is the argument: the ceiling "keeps one box's
    /// runaway build from killing every other box in the sandbox."
    ///
    /// Three ways to be uncapped and they are not one problem. *no-limit-computed* is skein's own
    /// memory plan producing nothing for this box. *could-not-join-cgroup* and
    /// *no-cgroup-delegation* are the sandbox's answer, and need a different fleet rather than a
    /// different setting. The reason travels so the row does not have to guess which.
    #[serde(default)]
    pub ceiling: String,
    /// Whether this box's GitHub credential is scoped to its own repository — `None` when the fleet
    /// cannot scope at all, so there is no distinction to draw.
    ///
    /// On the row because it is otherwise invisible: a scoped box and an unscoped one look
    /// identical everywhere in the cockpit, so "did the switch take" had no answer short of trying
    /// a push inside the box and reading the 403. Three-valued for the same reason `legacy` is
    /// suppressed on an unadopted host — badging every row before the distinction exists would
    /// label the normal case as the odd one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped: Option<bool>,
    /// MiB this box occupies on the fleet's shared disk, and what it is allowed. Absent for a box
    /// with a sandbox of its own, whose disk is nobody else's problem.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_mb: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_limit_mb: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{load_config, save_config};
    use crate::place::{forget_place, record_place, PlaceRecord};
    use crate::repos::REPOS_CACHE;
    use crate::testutil::*;
    use std::fs;

    // A fleet box is not a sandbox, so `sbx ls` never mentions it. Until the placement records were
    // consulted here, a perfectly healthy fleet was simply absent from the board — and the sandbox
    // hosting it sat there instead, looking like a stale box nobody could attach to.
    #[test]
    fn the_board_shows_boxes_in_the_shared_sandbox_and_not_the_sandbox_itself() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(home.join("sandboxes.json"), "{}").unwrap();
        env::set_var("SKEIN_LS_CMD", "echo '[{\"name\":\"skein-fleet\"}]'");
        // **A fleet root of this test's own.** `load_views` measures every box's disk through
        // `fleet::fleet_disk_usage`, which unpinned walks `/boxes` — the owner's live fleet,
        // measured at 383,606 files — so every run of this test recursively stats somebody's
        // running work to answer a question about two names. Nothing here is placed under the
        // root, which is what these assertions already assume: none of the boxes below is running.
        env::set_var("SKEIN_FLEET_ROOT", home.join("boxes"));
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();
        record_place(
            "demo-task",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                home: "/home/agent".into(),
                tree: "/boxes/demo-task/tree".into(),
                sock: "/boxes/demo-task/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();

        let names: Vec<String> = load_views().unwrap().into_iter().map(|v| v.name).collect();
        assert!(
            names.iter().any(|n| n == "demo-task"),
            "a placed box must appear even though sbx has never heard of it: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "skein-fleet"),
            "the sandbox that hosts the boxes is not itself a box: {names:?}"
        );

        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// A box skein started for itself says so on its row — and is on the row at all.
    ///
    /// The walk this asserts is placement record → `Purpose` → `BoxView::managed` → the page, and
    /// every one of those joins is a place the flag can be dropped without anything going red. The
    /// end-to-end half fails if `load_views` stops consulting `record.purpose` (returning a constant
    /// `false`, or filtering managed rows out the way foreign ones are); the string half fails if
    /// the page stops reading the field or stops offering the term, which is how a tag silently
    /// stops appearing.
    ///
    /// Both boxes are placed, because a flag that is true for everything is the same as no flag: the
    /// manual box is here so the assertion is a distinction rather than a constant.
    #[test]
    fn a_box_skein_started_itself_is_marked_as_skeins_and_not_hidden() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(home.join("sandboxes.json"), "{}").unwrap();
        env::set_var("SKEIN_LS_CMD", "echo '[{\"name\":\"skein-fleet\"}]'");
        // A fleet root of this test's own, for the reason given in
        // `the_board_shows_boxes_in_the_shared_sandbox_and_not_the_sandbox_itself`: the board's
        // per-box disk figure is a walk of whatever root it resolves.
        env::set_var("SKEIN_FLEET_ROOT", home.join("boxes"));
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();
        for (name, purpose) in [
            ("demo-task", crate::place::Purpose::Manual),
            ("pr-review-7", crate::place::Purpose::Review),
        ] {
            record_place(
                name,
                &PlaceRecord {
                    sandbox: "skein-fleet".into(),
                    ns_pid: 1,
                    home: format!("/boxes/{name}/home"),
                    tree: format!("/boxes/{name}/tree"),
                    sock: format!("/boxes/{name}/session.sock"),
                    generation: "test-boot".into(),
                    ns_start: 1,
                    purpose,
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let rows: Vec<(String, bool, bool)> = load_views()
            .unwrap()
            .into_iter()
            .map(|v| (v.name, v.managed, v.foreign))
            .collect();
        assert!(
            rows.contains(&("pr-review-7".into(), true, false)),
            "the purpose in the record did not reach the row: {rows:?}"
        );
        assert!(
            rows.contains(&("demo-task".into(), false, false)),
            "a box a person made is the ordinary case and carries no tag: {rows:?}"
        );
        // The half that separates this from `foreign`. A managed box is skein's own, in skein's own
        // sandbox, spending skein's model calls — it arrives on the ordinary tick and is grouped,
        // never withheld. Were it hidden like a foreign row, the first sight of it would be the bill.
        assert_eq!(
            rows.len(),
            2,
            "a box skein started for itself was dropped from the board instead of grouped: {rows:?}"
        );

        // The joins nothing in the language checks. `foreign` has the same three and for the same
        // reason: a row that carries the flag and a page that ignores it look identical from here.
        let page = include_str!("web/index.html");
        assert!(
            page.contains("b.managed"),
            "the page no longer reads the flag, so skein's own boxes would draw as ordinary ones"
        );
        // The filter's own `title`, not the bare word: `managed: false` appears in the page as an
        // object literal, so matching `managed:` alone would pass on a page that had lost the hint
        // entirely — which is the state where the term works and nobody can find out that it does.
        assert!(
            page.contains("Type `managed:`"),
            "without the term in the filter's own help there is no way to discover it"
        );
        assert!(
            crate::cockpit::BUNDLE.contains("wantsManaged"),
            "the page offers `managed:` and the bundle no longer implements it, so typing it \
             would narrow to nothing and read as an empty fleet"
        );

        forget_place("demo-task");
        forget_place("pr-review-7");
        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    // The board has to say which boxes still cost a whole VM, because nothing else on the row does:
    // a legacy box and a fleet box look and behave identically right up until the host runs out of
    // memory to reserve. It marks the exception, not the norm — the shared sandbox is the default.
    #[test]
    fn the_board_marks_a_box_that_still_owns_a_whole_vm() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(home.join("sandboxes.json"), "{}").unwrap();
        env::set_var(
            "SKEIN_LS_CMD",
            r#"echo '[{"name":"skein-fleet"},{"name":"old-box"}]'"#,
        );
        // The same walk, the same fixture: see
        // `the_board_shows_boxes_in_the_shared_sandbox_and_not_the_sandbox_itself`.
        env::set_var("SKEIN_FLEET_ROOT", home.join("boxes"));
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();
        record_place(
            "demo-task",
            &PlaceRecord {
                sandbox: "skein-fleet".into(),
                ns_pid: 1,
                home: "/boxes/demo-task/home".into(),
                tree: "/boxes/demo-task/tree".into(),
                sock: "/boxes/demo-task/session.sock".into(),
                generation: "test-boot".into(),
                ns_start: 1,
                ..Default::default()
            },
        )
        .unwrap();

        let tagged = || -> Vec<(String, bool)> {
            load_views()
                .unwrap()
                .into_iter()
                .map(|v| (v.name, v.foreign))
                .collect()
        };
        let rows = tagged();
        // **The board tick no longer carries them**, which is the point: they cost one `sbx ls`
        // every two seconds per open tab, for the one part of the board that is not about skein's
        // own boxes.
        assert!(
            !rows.iter().any(|(name, _)| name == "old-box"),
            "a sandbox skein did not place rode in on the tick: {rows:?}"
        );
        assert!(
            rows.contains(&("demo-task".into(), false)),
            "a box skein placed is the ordinary case and carries no tag: {rows:?}"
        );
        // Asked for, they are there — as *sandboxes*. `docs/parity.md` §7 removes the foreign-box
        // display; what survives is "what is on this machine", which `machine::sandboxes` answers
        // and which says of each one whether it is a skein fleet rather than pretending it is a box.
        let seen = crate::machine::sandboxes().expect("sbx answers here");
        // In `sbx ls` order, which is the machine's — this is a listing rather than a ranking, and
        // sorting it would be inventing an opinion about somebody else's sandboxes.
        let mut names: Vec<&str> = seen.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["old-box", "skein-fleet"]);
        let ours = seen.iter().find(|s| s.name == "skein-fleet").unwrap();
        assert!(ours.ours && ours.skein_fleet);
        let stranger = seen.iter().find(|s| s.name == "old-box").unwrap();
        assert!(
            !stranger.ours && !stranger.skein_fleet,
            "a sandbox skein never placed a box in was called a skein fleet"
        );

        // The flag is only half the feature: a row that carries it and a cockpit that ignores it look
        // identical from here, and that is how a tag silently stops appearing. The cockpit hides
        // these rows by default, reveals them on `foreign:`, and — since they stopped arriving on
        // the tick — has to go and ask for them.
        let page = include_str!("web/index.html");
        assert!(
            page.contains("b.foreign"),
            "the cockpit no longer reads the flag, so foreign boxes would show as ordinary ones"
        );
        // The HINT, not the term. `foreign:` alone also matches the `foreign: true` object literal
        // in `asRow` a few hundred lines below, so this assertion passed on a page that had lost
        // the one sentence telling anybody the term exists — which for a row hidden by default is
        // the whole of the feature. Found while giving `managed:` the same assertion (SKEIN-484's
        // sibling), and the wording is matched to the placeholder rather than to the code.
        assert!(
            page.contains("Type `foreign:`"),
            "the page no longer offers `foreign:`, so rows hidden by default have no way to be \
             seen at all — and the term still appears in this file as an object key, which is what \
             let this assertion pass while the hint was gone"
        );
        assert!(
            page.contains("/api/machine/sandboxes"),
            "the rows no longer arrive on the tick, so a cockpit that does not ask for them shows \
             an empty list where a machine's other sandboxes should be"
        );

        forget_place("demo-task");
        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The gap SKEIN-88 was filed from, closed at the layer a person reads.
    ///
    /// A box gets its cover from `box-session.sh` at start and keeps the namespace it was born
    /// with. `install_launcher` refreshes that script at every start and every heal, so the copy in
    /// the sandbox always describes the NEXT box — and there is nothing on the host that says which
    /// running boxes predate it. The placement record is where that answer now lives, and this is
    /// the walk from the record to the row.
    #[test]
    fn a_box_started_under_an_older_launcher_says_so_on_its_row() {
        let _g = env_lock();
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        env::set_var("SKEIN_REGISTRY", home.join("sandboxes.json"));
        fs::write(home.join("sandboxes.json"), "{}").unwrap();
        env::set_var("SKEIN_LS_CMD", "echo '[{\"name\":\"skein-fleet\"}]'");
        let mut config = load_config();
        config.fleet_sandbox = "skein-fleet".into();
        save_config(&config).unwrap();

        // The liveness sweep, answered: this only applies to a RUNNING box, since a stopped one has
        // no namespace to be uncovered in and will get the current cover the moment it has one.
        //
        // It was answered by a fake `sbx` on `$PATH`, standing in for the sweep's `sbx exec` hop.
        // There is no hop (SKEIN-576), so the fake was bypassed and the sweep read the real
        // `/boxes` — this machine's live fleet, which has no `demo-task` in it (SKEIN-530). A
        // fleet root of its own and a real listening socket say "running" the way the sweep
        // actually asks: by being something that accepts on the box's socket.
        let root = home.join("boxes");
        env::set_var("SKEIN_FLEET_ROOT", &root);
        fs::create_dir_all(root.join("demo-task")).unwrap();
        let sock = root.join("demo-task/session.sock");
        let _listening = std::os::unix::net::UnixListener::bind(&sock).expect("a listener");

        let place = |launcher: &str| PlaceRecord {
            sandbox: "skein-fleet".into(),
            ns_pid: 1,
            home: "/boxes/demo-task/home".into(),
            tree: "/boxes/demo-task/tree".into(),
            sock: sock.to_string_lossy().into_owned(),
            generation: "test-boot".into(),
            ns_start: 1,
            launcher: launcher.to_string(),
            ..Default::default()
        };
        let cover_of = || -> String {
            crate::fleet::disturbing_liveness(|| ());
            load_views()
                .unwrap()
                .into_iter()
                .find(|v| v.name == "demo-task")
                .expect("the placed box is on the board")
                .cover
        };

        record_place("demo-task", &place(&crate::fleet::launcher_revision())).unwrap();
        assert_eq!(
            cover_of(),
            "",
            "a box started by this very launcher was asked to restart for a cover it already has"
        );

        record_place("demo-task", &place("0000000000000000")).unwrap();
        assert_eq!(
            cover_of(),
            "older",
            "a box born under a different launcher looked exactly like a covered one — which is \
             the whole defect: from the row, from inside the box, and from the sandbox, it does"
        );

        // The case that actually exists in the fleet today: every box placed before the record
        // carried this field at all. Silence is not agreement.
        record_place("demo-task", &place("")).unwrap();
        assert_eq!(
            cover_of(),
            "older",
            "a record too old to name a cover was read as naming the current one"
        );

        // **The switch that changes no byte of the launcher** (SKEIN-572).
        //
        // `peer_messaging` lives in `repos.json`, and flipping it leaves `box-session.sh` byte-for
        // byte identical — so `launcher_revision`, which hashes that file, cannot see it. A box
        // still running the mount it was born with therefore compared equal to the current cover
        // and nobody was ever asked to restart it, which is a switch that silently does nothing.
        //
        // **The launcher revision is deliberately the CURRENT one in both assertions below**, and
        // that is what makes this a test of the new field rather than of the comparison above it:
        // if `peers` were ignored, the first would still pass and the second would pass too, and
        // the feature would be indistinguishable from not working.
        crate::repos::save_repos(&[crate::repos::Repo {
            id: "demo".into(),
            peer_messaging: true,
            ..Default::default()
        }])
        .unwrap();
        let born_on_the_network = PlaceRecord {
            peers: Some(true),
            ..place(&crate::fleet::launcher_revision())
        };
        record_place("demo-task", &born_on_the_network).unwrap();
        assert_eq!(
            cover_of(),
            "",
            "a box born on the peer network, under this launcher, with the switch still on, was \
             asked to restart for a cover it already has"
        );

        // Flip it off for the repo. Same launcher, same revision, same running box — and the ONLY
        // honest answer is that this box is not running what the config now describes.
        crate::repos::set_peer_messaging("demo", false).unwrap();
        assert_eq!(
            cover_of(),
            "older",
            "the peer switch was flipped and the row said nothing, because `launcher_revision` \
             hashes a script the switch does not touch — so the box keeps the mount it was born \
             with and every surface reports it as current"
        );

        // And a launcher too old to say which side it was born on is left alone rather than being
        // read as either position: `None` is the third answer, not a quiet `false`.
        record_place(
            "demo-task",
            &PlaceRecord {
                peers: None,
                ..place(&crate::fleet::launcher_revision())
            },
        )
        .unwrap();
        assert_eq!(
            cover_of(),
            "",
            "a record from a launcher too old to report the peer switch was read as disagreeing \
             with it, which asks for a restart that would tell nobody anything new"
        );

        forget_place("demo-task");
        env::remove_var("SKEIN_FLEET_ROOT");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
    }
}
