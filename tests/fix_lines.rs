//! Every command skein tells somebody to run is a command skein has.
//!
//! `skein doctor` and the cockpit's diagnostics panel answer a fault with one line: what to type.
//! For three faults that line was `` `skein restart <box>` ``, and `skein restart` was not a verb —
//! the CLI answered `unknown command "restart" (try: skein help)`. A fix line that does not run is
//! worse than none: it reads as skein being broken in a second, unrelated way, and the next line it
//! prints is not believed either.
//!
//! Checked against the source rather than by running anything. A verb is added to the dispatch in
//! `src/bin/skein.rs` and a fix line is written in `src/health/` or beside it, and the two drift
//! silently because nothing joins them. This is the join.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every verb the CLI's dispatch answers to, read out of the `match` that answers them.
///
/// The arms are `"start" => …` and `"remove" | "rm" => …`, so what is looked for is a quoted word
/// on a line that also has a `=>`. Crude on purpose: a stricter parse of Rust syntax would be a
/// second thing to keep in step with the first.
fn verbs() -> BTreeSet<String> {
    let cli = std::fs::read_to_string(root().join("src/bin/skein.rs")).expect("read the CLI");
    let mut found = BTreeSet::new();
    for line in cli.lines() {
        if !line.contains("=>") {
            continue;
        }
        let before = line.split("=>").next().unwrap_or("");
        let mut rest = before;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('"') else { break };
            let word = &after[..close];
            if !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                found.insert(word.to_string());
            }
            rest = &after[close + 1..];
        }
    }
    found
}

/// Every `` `skein <verb>` `` skein can print, with the file and line it is printed from.
fn told_to_run(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            told_to_run(&path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        for (n, line) in body.lines().enumerate() {
            // The backtick is what makes it an instruction rather than prose. `skein doctor says`
            // in a sentence is not something anybody is being told to type.
            let mut rest = line;
            while let Some(at) = rest.find("`skein ") {
                let after = &rest[at + "`skein ".len()..];
                let verb: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                    .collect();
                if !verb.is_empty() {
                    out.push((
                        verb,
                        format!("{}:{}", path.strip_prefix(root()).unwrap().display(), n + 1),
                    ));
                }
                rest = after;
            }
        }
    }
}

#[test]
fn every_fix_line_names_a_command_the_cli_has() {
    let verbs = verbs();
    // The parse is the thing most likely to break silently: a `verbs()` that found nothing would
    // make this test pass by having nothing to compare against.
    for expected in ["start", "attach", "doctor", "ls", "restart", "stop"] {
        assert!(
            verbs.contains(expected),
            "the dispatch parse missed `{expected}`, so this test proves nothing: {verbs:?}"
        );
    }

    let mut told = Vec::new();
    told_to_run(&root().join("src"), &mut told);
    assert!(
        told.len() >= 3,
        "no fix lines were found at all, so this test proves nothing"
    );

    let unrunnable: Vec<String> = told
        .iter()
        .filter(|(verb, _)| !verbs.contains(verb))
        .map(|(verb, at)| format!("{at} tells somebody to run `skein {verb}`"))
        .collect();
    assert!(
        unrunnable.is_empty(),
        "skein prints instructions it cannot carry out — add the verb to the CLI's dispatch, or \
         say something a person can actually type:\n  {}",
        unrunnable.join("\n  ")
    );
}
