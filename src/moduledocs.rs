//! Standing notes on what each part of a repo *is*, so a question about a PR can be answered with
//! more than the PR.
//!
//! # Why this is not one architecture document
//!
//! A single architecture doc has a failure mode that makes it worse than nothing: it drifts, and the
//! drift is invisible. Something reads it, compares a diff against it, and clears a change that
//! moved behaviour the doc described wrongly two months ago. You would also end up reviewing pull
//! requests against the document.
//!
//! Per-module notes fix that for one reason and it is the whole design: **invalidation is local and
//! provable**. Each note records the module's path and the commit that last touched it. If the
//! module has moved since, the note is *known* stale — not suspected, not probably-fine — and it is
//! refreshed before it is trusted. A monolithic document cannot do this, because every commit in the
//! repo invalidates some unidentifiable part of it.
//!
//! # What a module is
//!
//! CODEOWNERS, when the repo has one. That file is already a declared partition of the repo into
//! parts with owners, maintained by the team for other reasons, and it is the same boundary that
//! decides which pull requests reach you at all. Repos without one fall back to top-level
//! directories, which is a guess — but a cheap one, since a note is regenerable and a wrong
//! partition costs a rewrite rather than a mistake.
//!
//! # What they are for
//!
//! Answering *your* questions ([`crate::review::ask`]), not writing summaries. A summary is about a
//! diff and the diff carries its own before-state; a question is usually about the thing the diff
//! landed in, which the diff cannot show. They are deliberately **not** fed into every summary:
//! that would multiply the cost of the one path that runs thirty times a day to improve the path
//! that runs when you ask.
//!
//! Private, under `~/.skein/review/<repo-id>/modules/` — never the repo (they would become pull
//! requests of their own) and never the shared store (skein's rule against runtime state there).

use crate::ai::claude_oneshot_with;
use crate::codeowners;
use crate::prq::review_dir;
use crate::repos::Repo;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One part of a repo worth describing on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Module {
    /// Repo-relative directory, no trailing slash.
    pub path: String,
    /// Who owns it per CODEOWNERS, empty when the repo has none.
    pub owners: Vec<String>,
}

/// A written note, and the evidence for whether it is still true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Doc {
    pub path: String,
    /// The commit that last touched this module when the note was written.
    ///
    /// This is the entire safety mechanism. Without it a note is a claim with no expiry, and the
    /// only honest thing to do with such a claim is distrust it — which would make the whole
    /// feature pointless.
    pub sha: String,
    pub text: String,
    /// When it was written, for showing you how old the answer you are reading is.
    pub written: String,
}

/// A module and what skein currently knows about it.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub path: String,
    pub owners: Vec<String>,
    /// "fresh" | "stale" | "absent"
    pub state: String,
    pub written: String,
}

/// Directories never worth describing: they are not this repo's work.
const SKIP_DIRS: [&str; 12] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "venv",
    ".venv",
    "__pycache__",
    ".git",
    "coverage",
    "tmp",
    "fixtures",
];

/// How much source one note is written from.
///
/// Truncation is stated to the model rather than hidden, exactly as in [`crate::review`]: a note
/// written from half a module should say so, because "I did not see all of this" is the difference
/// between a useful note and a confident wrong one.
const MODULE_BYTES: usize = 120_000;

/// The modules of a repo: CODEOWNERS directories when it has them, else top-level directories.
pub fn modules(repo: &Repo) -> Vec<Module> {
    let work = Path::new(&repo.work);
    let mut out: Vec<Module> = Vec::new();
    if let Some(co) = codeowners::load(work) {
        for rule in &co.rules {
            // Only patterns that name a real directory. A CODEOWNERS line like `*.md` is a perfectly
            // good ownership rule and a meaningless thing to write a note about.
            let candidate = rule.pattern.trim_start_matches('/').trim_end_matches('/');
            if candidate.is_empty() || candidate.contains('*') || candidate.contains('?') {
                continue;
            }
            if !work.join(candidate).is_dir() {
                continue;
            }
            if let Some(existing) = out.iter_mut().find(|m| m.path == candidate) {
                // Last match wins in CODEOWNERS, so a later rule's owners replace an earlier one's.
                existing.owners = rule.owners.clone();
            } else {
                out.push(Module {
                    path: candidate.to_string(),
                    owners: rule.owners.clone(),
                });
            }
        }
    }
    if out.is_empty() {
        out = top_level_dirs(work)
            .into_iter()
            .map(|path| Module {
                path,
                owners: Vec::new(),
            })
            .collect();
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.truncate(40);
    out
}

fn top_level_dirs(work: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(work) else {
        return Vec::new();
    };
    let mut dirs: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()))
        .collect();
    dirs.sort();
    dirs
}

// ───────────────────────────── provenance ─────────────────────────────

fn docs_dir(repo_id: &str) -> PathBuf {
    review_dir(repo_id).join("modules")
}

/// One file per module, named after the path with separators flattened.
fn doc_path(repo_id: &str, module: &str) -> PathBuf {
    let slug: String = module
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '~'
            }
        })
        .collect();
    docs_dir(repo_id).join(format!("{slug}.json"))
}

/// The commit that last touched this module in the working clone.
///
/// Empty when git cannot answer — which is treated as *stale* everywhere below, because a note whose
/// freshness cannot be established is exactly a note that should not be trusted.
pub fn current_sha(repo: &Repo, module: &str) -> String {
    run_capture_for(
        "git",
        &["-C", &repo.work, "log", "-1", "--format=%H", "--", module],
        Duration::from_secs(15),
    )
    .ok()
    .filter(|(_, _, code)| *code == 0)
    .map(|(out, _, _)| out.trim().to_string())
    .unwrap_or_default()
}

/// Read a stored note, whether or not it is still true.
pub fn stored(repo_id: &str, module: &str) -> Option<Doc> {
    let text = fs::read_to_string(doc_path(repo_id, module)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Is this note still describing the module as it is now?
///
/// Fails toward stale on every uncertainty: no note, no recorded sha, git silent. The cost of
/// regenerating a note that was fine is a model call; the cost of trusting one that is wrong is an
/// answer you believe.
pub fn is_fresh(repo: &Repo, doc: &Doc) -> bool {
    let now = current_sha(repo, &doc.path);
    !now.is_empty() && !doc.sha.is_empty() && now == doc.sha
}

/// Every module and whether skein has a current note for it.
pub fn status(repo: &Repo) -> Vec<Status> {
    modules(repo)
        .into_iter()
        .map(|m| {
            let doc = stored(&repo.id, &m.path);
            let state = match &doc {
                None => "absent",
                Some(d) if is_fresh(repo, d) => "fresh",
                Some(_) => "stale",
            };
            Status {
                path: m.path,
                owners: m.owners,
                state: state.to_string(),
                written: doc.map(|d| d.written).unwrap_or_default(),
            }
        })
        .collect()
}

// ───────────────────────────── writing one ─────────────────────────────

/// Read a module's source, bounded, newest-shallowest first.
///
/// Shallow files before deep ones because a module's top level is where its entry points and its
/// intent live; if the budget runs out it should run out in the leaves.
fn read_module(work: &Path, module: &str) -> (String, bool) {
    let root = work.join(module);
    let mut files: Vec<PathBuf> = Vec::new();
    collect(&root, &mut files, 0);
    files.sort_by_key(|p| (p.components().count(), p.clone()));
    let mut body = String::new();
    let mut cut = false;
    for f in files {
        if body.len() >= MODULE_BYTES {
            cut = true;
            break;
        }
        let Ok(text) = fs::read_to_string(&f) else {
            continue; // binary or unreadable — not source
        };
        let rel = f.strip_prefix(work).unwrap_or(&f).display().to_string();
        let room = MODULE_BYTES.saturating_sub(body.len());
        if text.len() > room {
            cut = true;
        }
        let mut end = text.len().min(room);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        body.push_str(&format!("\n===== {rel} =====\n{}\n", &text[..end]));
    }
    (body, cut)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 || out.len() > 400 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => collect(&path, out, depth + 1),
            // A megabyte of anything is not prose a note should be written from, and reading it
            // only to throw it away is the expensive half.
            Ok(t) if t.is_file() && e.metadata().map(|m| m.len() < 400_000).unwrap_or(false) => {
                out.push(path)
            }
            _ => {}
        }
    }
}

/// Write (or rewrite) the note for one module.
///
/// Returns an error rather than a stale note when the model is unavailable: a caller that wanted
/// current context is better served by knowing it does not have it.
pub fn write(repo: &Repo, module: &str) -> Result<Doc, String> {
    if !crate::review::summaries_enabled() {
        return Err(
            "reading is switched off — turn \"Read pull requests\" back on in Settings → Boxes."
                .into(),
        );
    }
    if !modules(repo).iter().any(|m| m.path == module) {
        return Err(format!("{module:?} is not one of this repo's modules"));
    }
    let (body, cut) = read_module(Path::new(&repo.work), module);
    if body.trim().is_empty() {
        return Err(format!("nothing readable under {module:?}"));
    }
    // Taken BEFORE the model call, not after. Between reading the source and storing the note, the
    // clone can be pulled; stamping the later sha would mark a note fresh against code it never saw.
    let sha = current_sha(repo, module);
    let prompt = format!(
        r#"Write standing notes on one part of a codebase, for a senior engineer who reviews pull requests against it. They will read these to understand what a change *means*, not to learn the code.

Write at mechanism, architecture and product level. What this part is responsible for, what it does not do, the ideas someone has to hold to read a change here, the invariants that must not break, and where it meets the rest of the system. Name the important types and entry points, but do not walk through functions or explain syntax.

Be specific and be brief — under 400 words. Prefer one sentence that would change how someone reads a diff over five that restate the file listing. If something looks deliberate and surprising, say why it is that way if the code tells you.

Do not include a preamble or a conclusion. Markdown headings are fine.
{cut_note}
Module: {module}

{body}"#,
        cut_note = if cut {
            "\nNOTE: not all of this module's source fitted below. Say so in one line at the end if it limits what you can state.\n"
        } else {
            ""
        },
        module = module,
        body = body,
    );
    let text = claude_oneshot_with(&prompt, Some("claude-sonnet-5"), Duration::from_secs(240))
        .ok_or("no note came back — the model call failed or timed out.")?;
    let doc = Doc {
        path: module.to_string(),
        sha,
        text,
        written: chrono::Utc::now().to_rfc3339(),
    };
    let dir = docs_dir(&repo.id);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    write_atomic(&doc_path(&repo.id, module), &dir, &bytes)?;
    Ok(doc)
}

/// The current note for a module, written now if absent or stale.
pub fn ensure(repo: &Repo, module: &str) -> Result<Doc, String> {
    if let Some(doc) = stored(&repo.id, module) {
        if is_fresh(repo, &doc) {
            return Ok(doc);
        }
    }
    write(repo, module)
}

/// Which modules a set of changed paths falls into.
pub fn touched(repo: &Repo, paths: &[String]) -> Vec<String> {
    let mods = modules(repo);
    let mut out: Vec<String> = Vec::new();
    for p in paths {
        // Longest match wins: with both `src` and `src/web` as modules, a change to `src/web/x`
        // belongs to the more specific one, which is the one whose note will actually be about it.
        let best = mods
            .iter()
            .filter(|m| p == &m.path || p.starts_with(&format!("{}/", m.path)))
            .max_by_key(|m| m.path.len());
        if let Some(m) = best {
            if !out.contains(&m.path) {
                out.push(m.path.clone());
            }
        }
    }
    out
}

/// Notes for the modules a change touches, for answering a question about it.
///
/// **Only notes that already exist and are fresh.** Writing one is a minute of model time, and a
/// question typed into a box is not the moment to spend it silently — the pane offers writing them
/// as its own act. A stale note is skipped rather than used: an answer grounded in a description of
/// code that has since changed is worse than an answer that says it lacked context.
pub fn fresh_notes(repo: &Repo, paths: &[String]) -> Vec<Doc> {
    touched(repo, paths)
        .into_iter()
        .filter_map(|m| stored(&repo.id, &m))
        .filter(|d| is_fresh(repo, d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (crate::testutil::TempDir, Repo) {
        let dir = crate::testutil::tempdir();
        let work = (dir.as_ref() as &Path).join("work");
        fs::create_dir_all(work.join("src").join("web")).unwrap();
        fs::create_dir_all(work.join("docs")).unwrap();
        fs::create_dir_all(work.join("node_modules")).unwrap();
        fs::write(work.join("src").join("a.rs"), "fn a() {}").unwrap();
        let repo = Repo {
            id: "r".into(),
            source: "https://github.com/a/b".into(),
            work: work.display().to_string(),
            store: String::new(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        (dir, repo)
    }

    #[test]
    fn without_codeowners_the_modules_are_the_top_level_directories() {
        let (_d, repo) = fixture();
        let paths: Vec<String> = modules(&repo).into_iter().map(|m| m.path).collect();
        assert_eq!(
            paths,
            vec!["docs", "src"],
            "vendor dirs must not be modules"
        );
    }

    #[test]
    fn codeowners_directories_become_the_modules_and_carry_their_owners() {
        let (_d, repo) = fixture();
        let gh = Path::new(&repo.work).join(".github");
        fs::create_dir_all(&gh).unwrap();
        fs::write(
            gh.join("CODEOWNERS"),
            "src/ @me\nsrc/web/ @you\n*.md @nobody\n",
        )
        .unwrap();
        let mods = modules(&repo);
        let paths: Vec<String> = mods.iter().map(|m| m.path.clone()).collect();
        assert_eq!(paths, vec!["src", "src/web"], "a glob is not a module");
        assert_eq!(mods[1].owners, vec!["@you"]);
    }

    #[test]
    fn a_codeowners_pattern_naming_no_directory_is_not_a_module() {
        let (_d, repo) = fixture();
        let gh = Path::new(&repo.work).join(".github");
        fs::create_dir_all(&gh).unwrap();
        fs::write(gh.join("CODEOWNERS"), "does/not/exist/ @me\n").unwrap();
        // Falls back rather than returning an empty list: no modules would silently mean no context.
        assert_eq!(
            modules(&repo)
                .into_iter()
                .map(|m| m.path)
                .collect::<Vec<_>>(),
            vec!["docs", "src"]
        );
    }

    #[test]
    fn a_changed_path_belongs_to_its_most_specific_module() {
        let (_d, repo) = fixture();
        let gh = Path::new(&repo.work).join(".github");
        fs::create_dir_all(&gh).unwrap();
        fs::write(gh.join("CODEOWNERS"), "src/ @me\nsrc/web/ @you\n").unwrap();
        assert_eq!(
            touched(&repo, &["src/web/index.html".to_string()]),
            vec!["src/web"]
        );
        assert_eq!(touched(&repo, &["src/a.rs".to_string()]), vec!["src"]);
        assert!(touched(&repo, &["README.md".to_string()]).is_empty());
    }

    #[test]
    fn a_note_whose_module_has_moved_is_not_fresh() {
        let (_d, repo) = fixture();
        let doc = Doc {
            path: "src".into(),
            sha: "0000000000000000000000000000000000000000".into(),
            text: "notes".into(),
            written: "2026-01-01T00:00:00Z".into(),
        };
        // The fixture is not a git repo, so `git log` cannot answer — which must read as stale, not
        // as fresh. Failing the other way would trust every note forever on a clone git cannot see.
        assert!(!is_fresh(&repo, &doc));
    }

    #[test]
    fn a_note_with_no_recorded_commit_is_never_fresh() {
        let (_d, repo) = fixture();
        let doc = Doc {
            path: "src".into(),
            sha: String::new(),
            text: "notes".into(),
            written: String::new(),
        };
        assert!(!is_fresh(&repo, &doc));
    }

    #[test]
    fn status_reports_a_module_with_no_note_as_absent() {
        let (_d, repo) = fixture();
        let _home = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let st = status(&repo);
        assert!(st.iter().all(|s| s.state == "absent"), "{st:?}");
    }

    #[test]
    fn a_module_path_cannot_escape_the_notes_directory() {
        let p = doc_path("r", "../../etc/passwd");
        assert!(!p.to_string_lossy().contains(".."), "{}", p.display());
    }

    #[test]
    fn reading_a_module_skips_vendor_directories() {
        let (_d, repo) = fixture();
        let work = Path::new(&repo.work);
        fs::write(work.join("node_modules").join("big.js"), "junk").unwrap();
        let (body, _) = read_module(work, "src");
        assert!(body.contains("fn a()"));
        assert!(!body.contains("junk"));
    }

    #[test]
    fn writing_a_note_for_an_unknown_module_is_refused() {
        let (_d, repo) = fixture();
        let err = write(&repo, "nope").unwrap_err();
        assert!(err.contains("not one of this repo's modules"), "{err}");
    }
}
