//! Every road to a merge carries the same two guards. (SKEIN-338)
//!
//! There were two roads and they were not equally safe. The merge train's `PUT …/merge` carried
//! `sha` — the head skein decided about, so GitHub answers 409 rather than merging something
//! nobody looked at — and every act it took passed
//! `workflow::instead_of_merging_off_the_trunk`, which refuses to land a stacked child's commits
//! on its parent's branch (SKEIN-237). The merge a PERSON pressed in the cockpit carried neither:
//! `prq::merge` sent `{"merge_method": …}` and nothing else, and
//! `grep -rn instead_of_merging_off_the_trunk src/` found the trunk guard reachable only from
//! `workflow.rs` and `prwork.rs`. `$SKEIN_PR_WORKFLOWS` is off on the owner's fleet, so the road
//! with no guards was the only road anybody was driving.
//!
//! The behaviour is tested where it lives — `src/prwork/acts.rs` drives a posed GitHub through
//! `merge_by_hand`, `src/prq.rs` covers the wire and the 409's wording, `src/workflow/` pins the
//! two spellings of the trunk rule together. **What none of those can see is a THIRD merge**: a new
//! `PUT …/merge` somewhere else, or the cockpit route quietly going back to calling `prq::merge`
//! directly and stepping around the trunk check. Those are shape claims about the whole crate, so
//! they are checked against the whole crate, mechanically, here.
//!
//! Read from the source rather than from a running server on purpose. The claim is not "this merge
//! is guarded" — a test for that passes while an unguarded second one is added beside it. The claim
//! is "there is no unguarded merge in this crate", which is a statement about every line of it.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Every `.rs` under `src/`, as `(path-from-root, text)`.
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("src/ is readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let name = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                out.push((name, std::fs::read_to_string(&path).expect("readable")));
            }
        }
    }
    let mut out = Vec::new();
    walk(&root().join("src"), &root(), &mut out);
    assert!(
        out.len() > 10,
        "the source walk found {} files, which means it is not walking src/ and every assertion \
         below is vacuous",
        out.len()
    );
    out
}

/// The line of every request that ends in GitHub's merge endpoint, with the file and line it is on.
///
/// Matched on the PATH rather than on a function name, because a function can be renamed and the
/// URL cannot: `/pulls/{…}/merge` is the one thing every merge in this crate must contain, whatever
/// it calls itself and wherever it lives.
fn merge_endpoints() -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for (name, text) in sources() {
        for (i, line) in text.lines().enumerate() {
            let is_call =
                line.contains("/pulls/{number}/merge") || line.contains("/pulls/{n}/merge");
            // The comments and test fixtures that merely NAME the endpoint are not requests.
            let is_prose =
                line.trim_start().starts_with("//") || line.trim_start().starts_with("///");
            if is_call && !is_prose {
                out.push((name.clone(), i + 1, line.trim().to_string()));
            }
        }
    }
    out
}

/// **No merge request is built anywhere in this crate without the head it was decided about.**
///
/// The `sha` is not a nicety and not GitHub's suggestion: it is what makes the merge conditional.
/// Without it GitHub merges whatever HEAD is at the moment the request lands, which on a pull
/// request somebody pushed to between the render and the press is a merge of code nobody read. With
/// it, GitHub answers 409 and nothing happens.
///
/// Every call site is checked, not the two that exist today, and the whole request expression is
/// searched rather than one line — a body split across lines is still a body.
#[test]
fn every_merge_request_names_the_head_it_was_decided_about() {
    let found = merge_endpoints();
    assert!(
        !found.is_empty(),
        "no merge request was found in src/ at all — the pattern this test matches on has moved, \
         and it is now guarding nothing"
    );
    for (file, line, _) in &found {
        let text = std::fs::read_to_string(root().join(file)).expect("readable");
        let lines: Vec<&str> = text.lines().collect();
        // The request's own expression: from the endpoint to the end of the call. Ten lines is
        // more than any `send_json` in this crate spans and stops well short of the next function.
        let window = lines[line.saturating_sub(1)..(line + 9).min(lines.len())].join("\n");
        assert!(
            window.contains("\"sha\""),
            "{file}:{line} merges without naming a head — GitHub will merge whatever is there when \
             the request lands, which on a branch that moved is a merge of code nobody read:\n\
             {window}"
        );
    }
}

/// **Nothing merges except through the two functions that check the trunk.**
///
/// `prq::merge` holds the `PUT` and knows nothing about bases; `prwork::merge_by_hand` is what puts
/// the trunk guard in front of it. So a caller that reaches `prq::merge` directly has the head
/// anchor and NOT the base check — half a guard, which is exactly the shape SKEIN-338 was: the
/// cockpit route called `prq::merge` and a stacked child merged into its parent.
///
/// The module graph is what makes this the right split rather than an accident of taste:
/// `docs/modules.toml` gives `prq` no dependency on `workflow`, so the trunk rule cannot live in
/// `prq` at all, and `prwork` — which depends on both — is where the two compose. This test is the
/// part of that arrangement a reader cannot see from either file alone.
#[test]
fn the_only_caller_of_the_bare_merge_is_the_one_that_checks_the_base() {
    // The walk reaches the server binary itself now that it is a directory (SKEIN-1103); it used
    // to be pushed on by hand as well, which counted each of its lines twice.
    let all = sources();
    assert!(
        all.iter()
            .any(|(name, _)| name.starts_with("src/bin/skein-server/")),
        "the source walk no longer reaches the server binary, where the merge a person presses is"
    );

    let mut callers: Vec<(String, usize)> = Vec::new();
    for (name, text) in &all {
        for (i, line) in text.lines().enumerate() {
            let calls = line.contains("prq::merge(");
            let is_prose =
                line.trim_start().starts_with("//") || line.trim_start().starts_with("///");
            if calls && !is_prose {
                callers.push((name.clone(), i + 1));
            }
        }
    }
    assert!(
        !callers.is_empty(),
        "nothing calls prq::merge any more — either the merge moved and this test is guarding a \
         function nobody uses, or the cockpit lost its merge entirely"
    );

    // Found rather than named: `merge_by_hand` moved from `src/prwork.rs` to
    // `src/prwork/acts.rs` when that module became a directory (SKEIN-578), and a hard-coded path
    // would have panicked on the directory rather than reported anything about the guard. The
    // line arithmetic below is per-file, so this has to be the ONE file holding it, not the
    // module's text joined — `callers` carries per-file line numbers too.
    let (holder, prwork) = all
        .iter()
        .find(|(_, text)| text.contains("pub fn merge_by_hand"))
        .cloned()
        .expect(
            "merge_by_hand has been renamed or removed — the trunk guard on the hand path is \
             gone with it",
        );
    let guarded = prwork
        .find("pub fn merge_by_hand")
        .expect("just found it above");
    // Where the NEXT item begins: a call after this is in some other function.
    let after = prwork[guarded..]
        .find("\n}\n")
        .map(|n| guarded + n)
        .unwrap_or(prwork.len());
    let inside: Vec<usize> =
        (prwork[..guarded].lines().count() + 1..=prwork[..after].lines().count()).collect();

    for (file, line) in &callers {
        assert_eq!(
            file, &holder,
            "{file}:{line} calls prq::merge directly. That is the head anchor WITHOUT the base \
             check — half a guard, and exactly the shape SKEIN-338 was: the cockpit called it, and \
             a stacked child would merge into its parent. Go through prwork::merge_by_hand."
        );
        assert!(
            inside.contains(line),
            "{holder}:{line} calls prq::merge from outside merge_by_hand, so it skips the \
             trunk guard the hand path exists to apply"
        );
    }
}

/// **The merge a person presses goes through the guarded function, and takes its expected head from
/// what the reader saw rather than from GitHub.**
///
/// The second half is the one worth spelling out, because it is the mistake that would leave every
/// test above green while guarding nothing. If the route derived the expected head by asking GitHub
/// for the live head — `head_to_post_against` and `live_head_sha` are both right there in `prq` and
/// both used by the verdict path two arms above — then the `sha` sent would agree with whatever
/// GitHub has by construction, the 409 could never fire, and a merge would once again land whatever
/// was pushed last.
///
/// The two legitimate sources are the client's `drafted_at` (what is on screen) and
/// `prq::remembered_head` (what this machine last saw for that row, read from cache or disk and
/// never over the network — the SKEIN-272 rule, so a GitHub read failing cannot be what stops a
/// merge).
#[test]
fn the_cockpits_merge_takes_its_head_from_the_reader_and_not_from_github() {
    // Every file of the server binary, each whole, so the arm is bounded inside its own file.
    let text: String = sources()
        .into_iter()
        .filter(|(name, _)| name.starts_with("src/bin/skein-server/"))
        .map(|(_, text)| text)
        .collect();
    let start = text
        .find(r#"(None, "merge") =>"#)
        .expect("the cockpit's merge route has moved — this test no longer guards it");
    // The arm, generously bounded: up to the next match arm of the same `match`.
    let arm = &text[start
        ..text[start..]
            .find(r#"(None, "ask")"#)
            .map(|n| start + n)
            .unwrap_or(text.len())];

    assert!(
        arm.contains("merge_by_hand"),
        "the cockpit merges without the trunk guard — this is SKEIN-338 exactly, and a stacked \
         child pressed from the reading bar lands its parent's commits on its base branch:\n{arm}"
    );
    for derived in ["head_to_post_against", "live_head_sha", "base_and_head"] {
        assert!(
            !arm.contains(derived),
            "the merge route derives the head it claims to have READ from GitHub via {derived}: \
             the expectation would then agree with the live head by construction, the 409 could \
             never fire, and the `sha` would guard nothing:\n{arm}"
        );
    }
    assert!(
        arm.contains("drafted_at") && arm.contains("remembered_head"),
        "the merge route no longer takes its expected head from what the reader saw — both the \
         client's on-screen sha and this machine's remembered one must be consulted, or a merge \
         is refused whenever one of them is absent:\n{arm}"
    );
}
