//! From a box to its repo and back: which repo a box belongs to, what a box is called, and the
//! launch spec that records a box's branch and agent.

use super::*;

/// The repo a box belongs to: the registered repo whose id is the box-name prefix (`<id>-<branch>`).
/// Longest id wins, so `web` and `web-api` are unambiguous.
pub fn repo_for_box(name: &str) -> Option<Repo> {
    load_repos()
        .into_iter()
        .filter(|r| name == r.id || name.starts_with(&format!("{}-", r.id)))
        .max_by_key(|r| r.id.len())
}

/// Is this box on the fleet's peer network — [`Repo::peer_messaging`] for the repo it belongs to?
///
/// The one question `fleet::session_script` asks to decide `SKEIN_BOX_PEERS`, shaped like
/// `gitgate::box_is_scoped` so the answer lives beside the field rather than in the launcher's
/// caller. **The launcher is the enforcement point and this is only the input to it**: a value
/// read anywhere inside a box is advisory, because a box owns its own `settings.json`.
///
/// A box belonging to no registered repo is ON, which is the ship default rather than a fallback
/// chosen for safety. The alternative is the failure this whole switch exists to prevent, one
/// level up: a box that is quietly off the network its peers believe it is on.
pub fn box_is_on_the_peer_network(name: &str) -> bool {
    repo_for_box(name).map(|r| r.peer_messaging).unwrap_or(true)
}

/// The branch skein recorded for a box in its repo's launch spec (`<store>/skein/launch/<name>.json`).
/// Host-readable and authoritative for a clone-mode box (whose private clone isn't on the host), so
/// the board can show the box's real branch — including a slashed one the name slug would have flattened.
pub(crate) fn launch_spec_branch(repo: &Repo, name: &str) -> Option<String> {
    launch_spec(repo, name)?
        .get("branch")
        .and_then(|b| b.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

pub(crate) fn launch_spec(repo: &Repo, name: &str) -> Option<serde_json::Value> {
    let p = Path::new(&repo.store)
        .join("skein")
        .join("launch")
        .join(format!("{name}.json"));
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

pub(crate) fn launch_spec_agent(repo: &Repo, name: &str) -> Option<String> {
    launch_spec(repo, name)?
        .get("agent")
        .and_then(|a| a.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// The branch *slug* encoded in a box name (`<id>-<branch-slug>` → `<branch-slug>`). This is the
/// sanitized form (no `/`), used only as a fallback when the real branch isn't otherwise known — the
/// authoritative branch (which may contain `/`, e.g. `feat/auth`) is carried in the launch spec and
/// recovered host-side from git. See [`box_name`].
pub fn branch_from_box(name: &str, repo: &Repo) -> String {
    name.strip_prefix(&format!("{}-", repo.id))
        .unwrap_or(name)
        .to_string()
}

/// The sbx box name for a repo + branch: `<repo-id>-<branch-slug>`.
pub fn box_name(repo_id: &str, branch: &str) -> String {
    format!("{}-{}", repo_id, slug(branch))
}

/// What a box skein opened to review pull request `number` is called.
///
/// **The number, because a branch cannot carry it.** [`box_name`] is `<repo>-<slug(branch)>`, and a
/// pull request's branch is a name somebody else chose: two pull requests can share one (a force-
/// pushed reopen), a fork's branch collides with a local one, and the branch tells nobody which
/// review this is. The number is the one thing GitHub guarantees is unique per repository and
/// stable for the life of the pull request, which is exactly the life of this box.
///
/// **This is ergonomics, not safety, and the distinction matters.** An earlier draft of
/// `docs/pr-review.md` had this convention doing the collision-proofing — but a branch called
/// `pr-123` slugs to `pr-123`, so `<repo>-pr-123` is reachable by an ordinary box and a name can
/// never be the guard. `fleet::refuse_a_repurpose` is, because it reads the purpose already written
/// down for the name and a purpose cannot be a coincidence. What this buys is that the collision
/// is vanishingly rare rather than merely caught, and that a person reading the board can see which
/// pull request a box is about without opening it.
pub fn review_box_name(repo_id: &str, number: u64) -> String {
    format!("{repo_id}-pr-{number}")
}

// ───────────────────────────── kit / store provisioning ─────────────────────────────

/// Record, for box `name`, what its kit startup needs (branch + agent) at
/// `<store>/skein/launch/<name>.json`. The kit finds this file (the store is mounted) and checks out
/// the branch — our env-free channel into the box, since `sbx run --env` is unconfirmed.
pub(crate) fn write_launch_spec_for_agent(
    name: &str,
    branch: &str,
    repo: &Repo,
    agent: &str,
) -> Result<(), String> {
    let dir = Path::new(&repo.store).join("skein").join("launch");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "branch": branch,
        "agent": agent,
    });
    let bytes = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(format!("{name}.json")), &dir, &bytes)
}

/// Re-pin an already-created box to a different branch, without relaunching it. For when the agent
/// has moved off the box's recorded branch (e.g. branch-per-slice work) and the kit's startup hook —
/// which re-reads the launch spec on every reconnect — needs to stop re-asserting the stale one on
/// its next reconnect instead of the branch the agent actually wants to be on. This only rewrites the
/// launch spec; it does not touch the box's live working tree, so if the box is currently mid-session
/// on the wrong branch you still need to `git checkout` inside it once, or just reconnect after this.
/// Errs for an unknown box name or one that belongs to no registered repo (the legacy single-repo
/// path derives its branch from the box name and keeps no launch spec to repin).
pub fn repin_branch(name: &str, branch: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid box name".into());
    }
    let branch = branch.trim();
    if branch.is_empty() {
        return Err("branch is empty".into());
    }
    let repo = repo_for_box(name).ok_or_else(|| format!("no registered repo for box {name}"))?;
    let agent = launch_spec_agent(&repo, name).unwrap_or_else(crate::runtime::fleet_agent);
    write_launch_spec_for_agent(name, branch, &repo, &agent)
}

/// The box's current branch from its project registry, with launch branch only as a fallback.
pub fn branch_of(name: &str) -> Option<String> {
    // SAME cascade as the board (load_views): live registry → launch spec → box-name slug → host git.
    // This feeds *write* actions (gh pr create/merge --head), where the old registry-else-host-git
    // shortcut was dangerous: for a clone-mode box, `lookup_dir` is the SHARED host clone (often
    // sitting on master) — a missing registry branch meant creating/merging a PR for master, not
    // the box's real branch. The board never made that mistake; now the actions can't either.
    if let Some(branch) = registry_entry_for_box(name)
        .map(|box_| box_.branch)
        .filter(|branch| !branch.is_empty() && branch != "?")
    {
        return Some(branch);
    }
    if let Some(rp) = repo_for_box(name) {
        if let Some(b) = launch_spec_branch(&rp, name) {
            return Some(b);
        }
        return Some(branch_from_box(name, &rp));
    }
    git_branch_for(&lookup_dir(name)?)
}

/// Runtime configured for a box. Prefer sbx's live record, then the per-box launch spec (which
/// records the New-box pick), then the fleet's default ([`crate::runtime::fleet_agent`]).
pub fn agent_for_box(name: &str) -> String {
    // Not for a box in the fleet. Migration leaves the old sandbox stopped but still listed under
    // the box's name, so `sbx ls` answers with a record from before the move — which would outrank
    // the launch spec a later takeover wrote. A placed box's spec is the live answer.
    if crate::place::shared_record(name).is_none() {
        if let Some(agent) = fleet_boxes()
            .and_then(|boxes| boxes.into_iter().find(|b| b.name == name))
            .map(|b| b.agent)
            .filter(|a| valid_runtime(a))
        {
            return agent;
        }
    }
    repo_for_box(name)
        .and_then(|repo| launch_spec_agent(&repo, name))
        .unwrap_or_else(crate::runtime::fleet_agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{env_lock, env_pins, tempdir};

    #[test]
    fn repo_for_box_matches_longest_id_prefix() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        let repos = vec![
            Repo {
                read_prs: false,
                id: "web".into(),
                source: "s".into(),
                store: "/s".into(),
                plane_project: String::new(),
                sync_connection: String::new(),
                review_queue: true,
                sync_gateway_url: String::new(),
                ..Default::default()
            },
            Repo {
                read_prs: false,
                id: "web-api".into(),
                source: "s".into(),
                store: "/s".into(),
                plane_project: String::new(),
                sync_connection: String::new(),
                review_queue: true,
                sync_gateway_url: String::new(),
                ..Default::default()
            },
        ];
        save_repos(&repos).unwrap();
        // longest matching id wins, so "web-api-feat-x" is web-api/feat-x, not web/api-feat-x.
        let r = repo_for_box("web-api-feat-x").unwrap();
        assert_eq!(r.id, "web-api");
        assert_eq!(branch_from_box("web-api-feat-x", &r), "feat-x");
        let r2 = repo_for_box("web-login").unwrap();
        assert_eq!(r2.id, "web");
        assert_eq!(branch_from_box("web-login", &r2), "login");
        assert!(repo_for_box("other-x").is_none());
    }

    /// **A box nobody picked a runtime for runs the fleet's Default agent, whatever an old repo
    /// record says** (the owner, 2026-09-27: one fleet default plus a per-box pick).
    ///
    /// The record is written as an older skein wrote it, with the repo's own `agent` key, which
    /// used to be copied from the default at add time and then outrank it for good.
    ///
    /// What would make it fail: `Repo` refusing a key it no longer has (`deny_unknown_fields`), so
    /// an old `repos.json` stops parsing and every repo vanishes (`the old record still reads`); or
    /// the fallback being the built-in `claude` rather than the setting, which is what the old
    /// per-repo copy amounted to (`the fleet's default decides`).
    #[test]
    fn a_box_with_no_pick_runs_the_fleets_default_agent_not_an_old_repo_copy() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        let store = home.join("st").join(".claude");
        fs::create_dir_all(&store).unwrap();
        fs::write(
            home.join("repos.json"),
            serde_json::json!([{
                "id": "thing",
                "source": "https://example.com/thing.git",
                "store": store.to_string_lossy(),
                "agent": "claude",
            }])
            .to_string(),
        )
        .unwrap();
        crate::testutil::switch_on(|c| c.default_agent = "codex".into());

        assert_eq!(
            load_repos()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["thing"],
            "the old record still reads"
        );
        let repo = &load_repos()[0];
        // No pick recorded for this box, so re-pinning it writes whatever it would run.
        repin_branch("thing-feat-x", "feat-x").unwrap();
        assert_eq!(
            launch_spec_agent(repo, "thing-feat-x").as_deref(),
            Some("codex"),
            "the fleet's default decides"
        );
        // And a pick, once made, is kept: it is the per-box half of the rule.
        write_launch_spec_for_agent("thing-feat-y", "feat-y", repo, "claude").unwrap();
        repin_branch("thing-feat-y", "feat-z").unwrap();
        assert_eq!(
            launch_spec_agent(repo, "thing-feat-y").as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn repin_branch_rewrites_launch_spec_without_relaunch() {
        let _g = env_lock();
        let home = tempdir();
        let mut env = env_pins();
        env.set("SKEIN_HOME", &home);
        let store = home.join("st").join(".claude");
        fs::create_dir_all(&store).unwrap();
        let repos = vec![Repo {
            read_prs: false,
            id: "thing".into(),
            source: "s".into(),
            store: store.to_string_lossy().to_string(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
            ..Default::default()
        }];
        save_repos(&repos).unwrap();
        // box created on the wrong branch (its creation branch)…
        write_launch_spec_for_agent("thing-feat-x", "feat-x", &repos[0], "claude").unwrap();
        assert_eq!(
            launch_spec_branch(&repos[0], "thing-feat-x").as_deref(),
            Some("feat-x")
        );
        // …re-pinned to the branch the agent actually moved to, without relaunching.
        repin_branch("thing-feat-x", "feat-y").unwrap();
        assert_eq!(
            launch_spec_branch(&repos[0], "thing-feat-x").as_deref(),
            Some("feat-y")
        );
        // unknown / unregistered box name errs rather than silently no-opping.
        assert!(repin_branch("no-such-box", "main").is_err());
        assert!(repin_branch("thing-feat-x", "").is_err());
    }

    /// **A review box's name is ergonomics, and the collision it does not prevent is why.**
    ///
    /// `review_box_name` uses the pull request number because a branch cannot carry one: two pull
    /// requests can share a branch name, a fork's collides with a local one, and the branch tells
    /// nobody which review a box is about.
    ///
    /// What it is NOT is a guard, and this test pins the reason rather than the hope. A branch
    /// literally called `pr-42` slugs to `pr-42`, so an ordinary box lands on exactly the name a
    /// review box would take. An earlier draft of `docs/pr-review.md` had this convention doing the
    /// collision-proofing; `fleet::refuse_a_repurpose` does it instead, by reading the purpose
    /// already written down for the name — because a purpose cannot be a coincidence and a name
    /// can.
    ///
    /// If this assertion ever starts failing because the shapes were made not to collide, the
    /// guard is still the record: a naming scheme that is *usually* unique is the kind this repo
    /// has had to take back out.
    #[test]
    fn a_review_boxs_name_carries_the_number_and_is_not_a_guard() {
        assert_eq!(review_box_name("acme", 42), "acme-pr-42");
        assert_eq!(
            review_box_name("acme", 42),
            box_name("acme", "pr-42"),
            "the collision is reachable, which is why the placement record and not this name is \
             what stops a review box adopting somebody's work"
        );
        // And the ordinary case is not near it: a branch is slugged, a number is not.
        assert_ne!(review_box_name("acme", 42), box_name("acme", "feat/auth"));
    }

    #[test]
    fn slug_and_box_name_handle_slashes() {
        assert_eq!(slug("feat/auth"), "feat-auth");
        assert_eq!(slug("feat/auth/v2"), "feat-auth-v2");
        assert_eq!(slug("user@host~weird"), "user-host-weird");
        assert_eq!(slug("keep.dots_and-dashes"), "keep.dots_and-dashes");
        assert_eq!(slug("/leading/and/trailing/"), "leading-and-trailing");
        assert_eq!(box_name("thing", "feat/auth"), "thing-feat-auth");
    }
}
