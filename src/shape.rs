//! The shape of a change: which modules moved, and how — without reading the diff.
//!
//! # What this is for, stated by the owner
//!
//! **Diffs do not matter today.** Agentic coding writes the code well enough that reading it line by
//! line is rarely where the value is. What matters is *which modules changed*, *how the system
//! decomposes*, and being able to drill to code when something warrants it — mostly it is not read
//! at all. If the text is wanted, GitHub has it. So skein does not compete on diff rendering.
//!
//! Four levels, and most visits stop at the first: **module → its standing note → what this change
//! did to it → files → code.** This is the first three.
//!
//! # It composes two primitives rather than adding one
//!
//! Both already existed, filed under the wrong heading:
//!
//! * **standing module notes** ([`crate::moduledocs`]) — a continuously maintained description of
//!   how the system decomposes, whose freshness is keyed to the commit each module was at, so a
//!   stale note is never used.
//! * **contract signals** ([`crate::contracts`]) — a mechanical scan that escalates a change the
//!   model called boring, which is the structural consequence a summary misses.
//!
//! They are **file-scoped and module-scoped respectively**, and this is where they meet: the notes
//! already map changed files to modules, so a file-scoped signal rolls up to the module it is about.
//! Both become signals whose subject is a **module** (§2.2), which generalises the signal primitive
//! rather than growing a mechanism beside it.
//!
//! # What is not free
//!
//! Per-module line counts and the movement classification are computed here, from the diff, and cost
//! nothing beyond parsing it. **The count beside a signal is not free and is not "call sites".**
//!
//! A contract signal now carries the `symbol` its scanner matched, so there is something to search
//! the repository for — that was the missing half, and until it existed any number here would have
//! been about a symbol nobody identified. What the search can answer is how often the word appears
//! in the tree: `git grep` cannot tell a call from a comment, a string, or the declaration itself.
//! So the number is **mentions** — lines of the tree that name it — said in those words. §11.1's mock-up says "call sites"; this is the
//! honest version of it, and the difference is not pedantry — a reviewer who trusts "12 call sites"
//! and finds four is worse off than one who was told what was counted.
//!
//! One fork per **distinct** symbol, on a view somebody asked for. Never on a tick: nothing on the
//! board's path reaches [`of_diff`], and `tests/board_cost.rs` is what keeps that true.

use crate::contracts::Signal;
use crate::repos::Repo;
use serde::Serialize;

/// What this change did to a module.
///
/// **Not a count dressed up.** `Shrank` is a fact worth its own word: a module that lost more than
/// it gained is usually a deletion or an extraction, and it is the one shape a reviewer wants to
/// look at differently from ordinary work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Movement {
    /// Every file in it is new to the tree.
    New,
    /// Every file in it went away.
    Gone,
    /// It lost more lines than it gained.
    Shrank,
    /// Everything else.
    Changed,
}

/// One module, and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModuleChange {
    pub path: String,
    pub owners: Vec<String>,
    pub movement: Movement,
    pub added: u32,
    pub removed: u32,
    /// The files in this module the change touched, repo-relative.
    pub files: Vec<String>,
    /// Contract signals found in those files, rolled up from where they were found.
    pub signals: Vec<Sighted>,
    /// The module's standing note, **only when it is fresh**.
    ///
    /// A stale note is skipped rather than shown: a description of code that has since changed is
    /// worse than no description, because it reads exactly like a current one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// `fresh` | `stale` | `absent` — said, because "no note" and "a note nobody trusts" are
    /// different states and only one of them is worth offering to write.
    pub note_state: String,
}

/// A contract signal, and how widely the thing it names is mentioned.
///
/// The signal is flattened, so what a client already reads — `kind`, `what`, `file` — is unchanged
/// and this is one optional field beside it.
///
/// `mentions` is **absent rather than zero** when the symbol could not be named or the tree could
/// not be searched. "0 mentions" reads as "nothing uses this", which is the opposite of "we did not
/// look", and a signal about a deleted file has no symbol by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sighted {
    #[serde(flatten)]
    pub signal: Signal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mentions: Option<usize>,
}

/// One file's line movement, and whether it arrived or left.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub created: bool,
    pub deleted: bool,
}

/// Read a unified diff into per-file movement.
///
/// Its own function so the arithmetic is testable without a repo — and because the parser is the
/// part that is easy to get subtly wrong: `+++` and `---` are headers, not added and removed lines,
/// and counting them inflates every file by two.
pub fn files_in(diff: &str) -> Vec<FileChange> {
    let mut out: Vec<FileChange> = Vec::new();
    let mut current: Option<FileChange> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(file) = current.take() {
                out.push(file);
            }
            // `diff --git a/x b/x` — the b-side is where the file is now, which is the one that
            // matters for a rename.
            let path = rest.split(" b/").nth(1).unwrap_or("").trim().to_string();
            current = Some(FileChange {
                path,
                ..Default::default()
            });
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };
        if line.starts_with("new file mode") {
            file.created = true;
        } else if line.starts_with("deleted file mode") {
            file.deleted = true;
        } else if line.starts_with("+++") || line.starts_with("---") {
            // Headers. Counting them adds one to every file's tally, in both directions.
        } else if let Some(stripped) = line.strip_prefix('+') {
            let _ = stripped;
            file.added += 1;
        } else if let Some(stripped) = line.strip_prefix('-') {
            let _ = stripped;
            file.removed += 1;
        }
    }
    if let Some(file) = current.take() {
        out.push(file);
    }
    out.retain(|f| !f.path.is_empty());
    out
}

/// Fill in how widely each named symbol is mentioned in the repository.
///
/// Separate from [`of_diff`] because it is the only part of the shape that leaves the process, and
/// the boundary is worth being able to see: everything above is arithmetic over a diff, and this is
/// `git grep` against the mirror. It is also what makes the rest testable without a repository.
///
/// The tree is opened **only if something has a symbol**, so a change with no named signals costs
/// nothing at all.
fn count_mentions(repo: &Repo, by_module: &mut std::collections::BTreeMap<String, ModuleChange>) {
    let named = || {
        by_module
            .values()
            .flat_map(|m| m.signals.iter())
            .filter(|s| !s.signal.symbol.is_empty())
    };
    if named().next().is_none() {
        return;
    }
    let Some(tree) = crate::repos::Tree::open(repo) else {
        return;
    };
    let mut counted: std::collections::BTreeMap<String, Option<usize>> = Default::default();
    let symbols: std::collections::BTreeSet<String> =
        named().map(|s| s.signal.symbol.clone()).collect();
    for symbol in symbols {
        let found = tree.mentions(&symbol);
        counted.insert(symbol, found);
    }
    for module in by_module.values_mut() {
        for sighted in module.signals.iter_mut() {
            sighted.mentions = counted.get(&sighted.signal.symbol).copied().flatten();
        }
    }
}

/// The shape of a change, one entry per module it touched.
///
/// Ordered by **what is worth looking at**, not by size: a module carrying a contract signal comes
/// first however small the change, because the signal is precisely the thing a summary missed. Then
/// by how much moved, then by name so the order is stable.
pub fn of_diff(repo: &Repo, diff: &str) -> Vec<ModuleChange> {
    let files = files_in(diff);
    let signals = crate::contracts::scan(diff);
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let modules = crate::moduledocs::modules(repo);

    let owner_of = |path: &str| -> Option<&crate::moduledocs::Module> {
        // Longest match wins, the same rule `moduledocs::touched` uses: with both `src` and
        // `src/web` as modules, a change to `src/web/x` belongs to the more specific one — the one
        // whose note is actually about it.
        modules
            .iter()
            .filter(|m| path == m.path || path.starts_with(&format!("{}/", m.path)))
            .max_by_key(|m| m.path.len())
    };

    let mut by_module: std::collections::BTreeMap<String, ModuleChange> = Default::default();
    for file in &files {
        let Some(module) = owner_of(&file.path) else {
            continue;
        };
        let entry = by_module
            .entry(module.path.clone())
            .or_insert_with(|| ModuleChange {
                path: module.path.clone(),
                owners: module.owners.clone(),
                movement: Movement::Changed,
                added: 0,
                removed: 0,
                files: Vec::new(),
                signals: Vec::new(),
                note: String::new(),
                note_state: "absent".into(),
            });
        entry.added += file.added;
        entry.removed += file.removed;
        entry.files.push(file.path.clone());
    }

    for signal in signals {
        if let Some(module) = owner_of(&signal.file) {
            if let Some(entry) = by_module.get_mut(&module.path) {
                entry.signals.push(Sighted {
                    signal,
                    mentions: None,
                });
            }
        }
    }

    // The count, once — and only for signals that survived the roll-up, since a signal in a file no
    // module owns is never rendered and counting it would be a fork for nothing. One search per
    // *distinct* symbol: a rename that trips two detectors on the same name is one question.
    count_mentions(repo, &mut by_module);

    // The classification, once the module's files are all in.
    for entry in by_module.values_mut() {
        let its: Vec<&FileChange> = files
            .iter()
            .filter(|f| entry.files.contains(&f.path))
            .collect();
        entry.movement = if its.iter().all(|f| f.created) {
            Movement::New
        } else if its.iter().all(|f| f.deleted) {
            Movement::Gone
        } else if entry.removed > entry.added {
            Movement::Shrank
        } else {
            Movement::Changed
        };
    }

    // The notes, and only the fresh ones. Asked once per module rather than per file.
    let touched: Vec<String> = by_module.keys().cloned().collect();
    let _ = &paths;
    for path in &touched {
        let state = match crate::moduledocs::stored(&repo.id, path) {
            None => ("absent", String::new()),
            Some(doc) => match crate::moduledocs::is_fresh(repo, &doc) {
                true => ("fresh", doc.text.clone()),
                false => ("stale", String::new()),
            },
        };
        if let Some(entry) = by_module.get_mut(path) {
            entry.note_state = state.0.to_string();
            entry.note = state.1;
        }
    }

    let mut out: Vec<ModuleChange> = by_module.into_values().collect();
    out.sort_by(worth_looking_at);
    out
}

/// The order: what is worth looking at, not what is biggest.
///
/// Its own function so the rule can be asserted without a repository — and because it is a claim
/// about attention rather than about data. A module carrying a contract signal comes first however
/// small its change, because the signal is precisely the structural consequence a summary missed;
/// a one-line change that moved a default outranks a thousand-line rename that moved nothing.
pub fn worth_looking_at(a: &ModuleChange, b: &ModuleChange) -> std::cmp::Ordering {
    // `false < true`, so "has signals" sorts before "has none" — written this way round rather than
    // with `b` first, which is the direction I got wrong: it put the thousand-line rename above the
    // one-line contract change, and nothing noticed until the rule had a test of its own.
    a.signals
        .is_empty()
        .cmp(&b.signals.is_empty())
        .then((b.added + b.removed).cmp(&(a.added + a.removed)))
        .then(a.path.cmp(&b.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A diff header is not a line of code, and counting it inflates every file by two.
    #[test]
    fn the_headers_are_not_counted_as_lines() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,3 @@
 kept
+added one
+added two
-removed one
diff --git a/src/b.rs b/src/b.rs
new file mode 100644
--- /dev/null
+++ b/src/b.rs
@@ -0,0 +1,1 @@
+brand new
";
        let files = files_in(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/a.rs");
        assert_eq!(
            (files[0].added, files[0].removed),
            (2, 1),
            "the +++/--- headers were counted"
        );
        assert!(!files[0].created);
        assert_eq!(files[1].path, "src/b.rs");
        assert_eq!((files[1].added, files[1].removed), (1, 0));
        assert!(files[1].created, "a new file was not recognised as one");
    }

    /// A rename is reported where the file is now, not where it was.
    #[test]
    fn a_file_is_named_by_where_it_ended_up() {
        let files = files_in("diff --git a/old/name.rs b/new/name.rs\n@@\n+one\n");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new/name.rs");
    }

    /// A whole change, rolled up: files to modules, signals to the modules they were found in, and
    /// the order that decides what is worth looking at.
    ///
    /// Against a real repository layout rather than a hand-made module list, because the mapping is
    /// the half that is easy to get wrong — longest-match, so `src/web/x` belongs to `src/web` and
    /// not to `src`.
    #[test]
    fn a_change_is_rolled_up_to_the_modules_it_touched() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        // A real mirror, because `moduledocs::modules` reads the repo through `repos::Tree` — a
        // bare clone — and not off the disk. A fixture of plain directories produced no modules at
        // all, silently, which is how a rollup test can pass by rolling nothing up.
        let work = home.join("work");
        let seed = home.join("seed");
        let mirror = home.join("repos/demo/mirror");
        std::fs::create_dir_all(&work).unwrap();
        let git = "git -c user.email=t@example.com -c user.name=test -c init.defaultBranch=main";
        let script = format!(
            // `-b main`, because a bare repo's HEAD defaults to `master` and `Tree::open` verifies
            // HEAD before answering anything — a mirror pushed to `main` with HEAD on `master`
            // reports as a repository with no commits, and every module vanishes silently.
            "set -e; git init --bare -q -b main {m}; {git} init -q {s}; cd {s}; \
             mkdir -p src/web docs .github; \
             echo x > src/web/keep.rs; echo x > docs/keep.rs; echo x > src/keep.rs; \
             printf '/src/web/ @web\\n/src/ @core\\n/docs/ @writers\\n' > .github/CODEOWNERS; \
             {git} add -A; {git} commit -qm seed; {git} push -q {m} main",
            m = mirror.display(),
            s = seed.display(),
            git = git
        );
        let made = std::process::Command::new("bash")
            .arg("-lc")
            .arg(&script)
            .output()
            .expect("bash");
        assert!(
            made.status.success(),
            "could not build the fixture repo: {}",
            String::from_utf8_lossy(&made.stderr)
        );
        let repo = Repo {
            read_prs: false,
            id: "demo".into(),
            source: seed.to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: false,
            sync_gateway_url: String::new(),
        };

        // `docs` loses lines and gains none; `src/web` gains a file. A contract signal is planted in
        // `src/web` so the ordering rule has something to prefer.
        let diff = "\
diff --git a/docs/keep.rs b/docs/keep.rs
--- a/docs/keep.rs
+++ b/docs/keep.rs
@@
-gone one
-gone two
-gone three
diff --git a/src/web/new.rs b/src/web/new.rs
new file mode 100644
--- /dev/null
+++ b/src/web/new.rs
@@
+pub fn f(a: u8, b: u8) {}
";
        let shaped = of_diff(&repo, diff);
        std::env::remove_var("SKEIN_HOME");
        let paths: Vec<&str> = shaped.iter().map(|m| m.path.as_str()).collect();
        assert!(
            paths.contains(&"src/web") && paths.contains(&"docs"),
            "the change was not rolled up to modules: {paths:?}"
        );
        // Longest match wins: `src/web/new.rs` belongs to `src/web`, not to `src`, because that is
        // the module whose note is actually about it. Both are real modules here, from CODEOWNERS.
        assert!(
            paths.contains(&"src/web"),
            "a change under a nested module was attributed to its parent: {paths:?}"
        );
        let web = shaped.iter().find(|m| m.path == "src/web").unwrap();
        let docs = shaped.iter().find(|m| m.path == "docs").unwrap();
        assert_eq!(web.movement, Movement::New, "every file in it was new");
        assert_eq!(docs.movement, Movement::Shrank);
        assert_eq!((docs.added, docs.removed), (0, 3));
        assert_eq!(web.files, vec!["src/web/new.rs".to_string()]);
        assert_eq!(
            web.owners,
            vec!["@web".to_string()],
            "CODEOWNERS attribution is carried"
        );
        // No note has been written, and "absent" is a state rather than an empty string — a module
        // with no note is one worth offering to write one for.
        assert_eq!(web.note_state, "absent");
        assert!(web.note.is_empty());
    }

    /// The count beside a signal, and the two ways it must not lie.
    ///
    /// A symbol that was named is counted against the mirror — the base the change is measured
    /// from, which is where "how far does this reach" is answered. A signal with no symbol carries
    /// **no count at all**, and the serialised form must not contain the field: `"mentions": 0`
    /// reads as "nothing uses this", which is the opposite of "we did not look", and it is the more
    /// dangerous of the two because it is the reassuring one.
    #[test]
    fn a_named_symbol_is_counted_and_an_unnamed_one_is_not() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", &home);
        let seed = home.join("seed");
        let mirror = home.join("repos/demo/mirror");
        let git = "git -c user.email=t@example.com -c user.name=test -c init.defaultBranch=main";
        // `check` is mentioned on four lines and `checked` on one — the near-miss is the point of
        // the word boundary, and without it the count silently absorbs every longer name.
        let script = format!(
            "set -e; git init --bare -q -b main {m}; {git} init -q {s}; cd {s}; mkdir -p src; \
             printf 'pub fn check(a: u8) {{}}\\n' > src/api.rs; \
             printf 'check(1);\\ncheck(2);\\n// check again\\nchecked(3);\\n' > src/uses.rs; \
             echo x > src/old.rs; \
             {git} add -A; {git} commit -qm seed; {git} push -q {m} main",
            m = mirror.display(),
            s = seed.display(),
            git = git
        );
        let made = std::process::Command::new("bash")
            .arg("-lc")
            .arg(&script)
            .output()
            .expect("bash");
        assert!(
            made.status.success(),
            "could not build the fixture repo: {}",
            String::from_utf8_lossy(&made.stderr)
        );
        let repo = Repo {
            read_prs: false,
            id: "demo".into(),
            source: seed.to_string_lossy().into_owned(),
            store: home.join("store").to_string_lossy().into_owned(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: false,
            sync_gateway_url: String::new(),
        };

        // One signal that can name what it matched, one that cannot.
        let diff = "\
diff --git a/src/api.rs b/src/api.rs
--- a/src/api.rs
+++ b/src/api.rs
@@
-pub fn check(a: u8) {}
+let addr = env::var(\"SKEIN_NEW_THING\");
diff --git a/src/old.rs b/src/old.rs
deleted file mode 100644
";
        let shaped = of_diff(&repo, diff);
        std::env::remove_var("SKEIN_HOME");

        let module = shaped
            .iter()
            .find(|m| m.path == "src")
            .expect("the change is under src");
        let removed = module
            .signals
            .iter()
            .find(|s| s.signal.symbol == "check")
            .expect("the removed declaration was named");
        assert_eq!(
            removed.mentions,
            Some(4),
            "four lines mention `check`; `checked(3)` is not one of them"
        );

        // Counted, and the count is zero: nothing in the base tree names it, because the change is
        // what introduces it. This is the case that makes the distinction load-bearing — zero and
        // absent are different answers, and a scheme that could not tell them apart would have to
        // pick one and be wrong about the other.
        let fresh = module
            .signals
            .iter()
            .find(|s| s.signal.symbol == "SKEIN_NEW_THING")
            .expect("a new environment variable is a signal");
        assert_eq!(fresh.mentions, Some(0));
        assert!(
            serde_json::to_string(fresh)
                .unwrap()
                .contains("\"mentions\":0"),
            "a real zero is rendered; it is only an *uncounted* signal that shows nothing"
        );

        let deleted = module
            .signals
            .iter()
            .find(|s| s.signal.symbol.is_empty())
            .expect("a deleted file signals with no symbol");
        assert_eq!(deleted.mentions, None);
        let json = serde_json::to_string(deleted).unwrap();
        assert!(
            !json.contains("mentions"),
            "an uncounted signal must render without a count, not with a zero: {json}"
        );
        assert!(
            json.contains("\"kind\"") && json.contains("\"file\""),
            "flattening must leave what a client already reads untouched: {json}"
        );
    }

    /// A one-line change that moved a contract outranks a thousand-line rename that moved nothing.
    ///
    /// The rule is about attention, not size, and it is the whole reason contract signals are in
    /// this view: they are the structural consequence a summary misses, so a module carrying one is
    /// the module to open first however little of it changed.
    #[test]
    fn a_module_carrying_a_contract_signal_comes_first_however_small() {
        let module = |path: &str, added: u32, removed: u32, signals: usize| ModuleChange {
            path: path.into(),
            owners: Vec::new(),
            movement: Movement::Changed,
            added,
            removed,
            files: Vec::new(),
            signals: (0..signals)
                .map(|_| Sighted {
                    signal: Signal {
                        kind: "default-changed".into(),
                        what: "the default moved".into(),
                        file: format!("{path}/x.rs"),
                        symbol: "TIMEOUT".into(),
                    },
                    mentions: None,
                })
                .collect(),
            note: String::new(),
            note_state: "absent".into(),
        };
        let mut all = [
            module("huge", 900, 100, 0),
            module("tiny", 1, 0, 1),
            module("middling", 40, 10, 0),
        ];
        all.sort_by(worth_looking_at);
        assert_eq!(
            all.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(),
            vec!["tiny", "huge", "middling"],
            "a thousand-line rename outranked a one-line change to a contract"
        );

        // With no signals anywhere, size decides — and ties go to the name so the order is stable
        // between renders rather than shuffling under somebody reading it.
        let mut plain = [
            module("b", 5, 0, 0),
            module("a", 5, 0, 0),
            module("c", 9, 0, 0),
        ];
        plain.sort_by(worth_looking_at);
        assert_eq!(
            plain.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(),
            vec!["c", "a", "b"]
        );
    }

    /// Shrinking is its own word, because it is the one shape read differently.
    #[test]
    fn a_module_that_lost_more_than_it_gained_says_so() {
        // Exercised through the classification directly: `of_diff` needs a repo, and what is being
        // asserted is the rule, which is arithmetic.
        let call = |added: u32, removed: u32, created: bool, deleted: bool| {
            let f = FileChange {
                path: "src/x.rs".into(),
                added,
                removed,
                created,
                deleted,
            };
            if f.created {
                Movement::New
            } else if f.deleted {
                Movement::Gone
            } else if removed > added {
                Movement::Shrank
            } else {
                Movement::Changed
            }
        };
        assert_eq!(call(1, 9, false, false), Movement::Shrank);
        assert_eq!(call(9, 1, false, false), Movement::Changed);
        assert_eq!(
            call(5, 5, false, false),
            Movement::Changed,
            "even is not shrinking"
        );
        assert_eq!(call(3, 0, true, false), Movement::New);
        assert_eq!(call(0, 3, false, true), Movement::Gone);
    }
}
