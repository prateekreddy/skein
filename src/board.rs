//! The fleet, enriched and sorted so the first row is the one that needs you.
//!
//! One projection, rendered by both surfaces: the CLI table and the cockpit board are the same
//! `BoxView` list, which is why they cannot disagree about what a box is doing.
//!
//! **`sbx` is the source of record, and the registry only enriches.** A box exists because the
//! sandbox says so; the registry carries the one datum `sbx` cannot — the agent's turn state — and
//! stands in as a fallback when `sbx` cannot be reached. Getting that the wrong way round is how a
//! board shows boxes that were destroyed, and keeps showing them.
//!
//! The order is "who needs me first" rather than alphabetical or newest-first, because a board that
//! is read at a glance answers exactly one question and the answer has to be the top row.

use crate::diff::{read_diffstat_file, DiffStat};
use crate::fleet::{box_disk_limit, fleet_disk_usage};
use crate::place::{fleet_sandbox, placed_boxes, shared_record};
use crate::registry::{all_sandboxes, store_for_box, Sandbox};
use crate::repos::{branch_from_box, launch_spec_agent, launch_spec_branch, repo_for_box};
use crate::runtime::{default_agent, valid_runtime};
use crate::sbx::{box_liveness, fleet_boxes, git_branch_for, Liveness};
use crate::signals::{
    classify_message, classify_pane, current_status_detail, current_task, fuse_status,
    is_generic_wait, pane_is_fresh, probe_is_stale, read_pane_raw, screen_health, session_signal,
    status_edge, title_activity, Pause, Screen, TITLE_FRESH_SECS,
};
use crate::tracking::sync_docs_available;
use crate::util::{first_line, shorten};
use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::path::Path;

/// The fleet, enriched and sorted "who needs me first" (tier asc, then name).
pub fn load_views() -> Result<Vec<BoxView>, String> {
    // **Which boxes exist is a question the placements answer, and `sbx ls` cannot.** In the fleet
    // model a box is not a sandbox — `sbx ls` has never heard of one — so the listing survived here
    // only to serve the per-VM model that is being retired, at the cost of one subprocess every two
    // seconds for every open browser tab. It is the most expensive thing a board tick did.
    //
    // So it is asked only where it is still the source of record: a host with no fleet configured.
    // With one, `placed_boxes` is the register, and `sbx ls` becomes what it should always have
    // been — something a person asks when they want to know what sandboxes are on this machine.
    let fleet = fleet_sandbox();
    let in_fleet = !fleet.is_empty();
    let sbx = match in_fleet {
        true => None,
        false => fleet_boxes(),
    };
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
    if in_fleet {
        names.extend(placed_boxes(&fleet).into_iter().map(|(name, _)| name));
    } else {
        // The per-VM model, unchanged and on its way out. sbx is authoritative for which boxes
        // exist; the registry populates the board only when sbx cannot be consulted, so a destroyed
        // box whose registry entry lingers no longer shows up as a phantom once sbx confirms it is
        // gone.
        match &sbx {
            Some(v) => names.extend(v.iter().map(|b| b.name.clone())),
            None => names.extend(reg.keys().cloned()),
        }
    }
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
            let s = sbx.as_ref().and_then(|v| v.iter().find(|b| b.name == name));
            let r = reg.get(&name);
            // dir/branch: prefer the registry's known-good values (no regression for registered
            // boxes); fall back to sbx workspaces + host git for boxes the registry doesn't know.
            let dir = r
                .map(|x| x.dir.clone())
                .filter(|d| !d.is_empty())
                .or_else(|| s.map(|x| x.dir.clone()).filter(|d| !d.is_empty()))
                .unwrap_or_default();
            // The repo this box belongs to (if any), used for grouping + branch fallback.
            let repo = repo_for_box(&name);
            // Runtime resolution mirrors branch resolution: sbx knows what image/agent created the
            // box; the launch spec preserves an explicit per-box override; the repo is the default.
            let agent = s
                .map(|x| x.agent.clone())
                .filter(|a| valid_runtime(a))
                .or_else(|| repo.as_ref().and_then(|rp| launch_spec_agent(rp, &name)))
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
            let raw_pane = read_pane_raw(&name);
            let pane = raw_pane.clone().filter(pane_is_fresh);
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
            // Cold-start fallback has no authoritative existence/liveness signal. Old outcome files
            // must not resurrect destroyed boxes in "needs you": once the registry heartbeat is
            // stale (or absent), show the record as stale regardless of its sticky error/wait state.
            // Only on the per-VM path. In the fleet, liveness comes from the sweep and a box with
            // no recent heartbeat is a box that is not talking, not a box that is gone.
            if !in_fleet
                && sbx.is_none()
                && self_box.as_deref() != Some(name.as_str())
                && sb.age_secs().is_none_or(|seconds| seconds >= 30 * 60)
            {
                state = "stale".into();
                tier = 5;
            }
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
                let fresh = (0..=TITLE_FRESH_SECS).contains(&obs.title_age);
                (agent == "claude"
                    && fresh
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
            // Hook-health: distinguish never-wired probes from sessions still running an older
            // box-side contract. Silence alone is normal between lifecycle events; revision drift
            // is not, because the old session may emit a payload shape the new host misreads.
            let hook_health = if live == Some(Liveness::Running) {
                let store = store_for_box(&name);
                let dark = store.as_ref().is_none_or(|st| {
                    !st.join("hook-log").join(format!("{name}.jsonl")).exists()
                        && !st.join("status").join(format!("{name}.json")).exists()
                });
                if dark {
                    "never".to_string()
                } else if store.as_ref().is_some_and(|st| probe_is_stale(st, &name)) {
                    "stale".to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            // The other half's health: a box can be perfectly wired for hooks and still be blind to
            // its own screen (no observer, an observer that stopped, a screen we can't parse), which
            // is invisible unless we say it.
            let screen = screen_health(&agent, raw_pane.as_ref(), live == Some(Liveness::Running));
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
                foreign: shared_record(&name).is_none(),
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

/// The sandboxes on this machine that skein did not place, asked when somebody wants to know.
///
/// **On demand, and that is the whole point of it being here rather than in `load_views`.** These
/// rows used to arrive on every board tick, which cost one `sbx ls` every two seconds for every open
/// browser tab — and they are the one part of the board that is not about skein's own boxes. `sbx
/// ls` is the right instrument for "what sandboxes are on this machine", including another skein
/// fleet running beside this one; it is the wrong instrument for "which boxes exist", which the
/// placement records answer without a subprocess.
///
/// Empty is a real answer and so is `Err`: a machine with nothing else on it and a machine that
/// could not be asked are different, and the caller renders them differently.
pub fn foreign_views() -> Result<Vec<BoxView>, String> {
    let fleet = fleet_sandbox();
    let boxes = fleet_boxes().ok_or_else(|| {
        crate::sbx::fleet_failure().unwrap_or_else(|| "sbx did not answer".to_string())
    })?;
    let placed: BTreeSet<String> = match fleet.is_empty() {
        true => BTreeSet::new(),
        false => placed_boxes(&fleet).into_iter().map(|(n, _)| n).collect(),
    };
    Ok(boxes
        .into_iter()
        // The fleet sandbox is skein's own, and a box skein placed is not foreign however `sbx ls`
        // reports it — a migrated box's old sandbox keeps its name and is still listed.
        .filter(|b| b.name != fleet && !placed.contains(&b.name))
        .map(|b| {
            let (state, tier) = Sandbox {
                branch: String::new(),
                dir: b.dir.clone(),
                last_seen: String::new(),
                status: String::new(),
            }
            .state_with(b.live);
            BoxView {
                name: b.name,
                state,
                tier,
                dir: shorten(&b.dir),
                agent: b.agent,
                foreign: true,
                ..Default::default()
            }
        })
        .collect())
}

/// A registry entry enriched for display — what the CLI table and the web API both render.
#[derive(Debug, Default, Serialize)]
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
    /// probe wiring health: "" = fine; "never" = the sandbox is Running but no probe has EVER
    /// reported (no heartbeat, no status file) — hooks dark for this box; the cockpit badges it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hook_health: String,
    /// the *other* half's health — whether this box's own screen is being read, and if not why:
    /// Where the displayed state came from: `screen`, `edge`, or `edge-ahead`. See
    /// [`crate::signals::StatusFrom`] — the last one is a healthy observer whose reading lost to a
    /// newer edge, which nothing disclosed before it existed.
    pub status_from: String,
    /// "" | "none" | "stale" | "unreadable" | "unsupported". See [`screen_health`]. Without it,
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
    use crate::config::{load_config, save_config, Config};
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
        // Asked for, they are all there — and the fleet sandbox and skein's own boxes are not.
        let asked = foreign_views().expect("sbx answers here");
        let names: Vec<&str> = asked.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["old-box"],
            "the on-demand listing must be the sandboxes skein did not place, and only those"
        );
        assert!(asked.iter().all(|v| v.foreign));

        // The flag is only half the feature: a row that carries it and a cockpit that ignores it look
        // identical from here, and that is how a tag silently stops appearing. The cockpit hides
        // these rows by default, reveals them on `foreign:`, and — since they stopped arriving on
        // the tick — has to go and ask for them.
        let page = include_str!("web/index.html");
        assert!(
            page.contains("b.foreign"),
            "the cockpit no longer reads the flag, so foreign boxes would show as ordinary ones"
        );
        assert!(
            page.contains("foreign:"),
            "without the filter keyword there is no way to see them at all"
        );
        assert!(
            page.contains("/api/fleet/foreign"),
            "the rows no longer arrive on the tick, so a cockpit that does not ask for them shows \
             an empty list where a machine's other sandboxes should be"
        );

        forget_place("demo-task");
        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
        *REPOS_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[test]
    fn load_views_promotes_only_the_self_box_when_quiet() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        // No fleet, explicitly. `fleet_sandbox` DEFAULTS to `skein-fleet`, so a config nobody
        // wrote still puts a host in the fleet model — which is the whole reason `sbx ls` stopped
        // being asked on the tick. This test is about the other path, and has to say so.
        save_config(&Config {
            fleet_sandbox: String::new(),
            ..load_config()
        })
        .unwrap();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"thing-self":{{"branch":"s","dir":"/d","lastSeen":"{}","status":""}},
                    "thing-other":{{"branch":"o","dir":"/d","lastSeen":"{}","status":"error"}}}}"#,
                secs_ago(7200),
                secs_ago(7200)
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::set_var("SKEIN_SELF", "thing-self");
        // Force `fleet_boxes()` to None so the board is built from the registry (the "sbx can't be
        // consulted" path). Without this the test would behave differently on a host that has sbx.
        env::set_var("SKEIN_LS_CMD", "false");

        let v = load_views().unwrap();
        let self_v = v.iter().find(|b| b.name == "thing-self").unwrap();
        let other_v = v.iter().find(|b| b.name == "thing-other").unwrap();
        assert_eq!(self_v.state, "live"); // promoted despite a 2h-old lastSeen
        assert_eq!(other_v.state, "stale"); // stale sticky error cannot resurrect a dead peer

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_SELF");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn load_views_drops_registry_only_boxes_when_sbx_is_authoritative() {
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        // No fleet, explicitly. `fleet_sandbox` DEFAULTS to `skein-fleet`, so a config nobody
        // wrote still puts a host in the fleet model — which is the whole reason `sbx ls` stopped
        // being asked on the tick. This test is about the other path, and has to say so.
        save_config(&Config {
            fleet_sandbox: String::new(),
            ..load_config()
        })
        .unwrap();
        let reg = dir.join("sandboxes.json");
        // Registry remembers two boxes, but sbx only lists one — the other was destroyed and its
        // registry entry lingered (e.g. a delist that failed on a corrupt registry).
        fs::write(
            &reg,
            format!(
                r#"{{"thing-live":{{"branch":"l","dir":"/d","lastSeen":"{}","status":""}},
                    "thing-ghost":{{"branch":"g","dir":"/d","lastSeen":"{}","status":""}}}}"#,
                secs_ago(60),
                secs_ago(60)
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::remove_var("SKEIN_SELF");
        // sbx is consulted and lists only thing-live, so thing-ghost must not show up.
        env::set_var(
            "SKEIN_LS_CMD",
            r#"printf '[{"name":"thing-live","status":"running"}]'"#,
        );

        let v = load_views().unwrap();
        assert!(v.iter().any(|b| b.name == "thing-live"));
        assert!(
            !v.iter().any(|b| b.name == "thing-ghost"),
            "a destroyed box that sbx no longer lists must not linger on the board"
        );

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }

    #[test]
    fn load_views_treats_successful_empty_sbx_fleet_as_authoritative() {
        let _g = env_lock();
        let dir = tempdir();
        let reg = dir.join("sandboxes.json");
        fs::write(
            &reg,
            format!(
                r#"{{"dead-box":{{"branch":"old","dir":"/d","lastSeen":"{}","status":"error"}}}}"#,
                secs_ago(86_400)
            ),
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");
        env::remove_var("SKEIN_SELF");
        env::set_var("SKEIN_LS_CMD", "printf '[]'");

        assert!(
            !load_views()
                .unwrap()
                .iter()
                .any(|view| view.name == "dead-box"),
            "a valid empty sbx response must not resurrect a registry-only box"
        );

        env::remove_var("SKEIN_LS_CMD");
        env::remove_var("SKEIN_REGISTRY");
    }
}
