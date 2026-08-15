//! Mechanical evidence that a change moved something's contract.
//!
//! [`crate::review`] asks a model whether a PR changes how something works. This asks the diff. The
//! two are not redundant: a model reasons about intent and can be talked out of a correct reading,
//! while a scanner cannot reason at all and cannot be talked out of anything. Together they cover
//! each other's failure, and the composition rule is what makes that safe:
//!
//! **A signal can only ever add scrutiny.** It escalates a PR the model called boring; it never
//! clears one the model flagged. That has a consequence worth stating plainly, because it is the
//! whole reason this file can ship with imperfect rules: a rule that misses costs nothing — the
//! model's judgement stands. A rule that fires spuriously costs one extra expansion. There is no
//! failure mode here that hides a PR from you, by construction.
//!
//! No regex dependency, for the same reason [`crate::gitgate`] hand-rolled base64: this is the
//! whole of what would be used from one, and hand-written scanners are more precise about the few
//! shapes that actually matter than a pile of patterns would be.
//!
//! The rules are deliberately few and sharp. A scanner that fires on every pull request tells you
//! nothing; the bar for adding one is that its absence would let a real contract change through
//! looking ordinary.

use serde::{Deserialize, Serialize};

/// One piece of evidence that something's contract moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    /// Which tripwire — one of [`crate::review`]'s flag vocabulary, so the UI has one set of words.
    pub kind: String,
    /// What moved, in a few words. Shown to you verbatim.
    pub what: String,
    /// The repo-relative file it was found in.
    pub file: String,
}

/// Everything the scanners found, capped and deduplicated.
///
/// The cap exists because a rename across two hundred files is one fact, not two hundred, and a
/// list that long stops being read at all — which would waste the one signal that was worth seeing.
const MAX_SIGNALS: usize = 12;

/// Scan a unified diff for contract movement.
pub fn scan(diff: &str) -> Vec<Signal> {
    let mut out: Vec<Signal> = Vec::new();
    let mut file = String::new();
    let mut removed: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();

    // Hunks are compared as a unit: "this default changed" is a claim about a removed line and an
    // added line sitting together, which is exactly what a hunk is.
    let flush =
        |file: &str, removed: &mut Vec<String>, added: &mut Vec<String>, out: &mut Vec<Signal>| {
            if !file.is_empty() && !is_test_path(file) {
                defaults(file, removed, added, out);
                for line in removed.iter().chain(added.iter()) {
                    touches_env(file, line, out);
                    touches_route(file, line, out);
                }
                for line in removed.iter() {
                    public_removed(file, line, out);
                }
            }
            removed.clear();
            added.clear();
        };

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(&file, &mut removed, &mut added, &mut out);
            file = path_from_git_header(rest);
        } else if line.starts_with("deleted file mode") {
            push(
                &mut out,
                "architecture",
                format!("{file} was deleted"),
                &file,
            );
        } else if let Some(to) = line.strip_prefix("rename to ") {
            push(
                &mut out,
                "architecture",
                format!("{file} was renamed to {}", to.trim()),
                &file,
            );
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            // A `+++ b/path` header names the file too, and some diffs have nothing else: `git
            // format-patch` output, a saved `.patch`, anything piped through a tool that drops the
            // `diff --git` line. Reading only one of the two spellings makes the scanner silently
            // find nothing rather than fail, which is the worst way for it to be wrong.
            flush(&file, &mut removed, &mut added, &mut out);
            let named = rest.trim().trim_start_matches("b/").trim();
            if !named.is_empty() && named != "/dev/null" {
                file = named.to_string();
            }
        } else if line.starts_with("@@") {
            flush(&file, &mut removed, &mut added, &mut out);
        } else if let Some(rest) = line.strip_prefix('-') {
            if !line.starts_with("---") {
                removed.push(rest.to_string());
            }
        } else if let Some(rest) = line.strip_prefix('+') {
            if !line.starts_with("+++") {
                added.push(rest.to_string());
            }
        }
    }
    flush(&file, &mut removed, &mut added, &mut out);
    out.truncate(MAX_SIGNALS);
    out
}

fn push(out: &mut Vec<Signal>, kind: &str, what: String, file: &str) {
    let signal = Signal {
        kind: kind.to_string(),
        what,
        file: file.to_string(),
    };
    if !out.contains(&signal) {
        out.push(signal);
    }
}

/// Is this a test, a fixture, or a suite — code whose contracts are with nobody?
///
/// Measured need, not caution: scanning this repo's own history, the browser suite's `$PATH`,
/// `$SKEIN_SHOT` and `$SKEIN_KEEP` were reported as interface changes. They are real environment
/// variables and they are nobody's interface, and three of those on a row is enough to stop anyone
/// reading the fourth line, which was the route that genuinely mattered.
fn is_test_path(file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    const DIRS: [&str; 6] = [
        "tests/",
        "test/",
        "spec/",
        "__tests__/",
        "fixtures/",
        "testdata/",
    ];
    if DIRS
        .iter()
        .any(|d| f.starts_with(d) || f.contains(&format!("/{d}")))
    {
        return true;
    }
    let base = f.rsplit('/').next().unwrap_or(&f);
    base.starts_with("test_")
        || base.starts_with("conftest.")
        || ["_test.", ".test.", "_spec.", ".spec."]
            .iter()
            .any(|m| base.contains(m))
}

/// `a/src/x.rs b/src/x.rs` → `src/x.rs`. The b-side, so a rename reports where it landed.
fn path_from_git_header(rest: &str) -> String {
    rest.split_whitespace()
        .next_back()
        .unwrap_or("")
        .trim_start_matches("b/")
        .to_string()
}

// ───────────────────────────── the scanners ─────────────────────────────

/// A default that moved: the same setting, a different value.
///
/// The strongest signal here and the most language-agnostic, because it needs no idiom at all —
/// only that a line's left-hand side survived while its right-hand side did not. That shape is a
/// changed default in every language anyone writes configuration in.
fn defaults(file: &str, removed: &[String], added: &[String], out: &mut Vec<Signal>) {
    for r in removed {
        let Some((key, old)) = split_assignment(r) else {
            continue;
        };
        for a in added {
            let Some((akey, new)) = split_assignment(a) else {
                continue;
            };
            if akey != key || new == old {
                continue;
            }
            // Only literals. A changed expression is ordinary work; a changed *value* is a decision
            // somebody made on your behalf, and that is the thing worth stopping for.
            if !is_literal(&old) || !is_literal(&new) {
                continue;
            }
            // A sentence is copy, not a default. Both are worth your attention and they are not the
            // same thing: calling reworded UI text a "changed default" sends you looking for a
            // setting that never moved. Measured on this repo — a settings-pane rewrite reported
            // its own prose as a default change.
            let kind = if is_prose(&old) && is_prose(&new) {
                "ux"
            } else {
                "default"
            };
            let what = if kind == "ux" {
                format!("wording of {key} changed")
            } else {
                format!("{key}: {old} → {new}")
            };
            push(out, kind, what, file);
        }
    }
}

/// Split `NAME = value` / `name: value` into its two halves, normalised.
fn split_assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("//") || line.starts_with('#') || line.starts_with('*') {
        return None;
    }
    // `=` wins over `:` when a line has both, because in most typed languages the `:` is an
    // annotation and the `=` is the value: `const TIMEOUT: u64 = 30` splits at the wrong place
    // otherwise, and every Rust and TypeScript constant in the repo becomes invisible to this.
    // `:` alone is the YAML/JSON shape, where it *is* the assignment.
    //
    // `==`, `!=`, `<=`, `>=` are comparisons, not assignments. Missing that reads every changed
    // condition in a diff as a changed default.
    let bytes = line.as_bytes();
    let eq = (0..bytes.len()).find(|&i| {
        bytes[i] == b'='
            && bytes.get(i + 1) != Some(&b'=')
            && (i == 0 || !b"=!<>+-*/%&|^".contains(&bytes[i - 1]))
    });
    let at = match eq {
        Some(i) => i,
        None => (0..bytes.len()).find(|&i| bytes[i] == b':' && bytes.get(i + 1) != Some(&b':'))?,
    };
    let key = line[..at].trim().trim_end_matches(':').trim().to_string();
    let value = line[at + 1..]
        .trim()
        .trim_end_matches([',', ';'])
        .trim()
        .to_string();
    if key.is_empty() || value.is_empty() || key.contains(' ') && key.split_whitespace().count() > 4
    {
        return None;
    }
    Some((key, value))
}

/// Is this a literal value — a number, a bool, a quoted string, a duration?
fn is_literal(v: &str) -> bool {
    let v = v.trim().trim_end_matches(&[',', ';'][..]).trim();
    if v.is_empty() || v.len() > 60 {
        return false;
    }
    if matches!(
        v,
        "true" | "false" | "True" | "False" | "null" | "None" | "nil"
    ) {
        return true;
    }
    if (v.starts_with('"') && v.ends_with('"') && v.len() > 1)
        || (v.starts_with('\'') && v.ends_with('\'') && v.len() > 1)
    {
        return true;
    }
    // A number, possibly with a unit or a separator: 30, 5_000, 1.5, 30s, 100ms.
    let head: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '_' || *c == '.')
        .collect();
    if head.is_empty() {
        return false;
    }
    let tail = &v[head.len()..];
    tail.is_empty() || tail.chars().all(|c| c.is_ascii_alphabetic()) && tail.len() <= 4
}

/// A quoted string long enough, and spaced enough, to be a sentence rather than a value.
fn is_prose(v: &str) -> bool {
    let inner = v.trim_matches(['"', '\'']);
    inner.len() > 24 && inner.contains(' ')
}

/// Idioms that mean "this line reads the environment".
const ENV_IDIOMS: [&str; 6] = [
    "env::var",
    "getenv",
    "process.env",
    "os.environ",
    "ENV[",
    "env.get",
];

/// An environment variable is a public interface: something outside this repo sets it.
fn touches_env(file: &str, line: &str, out: &mut Vec<Signal>) {
    if !ENV_IDIOMS.iter().any(|i| line.contains(i)) {
        return;
    }
    if let Some(name) = screaming_token(line) {
        push(
            out,
            "interface",
            format!("environment variable {name}"),
            file,
        );
    }
}

/// The first SCREAMING_SNAKE token of four or more characters.
fn screaming_token(line: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut cur = String::new();
    for c in line.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_uppercase() || c == '_' || (c.is_ascii_digit() && !cur.is_empty()) {
            cur.push(c);
        } else {
            if cur.len() >= 4 && cur.chars().any(|c| c.is_ascii_uppercase()) && best.is_none() {
                best = Some(cur.clone());
            }
            cur.clear();
        }
    }
    best
}

/// Words that mean the string beside them is an address other software depends on.
const ROUTE_IDIOMS: [&str; 6] = ["route", "router", "endpoint", "path=", "url(", "app."];

/// A URL path is a contract with every client that calls it.
fn touches_route(file: &str, line: &str, out: &mut Vec<Signal>) {
    let lower = line.to_ascii_lowercase();
    if !ROUTE_IDIOMS.iter().any(|i| lower.contains(i)) {
        return;
    }
    if let Some(p) = quoted_path(line) {
        push(out, "interface", format!("route {p}"), file);
    }
}

/// A quoted string that starts with `/` — a path, not prose.
fn quoted_path(line: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let mut parts = line.split(quote);
        parts.next();
        for (i, part) in parts.enumerate() {
            if i % 2 == 0 && part.starts_with('/') && part.len() > 1 && !part.contains(' ') {
                return Some(part.to_string());
            }
        }
    }
    None
}

/// Markers that a declaration was public — visible outside the file that declared it.
const PUBLIC_MARKERS: [&str; 4] = ["pub ", "export ", "public ", "@public"];

/// A public declaration that disappeared. Whatever called it no longer can.
///
/// Only *unindented* lines, and only explicitly-public ones: a removed private helper is ordinary
/// refactoring, and treating it as a contract change would fire this on every cleanup in the repo.
fn public_removed(file: &str, line: &str, out: &mut Vec<Signal>) {
    if line.starts_with(' ') || line.starts_with('\t') {
        return;
    }
    let trimmed = line.trim();
    if !PUBLIC_MARKERS.iter().any(|m| trimmed.starts_with(m)) {
        return;
    }
    let name = declaration_name(trimmed);
    if let Some(name) = name {
        push(
            out,
            "interface",
            format!("{name} was removed or renamed"),
            file,
        );
    }
}

/// The declared name in `pub fn foo(…)` / `export const bar =` / `public class Baz`.
fn declaration_name(decl: &str) -> Option<String> {
    let mut words = decl.split_whitespace();
    // Skip the visibility marker and any keywords before the name (fn, const, class, async, …).
    let mut last_keyword = false;
    for w in words.by_ref().skip(1) {
        let w = w.trim_start_matches(['*', '&']);
        if w.is_empty() {
            continue;
        }
        const KEYWORDS: [&str; 14] = [
            "fn",
            "const",
            "static",
            "struct",
            "enum",
            "trait",
            "type",
            "class",
            "function",
            "async",
            "let",
            "var",
            "def",
            "interface",
        ];
        if KEYWORDS.contains(&w) {
            last_keyword = true;
            continue;
        }
        if last_keyword {
            let name: String = w
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            return (!name.is_empty()).then_some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(diff: &str) -> Vec<(String, String)> {
        scan(diff).into_iter().map(|s| (s.kind, s.what)).collect()
    }

    #[test]
    fn a_changed_default_is_found_without_knowing_the_language() {
        let diff = "diff --git a/src/x.rs b/src/x.rs\n@@\n-const TIMEOUT: u64 = 30;\n+const TIMEOUT: u64 = 5;\n";
        let found = kinds(diff);
        assert!(
            found
                .iter()
                .any(|(k, w)| k == "default" && w.contains("30") && w.contains('5')),
            "{found:?}"
        );
    }

    #[test]
    fn a_yaml_default_is_the_same_shape() {
        let diff = "diff --git a/c.yml b/c.yml\n@@\n-retries: 3\n+retries: 10\n";
        assert!(kinds(diff).iter().any(|(k, _)| k == "default"));
    }

    /// The noise test. A changed comparison, a changed expression and a reworded comment are the
    /// three things that would otherwise fire this on every pull request in the repo.
    #[test]
    fn ordinary_edits_are_not_contract_changes() {
        let quiet = [
            "diff --git a/a.rs b/a.rs\n@@\n-if x == 1 {\n+if x == 2 {\n",
            "diff --git a/a.rs b/a.rs\n@@\n-let n = compute(a);\n+let n = compute(b);\n",
            "diff --git a/a.rs b/a.rs\n@@\n-// explains the thing\n+// explains the thing better\n",
            "diff --git a/a.rs b/a.rs\n@@\n-    fn helper() {\n+    fn helper2() {\n",
        ];
        for d in quiet {
            assert!(
                scan(d).is_empty(),
                "fired on ordinary work:\n{d}\n{:?}",
                scan(d)
            );
        }
    }

    #[test]
    fn an_environment_variable_is_an_interface() {
        let diff =
            "diff --git a/a.rs b/a.rs\n@@\n+    let v = env::var(\"SKEIN_NEW_THING\").ok();\n";
        let found = kinds(diff);
        assert!(
            found
                .iter()
                .any(|(k, w)| k == "interface" && w.contains("SKEIN_NEW_THING")),
            "{found:?}"
        );
    }

    #[test]
    fn a_route_is_an_interface() {
        let diff =
            "diff --git a/s.rs b/s.rs\n@@\n+        .route(\"/api/repos/:id/review\", get(h))\n";
        let found = kinds(diff);
        assert!(
            found
                .iter()
                .any(|(k, w)| k == "interface" && w.contains("/api/repos")),
            "{found:?}"
        );
    }

    #[test]
    fn a_removed_public_declaration_is_an_interface_but_a_private_one_is_not() {
        let public =
            "diff --git a/a.rs b/a.rs\n@@\n-pub fn ship_status(name: &str) -> ShipStatus {\n";
        let found = kinds(public);
        assert!(
            found
                .iter()
                .any(|(k, w)| k == "interface" && w.contains("ship_status")),
            "{found:?}"
        );

        let private = "diff --git a/a.rs b/a.rs\n@@\n-fn helper(name: &str) -> u8 {\n";
        assert!(
            scan(private).is_empty(),
            "a private helper is refactoring, not a contract"
        );
    }

    /// A patch with no `diff --git` line — `format-patch` output, a saved `.patch`. It must still
    /// find things, because finding nothing looks exactly like a clean PR.
    #[test]
    fn a_diff_that_names_files_only_in_its_headers_still_scans() {
        let diff = "--- a/src/parser.rs\n+++ b/src/parser.rs\n@@\n-const TIMEOUT: u64 = 30;\n+const TIMEOUT: u64 = 5;\n";
        let found = scan(diff);
        assert!(
            found
                .iter()
                .any(|s| s.kind == "default" && s.file == "src/parser.rs"),
            "{found:?}"
        );
    }

    #[test]
    fn a_deleted_file_is_architecture() {
        let diff = "diff --git a/src/old.rs b/src/old.rs\ndeleted file mode 100644\n";
        assert!(kinds(diff).iter().any(|(k, _)| k == "architecture"));
    }

    #[test]
    fn a_rename_says_where_it_went() {
        let diff = "diff --git a/src/a.rs b/src/b.rs\nrename from src/a.rs\nrename to src/b.rs\n";
        let found = kinds(diff);
        assert!(
            found.iter().any(|(_, w)| w.contains("src/b.rs")),
            "{found:?}"
        );
    }

    #[test]
    fn one_rename_across_many_files_does_not_bury_the_list() {
        let mut diff = String::new();
        for i in 0..50 {
            diff.push_str(&format!(
                "diff --git a/f{i}.rs b/f{i}.rs\ndeleted file mode 100644\n"
            ));
        }
        assert!(scan(&diff).len() <= MAX_SIGNALS);
    }

    #[test]
    fn signals_carry_the_file_they_were_found_in() {
        let diff = "diff --git a/src/x.rs b/src/x.rs\n@@\n-const A: u64 = 1;\n+const A: u64 = 2;\n";
        assert_eq!(scan(diff)[0].file, "src/x.rs");
    }

    #[test]
    fn declaration_names_survive_the_syntax_around_them() {
        assert_eq!(
            declaration_name("pub fn ship_status(n: &str)").as_deref(),
            Some("ship_status")
        );
        assert_eq!(
            declaration_name("export const revFilter = 1").as_deref(),
            Some("revFilter")
        );
        assert_eq!(
            declaration_name("pub struct Queue {").as_deref(),
            Some("Queue")
        );
        assert_eq!(
            declaration_name("export async function load()").as_deref(),
            Some("load")
        );
    }
}
