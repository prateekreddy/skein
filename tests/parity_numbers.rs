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

/// Every number in the `# …` comment on the line of the parity block that starts with
/// `starts_with`, in the order it is written.
///
/// Deliberately anchored on the command rather than on a line number: this file would otherwise
/// have the same problem it exists to fix.
///
/// **Runs of digits, not words that parse as numbers** (SKEIN-986). This read the first
/// whitespace-separated word and stopped, so a count written inside a parenthetical was invisible
/// to it: the route line's `that gives 84)` had no reader at all, and by the time anyone ran the
/// command it named the answer was 86. A number in this block that nothing reproduces is the one
/// thing the block exists to rule out, and it was sitting on the first line of it.
fn counts_on(parity: &str, starts_with: &str) -> Vec<u64> {
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
    let counts: Vec<u64> = after
        .split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .filter_map(|run| run.parse().ok())
        .collect();
    assert!(!counts.is_empty(), "no count in the `#` comment on: {line}");
    counts
}

/// The one count on a line that states one.
fn stated(parity: &str, starts_with: &str) -> u64 {
    let counts = counts_on(parity, starts_with);
    assert_eq!(
        counts.len(),
        1,
        "`{starts_with}…` states {} numbers, and this reads it as stating one. Every number in \
         that block is a claim about the code and has to be checked as one — add the check rather \
         than letting the extra number ride unread, which is how `that gives 84` outlived the \
         answer 86 (SKEIN-986).",
        counts.len()
    );
    counts[0]
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
    let server = read("src/bin/skein-server/main.rs");
    let index = read("src/web/index.html");

    // `.route(` and not `.route("`, which the document also records — the second misses every
    // entry the router writes across several lines, whose path is on the line BELOW the call, and
    // the difference between the two numbers is itself the note. Both are checked: the
    // parenthetical is a claim about the code exactly like the count beside it, and it was the one
    // number in this block that nothing ran (SKEIN-986). The measurement matches what `grep -c`
    // does — LINES containing the string, not occurrences of it.
    let routes = counts_on(&parity, "grep -c '\\.route('");
    assert_eq!(
        routes.len(),
        2,
        "the route line should state the count and the under-count the quoted form gives: it says \
         {routes:?}"
    );
    check(
        "routes",
        routes[0],
        server.matches(".route(").count() as u64,
        "grep -c '\\.route('  src/bin/skein-server/main.rs",
    );
    check(
        "routes the quoted form finds",
        routes[1],
        server.lines().filter(|l| l.contains(".route(\"")).count() as u64,
        "grep -c '\\.route(\"'  src/bin/skein-server/main.rs",
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
    let numbers = counts_on(&parity, "grep -oE 'id=");
    assert_eq!(
        numbers.len(),
        2,
        "the id line should state both the unique count and the occurrences: it says {numbers:?}"
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

    // The health banner's checks — the claim that had no command at all (SKEIN-1004). It read
    // "seven checks" from the audit that wrote it while the page's list grew to twelve and then to
    // fourteen, and the gate cannot run what the line does not state, so it was the one number in
    // §5a that could only go stale quietly.
    //
    // The page keeps no list of its own any more (SKEIN-1186): the banner and the diagnostics pane
    // walk the report's `labels`, which is `CHECK_LABELS` in `src/health/report.rs`, one
    // `CheckLabel::new(` line per check.
    // `health::tests::every_check_has_one_label_and_every_surface_reads_it` is what holds that
    // table level with the checks the report actually carries.
    let report = read("src/health/report.rs");
    let checked = report
        .lines()
        .filter(|l| l.contains("CheckLabel::new("))
        .count() as u64;
    check(
        "checks on the health banner",
        stated(&parity, "grep -c 'CheckLabel::new('"),
        checked,
        "grep -c 'CheckLabel::new(' src/health/report.rs",
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
