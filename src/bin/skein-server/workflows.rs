//! The pull-request workflows: the workflow file, a repo's workflows, and one pull request's
//! workflow and triggers.

use super::*;

/// The fleet's workflows, and every word one can be written with.
///
/// The vocabulary is served rather than hard-coded in the page for the same reason the tables exist
/// at all: a picker offering a word the parser refuses is a workflow somebody builds and cannot
/// save, and it is found by a person in the one moment they were trusting the tool.
pub(super) async fn api_workflow_file() -> Response {
    let flows = skein::workflow::load();
    Json(serde_json::json!({
        "enabled": skein::prwork::enabled(),
        // The variable holding the switch off, if one is: the pause button cannot resume past it,
        // and the panel says so rather than offering a press that changes nothing.
        "held": skein::config::held_by_env().get("pr_workflows"),
        // A file that will not parse is reported as itself. The editor refuses to save over it,
        // because a person who has not seen what is there cannot mean to replace it.
        "error": flows.as_ref().err().cloned().unwrap_or_default(),
        "workflow": flows
            .as_ref()
            .map(|f| f.iter().map(written).collect::<Vec<_>>())
            .unwrap_or_default(),
        "conditions": skein::workflow::conditions(),
        "actions": skein::workflow::actions(),
    }))
    .into_response()
}

/// A workflow in the shape the file has and the editor edits.
///
/// Delegated to `workflow::editor_shape` — the same code `to_bytes` writes the file with. The
/// hand-built copy this replaces dropped `serial` the day it was added, which under-reported a
/// running train AND meant an editor save would strip it from the file (see `editor_shape`).
fn written(f: &skein::workflow::Workflow) -> serde_json::Value {
    skein::workflow::editor_shape(f)
}

/// Replace the fleet's workflows with what the editor sends.
///
/// The whole file at once, because a workflow is only meaningful as an ordered whole and a
/// step-by-step API would let a half-written one run on the next tick.
pub(super) async fn api_save_workflows(
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let raw = body.to_string();
    match skein::workflow::save(raw.as_bytes()) {
        Ok(flows) => Json(serde_json::json!({
            "ok": true,
            "workflow": flows.iter().map(written).collect::<Vec<_>>(),
        })),
        // The refusal names the workflow, the step and the word — see `workflow::from_bytes`. It is
        // shown as it is: a message that says "invalid" teaches nobody the vocabulary.
        Err(error) => Json(serde_json::json!({ "ok": false, "error": error })),
    }
}

/// What every pull request in this repo's queue would have happen to it, and what could.
///
/// A separate read from the queue on purpose. The queue is what GitHub says and is cached for a
/// minute; this is skein's own answer about it, and it changes the moment somebody assigns a
/// workflow — folding the two together would mean a page that cannot show a choice taking effect
/// without re-reading GitHub.
pub(super) async fn api_workflows(Path(id): Path<String>) -> Response {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return (StatusCode::NOT_FOUND, "no such repo").into_response();
    };
    let out = tokio::task::spawn_blocking(move || {
        // A file skein cannot read is reported as itself, not as "no workflows": a fleet whose
        // workflows all stopped working because of a typo must say so, or the automation simply
        // appears to have been forgotten.
        let flows = skein::workflow::load()?;
        // The queue as skein already knows it (SKEIN-291). This route's own note above says it is
        // "a separate read from the queue" and the page's call site calls it "cheap: it reads the
        // cached queue" — but `queue(&repo, false)` refreshes the moment that cache is a minute
        // old, and the pane fetches this per repo alongside `/review` and `/review/summaries`, so
        // a cold minute made three routes wait on three refreshes of one queue.
        let queue = queue_as_known(&repo)?;
        // One read for every PR's history — `journal()` per PR would re-read the same file
        // per row.
        let mut journals = skein::prwork::journals(&repo.id);
        // What the train view is computed FROM: the same carrying set the sweep uses — archived
        // PRs excluded, because a PR set aside is one you said "not now" about and the
        // sweep honours that; a panel that showed it in the line would promise an act the tick
        // will never take.
        let mut carrying: Vec<(u64, String)> = Vec::new();
        let mut prs = serde_json::Map::new();
        for pr in &queue.prs {
            // `facts_of_in`, not `facts_of`: the tick answers the reviewer's reading facts from
            // the repo's own cache, and a panel that answered them from nothing would show an
            // approval as unreachable while the tick reached it. That is the disagreement the
            // comment above forbids, one field further down.
            let facts = skein::prwork::facts_of_in(&repo.id, pr, &queue.viewer, &queue.trunk);
            let standing = skein::prwork::standing(&repo.id, pr.number, &facts, &flows);
            // **`holding` too, not just `workflow`** (SKEIN-326). A holding pull request — assigned
            // by hand, its workflow's own `matches` not met (SKEIN-279) — carries a non-empty
            // `standing.workflow` ON PURPOSE, so the row can still show which workflow somebody
            // chose. But `prwork::sweep` acts on `Carries::acting`, not `Carries::name`, and skips
            // it. Without this condition the panel drew it as a car, and as the FRONT if it had the
            // lowest number, while the tick's front was somebody else — which is precisely what the
            // comment above says must not happen.
            if !standing.workflow.is_empty()
                && standing.holding.is_empty()
                && !matches!(pr.lane, skein::prq::Lane::Archived)
            {
                carrying.push((pr.number, standing.workflow.clone()));
            }
            let mut entry = serde_json::to_value(standing).unwrap_or_default();
            // The history rides beside the standing: "which step is it on" and "what has it
            // already done" are one question to the person automating this.
            entry["journal"] =
                serde_json::to_value(journals.remove(&pr.number).unwrap_or_default())
                    .unwrap_or_default();
            prs.insert(pr.number.to_string(), entry);
        }
        let trains = skein::prwork::trains(&repo.id, &carrying, &flows);
        Ok::<_, String>((
            queue,
            serde_json::json!({
                "enabled": skein::prwork::enabled(),
                "held": skein::config::held_by_env().get("pr_workflows"),
                "read_prs": repo.read_prs,
                // `editor_shape`, NOT a hand-built copy: this route had the second of the two hand
                // serializers that silently dropped `serial` — see workflow::editor_shape.
                "defined": flows.iter().map(skein::workflow::editor_shape).collect::<Vec<_>>(),
                "trains": trains,
                "prs": prs,
            }),
        ))
    })
    .await;
    match out {
        Ok(Ok((queue, value))) => answered_from(&queue, value),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct WorkflowReq {
    /// The workflow to put on this pull request. The empty string means "no workflow, and no rule
    /// either" — the exclusion, which is a choice and not an absence.
    ///
    /// Absent means **leave the choice alone**. It used to mean "forget it", which made
    /// `clear_stop` on its own silently take the workflow off the pull request as well: one button
    /// doing a second thing nobody asked it to.
    #[serde(default)]
    name: Option<String>,
    /// Forget the choice entirely, and let the rules speak for this pull request again.
    #[serde(default)]
    unassign: bool,
    /// Let a stopped workflow run again. What a person presses after fixing whatever stopped it.
    #[serde(default)]
    clear_stop: bool,
}

/// Choose what governs one pull request, or let it run again.
#[derive(Deserialize)]
pub(super) struct TriggersReq {
    /// The words this pull request wakes on. Absent forgets the override and lets the repo's set
    /// speak again; an EMPTY list is the deliberate "wake on nothing", which is a third state and
    /// not the same as absent.
    #[serde(default)]
    on: Option<Vec<String>>,
}

/// Give one pull request its own trigger set, or take it back off — §10's "overridable per pull
/// request", which until now only the workflow assignment was.
///
/// The three states live in `repos::set_pr_triggers` where they are tested, for the reason the
/// route above records: the last time a three-state meaning was written in a route it grew a bug
/// within the hour.
pub(super) async fn api_set_pr_triggers(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<TriggersReq>,
) -> Json<serde_json::Value> {
    if !skein::repos::load_repos().iter().any(|r| r.id == id) {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    }
    match skein::repos::set_pr_triggers(&id, number, req.on) {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}

pub(super) async fn api_set_workflow(
    Path((id, number)): Path<(String, u64)>,
    Json(req): Json<WorkflowReq>,
) -> Json<serde_json::Value> {
    let Some(repo) = skein::repos::load_repos().into_iter().find(|r| r.id == id) else {
        return Json(serde_json::json!({ "ok": false, "error": "no such repo" }));
    };
    // The meaning of the three answers lives in `prwork`, where it is tested. It grew a bug in an
    // hour when it lived here: clearing a stop also un-assigned the workflow.
    let done = skein::prwork::apply(
        &repo.id,
        number,
        req.name.as_deref(),
        req.unassign,
        req.clear_stop,
    );
    match done {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e })),
    }
}
