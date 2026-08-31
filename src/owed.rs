//! What a repository owes a reviewer before a verdict — `docs/pr-review.md` §8.
//!
//! # The scar, as a condition rather than a paragraph
//!
//! The interviewed box was asked what the smallest durable artifact would be that carries a lesson
//! to a fresh agent, *because a stateless engine has no memory by construction*. Its answer was six
//! lines per repository, and its own most valuable carry was a scar: it learned mid-session that a
//! deletion audit must be base-versus-head, then applied it four hours later. A fresh agent starts
//! without that, every time.
//!
//! So the lesson is not prose in a prompt that a fresh agent may or may not weigh. It is a
//! condition that is either satisfied or is not: **if the diff deletes lines and no deletion audit
//! is recorded at this sha, the next step is [`crate::workflow::Act::Audit`], not a post.**
//!
//! # Per repository, and that is the whole point
//!
//! What a repository owes a reviewer is a property of that repository. A global list would be a
//! list nobody could disagree with, which is a list nobody reads.
//!
//! # Which triggers this build can actually see, and the ones it says it cannot
//!
//! [`crate::workflow::Wake`] set the precedent and it is followed here exactly: a word this build
//! cannot answer says so out loud rather than sitting in a set looking switched on. Three of §8's
//! six triggers are findable in a unified diff by a scanner; two are claims about *prose* — "a
//! comment naming a mechanism", "a claim that something is absent" — that no scanner can find
//! without reading English, and inventing a pattern for them would produce a check that fires on
//! the wrong pull requests and stays silent on the right ones.
//!
//! **The safe direction, stated rather than implied.** This is [`crate::contracts`]'s composition
//! rule pointed the other way, and it lands in the same place. A trigger that fires spuriously
//! costs one audit turn — money, and a reviewer told to check something that was not there. A
//! trigger that misses costs a verdict posted without the check, which is exactly what happens
//! today with no owed checks at all. So a miss is never worse than the status quo and a false
//! positive is bounded, which is what makes it honest to ship scanners that are imperfect.

use crate::config::skein_home;
use std::path::PathBuf;

/// One thing a repository owes a reviewer before a verdict — §8's six rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
    /// The diff removes lines.
    Deletions,
    /// The diff adds an assertion or a guard.
    GuardAdded,
    /// The diff adds a comment naming a mechanism.
    MechanismNamed,
    /// The diff claims something is absent.
    AbsenceClaimed,
    /// The diff cites a document or a plan.
    DocCited,
    /// The diff adds a test.
    TestAdded,
}

/// Every check there is, which is also §8's table in its order.
pub const EVERY: [Check; 6] = [
    Check::Deletions,
    Check::GuardAdded,
    Check::MechanismNamed,
    Check::AbsenceClaimed,
    Check::DocCited,
    Check::TestAdded,
];

impl Check {
    /// How it is written in a repo's settings, and in the record on disk.
    pub fn spelled(&self) -> &'static str {
        match self {
            Check::Deletions => "deletions",
            Check::GuardAdded => "guard-added",
            Check::MechanismNamed => "mechanism-named",
            Check::AbsenceClaimed => "absence-claimed",
            Check::DocCited => "doc-cited",
            Check::TestAdded => "test-added",
        }
    }

    /// **Can this build see the trigger in a diff?** See the module note for why two cannot.
    ///
    /// The consequence is the same one [`crate::workflow::Wake::computable`] has: a check whose
    /// trigger cannot be detected can never fire, so leaving it in a repo's set would be a line in
    /// a settings file that does nothing and says nothing. It is refused by name instead.
    pub fn computable(&self) -> bool {
        !matches!(self, Check::MechanismNamed | Check::AbsenceClaimed)
    }

    /// What is owed when it fires — §8's right-hand column, addressed to the reviewer.
    pub fn owed(&self) -> &'static str {
        match self {
            Check::Deletions => {
                "this change removes lines. Audit them base-versus-head: for each removal, say what \
                 the removed line GUARANTEED, and where that guarantee is now made instead. A \
                 removal whose guarantee is nowhere else is the finding."
            }
            Check::GuardAdded => {
                "this change adds a guard or an assertion. Name the concrete change that would make \
                 it fail. If you cannot name one, the guard is decoration and that is the finding."
            }
            Check::MechanismNamed => {
                "this change adds a comment naming a mechanism. Check that mechanism is actually \
                 called, rather than described."
            }
            Check::AbsenceClaimed => {
                "this change claims something is absent. Check every branch and pull request before \
                 letting the claim stand."
            }
            Check::DocCited => {
                "this change cites a document or a plan. Verify it exists somewhere reachable, and \
                 say which branch it is on."
            }
            Check::TestAdded => {
                "this change adds a test. Check it can fail: name the change that would break it, \
                 and say whether the test would notice."
            }
        }
    }
}

/// Read a repo's configured set, and say which words were refused.
///
/// **Refused rather than ignored, and refused narrowly.** An unrecognised word is one a newer skein
/// wrote and this build cannot evaluate; a recognised one whose trigger this build cannot detect is
/// [`Check::computable`]. Both land in the same place for the same reason as the trigger set in §10
/// — *"a trigger from a newer skein is one this build cannot tell has fired"* — and both fail
/// towards owing nothing rather than towards a check that silently never fires.
pub fn read(words: &[String]) -> (Vec<Check>, Vec<String>) {
    let mut set = Vec::new();
    let mut refused = Vec::new();
    for word in words {
        let word = word.trim();
        if word.is_empty() {
            continue;
        }
        match EVERY.iter().find(|c| c.spelled() == word) {
            Some(c) if c.computable() => set.push(*c),
            Some(c) => refused.push(format!(
                "{} — this build cannot see that trigger in a diff, so it could never fire",
                c.spelled()
            )),
            None => refused.push(format!("{word} — not a check this build knows")),
        }
    }
    set.sort();
    set.dedup();
    (set, refused)
}

/// What a repository owes when nobody has said otherwise: every check this build can evaluate.
///
/// **Not the empty set.** §8's argument is that the lesson must not depend on somebody remembering
/// to write it down, and a default of nothing would put it back exactly there. The cost of the
/// other direction is bounded and visible: an audit turn before the first verdict on a change that
/// deletes lines, which is the thing the scar is about.
pub fn default_set() -> Vec<Check> {
    EVERY.iter().copied().filter(|c| c.computable()).collect()
}

/// The set a repository owes, from its own words or from the default.
///
/// `None` is "this repository has never said", which takes [`default_set`]. `Some(&[])` is a
/// repository that has explicitly said it owes nothing — a real answer, and a different one.
pub fn for_repo(configured: Option<&Vec<String>>) -> (Vec<Check>, Vec<String>) {
    match configured {
        None => (default_set(), Vec::new()),
        Some(words) => read(words),
    }
}

// ───────────────────────────── what the diff fired ─────────────────────────────

/// Which checks this diff triggers — **found in the diff, never reasoned about.**
///
/// [`crate::contracts::scan`]'s neighbour and its discipline: no regex, hand-written scanners over
/// the few shapes that actually matter, and a rule earns its place only if its absence would let a
/// real obligation through looking ordinary.
pub fn triggered(diff: &str) -> Vec<Check> {
    let mut out: Vec<Check> = Vec::new();
    let mut push = |c: Check| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    for line in diff.lines() {
        // The unified-diff file headers start with the same characters as content lines and are not
        // content. Missing this is how every hand-rolled diff scanner first goes wrong: `--- a/x`
        // would make every single file in every diff look like a deletion.
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        let (added, body) = match line.as_bytes().first() {
            Some(b'-') => (false, &line[1..]),
            Some(b'+') => (true, &line[1..]),
            _ => continue,
        };
        if !added {
            // A removed line that is only whitespace is a formatting artefact, not a guarantee that
            // went away. Counting it would make "this change removes lines" true of almost every
            // pull request, which is the way a check stops meaning anything.
            if !body.trim().is_empty() {
                push(Check::Deletions);
            }
            continue;
        }
        let body = body.trim();
        if is_guard(body) {
            push(Check::GuardAdded);
        }
        if is_test(body) {
            push(Check::TestAdded);
        }
        if cites_a_document(body) {
            push(Check::DocCited);
        }
    }
    out.sort();
    out
}

/// An added assertion or guard.
///
/// Word-ish rather than substring where it matters: `assert` catches `assert!`, `assert_eq!`,
/// `assertEquals` and `self.assertTrue` in one, which is the point — the shape is the same in every
/// language a reviewer here will meet.
fn is_guard(line: &str) -> bool {
    const GUARDS: [&str; 7] = [
        "assert",
        "expect(",
        "panic!",
        "unreachable!",
        "ensure!",
        "require(",
        "raise ",
    ];
    GUARDS.iter().any(|g| line.contains(g))
}

/// An added test.
fn is_test(line: &str) -> bool {
    const TESTS: [&str; 6] = [
        "#[test]",
        "#[tokio::test]",
        "fn test_",
        "def test_",
        "it(\"",
        "test(\"",
    ];
    TESTS.iter().any(|t| line.contains(t))
}

/// An added line citing a document or a plan.
///
/// A path with a documentary extension, or one under a directory people keep plans in. Not a bare
/// URL: a link to an API reference is not a plan somebody has to go and find, and treating it as
/// one would fire this on nearly every comment in this codebase.
fn cites_a_document(line: &str) -> bool {
    const SUFFIX: [&str; 3] = [".md", ".rst", ".adoc"];
    const UNDER: [&str; 3] = ["docs/", "doc/", "rfcs/"];
    SUFFIX.iter().any(|s| line.contains(s)) || UNDER.iter().any(|d| line.contains(d))
}

// ───────────────────────────── what has been answered ─────────────────────────────

/// Where the answers live: `~/.skein/owed/<repo-id>/<number>-<sha>.json`.
///
/// **The sha is in the filename**, which is [`crate::review`]'s rule for its summaries and it is
/// load-bearing here for the same reason: an audit is an answer about one tree, and a lookup for a
/// new head must MISS rather than find an answer about the old one. §8's step says *recorded at
/// this sha* and this is the whole of what enforces it.
///
/// Its own directory rather than beside the summaries, because it is not `review`'s state and
/// reaching for `prq::review_dir` would put this module inside the `{prq, review}` cycle — the
/// dependency `reviewbox` was already refused once for.
fn record_path(repo_id: &str, number: u64, head_sha: &str) -> PathBuf {
    let id: String = repo_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(80)
        .collect();
    let sha: String = head_sha
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect();
    skein_home()
        .join("owed")
        .join(id)
        .join(format!("{number}-{sha}.json"))
}

/// Which checks have been answered at this sha.
pub fn answered(repo_id: &str, number: u64, head_sha: &str) -> Vec<Check> {
    let Ok(text) = std::fs::read_to_string(record_path(repo_id, number, head_sha)) else {
        return Vec::new();
    };
    let Ok(words) = serde_json::from_str::<Vec<String>>(&text) else {
        return Vec::new();
    };
    // Only the words this build knows, and `read`'s refusals are dropped rather than reported: a
    // record written by a newer skein naming a check this one cannot evaluate is not something a
    // person needs telling about, and treating it as answered would be the widening direction.
    read(&words).0
}

/// Write one answered check down beside the others at this sha.
///
/// Read-modify-write rather than an append, so the file is always a whole list — and the read is of
/// the file rather than of a caller's idea of it, because two audits of the same pull request can
/// be in flight on two passes and the second must not erase the first.
pub fn record(repo_id: &str, number: u64, head_sha: &str, check: Check) -> Result<(), String> {
    let path = record_path(repo_id, number, head_sha);
    let mut have: Vec<String> = answered(repo_id, number, head_sha)
        .iter()
        .map(|c| c.spelled().to_string())
        .collect();
    let word = check.spelled().to_string();
    if !have.contains(&word) {
        have.push(word);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let body = serde_json::to_string(&have).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))
}

/// What is still owed: fired by the diff, asked for by the repository, and not yet answered here.
///
/// The intersection is the whole rule. A check the diff did not fire is not owed however the
/// repository is configured; a check the repository does not ask for is not owed however the diff
/// looks; and one already answered at this sha is done, which is what stops the engine auditing the
/// same removal on every pass for ever.
pub fn outstanding(set: &[Check], fired: &[Check], done: &[Check]) -> Vec<Check> {
    let mut out: Vec<Check> = set
        .iter()
        .filter(|c| fired.contains(c) && !done.contains(c))
        .copied()
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A file header is not a deletion**, which is the first way a hand-rolled diff scanner goes
    /// wrong and it goes wrong silently.
    ///
    /// **What would make this fail:** dropping the `---`/`+++` skip. Every unified diff of every
    /// pull request begins each file with `--- a/<path>`, so without the skip `Deletions` fires on
    /// every change ever made and the audit stops carrying information.
    #[test]
    fn the_headers_of_a_diff_are_not_the_lines_it_removes() {
        let added_only = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,3 @@
 fn main() {}
+fn other() {}
";
        assert_eq!(
            triggered(added_only),
            Vec::new(),
            "the file headers were counted as removed lines"
        );

        let removes = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,2 @@
 fn main() {}
-fn gone() {}
";
        assert_eq!(triggered(removes), vec![Check::Deletions]);
    }

    /// Whitespace-only removals are formatting, not a guarantee that went away.
    ///
    /// **What would make this fail:** dropping the `trim().is_empty()` test. A reformat is one of
    /// the commonest diffs there is, and an audit demanded of every one of them is an audit a
    /// person learns to skip.
    #[test]
    fn a_blank_line_that_went_away_is_not_a_deletion() {
        let reflow = "\
--- a/a.rs
+++ b/a.rs
@@ -1,4 +1,3 @@
 one
-
-   
+two
";
        assert_eq!(
            triggered(reflow),
            Vec::new(),
            "a reflow that removed only blank lines was called a deletion"
        );
        // And one real removal in the same shape does fire, or the assertion above passes by the
        // scanner being broken rather than by the blanks being skipped.
        let real = "--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,2 @@\n one\n-\n-fn gone() {}\n";
        assert_eq!(triggered(real), vec![Check::Deletions]);
    }

    /// The three scanners find what §8's table describes, and each on its own line.
    #[test]
    fn each_trigger_is_found_in_the_shape_it_actually_takes() {
        let diff = "\
--- a/a.rs
+++ b/a.rs
@@
+    assert_eq!(one, two);
+    #[test]
+    // see docs/pr-review.md for why
";
        let fired = triggered(diff);
        assert!(fired.contains(&Check::GuardAdded), "{fired:?}");
        assert!(fired.contains(&Check::TestAdded), "{fired:?}");
        assert!(fired.contains(&Check::DocCited), "{fired:?}");
        // And nothing was removed, so the row §8 leads with must not be in there.
        assert!(!fired.contains(&Check::Deletions), "{fired:?}");
    }

    /// **A check this build cannot evaluate is refused by name, never kept as a switch that does
    /// nothing.** The `workflow::Wake::computable` rule, applied to §8's two prose rows.
    ///
    /// **What would make this fail:** letting `computable` return true for everything. The two
    /// refused rows are claims about English — a comment naming a mechanism, a claim of absence —
    /// and a scanner for them would fire on the wrong pull requests while staying silent on the
    /// right ones. Keeping them in the set would mean a repository configured with only those two
    /// owes checks that can never fire, and reads as switched on.
    #[test]
    fn a_check_whose_trigger_cannot_be_seen_is_refused_by_name() {
        let (set, refused) = read(&[
            "deletions".into(),
            "mechanism-named".into(),
            "absence-claimed".into(),
            "not-a-check".into(),
        ]);
        assert_eq!(set, vec![Check::Deletions]);
        assert_eq!(refused.len(), 3, "{refused:?}");
        assert!(refused[0].contains("mechanism-named"), "{refused:?}");
        assert!(
            refused[0].contains("could never fire"),
            "the refusal does not say why: {refused:?}"
        );
        assert!(refused[2].contains("not a check this build knows"));
        // And the default is every check that CAN fire, not the empty set: §8's whole argument is
        // that the lesson must not wait on somebody remembering to write it down.
        assert!(default_set().contains(&Check::Deletions));
        assert!(!default_set().contains(&Check::MechanismNamed));
        assert_eq!(for_repo(None).0, default_set());
        // A repository that has explicitly said it owes nothing is a different answer from one
        // that has never said anything, and it is honoured.
        assert_eq!(for_repo(Some(&vec![])).0, Vec::new());
    }

    /// **Owed is the intersection**, and each of the three sets can take a check out of it.
    ///
    /// **What would make this fail:** dropping the `fired` test audits a pull request that changed
    /// nothing relevant; dropping the `set` test ignores a repository that said it does not want
    /// this; dropping the `done` test re-audits the same removal on every pass for ever, which is
    /// an unbounded spend on an engine that runs unattended.
    #[test]
    fn what_is_owed_is_what_fired_and_was_asked_for_and_is_not_yet_answered() {
        let set = vec![Check::Deletions, Check::TestAdded];
        assert_eq!(
            outstanding(&set, &[Check::Deletions], &[]),
            vec![Check::Deletions]
        );
        // Fired but not asked for.
        assert_eq!(outstanding(&set, &[Check::DocCited], &[]), Vec::new());
        // Asked for but not fired.
        assert_eq!(outstanding(&set, &[], &[]), Vec::new());
        // Fired, asked for, and already answered.
        assert_eq!(
            outstanding(&set, &[Check::Deletions], &[Check::Deletions]),
            Vec::new()
        );
    }

    /// **An answer is recorded against one commit and is not found at another** — §8's "recorded at
    /// this sha", which is the whole of the guard.
    ///
    /// **What would make this fail:** keying the record on the pull request alone. A reviewer would
    /// then audit the deletions of one commit and have that answer stand for every commit after
    /// it, which is the anchoring failure `docs/pr-review.md` §4 is about, wearing a different hat.
    #[test]
    fn an_answer_belongs_to_the_commit_it_was_given_about() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &*home);

        assert_eq!(answered("r", 7, "aaa"), Vec::new(), "a fresh home has none");
        record("r", 7, "aaa", Check::Deletions).unwrap();
        assert_eq!(answered("r", 7, "aaa"), vec![Check::Deletions]);
        assert_eq!(
            answered("r", 7, "bbb"),
            Vec::new(),
            "an audit of one commit answered for another"
        );
        assert_eq!(answered("r", 8, "aaa"), Vec::new(), "and for another PR");

        // A second check joins the first rather than replacing it.
        record("r", 7, "aaa", Check::TestAdded).unwrap();
        assert_eq!(
            answered("r", 7, "aaa"),
            vec![Check::Deletions, Check::TestAdded]
        );
        // And recording the same one twice leaves one.
        record("r", 7, "aaa", Check::TestAdded).unwrap();
        assert_eq!(answered("r", 7, "aaa").len(), 2);

        std::env::remove_var("SKEIN_HOME");
    }
}
