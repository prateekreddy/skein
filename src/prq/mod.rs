//! The review queue: every open PR in a repo that is *yours*, and which lane it sits in.
//!
//! skein's PR surface used to hang off a box — `box → branch → gh pr view`, in a `ship` module
//! since retired — which makes a PR nobody in the fleet authored invisible. That is fatal for
//! review, where most of the queue is other people's work. So the spine here is inverted: the **repo** owns the list, a **PR**
//! is the object, and a box is something you summon onto a branch when one turns out to need hands.
//!
//! Three rules decide membership, and all three are GitHub's answer, not skein's: you were asked to
//! review it, you opened it, or you were mentioned in it. CODEOWNERS is not consulted for membership
//! — only for how deeply to explain a change once it's already in your queue. See
//! [`crate::codeowners`] for why that split matters.
//!
//! **Identity lives on the host.** Every call here runs as *you*, on your own credential for the
//! repository it is about — never on a box's. [`crate::gitgate`] narrows each box to its own
//! repository, so outside it a box cannot act as you. An approval that isn't yours is worth nothing
//! when the base branch is protected, so the review path stays on your side of that line.
//!
//! Nothing in this module needs AI. A queue that lists and lanes PRs correctly is already the
//! product; summaries in [`crate::review`] only decide how much reading each row saves you.

use crate::config::skein_home;
use crate::repos::Repo;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// The module is split by the question each file answers, not by size. `mod.rs` holds only the
// wiring: every name that resolved as `crate::prq::X` before the split still does, at the same
// visibility, so nothing outside this directory had to change.

mod checks;
mod credentials;
mod node;
mod refresh;
mod search;
mod store;
mod types;
mod write;

#[cfg(test)]
mod fixtures;

pub use credentials::{
    credential_for_repo, credential_lives, days_until, forget_host_token, forget_renames,
    forget_trunks, host_token, host_token_source, repo_slug, repo_token_source, slug_for_write,
    token_for, trunk_of, viewer, CredentialLife, GhToken, Life, Need,
};
pub use refresh::{
    counts, invalidate, merged, queue, queue_within, unexpired, Count, MergedQueue, StoppedPr,
};
pub use search::forget_batch_widths;
pub use store::{
    archived, move_set_aside_aside, remembered, remembered_head, review_dir, set_archived,
    set_snoozed, snoozed,
};
pub use types::{
    FailedCheck, Lane, Pr, PrComment, Queue, Reason, ReviewRequest, ReviewThread,
    UnreadableSetAside, FAILING_CHECKS_SHOWN,
};
pub use write::{
    base_and_head, head_to_post_against, live_head_sha, merge, pr_body, pr_diff_text, pr_files,
    pr_is_open, re_anchor, resolve_review_thread, submit_review, submit_review_with_comments,
    unresolve_review_thread, ReviewComment, ReviewPost, Verdict,
};

pub(crate) use types::newest_first;
pub(crate) use write::refused_for_conflicts;

#[cfg(test)]
pub(crate) use refresh::CachedQueues;
#[cfg(test)]
pub(crate) use store::remember_for_test;
#[cfg(test)]
pub(crate) use types::blank_pr;

#[cfg(test)]
mod tests {
    /// Nothing in skein runs `gh` any more.
    ///
    /// The queue was built out of the CLI, which made a third-party binary a hard requirement of a
    /// default-on feature — announced nowhere, met as a bug — and dragged in its credential store:
    /// `gh` keeps its token in the system keyring on Linux, so every call was a keyring read and a
    /// locked keyring answered each with a password dialog, every three minutes, for ever.
    ///
    /// Asserted against the source because the way it comes back is a single convenient call in a
    /// module that has no other reason to think about it — the same shape as the bypass that made
    /// most of the earlier keyring fix a no-op.
    #[test]
    fn nothing_here_shells_out_to_gh() {
        let mut offenders = Vec::new();
        // **Every `.rs` under `src/`, not just the top level.** This walked one directory until
        // `prq` became a directory itself — at which point the module the test was written for
        // would have left its own scan, silently, and the gate would have gone on passing.
        let mut todo = vec![std::path::PathBuf::from("src")];
        let mut sources = Vec::new();
        while let Some(dir) = todo.pop() {
            for file in std::fs::read_dir(&dir).expect("a directory under src") {
                let path = file.expect("entry").path();
                if path.is_dir() {
                    todo.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    sources.push(path);
                }
            }
        }
        sources.sort();
        for path in sources {
            // The one legitimate `gh`: `gh_cli_token`, which asks `gh auth token` for the login
            // `gh` itself holds, as the last of the credential sources and only when every other
            // one came up empty. That is about `gh`'s own login by definition, and it is not this —
            // the queue's dependency was the hidden one.
            if path.ends_with("repos/add.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            // Production code only: this test names `gh` in its own strings.
            let source = source.split("\nmod tests {").next().unwrap_or_default();

            for (n, line) in source.lines().enumerate() {
                let runs_gh = line.contains("Command::new(\"gh\")")
                    || line.contains("run_capture(\"gh\"")
                    || line.contains("run_capture_for(\"gh\"")
                    || line.contains("gh_bin()");
                if runs_gh {
                    offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "skein reads GitHub over its own API client; these run the CLI instead:\n{}",
            offenders.join("\n")
        );
    }
}
