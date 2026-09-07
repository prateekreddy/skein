//! Where a reading is filed, and what is thrown away.
//!
//! Everything is keyed on `(number, head_sha)`, and that key is the module's central fact: a
//! summary for any other head is stale by definition, so a lookup for a new commit simply misses
//! rather than returning something subtly wrong. This holds the whole of that — the path, the
//! readers ([`cached`], [`previous`], [`newest_for`], [`known`], [`held`]), the atomic writers, and
//! [`prune`], which is the only thing that ever deletes one.
//!
//! The read-tried notes live here too, and for the same reason: they are the second thing filed
//! per head commit. A reading that FAILED is deliberately never cached — that would make a failure
//! read as a reading — so the note beside the cache is what stops the background pass re-buying the
//! same failure every ten minutes for ever.

use super::summary::{Depth, Known, Summary};
use crate::prq::review_dir;
use crate::util::*;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

// ───────────────────────────── cache ─────────────────────────────

pub(super) fn cache_path(repo_id: &str, number: u64, head_sha: &str) -> PathBuf {
    // The head SHA is in the *filename*, so a stale summary can never be read as a fresh one — the
    // lookup for a new head simply misses. Storing one file per PR and comparing a field inside it
    // would work until the day the comparison was forgotten.
    let sha: String = head_sha
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect();
    review_dir(repo_id)
        .join("summaries")
        .join(format!("{number}-{sha}.json"))
}

/// A previously written summary for exactly this PR *and* this head commit.
pub fn cached(repo_id: &str, number: u64, head_sha: &str) -> Option<Summary> {
    let text = fs::read_to_string(cache_path(repo_id, number, head_sha)).ok()?;
    // `computed` is forced false rather than trusted from the file, for the same reason
    // `prq::remembered` forces `fresh` false: what was written was computed WHEN it was written, and
    // the one thing this must never do is let a free answer be counted as one that cost something.
    serde_json::from_str::<Summary>(&text).ok().map(|mut s| {
        s.computed = false;
        s
    })
}

/// What skein said about this PR at an EARLIER commit — the newest such reading, if any.
///
/// The prompt has always had a "what skein already said" slot, and until now nothing could fill it
/// in the case that matters. [`summarise`] returns the cached summary before building a prompt at
/// all, so the only lookup that existed — keyed on the CURRENT head — could only ever hit when
/// somebody pressed "re-read" on a commit already read. A new commit landing, which is the whole
/// reason a PR comes back to you, started from nothing every time.
///
/// So the reading of the commit it moved FROM is what gets handed over, and the model is asked to
/// account for the change rather than for the pull request. Cheaper and better in the same move: it
/// is the difference between "read these forty files" and "you said this yesterday, here is what
/// moved".
///
/// Newest by modification time, because a PR force-pushed twice leaves two and only the last one
/// describes what the branch actually looked like before this push.
pub fn previous(repo_id: &str, number: u64, not_sha: &str) -> Option<Summary> {
    let dir = review_dir(repo_id).join("summaries");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for path in fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()) {
        let Some((n, sha)) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|name| name.split_once('-'))
        else {
            continue;
        };
        if n.parse::<u64>().ok() != Some(number) || sha == not_sha {
            continue;
        }
        let Ok(at) = fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(seen, _)| at > *seen) {
            best = Some((at, path));
        }
    }
    let text = fs::read_to_string(best?.1).ok()?;
    serde_json::from_str::<Summary>(&text)
        .ok()
        .map(|mut s| {
            // `cached`'s rule, and the third and last place that reads a `Summary` off disk
            // (SKEIN-292). What was written was computed WHEN it was written; a free answer
            // counted as a paid one is what let a page reload spend the whole day's allowance on
            // cache hits, and the invariant has to hold at every reader or the next caller
            // inherits the bug rather than the rule.
            s.computed = false;
            s
        })
        .filter(|s| s.depth != Depth::Unread)
}

pub(super) fn store(repo_id: &str, s: &Summary) -> Result<(), String> {
    store_at(repo_id, &s.head_sha, s)
}

/// The same, filed under a commit that is not the one the reading describes.
///
/// Separate from [`store`] because the key and the reading's own commit are different questions,
/// and conflating them is how a reading comes to claim it describes code it never saw. `store` is
/// its only caller in the crate now — the round gate that filed a KEPT reading under a commit it
/// had not read went with the gate (SKEIN-444) — and the split stays because `known` derives
/// staleness by comparing the two, so the shape has to remain expressible.
///
/// The summary's own `head_sha` is left alone on purpose — it still names the commit it read, so
/// `Known::stale` goes on telling the truth and nothing has to remember to compare.
pub(super) fn store_at(repo_id: &str, key_sha: &str, s: &Summary) -> Result<(), String> {
    let path = cache_path(repo_id, s.number, key_sha);
    let dir = path.parent().ok_or("no parent")?.to_path_buf();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    write_atomic(&path, &dir, &bytes)
}

/// Every reading skein already holds for these pull requests, off disk, costing nothing.
///
/// **Why this exists as a bulk read.** The pane used to discover an existing reading only by asking
/// for it one pull request at a time, through the same path that COMPUTES one — so every limit that
/// belongs to spending money was also a limit on remembering. A reading skein had already paid for
/// was hidden if the pull request was a draft, or unsettled, or in the waiting lane, or simply past
/// the sixth row, because the loop that asks stops when the allowance is gone. Reported as "I can
/// only see 2 PRs with summaries while before there were a bunch", with the owner's own diagnosis:
/// the limits are for new analysis, not for reading from disk.
///
/// So: one request, every reading, no model calls and no rules. What the pane then asks to have
/// COMPUTED is a separate question, and that one keeps every limit it had.
///
/// A reading of an earlier commit is returned too, marked. It is still true about the code it read,
/// and the row says which commit that was — without it, everything skein knew about a pull request
/// vanished from the pane the moment somebody pushed, and came back only if asked for again.
pub fn known(repo_id: &str, prs: &[(u64, String)]) -> std::collections::BTreeMap<u64, Known> {
    let mut out = std::collections::BTreeMap::new();
    for (number, head_sha) in prs {
        // **Stale is a fact about the reading, not about which file it was found in** (SKEIN-433).
        // It used to be `false` here and `true` below — "found under the head the queue holds" —
        // which was the same answer right up until a reading could be FILED under a commit it had
        // not read. The round gate did exactly that: it kept the earlier reading and stored it
        // under the new head, and the row then said the reading was current when its own
        // `head_sha` named an older commit. Nothing failed — the stale block simply never drew,
        // and `not_reread` lives inside it, so the whole visible half of it was dead. The gate has
        // since been replaced by GitHub's review request (SKEIN-444) and files nothing, but the
        // rule below was the right one either way: ask the reading what it describes.
        //
        // Asking the summary what it describes answers both cases with one rule.
        let found = cached(repo_id, *number, head_sha).or_else(|| newest_for(repo_id, *number));
        if let Some(summary) = found {
            let stale = summary.head_sha != *head_sha;
            out.insert(*number, Known::new(summary, stale));
        }
    }
    out
}

/// What skein already holds for ONE pull request, off disk, costing nothing — the prose behind a
/// thinned row, fetched when the row is opened.
///
/// **Why this is not `GET /review/:n/summary` as it stands.** That route computes: it goes through
/// [`visit`], which serves a reading [`cached`] at THIS head first and otherwise falls through the
/// scope and budget doors to a model call. So it answers an expanded row correctly in the common
/// case and wrongly in the one the queue deliberately keeps: a reading of an EARLIER commit.
/// [`known`] hands that reading over marked `stale`, and `visit` cannot — its cache lookup is
/// keyed on the current head, so it misses, and expanding the row would either spend a model call
/// nobody asked for or come back `unread`. [`known_at`] could not patch that either: it hard-codes
/// `stale: false` and filters the draft to the head it was given.
///
/// So this is [`known`] for one pull request, delegating rather than repeating it, and a miss is
/// an honest unread answer rather than a 404 — a row that opens onto a transport error is how a
/// reading that exists comes to look like a pull request nobody read.
pub fn held(repo_id: &str, number: u64, head_sha: &str) -> Known {
    known(repo_id, &[(number, head_sha.to_string())])
        .remove(&number)
        .unwrap_or_else(|| {
            Known::new(
                Summary::unread(
                    number,
                    head_sha,
                    "skein holds no reading of this pull request yet.",
                ),
                false,
            )
        })
}

/// The most recent reading of any commit of this pull request.
///
/// By modification time rather than by parsing shas out of filenames: the file's own age is what
/// "most recent" means, and a sha says nothing about which came first.
pub(super) fn newest_for(repo_id: &str, number: u64) -> Option<Summary> {
    let dir = review_dir(repo_id).join("summaries");
    let prefix = format!("{number}-");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) || !name.ends_with(".json") {
            continue;
        }
        let Ok(at) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(seen, _)| at > *seen) {
            best = Some((at, entry.path()));
        }
    }
    let text = fs::read_to_string(best?.1).ok()?;
    serde_json::from_str::<Summary>(&text).ok().map(|mut s| {
        // Same rule as `cached`: what came off disk did not cost anything NOW.
        s.computed = false;
        s
    })
}

/// Drop what describes a pull request that is over, or a commit that has been replaced.
///
/// Nothing used to. `summaries/<number>-<head_sha>.json` holds one file per PR **per head commit**,
/// which is what makes a stale summary unreadable rather than wrong — the lookup for a new head
/// simply misses — but the miss leaves the old file behind. Every push to a PR under review wrote
/// another and abandoned the last, and merging or closing it abandoned them all.
///
/// Two rules, because the two questions are not the same shape:
///
/// **A superseded head is exact and free.** The queue has just named every open PR's current sha.
/// A file for that number with any other sha can never be read again by construction, so it goes
/// with no ambiguity and no call to anybody.
///
/// **Closed and merged is asked, not inferred.** A PR absent from this queue has very often just
/// stopped involving you — the searches behind it are `review-requested:you`, `author:you`,
/// `mentions:you` — so absence is not death, and treating it as death deletes the reading of a live
/// PR whose review request was reassigned. `prq::pr_is_open` answers the actual question.
///
/// Best-effort throughout: a failed lookup keeps the file. Deleting a summary costs one re-read;
/// keeping one costs a few kilobytes, and only one of those is irreversible.
///
/// **Only the old ones are asked about.** GitHub reads are cheap here and model calls are not, but
/// cheap is not free and asking about every file on every tab open would be a call per abandoned
/// summary for ever. A file written in the last [`SETTLE`] is left alone: if its PR did just close,
/// keeping it a few more days costs kilobytes, and the next pass reaps it. The superseded-head rule
/// above has no such delay — it needs no call at all, so it runs on everything every time.
pub fn prune(repo_id: &str, slug: &str, open: &[(u64, String)]) -> usize {
    let dir = review_dir(repo_id).join("summaries");
    let Ok(entries) = fs::read_dir(&dir) else {
        return 0;
    };
    let mut gone = 0;
    // Asked once per number rather than once per file: a PR force-pushed ten times has ten files and
    // exactly one answer.
    let mut closed: std::collections::HashMap<u64, bool> = std::collections::HashMap::new();
    let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    // The newest superseded reading per open PR, worked out before anything is removed — it is what
    // [`previous`] will hand to the next pass instead of starting from nothing.
    let newest_stale = keepsakes(&files, open);
    for path in files {
        let Some((number, sha)) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|name| name.split_once('-'))
            .and_then(|(n, sha)| Some((n.parse::<u64>().ok()?, sha.to_string())))
        else {
            continue;
        };
        let doomed = match open.iter().find(|(n, _)| *n == number) {
            // Open, and this is not the commit it is at — but the most recent of those is kept.
            //
            // Deleting every superseded reading and then asking the model to build on "what skein
            // already said" are two changes that undo each other, and they were written an hour
            // apart. [`previous`] hands the reading of the commit a PR moved FROM to the next pass,
            // so exactly one superseded file per PR is not waste — it is the input that makes the
            // next read cheap. The ones behind it describe branches nobody will ever ask about.
            Some((_, head)) => {
                !head.starts_with(&sha) && *head != sha && Some(&path) != newest_stale.get(&number)
            }
            // Not in your queue. That is a question, not an answer — and one worth paying for
            // only once the file has stopped being current enough to be worth keeping anyway.
            None if fresh(&path) => false,
            None => match closed.get(&number) {
                Some(known) => *known,
                None => {
                    let over = crate::prq::pr_is_open(slug, number).map(|open| !open);
                    // `None` — GitHub could not say — keeps the file, and is not remembered, so the
                    // next pass asks again rather than treating one bad call as a verdict.
                    match over {
                        Some(over) => {
                            closed.insert(number, over);
                            over
                        }
                        None => false,
                    }
                }
            },
        };
        if doomed && fs::remove_file(&path).is_ok() {
            gone += 1;
        }
    }
    gone
}

/// For each open pull request, the newest reading of a commit it is no longer at.
///
/// Kept by [`prune`] so [`previous`] has something to hand the next pass. One per PR: the ones
/// behind it describe branches that will never be asked about again.
pub(super) fn keepsakes(
    files: &[PathBuf],
    open: &[(u64, String)],
) -> std::collections::HashMap<u64, PathBuf> {
    let mut best: std::collections::HashMap<u64, (std::time::SystemTime, PathBuf)> =
        std::collections::HashMap::new();
    for path in files {
        let Some((n, sha)) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|name| name.split_once('-'))
        else {
            continue;
        };
        let Ok(number) = n.parse::<u64>() else {
            continue;
        };
        // Only for PRs still open, and only for commits they have moved past.
        let Some((_, head)) = open.iter().find(|(o, _)| *o == number) else {
            continue;
        };
        if head.starts_with(sha) || head == sha {
            continue;
        }
        let Ok(at) = fs::metadata(path).and_then(|m| m.modified()) else {
            continue;
        };
        let better = best.get(&number).is_none_or(|(seen, _)| at > *seen);
        if better {
            best.insert(number, (at, path.clone()));
        }
    }
    best.into_iter().map(|(n, (_, path))| (n, path)).collect()
}

/// How long a summary is left alone before it is worth a call to ask whether its PR still exists.
///
/// A week, and the number is a trade rather than a guess: below it, a busy repo spends a GitHub read
/// per abandoned summary on every tab open; above it, an unreadable file survives longer than
/// anybody would notice. Nothing depends on the exact value — being wrong costs kilobytes and one
/// later pass.
pub(super) const SETTLE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Written recently enough that it is not worth asking about yet.
pub(super) fn fresh(path: &std::path::Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|at| at.elapsed().map(|age| age < SETTLE).unwrap_or(true))
        .unwrap_or(false)
}

/// What the background reader has already tried and failed to read, per head commit.
///
/// **Why a failure needs remembering at all.** A reading that fails is deliberately NOT cached —
/// caching it would make a failure read as a reading, which is the rule the whole `unread` shape
/// exists for. But the background pass picks work by "has no reading at this head", so an
/// uncacheable failure is picked again on the next pass, and the one after: a pull request whose
/// model call fails costs a model call every ten minutes, for ever. That is the most expensive
/// mistake available in this module, and it is invisible — the pane shows the same "not summarised"
/// row throughout.
///
/// So the reader keeps its own note of what it tried. Nothing else consults it: **the button in the
/// row does not**, because a person pressing "read it" is saying they think it will work now, and a
/// standing failure must never make a button do nothing (same rule as `ai::forget_refusal`).
pub(super) fn tried_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("read-tried.json")
}

pub(super) fn tried_at(path: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// **Is this failure a fact about the SETUP rather than about this commit?**
///
/// [`crate::ai`] draws exactly this line and states it in `after_merged`'s doc: "a timeout is a
/// fact about this diff and the next attempt may differ, while every other refusal is a fact about
/// the setup and will not". [`read_tried`] needs the same line for a different purpose — whether a
/// note may go on REFUSING a row — so it is one function rather than the rule written twice.
///
/// Matched on the sentences [`crate::ai::Unread::say`] itself produces, and the test walks real
/// `Unread` values through `say()` to check each lands on the right side. So a message that is
/// reworded fails a named assertion instead of quietly reclassifying half the queue.
pub(super) fn about_the_setup(why: &str) -> bool {
    [
        // `Unread::Missing`
        "skein could not start",
        // `Unread::Unreachable`
        "skein could not reach the fleet sandbox",
        // `Unread::AbsentInSandbox`
        "is not installed in the fleet sandbox",
        // `Unread::Refused` — it ran and refused: not logged in, a model it will not serve, a rate
        // limit, a working directory it could not make. Every one of those is cured somewhere
        // other than this pull request.
        "` exited ",
        // Older notes, from before transport failures stopped being written down at all.
        "its diff could not be read",
    ]
    .iter()
    .any(|mark| why.contains(mark))
}

/// The notes that may still refuse a row: the ones about THIS COMMIT.
///
/// **A failure of the setup must not latch a per-commit refusal**, because fixing the setup does
/// not clear it and nothing tells anyone it is there. Found live on the owner's fleet: a sandbox
/// `mkdir: Permission denied` (fixed by dropping the sandbox hop, SKEIN-576) had already been written down
/// against `acme/thing#753`, so every later unattended pass answered "skein already spent a
/// reading on this commit and will not buy another by itself" — over a cause that no longer
/// existed, on a pull request whose review GitHub was actively requesting.
///
/// The file keeps them: the note is also the SENTENCE the row shows, and "why is this not read" is
/// worth answering. What it stops doing is standing in the way of the next attempt.
pub(super) fn read_tried(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    let mut all = tried_at(&tried_path(repo_id));
    all.retain(|_, why| !about_the_setup(why));
    all
}

pub(super) fn note_into(
    path: &std::path::Path,
    mut all: std::collections::BTreeMap<String, String>,
    number: u64,
    head_sha: &str,
    why: &str,
) {
    all.insert(format!("{number}-{head_sha}"), why.to_string());
    // Bounded: this is per head commit, so a busy repo would otherwise grow one entry per push for
    // ever. The pruning that drops stale summaries has the same job and the same shape.
    if all.len() > 200 {
        let drop: Vec<_> = all.keys().take(all.len() - 200).cloned().collect();
        for key in drop {
            all.remove(&key);
        }
    }
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
        if let Ok(bytes) = serde_json::to_vec_pretty(&all) {
            let _ = write_atomic(path, dir, &bytes);
        }
    }
}

pub(super) fn note_tried(repo_id: &str, number: u64, head_sha: &str, why: &str) {
    // Read through `read_tried`, not `tried_at`, so its legacy-note filter still gets its
    // "dropped from the file on the next write".
    note_into(
        &tried_path(repo_id),
        read_tried(repo_id),
        number,
        head_sha,
        why,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::summary::*;
    use crate::review::testkit::*;

    /// What skein already holds is handed over in one go, and an older reading is marked, not lost.
    ///
    /// The bulk read exists because the pane used to learn what skein knew only by asking for one
    /// pull request at a time, down the path that COMPUTES a reading — so a draft's reading, an
    /// unsettled branch's reading, and everything past the sixth row were all hidden by limits meant
    /// to bound money. Reading from disk is not spending, and this is the call that says so.
    #[test]
    fn every_reading_on_disk_is_handed_over_at_once() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let put = |number: u64, head: &str, line: &str| {
            store(
                "demo",
                &Summary {
                    // A fixture, and this is the honest value for one: nobody scanned a diff.
                    owed_triggered: None,
                    findings_block: None,

                    swept: false,
                    number,
                    head_sha: head.into(),
                    depth: Depth::Line,
                    line: line.into(),
                    detail: String::new(),
                    flags: Vec::new(),
                    signals: Vec::new(),
                    yours: Vec::new(),
                    others: 0,
                    ownership_unknown: String::new(),
                    unread_because: String::new(),
                    not_reread: String::new(),
                    computed: true,
                    budget_stopped: false,
                },
            )
            .unwrap();
        };
        put(1, "aaa", "read at the current head");
        put(2, "old", "read before the last push");

        let asked = [
            (1, "aaa".to_string()),
            // #2 has moved since it was read.
            (2, "new".to_string()),
            // #3 has never been read at all.
            (3, "ccc".to_string()),
        ];
        let known = known("demo", &asked);

        assert_eq!(known.len(), 2, "a reading was lost or invented: {known:?}");
        let one = &known[&1];
        assert_eq!(one.summary.line, "read at the current head");
        assert!(!one.stale, "a reading of the current head was marked stale");
        assert!(
            !one.summary.computed,
            "a reading off disk claims to have cost a model call, so a budget that counts spending \
             is spent by answers that were free"
        );

        // The one that matters: a reading of an earlier commit is KEPT and marked, not dropped.
        // Dropped, everything skein knew about a pull request vanished the moment somebody pushed —
        // and only a person asking again could bring it back.
        let two = &known[&2];
        assert_eq!(two.summary.line, "read before the last push");
        assert!(
            two.stale,
            "a reading of an earlier commit was handed over as current"
        );

        // And nothing is invented for one that was never read.
        assert!(!known.contains_key(&3));

        std::env::remove_var("SKEIN_HOME");
    }

    /// An answer served from the cache does not report as having cost anything.
    ///
    /// The client keeps a budget for how many pull requests are read WITHOUT being asked for, and
    /// that budget counted REQUESTS. A request answered from the cache on disk costs nothing, so a
    /// page reload spent the whole allowance on free answers and the rows past it were never read —
    /// on any reload, for ever. Strictly worse than having no budget, which is the shape of mistake
    /// worth a test of its own: the limit was doing the opposite of its name.
    #[test]
    fn a_cached_summary_does_not_count_as_a_model_call() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let fresh = Summary {
            // A fixture, and this is the honest value for one: nobody scanned a diff.
            owed_triggered: None,
            findings_block: None,

            swept: false,
            number: 4,
            head_sha: "abc".into(),
            depth: Depth::Line,
            line: "a change".into(),
            detail: String::new(),
            flags: Vec::new(),
            signals: Vec::new(),
            yours: Vec::new(),
            others: 0,
            ownership_unknown: String::new(),
            unread_because: String::new(),
            not_reread: String::new(),
            computed: true,
            budget_stopped: false,
        };
        store("demo", &fresh).unwrap();

        let read_back = cached("demo", 4, "abc").expect("the summary was stored");
        assert_eq!(
            read_back.line, "a change",
            "the summary itself did not survive"
        );
        assert!(
            !read_back.computed,
            "a summary read off disk claims to have cost a model call, so a budget that counts \
             spending is spent by answers that were free"
        );

        // And "unread" from the switch being off is not spending either — nothing was asked.
        let off = Summary::unread(4, "abc", "AI is off");
        assert!(!off.computed);
    }

    /// Pruning keeps exactly the readings that can still be read.
    ///
    /// Nothing pruned anything: one file per PR per head commit, and every push abandoned the last
    /// while merging abandoned them all. The two rules have to be told apart here, because getting
    /// the second one wrong deletes a live PR's reading — absence from this queue means "no longer
    /// involves you" at least as often as it means "closed", and the queue is personal.
    #[test]
    fn pruning_drops_replaced_commits_and_keeps_what_it_cannot_ask_about() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        // A GitHub that answers by number: #9 is closed, #8 is still open. Both are absent from the
        // lane below, which is the whole point — absence is the question, not the answer.
        let (base, asked) = stub_github();
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let dir = crate::prq::review_dir("demo").join("summaries");
        fs::create_dir_all(&dir).unwrap();
        let put = |name: &str| {
            fs::write(dir.join(format!("{name}.json")), "{}").unwrap();
        };
        put("7-aaaaaaaa"); // #7 is open at aaaaaaaa — the current reading
        put("7-ffffffff"); // two pushes ago: nobody will ever ask about that branch again
        put("7-bbbbbbbb"); // the commit it moved FROM — kept, because `previous` hands it on
        put("9-cccccccc"); // #9 left the lane because it CLOSED — old enough to be worth asking
        put("8-eeeeeeee"); // #8 left the lane and is still open — asked, and kept
        put("4-dddddddd"); // #4 left the lane too, but is too recent to spend a call on
        put("nonsense"); // not a summary; must be left alone rather than guessed at

        // Backdated past SETTLE, or the lookup is never reached and this test passes for a reason
        // that has nothing to do with what it claims to check — which is exactly what it did on its
        // first draft: sabotaging the failed-lookup arm changed nothing, because every file was new.
        let old = std::time::SystemTime::now() - (SETTLE + Duration::from_secs(60));
        // Explicit ordering between the two superseded readings, so "the newest" is a fact rather
        // than whatever order the filesystem happened to create them in.
        backdate(
            &dir.join("7-ffffffff.json"),
            std::time::SystemTime::now() - Duration::from_secs(3600),
        );
        for name in ["9-cccccccc", "8-eeeeeeee"] {
            let handle = fs::OpenOptions::new()
                .write(true)
                .open(dir.join(format!("{name}.json")))
                .unwrap();
            handle
                .set_times(fs::FileTimes::new().set_modified(old).set_accessed(old))
                .unwrap();
        }

        let gone = prune("demo", "acme/repo", &[(7, "aaaaaaaa".to_string())]);

        let left = |name: &str| dir.join(format!("{name}.json")).exists();
        assert!(
            left("7-aaaaaaaa"),
            "the reading of the commit the PR is AT was deleted"
        );
        // Exactly one superseded reading survives, and it is the newest. Not waste: the next pass
        // hands it to the model as "what skein already said", which is the difference between
        // re-reading forty files and accounting for what moved.
        assert!(
            left("7-bbbbbbbb"),
            "the reading of the commit this PR moved FROM was deleted, so the next read starts \
             from nothing — pruning and building-on-the-last-reading undoing each other"
        );
        assert!(!left("7-ffffffff"), "a reading two pushes back was kept");
        assert!(
            !left("9-cccccccc"),
            "the reading of a CLOSED pull request was kept — which is the \
             whole of what was asked for"
        );
        // The one that would lose real work. This queue is personal, so a PR leaving it usually
        // means it stopped involving you.
        assert!(
            left("8-eeeeeeee"),
            "a pull request that merely left your lane, and is still open, was deleted"
        );
        assert!(left("nonsense"), "a file that is not a summary was deleted");
        assert_eq!(
            gone, 2,
            "exactly the oldest superseded commit and the closed PR"
        );

        // And the recent one was never ASKED about — cheap is not free, and a call per abandoned
        // summary on every tab open is a cost nobody agreed to. Asserted on the requests actually
        // made, because "it survived" is also true of a file that was asked about and kept.
        let asked = asked.lock().unwrap().clone();
        assert!(
            !asked.iter().any(|p| p.contains("/pulls/4")),
            "a summary too recent to be worth a call was asked about anyway: {asked:?}"
        );
        assert!(
            asked.iter().any(|p| p.contains("/pulls/9")),
            "the old one was never asked about, so nothing here was tested: {asked:?}"
        );

        std::env::remove_var("SKEIN_GITHUB_API");
        std::env::remove_var("GH_TOKEN");
        crate::prq::forget_host_token();
    }

    /// A GitHub that cannot be reached deletes nothing it was not certain about.
    ///
    /// Its own test because the sibling above uses a working stub, which never reaches this arm —
    /// and this is the arm where being wrong is unrecoverable. Deleting a summary normally costs one
    /// re-read; deleting every summary in the repo because GitHub was down for a minute costs the
    /// lot, and the superseded rule must still work while it happens.
    #[test]
    fn a_github_that_cannot_be_reached_deletes_only_what_needs_no_asking() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        // Nothing listens here.
        std::env::set_var("SKEIN_GITHUB_API", "http://127.0.0.1:1");

        let dir = crate::prq::review_dir("demo").join("summaries");
        fs::create_dir_all(&dir).unwrap();
        for name in ["7-aaaaaaaa", "7-ffffffff", "7-bbbbbbbb", "9-cccccccc"] {
            fs::write(dir.join(format!("{name}.json")), "{}").unwrap();
        }
        backdate(
            &dir.join("7-ffffffff.json"),
            std::time::SystemTime::now() - Duration::from_secs(3600),
        );
        let old = std::time::SystemTime::now() - (SETTLE + Duration::from_secs(60));
        let handle = fs::OpenOptions::new()
            .write(true)
            .open(dir.join("9-cccccccc.json"))
            .unwrap();
        handle
            .set_times(fs::FileTimes::new().set_modified(old).set_accessed(old))
            .unwrap();

        let gone = prune("demo", "acme/repo", &[(7, "aaaaaaaa".to_string())]);
        assert_eq!(
            gone, 1,
            "only the oldest superseded commit, which needs nobody's opinion"
        );
        assert!(dir.join("7-aaaaaaaa.json").exists());
        assert!(
            dir.join("7-bbbbbbbb.json").exists(),
            "the keepsake was deleted"
        );
        assert!(!dir.join("7-ffffffff.json").exists());
        assert!(
            dir.join("9-cccccccc.json").exists(),
            "a summary was deleted on a lookup that never got an answer — an unreachable GitHub \
             would empty the whole cache, and nothing here is recoverable"
        );

        std::env::remove_var("SKEIN_GITHUB_API");
        std::env::remove_var("GH_TOKEN");
        crate::prq::forget_host_token();
    }

    #[test]
    fn the_cache_key_is_the_head_commit() {
        let a = cache_path("r", 7, "aaa");
        let b = cache_path("r", 7, "bbb");
        assert_ne!(a, b, "two heads of one PR must not share a summary file");
        assert!(a.to_string_lossy().contains("7-aaa"));
    }

    #[test]
    fn a_hostile_sha_cannot_escape_the_cache_directory() {
        let p = cache_path("r", 1, "../../etc/passwd");
        assert!(!p.to_string_lossy().contains(".."), "{}", p.display());
    }

    /// **Nothing read off disk says it cost a model call** (SKEIN-292).
    ///
    /// [`cached`] has forced `computed = false` since the day a page reload spent the whole
    /// allowance on cache hits, with the reason written beside it. The rule is not about that one
    /// function though: it is about every way a stored reading comes back, and there were three
    /// readers and only two of them obeyed it. [`previous`] deserialised straight from the file, so
    /// a reading handed to the next prompt claimed to have cost something.
    ///
    /// Harmless the day it was found — nothing downstream read the flag — which is exactly the
    /// argument for pinning it now, because the day something does read it the failure is a budget
    /// that spends itself on remembering.
    ///
    /// The last assertion is the one that stops a FOURTH reader inheriting the bug instead of the
    /// rule: it counts the deserialisations in this file and insists each is followed by the line
    /// that forces the flag down.
    #[test]
    fn a_reading_off_disk_never_says_it_cost_a_model_call() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // Stored as it is stored for real: computed, because when it was written it was.
        store(
            "vintage",
            &Summary {
                // A fixture, and this is the honest value for one: nobody scanned a diff.
                owed_triggered: None,
                findings_block: None,

                swept: false,
                number: 3,
                head_sha: "old".into(),
                depth: Depth::Line,
                line: "it changes a thing".into(),
                detail: String::new(),
                flags: Vec::new(),
                signals: Vec::new(),
                yours: Vec::new(),
                others: 0,
                ownership_unknown: String::new(),
                unread_because: String::new(),
                not_reread: String::new(),
                computed: true,
                budget_stopped: false,
            },
        )
        .unwrap();

        // The head has moved. The bulk payload still hands the reading over, marked stale — and
        // that is the path the item is about.
        let bulk = known("vintage", &[(3, "new".to_string())]);
        let row = bulk
            .get(&3)
            .expect("the earlier reading is still handed over");
        assert!(
            row.stale,
            "the reading is of an earlier commit and must say so"
        );
        assert!(
            !row.summary.computed,
            "a reading served from disk reported that it cost a model call — the exact bug \
             `cached`'s comment exists to prevent, one function along"
        );
        // The single-PR route goes the same way, and so does the reading at its own head.
        assert!(!held("vintage", 3, "new").summary.computed);
        assert!(
            !cached("vintage", 3, "old")
                .expect("stored at its own head")
                .computed
        );
        // And the lookup that fills the prompt's "what skein already said" slot.
        assert!(
            !previous("vintage", 3, "new")
                .expect("the earlier reading is what the next prompt is built on")
                .computed,
            "the reading handed to the next prompt claimed to have cost something"
        );

        // Every reader of a stored `Summary`, pinned. Asserted against the source because the
        // thing that goes wrong is a NEW one being written without the line — which no runtime
        // test of the three that exist could ever notice.
        //
        // The whole module rather than this one file. All three readers live here today, and a
        // fourth written into any other file of `src/review/` is precisely the case this exists
        // to catch — a census narrowed to the file it was written in stops covering the module
        // the moment the module has more than one file.
        let src: String = std::fs::read_dir("src/review")
            .expect("the review module")
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).expect("a file of the review module"))
            .collect();
        // Assembled at runtime so the needle never appears in this file as a literal — a source
        // check that matches its own search term counts itself, and then the number it reports is
        // about the test rather than about the code.
        let needle = format!("{}from_str::<Summary>", "serde_json::");
        let readers: Vec<String> = src
            .split(&needle)
            .skip(1)
            .map(|after| after.chars().take(600).collect())
            .collect();
        assert_eq!(
            readers.len(),
            3,
            "a reading is deserialised somewhere new; every one of them must force `computed` down"
        );
        for (i, reader) in readers.iter().enumerate() {
            assert!(
                reader.contains("computed = false"),
                "reader {i} hands back a stored summary still claiming it cost a model call: \
                 {reader}"
            );
        }

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A failure of the SETUP must not latch a per-commit refusal.**
    ///
    /// Found live on `acme/thing#753`, and it is the shape that makes it dangerous: skein
    /// wrote down `mkdir: Permission denied` — a sandbox path bug, since fixed — against that
    /// commit, and from then on every unattended pass answered "skein already spent a reading on
    /// this commit and will not buy another by itself". GitHub was requesting the review the whole
    /// time. Nothing was broken any more and nothing said the note was there.
    ///
    /// **Derived, not guessed**: the sentences come from [`crate::ai::Unread::say`] itself, so a
    /// reworded message fails here rather than quietly reclassifying half the queue.
    #[test]
    fn a_failure_of_the_setup_does_not_go_on_refusing_a_commit() {
        use crate::ai::Unread;
        let setup = [
            Unread::Missing {
                bin: "claude".into(),
                why: "No such file".into(),
            },
            Unread::Unreachable {
                sandbox: "fleet".into(),
                why: "sbx: not found".into(),
            },
            Unread::AbsentInSandbox {
                bin: "claude".into(),
                sandbox: "fleet".into(),
            },
            Unread::Refused {
                code: "1".into(),
                said: "mkdir: Permission denied".into(),
            },
            Unread::Refused {
                code: "1".into(),
                said: String::new(),
            },
        ];
        for unread in setup {
            let said = unread.say();
            assert!(
                super::about_the_setup(&said),
                "this is cured somewhere other than the pull request, and skein will go on \
                 refusing the commit long after it is fixed: {said}"
            );
        }

        // And its opposite: a budget that ran out IS a fact about this diff. A note for it has to
        // go on refusing, or an unattended pass buys the same timeout every ten minutes for ever.
        let slow = Unread::Slow(std::time::Duration::from_secs(900)).say();
        assert!(
            !super::about_the_setup(&slow),
            "a call that ran out of time reads as a broken setup, so the one note that SHOULD \
             stop a row being re-bought stopped stopping it: {slow}"
        );
        assert!(
            !super::about_the_setup("the model answered in a shape skein could not read"),
            "an answer skein could not parse is a fact about this reading, not about the machine"
        );
    }

    /// **A reading the gate kept still reports itself stale** (SKEIN-433) — found on the rig, where
    /// the row said a reading was current while its own `head_sha` named an older commit.
    ///
    /// `known` used to answer "stale" from which lookup found the file — `false` for the current
    /// head, `true` for the fallback — which was the same answer right up until a reading could be
    /// filed under a commit it had not read. The round gate did that; it is gone (SKEIN-444), so
    /// this now guards the RULE rather than a live caller: staleness is a fact about the reading,
    /// and anything that files one away again inherits a correct answer instead of a fresh version
    /// of this bug.
    #[test]
    fn a_reading_filed_under_a_commit_it_did_not_read_still_says_it_is_stale() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // What the gate leaves behind: a reading OF `9c1de07`, filed under `4f2ab1c`.
        let mut kept = super::Summary::unread(7, "9c1de07abc", "");
        kept.depth = super::Depth::Line;
        kept.line = "adds a bounds check the caller already makes.".into();
        kept.not_reread = "skein did not re-read 4f2ab1c — a comment typo.".into();
        super::store_at("acme", "4f2ab1cdef", &kept).unwrap();

        let seen = super::known("acme", &[(7, "4f2ab1cdef".to_string())]);
        let row = seen
            .get(&7)
            .expect("the kept reading is not on the row at all");
        assert!(
            row.stale,
            "the row says this reading is current, but it describes {} and the branch is at \
             4f2ab1cdef — so the pane draws no stale block, and the sentence saying skein LOOKED \
             and chose not to re-read is inside it and never appears",
            row.summary.head_sha
        );
        assert_eq!(
            row.summary.not_reread, kept.not_reread,
            "the gate's own sentence did not survive the trip to the row"
        );

        // The counter-case, or the fix is just "always stale": an ordinary reading OF the commit
        // that is there is not stale, and a row that cried stale on every reading would be telling
        // the reader to press "read it again" for ever.
        let mut current = super::Summary::unread(9, "4f2ab1cdef", "");
        current.depth = super::Depth::Line;
        current.line = "a real reading of this commit.".into();
        super::store("acme", &current).unwrap();
        let seen = super::known("acme", &[(9, "4f2ab1cdef".to_string())]);
        assert!(
            !seen.get(&9).expect("no row").stale,
            "a reading of the commit that is actually there was marked stale"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// Opening a row hands over the prose skein already has — including for a reading of an
    /// EARLIER commit, which is the case the computing route cannot answer.
    ///
    /// `GET /review/:n/summary` goes through [`visit`], whose cache lookup is keyed on the CURRENT
    /// head: on a pull request that has moved since it was read, that misses and falls through to
    /// a model call. The queue deliberately keeps and marks that reading ([`known`]), so a row
    /// showing it must be able to open it without buying a new one.
    #[test]
    fn a_row_opens_onto_the_prose_skein_already_holds() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let mut old = fat(4, "before").summary;
        old.detail = "the brief written against the commit before the push.".into();
        store("demo", &old).unwrap();

        // The branch has moved: `after` is the head now, and nothing has been read at it.
        let opened = held("demo", 4, "after");
        assert!(
            opened.stale,
            "a reading of an earlier commit was handed over as current"
        );
        assert_eq!(
            opened.summary.detail, "the brief written against the commit before the push.",
            "expanding a row that has a reading found no prose in it"
        );
        assert!(
            !opened.summary.signals.is_empty(),
            "the signals behind the fold were not handed over"
        );

        // And a pull request skein has never read is an honest unread answer, not an error: a row
        // that opens onto a transport failure is how a reading that exists comes to look like a
        // pull request nobody read.
        let never = held("demo", 9, "zzz");
        assert_eq!(never.summary.depth, Depth::Unread);
        assert!(!never.summary.unread_because.is_empty());
        assert_eq!(never.summary.head_sha, "zzz");

        std::env::remove_var("SKEIN_HOME");
    }
}
