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
    pub store: String, // host shared `.claude` store
    /// May skein read this repo's pull requests without being asked, with nobody watching?
    ///
    /// **Off unless it is switched on, per repo**, as it was asked for: "only ones where I mark
    /// the automatic reading enabled". Every other setting here describes how a repo is worked;
    /// this one decides whether skein spends model calls on it while nobody is looking, so a
    /// registry entry added for an unrelated reason cannot start costing money.
    #[serde(default)]
    pub read_prs: bool,
    // No `agent` (the owner, 2026-09-27). A repo's own runtime was copied from the fleet default at
    // add time, could not be edited afterwards, and outranked the default for every box of the repo.
    // The runtime is the fleet's default plus a pick per box (`runtime::fleet_agent`). An old
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
    /// fleet's only inter-box channel that does not leave the sandbox, so this ships **ON** — unlike
    /// [`Repo::read_prs`] and [`Repo::auto_review`], and like [`Repo::review_queue`]. Absent from
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
    /// **May the reviewer engine ACT on this repo?** (`docs/pr-review.md` §10, layer 3.)
    ///
    /// Distinct from [`Repo::read_prs`] and [`Repo::review_queue`] on purpose, and folding it into
    /// either would give two places to look for why nothing happened. Reading costs money and is
    /// useful with no automation at all; the queue is a view; this decides whether skein acts. A
    /// repo can reasonably be read and queued with the engine off, and that is the state everything
    /// starts in.
    ///
    /// **No repo starts with this on**, decided 2026-08-30: *"auto review I will toggle on when
    /// needed. So no default."* Unlike [`Repo::review_queue`], whose serde default is TRUE
    /// because absent means a file written when every queue was on, absent here can only mean a
    /// file written before skein could do this at all — so `false` is both the new-repo choice and
    /// the honest reading of the past.
    #[serde(default)]
    pub auto_review: bool,
    /// **Which events wake it** (§10). The owner's ask was a mode where *"the trigger is just
    /// review requested state, but not new commits"*, and that is this list's default rather than a
    /// special case: `requested` alone.
    ///
    /// Turning every trigger on is the full-auto mode. **The empty set is not a third state** —
    /// it means the engine is awake and nothing can wake it, which reads as broken rather than as
    /// off, so [`auto_review_stands`] reports it as off and says which switch to use instead.
    #[serde(default = "default_auto_review_on")]
    pub auto_review_on: Vec<String>,
    /// **How far the engine may go on its own** (§10). Anything past the ceiling is drafted and
    /// waits for a person.
    #[serde(default, deserialize_with = "ceiling_or_nothing")]
    pub auto_review_ceiling: Ceiling,
    /// **Whose pull requests** — `mine` or `all`. `mine` by default: the intended use is reviewing
    /// what your own boxes open, and an outside contributor's pull request is a different risk, a
    /// different audience, and the first place a wrong verdict is seen by somebody who did not opt
    /// into any of this.
    #[serde(default = "default_auto_review_authors")]
    pub auto_review_authors: String,
    /// **Decide and show, post nothing** (§10). The precedent is `prwork::Standing`, the dry run
    /// asked for before trusting the author side — *"a preview computed a second way is a preview
    /// that can disagree with what happens"*, so it is the same evaluator either way.
    ///
    /// Off by default rather than on: two switches to get any effect reads as a broken feature, and
    /// [`Repo::auto_review_ceiling`] is what makes switching on safe without a second step.
    #[serde(default)]
    pub auto_review_dry_run: bool,
    /// **What this repository owes a reviewer before a verdict** — `docs/pr-review.md` §8, and the
    /// one setting here that is a list of obligations rather than a permission.
    ///
    /// `None` — the field absent — is *"this repository has never said"*, and takes
    /// [`crate::owed::default_set`]: every check this build can evaluate. `Some([])` is a
    /// repository that has explicitly said it owes nothing, which is a different answer and is
    /// honoured. That distinction is why this is an `Option` and not a `Vec` with an empty default:
    /// the two states are not the same and a `Vec` cannot tell them apart.
    ///
    /// Defaulting to the whole set rather than to nothing is §8's argument and it cuts against this
    /// file's usual grain — every flag above starts off. It is not a permission: nothing here lets
    /// skein spend or post anything it could not already. It withholds a verdict until a check has
    /// been made, so the fail-closed direction is ON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owed_checks: Option<Vec<String>>,
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

/// **How far the reviewer engine may go without a person** — ordered by consequence, not by kind.
///
/// `docs/pr-review.md` §10 proposed this instead of the three independent post switches §9 first
/// drew, and the reason is that the three values are not independent: three booleans permit
/// *"approve unattended, but ask me before commenting"*, which is not a policy anybody wants and is
/// exactly the state a checkbox grid makes reachable by accident. A ceiling cannot express it.
///
/// **An unrecognised ceiling reads as [`Ceiling::None`]**, which is the opposite direction from
/// [`crate::place::Purpose`]'s lenient reader and deliberately so. There, an unknown value meant a
/// box skein could not reach, and the cost of guessing was losing it. Here the value is a
/// PERMISSION, and a newer skein's word read by an older one must never widen what that older one
/// will do unasked. Both are the same judgement — fail towards the answer that cannot surprise
/// anybody — and they point opposite ways because the fields mean opposite kinds of thing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ceiling {
    /// Post nothing unattended. The engine reads, and everything it concludes waits for a person.
    None,
    /// Findings only. The default when a repo is switched on: a wrong comment is loud and somebody
    /// argues with it, which is the asymmetry the interviewed box made its whole argument from.
    #[default]
    Comment,
    /// Findings, and a refusal. Still never an approval.
    Changes,
    /// Everything, including the verdict that discharges a review. Deliberately reachable
    /// (2026-08-30, "Both, unattended"); it is not what a repo starts at.
    Approve,
}

impl Ceiling {
    /// The word for it in a sentence somebody reads.
    pub fn spelled(self) -> &'static str {
        match self {
            Ceiling::None => "none",
            Ceiling::Comment => "comment",
            Ceiling::Changes => "changes",
            Ceiling::Approve => "approve",
        }
    }
}

/// A ceiling this build does not recognise reads as [`Ceiling::None`] — post nothing unattended.
///
/// **Not `Ceiling::default()`**, which is `Comment`, and the difference is the whole reason this
/// exists rather than a `#[serde(other)]`. The default is what a repo gets when nobody has chosen;
/// this is what an older skein does with a word a NEWER one wrote, and those are different
/// questions. Downgrading must never widen what skein will do while nobody is looking, so an
/// unreadable permission is the narrowest one and not the usual one.
///
/// The mirror image of `place::purpose_or_manual`, which falls the other way for the opposite
/// reason: there an unknown value costs a box skein can no longer reach, so it guesses towards
/// keeping it. Both fail towards the answer that cannot surprise anybody.
fn ceiling_or_nothing<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Ceiling, D::Error> {
    Ok(match String::deserialize(de) {
        Ok(word) => {
            serde_json::from_value(serde_json::Value::String(word)).unwrap_or(Ceiling::None)
        }
        Err(_) => Ceiling::None,
    })
}

pub(super) fn default_auto_review_on() -> Vec<String> {
    vec!["requested".to_string()]
}

pub(super) fn default_auto_review_authors() -> String {
    "mine".to_string()
}

/// **Why the reviewer engine will not act on this repo**, or `None` when it will.
///
/// §10's layers, outermost first, and the first `no` ends it. The order is not cosmetic: each layer
/// answers a different question, and answering them in the wrong order would report the wrong cause
/// — which is the whole point of there being one answer to *"why did it not review this"*.
///
/// **The money door is layer 1 and nothing overrides it.** A per-pull-request assignment overrides
/// [`Repo::auto_review`], never [`Repo::read_prs`]: a pull request explicitly switched on in a repo
/// whose reading is off must SAY so rather than silently doing nothing or silently spending. That
/// is why this returns a sentence rather than a bool — failing quietly in either direction is the
/// thing every other guard in this feature exists to avoid.
///
/// The global kill switch (`prwork::enabled`) is layer 0 and is checked by the tick, not here: it
/// is about the whole fleet and this function is about one repo.
pub fn auto_review_stands(repo: &Repo) -> Option<String> {
    auto_review_stands_for(repo, false)
}

/// The same layers, told whether somebody chose this pull request's workflow **by hand** — §10's
/// layer 7, and the one thing that overrides layer 3.
///
/// > A per-PR assignment always wins over a match, in both directions — including an explicit "no
/// > workflow" on a PR a rule would otherwise claim.
///
/// The other direction needs nothing here: an excluded pull request carries no workflow at all
/// (`prwork::Carries::Excluded`), so no step is ever chosen for it and there is nothing to refuse.
/// This is the "on, in a repo that is off" half — assign the reviewer flow to one pull request in a
/// repo whose engine is not switched on, and it acts.
///
/// **`assigned` is passed in rather than looked up**, which is `reviewbox::close_finished`'s shape
/// and the same reason: the assignment file belongs to `prwork`, and reaching for it from here
/// would give this module an opinion about workflows in order to answer a question about flags.
///
/// **Layer 1 is untouched and that is the whole rule about the money door.** A pull request
/// switched on inside a repo skein may not read is refused with a sentence that says exactly that,
/// because the two failures worth avoiding are silently doing nothing and silently spending.
pub fn auto_review_stands_for(repo: &Repo, assigned: bool) -> Option<String> {
    if !repo.read_prs {
        return Some(format!(
            "reading is switched off for {} — automatic review cannot be switched on for one pull \
             request past a repo skein may not read at all",
            repo.id
        ));
    }
    if !repo.auto_review && !assigned {
        return Some(format!(
            "automatic review is switched off for {} (this is the default; nothing turns it on by \
             itself)",
            repo.id
        ));
    }
    if repo.auto_review_on.iter().all(|t| t.trim().is_empty()) {
        // Said without claiming which switch is on, because with an assignment in hand this is
        // reachable on a repo whose `auto_review` is off — and "automatic review is on for X" would
        // then be the one false sentence in a function whose whole job is naming the true cause.
        return Some(format!(
            "there is no trigger that could wake a review of {} — its trigger set is empty. Switch \
             it off rather than leaving it awake with nothing to wake for",
            repo.id
        ));
    }
    None
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

    /// **The layers answer in order, and the money door is not one a per-PR switch can open.**
    ///
    /// §10's whole point is that "why did it not review this" has ONE answer. Reporting the wrong
    /// layer is not a cosmetic bug: a person told "automatic review is off" turns it on, and
    /// nothing happens, because the real answer was that skein may not read the repo at all.
    ///
    /// Sabotages: swap the first two checks and the both-off case names the wrong switch; drop the
    /// empty-trigger branch and a repo that is awake with nothing to wake it reports as ready.
    #[test]
    fn the_first_no_is_the_one_reported_and_reading_is_the_first_question() {
        let off: Repo = serde_json::from_value(serde_json::json!({
            "id": "acme", "source": "https://example.invalid/a.git", "store": ""
        }))
        .expect("a registry entry with no auto-review keys still parses");

        // Everything off, which is what a repo starts as.
        assert!(!off.auto_review, "a repo must not start with the engine on");
        assert_eq!(
            off.auto_review_on,
            vec!["requested".to_string()],
            "the default trigger set is review-requested, not new commits"
        );

        // BOTH are off, and the reported reason is the outer one. A person who is told the inner
        // one turns it on and watches nothing happen.
        let said = auto_review_stands(&off).expect("an untouched repo does not act");
        assert!(
            said.contains("reading is switched off"),
            "the money door is layer 1 and must be what is reported when both are shut: {said}"
        );

        // Reading on, engine off: now the inner answer is the true one.
        let readable = Repo {
            read_prs: true,
            ..off.clone()
        };
        let said = auto_review_stands(&readable).expect("reading alone does not act");
        assert!(
            said.contains("automatic review is switched off"),
            "with reading on, the engine's own switch is the answer: {said}"
        );

        // On, with triggers: it stands.
        let live = Repo {
            auto_review: true,
            ..readable.clone()
        };
        assert!(
            auto_review_stands(&live).is_none(),
            "a repo that is readable, switched on and has a trigger must be allowed to act"
        );

        // Awake with nothing to wake it is OFF, said out loud — not a third state, and not ready.
        let mute = Repo {
            auto_review_on: Vec::new(),
            ..live.clone()
        };
        let said = auto_review_stands(&mute).expect("no trigger can wake it, so it does not act");
        assert!(
            said.contains("no trigger"),
            "an empty trigger set must read as off and say which switch to use: {said}"
        );
    }

    /// **A ceiling this build cannot read is the narrowest one, not the usual one.**
    ///
    /// The default is `Comment`, which is what a repo gets when nobody chose. A word written by a
    /// NEWER skein is a different question, and answering it with the default would let a
    /// downgrade widen what skein does unattended — the one direction a permission must never fail.
    ///
    /// Sabotage: `unwrap_or_default()` in `ceiling_or_nothing` and this reads `Comment`.
    #[test]
    fn a_ceiling_from_a_newer_skein_narrows_rather_than_widens() {
        let mk = |c: &str| -> Repo {
            serde_json::from_value(serde_json::json!({
                "id": "acme", "source": "https://example.invalid/a.git", "store": "",
                "auto_review_ceiling": c
            }))
            .expect("a ceiling never fails the whole registry entry")
        };
        assert_eq!(mk("approve").auto_review_ceiling, Ceiling::Approve);
        assert_eq!(mk("comment").auto_review_ceiling, Ceiling::Comment);
        assert_eq!(
            mk("merge-and-deploy").auto_review_ceiling,
            Ceiling::None,
            "a permission this build does not understand must not be read as one it does"
        );
        // And absent is the chosen default rather than the unreadable one: nobody wrote anything,
        // which is not the same as somebody writing something incomprehensible.
        let absent: Repo = serde_json::from_value(serde_json::json!({
            "id": "acme", "source": "https://example.invalid/a.git", "store": ""
        }))
        .unwrap();
        assert_eq!(absent.auto_review_ceiling, Ceiling::Comment);
        // Ordered by consequence, which is what makes a ceiling a ceiling.
        assert!(Ceiling::None < Ceiling::Comment && Ceiling::Comment < Ceiling::Changes);
        assert!(Ceiling::Changes < Ceiling::Approve);
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
