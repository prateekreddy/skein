//! The parity gate's own numbers, checked against the code they are about.
//!
//! `docs/parity.md` is the acceptance gate for the rewrite, and its header explains why it is
//! written the way it is: two earlier drafts contained capabilities that **do not exist**, and "a
//! list with fabricated entries cannot be a gate, because the absence of an item stops carrying
//! information". Its answer was to state the commands that reproduce its counts.
//!
//! They stopped reproducing. Between the audit that wrote them and the one that re-ran them, the
//! route count had gone 67 → 81 and every numbered line citation in the document had drifted. That
//! is not a documentation tidy-up: a reader who checks two claims and finds both wrong stops
//! checking the third, and from then on a capability that quietly disappeared reads exactly like one
//! that moved — which is the one thing this document exists to make impossible.
//!
//! So the numbers are checked like the claims about the code that they are. The counts are
//! **parsed out of the document itself** rather than written here, because a copy in this file is a
//! second place to update and the drift would just move.
//!
//! **This test failing is not a bug.** Adding a route or a function is normal; the fix is one line
//! in `docs/parity.md`, and the failure says which line and what to put in it. What it stops is the
//! quiet version, where the number stays and stops meaning anything.

use std::path::Path;

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The number in the `# NN` comment on the line of the parity block that starts with `starts_with`.
///
/// Deliberately anchored on the command rather than on a line number: this file would otherwise
/// have the same problem it exists to fix.
fn stated(parity: &str, starts_with: &str) -> u64 {
    let line = parity
        .lines()
        .find(|l| l.trim_start().starts_with(starts_with))
        .unwrap_or_else(|| {
            panic!(
                "docs/parity.md no longer runs `{starts_with}…`. If the command changed, change it \
                 here too; if the claim is gone, §7 is where a removal goes — never by deletion."
            )
        });
    let after = line
        .split_once('#')
        .unwrap_or_else(|| panic!("no `# <count>` on: {line}"))
        .1;
    after
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the count after `#` is not a number: {line}"))
}

fn check(what: &str, stated: u64, measured: u64, command: &str) {
    assert_eq!(
        stated, measured,
        "docs/parity.md says there are {stated} {what} and there are {measured}.\n\
         \n\
         This is the gate's own reproduction command, and it no longer reproduces. Run it —\n\
         \n    {command}\n\n\
         — and put the answer in the `# …` comment beside it. If the change also removed a \
         capability, §7 is where that goes with its reason: the document's rule is that an item \
         leaves the list only by moving there, never by being forgotten."
    );
}

#[test]
fn the_parity_gate_still_reproduces_its_own_counts() {
    let parity = read("docs/parity.md");
    let server = read("src/bin/skein-server.rs");
    let index = read("src/web/index.html");

    // `.route(` and not `.route("`, which the document also records — the second misses the routes
    // whose path is a constant, and the difference between the two numbers is itself the note.
    check(
        "routes",
        stated(&parity, "grep -c '\\.route('"),
        server.matches(".route(").count() as u64,
        "grep -c '\\.route('  src/bin/skein-server.rs",
    );

    // Unique element ids, and the occurrences beside them: two ids written twice is a bug the page
    // cannot report on itself, so the gap between the two numbers is load-bearing.
    let ids: Vec<&str> = index
        .match_indices("id=\"")
        .filter_map(|(at, _)| {
            let rest = &index[at + 4..];
            let end = rest.find('"')?;
            let name = &rest[..end];
            let ok = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            ok.then_some(name)
        })
        .collect();
    let mut unique: Vec<&str> = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    let line = parity
        .lines()
        .find(|l| l.trim_start().starts_with("grep -oE 'id="))
        .expect("docs/parity.md no longer counts the page's element ids");
    let numbers: Vec<u64> = line
        .split_once('#')
        .expect("no counts on the id line")
        .1
        .split_whitespace()
        .filter_map(|w| w.parse().ok())
        .collect();
    assert_eq!(
        numbers.len(),
        2,
        "the id line should state both the unique count and the occurrences: {line}"
    );
    check(
        "unique element ids",
        numbers[0],
        unique.len() as u64,
        "grep -oE 'id=\"[a-zA-Z0-9_-]+\"' src/web/index.html | sort -u | wc -l",
    );
    check(
        "element id occurrences",
        numbers[1],
        ids.len() as u64,
        "grep -oE 'id=\"[a-zA-Z0-9_-]+\"' src/web/index.html | wc -l",
    );

    check(
        "JavaScript functions in the page",
        stated(&parity, "grep -c 'function '"),
        // `grep -c` counts LINES containing the string, not occurrences — two on one line is one.
        // Matched here rather than approximated, because a count that is nearly the grep's is worse
        // than one that is obviously something else.
        index.lines().filter(|l| l.contains("function ")).count() as u64,
        "grep -c 'function ' src/web/index.html",
    );
}

/// The line range the gate points at for the CLI still holds the dispatch.
///
/// `sed -n '25,90p'` was the citation and the dispatch had moved out from under it. A range is the
/// one kind of citation that cannot be made grep-able, so it gets a check instead: the window has to
/// contain the `match` and the verbs, or it is pointing at nothing.
#[test]
fn the_cli_line_range_the_gate_cites_still_holds_the_dispatch() {
    let parity = read("docs/parity.md");
    let cited = parity
        .lines()
        .find(|l| l.trim_start().starts_with("sed -n '") && l.contains("src/bin/skein.rs"))
        .expect("docs/parity.md no longer cites the CLI dispatch");
    let range = cited
        .split_once('\'')
        .and_then(|(_, rest)| rest.split_once('\''))
        .map(|(range, _)| range.to_string())
        .unwrap_or_else(|| panic!("no 'from,to' in: {cited}"));
    let (from, to) = range
        .split_once(',')
        .map(|(a, b)| {
            (
                a.parse::<usize>().expect("start line"),
                b.trim_end_matches('p').parse::<usize>().expect("end line"),
            )
        })
        .unwrap_or_else(|| panic!("not a line range: {range}"));

    let cli = read("src/bin/skein.rs");
    let window: String = cli
        .lines()
        .skip(from.saturating_sub(1))
        .take(to.saturating_sub(from) + 1)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        window.contains("match cmd"),
        "docs/parity.md cites src/bin/skein.rs:{from},{to} for the CLI's subcommands and the \
         dispatch is not in that window any more. Find `match cmd` and update the range."
    );
    // And it reaches the verbs, not just the opening line. `attach` is the last arm that matters and
    // has been for a while; a window that stops before it shows half the surface as if it were all.
    for verb in ["\"doctor\"", "\"start\"", "\"attach\""] {
        assert!(
            window.contains(verb),
            "the cited window src/bin/skein.rs:{from},{to} does not reach {verb}, so the gate's \
             own citation shows less of the CLI than it claims to"
        );
    }
}
