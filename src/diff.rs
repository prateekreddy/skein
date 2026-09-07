//! What a box changed on its branch, measured against the **remote** base.
//!
//! Computed inside the box, because that is where the box's checkout is: host-side git answered
//! from `~/.skein/repos/<id>/work`, which for a clone-mode box is a different checkout on a
//! different branch — a confidently wrong answer that looked exactly like a right one.

use crate::answer::Answer;
use crate::config::*;
use crate::registry::{locate_registry, store_for_box};
use crate::sandbox::sbx_guest_output;
use crate::sbx::lookup_dir;
use crate::sbx::{box_liveness, Liveness};
use crate::util::valid_name;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;
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

/// The base-ref ladder, most specific first: the configured base branch on the remote, then the
/// usual remote defaults, then their local counterparts as a last resort for a repo with no remote.
///
/// Remote-first is the point. The old host-side path measured against whatever the *host clone*
/// had checked out, which for a clone-mode box is a different branch of a different checkout —
/// a wrong answer that looked exactly like a right one.
pub(crate) fn diff_base_refs() -> Vec<String> {
    let mut refs = Vec::new();
    let configured = load_config().base_branch.trim().to_string();
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

/// Shell that resolves the base inside the box and emits the patch, base first.
pub(crate) fn diff_script(refs: &[String]) -> String {
    let list = refs
        .iter()
        .map(|r| sh_quote(r))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "root=\"$(git rev-parse --show-toplevel 2>/dev/null || pwd)\"; cd \"$root\" || exit 1; \
         base=''; for ref in {list}; do \
           if git rev-parse --verify -q \"$ref\" >/dev/null 2>&1; then base=\"$ref\"; break; fi; \
         done; \
         range=HEAD; \
         if [ -n \"$base\" ]; then mb=\"$(git merge-base HEAD \"$base\" 2>/dev/null || true)\"; \
           [ -n \"$mb\" ] && range=\"$mb\"; fi; \
         printf '{DIFF_BASE_MARK}%s\\n' \"${{base:-HEAD}}\"; \
         git diff \"$range\" 2>/dev/null | head -c {DIFF_CAP}"
    )
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
        if let Ok(raw) = sbx_guest_output(name, &diff_script(&diff_base_refs()), DIFF_TIMEOUT) {
            let (base, mut patch) = split_diff(&raw);
            if patch.len() > DIFF_CAP {
                // Not `String::truncate`: it panics on a byte index inside a character, and a diff
                // is the likeliest place to meet one — any non-ASCII line the cap happens to land in.
                patch = clip_bytes(&patch, DIFF_CAP).to_string();
                patch.push_str("\n\n# … diff truncated by skein (too large to render) …\n");
            }
            // `sbx_guest_output` goes through `Place::exec`, which for a placed box is an
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
    let patch = fs::read_to_string(&path).ok()?;
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

pub(crate) fn git_ok(dir: &str, args: &[&str]) -> bool {
    let mut a = vec!["-C", dir];
    a.extend_from_slice(args);
    let mut command = Command::new("git");
    command.args(&a);
    bounded_output(&mut command, "git", Duration::from_secs(10))
        .is_ok_and(|output| output.status.success())
}

/// The git ref a host-side branch-vs-base range starts at, or None if `dir` isn't a git repo here.
/// Same ladder as the in-box diff, so the takeover brief's file list and the diff pane agree about
/// what "the branch" means. With no common ancestor it falls back to `HEAD` (uncommitted only)
/// rather than exploding into an unrelated-history diff.
pub(crate) fn git_range(dir: &str) -> Option<String> {
    if !Path::new(dir).join(".git").exists() {
        return None;
    }
    let refs = diff_base_refs();
    let base = refs
        .iter()
        .find(|b| git_ok(dir, &["rev-parse", "--verify", "-q", b]));
    let merge_base = base.and_then(|b| {
        let mut command = Command::new("git");
        command.args(["-C", dir, "merge-base", "HEAD", b]);
        let o = bounded_output(&mut command, "git merge-base", Duration::from_secs(10)).ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (o.status.success() && !s.is_empty()).then_some(s)
    });
    Some(merge_base.unwrap_or_else(|| "HEAD".into()))
}

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
    let s = fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let stat = DiffStat {
        files: get("files"),
        ins: get("ins"),
        del: get("del"),
    };
    (stat.files != 0 || stat.ins != 0 || stat.del != 0).then_some(stat)
}

/// The files a box changed on its branch (host-side `git diff --name-only`, or parsed from the
/// box-reported patch in clone mode where this host can't see the box's `.git`). Feeds the
/// takeover brief, which is the only thing that still needs a file list.
pub fn changed_files(name: &str) -> Vec<String> {
    if !valid_name(name) {
        return vec![];
    }
    if let Some(dir) = lookup_dir(name) {
        if let Some(range) = git_range(&dir) {
            let mut command = Command::new("git");
            command.args(["-C", &dir, "diff", "--name-only", &range]);
            if let Ok(out) = bounded_output(
                &mut command,
                "git diff --name-only",
                Duration::from_secs(15),
            ) {
                if out.status.success() {
                    let v: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !v.is_empty() {
                        return v;
                    }
                }
            }
        }
    }
    // clone-mode fallback: pull the file list out of the patch the box reported.
    let mut files = Vec::new();
    if let Some(reg) = locate_registry()
        .ok()
        .and_then(|r| r.parent().map(|p| p.to_path_buf()))
    {
        if let Ok(patch) = fs::read_to_string(reg.join("diffs").join(format!("{name}.patch"))) {
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
        let _g = env_lock();
        let dir = tempdir();
        env::set_var("SKEIN_HOME", &dir);
        save_config(&Config::default()).unwrap();
        assert_eq!(
            diff_base_refs(),
            ["origin/main", "origin/master", "main", "master"],
            "remote refs first; the local ones are a last resort for a repo with no remote"
        );
        save_config(&Config {
            base_branch: "develop".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            diff_base_refs().first().map(String::as_str),
            Some("origin/develop"),
            "a repo whose base branch is `develop` must not be diffed against main"
        );
        save_config(&Config {
            base_branch: "main".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            diff_base_refs(),
            ["origin/main", "origin/master", "main", "master"],
            "configuring the default must not duplicate it in the ladder"
        );
        // The script names every ref it will try, so a base that never resolves is visible in the
        // command rather than being silently swallowed into `HEAD`.
        let script = diff_script(&diff_base_refs());
        assert!(script.contains("origin/main"), "{script}");
        assert!(script.contains("merge-base"), "{script}");
        assert!(
            script.contains("show-toplevel"),
            "the diff runs at the box's repo root, not wherever the shell landed: {script}"
        );
        env::remove_var("SKEIN_HOME");
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

    #[test]
    fn git_range_handles_repo_and_nonrepo() {
        if Command::new("git").arg("--version").output().is_err() {
            return; // git not available in this environment
        }
        // `git_range` reads the base-branch ladder out of the config, so it resolves
        // `config::skein_home` — which refuses an unpinned test rather than answering with the real
        // `~/.skein` (SKEIN-626). Unpinned it read the owner's live `~/.skein/config.json`, and it
        // only ever passed because a neighbour in the same process had left `$SKEIN_HOME` set.
        let _g = env_lock();
        // A home of its own, not `dir`: `dir` becomes a git repo below and `git add .` would
        // commit whatever the config layer put there.
        let home = tempdir();
        env::set_var("SKEIN_HOME", &home);
        let dir = tempdir();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["-C", d];
            a.extend_from_slice(args);
            assert!(Command::new("git")
                .args(&a)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        fs::write(dir.join("a.txt"), "hello\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "x"]);
        fs::write(dir.join("a.txt"), "hello world\n").unwrap();
        // A repo with no remote still yields a usable range — the local branch tail of the ladder.
        let range = git_range(d).expect("a git repo yields a range");
        assert!(!range.is_empty());

        let empty = tempdir(); // not a git repo → None, never explodes
        assert!(git_range(empty.to_str().unwrap()).is_none());
        env::remove_var("SKEIN_HOME");
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
}
