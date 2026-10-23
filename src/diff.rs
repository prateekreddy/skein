//! What a box changed on its branch, measured against the **remote** base.
//!
//! Computed inside the box, because that is where the box's checkout is: host-side git answered
//! from `~/.skein/repos/<id>/work`, which for a clone-mode box is a different checkout on a
//! different branch — a confidently wrong answer that looked exactly like a right one.

use crate::answer::Answer;
use crate::place::located;
use crate::registry::{locate_registry, store_for_box};
use crate::sbx::{box_liveness, Liveness};
use crate::util::valid_name;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Diff summary a box reports for its branch-vs-base work (written by box-diff.sh).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
pub struct DiffStat {
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub ins: u32,
    #[serde(default)]
    pub del: u32,
}

/// Read the full branch-vs-base patch a box wrote to `<store>/diffs/<name>.patch`.
/// (Boxes report their own diff because `sbx run` can't exec an arbitrary command in them.)
/// A branch-vs-base patch and the ref it was measured from. Wrapped in an [`Answer`] by
/// [`box_diff`], which is where "which tree is this?" gets answered.
#[derive(Debug, Clone, Serialize)]
pub struct Diff {
    pub patch: String,
    /// The ref the diff starts from — `origin/main`, or `HEAD` when no base ref resolves (in
    /// which case the patch is uncommitted work only). Shown, because a diff whose base you
    /// can't see is a number you can't act on.
    pub base: String,
}

const DIFF_BASE_MARK: &str = "SKEIN_DIFF_BASE ";

pub(crate) const DIFF_CAP: usize = 2_000_000;

/// The most of a stored patch skein reads back. The box caps what it shows at [`DIFF_CAP`]; the file
/// it writes is its own, so this is a bound on what a poll holds rather than a measure of a diff.
const PATCH_READ_CAP: u64 = 64 << 20;

/// The base-ref ladder, most specific first: the configured base branch on the remote, then the
/// usual remote defaults, then their local counterparts as a last resort for a repo with no remote.
///
/// Remote-first is the point. The old host-side path measured against whatever the *host clone*
/// had checked out, which for a clone-mode box is a different branch of a different checkout —
/// a wrong answer that looked exactly like a right one.
pub(crate) fn diff_base_refs(repo: Option<&crate::repos::Repo>) -> Vec<String> {
    let mut refs = Vec::new();
    // The repo's own base (`Repo::base_branch`); a box of no registered repo has none.
    let configured = repo
        .map(|r| r.base_branch.trim().to_string())
        .unwrap_or_default();
    if !configured.is_empty() {
        refs.push(format!("origin/{configured}"));
    }
    for r in ["origin/main", "origin/master", "main", "master"] {
        if !refs.iter().any(|x| x == r) {
            refs.push(r.to_string());
        }
    }
    refs
}

/// Shell that resolves the base inside the box and leaves it in `$base` (empty when no ref
/// resolves) and the start of the branch range in `$range` (`HEAD` when there is no common ancestor,
/// so the answer is uncommitted work rather than an unrelated-history diff). Shared by every
/// question skein asks of a box's git, so the diff pane, the takeover brief's file list and its
/// commits agree about what "the branch" means.
///
/// The branch the box was cloned from, which the clone records as `skein.base`, is tried before
/// `refs` — the same first rung `box-diff.sh` uses, so the pane skein asks for and the diff the box
/// stores measure against the same base (SKEIN-1283).
fn base_script(refs: &[String]) -> String {
    let list = refs
        .iter()
        .map(|r| sh_quote(r))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; cd \"$root\" || exit 1; \
         rec=\"$(git config --get skein.base 2>/dev/null)\"; \
         base=''; for ref in ${{rec:+\"origin/$rec\"}} {list}; do \
           if git rev-parse --verify -q \"$ref\" >/dev/null 2>&1; then base=\"$ref\"; break; fi; \
         done; \
         range=HEAD; \
         if [ -n \"$base\" ]; then mb=\"$(git merge-base HEAD \"$base\" 2>/dev/null || true)\"; \
           [ -n \"$mb\" ] && range=\"$mb\"; fi; "
    )
}

/// Shell that resolves the base inside the box and emits the patch, base first.
pub(crate) fn diff_script(refs: &[String]) -> String {
    format!(
        "{}printf '{DIFF_BASE_MARK}%s\\n' \"${{base:-HEAD}}\"; \
         git diff \"$range\" 2>/dev/null | head -c {DIFF_CAP}",
        base_script(refs)
    )
}

/// Shell that resolves the base inside the box and lists the files changed since it, one a line.
pub(crate) fn names_script(refs: &[String]) -> String {
    format!(
        "{}git diff --name-only \"$range\" 2>/dev/null",
        base_script(refs)
    )
}

/// Shell that resolves the base inside the box and lists the branch's commit subjects, newest
/// first. With no base the range is `HEAD..HEAD`, which is no commits rather than all of them.
pub(crate) fn commits_script(refs: &[String]) -> String {
    format!(
        "{}git log --format=%s -n 20 \"$range..HEAD\" 2>/dev/null",
        base_script(refs)
    )
}

/// Run one of the scripts above inside `name`'s own namespace, and only while it is running.
///
/// **Never against the box's tree from here.** The tree and its `.git` are the box's to write, and
/// skein-server runs as the same user as every box, so git run on that tree from outside the box
/// runs the box's own config with the server's reach: `core.fsmonitor` on `git diff`, and a
/// `filter.<x>.clean` that `.gitattributes` names on anything that refreshes the index. No `-c`
/// override covers the filters, whose names the box chooses. Inside the box the same config runs
/// with the box's own reach, which it already had (SKEIN-1252).
fn in_box_lines(name: &str, script: &str) -> Option<Vec<String>> {
    if box_liveness(name) != Some(Liveness::Running) {
        return None;
    }
    let out = located(name).and_then(|p| p.exec(script, Duration::from_secs(15)));
    let lines: Vec<String> = out
        .ok()?
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// [`in_box_lines`] for the branch's commit subjects — the takeover brief's and the session tab's
/// fallback when the box has not yet written its own `.commits` file.
pub(crate) fn in_box_commits(name: &str) -> Option<Vec<String>> {
    let refs = diff_base_refs(crate::repos::repo_for_box(name).as_ref());
    in_box_lines(name, &commits_script(&refs))
}

/// What a box holds that no remote has: the work a destroy takes with it (SKEIN-1313).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Unpushed {
    /// Commits on any local branch that no remote-tracking ref has: never pushed, never fetched.
    pub commits: u64,
    /// Paths `git status` lists — staged, unstaged or untracked.
    pub changes: u64,
}

/// Shell that prints [`Unpushed`]'s two numbers, one a line, and nothing at all when git cannot
/// answer — so a checkout git cannot read is "could not ask", never "nothing to lose".
pub(crate) const UNPUSHED_SCRIPT: &str =
    "cd \"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\" || exit 1; \
     c=\"$(git rev-list --count --branches --not --remotes 2>/dev/null)\" || exit 1; \
     s=\"$(git status --porcelain 2>/dev/null)\" || exit 1; \
     printf '%s\\n%s\\n' \"$c\" \"$(printf '%s' \"$s\" | grep -c .)\"";

/// [`UNPUSHED_SCRIPT`]'s answer, or `None` when it is not two numbers.
pub(crate) fn parse_unpushed(lines: &[String]) -> Option<Unpushed> {
    match lines {
        [c, s] => Some(Unpushed {
            commits: c.parse().ok()?,
            changes: s.parse().ok()?,
        }),
        _ => None,
    }
}

/// [`Unpushed`] for a box, asked of the box itself; `None` when it is not running or git there
/// could not answer. Never read from a file the box wrote earlier: a stale "nothing to lose" in
/// front of a destroy is worse than "could not ask".
pub fn box_unpushed(name: &str) -> Option<Unpushed> {
    if !valid_name(name) {
        return None;
    }
    parse_unpushed(&in_box_lines(name, UNPUSHED_SCRIPT)?)
}

/// Split the box's answer into (base, patch). A patch line can say anything, so only the *first*
/// line is ever read as the marker.
pub(crate) fn split_diff(raw: &str) -> (String, String) {
    match raw.split_once('\n') {
        Some((first, rest)) if first.starts_with(DIFF_BASE_MARK) => (
            first[DIFF_BASE_MARK.len()..].trim().to_string(),
            rest.to_string(),
        ),
        _ => ("HEAD".into(), raw.to_string()),
    }
}

/// The branch-vs-base patch for a box.
///
/// Computed **inside the box**, because that is where the box's checkout is. Host-side git was
/// answering from `~/.skein/repos/<id>/work` — a different clone on a different branch — which is
/// silently wrong for every clone-mode box and empty for one whose host clone has no working tree.
/// On demand only, never per tick: it forks a git inside a sandbox.
pub fn box_diff(name: &str) -> Option<Answer<Diff>> {
    if !valid_name(name) {
        return None;
    }
    if box_liveness(name) == Some(Liveness::Running) {
        let refs = diff_base_refs(crate::repos::repo_for_box(name).as_ref());
        if let Ok(raw) = located(name).and_then(|p| p.exec(&diff_script(&refs), DIFF_TIMEOUT)) {
            let (base, mut patch) = split_diff(&raw);
            if patch.len() > DIFF_CAP {
                // Not `String::truncate`: it panics on a byte index inside a character, and a diff
                // is the likeliest place to meet one — any non-ASCII line the cap happens to land in.
                patch = clip_bytes(&patch, DIFF_CAP).to_string();
                patch.push_str("\n\n# … diff truncated by skein (too large to render) …\n");
            }
            // `Place::exec`, which for a placed box is an
            // `nsenter` into its namespace — §2.3's `enter`, and the reason this is on-demand only.
            return Some(Answer::from_box(
                Diff { patch, base },
                crate::source::Source::Enter,
            ));
        }
    }
    // The box can't be asked — fall back to what it wrote at its last turn end, and say so, so a
    // stale patch is never mistaken for the current tree.
    let path = store_for_box(name)?
        .join("diffs")
        .join(format!("{name}.patch"));
    let patch = crate::boxfile::read_text(&path, PATCH_READ_CAP).ok()?;
    // *When* it was written is the whole question for a stored answer: "at its last turn end" is
    // reassuring if that was a minute ago and misleading if it was yesterday.
    let written = file_ago(&path).unwrap_or_default();
    (!patch.trim().is_empty()).then(|| {
        Answer::from_store(
            Diff {
                patch,
                base: String::new(),
            },
            "the box isn't running — this is the patch it wrote at its last turn end",
        )
        .at(written)
    })
}

const DIFF_TIMEOUT: Duration = Duration::from_secs(30);

/// The diff± badge for a fleet row: the shortstat the box itself wrote at its last turn end.
///
/// The box's own number, never the host's. Host-side git ran against whatever `dir` resolved to
/// here — for a clone-mode box that's a different checkout on a different branch — so it produced
/// a plausible wrong number for exactly the boxes the badge matters most for. Free, because the
/// box already wrote it to `<store>/diffs/<name>.json`; this runs on every fleet tick and must
/// never fork anything.
pub(crate) fn read_diffstat_file(name: &str) -> Option<DiffStat> {
    let path = store_for_box(name)?
        .join("diffs")
        .join(format!("{name}.json"));
    // Through `boxfile`, because this runs on every board poll and the box writes this directory:
    // a FIFO here parked a blocking-pool thread per poll per tab (SKEIN-1257).
    let s = crate::boxfile::read_text(&path, crate::boxfile::SIGNAL_CAP).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let stat = DiffStat {
        files: get("files"),
        ins: get("ins"),
        del: get("del"),
    };
    (stat.files != 0 || stat.ins != 0 || stat.del != 0).then_some(stat)
}

/// The files a box changed on its branch: asked of the box itself while it runs, else parsed from
/// the patch it reported at its last turn end. Feeds the takeover brief, which is the only thing
/// that still needs a file list. Never `git` against the box's tree from here — see
/// [`in_box_lines`].
pub fn changed_files(name: &str) -> Vec<String> {
    if !valid_name(name) {
        return vec![];
    }
    let refs = diff_base_refs(crate::repos::repo_for_box(name).as_ref());
    if let Some(files) = in_box_lines(name, &names_script(&refs)) {
        return files;
    }
    // clone-mode fallback: pull the file list out of the patch the box reported.
    let mut files = Vec::new();
    if let Some(reg) = locate_registry()
        .ok()
        .and_then(|r| r.parent().map(|p| p.to_path_buf()))
    {
        let path = reg.join("diffs").join(format!("{name}.patch"));
        if let Ok(patch) = crate::boxfile::read_text(&path, PATCH_READ_CAP) {
            for line in patch.lines() {
                if let Some(rest) = line.strip_prefix("+++ b/") {
                    let f = rest.trim();
                    if !f.is_empty() && f != "/dev/null" {
                        files.push(f.to_string());
                    }
                }
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::testutil::*;
    #[allow(unused_imports)]
    use std::{env, fs};

    // A diff is only meaningful against a base you can name, and the base has to be the REMOTE
    // branch — measuring against a local ref is how the old host-side path produced a confident
    // wrong answer for every clone-mode box.
    #[test]
    fn the_diff_base_ladder_prefers_the_configured_remote_branch() {
        let repo = |base: &str| crate::repos::Repo {
            base_branch: base.into(),
            ..Default::default()
        };
        assert_eq!(
            diff_base_refs(None),
            ["origin/main", "origin/master", "main", "master"],
            "remote refs first; the local ones are a last resort for a repo with no remote"
        );
        assert_eq!(
            diff_base_refs(Some(&repo(""))),
            diff_base_refs(None),
            "a repo that names no base is the remote defaults"
        );
        assert_eq!(
            diff_base_refs(Some(&repo("develop")))
                .first()
                .map(String::as_str),
            Some("origin/develop"),
            "a repo whose base branch is `develop` must not be diffed against main"
        );
        assert_eq!(
            diff_base_refs(Some(&repo("main"))),
            ["origin/main", "origin/master", "main", "master"],
            "configuring the default must not duplicate it in the ladder"
        );
        // The script names every ref it will try, so a base that never resolves is visible in the
        // command rather than being silently swallowed into `HEAD`.
        let script = diff_script(&diff_base_refs(None));
        assert!(script.contains("origin/main"), "{script}");
        assert!(script.contains("merge-base"), "{script}");
        assert!(
            script.contains("show-toplevel"),
            "the diff runs at the box's repo root, not wherever the shell landed: {script}"
        );
    }

    // The box answers with the base on the first line and the patch after it. A patch can contain
    // anything — including that marker — so only the first line may ever be read as one.
    #[test]
    fn the_base_is_read_from_the_first_line_and_only_the_first_line() {
        let (base, patch) = split_diff("SKEIN_DIFF_BASE origin/main\ndiff --git a/x b/x\n+ok\n");
        assert_eq!(base, "origin/main");
        assert_eq!(patch, "diff --git a/x b/x\n+ok\n");
        // A patch that quotes the marker must not move the base.
        let (base, patch) = split_diff("SKEIN_DIFF_BASE HEAD\n+SKEIN_DIFF_BASE origin/evil\n");
        assert_eq!(base, "HEAD");
        assert!(patch.contains("origin/evil"), "kept in the patch, not read");
        // No marker at all (an old box, or a shell that died early) is a patch with an unknown
        // base — reported as HEAD rather than guessed at.
        let (base, patch) = split_diff("diff --git a/x b/x\n");
        assert_eq!(base, "HEAD");
        assert_eq!(patch, "diff --git a/x b/x\n");
        assert_eq!(split_diff("").0, "HEAD");
    }

    // What a destroy would take (SKEIN-1313), run with real git in a scratch repo: commits on every
    // local branch that no remote-tracking ref has, and every path `git status` lists. Fails if the
    // count is of HEAD alone (the commit on `other` is missed: 2, not 3), if a pushed commit is
    // counted (4), if untracked files are left out (1 change, not 2), or if a directory git cannot
    // read answers "0 and 0" — nothing to lose — rather than nothing.
    #[test]
    fn a_box_counts_the_commits_no_remote_has_and_its_uncommitted_paths() {
        use std::process::Command;
        let dir = tempdir();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&*dir)
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        let run = |at: &std::path::Path| {
            let out = Command::new("sh")
                .args(["-c", UNPUSHED_SCRIPT])
                .current_dir(at)
                .output()
                .expect("sh runs");
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["commit", "-q", "--allow-empty", "-m", "pushed"]);
        git(&["update-ref", "refs/remotes/origin/main", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);
        git(&["branch", "other", "origin/main"]);
        git(&["checkout", "-q", "other"]);
        git(&["commit", "-q", "--allow-empty", "-m", "on another branch"]);
        fs::write(dir.join("tracked.txt"), "one\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-q", "-m", "tracked"]);
        git(&["update-ref", "refs/remotes/origin/other", "HEAD"]);
        git(&["commit", "-q", "--allow-empty", "-m", "three"]);
        fs::write(dir.join("tracked.txt"), "edited\n").unwrap();
        fs::write(dir.join("new.txt"), "new\n").unwrap();

        assert_eq!(
            parse_unpushed(&run(&dir)),
            Some(Unpushed {
                commits: 3,
                changes: 2
            })
        );

        let not_git = tempdir();
        assert_eq!(run(&not_git), Vec::<String>::new());
        assert_eq!(parse_unpushed(&[]), None);
        assert_eq!(parse_unpushed(&["x".into(), "1".into()]), None);
    }

    // The scripts the box runs for the takeover brief, run here with real git in a scratch repo:
    // the file list must hold both the branch's commits and its uncommitted edits, and the commits
    // must be the branch's only. Fails if `$range` stops being the merge-base (a diff against `HEAD`
    // loses `b.txt`; a log of all of `HEAD` gains "on main").
    #[test]
    fn the_box_lists_its_branch_files_and_commits_against_the_base() {
        use std::process::Command;
        let dir = tempdir();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&*dir)
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "on main"]);
        git(&["checkout", "-q", "-b", "feat"]);
        fs::write(dir.join("b.txt"), "two\n").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "-q", "-m", "on the branch"]);
        fs::write(dir.join("a.txt"), "one, edited\n").unwrap();

        let run = |script: String| {
            let out = Command::new("sh")
                .args(["-c", &script])
                .current_dir(&*dir)
                .output()
                .expect("sh runs");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let refs = diff_base_refs(None);
        let names = run(names_script(&refs));
        assert_eq!(names.lines().collect::<Vec<_>>(), ["a.txt", "b.txt"]);
        let commits = run(commits_script(&refs));
        assert_eq!(commits.lines().collect::<Vec<_>>(), ["on the branch"]);

        // A box cloned from a base that is neither `main` nor `master` records it, and the host's
        // questions use it first, as the box's own `box-diff.sh` does (SKEIN-1283): measured
        // against `main` the branch would also own "on develop" and `c.txt`.
        git(&["checkout", "-q", "-b", "develop", "main"]);
        fs::write(dir.join("c.txt"), "three\n").unwrap();
        git(&["add", "c.txt"]);
        git(&["commit", "-q", "-m", "on develop"]);
        git(&["update-ref", "refs/remotes/origin/develop", "develop"]);
        git(&["checkout", "-q", "-b", "feat2"]);
        fs::write(dir.join("d.txt"), "four\n").unwrap();
        git(&["add", "d.txt"]);
        git(&["commit", "-q", "-m", "on feat2"]);
        git(&["config", "skein.base", "develop"]);
        // `a.txt` is still the uncommitted edit from above, carried across the checkouts.
        assert_eq!(
            run(names_script(&refs)).lines().collect::<Vec<_>>(),
            ["a.txt", "d.txt"]
        );
        assert_eq!(
            run(commits_script(&refs)).lines().collect::<Vec<_>>(),
            ["on feat2"]
        );
    }

    #[test]
    fn a_boxs_changed_files_are_read_from_the_patch_it_reported() {
        let _g = env_lock();
        let dir = tempdir();
        // `changed_files` looks the box up through `repos.json` before it falls back to the patch,
        // so it resolves `config::skein_home` — which refuses an unpinned test rather than
        // answering with the real `~/.skein` (SKEIN-626). Unpinned it read the owner's live
        // `~/.skein/repos.json`, and it only ever passed because a neighbour had left the variable
        // set. An empty home is the right fixture: the repo lookup must miss, or the patch fallback
        // this test is about is never reached.
        env::set_var("SKEIN_HOME", &dir);
        // `changed_files` asks whether the box is running before anything else, which reads the
        // fleet's placement records and `sbx ls`: both pinned to nothing here.
        let mut pins = env_pins();
        pins.set("SKEIN_FLEET_ROOT", dir.join("fleet"))
            .set("SKEIN_LS_CMD", "false");
        let reg = dir.join("sandboxes.json");
        // dirs aren't git repos here → changed_files falls back to parsing the reported patches
        fs::write(
            &reg,
            r#"{"box-a":{"branch":"a","dir":"/nope-a","lastSeen":"","status":""},
               "box-b":{"branch":"b","dir":"/nope-b","lastSeen":"","status":""}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("diffs")).unwrap();
        fs::write(
            dir.join("diffs").join("box-a.patch"),
            "+++ b/src/shared.rs\n+++ b/src/only_a.rs\n",
        )
        .unwrap();
        fs::write(
            dir.join("diffs").join("box-b.patch"),
            "+++ b/src/shared.rs\n+++ b/src/only_b.rs\n",
        )
        .unwrap();
        env::set_var("SKEIN_REGISTRY", &reg);
        env::remove_var("SKEIN_SHARED");

        assert_eq!(
            changed_files("box-a"),
            vec!["src/only_a.rs", "src/shared.rs"]
        );
        assert_eq!(
            changed_files("box-b"),
            vec!["src/only_b.rs", "src/shared.rs"]
        );
        env::remove_var("SKEIN_REGISTRY");
        env::remove_var("SKEIN_HOME");
    }

    // The board reads `<store>/diffs/<name>.json` on every poll, and the box writes that directory.
    // Fails if the read opens the path plainly again: with no writer, the FIFO open waits for ever
    // and `within` fails after five seconds.
    #[test]
    fn a_fifo_where_the_diff_badge_is_read_is_no_badge_rather_than_a_hang() {
        let _g = env_lock();
        let dir = tempdir();
        let mut pins = env_pins();
        pins.set("SKEIN_HOME", &dir)
            .set("SKEIN_FLEET_ROOT", dir.join("fleet"))
            .set("SKEIN_REGISTRY", dir.join("sandboxes.json"))
            .unset("SKEIN_SHARED");
        fs::write(
            dir.join("sandboxes.json"),
            r#"{"fifo-box":{"branch":"b","dir":"/nope","lastSeen":"","status":""}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("diffs")).unwrap();
        let fifo = dir.join("diffs").join("fifo-box.json");
        crate::boxfile::plant_fifo(&fifo);
        let stat =
            crate::boxfile::within(&fifo, "the diff badge", || read_diffstat_file("fifo-box"));
        assert_eq!(stat, None);
    }
}
