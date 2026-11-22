//! What one managed repo is: the `Repo` record, what an entry with only an id, a source and a
//! store parses as, and the reviewer engine's ceiling and why it will not act on a repo.

use super::*;

/// One managed repo: a **mirror** on the volume and a **store** every box of it reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    /// The remote this repo lives at. **Always a git URL** — a local path is refused at
    /// registration (see [`add_repo`]), because skein runs inside the fleet sandbox and no checkout
    /// on the host is reachable from there.
    ///
    /// There was a `source_tree` beside this, the user's own checkout, kept for the one question a
    /// mirror cannot answer: what a project keeps OUT of git. In the fleet that answer was already
    /// unreachable — the step that copied them warned on every launch that they had not arrived —
    /// so the field and its machinery are gone rather than left as a thing that only ever
    /// apologised.
    pub source: String,
    pub store: String, // host shared store, `repos/<id>/store/data` unless `--store` chose one
    /// The branch a new box of this repo starts from. Empty ⇒ whatever the remote calls its own
    /// default.
    ///
    /// **Per repo** (the owner, 2026-09-27): it used to be one fleet-wide value, honoured only by
    /// the repos whose remote had that branch. It is still not trusted on its own —
    /// [`crate::fleet::base_branch`] asks the remote with `ls-remote --symref` and takes this only
    /// if the remote has it, and `diff::diff_base_refs` leads its ladder with `origin/<value>`.
    /// What reads the resolver is the clone a box comes up on (`fleet::clone_script`) and
    /// `reviewbox::open_at`. A merge never reads it: it reads the pull request's own base.
    #[serde(default)]
    pub base_branch: String,
    // No `agent` (the owner, 2026-09-27). A repo's own runtime was copied from the fleet default at
    // add time, could not be edited afterwards, and outranked the default for every box of the repo.
    // The runtime is the fleet's default plus a pick per box (`runtime::fleet_default_runtime`). An old
    // `repos.json` that still carries the key reads fine: serde skips a field it does not know.
    /// The Plane project this repo's work is tracked in — a project URL or a bare uuid, kept
    /// verbatim so the cockpit can link to the board. Per-repo because a project is what an agent
    /// token binds to; empty ⇒ this repo's boxes get a tracker token with no default project, and
    /// must name a project on every call.
    #[serde(default)]
    pub plane_project: String,
    /// Which [`SyncConnection`] this repo's boxes claim work through, by id. Empty ⇒ not tracked.
    /// A *selection*, not a URL: a gateway and the personal token that mints tokens at it are one
    /// thing, and a repo pointed at gateway B while the host holds only gateway A's token is a
    /// setting that can only be right by accident. Per-repo because a gateway is a backlog — two
    /// products in different Plane instances cannot share a claim namespace.
    #[serde(default)]
    pub sync_connection: String,
    /// **May this repo's boxes talk to the rest of the fleet?** (architecture §9.5 R11.)
    ///
    /// `ListAgents` naming every live box and `SendMessage` reaching one is a real feature and the
    /// fleet's only inter-box channel that does not leave the sandbox, so this ships **ON** — like
    /// [`Repo::review_queue`]. Absent from
    /// the file means a repo written before the switch existed, when every box was on the peer
    /// network, so the serde default and the new-repo default agree here rather than disagreeing
    /// the way `review_queue`'s do.
    ///
    /// **Off is full isolation: neither the socket bind nor the session registry**, so the repo's
    /// boxes neither see peers nor are seen. Not a preference between two designs — the half where
    /// discovery stays shared and only the socket goes is unenforceable and worse than either
    /// whole: *"the problem with only peer half closed is that everyone else thinks that it is live
    /// so they write to it but it never gets delivered and left wondering what happened"* (owner,
    /// 2026-09-07). With the directory bound, a box can write a peer socket path whether or not the
    /// registry is shared, and the only things that would stop it are settings that box can edit.
    ///
    /// **The enforcement point is the mount, and it is [`crate::fleet::session_script`] that must
    /// carry this** — as `SKEIN_BOX_PEERS`, the way `SKEIN_GIT_SCOPE` carries the git switch. A
    /// value read anywhere else is advisory: a box owns its own `settings.json`.
    ///
    /// **Flipping it does not reach a running box, and nothing here can make it.** The launcher
    /// reports what each box was born with on its own stdout, into
    /// [`crate::place::PlaceRecord::peers`]; `fleet::cover_is_current` compares that alongside the
    /// launcher revision, because the revision hashes `box-session.sh` and this switch changes not
    /// one byte of it. Without that comparison the board would call a box current while it ran the
    /// opposite mount.
    #[serde(default = "crate::config::default_true")]
    pub peer_messaging: bool,
    /// **May Claude Code's built-in `telemetry` plugin run in this repo's boxes?** (SKEIN-1225.)
    ///
    /// **Off unless it is switched on, per repo** (the owner, 2026-09-29: "Plugin only"). Off,
    /// every box of the repo has `enabledPlugins["telemetry@builtin"] = false` in its user
    /// settings, so Claude Code does not load the plugin that batches other built-in plugins'
    /// analytics rows to Anthropic. That is all it turns off: Claude Code's own usage statistics
    /// still go to Anthropic and its feature flags still evaluate, which is why this is not
    /// `DISABLE_TELEMETRY`. skein's own per-turn telemetry is a different thing again: it is
    /// written into the repo's store and goes nowhere else. Absent from the file is off, so a repo
    /// registered before the switch existed gets the new default at its boxes' next start.
    ///
    /// **The enforcement point is the launcher**, the way [`Repo::peer_messaging`]'s is:
    /// [`crate::fleet::session_script`] carries it as `SKEIN_BOX_TELEMETRY`, and `box-session.sh`
    /// writes the setting into the box's own user settings, the one file every Claude session in
    /// the box reads whatever directory it starts in. Like that switch, flipping it reaches a box
    /// at its next start and not before.
    #[serde(default)]
    pub anthropic_telemetry: bool,
    /// Does this repo have a review queue, and may the badge poll it?
    ///
    /// **On by default**, and separate from whether summaries are allowed: this is about *this*
    /// repo, not about AI. A repo you have registered only to run boxes in — a fork, a scratch
    /// clone, somebody else's project you read — has pull requests that are none of your business,
    /// and polling it every few minutes to say so would spend `gh` calls to produce a zero.
    ///
    /// A repo with no GitHub remote is skipped whether or not this is set: it cannot have a queue.
    ///
    /// **A repo registered now starts OFF** (see `add`), and the serde default stays TRUE on
    /// purpose: the two answer different questions. Absent from the file means the repo predates
    /// the field, when every queue was on — so reading it as off would switch off a queue somebody
    /// has been using, on upgrade, without being asked. What a new repo starts as is a choice about
    /// spending; what an old file means is a fact about the past.
    #[serde(default = "crate::config::default_true")]
    pub review_queue: bool,
    /// **What kinds of project this repo is, as the owner chose them** (rust, node, python, go;
    /// `kinds::KINDS`), which decide which of the repo's shared caches its boxes are pointed at
    /// (SKEIN-1373, the owner's decision of 2026-11-06).
    ///
    /// `None`, the field absent, is "never chosen", and each box reads the kinds off its own
    /// checkout as it starts (`kinds::kinds_in_dir`). `Some([])` is a repo the owner has said needs
    /// none of the caches, which is a different answer and is honoured: which is why this is an `Option`. Kept here, in skein's record, and never in the repo's own files,
    /// which a box can change. Read at launch, so a change reaches a box at its next start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,
    /// Superseded by [`Repo::sync_connection`]; read once by the migration, then cleared. Kept so
    /// a `repos.json` written before connections existed still parses.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sync_gateway_url: String,
}

/// **What a registry entry with nothing but an id, a source and a store parses as** — and
/// therefore what a fixture that has no opinion should inherit.
///
/// Written through serde rather than derived, and that is the whole point. `Repo`'s defaults are
/// not Rust's: `agent` is `default_agent()`, `review_queue` is TRUE because absent means a file
/// written when every queue was on, and the reviewer's trigger set is `["requested"]`. A
/// `#[derive(Default)]` would hand every fixture an empty agent, a switched-off queue and an empty
/// trigger set — and an empty trigger set is a state [`auto_review_stands`] reports as OFF, so
/// tests would inherit a repo that is quietly the opposite of the one they meant.
///
/// Round-tripping the minimal entry means there is no second list of defaults to keep in step with
/// the serde attributes. Drift is not made unlikely here; it is made unrepresentable.
impl Default for Repo {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({"id": "", "source": "", "store": ""}))
            .expect("a registry entry needs an id, a source and a store, and nothing else")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repo registered now does not start polling GitHub, and an older file that never wrote the
    /// field keeps the queue it has been running with.
    ///
    /// The two are deliberately different answers, and conflating them is how an upgrade turns off
    /// something somebody was watching: what a NEW repo starts as is a choice about spending, and
    /// what an ABSENT field means is a fact about a file written when every queue was on.
    #[test]
    fn a_new_repo_starts_without_a_queue_and_an_old_file_keeps_its_own() {
        let old: Repo = serde_json::from_value(serde_json::json!({
            "id": "written-before-the-field",
            "source": "https://github.com/acme/thing.git",
            "source_tree": "",
            "store": "",
        }))
        .unwrap();
        assert!(
            old.review_queue,
            "an upgrade switched off a queue that had been running, without asking"
        );

        let explicit: Repo = serde_json::from_value(serde_json::json!({
            "id": "said-so",
            "source": "https://github.com/acme/thing.git",
            "source_tree": "",
            "store": "",
            "review_queue": false,
        }))
        .unwrap();
        assert!(!explicit.review_queue, "a deliberate off was not honoured");
    }

    /// **A `repos.json` written before the switch existed keeps its boxes on the peer network.**
    ///
    /// Every box in this fleet was on it before there was a field to say so, so absent means on —
    /// and unlike `review_queue`, whose serde default and new-repo default deliberately disagree,
    /// both answers are the same here. Reading absent as OFF would take a working fleet's only
    /// non-vendor inter-box channel away on upgrade, silently, and the boxes it took it from would
    /// be the ones whose peers still saw them in `ListAgents` until they were restarted.
    ///
    /// **What would make this fail:** `#[serde(default)]` on the field instead of
    /// `default = "crate::config::default_true"` — `bool`'s own default is `false`.
    #[test]
    fn a_repos_json_written_before_the_switch_keeps_its_boxes_on_the_peer_network() {
        let before_the_field: Repo =
            serde_json::from_str(r#"{"id":"web","source":"git@x:web","store":"/s"}"#).unwrap();
        assert!(
            before_the_field.peer_messaging,
            "a repo that predates the switch must not be read as having turned it off"
        );
    }
}
