//! The box skein opens to read one pull request — everything about its life except the reading.
//!
//! `docs/pr-review.md` §11, which is the owner's decision that the reviewer is *"not a new class of
//! sessions but just box, managed automatically"*. That decision deleted four mechanisms an earlier
//! draft had proposed — a cgroup, a memory cap, a cleanup rule, a pause between rounds — because a
//! box already has all four. What it did not delete is the **lifecycle**, and this is that:
//! *created* on the first round, *stopped* between rounds, *started again* when a trigger fires,
//! *destroyed* when the pull request closes.
//!
//! # Why the teardown was written before the create path
//!
//! The same rule as the kill switch and the audit in [`crate::prwork`]: the thing that ends a box
//! is written before the thing that makes one. A create path shipped without its teardown is a
//! fleet that accumulates checkouts nobody asked for, and it accumulates them silently — the boxes
//! are grouped as skein's own, so they do not even look wrong on the board. Both halves are here
//! now — [`close_finished`] and [`open_at`] — and the order they were written in is the reason
//! there was never a window in which one existed without the other.
//!
//! # What reaches this
//!
//! **Every reading does, and `skein-server`'s own process is the fallback rather than the rule.**
//! `review::conversation_of` asks `review::at_a_review_box` before it does anything else, and that
//! calls [`open_at`] — so the box is opened first and the reading happens inside it. The old path
//! is taken only when the box is declined: the repo may not be read, or the fleet is already at
//! [`AT_ONCE`] and this pull request has no box of its own, or the box will not start or will not
//! stand at the head. Each of those is printed rather than swallowed, and then read the way it was
//! read before any of this existed.
//!
//! So [`close_finished`], which runs from the queue's housekeeping pass, is no longer a no-op
//! waiting on a first box — there are boxes for it to find on every repo that reads pull requests,
//! and the conservative rules below are load-bearing rather than theoretical. `docs/pr-review.md`
//! §15 marks step 3 done in both halves: 3a wired `Read` to the reading, 3b moved where that
//! reading runs, and 3b is this module.
//!
//! # The one asymmetry that shapes every decision below
//!
//! [`crate::review::prune`] deletes a *reading* and says of its own risk: *"deleting a summary costs
//! one re-read; keeping one costs a few kilobytes, and only one of those is irreversible."*
//!
//! **A box is not a file.** Destroying one takes a checkout and a conversation with it, and the
//! conversation is the whole reason §11 chose a box over a directory. So every rule here is the
//! conservative half of `prune`'s: absence from a queue is never death, an unreadable queue decides
//! nothing, and only GitHub saying *closed* in as many words destroys anything.

use crate::place::Purpose;
use crate::repos::Repo;
use std::time::Duration;

/// How many review boxes may exist at once, across the fleet.
///
/// **A cap on boxes, which is what §11 says it is** — *"the same unanswered question skein already
/// has, not a new one."* A box's memory ceiling is 70% of the whole pool, and ceilings are not
/// reservations: they stop one box killing the sandbox, not five exhausting it together. So the
/// number is small on purpose, and it bounds boxes rather than readings — `READINGS_PER_SWEEP`
/// bounds the rate, this bounds the standing footprint.
///
/// Two rather than one, because a stopped box costs almost nothing and one review that is slow to
/// finish should not block every other repo's first round for as long as it runs.
pub const AT_ONCE: usize = 2;

/// The pull request a review box was opened for, or `None` if this is not a review box's name.
///
/// **The exact inverse of [`crate::repos::review_box_name`]**, and it has to be: the forward
/// direction names a box and this one is how teardown finds it again. A test pins the round trip,
/// because a drift here does not fail — it leaves boxes standing that nothing will ever destroy.
///
/// Split on the LAST `-pr-`, and the tail must be all digits. A repo whose id itself contains
/// `-pr-` would otherwise split in the wrong place; splitting from the right means the only string
/// that can be misread is one where the repo id ends in `-pr-<digits>`, and the caller checks the
/// repo id it got back against the one it asked about.
pub fn number_in(repo_id: &str, box_name: &str) -> Option<u64> {
    let tail = box_name.strip_prefix(repo_id)?.strip_prefix("-pr-")?;
    // `parse` alone would accept `+7` and `  7`, which are not names this ever wrote.
    (!tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()))
        .then(|| tail.parse().ok())
        .flatten()
}

/// Every review box this fleet is holding for `repo_id`, as `(box name, pull request number)`.
///
/// Read off the **placement records**, which is where a box's purpose is written — never off its
/// name. A name is a guess and a placement record is a statement, and `fleet::refuse_a_repurpose`
/// already makes the same choice for the same reason: two boxes can be named alike and only one of
/// them can be skein's.
///
/// **`== Purpose::Review`, not `Purpose::managed()`**, and the difference is deliberate against
/// that method's own advice. `managed` is written as "not Manual" so a purpose added later is
/// managed by default, which is right for the board — a box skein drives must never be filed among
/// the ones a person is responsible for. It is wrong here: this list feeds a teardown, and a
/// purpose this build has never heard of is not one it may destroy on a rule written before it
/// existed. Grouping fails open; destroying fails closed.
pub fn theirs(repo_id: &str) -> Vec<(String, u64)> {
    let sandbox = crate::place::fleet_sandbox();
    // **Unreachable today, and kept anyway.** `config::load_config` substitutes the default fleet
    // name for an empty one before anybody sees it, so `place::fleet_sandbox` cannot answer with
    // nothing — which is why the test that used to sit under this guard was deleted: it asserted a
    // state the config layer makes impossible, and it went on passing with the guard removed.
    // The guard stays because of what is downstream of this list rather than because of what
    // reaches it: `placed_boxes("")` would answer about every record that names no sandbox, and
    // that set feeds a function which destroys boxes.
    if sandbox.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(String, u64)> = crate::place::placed_boxes(&sandbox)
        .into_iter()
        .filter(|(_, record)| record.purpose == Purpose::Review)
        .filter_map(|(name, _)| Some((name.clone(), number_in(repo_id, &name)?)))
        .collect();
    found.sort_by_key(|(_, number)| *number);
    found
}

/// **Is this pull request's review box finished with?** Pure, so the rule can be read on its own.
///
/// Three inputs and only one of them can say yes:
///
/// * `open` — the numbers in a queue that was read whole and fresh. Present means alive, and no
///   question is asked. Absent means *nothing*: the searches behind a queue are
///   `review-requested:you`, `author:you` and `mentions:you`, so a pull request drops out of it the
///   moment a review request is reassigned, and it is still very much open.
/// * `asked` — [`crate::prq::pr_is_open`]'s answer to the actual question. `Some(false)` is the
///   only value that destroys anything.
/// * `None` — GitHub could not say. Keeps the box, and is not remembered, so the next pass asks
///   again rather than treating one bad minute as a verdict.
///
/// **This is `review::prune`'s rule with its cheap half removed.** `prune` also deletes on a
/// superseded head, because a summary keyed to an old commit can never be read again. A box has no
/// such rule: the same box stands at whatever head the pull request is at now, which is the point
/// of `fleet::stand_at_head_script`, and a head that moved is a reason to *use* it rather than to
/// end it.
fn finished(number: u64, open: &[u64], asked: Option<bool>) -> bool {
    if open.contains(&number) {
        return false;
    }
    asked == Some(false)
}

/// Destroy the review boxes whose pull requests are over. Returns what it destroyed.
///
/// `open` must come from a queue that was read **whole and fresh** — the caller's job, and the same
/// gate `review::prune`'s caller applies (`prunable`). A truncated queue read as complete would
/// present every pull request past the cap as absent, and absence here is one GitHub call away from
/// destroying a live box's checkout.
///
/// **`ask` is the caller's, and that is not a style choice.** The question is
/// `prq::pr_is_open`'s, and reaching for it here would put this module inside the `{prq, review}`
/// cycle — around the one module that destroys boxes, whose own note asks for the opposite. The
/// gate caught it. Taking the answer instead of the asker leaves the edge out and makes this
/// function testable at the same time, which is the shape worth noticing: the dependency that was
/// hard to justify was also the one making the code hard to prove.
///
/// Best-effort per box: one that will not tear down is reported and the rest still go, because a
/// single stuck box must not leave the fleet accumulating the others.
pub fn close_finished(
    repo_id: &str,
    open: &[u64],
    ask: impl Fn(u64) -> Option<bool>,
) -> Vec<String> {
    let mut gone = Vec::new();
    for (name, number) in theirs(repo_id) {
        // Asked only for boxes the queue does not account for, and once each: the call is cheap but
        // not free, and asking about a pull request that is sitting in the queue in front of us is
        // a call whose answer we already have.
        if open.contains(&number) {
            continue;
        }
        if !finished(number, open, ask(number)) {
            continue;
        }
        match crate::sandbox::destroy_box(&name) {
            Ok(()) => {
                forget_the_conversation(&name);
                gone.push(name);
            }
            Err(why) => eprintln!(
                "skein: #{number} is closed and its review box {name} is still here — {why}"
            ),
        }
    }
    gone
}

/// **Take the conversation with the box** — for a review box, and only from here.
///
/// `sandbox::destroy_box` removes `/boxes/<name>`: the checkout, the session, the cgroups. It does
/// not touch `$SKEIN_HOME/boxes/<name>`, where `box-session.sh` binds the box's `.claude/projects`
/// and `.codex/sessions` from — so a destroyed box leaves its conversation on the host. Measured on
/// the owner's fleet, 2026-09-03: twenty-seven per-box directories against nine boxes, 1.2 GB
/// belonging to boxes that no longer exist.
///
/// This module's own header says destroying a box "takes a checkout and a conversation with it".
/// It takes the checkout. So either the header or the behaviour was wrong, and for a REVIEW box
/// there is no question which: [`close_finished`] destroys one per closed pull request, so every
/// closed pull request would leave a directory behind for ever — the silent accumulation this file
/// was written before any create path to prevent.
///
/// **Narrow on purpose.** `destroy_box` keeps its behaviour for work boxes, where a transcript that
/// outlives the box is plausibly the point — somebody may want to read what a box did after
/// deciding they are done with it. A review box's reading is already stored host-side under
/// `review/<repo>/summaries/`, which is why it survives this; the box conversation has no reader
/// once the pull request is closed.
///
/// Best-effort and quiet on absence: a box that never opened a conversation has nothing here, and a
/// failure to remove one must not turn a completed teardown into a reported failure.
fn forget_the_conversation(name: &str) {
    if !crate::util::valid_name(name) {
        return;
    }
    let dir = std::path::PathBuf::from(crate::fleet::box_state(name));
    // Under the state root and not equal to it, checked rather than assumed: everything below is a
    // recursive delete, and `box_state("")` would be the root itself.
    let root = std::path::PathBuf::from(crate::fleet::box_state_root());
    if !dir.starts_with(&root) || dir == root {
        return;
    }
    if let Err(why) = std::fs::remove_dir_all(&dir) {
        if why.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "skein: {name} is destroyed but its conversation is still at {} — {why}",
                dir.display()
            );
        }
    }
}

/// May another review box be opened? `None` when there is room, a sentence when there is not.
///
/// A sentence rather than a bool because the caller is going to report it: a round that does not
/// happen because the fleet is full is the ordinary state of a busy queue, and one that presents as
/// nothing happening is the silence every guard in this design exists to avoid.
pub fn room_for_another(standing: usize) -> Option<String> {
    (standing >= AT_ONCE).then(|| {
        format!(
            "the fleet is already holding {standing} review boxes, which is the limit ({AT_ONCE}) \
             — this one waits for a round to finish rather than crowding them"
        )
    })
}

/// **Open this pull request's review box and stand it at `head_sha`.** Idempotent by design.
///
/// Three steps, and the first two are already written:
///
/// 1. `fleet::start_box` with [`Purpose::Review`], which refuses rather than adopts if a box of
///    that name exists for another purpose (`fleet::refuse_a_repurpose`), and keeps the checkout
///    and the session if this one already exists.
/// 2. `fleet::stand_at_head_script`, run in the sandbox exactly as `fleet::snapshot_box` runs its
///    own in-checkout script — the script does its own `cd`, fetches `refs/pull/<n>/head` when the
///    commit is not there already, checks out detached, and cleans the tree. Detached because there
///    is no branch to be on, and because nothing about reviewing should be able to push.
/// 3. The round itself, which is **not here**: giving the box its instruction is the dispatch, and
///    that is the next increment. What this returns is a box standing at the right commit.
///
/// Called on **every** round, not only the first. That is what makes it the answer to §11's "a box
/// comes up at the wrong commit": step 1 is a no-op on a box that exists and step 2 moves it, so
/// round N is the same call as round one.
pub fn open_at(repo: &Repo, number: u64, head_sha: &str) -> Result<String, String> {
    let name = crate::repos::review_box_name(&repo.id, number);
    // The base branch, not the pull request's. A review box never stands on a branch at all — step
    // 2 detaches it — so what this decides is only what the clone starts from, and the base is the
    // branch whose history makes `git merge-base` resolve.
    let base = crate::fleet::base_branch(repo);
    // **A box that is already up and provisioned for its current start needs none of `start_box`.**
    // Measured on 2026-09-03, verifying §15 step 3: every reading calls this, `start_box_inner`
    // adopts what it finds — `already has a checkout; keeping it`, `already has a live session;
    // keeping it` — and then provisions anyway, because it provisions unconditionally. Provisioning
    // IS the cost of a box start, so round two of a reading paid the whole of it before its model
    // call could begin.
    //
    // Skipped here rather than in `start_box`, and the difference matters: `skein start` on a live
    // box is how a box picks up a new build's kit and hooks, so making that path conditional would
    // trade this cost for boxes running yesterday's kit. A review box does not need that — it runs
    // one model call and is destroyed when its pull request closes — so the exemption belongs to
    // the caller that can justify it.
    //
    // The purpose is checked before skipping because skipping also skips
    // `fleet::refuse_a_repurpose`, and that guard is the one thing here that prevents skein
    // adopting somebody's work box. A record that does not already say `Review` goes the long way.
    if !stands_already(&name) {
        crate::fleet::start_box(&name, repo, &base, "exec bash -l", Purpose::Review)?;
    }
    let Some(record) = crate::place::shared_record(&name) else {
        return Err(format!(
            "{name} started and left no placement record, so skein cannot reach it to stand it at \
             {head_sha}"
        ));
    };
    crate::place::own_sandbox(&record.sandbox)
        .exec(
            &crate::fleet::stand_at_head_script(&name, number, head_sha),
            Duration::from_secs(300),
        )
        .map(|_| name)
}

/// Whether this name already IS a review box that is up and provisioned, so [`open_at`] may go
/// straight to standing it at the head.
///
/// Both halves, and neither is sufficient. Without the purpose check this would let a work box of
/// the same name skip the one guard that refuses a repurpose; without the readiness check it would
/// skip provisioning a box that has never had any.
fn stands_already(name: &str) -> bool {
    crate::place::shared_record(name).is_some_and(|r| r.purpose == Purpose::Review)
        && crate::fleet::box_is_ready(name)
}

/// **What is standing in this pull request's review box**, once [`open_at`] has put it at the head.
///
/// The commit the change starts from, or `None` when the box can be read and the change cannot —
/// a base branch this clone has never seen, most often. Best-effort throughout, and deliberately:
/// nothing downstream may tell a model it has the change without being told that it does, which is
/// `review::Standing`'s whole reason for being three values rather than a bool.
pub fn change_starts_at(name: &str, base_ref: &str) -> Option<String> {
    let record = crate::place::shared_record(name)?;
    let said = crate::place::own_sandbox(&record.sandbox)
        .exec(
            &crate::fleet::change_starts_script(name, base_ref),
            Duration::from_secs(60),
        )
        .ok()?;
    let sha = said.trim().to_string();
    // A merge base is a full sha or it is nothing. Anything else is a message that reached stdout,
    // and a message read as a commit is a prompt telling a model to `git diff` against a sentence.
    (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit())).then_some(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A destroyed review box does not leave its conversation on the host.**
    ///
    /// `sandbox::destroy_box` removes `/boxes/<name>` and nothing under `$SKEIN_HOME/boxes/<name>`,
    /// which is where the box's `.claude/projects` is bound from. Measured on the owner's fleet on
    /// 2026-09-03: twenty-seven per-box directories against nine live boxes, 1.2 GB of them
    /// belonging to boxes that were destroyed. For a review box that is unbounded by construction —
    /// one destroy per closed pull request, for ever.
    ///
    /// **What would make each half fail:** dropping the `remove_dir_all` leaves the directory, which
    /// is the leak; dropping the root guard turns a name that resolves to the state root into a
    /// recursive delete of every box's conversation, which is the one way this fix could be worse
    /// than the bug.
    #[test]
    fn a_destroyed_review_box_takes_its_conversation_with_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let root = std::path::PathBuf::from(crate::fleet::box_state_root());
        let mine = root
            .join("demo-pr-7")
            .join("claude-projects")
            .join("-boxes-demo-pr-7-tree");
        std::fs::create_dir_all(&mine).expect("a conversation");
        std::fs::write(mine.join("talk.jsonl"), "{}").expect("a transcript");
        let neighbour = root.join("someone-else").join("claude-projects");
        std::fs::create_dir_all(&neighbour).expect("a neighbour");

        forget_the_conversation("demo-pr-7");
        assert!(
            !root.join("demo-pr-7").exists(),
            "the box is gone and its conversation is still here, which is the leak this exists to \
             stop — and for a review box it is one per closed pull request, for ever"
        );
        assert!(
            neighbour.exists(),
            "another box's conversation went with it"
        );

        // The guard. An unusable name must reach nothing at all — `box_state("")` IS the root, and
        // a recursive delete there takes every box's conversation rather than one.
        forget_the_conversation("");
        forget_the_conversation("..");
        assert!(
            neighbour.exists() && root.exists(),
            "a name that resolves to the state root deleted every box's conversation"
        );

        // Absence is not a failure: a box that never opened a conversation has nothing here.
        forget_the_conversation("never-talked");

        std::env::remove_var("SKEIN_HOME");
    }

    /// Every name [`crate::repos::review_box_name`] writes reads back as the number it was made
    /// from, and nothing else does.
    ///
    /// **What would make this fail:** changing either function's separator without the other — the
    /// drift that leaves review boxes standing that teardown can never find, silently, because
    /// nothing about a box nobody looks for looks wrong.
    #[test]
    fn every_review_box_name_reads_back_as_the_pull_request_it_was_made_for() {
        for (repo, number) in [
            ("demo", 1u64),
            ("acme-web", 41),
            // A repo id that contains the separator, which is why the split is from the right.
            ("thing-pr-shop", 7),
            ("x", 18_446_744_073_709_551_615),
        ] {
            let name = crate::repos::review_box_name(repo, number);
            assert_eq!(
                number_in(repo, &name),
                Some(number),
                "{name} did not read back as #{number}"
            );
        }

        // And a name that is not one of ours is not read as one, in every way it can fail.
        for (repo, name) in [
            ("demo", "demo-feat"),        // an ordinary box on a branch
            ("demo", "demo-pr-"),         // the prefix and no number
            ("demo", "demo-pr-x"),        // a number that is not one
            ("demo", "demo-pr-+7"),       // parseable by `parse`, never written by us
            ("demo", "demo-pr- 7"),       // likewise
            ("demo", "other-pr-7"),       // another repo's review box
            ("demo", "demopr-7"),         // the separator missing
            ("demo", "demo-pr-7-pr-8xy"), // a tail that is not all digits
        ] {
            assert_eq!(number_in(repo, name), None, "{name} was read as {repo}'s");
        }
    }

    /// **Absence from a queue never destroys a box**, and neither does a GitHub call that could not
    /// answer. Only `closed`, said in as many words.
    ///
    /// The table is the whole rule, so a fifth input added later has to appear here to be reached.
    ///
    /// **What would make this fail:** writing `finished` as `!open.contains(&number)` — the
    /// inference `review::prune` explicitly refuses, because the searches behind a queue drop a
    /// pull request the moment its review request is reassigned. Rows two and three would then
    /// destroy a live box.
    #[test]
    fn only_github_saying_closed_ends_a_review_box() {
        let open = [7u64, 41];
        for (what, number, asked, want) in [
            ("in the queue, and nobody asked", 41, None, false),
            ("out of the queue, and GitHub could not say", 9, None, false),
            (
                "out of the queue, and GitHub says it is open",
                9,
                Some(true),
                false,
            ),
            (
                "out of the queue, and GitHub says it is closed",
                9,
                Some(false),
                true,
            ),
            // The one that matters most: a queue that still holds it outranks a stale answer,
            // because the queue was read this second and the call may be minutes old.
            (
                "in the queue, and a call said closed",
                7,
                Some(false),
                false,
            ),
        ] {
            assert_eq!(
                finished(number, &open, asked),
                want,
                "{what}: #{number} was {}",
                match want {
                    true => "kept",
                    false => "destroyed",
                }
            );
        }
    }

    /// **Only this repo's review boxes, in this fleet's sandbox, and no manual box ever.**
    ///
    /// The list this returns is the list a teardown will destroy from, so every one of the three
    /// filters is load-bearing and each has a box in the fixture that only it excludes.
    ///
    /// **What would make this fail:** reading the purpose as `managed()` rather than
    /// `== Purpose::Review` would still pass today — there is no third purpose to tell them apart
    /// — so that is NOT what this pins; it pins the other three. Dropping the purpose filter
    /// entirely puts `demo-feat` in the list, which is somebody's own box; dropping the sandbox
    /// scope puts another fleet's box in it; and a `number_in` that split from the left would miss
    /// `thing-pr-shop-pr-9` altogether.
    #[test]
    fn only_this_repos_review_boxes_in_this_sandbox_are_ours_to_end() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let mut config = crate::config::load_config();
        config.fleet_sandbox = "skein-fleet".into();
        crate::config::save_config(&config).unwrap();

        let place = |name: &str, sandbox: &str, purpose: Purpose| {
            crate::place::record_place(
                name,
                &crate::place::PlaceRecord {
                    sandbox: sandbox.into(),
                    purpose,
                    ..Default::default()
                },
            )
            .unwrap();
        };
        // Ours, and the one the whole feature is about.
        place("demo-pr-41", "skein-fleet", Purpose::Review);
        place("demo-pr-7", "skein-fleet", Purpose::Review);
        // A person's own box on a branch. Never ours to destroy, whatever it is called.
        place("demo-feat", "skein-fleet", Purpose::Manual);
        // A person's box that happens to be named like one of ours. The placement record is what
        // tells them apart, which is the whole reason the purpose is written down.
        place("demo-pr-99", "skein-fleet", Purpose::Manual);
        // Another repo's review box.
        place("other-pr-3", "skein-fleet", Purpose::Review);
        // Ours by name and purpose, in a fleet this skein is not driving.
        place("demo-pr-500", "another-fleet", Purpose::Review);

        assert_eq!(
            theirs("demo"),
            vec![("demo-pr-7".to_string(), 7), ("demo-pr-41".to_string(), 41)],
            "the list a teardown destroys from is not the boxes skein opened for this repo"
        );

        // And a repo id containing the separator is still found, which is why the split is from
        // the right — it is the case a left split silently loses.
        place("thing-pr-shop-pr-9", "skein-fleet", Purpose::Review);
        assert_eq!(
            theirs("thing-pr-shop"),
            vec![("thing-pr-shop-pr-9".to_string(), 9)]
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **The teardown, end to end** — which boxes are asked about, which are destroyed, and which
    /// are never asked about at all.
    ///
    /// Testable only because `close_finished` takes the asking rather than doing it: the edge that
    /// would have put this module in the `{prq, review}` cycle was the same one that made this
    /// unprovable. The destroy itself needs `sbx` and cannot run here, so what is pinned is
    /// everything up to it — and that is where the rules are.
    ///
    /// **What would make this fail:** asking about a pull request the queue already accounts for
    /// (row one of `asked`), which is a GitHub call per open pull request per pane-open; or acting
    /// on anything but `Some(false)`, which the counts below catch in both directions.
    #[test]
    fn the_teardown_asks_only_about_what_the_queue_cannot_account_for() {
        let _g = crate::testutil::env_lock();
        // Pinned where nothing listens, because this reaches `warden_client`: it refuses a
        // test process that has not said which warden to ask rather than opening a connection
        // to whatever warden the machine running the suite can reach (SKEIN-762).
        let _warden = crate::testutil::no_warden();
        // A stand-in for the crossing, because what is asserted below is the decision in FRONT
        // of it: `Place::spawning` refuses a test process that installed none rather than
        // running a fleet-scope command on this machine for real (SKEIN-530).
        let _crossing = crate::place::seam::doing_nothing();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        // **`close_finished` reaches `sandbox::destroy_box`, and that removes `<fleet root>/<box>`.**
        // Unpinned the root is `/boxes`, so this test runs a delete at the owner's live fleet and is
        // saved only by no real box being called `demo-pr-41`. A fixture root is what makes that a
        // property rather than a coincidence — the box names below are not real anywhere, and now
        // the directory they would be deleted from is not real either.
        std::env::set_var("SKEIN_FLEET_ROOT", home.join("boxes"));
        let mut config = crate::config::load_config();
        config.fleet_sandbox = "skein-fleet".into();
        crate::config::save_config(&config).unwrap();

        for name in ["demo-pr-7", "demo-pr-41", "demo-pr-9"] {
            crate::place::record_place(
                name,
                &crate::place::PlaceRecord {
                    sandbox: "skein-fleet".into(),
                    purpose: Purpose::Review,
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let asked = std::cell::RefCell::new(Vec::new());
        // #7 is in the queue. #41 is not, and is closed. #9 is not, and GitHub cannot say.
        let ask = |number: u64| {
            asked.borrow_mut().push(number);
            match number {
                41 => Some(false),
                _ => None,
            }
        };
        // The destroy needs `sbx` and will fail here, so the return value cannot be asserted on —
        // what CAN is who was asked about, which is the whole decision this function makes.
        let _ = close_finished("demo", &[7], ask);

        assert_eq!(
            *asked.borrow(),
            vec![9, 41],
            "the queue's own pull request was paid a GitHub call for, or one that needed asking \
             about was skipped"
        );

        std::env::remove_var("SKEIN_FLEET_ROOT");
        std::env::remove_var("SKEIN_HOME");
    }

    /// The cap answers with a sentence, and it answers at the limit rather than past it.
    ///
    /// **What would make this fail:** writing `>` for `>=`, which lets one more box in than the
    /// constant says — the classic off-by-one in a limit, and the one nobody notices because the
    /// fleet merely runs a little hotter than it was told to.
    #[test]
    fn the_cap_bites_at_the_limit_and_says_why() {
        assert_eq!(room_for_another(0), None);
        assert_eq!(room_for_another(AT_ONCE - 1), None);
        let full = room_for_another(AT_ONCE).expect("the limit must refuse");
        assert!(
            full.contains(&AT_ONCE.to_string()),
            "the refusal must name the limit: {full}"
        );
        assert!(room_for_another(AT_ONCE + 1).is_some());
    }
}
