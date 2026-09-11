//! What this machine keeps on disk between refreshes.
//!
//! Three things, all under [`review_dir`] and all host-side and private: the PR numbers you set
//! aside, the ones you snoozed and at which head, and the last queue read for the repo — so
//! opening the tab paints last night's pull requests instead of a blank panel.

use super::*;

// ───────────────────────────── the archive ─────────────────────────────

/// Where a repo's review state lives: `~/.skein/review/<repo-id>/`.
///
/// Host-side and private, never the repo and never the shared `.claude` store — that store is
/// mounted into every box for the repo, and skein's rule is that runtime state and caches do not go
/// there. It is also the answer you gave for module docs: private first.
///
/// **It joins `repo_id` unchecked, and that is only safe because of who may reach it.** Every
/// production caller passes an id that came out of `repos.json`, and [`crate::repos::add_repo`] is
/// the one place an id is ever put there — so it refuses a `repo_id` that is not
/// [`crate::util::valid_name`], and the invariant holds for everything read back. The exceptions
/// are the writers below, which are reachable from a route with a raw URL segment, so they check
/// for themselves rather than trusting their caller.
pub fn review_dir(repo_id: &str) -> PathBuf {
    skein_home().join("review").join(repo_id)
}

/// The refusal both writers below share.
///
/// It is a *safety* check and not a semantic one: it says "this string can be a path component",
/// not "this repo exists". The routes ask the second question by resolving the id through
/// `load_repos()`, the way their fifteen siblings do — but a route is not the only caller and the
/// two that forgot are why this is here as well. `..%2F..%2Ftmp%2Fx` reaches an axum `Path<String>`
/// as `../../tmp/x`; measured, not assumed.
fn usable_repo_id(repo_id: &str) -> Result<(), String> {
    match valid_name(repo_id) {
        true => Ok(()),
        false => Err(format!("unusable repo id {repo_id:?}")),
    }
}

fn archive_path(repo_id: &str) -> PathBuf {
    review_dir(repo_id).join("archived.json")
}

/// PR numbers you have set aside in this repo.
pub fn archived(repo_id: &str) -> Vec<u64> {
    fs::read_to_string(archive_path(repo_id))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<u64>>(&t).ok())
        .unwrap_or_default()
}

/// Archive or unarchive one PR. Idempotent in both directions.
///
/// **Read, change and write under the file's own lock** ([`crate::util::update_json`]), because
/// this is not the only writer: [`prune_archived`] runs inside every queue refresh, and a refresh
/// takes long enough that a click lands inside one routinely. The read this changes has to be the
/// read the lock covers, so it happens in the closure and not before it.
pub fn set_archived(repo_id: &str, number: u64, on: bool) -> Result<(), String> {
    usable_repo_id(repo_id)?;
    update_json(&archive_path(repo_id), |list: &mut Vec<u64>| {
        match (on, list.contains(&number)) {
            (true, false) => list.push(number),
            (false, true) => list.retain(|n| *n != number),
            _ => {}
        }
        Ok(())
    })
}

/// Drop archive entries whose pull request is no longer open, **re-reading the file under the
/// lock** so a set-aside made while this refresh was in flight survives it.
///
/// `seen` is the copy [`queue_within`] sampled at the top of the refresh, and it is used for
/// exactly one thing: deciding whether there is anything to prune, so a repo with nothing set
/// aside neither takes the lock nor creates the file. It is deliberately **not** what gets
/// written. The sample and the write are ~200 lines and a GraphQL round trip apart, `set_archived`
/// runs from a route in between, and writing the sample back is how that click disappeared.
///
/// A refusal is dropped rather than raised: [`crate::util::update_json`] refuses on a file it
/// could not read, and leaving an unreadable archive alone errs in the safe direction — a hold
/// that is kept shows a row that a person already asked not to see, where a hold that is lost
/// silently loses their decision.
pub(super) fn prune_archived(repo_id: &str, open: &[u64], seen: &[u64]) {
    if !seen.iter().any(|n| !open.contains(n)) {
        return;
    }
    let _ = update_json(&archive_path(repo_id), |list: &mut Vec<u64>| {
        list.retain(|n| open.contains(n));
        Ok(())
    });
}

fn snooze_path(repo_id: &str) -> PathBuf {
    review_dir(repo_id).join("snoozed.json")
}

/// PRs set aside *until their head moves*: number → the head sha it was set aside at.
///
/// A second store beside [`archived`] rather than a flag on it, because the two end differently
/// and mixing them loses the ending: an archive holds until a human undoes it, a snooze holds
/// until the BRANCH answers — the next push is the author acting on the red the snooze was
/// waiting out, which is exactly the moment the row should return by itself (SKEIN-144). The sha
/// is what makes that automatic: an entry whose sha no longer matches the open PR's head is
/// simply ignored, so un-snoozing needs no poller and no act.
pub fn snoozed(repo_id: &str) -> BTreeMap<u64, String> {
    fs::read_to_string(snooze_path(repo_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Snooze one PR at a head, or (`None`) bring it back by hand. Idempotent, like [`set_archived`]:
/// a retried request must not flip a row back out of where you already moved it.
///
/// The ordinary ending is nobody calling the `None` arm at all — a push stops the sha matching
/// and the row returns on its own.
/// Under the snooze file's own lock, for the reason [`set_archived`] gives for the archive.
pub fn set_snoozed(repo_id: &str, number: u64, head_sha: Option<&str>) -> Result<(), String> {
    usable_repo_id(repo_id)?;
    update_json(&snooze_path(repo_id), |map: &mut BTreeMap<u64, String>| {
        match head_sha {
            // An empty sha would hide the row forever on a PR whose head GitHub did not report —
            // build_pr refuses to match it, so refusing to store it keeps the file free of dead
            // weight.
            Some(sha) if !sha.is_empty() => {
                map.insert(number, sha.to_string());
            }
            _ => {
                map.remove(&number);
            }
        }
        Ok(())
    })
}

/// [`prune_archived`] for the snoozes: `seen` gates, and what is written is re-read under the lock.
pub(super) fn prune_snoozed(
    repo_id: &str,
    seen: &BTreeMap<u64, String>,
    live: impl Fn(&u64, &str) -> bool,
) {
    if !seen.iter().any(|(n, sha)| !live(n, sha)) {
        return;
    }
    let _ = update_json(&snooze_path(repo_id), |map: &mut BTreeMap<u64, String>| {
        map.retain(|n, sha| live(n, sha));
        Ok(())
    });
}

// ───────────────────────────── what to show before the answer ─────────────────────────────

/// Where a repo's last queue is kept between runs.
fn remembered_path(repo_id: &str) -> PathBuf {
    review_dir(repo_id).join("queue.json")
}

/// Keep this queue for the next cold start. Best-effort: failing to cache is not failing.
pub(super) fn remember(q: &Queue) {
    let dir = review_dir(&q.repo_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec(q) {
        let _ = write_atomic(&remembered_path(&q.repo_id), &dir, &bytes);
    }
}

/// Test-only: put a queue where [`remembered`] reads it, for tests elsewhere in the crate.
///
/// [`queue_within`] neither caches nor remembers in this crate's unit tests unless one asks with
/// [`CachedQueues`], so a test's world has no remembered queue in it unless it says so — and "no
/// remembered queue" is a state a real post is almost never in, because the pane must have
/// rendered this repo for a draft to exist at all. A write path's test that wants the state it
/// will actually run in seeds it here; a test that wants the whole cache, live, takes the guard.
#[cfg(test)]
pub(crate) fn remember_for_test(q: &Queue) {
    remember(q);
}

/// The last queue read for this repo, however old — marked as not fresh.
///
/// **Whatever exists, immediately.** Opening the tab used to block on three GraphQL searches per
/// repo plus the viewer lookup, and on a cold cache — a fresh server, a repo not looked at yet,
/// any refresh past the micro-cache — it painted nothing until they all came back. The thing it was
/// being compared against was a blank panel, and last night's pull requests beat a blank panel every
/// time so long as their age is on screen.
///
/// `fresh` is forced false here rather than trusted from the file: what was written was fresh when
/// it was written, and the one thing this must never do is hand somebody an old queue that claims
/// to be current.
pub fn remembered(repo_id: &str) -> Option<Queue> {
    let text = std::fs::read_to_string(remembered_path(repo_id)).ok()?;
    let mut q: Queue = serde_json::from_str(&text).ok()?;
    q.fresh = false;
    Some(q)
}

/// The head this repo's queue last SAW for one pull request, from what is already on this machine.
/// (SKEIN-272)
///
/// **Reads nothing over the network, and never refreshes.** That is the point: it is the fallback
/// [`head_to_post_against`] uses when GitHub will not say what the live head is, and a fallback
/// that could itself fail over the network would put the read failure back on the write path this
/// exists to take it off.
///
/// Why a write wants it at all is SKEIN-230. The comparison that decides whether anything needs
/// re-anchoring is "the sha the draft was read at" against "the sha being posted against", and
/// handing the drafted sha in as its own fallback makes those two equal by construction: nothing
/// re-anchors, and vetted comments post at line numbers computed against a diff that no longer
/// exists. A remembered sha is independent evidence — possibly stale, but never the same value by
/// accident.
///
/// `None` when nothing about this pull request is remembered, which is honest: the caller then has
/// no second opinion and must say so rather than invent one.
pub fn remembered_head(repo_id: &str, number: u64) -> Option<String> {
    let known = unexpired(repo_id).or_else(|| remembered(repo_id))?;
    known
        .prs
        .iter()
        .find(|p| p.number == number)
        .map(|p| p.head_sha.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prq::fixtures::node;
    use crate::prq::node::build_pr;

    /// A fresh `$SKEIN_HOME`, plus the guards that put it back — the same fixture
    /// [`crate::gitgate`]'s tests use, for the same reason: the archive is a file under it.
    ///
    /// **Destructure it**, rather than binding the tuple whole. The pins come last so they drop
    /// first — bindings from one `let` are dropped in reverse — and `$SKEIN_HOME` has to stop naming
    /// the temp directory before the temp directory is removed. A tuple bound to one name drops its
    /// fields the other way round, which is why every caller here spells all three out.
    fn fresh_home() -> (
        crate::testutil::EnvGuard,
        crate::testutil::TempDir,
        crate::testutil::EnvPins,
    ) {
        let lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let mut pins = crate::testutil::env_pins();
        pins.set("SKEIN_HOME", home.as_ref() as &std::path::Path);
        (lock, home, pins)
    }

    #[test]
    fn archiving_is_idempotent_in_both_directions() {
        let (_lock, _home, _pins) = fresh_home();
        set_archived("r", 7, true).unwrap();
        set_archived("r", 7, true).unwrap();
        assert_eq!(archived("r"), vec![7]);
        set_archived("r", 7, false).unwrap();
        set_archived("r", 7, false).unwrap();
        assert!(archived("r").is_empty());
    }

    #[test]
    fn archives_are_per_repo() {
        let (_lock, _home, _pins) = fresh_home();
        set_archived("one", 4, true).unwrap();
        assert_eq!(archived("one"), vec![4]);
        assert!(archived("two").is_empty());
    }

    /// **A set-aside made while a refresh is in flight survives that refresh's prune.**
    ///
    /// The interleaving as it actually happens, played in order rather than raced. [`queue_within`]
    /// samples the archive at the top of a refresh, spends a GraphQL round trip and ~200 lines
    /// building the queue, and prunes at the bottom. A `POST …/archive` from the cockpit lands in
    /// that gap routinely — and the prune used to write back the copy it sampled, which does not
    /// have the click in it. One press of a button, gone, with nothing anywhere to say it happened.
    ///
    /// **What would make this fail:** having [`prune_archived`] write `seen` filtered by `open`
    /// instead of re-reading the file inside the lock — which is exactly what the code did before.
    /// #5 then vanishes and this reads `[]`. Verified by making that change and watching it go.
    #[test]
    fn a_set_aside_made_during_a_refresh_survives_the_prune() {
        let (_lock, _home, _pins) = fresh_home();
        // #7 was set aside a while ago; its pull request has since closed, so the prune wants it.
        set_archived("r", 7, true).unwrap();

        // The refresh begins and samples the archive — `queue_within`'s `archived(&repo.id)`.
        let sampled = archived("r");
        assert_eq!(sampled, vec![7], "the fixture did not set anything aside");

        // Mid-refresh, from the route: a person sets #5 aside.
        set_archived("r", 5, true).unwrap();

        // The refresh finishes, having been told #5 is the only open pull request.
        prune_archived("r", &[5], &sampled);

        assert_eq!(
            archived("r"),
            vec![5],
            "the prune wrote back the list it read before the click, so the click never happened"
        );
    }

    /// [`a_set_aside_made_during_a_refresh_survives_the_prune`] for snoozes — same gap, same route,
    /// same loss, and the snooze prune is the second of the three writers that had no lock.
    ///
    /// **What would make this fail:** writing `seen` back from [`prune_snoozed`] instead of
    /// re-reading under the lock. #5's snooze is then dropped and the row comes back unhidden.
    #[test]
    fn a_snooze_made_during_a_refresh_survives_the_prune() {
        let (_lock, _home, _pins) = fresh_home();
        set_snoozed("r", 7, Some("closed7")).unwrap();
        let sampled = snoozed("r");

        set_snoozed("r", 5, Some("live5")).unwrap();

        // Only #5 at `live5` is still open at that head.
        prune_snoozed("r", &sampled, |n, sha| *n == 5 && sha == "live5");

        assert_eq!(
            snoozed("r"),
            BTreeMap::from([(5u64, "live5".to_string())]),
            "the prune wrote back the map it read before the snooze was stored"
        );
    }

    /// **An archive that will not parse is not written over.**
    ///
    /// [`archived`] reads an unreadable file as `[]`, and the writer used to take that empty list,
    /// add to it and write it back — turning "skein cannot read this" into "there was nothing in
    /// it", which is how a file holding somebody's decisions is destroyed by one click.
    /// [`crate::util::update_json`] refuses instead and names what it would have replaced.
    ///
    /// **What would make this fail:** going back to `archived()` + `write_atomic`, or reaching for
    /// `update_json_lossy`. Either returns `Ok` and leaves `[9]` where the file used to be.
    #[test]
    fn an_unreadable_archive_is_refused_rather_than_replaced() {
        let (_lock, _home, _pins) = fresh_home();
        let path = archive_path("r");
        std::fs::create_dir_all(review_dir("r")).unwrap();
        let half_an_edit = "[7, 8,";
        std::fs::write(&path, half_an_edit).unwrap();

        let refused = set_archived("r", 9, true)
            .expect_err("an unreadable archive was written over as though it were empty");
        assert!(
            refused.contains("skein cannot read it"),
            "the refusal does not say why, so nobody can act on it: {refused}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            half_an_edit,
            "the file somebody's set-asides were in was replaced anyway"
        );

        // And the prune, whose refusal is dropped, errs the same way: it leaves the file alone
        // rather than pruning from a list it could not read.
        prune_archived("r", &[], &[7]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            half_an_edit,
            "the prune emptied a file it could not read"
        );
    }

    /// SKEIN-144: set aside *until the head moves*. The snooze names the sha it was taken at, so
    /// the author's next push — not an act, not a timer — is what brings the row back: the entry
    /// stops matching and is ignored.
    #[test]
    fn a_snooze_holds_a_pr_only_at_the_head_it_was_set_aside_at() {
        let held = BTreeMap::from([(3u64, "abc".to_string())]);

        let same = node(r#"{"number":3,"headRefOid":"abc","author":{"login":"someone"}}"#);
        let pr = build_pr(&same, 3, "me", &Reason::Reviewer, &[], &held);
        assert_eq!(
            pr.lane,
            Lane::Archived,
            "out of Needs you while the head sits"
        );
        assert!(pr.snoozed, "the row can say WHY it is set aside");

        let moved = node(r#"{"number":3,"headRefOid":"def","author":{"login":"someone"}}"#);
        let pr = build_pr(&moved, 3, "me", &Reason::Reviewer, &[], &held);
        assert_eq!(
            pr.lane,
            Lane::NeedsYou,
            "the push IS the un-snooze — the row returns with no action"
        );
        assert!(!pr.snoozed);

        // "GitHub did not say" must never be what keeps a row hidden: an absent head matches no
        // snooze, even one whose stored sha is somehow empty too.
        let unknown = node(r#"{"number":9,"author":{"login":"someone"}}"#);
        let empty_sha = BTreeMap::from([(9u64, String::new())]);
        let pr = build_pr(&unknown, 9, "me", &Reason::Reviewer, &[], &empty_sha);
        assert_eq!(pr.lane, Lane::NeedsYou);

        // Archived outright is the other instrument, and the reason stays distinguishable.
        let pr = build_pr(&same, 3, "me", &Reason::Reviewer, &[3], &BTreeMap::new());
        assert_eq!(pr.lane, Lane::Archived);
        assert!(!pr.snoozed, "archived-forever is not a snooze");
    }

    #[test]
    fn snoozes_are_idempotent_re_aimable_and_cleared_by_hand_with_none() {
        let (_lock, _home, _pins) = fresh_home();
        set_snoozed("r", 7, Some("abc")).unwrap();
        set_snoozed("r", 7, Some("abc")).unwrap();
        assert_eq!(snoozed("r"), BTreeMap::from([(7u64, "abc".to_string())]));

        // Snoozing again at a newer head re-aims the hold rather than stacking one.
        set_snoozed("r", 7, Some("def")).unwrap();
        assert_eq!(snoozed("r"), BTreeMap::from([(7u64, "def".to_string())]));

        set_snoozed("r", 7, None).unwrap();
        set_snoozed("r", 7, None).unwrap();
        assert!(snoozed("r").is_empty());

        // An empty sha is refused, not stored: build_pr would never match it, so storing it could
        // only ever be dead weight in the file.
        set_snoozed("r", 9, Some("")).unwrap();
        assert!(snoozed("r").is_empty());

        // Per repo, like the archive.
        set_snoozed("one", 4, Some("s")).unwrap();
        assert!(snoozed("two").is_empty());
    }

    /// **A repo id that is a path cannot write a holding file outside the review directory.**
    ///
    /// [`review_dir`] joins its id unchecked on purpose — twelve production callers read an id back
    /// out of `repos.json`, and [`crate::repos::add_repo`] is the only thing that ever puts one
    /// there — so the guard that has to hold is [`usable_repo_id`], on the two writers a route can
    /// reach with a raw URL segment.
    ///
    /// Nothing reached it. `tests/server.rs`'s traversal test asks the *route*, whose `load_repos()`
    /// lookup answers "no such repo" before the writer is called, so that test stays **green with
    /// this guard deleted** — measured by deleting it, not reasoned. A guard no test can reach is a
    /// guard the next refactor removes silently.
    ///
    /// **What would make this fail:** making [`usable_repo_id`] return `Ok(())` unconditionally.
    /// Done, and the `climb` row goes red naming the id it accepted. The containment assertion was
    /// proved separately, by making that same change *and* neutering the two in-loop assertions:
    /// the archive is then written under `<parent of $SKEIN_HOME>/skein-prq-escape-<pid>`, which is
    /// the escape itself rather than a claim about a string.
    #[test]
    fn a_traversing_repo_id_cannot_write_a_holding_file_outside_the_review_directory() {
        let (_lock, home, _pins) = fresh_home();

        // The present half, so the refusals below are measured against a write that does happen.
        set_archived("r", 7, true).expect("an ordinary repo id sets a pull request aside");
        set_snoozed("r", 8, Some("abc")).expect("an ordinary repo id snoozes one");
        assert_eq!(archived("r"), vec![7]);
        assert!(
            home.join("review/r/archived.json").exists(),
            "the ordinary archive left no file, which would make every refusal below vacuous"
        );

        // One level above `$SKEIN_HOME` — where `../../` from `<home>/review/<id>` lands. The pid
        // is in the name because test binaries share a parent directory and run concurrently.
        let marker = (home.as_ref() as &std::path::Path)
            .parent()
            .expect("a temporary directory has a parent")
            .join(format!("skein-prq-escape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&marker);
        let climb = format!("../../{}", marker.file_name().unwrap().to_string_lossy());

        // Named inputs rather than a generic "some bad id": a path that climbs, the bare climb, a
        // separator, nothing at all, a NUL, an absolute path, and the single dot that
        // `Path::join` resolves to the parent itself.
        for climbing in [climb.as_str(), "..", "a/b", "", "x\0y", "/etc/passwd", "."] {
            assert!(
                set_archived(climbing, 7, true).is_err(),
                "{climbing:?} was accepted as a repo id and set a pull request aside"
            );
            assert!(
                set_snoozed(climbing, 7, Some("abc")).is_err(),
                "{climbing:?} was accepted as a repo id and snoozed a pull request"
            );
        }
        assert!(
            !marker.exists(),
            "{} was created: a repo id from a request wrote outside SKEIN_HOME",
            marker.display()
        );
    }
}
