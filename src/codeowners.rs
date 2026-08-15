//! CODEOWNERS: which parts of a change are *yours*.
//!
//! This exists to set the **depth** of a review summary, never its **membership**. Whether a PR
//! reaches you is GitHub's answer (requested reviewer, author, mentioned); this file only decides
//! how much of it to explain in detail. That split is deliberate and load-bearing: a bug in a glob
//! here can make a summary shallower than it should be, and must never make a PR disappear.
//!
//! Every function degrades to "no narrowing" rather than to "nothing matched". A repo with no
//! CODEOWNERS is the normal case, not an error — you get full-depth summaries for everything, which
//! is more attention, not less. That direction is chosen everywhere in this module.
//!
//! Syntax is gitignore-style, per GitHub, with the two exclusions GitHub itself names: no `!`
//! negation and no `[...]` character ranges. The rule that catches people out is that the **last**
//! matching line wins, not the most specific one — so the file is scanned in order and the final
//! match kept.

use std::fs;
use std::path::Path;

/// One parsed CODEOWNERS line: a path pattern and the owners it assigns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub pattern: String,
    pub owners: Vec<String>,
}

/// A repo's parsed CODEOWNERS, in file order.
///
/// Order is preserved rather than indexed because precedence is positional: resolving an owner
/// means walking every rule and keeping the last that matched. An index keyed by pattern would be
/// faster and would quietly get precedence wrong.
#[derive(Debug, Clone, Default)]
pub struct CodeOwners {
    pub rules: Vec<Rule>,
    /// Where it was found, relative to the repo root — for telling you *which* file is in play when
    /// a repo has more than one (GitHub picks one; showing it beats guessing).
    pub source: String,
}

/// The three locations GitHub honours, in the order it checks them.
const LOCATIONS: [&str; 3] = [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"];

/// Load a repo's CODEOWNERS from a working clone, or `None` when it has none.
///
/// `None` is a first-class, expected answer — see the module docs. Callers must treat it as "do not
/// narrow", never as "you own nothing".
pub fn load(work: &Path) -> Option<CodeOwners> {
    for rel in LOCATIONS {
        let path = work.join(rel);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut co = parse(&text);
        co.source = rel.to_string();
        // A file that exists but parses to nothing (all comments, or entirely malformed) is treated
        // as absent. Returning an empty rule set would be indistinguishable from "nobody owns
        // anything", and would silently strip depth from every summary in the repo.
        if co.rules.is_empty() {
            return None;
        }
        return Some(co);
    }
    None
}

/// Parse CODEOWNERS text. Unowned patterns are kept out: a line with a pattern and no owners is
/// legal in gitignore and meaningless here.
pub fn parse(text: &str) -> CodeOwners {
    let mut rules = Vec::new();
    for line in text.lines() {
        let line = match line.split_once('#') {
            Some((before, _)) => before,
            None => line,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(pattern) = parts.next() else {
            continue;
        };
        let owners: Vec<String> = parts.map(str::to_string).collect();
        if owners.is_empty() {
            continue;
        }
        rules.push(Rule {
            pattern: pattern.to_string(),
            owners,
        });
    }
    CodeOwners {
        rules,
        source: String::new(),
    }
}

impl CodeOwners {
    /// The owners of one path, or empty when no rule matches.
    ///
    /// Last match wins — GitHub's rule, and the one worth stating twice because it inverts the
    /// intuition that the most specific pattern should win.
    pub fn owners_of(&self, path: &str) -> &[String] {
        let mut found: &[String] = &[];
        for rule in &self.rules {
            if matches(&rule.pattern, path) {
                found = &rule.owners;
            }
        }
        found
    }

    /// Is any of `identities` an owner of this path?
    ///
    /// Identities are compared case-insensitively because GitHub preserves the case a handle was
    /// typed in but resolves without it — the same reason [`crate::gitgate`] compares repo slugs
    /// that way. A CODEOWNERS saying `@Acme/core` and a token saying `acme/core` are one
    /// team, and matching exactly would silently drop the depth from every PR in the module.
    pub fn is_owned_by(&self, path: &str, identities: &[String]) -> bool {
        let owners = self.owners_of(path);
        owners.iter().any(|o| {
            identities.iter().any(|id| {
                id.eq_ignore_ascii_case(o.trim_start_matches('@')) || id.eq_ignore_ascii_case(o)
            })
        })
    }

    /// Split changed paths into (yours, everyone else's), preserving input order.
    ///
    /// The pair is what a summary needs: the first list gets explained at mechanism level, the
    /// second gets a line. Returning both — rather than filtering — is what lets a summary say "and
    /// it also touches 9 files you don't own" instead of pretending they aren't there.
    pub fn partition<'a>(
        &self,
        paths: &'a [String],
        identities: &[String],
    ) -> (Vec<&'a str>, Vec<&'a str>) {
        let mut mine = Vec::new();
        let mut theirs = Vec::new();
        for p in paths {
            if self.is_owned_by(p, identities) {
                mine.push(p.as_str());
            } else {
                theirs.push(p.as_str());
            }
        }
        (mine, theirs)
    }

    /// Every distinct team (`@org/team`) named anywhere in the file.
    ///
    /// Used to ask GitHub for team-requested reviews, which `review-requested:@me` does not return.
    /// A PR whose review was requested from a team you belong to is invisible to the personal query,
    /// and invisible is the one failure mode that actually costs something here.
    pub fn teams(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for rule in &self.rules {
            for owner in &rule.owners {
                let handle = owner.trim_start_matches('@');
                if handle.contains('/') && !out.iter().any(|t| t.eq_ignore_ascii_case(handle)) {
                    out.push(handle.to_string());
                }
            }
        }
        out
    }
}

/// Does a gitignore-style CODEOWNERS pattern match this repo-relative path?
///
/// Anchoring is the subtle part. A pattern containing a `/` anywhere but its end is anchored to the
/// repo root (`src/*.rs` means only top-level `src`); one without is matched at any depth (`*.rs`
/// means any Rust file anywhere). A pattern that consumes all its segments against a *prefix* of the
/// path matches, which is how `docs/` and `docs` both come to own everything beneath them.
pub fn matches(pattern: &str, path: &str) -> bool {
    let path = path.trim_start_matches('/');
    let anchored = pattern.starts_with('/') || pattern.trim_end_matches('/').contains('/');
    let pat = pattern.trim_start_matches('/').trim_end_matches('/');
    if pat.is_empty() {
        return false;
    }
    let pat_segs: Vec<&str> = pat.split('/').collect();
    let path_segs: Vec<&str> = path.split('/').collect();
    if anchored {
        return match_segs(&pat_segs, &path_segs);
    }
    // Unanchored: allowed to start at any depth. `*.rs` matches `src/a/b.rs`.
    (0..path_segs.len()).any(|i| match_segs(&pat_segs, &path_segs[i..]))
}

/// Match pattern segments against a prefix of path segments, with `**` spanning any number.
fn match_segs(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        // All pattern segments consumed: this is a prefix match, so a directory pattern owns
        // everything under it. Requiring `path.is_empty()` here would make `docs/` own the
        // directory and none of its contents, which is never what anyone writes it to mean.
        None => true,
        Some((&"**", rest)) => {
            if rest.is_empty() {
                return true;
            }
            (0..=path.len()).any(|i| match_segs(rest, &path[i..]))
        }
        Some((seg, rest)) => match path.split_first() {
            None => false,
            Some((head, tail)) => match_seg(seg, head) && match_segs(rest, tail),
        },
    }
}

/// Glob one path segment: `*` matches any run of non-`/` characters, `?` matches one.
///
/// Iterative with a backtrack point rather than recursive: a pathological pattern of many `*`s
/// against a long filename is exponential recursively, and CODEOWNERS is user-supplied text.
fn match_seg(pat: &str, seg: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), seg.chars().collect());
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut resume) = (usize::MAX, 0usize);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            resume = si;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            resume += 1;
            si = resume;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn co(text: &str) -> CodeOwners {
        parse(text)
    }

    #[test]
    fn parses_owners_and_skips_comments() {
        let c = co("# top\n\nsrc/ @alice @org/core\n*.md @bob # trailing\n");
        assert_eq!(c.rules.len(), 2);
        assert_eq!(c.rules[0].pattern, "src/");
        assert_eq!(c.rules[0].owners, vec!["@alice", "@org/core"]);
        assert_eq!(c.rules[1].owners, vec!["@bob"]);
    }

    #[test]
    fn a_pattern_with_no_owner_is_not_a_rule() {
        assert!(co("src/\n").rules.is_empty());
    }

    #[test]
    fn last_match_wins_not_the_most_specific() {
        let c = co("src/gitgate.rs @alice\nsrc/ @bob\n");
        assert_eq!(c.owners_of("src/gitgate.rs"), ["@bob"]);
    }

    #[test]
    fn directory_patterns_own_their_contents() {
        assert!(matches("docs/", "docs/a/b.md"));
        assert!(matches("docs", "docs/a/b.md"));
        assert!(!matches("docs/", "src/a.rs"));
    }

    #[test]
    fn unanchored_patterns_match_at_any_depth() {
        assert!(matches("*.rs", "src/deep/gitgate.rs"));
        assert!(matches("*.rs", "main.rs"));
        assert!(!matches("*.rs", "src/index.html"));
    }

    #[test]
    fn a_slash_anchors_to_the_repo_root() {
        assert!(matches("src/*.rs", "src/main.rs"));
        assert!(!matches("src/*.rs", "vendor/src/main.rs"));
        assert!(matches("/src/", "src/a.rs"));
    }

    #[test]
    fn double_star_spans_directories() {
        assert!(matches("src/**/test.rs", "src/a/b/test.rs"));
        assert!(matches("src/**/test.rs", "src/test.rs"));
        assert!(matches("src/**", "src/a/b.rs"));
        assert!(!matches("src/**/test.rs", "src/a/b/other.rs"));
    }

    #[test]
    fn ownership_ignores_case_and_the_at_sign() {
        let c = co("src/ @Acme/Core\n");
        assert!(c.is_owned_by("src/a.rs", &["acme/core".into()]));
        assert!(c.is_owned_by("src/a.rs", &["@ACME/CORE".into()]));
        assert!(!c.is_owned_by("src/a.rs", &["someone-else".into()]));
    }

    #[test]
    fn partition_keeps_both_sides() {
        let c = co("src/ @me\n");
        let paths = vec!["src/a.rs".to_string(), "web/b.js".to_string()];
        let (mine, theirs) = c.partition(&paths, &["me".into()]);
        assert_eq!(mine, ["src/a.rs"]);
        assert_eq!(theirs, ["web/b.js"]);
    }

    #[test]
    fn teams_are_the_owners_with_a_slash() {
        let c = co("a/ @alice @org/core\nb/ @org/core @org/ui\n");
        assert_eq!(c.teams(), ["org/core", "org/ui"]);
    }

    #[test]
    fn an_unmatched_path_has_no_owners_rather_than_all_of_them() {
        let c = co("src/ @me\n");
        assert!(c.owners_of("README.md").is_empty());
    }

    #[test]
    fn many_stars_do_not_blow_up() {
        // Exponential under a naive recursive matcher; must return promptly.
        assert!(!match_seg(
            "*a*a*a*a*a*a*a*b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaac"
        ));
    }
}
