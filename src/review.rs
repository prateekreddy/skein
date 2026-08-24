//! What a pull request *means*, at the depth it deserves.
//!
//! The queue in [`crate::prq`] answers which PRs are yours. This answers the question you actually
//! open one for: what is being changed, and is it the kind of change you need to have an opinion
//! about. A bug fix gets a line. A change to how something behaves gets explained.
//!
//! # The rule this module must not break
//!
//! [`crate::ai`] states it: **AI may only add scrutiny, never remove it.** Every other AI feature in
//! skein obeys that easily, because they can only escalate. This one cannot — its whole purpose is
//! to tell you a PR is boring, and "boring" is a claim that removes attention.
//!
//! So the failure direction is fixed in the type: [`Depth::Unread`] is what you get when AI is off,
//! the diff could not be fetched, the model timed out, or its answer did not parse. A PR skein has
//! not actually read stays at full attention and says so. A summary can only ever lower depth by
//! **succeeding**, never by failing quietly.
//!
//! # Three stages, so the expensive one runs rarely
//!
//! 0. **Free.** Which changed paths you own per [`crate::codeowners`], and what
//!    [`crate::contracts`] can prove moved by reading the diff. No model, always available — one
//!    scopes the prompts that follow, the other can overrule their verdict.
//! 1. **Cheap.** One small-model pass: a line, and a verdict on whether this needs expanding.
//! 2. **Earned.** The fuller brief, when stage 1 asks for it *or* stage 0 found evidence. The
//!    scanner escalates and never clears, so the two stages cannot talk each other down.
//!
//! Everything is cached against `(number, head_sha)`. That key is not an optimisation: it is the
//! same fact that decides whether your review still counts in [`crate::prq`], so a PR that gains a
//! commit gets a fresh summary and a fresh place in your queue from one change of state.

use crate::ai::claude_oneshot_with;
use crate::codeowners;
use crate::prq::{review_dir, Pr};
use crate::repos::Repo;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

/// Is skein allowed to read pull requests? **On unless you turn it off.**
///
/// Deliberately not [`crate::ai::ai_enabled`], which stays off by default. The two spend on opposite
/// terms: enrichment sweetens a board that already works and runs unasked, while a summary happens
/// only for a PR already in your queue, at most once per head commit — and without it the queue
/// does not do the job it exists for. `$SKEIN_REVIEW_AI` wins, so one env var can pin a run.
///
/// Off is a supported state, not a broken one: every PR reads "not summarised" and keeps your full
/// attention, which is the direction every failure in this module runs.
pub fn summaries_enabled() -> bool {
    // The rule lives in `ai`, which owns both switches — `health` must be able to ask and does not
    // depend on this module. Kept as a name here because every caller in this file reads better for
    // it, and one of them is a public API.
    crate::ai::summaries_enabled()
}

/// How much of a PR skein is prepared to vouch for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Depth {
    /// Read, and small enough to state in a line.
    Line,
    /// Read, and carrying something you should decide on — the line is not enough.
    Expanded,
    /// **Not read.** Full attention. Never a judgement about the PR, always about skein.
    Unread,
}

/// A PR explained, or explicitly not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub number: u64,
    /// The commit this was written against. A summary for any other head is stale by definition.
    pub head_sha: String,
    pub depth: Depth,
    /// One sentence. Empty when [`Depth::Unread`].
    pub line: String,
    /// The fuller brief — mechanism, behaviour, decisions. Only for [`Depth::Expanded`].
    pub detail: String,
    /// Why it was expanded: `behaviour`, `interface`, `default`, `architecture`, `ux`. These are
    /// the tripwires — the things that change how something works rather than whether it works.
    pub flags: Vec<String>,
    /// Changed paths you own, per CODEOWNERS. Empty when the repo has none, which is not a claim
    /// that you own nothing — see [`crate::codeowners`].
    pub yours: Vec<String>,
    /// How many changed paths you do not own, so the summary can say what it left out.
    pub others: usize,
    /// Mechanical evidence from [`crate::contracts`] — what moved, found in the diff rather than
    /// reasoned about. Shown beside the brief because "the model thinks so" and "the diff says so"
    /// are different kinds of claim and you should be able to tell them apart.
    #[serde(default)]
    pub signals: Vec<crate::contracts::Signal>,
    /// Why there is no summary. Only set for [`Depth::Unread`], and written to be shown verbatim.
    pub unread_because: String,
    /// Did answering this actually spend a model call?
    ///
    /// The client keeps a budget for how many pull requests are read WITHOUT being asked, and that
    /// budget counted requests. A request served from the cache on disk costs nothing, so a page
    /// reload spent the whole allowance on six free answers and rows seven onward were never read —
    /// on any reload, for ever. A limit on spending has to count spending.
    ///
    /// Defaults false so a summary read back off disk reports what it is, whatever was written into
    /// it when it was computed.
    #[serde(default)]
    pub computed: bool,
}

impl Summary {
    /// The honest empty answer: this PR has not been read, and here is why.
    fn unread(number: u64, head_sha: &str, because: &str) -> Self {
        Summary {
            number,
            head_sha: head_sha.to_string(),
            depth: Depth::Unread,
            line: String::new(),
            detail: String::new(),
            flags: Vec::new(),
            signals: Vec::new(),
            // Whether getting here cost anything is the caller's to say: an unread summary is
            // written both by a model call that failed (it did) and by the switch being off (it did
            // not). `with_spend` marks the ones that did.
            computed: false,
            yours: Vec::new(),
            others: 0,
            unread_because: because.to_string(),
        }
    }
}

// ───────────────────────────── cache ─────────────────────────────

fn cache_path(repo_id: &str, number: u64, head_sha: &str) -> PathBuf {
    // The head SHA is in the *filename*, so a stale summary can never be read as a fresh one — the
    // lookup for a new head simply misses. Storing one file per PR and comparing a field inside it
    // would work until the day the comparison was forgotten.
    let sha: String = head_sha
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect();
    review_dir(repo_id)
        .join("summaries")
        .join(format!("{number}-{sha}.json"))
}

/// A previously written summary for exactly this PR *and* this head commit.
pub fn cached(repo_id: &str, number: u64, head_sha: &str) -> Option<Summary> {
    let text = fs::read_to_string(cache_path(repo_id, number, head_sha)).ok()?;
    // `computed` is forced false rather than trusted from the file, for the same reason
    // `prq::remembered` forces `fresh` false: what was written was computed WHEN it was written, and
    // the one thing this must never do is let a free answer be counted as one that cost something.
    serde_json::from_str::<Summary>(&text).ok().map(|mut s| {
        s.computed = false;
        s
    })
}

/// What skein said about this PR at an EARLIER commit — the newest such reading, if any.
///
/// The prompt has always had a "what skein already said" slot, and until now nothing could fill it
/// in the case that matters. [`summarise`] returns the cached summary before building a prompt at
/// all, so the only lookup that existed — keyed on the CURRENT head — could only ever hit when
/// somebody pressed "re-read" on a commit already read. A new commit landing, which is the whole
/// reason a PR comes back to you, started from nothing every time.
///
/// So the reading of the commit it moved FROM is what gets handed over, and the model is asked to
/// account for the change rather than for the pull request. Cheaper and better in the same move: it
/// is the difference between "read these forty files" and "you said this yesterday, here is what
/// moved".
///
/// Newest by modification time, because a PR force-pushed twice leaves two and only the last one
/// describes what the branch actually looked like before this push.
pub fn previous(repo_id: &str, number: u64, not_sha: &str) -> Option<Summary> {
    let dir = review_dir(repo_id).join("summaries");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for path in fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()) {
        let Some((n, sha)) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|name| name.split_once('-'))
        else {
            continue;
        };
        if n.parse::<u64>().ok() != Some(number) || sha == not_sha {
            continue;
        }
        let Ok(at) = fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(seen, _)| at > *seen) {
            best = Some((at, path));
        }
    }
    let text = fs::read_to_string(best?.1).ok()?;
    serde_json::from_str::<Summary>(&text)
        .ok()
        .filter(|s| s.depth != Depth::Unread)
}

fn store(repo_id: &str, s: &Summary) -> Result<(), String> {
    let path = cache_path(repo_id, s.number, &s.head_sha);
    let dir = path.parent().ok_or("no parent")?.to_path_buf();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    write_atomic(&path, &dir, &bytes)
}

/// A reading skein already has, and whether it is of the commit that is there now.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Known {
    #[serde(flatten)]
    pub summary: Summary,
    /// True when this reading describes an earlier commit — the branch has moved since.
    pub stale: bool,
}

/// Every reading skein already holds for these pull requests, off disk, costing nothing.
///
/// **Why this exists as a bulk read.** The pane used to discover an existing reading only by asking
/// for it one pull request at a time, through the same path that COMPUTES one — so every limit that
/// belongs to spending money was also a limit on remembering. A reading skein had already paid for
/// was hidden if the pull request was a draft, or unsettled, or in the waiting lane, or simply past
/// the sixth row, because the loop that asks stops when the allowance is gone. Reported as "I can
/// only see 2 PRs with summaries while before there were a bunch", with the owner's own diagnosis:
/// the limits are for new analysis, not for reading from disk.
///
/// So: one request, every reading, no model calls and no rules. What the pane then asks to have
/// COMPUTED is a separate question, and that one keeps every limit it had.
///
/// A reading of an earlier commit is returned too, marked. It is still true about the code it read,
/// and the row says which commit that was — without it, everything skein knew about a pull request
/// vanished from the pane the moment somebody pushed, and came back only if asked for again.
pub fn known(repo_id: &str, prs: &[(u64, String)]) -> std::collections::BTreeMap<u64, Known> {
    let mut out = std::collections::BTreeMap::new();
    for (number, head_sha) in prs {
        if let Some(summary) = cached(repo_id, *number, head_sha) {
            out.insert(
                *number,
                Known {
                    summary,
                    stale: false,
                },
            );
            continue;
        }
        if let Some(summary) = newest_for(repo_id, *number) {
            out.insert(
                *number,
                Known {
                    summary,
                    stale: true,
                },
            );
        }
    }
    out
}

/// The most recent reading of any commit of this pull request.
///
/// By modification time rather than by parsing shas out of filenames: the file's own age is what
/// "most recent" means, and a sha says nothing about which came first.
fn newest_for(repo_id: &str, number: u64) -> Option<Summary> {
    let dir = review_dir(repo_id).join("summaries");
    let prefix = format!("{number}-");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) || !name.ends_with(".json") {
            continue;
        }
        let Ok(at) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(seen, _)| at > *seen) {
            best = Some((at, entry.path()));
        }
    }
    let text = fs::read_to_string(best?.1).ok()?;
    serde_json::from_str::<Summary>(&text).ok().map(|mut s| {
        // Same rule as `cached`: what came off disk did not cost anything NOW.
        s.computed = false;
        s
    })
}

/// Drop what describes a pull request that is over, or a commit that has been replaced.
///
/// Nothing used to. `summaries/<number>-<head_sha>.json` holds one file per PR **per head commit**,
/// which is what makes a stale summary unreadable rather than wrong — the lookup for a new head
/// simply misses — but the miss leaves the old file behind. Every push to a PR under review wrote
/// another and abandoned the last, and merging or closing it abandoned them all.
///
/// Two rules, because the two questions are not the same shape:
///
/// **A superseded head is exact and free.** The queue has just told us every open PR's current sha.
/// A file for that number with any other sha can never be read again by construction, so it goes
/// with no ambiguity and no call to anybody.
///
/// **Closed and merged is asked, not inferred.** A PR absent from this queue has very often just
/// stopped involving you — the searches behind it are `review-requested:you`, `author:you`,
/// `mentions:you` — so absence is not death, and treating it as death deletes the reading of a live
/// PR whose review request was reassigned. `prq::pr_is_open` answers the actual question.
///
/// Best-effort throughout: a failed lookup keeps the file. Deleting a summary costs one re-read;
/// keeping one costs a few kilobytes, and only one of those is irreversible.
///
/// **Only the old ones are asked about.** GitHub reads are cheap here and model calls are not, but
/// cheap is not free and asking about every file on every tab open would be a call per abandoned
/// summary for ever. A file written in the last [`SETTLE`] is left alone: if its PR did just close,
/// keeping it a few more days costs kilobytes, and the next pass reaps it. The superseded-head rule
/// above has no such delay — it needs no call at all, so it runs on everything every time.
pub fn prune(repo_id: &str, slug: &str, open: &[(u64, String)]) -> usize {
    let dir = review_dir(repo_id).join("summaries");
    let Ok(entries) = fs::read_dir(&dir) else {
        return 0;
    };
    let mut gone = 0;
    // Asked once per number rather than once per file: a PR force-pushed ten times has ten files and
    // exactly one answer.
    let mut closed: std::collections::HashMap<u64, bool> = std::collections::HashMap::new();
    let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    // The newest superseded reading per open PR, worked out before anything is removed — it is what
    // [`previous`] will hand to the next pass instead of starting from nothing.
    let newest_stale = keepsakes(&files, open);
    for path in files {
        let Some((number, sha)) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|name| name.split_once('-'))
            .and_then(|(n, sha)| Some((n.parse::<u64>().ok()?, sha.to_string())))
        else {
            continue;
        };
        let doomed = match open.iter().find(|(n, _)| *n == number) {
            // Open, and this is not the commit it is at — but the most recent of those is kept.
            //
            // Deleting every superseded reading and then asking the model to build on "what skein
            // already said" are two changes that undo each other, and they were written an hour
            // apart. [`previous`] hands the reading of the commit a PR moved FROM to the next pass,
            // so exactly one superseded file per PR is not waste — it is the input that makes the
            // next read cheap. The ones behind it describe branches nobody will ever ask about.
            Some((_, head)) => {
                !head.starts_with(&sha) && *head != sha && Some(&path) != newest_stale.get(&number)
            }
            // Not in your queue. That is a question, not an answer — and one worth paying for
            // only once the file has stopped being current enough to be worth keeping anyway.
            None if fresh(&path) => false,
            None => match closed.get(&number) {
                Some(known) => *known,
                None => {
                    let over = crate::prq::pr_is_open(slug, number).map(|open| !open);
                    // `None` — GitHub could not say — keeps the file, and is not remembered, so the
                    // next pass asks again rather than treating one bad call as a verdict.
                    match over {
                        Some(over) => {
                            closed.insert(number, over);
                            over
                        }
                        None => false,
                    }
                }
            },
        };
        if doomed && fs::remove_file(&path).is_ok() {
            gone += 1;
        }
    }
    gone
}

/// For each open pull request, the newest reading of a commit it is no longer at.
///
/// Kept by [`prune`] so [`previous`] has something to hand the next pass. One per PR: the ones
/// behind it describe branches that will never be asked about again.
fn keepsakes(files: &[PathBuf], open: &[(u64, String)]) -> std::collections::HashMap<u64, PathBuf> {
    let mut best: std::collections::HashMap<u64, (std::time::SystemTime, PathBuf)> =
        std::collections::HashMap::new();
    for path in files {
        let Some((n, sha)) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|name| name.split_once('-'))
        else {
            continue;
        };
        let Ok(number) = n.parse::<u64>() else {
            continue;
        };
        // Only for PRs still open, and only for commits they have moved past.
        let Some((_, head)) = open.iter().find(|(o, _)| *o == number) else {
            continue;
        };
        if head.starts_with(sha) || head == sha {
            continue;
        }
        let Ok(at) = fs::metadata(path).and_then(|m| m.modified()) else {
            continue;
        };
        let better = best.get(&number).is_none_or(|(seen, _)| at > *seen);
        if better {
            best.insert(number, (at, path.clone()));
        }
    }
    best.into_iter().map(|(n, (_, path))| (n, path)).collect()
}

/// How long a summary is left alone before it is worth a call to ask whether its PR still exists.
///
/// A week, and the number is a trade rather than a guess: below it, a busy repo spends a GitHub read
/// per abandoned summary on every tab open; above it, an unreadable file survives longer than
/// anybody would notice. Nothing depends on the exact value — being wrong costs kilobytes and one
/// later pass.
const SETTLE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Written recently enough that it is not worth asking about yet.
fn fresh(path: &std::path::Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|at| at.elapsed().map(|age| age < SETTLE).unwrap_or(true))
        .unwrap_or(false)
}

// ───────────────────────────── the diff ─────────────────────────────

/// How much diff each stage is willing to read.
///
/// Truncation is stated to the model rather than hidden, so a large PR produces "I only saw part of
/// this" instead of a confident summary of its first 40KB. A partial read is a reason to keep your
/// attention, not to spend it.
const STAGE1_BYTES: usize = 40_000;
const STAGE2_BYTES: usize = 140_000;
/// The actual review reads more than a summary does: a summary of half a change is still a fair
/// summary, while a review that never saw a file cannot say anything about it. Sized to fit the
/// stronger model's context with room for the answer.
const CRITIQUE_BYTES: usize = 300_000;

/// The PR's diff, and whether it was cut short.
fn pr_diff(slug: &str, number: u64, limit: usize) -> Result<(String, bool), String> {
    Ok(truncate_diff(
        &crate::prq::pr_diff_text(slug, number)?,
        limit,
    ))
}

/// Cut a diff at a FILE boundary under the limit, and name every file that fell off.
///
/// The blind byte cut used to stop mid-hunk — reported live as a review saying "the diff was
/// truncated mid-file (inside the new transport.rs…), so I can't confirm…", a guess-list of what
/// it had not seen. A reader told exactly which files are missing says "these five files were not
/// read" instead of guessing at the shape of the tail; and a cut that lands between files never
/// leaves half a hunk to be mistaken for the whole change.
///
/// One file bigger than the whole limit still has to be cut mid-file — there is no boundary to
/// prefer — and then the note says that instead.
fn truncate_diff(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    // The last file boundary that fits. Boundaries are `diff --git ` at line start.
    let cut_at = text[..limit]
        .match_indices("\ndiff --git ")
        .last()
        .map(|(i, _)| i + 1)
        .filter(|&i| i > 1);
    let Some(cut_at) = cut_at else {
        // The first file alone exceeds the limit: nothing better than the old cut, said plainly.
        let (mut head, _) = truncate(text, limit);
        head.push_str(
            "\n\n(cut for size MID-FILE: this one file is larger than the whole reading budget)\n",
        );
        return (head, true);
    };
    let dropped: Vec<&str> = text[cut_at..]
        .lines()
        .filter_map(|l| l.strip_prefix("diff --git a/"))
        .filter_map(|rest| rest.split(" b/").next())
        .collect();
    let mut head = text[..cut_at].to_string();
    let named = dropped
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    head.push_str(&format!(
        "\n(cut for size: {} more file{} not shown — {}{})\n",
        dropped.len(),
        if dropped.len() == 1 { "" } else { "s" },
        named,
        if dropped.len() > 20 {
            format!(", and {} more", dropped.len() - 20)
        } else {
            String::new()
        },
    ));
    (head, true)
}

/// Cut on a character boundary, reporting whether anything was dropped.
fn truncate(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// The paths a PR touches.
fn changed_paths(slug: &str, number: u64) -> Vec<String> {
    crate::prq::pr_files(slug, number).unwrap_or_default()
}

// ───────────────────────────── stage 0: ownership ─────────────────────────────

/// Which changed paths are yours, and how many are not.
///
/// A repo with no CODEOWNERS returns `(vec![], 0)` — deliberately indistinguishable from "no
/// narrowing available", because that is what it is. Callers must not read an empty `yours` as
/// "none of this is yours".
pub fn ownership(repo: &Repo, identities: &[String], paths: &[String]) -> (Vec<String>, usize) {
    let Some(co) =
        crate::repos::Tree::open(repo).and_then(|tree| codeowners::load(|p| tree.read(p)))
    else {
        return (Vec::new(), 0);
    };
    let (mine, theirs) = co.partition(paths, identities);
    (mine.into_iter().map(str::to_string).collect(), theirs.len())
}

// ───────────────────────────── stage 1 & 2: reading ─────────────────────────────

/// What stage 1 answers. Parsed strictly — see [`parse_stage1`].
struct Verdict {
    line: String,
    expand: bool,
    flags: Vec<String>,
}

/// The tripwires, in the words the prompt uses and the UI shows.
///
/// These are the "way it works is being changed" list: not risk, not size, but whether something's
/// contract moved. A 900-line refactor that changes no behaviour needs a line; a three-line default
/// change needs a paragraph.
const FLAGS: [&str; 5] = ["behaviour", "interface", "default", "architecture", "ux"];

fn stage1_prompt(pr: &Pr, yours: &[String], others: usize, diff: &str, cut: bool) -> String {
    let scope = if yours.is_empty() {
        String::from("This repo has no CODEOWNERS, or none of it is attributed — treat the whole change as in scope.")
    } else {
        format!(
            "The reviewer owns these paths: {}. {} other changed path(s) are outside their ownership — mention them only in passing.",
            yours.join(", "),
            others
        )
    };
    format!(
        r#"You are triaging a pull request for a senior engineer who reviews to stay informed, not to catch bugs. CI and the author already cover correctness. Their words: "I want mechanism level, product level, architectural and user level details. I don't need exact functions or code level details."

Decide how much of their attention this deserves.

Expand ONLY if the change moves something's contract or behaviour. The tripwires are:
- behaviour: an existing feature now does something different
- interface: a flag, route, config key, env var, file format or public API changed
- default: a default value or default-on/off choice changed
- architecture: a mechanism was replaced, removed, or its responsibility moved
- ux: what a person sees or has to do changed

A bug fix, a test, a refactor with no behaviour change, docs, or a dependency bump does NOT expand, however large the diff.
When you are genuinely unsure, expand. Being pulled into one PR too many costs a minute; missing one costs a merge.

PR #{number}: {title}
Branch {head} into {base}.
{scope}
{cut_note}

Answer in EXACTLY this format and nothing else:
KIND: <fix|feature|refactor|docs|chore>
LINE: <one sentence, plain English, saying what this changes and why it matters. For a fix, say what was broken.>
EXPAND: <yes|no>
FLAGS: <comma-separated from: {flags} — or "none" when EXPAND is no>

--- diff ---
{diff}"#,
        number = pr.number,
        title = pr.title,
        head = pr.head_ref,
        base = pr.base_ref,
        scope = scope,
        cut_note = if cut {
            "NOTE: the diff below was truncated. If what you can see is not enough to be sure, answer EXPAND: yes."
        } else {
            ""
        },
        flags = FLAGS.join(", "),
        diff = diff,
    )
}

/// Parse stage 1's answer, strictly.
///
/// Strict on purpose. A model that ignored the format has also ignored the instructions that came
/// with it, and turning half-recognised prose into a confident one-liner is precisely how this
/// module would start removing scrutiny. Unparseable is [`Depth::Unread`], which is loud.
fn parse_stage1(text: &str) -> Option<Verdict> {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
    };
    let line = field("LINE:")?;
    let expand_raw = field("EXPAND:")?.to_ascii_lowercase();
    if line.is_empty() {
        return None;
    }
    let expand = match expand_raw.as_str() {
        "yes" => true,
        "no" => false,
        // Neither yes nor no is not a third option — it is an answer that did not follow the
        // format, and the safe reading is more attention, not less.
        _ => true,
    };
    let flags = field("FLAGS:")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .split(',')
        .map(|f| f.trim().to_string())
        .filter(|f| FLAGS.contains(&f.as_str()))
        .collect();
    Some(Verdict {
        line,
        expand,
        flags,
    })
}

fn stage2_prompt(
    pr: &Pr,
    verdict: &Verdict,
    yours: &[String],
    signals: &[crate::contracts::Signal],
    diff: &str,
    cut: bool,
) -> String {
    format!(
        r#"Explain this pull request to a senior engineer who is reviewing it to understand the system, not to check the code. They will decide whether to approve from what you write, and they will not open the diff. Write at mechanism, product, architecture and user level. Do not describe functions, variables or line-level edits.

Triage already found: {line}
Tripwires: {flags}
{evidence}{scope}
{cut_note}

Write plain prose under exactly these headings, omitting any that has nothing true to say:

## What it does
Two or three sentences at product level.

## What changes in how it works
The part that matters. Be specific about the before and the after: what behaved one way and now behaves another, what a person or a caller has to do differently. If a default moved, say the old value and the new one.

## Worth your call
ONLY if there was a genuinely close alternative the author could reasonably have chosen instead, and reasonable people would differ. State the choice and the alternative in two sentences. If there is no real fork here, omit this heading entirely — do not invent one.

Be brief. Every sentence should be one they would be annoyed to have missed.

PR #{number}: {title}

--- diff ---
{diff}"#,
        line = verdict.line,
        evidence = if signals.is_empty() {
            String::new()
        } else {
            // Named as found-in-the-diff rather than as opinion, so the model treats it as fact to
            // explain rather than a suggestion it may politely disagree with.
            format!(
                "Scanning the diff mechanically found these moved, which is not in dispute — explain what each means for someone using this:\n{}\n",
                signals.iter().map(|s| format!("- {} ({})", s.what, s.file)).collect::<Vec<_>>().join("\n")
            )
        },
        flags = if verdict.flags.is_empty() {
            "none named".to_string()
        } else {
            verdict.flags.join(", ")
        },
        scope = if yours.is_empty() {
            String::new()
        } else {
            format!(
                "The reviewer owns: {}. Go deep there, and stay brief about everything else.",
                yours.join(", ")
            )
        },
        cut_note = if cut {
            "NOTE: the diff was truncated — say so if it limits what you can tell them."
        } else {
            ""
        },
        number = pr.number,
        title = pr.title,
        diff = diff,
    )
}

/// Read a PR, at the depth it earns. Cached against `(number, head_sha)`; `force` re-reads.
///
/// Never returns an error: a PR that could not be read is a [`Depth::Unread`] summary carrying the
/// reason, because the caller's only sane response to a failure here is to show you the PR anyway.
/// What the background reader has already tried and failed to read, per head commit.
///
/// **Why a failure needs remembering at all.** A reading that fails is deliberately NOT cached —
/// caching it would make a failure read as a reading, which is the rule the whole `unread` shape
/// exists for. But the background pass picks work by "has no reading at this head", so an
/// uncacheable failure is picked again on the next pass, and the one after: a pull request whose
/// model call fails costs a model call every ten minutes, for ever. That is the most expensive
/// mistake available in this module, and it is invisible — the pane shows the same "not summarised"
/// row throughout.
///
/// So the reader keeps its own note of what it tried. Nothing else consults it: **the button in the
/// row does not**, because a person pressing "read it" is saying they think it will work now, and a
/// standing failure must never make a button do nothing (same rule as `ai::forget_refusal`).
fn tried_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("read-tried.json")
}

/// The same note, kept for review drafts. A separate file rather than a shared one because the
/// keys are the same `number-sha` shape, and in one file a summary that failed would silence the
/// draft that was never attempted — the two costs are rationed independently.
fn critique_tried_path(repo_id: &str) -> PathBuf {
    crate::prq::review_dir(repo_id).join("critique-tried.json")
}

fn tried_at(path: &std::path::Path) -> std::collections::BTreeMap<String, String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn read_tried(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    let mut all = tried_at(&tried_path(repo_id));
    // Notes written before transport failures stopped being noted at all. They recorded failures
    // that never reached a model — a diff that would not download — and honouring them keeps rows
    // stuck on errors whose cause is already fixed. Dropped on read; the next write drops them
    // from the file too.
    all.retain(|_, why| !why.contains("its diff could not be read"));
    all
}

fn critique_tried(repo_id: &str) -> std::collections::BTreeMap<String, String> {
    tried_at(&critique_tried_path(repo_id))
}

fn note_into(
    path: &std::path::Path,
    mut all: std::collections::BTreeMap<String, String>,
    number: u64,
    head_sha: &str,
    why: &str,
) {
    all.insert(format!("{number}-{head_sha}"), why.to_string());
    // Bounded: this is per head commit, so a busy repo would otherwise grow one entry per push for
    // ever. The pruning that drops stale summaries has the same job and the same shape.
    if all.len() > 200 {
        let drop: Vec<_> = all.keys().take(all.len() - 200).cloned().collect();
        for key in drop {
            all.remove(&key);
        }
    }
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
        if let Ok(bytes) = serde_json::to_vec_pretty(&all) {
            let _ = write_atomic(path, dir, &bytes);
        }
    }
}

fn note_tried(repo_id: &str, number: u64, head_sha: &str, why: &str) {
    // Read through `read_tried`, not `tried_at`, so its legacy-note filter still gets its
    // "dropped from the file on the next write".
    note_into(
        &tried_path(repo_id),
        read_tried(repo_id),
        number,
        head_sha,
        why,
    )
}

fn note_critique_tried(repo_id: &str, number: u64, head_sha: &str, why: &str) {
    note_into(
        &critique_tried_path(repo_id),
        critique_tried(repo_id),
        number,
        head_sha,
        why,
    )
}

/// How many pull requests one background pass reads.
///
/// Burst control, not a budget: the budget is the SCOPE — pull requests somebody asked you to
/// review, in a repo you switched reading on for (the owner's own answer, SKEIN-185). This only
/// stops a queue that has been quiet all week from firing thirty model calls in one minute when it
/// finally settles.
const READ_PER_PASS: usize = 3;

/// Read the pull requests waiting on you, in the repos you asked skein to read, with nobody
/// watching.
///
/// **Three things bound this, and two of them are the owner's answers rather than my guesses:**
///
/// * a repo reads nothing until `read_prs` is switched on for it, so the feature costs exactly
///   nothing on a fleet nobody has opted in;
/// * only pull requests where you are the REVIEWER — your own do not need summarising for you, and
///   being mentioned is not a request to review. This is why there is no daily quota: the scope is
///   the budget, and a number would only bound the damage of reading the wrong things;
/// * settled ([`crate::prq::settled`]), not a draft, and not already read at this head.
///
/// Returns what it read, for the server's log.
pub fn read_waiting() -> Vec<String> {
    if !summaries_enabled() {
        return Vec::new();
    }
    let mut read = Vec::new();
    for repo in crate::repos::load_repos() {
        if !repo.read_prs {
            continue;
        }
        let Ok(queue) = crate::prq::queue(&repo, false) else {
            continue;
        };
        if !queue.ai {
            continue;
        }
        let identities = std::iter::once(queue.viewer.clone()).collect::<Vec<_>>();
        for pr in queue.prs.iter() {
            let read_it = worth_reading(&repo.id, pr);
            // The critique door is the same doorway reading uses — lane, draft, settled — so a PR
            // the reader would not summarise cannot enter the pass through its second half. What
            // it deliberately does NOT require is an uncached summary: the queue this feature
            // landed on was already summarised at its current heads, and "drafted alongside the
            // summary" must cover those rows too, not only heads that move later.
            let draft_it = matches!(pr.lane, crate::prq::Lane::NeedsYou)
                && !pr.draft
                && pr.settled
                && worth_critiquing(&repo.id, pr, &queue.viewer);
            if !read_it && !draft_it {
                continue;
            }
            if read.len() >= READ_PER_PASS {
                return read;
            }
            if read_it {
                // Never `force`: a reading already on disk for this head is the answer, and asking
                // again would spend a model call to be told what skein already knows.
                let summary = summarise(&repo, &queue.slug, pr, &identities, false);
                // A reading that could not be made is written down as tried, or the next pass picks
                // it straight back up — see `tried_path`. The row still says "not summarised", and
                // the button still reads it on request.
                //
                // Only when a model call was SPENT, though (`computed`) — that is the cost the note
                // exists to stop repeating. A failure before the model — the diff would not
                // download, GitHub was slow — costs one HTTP call to retry, and writing it down
                // here pinned a bad network minute to the head sha as a permanent error row. Left
                // unnoted, the next pass simply tries again. A PR that could not even be
                // summarised earns no draft either: the same diff feeds both.
                if matches!(summary.depth, Depth::Unread) {
                    if summary.computed {
                        note_tried(&repo.id, pr.number, &pr.head_sha, &summary.unread_because);
                    }
                    continue;
                }
                // Only what actually cost something is reported. A cache hit is not news, and a
                // line per cache hit would bury the ones that are.
                if summary.computed {
                    read.push(format!("{}: read #{}", repo.id, pr.number));
                }
            }
            // The reader's second half: where the review is yours to give, draft it in the same
            // pass, so opening the row finds summary AND review waiting instead of costing a
            // second round trip.
            if draft_it {
                match draft_critique(&repo, &queue.slug, pr) {
                    Ok(_) => read.push(format!("{}: drafted a review for #{}", repo.id, pr.number)),
                    // Non-fatal by construction: the summary above already stands, the row simply
                    // opens without a draft and the button still drafts on request. Noted only
                    // when a model call was SPENT — the same boundary the summary path draws with
                    // `computed`, or a bad network minute pins "no draft" to this head for ever.
                    Err(fail) if fail.spent => {
                        note_critique_tried(&repo.id, pr.number, &pr.head_sha, &fail.why)
                    }
                    Err(_) => {}
                }
            }
        }
    }
    read
}

/// Is the review of this pull request yours to give — and therefore worth drafting, unasked?
///
/// A narrower question than [`worth_reading`]'s, because the spend is bigger: a summary tells you
/// about a PR you are involved in for any reason, a drafted review presumes you will be the one
/// reviewing. Yours to give means asked (personally or through a team — a team request IS a
/// review request, same rule as `worth_reading`), already reviewing (you acted once and the PR is
/// still open), or your own pull request. Being mentioned is somebody talking *about* you, not a
/// request to review, and must never cost the model call a draft is.
fn worth_critiquing(repo_id: &str, pr: &Pr, viewer: &str) -> bool {
    let yours_to_give = pr.author == viewer
        || pr.reasons.iter().any(|r| {
            matches!(
                r,
                crate::prq::Reason::Reviewer
                    | crate::prq::Reason::Reviewed
                    | crate::prq::Reason::Team(_)
            )
        });
    yours_to_give
        // Never twice for one `(number, head_sha)` — the stored draft IS the answer at this head,
        // the same key discipline as the summary cache, and for the same money reason. A new
        // commit is a new key, so a moved head drafts again exactly as it summarises again.
        && !critiqued(repo_id, pr.number).is_some_and(|c| c.head_sha == pr.head_sha)
        // Tried at this head and could not be drafted. Only the background consults this note —
        // the button in the pane goes nowhere near it, same rule as `tried_path`.
        && !critique_tried(repo_id).contains_key(&format!("{}-{}", pr.number, pr.head_sha))
}

/// Is this a pull request skein should read for you, unasked?
fn worth_reading(repo_id: &str, pr: &Pr) -> bool {
    // Somebody asked you to review it — personally or through a team you are in. A team request IS
    // a review request; the queue's own filter says so, and dropping those here would silently
    // exclude exactly the pull requests the team query was added to find.
    let asked = pr.reasons.iter().any(|r| {
        matches!(
            r,
            crate::prq::Reason::Reviewer
                | crate::prq::Reason::Reviewed
                | crate::prq::Reason::Team(_)
        )
    });
    asked
        && matches!(pr.lane, crate::prq::Lane::NeedsYou)
        && !pr.draft
        && pr.settled
        // A reading of THIS head. One of an earlier commit is kept and shown as such (SKEIN-184),
        // but it is not a reason to leave the current one unread.
        && cached(repo_id, pr.number, &pr.head_sha).is_none()
        // Tried at this head and could not be read. Not for ever: a new commit is a new key, and
        // asking for it by hand goes nowhere near this.
        && !read_tried(repo_id).contains_key(&format!("{}-{}", pr.number, pr.head_sha))
}

pub fn summarise(repo: &Repo, slug: &str, pr: &Pr, identities: &[String], force: bool) -> Summary {
    // Somebody pressed "read it". Whatever the model refused with last time, they are entitled to
    // find out whether it still refuses — a standing refusal must never make a button do nothing.
    if force {
        crate::ai::forget_refusal();
    }
    if !force {
        if let Some(hit) = cached(&repo.id, pr.number, &pr.head_sha) {
            return hit;
        }
    }
    if !summaries_enabled() {
        return Summary::unread(
            pr.number,
            &pr.head_sha,
            "summaries are switched off — turn \"Read pull requests\" back on in Settings → Boxes. Until then every PR stays at full attention.",
        );
    }
    if pr.head_sha.is_empty() {
        // Without a head commit there is nothing to key a cache on, so a summary written now could
        // outlive the code it describes. Refusing is cheaper than being subtly wrong later.
        return Summary::unread(
            pr.number,
            "",
            "GitHub did not report a head commit for this PR.",
        );
    }
    let paths = changed_paths(slug, pr.number);
    let (yours, others) = ownership(repo, identities, &paths);

    // One fetch, three readers. The scanner wants as much of the diff as it can get — a contract
    // change in the tail is still a contract change — while stage 1 only needs enough to classify.
    // Fetching twice would cost a second round trip to tell us something we already had.
    let (full, deep_cut) = match pr_diff(slug, pr.number, STAGE2_BYTES) {
        Ok(d) => d,
        Err(e) => {
            return Summary::unread(
                pr.number,
                &pr.head_sha,
                &format!("its diff could not be read: {e}"),
            )
        }
    };
    if full.trim().is_empty() {
        return Summary::unread(
            pr.number,
            &pr.head_sha,
            "GitHub returned an empty diff for this PR.",
        );
    }
    let signals = crate::contracts::scan(&full);
    let (diff, cut) = truncate(&full, STAGE1_BYTES);

    let raw = match crate::ai::claude_oneshot_telling(
        &stage1_prompt(pr, &yours, others, &diff, cut),
        review_model(None).as_deref(),
        Duration::from_secs(60),
    ) {
        Ok(raw) => raw,
        // The reason, not a disjunction. "the model call failed or timed out" was the whole of what
        // this said, for four different problems with four different fixes — and it named the
        // timeout first for a failure that came back in two seconds.
        // Asked, and could not answer. That counts as spent — a `claude` that is not logged in
        // fails instantly and free, and a budget that did not count it would ask it once per row on
        // every reload for ever.
        Err(unread) => {
            let mut said = Summary::unread(pr.number, &pr.head_sha, &unread.say());
            said.computed = true;
            return said;
        }
    };
    let Some(verdict) = parse_stage1(&raw) else {
        let mut said = Summary::unread(pr.number, &pr.head_sha, "skein read it but could not make sense of its own answer, so it is not vouching for one.");
        said.computed = true;
        return said;
    };

    // The scanner escalates and never clears. A model that read a moved default as routine is
    // overruled by the diff itself; a model that flagged something the scanner has no rule for
    // keeps its flag. There is no path here where mechanical evidence *lowers* the depth, which is
    // what makes shipping imperfect rules safe — see [`crate::contracts`].
    let mut flags = verdict.flags.clone();
    for s in &signals {
        if !flags.contains(&s.kind) {
            flags.push(s.kind.clone());
        }
    }
    let expand = verdict.expand || !signals.is_empty();

    let mut summary = Summary {
        number: pr.number,
        head_sha: pr.head_sha.clone(),
        depth: if expand { Depth::Expanded } else { Depth::Line },
        // Reached only by having run the model.
        computed: true,
        line: verdict.line.clone(),
        detail: String::new(),
        flags,
        signals: signals.clone(),
        yours,
        others,
        unread_because: String::new(),
    };

    if expand {
        // The whole diff and a longer budget, because this is the pass whose output you will
        // actually decide from. The stronger model is named here rather than in the env so a pinned
        // `$SKEIN_AI_MODEL` still overrides both stages together.
        match claude_oneshot_with(
            &stage2_prompt(pr, &verdict, &summary.yours, &signals, &full, deep_cut),
            review_model(Some("claude-sonnet-5")).as_deref(),
            Duration::from_secs(180),
        ) {
            Some(detail) => summary.detail = detail,
            // Stage 1 said this one deserves explaining and stage 2 could not. Falling back to the
            // one-liner would be the exact inversion of this module's rule: it would present a PR
            // flagged as needing your judgement as though it had been summarised.
            None => {
                return Summary::unread(
                    pr.number,
                    &pr.head_sha,
                    &format!(
                    "this one needs explaining ({}) and skein could not do it — read it yourself.",
                    verdict.line
                ),
                )
            }
        }
    }
    let _ = store(&repo.id, &summary);
    summary
}

// ───────────────────────────── asking, and drafting ─────────────────────────────

/// The context every follow-up is answered against: the PR, its diff, and what skein already
/// concluded about it.
///
/// Built once and shared by [`ask`] and [`draft_comment`] because the two differ only in what they
/// are asked to produce. Both get the *summary* as well as the diff — a question asked after
/// reading the brief is usually a question about the brief.
fn context(slug: &str, pr: &Pr, repo: &Repo) -> String {
    let repo_id = &repo.id;
    let (diff, cut) = pr_diff(slug, pr.number, STAGE2_BYTES).unwrap_or_default();
    // Standing notes on the parts this change lands in — the thing a diff structurally cannot show,
    // and the reason [`crate::moduledocs`] exists. Only fresh ones, and only ones already written:
    // a question typed into a box is not the moment to spend a minute writing four of them.
    let notes = crate::moduledocs::fresh_notes(repo, &changed_paths(slug, pr.number));
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!(
            "\n--- standing notes on the parts this touches ---\n{}\n",
            notes
                .iter()
                .map(|d| format!("## {}\n{}", d.path, d.text))
                .collect::<Vec<_>>()
                .join("\n\n")
        )
    };
    // The current head first — that is the "re-read this" case, where what skein said about THIS
    // commit is the thing to improve on. Otherwise the commit it moved from, which is the case that
    // was unreachable and is the common one.
    let prior = cached(repo_id, pr.number, &pr.head_sha)
        .filter(|s| s.depth != Depth::Unread)
        .or_else(|| previous(repo_id, pr.number, &pr.head_sha))
        .map(|s| {
            if s.detail.is_empty() {
                format!("\nWhat skein already said about it: {}\n", s.line)
            } else {
                format!(
                    "\nWhat skein already said about it:\n{}\n{}\n",
                    s.line, s.detail
                )
            }
        })
        .unwrap_or_default();
    format!(
        "PR #{n}: {title}\nBranch {head} into {base}.{prior}{notes}{cut_note}\n\n--- diff ---\n{diff}",
        n = pr.number,
        title = pr.title,
        head = pr.head_ref,
        base = pr.base_ref,
        prior = prior,
        notes = notes,
        cut_note = if cut {
            "\nNOTE: the diff below was truncated."
        } else {
            ""
        },
        diff = diff,
    )
}

/// Answer a question about a PR, privately. Nothing here is posted anywhere.
///
/// The separation from [`draft_comment`] is the point: most questions are for your own
/// understanding, and an answer that might be published is a different, more careful, less useful
/// answer. Posting is a second, deliberate act.
pub fn ask(repo: &Repo, slug: &str, pr: &Pr, question: &str) -> Result<String, String> {
    if !summaries_enabled() {
        return Err(
            "reading PRs is switched off — turn \"Read pull requests\" back on in Settings → Boxes."
                .into(),
        );
    }
    let question = question.trim();
    if question.is_empty() {
        return Err("ask something".into());
    }
    let prompt = format!(
        r#"A senior engineer is reviewing this pull request to understand the system, not to check the code. Answer their question at mechanism, product, architecture and user level. Do not walk through functions or lines unless they ask for that specifically.

This answer is PRIVATE — it goes to them, not onto the pull request. Be direct, be brief, and say plainly when the diff does not tell you the answer rather than inferring one.

Their question: {question}

{context}"#,
        question = question,
        context = context(slug, pr, repo),
    );
    claude_oneshot_with(
        &prompt,
        review_model(Some("claude-sonnet-5")).as_deref(),
        Duration::from_secs(180),
    )
    .ok_or_else(|| "no answer came back — the model call failed or timed out.".into())
}

/// Draft a comment for a PR from your rough intent. Returns text to **edit**, never to post.
///
/// The posting is a separate call for the reason you gave: the agent drafts, you correct it, then it
/// goes. A draft that could post itself would be a different feature with a different risk.
pub fn draft_comment(repo: &Repo, slug: &str, pr: &Pr, intent: &str) -> Result<String, String> {
    if !summaries_enabled() {
        return Err("reading PRs is switched off — turn \"Read pull requests\" back on in Settings → Boxes.".into());
    }
    let intent = intent.trim();
    if intent.is_empty() {
        return Err("say roughly what you want to tell them".into());
    }
    let prompt = format!(
        r#"Write a pull request comment from a reviewer's rough notes. This WILL be posted publicly on GitHub under their name once they have edited it, so write what they would write.

Rules:
- Say only what the notes say. Do not add praise, caveats, or requests they did not make.
- Be specific about code where being specific helps the author act; reference paths, not line numbers.
- Plain, direct, collegial. No preamble, no sign-off, no "great work overall".
- Markdown is fine. Keep it as short as the point allows.
- Output a line reading exactly COMMENT: and then the comment body, and NOTHING else — no
  explanation of what you wrote or changed, no notes to the reviewer, before or after.

Their notes: {intent}

{context}"#,
        intent = intent,
        context = context(slug, pr, repo),
    );
    claude_oneshot_with(
        &prompt,
        review_model(Some("claude-sonnet-5")).as_deref(),
        Duration::from_secs(180),
    )
    .map(|raw| drafted_body(&raw))
    .ok_or_else(|| "no draft came back — the model call failed or timed out.".into())
}

// --- An actual review: comments drafted for a person to vet, then post -------------------------
//
// Asked for in the owner's words: "an actual review of the code with option for me go through the
// comments and then ask to post on PR, do not find issues for the sake of it." Three properties
// follow from that sentence and everything here serves one of them:
//
// 1. **Nothing posts without the person.** Drafting and posting are separate calls, and posting
//    takes the vetted comments as input — the server never posts what it stored, only what the
//    person kept (and possibly edited).
// 2. **A comment lands where the problem is.** Each one is anchored to a file and a NEW-side line,
//    validated against the diff's own hunks. One the model mis-anchored is not thrown away and not
//    guessed at — it travels in the review body, marked as such.
// 3. **A review of one commit is not posted onto another.** The draft carries the head sha it read;
//    posting against a moved head is refused out loud, with the fix (draft again) named.

/// One drafted review comment. `line` is a NEW-side line number; `anchored` says the diff actually
/// shows that line, which is GitHub's own condition for accepting the comment there.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Draft {
    pub path: String,
    pub line: u64,
    pub anchored: bool,
    pub text: String,
}

/// A drafted review: the overall note and the comments, tied to the commit that was read.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Critique {
    pub number: u64,
    pub head_sha: String,
    /// The reviewer's note on the change as a whole. "nothing to flag" is a complete, valid answer
    /// — the prompt says so, because a reviewer made to produce findings produces noise.
    pub overall: String,
    pub comments: Vec<Draft>,
    /// The diff was cut at the byte cap, so this review saw part of the change.
    pub truncated: bool,
}

fn critique_path(repo_id: &str, number: u64, head_sha: &str) -> PathBuf {
    crate::prq::review_dir(repo_id)
        .join("critiques")
        .join(format!("{number}-{head_sha}.json"))
}

/// The newest draft for this pull request, whatever commit it was drafted at. The caller compares
/// `head_sha` with the queue's — same shape as [`known`]: an old draft is shown as old, not hidden.
pub fn critiqued(repo_id: &str, number: u64) -> Option<Critique> {
    let dir = crate::prq::review_dir(repo_id).join("critiques");
    let prefix = format!("{number}-");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) || !name.ends_with(".json") {
            continue;
        }
        let Ok(at) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(seen, _)| at > *seen) {
            best = Some((at, entry.path()));
        }
    }
    serde_json::from_str(&fs::read_to_string(best?.1).ok()?).ok()
}

fn store_critique(repo_id: &str, c: &Critique) -> Result<(), String> {
    let path = critique_path(repo_id, c.number, &c.head_sha);
    let dir = path.parent().ok_or("no parent")?.to_path_buf();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    write_atomic(
        &path,
        &dir,
        &serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?,
    )
}

/// The NEW-side lines the diff actually shows, per file — exactly the lines GitHub accepts a
/// RIGHT-side review comment on. Context and added lines count; a deleted line exists only on the
/// left, and a deleted file has no right side at all.
fn commentable(diff: &str) -> std::collections::BTreeMap<String, std::collections::BTreeSet<u64>> {
    let mut map: std::collections::BTreeMap<String, std::collections::BTreeSet<u64>> =
        Default::default();
    let mut file: Option<String> = None;
    let mut line = 0u64;
    let mut in_hunk = false;
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("+++ b/") {
            file = Some(rest.to_string());
            in_hunk = false;
        } else if l.starts_with("+++ ") {
            // `+++ /dev/null` — a deleted file.
            file = None;
            in_hunk = false;
        } else if let Some(hunk) = l.strip_prefix("@@") {
            line = hunk
                .split('+')
                .nth(1)
                .and_then(|v| v.split([',', ' ']).next())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            in_hunk = line > 0;
        } else if in_hunk && (l.starts_with('+') || l.starts_with(' ') || l.is_empty()) {
            // An empty line inside a hunk is a context line whose content is empty.
            if let Some(f) = &file {
                map.entry(f.clone()).or_default().insert(line);
            }
            line += 1;
        } else if in_hunk && l.starts_with('-') {
            // Left side only; the new-file counter does not move.
        } else {
            in_hunk = false;
        }
    }
    map
}

/// The model a review call uses: `$SKEIN_REVIEW_MODEL`, else the setting, else this call's own
/// default. Layered UNDER `$SKEIN_AI_MODEL`, which `ai::binary_and_model` lets win over everything.
fn review_model(fallback: Option<&'static str>) -> Option<String> {
    std::env::var("SKEIN_REVIEW_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .or_else(|| {
            let m = crate::config::load_config().review_model;
            (!m.trim().is_empty()).then(|| m.trim().to_string())
        })
        .or_else(|| fallback.map(str::to_string))
}

fn critique_prompt(pr: &Pr, diff: &str, cut: bool) -> String {
    format!(
        r#"Review this pull request as a careful senior engineer. A human reviewer will go through every comment you produce, keep or drop each one, and post the kept ones under their own name — so every comment must earn its place.

Comment ONLY on actual problems and improvements that matter: bugs, correctness risks, races, security holes, data loss, error paths that can actually fail and are not handled, misleading names or comments that will cause a wrong call later, real performance traps. Do not manufacture findings to seem thorough; do not comment on style, formatting, or preferences; no praise, no hedged maybes, no restating what the diff does. If the change is fine, say so and stop — an empty review is a valid review.

Title: {title}
Author: {author}
Branch: {head} into {base}
{cut_note}
Format, EXACTLY:
First line:  OVERALL: <one sentence on the change as a whole, or "nothing to flag">
Then one block per comment, each ended by a line containing only three dashes:
FILE: <the path exactly as it appears in the diff>
LINE: <the line number IN THE NEW FILE this is about — count from the +start in the nearest @@ header. 0 if it is about the change as a whole>
COMMENT: <the comment. Say what is wrong and what to do instead. May span lines.>
---

The diff:
{diff}"#,
        title = pr.title,
        author = pr.author,
        head = pr.head_ref,
        base = pr.base_ref,
        cut_note = if cut {
            "NOTE: the diff below was cut at a byte cap — you are seeing part of the change. \
             The cut names the files that are missing; do not guess about them, and say your \
             review does not cover them.\n"
        } else {
            ""
        },
        diff = diff,
    )
}

/// Parse the model's review. `None` when the answer did not follow the format at all — that is an
/// answer to show as a failure, not to guess comments out of.
fn parse_critique(text: &str) -> Option<Critique> {
    let overall = text.lines().find_map(|l| {
        l.trim()
            .strip_prefix("OVERALL:")
            .map(|v| v.trim().to_string())
    })?;
    let mut comments = Vec::new();
    for block in text.split("\n---") {
        let field = |key: &str| {
            block
                .lines()
                .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
        };
        let (Some(path), Some(line)) = (field("FILE:"), field("LINE:")) else {
            continue;
        };
        // The comment is everything from COMMENT: to the end of the block — it may span lines.
        let Some(at) = block.find("COMMENT:") else {
            continue;
        };
        let body = block[at + "COMMENT:".len()..].trim().to_string();
        if body.is_empty() {
            continue;
        }
        comments.push(Draft {
            path,
            line: line.parse().unwrap_or(0),
            anchored: false, // decided against the diff by the caller, never by the model
            text: body,
        });
    }
    Some(Critique {
        number: 0,
        head_sha: String::new(),
        overall,
        comments,
        truncated: false,
    })
}

/// Fold the vetted comments into what GitHub is told: anchored ones ride as line comments, the
/// unanchored join the body named by their file, and a dropped one is dropped by never arriving
/// here. Pure, because this is the step where "what the person kept" becomes "what gets posted" —
/// the one transformation that must never be wrong quietly.
pub fn assemble_post(overall: &str, kept: &[Draft]) -> (String, Vec<crate::prq::ReviewComment>) {
    let mut body = overall.trim().to_string();
    for d in kept.iter().filter(|d| !d.anchored) {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&format!("**{}**: {}", d.path, d.text));
    }
    let anchored = kept
        .iter()
        .filter(|d| d.anchored)
        .map(|d| crate::prq::ReviewComment {
            path: d.path.clone(),
            line: d.line,
            body: d.text.clone(),
        })
        .collect();
    (body, anchored)
}

/// Post what the person kept, and nothing else — the whole write path, so the rules live where
/// they can be proven: a review of one commit is not posted onto another, and the payload is
/// assembled from the VETTED comments handed in, never from what was stored.
pub fn post_critique(
    repo: &Repo,
    number: u64,
    head_sha: &str,
    overall: &str,
    kept: &[Draft],
) -> Result<String, String> {
    let queue = crate::prq::queue(repo, false)?;
    let pr = queue
        .prs
        .iter()
        .find(|p| p.number == number)
        .ok_or("that PR is not in your queue")?;
    // The draft was anchored against the commit it read; if the branch moved, the right move is a
    // fresh draft, and saying so beats GitHub's 422.
    if pr.head_sha != head_sha {
        return Err(
            "the branch has moved since this review was drafted — draft it again against the              new commits before posting."
                .into(),
        );
    }
    let (body, anchored) = assemble_post(overall, kept);
    let said = crate::prq::submit_review_with_comments(
        &queue.slug,
        number,
        head_sha,
        crate::prq::Verdict::Comment,
        &body,
        &anchored,
    )?;
    crate::prq::invalidate(&repo.id);
    Ok(said)
}

/// Draft an actual review of the PR: read the diff, produce comments, anchor each against the
/// hunks, store the draft. Returns it for the pane to lay out for vetting. Costs a model call —
/// run because a person asked, or by the background reader for a PR whose review is yours to give
/// (see [`worth_critiquing`]).
pub fn critique(repo: &Repo, slug: &str, pr: &Pr) -> Result<Critique, String> {
    if !summaries_enabled() {
        return Err(
            "reading PRs is switched off — turn \"Read pull requests\" back on in Settings → Boxes."
                .into(),
        );
    }
    // A person pressed the button; a standing refusal must never make it do nothing. The
    // background reader deliberately does not come through here — an unattended pass has no
    // standing to clear a refusal a person has not seen.
    crate::ai::forget_refusal();
    draft_critique(repo, slug, pr).map_err(|fail| fail.why)
}

/// What the reading view shows: the change itself, at a display budget, cut honestly.
///
/// This is the diff `summarise` already fetches and drops — SKEIN-148's finding was that skein
/// had the diff, had a renderer, and connected neither to pull requests, sending a 30-a-day
/// reviewer to github.com for the primary act. No model call anywhere on this path: reading the
/// code needs no summary, so an unread PR opens exactly as readably as a read one.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Reading {
    pub head_sha: String,
    pub diff: String,
    /// The diff was cut at a file boundary to fit the budget. The pane says so rather than
    /// silently ending — the same honesty rule the summaries follow.
    pub cut: bool,
}

/// Ten times the model's critique budget: a person scrolls where a prompt cannot, and the cost of
/// a bigger answer here is bytes on loopback, not tokens. Still bounded, because "the browser tab
/// died" is a worse ending than "the tail is on GitHub".
const READING_BYTES: usize = 3 * CRITIQUE_BYTES;

pub fn reading(slug: &str, pr: &Pr) -> Result<Reading, String> {
    let raw = crate::prq::pr_diff_text(slug, pr.number)?;
    let (diff, cut) = truncate_diff(&raw, READING_BYTES);
    Ok(Reading {
        head_sha: pr.head_sha.clone(),
        diff,
        cut,
    })
}

/// A draft that did not happen, and whether it cost a model call. `spent` is the same boundary
/// [`Summary::computed`] draws: everything from the model call onward counts (a `claude` that is
/// not logged in fails instantly and free, and an unnoted failure is retried every pass, for
/// ever), while a diff that would not download costs one HTTP call to retry and must not be
/// pinned to the head sha as permanent.
struct CritiqueFail {
    why: String,
    spent: bool,
}

fn draft_critique(repo: &Repo, slug: &str, pr: &Pr) -> Result<Critique, CritiqueFail> {
    let free = |why: String| CritiqueFail { why, spent: false };
    let spent = |why: String| CritiqueFail { why, spent: true };
    let (diff, cut) = pr_diff(slug, pr.number, CRITIQUE_BYTES).map_err(free)?;
    if diff.trim().is_empty() {
        return Err(free("GitHub returned an empty diff for this PR.".into()));
    }
    let raw = crate::ai::claude_oneshot_telling(
        &critique_prompt(pr, &diff, cut),
        review_model(Some("claude-sonnet-5")).as_deref(),
        Duration::from_secs(300),
    )
    .map_err(|unread| spent(unread.say()))?;
    let mut drafted = parse_critique(&raw).ok_or_else(|| {
        spent(
            "the model's review did not follow the format, so no comments are being offered from it — try again."
                .into(),
        )
    })?;
    let lines = commentable(&diff);
    for d in &mut drafted.comments {
        d.anchored = d.line > 0 && lines.get(&d.path).is_some_and(|set| set.contains(&d.line));
    }
    drafted.number = pr.number;
    drafted.head_sha = pr.head_sha.clone();
    drafted.truncated = cut;
    // Spent: the model already answered, and losing the write is worth noting rather than
    // re-buying the answer next pass.
    store_critique(&repo.id, &drafted).map_err(spent)?;
    Ok(drafted)
}

/// The comment body out of a model answer that may carry meta-chatter before it.
///
/// The prompt has always said body-only, and a model narrated anyway — a live draft opened with
/// `Publishing "…" isn't right for a PR comment — let me rewrite that as feedback in the
/// reviewer's own voice.` and THAT landed in the box the person was about to post from. Telling a
/// model harder is not a mechanism; a marker it must emit is. Everything before the first
/// `COMMENT:` is the model talking to itself, and an answer without the marker is taken whole, so
/// an answer that followed the old instruction exactly still works.
fn drafted_body(raw: &str) -> String {
    match raw.find("COMMENT:") {
        Some(at) => raw[at + "COMMENT:".len()..].trim().to_string(),
        None => raw.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {

    /// What skein reads with nobody watching, and — mostly — what it does not.
    ///
    /// The two limits are the owner's own, and they are limits of SCOPE rather than of number:
    ///
    /// * "only ones where I mark the automatic reading enabled" — a repo reads nothing until it is
    ///   switched on, so the ordinary state of this feature on a fleet is that it costs nothing;
    /// * "read only PRs where I am reviewer" — your own pull requests do not need summarising for
    ///   you, and being mentioned is not a request to review.
    ///
    /// Which is why there is no daily quota. A number bounds the damage of reading the wrong things;
    /// a scope stops reading them. This asserts the scope, one exclusion at a time, because every
    /// one of them is a model call that would otherwise be spent while nobody is looking.
    #[cfg(unix)]
    #[test]
    fn what_it_reads_unwatched_is_what_you_were_asked_to_review() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        // A `claude` that answers stage one in the format the prompt demands. The shared stub does
        // not, and an answer that does not parse is an UNREAD summary — which would make this test
        // assert the failure path while looking like it asserted the happy one.
        let claude = home.join("claude-stage1.sh");
        std::fs::write(
            &claude,
            "#!/bin/sh\nprintf 'KIND: fix\\nLINE: it changes a thing.\\nEXPAND: no\\nFLAGS: none\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(
            &claude,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        std::env::set_var("SKEIN_CLAUDE_BIN", &claude);

        let pr = |number: u64, reason: crate::prq::Reason, lane: crate::prq::Lane| crate::prq::Pr {
            number,
            title: "t".into(),
            author: "someone".into(),
            url: String::new(),
            head_ref: "feat".into(),
            head_sha: format!("sha{number}"),
            base_ref: "main".into(),
            draft: false,
            updated_at: String::new(),
            committed_at: String::new(),
            settled: true,
            labels: Vec::new(),
            review_decision: String::new(),
            mergeable: None,
            additions: None,
            deletions: None,
            changed_files: None,
            checks: "none".into(),
            my_review: "none".into(),
            review_is_current: false,
            reasons: vec![reason],
            lane,
            box_name: String::new(),
        };
        use crate::prq::{Lane, Reason};

        // Asked to review it, personally or through a team: both are review requests, and dropping
        // the team one here would silently exclude the pull requests the team query exists to find.
        assert!(worth_reading(
            "demo",
            &pr(1, Reason::Reviewer, Lane::NeedsYou)
        ));
        assert!(worth_reading(
            "demo",
            &pr(2, Reason::Team("infra".into()), Lane::NeedsYou)
        ));

        // Yours. You know what is in it.
        assert!(!worth_reading(
            "demo",
            &pr(3, Reason::Author, Lane::NeedsYou)
        ));
        // Mentioned in a comment is not a request to review.
        assert!(!worth_reading(
            "demo",
            &pr(4, Reason::Mentioned, Lane::NeedsYou)
        ));
        // Already decided on, or set aside: not waiting on you.
        assert!(!worth_reading(
            "demo",
            &pr(5, Reason::Reviewer, Lane::Waiting)
        ));
        assert!(!worth_reading(
            "demo",
            &pr(6, Reason::Reviewer, Lane::Archived)
        ));

        // A draft is the author saying it is not finished.
        let mut draft = pr(7, Reason::Reviewer, Lane::NeedsYou);
        draft.draft = true;
        assert!(!worth_reading("demo", &draft));

        // Still being pushed to: reading it describes a commit about to be replaced.
        let mut moving = pr(8, Reason::Reviewer, Lane::NeedsYou);
        moving.settled = false;
        assert!(!worth_reading("demo", &moving));

        // And one already read AT THIS HEAD is not read again — the single most expensive mistake
        // available here, since it would spend a model call every pass, for ever, on every row.
        let already = pr(9, Reason::Reviewer, Lane::NeedsYou);
        assert!(worth_reading("demo", &already));
        store(
            "demo",
            &Summary {
                number: 9,
                head_sha: already.head_sha.clone(),
                depth: Depth::Line,
                line: "read".into(),
                detail: String::new(),
                flags: Vec::new(),
                signals: Vec::new(),
                yours: Vec::new(),
                others: 0,
                unread_because: String::new(),
                computed: true,
            },
        )
        .unwrap();
        assert!(
            !worth_reading("demo", &already),
            "a pull request already read at this commit would be read again, every pass"
        );

        // A repo nobody switched on reads nothing, whatever is in its queue.
        //
        // Asserted against a GitHub that ANSWERS, and both ways round. The first version of this
        // checked only that the pass read nothing with consent off — and passed with the consent
        // check deleted, because the queue could not be read at all in the fixture. A test that
        // cannot tell "declined" from "failed" is not testing consent.
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        // While this file exists, the stub fails the diff request — a GitHub having a bad minute.
        let diff_broken = home.join("diff-broken");
        let diff_broken_flag = diff_broken.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::{Read as _, Write as _};
                let mut stream = stream;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let body = said
                    .split("\r\n\r\n")
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let (status, answer) = if head.contains("/pulls/11 HTTP")
                    && diff_broken_flag.exists()
                {
                    (500u16, r#"{"message":"transient"}"#.to_string())
                } else if head.contains("/user/teams") {
                    (200, "[]".to_string())
                } else if head.contains("/user") {
                    (200, r#"{"login":"me"}"#.to_string())
                } else if body.contains("review-requested") {
                    // One pull request, waiting on your review, settled, unread.
                    (200, r#"{"data":{"search":{"nodes":[{"number":11,"title":"t","url":"u",
                       "isDraft":false,"author":{"login":"someone"},"headRefName":"feat",
                       "headRefOid":"sha11","baseRefName":"main",
                       "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                       "latestReviews":{"nodes":[]},
                       "commits":{"nodes":[{"commit":{"committedDate":"2020-01-01T00:00:00Z"}}]}}]}}}"#
                        .to_string())
                } else if head.contains("/graphql") {
                    (200, r#"{"data":{"search":{"nodes":[]}}}"#.to_string())
                } else {
                    (200, "{}".to_string())
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        crate::repos::save_repos(&[serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/thing.git",
            "source_tree": "",
            "store": "",
            "read_prs": false,
        }))
        .unwrap()])
        .unwrap();
        assert!(
            read_waiting().is_empty(),
            "a repo nobody switched reading on for was read anyway"
        );

        // And with consent given, the same queue IS read — which is what makes the assertion above
        // mean "it declined" rather than "it could not".
        crate::repos::set_read_prs("demo", true).unwrap();
        let read = read_waiting();
        assert_eq!(
            read.len(),
            1,
            "the repo was switched on and nothing was read: {read:?}"
        );
        assert!(read[0].contains("#11"), "{read:?}");

        // Twice does not read twice: the second pass finds the reading already on disk for this
        // head, which is the difference between a background reader and a standing order.
        // Twice does not read twice. Whether the reading succeeded or failed, the next pass leaves
        // it alone: a success is cached at this head, and a failure is written down as tried —
        // without which a pull request whose model call fails costs one every pass, for ever, while
        // the pane shows the same "not summarised" row throughout.
        assert!(
            read_waiting().is_empty(),
            "the same pull request was read again on the next pass"
        );

        // **And a reading that FAILS is not tried again either**, which is the expensive half. A
        // failed reading is deliberately not cached — caching it would make a failure read as a
        // reading — so without a note of the attempt the pass picks it straight back up: one model
        // call every ten minutes, for ever, while the pane shows the same "not summarised" row.
        //
        // Observed by counting how often the CLI is actually run, because "it returned nothing"
        // looks identical whether or not it asked.
        let asked = home.join("asked");
        let broken = home.join("claude-broken.sh");
        std::fs::write(
            &broken,
            format!(
                "#!/bin/sh\necho x >> {}\nprintf 'no format here'\n",
                asked.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &broken,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        std::env::set_var("SKEIN_CLAUDE_BIN", &broken);
        // Forget the good reading, so #11 is due again.
        std::fs::remove_dir_all(crate::prq::review_dir("demo").join("summaries")).unwrap();

        assert!(
            read_waiting().is_empty(),
            "an unreadable answer was reported as a reading"
        );
        let after_one = std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(after_one, 1, "the reader did not ask once: {after_one}");

        read_waiting();
        let after_two = std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(
            after_two, 1,
            "a pull request that could not be read was asked about again — one model call per pass, \
             for ever"
        );

        // **A failure that never reached the model is NOT written down** — the opposite rule from
        // the one just proved, and they share a boundary: `computed`. A diff that would not
        // download costs one HTTP call to retry; noting it pinned a bad network minute to the head
        // sha as a permanent error row (found live, as every big diff "timing out" once and
        // sticking). So: break the diff endpoint, watch the pass fail WITHOUT asking the model or
        // writing a note — then heal the endpoint and watch the same head get read, no new commit
        // and no button pressed.
        let _ = std::fs::remove_dir_all(crate::prq::review_dir("demo").join("summaries"));
        let _ = std::fs::remove_file(crate::prq::review_dir("demo").join("read-tried.json"));
        std::fs::write(&diff_broken, "").unwrap();
        assert!(
            read_waiting().is_empty(),
            "a pull request whose diff would not download was reported as read"
        );
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "the model was asked about a diff that never arrived"
        );
        // The raw file, not `read_tried` — that loader also filters legacy transport notes out,
        // and asserting through it let a wrongly-written note pass as an unwritten one.
        let raw = std::fs::read_to_string(crate::prq::review_dir("demo").join("read-tried.json"))
            .unwrap_or_default();
        assert!(
            !raw.contains("11-sha11"),
            "a transport failure was pinned to the head sha — it would never retry: {raw}"
        );
        std::fs::remove_file(&diff_broken).unwrap();
        std::env::set_var("SKEIN_CLAUDE_BIN", &claude);
        let healed = read_waiting();
        assert_eq!(
            healed.len(),
            1,
            "GitHub came back and the reader did not: {healed:?}"
        );
        assert!(healed[0].contains("#11"), "{healed:?}");

        for key in [
            "SKEIN_HOME",
            "SKEIN_REVIEW_AI",
            "SKEIN_CLAUDE_BIN",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
    }

    /// A GitHub that answers with two pull requests — #21 waiting on your review, #22 where you
    /// are only mentioned — and a `claude` that answers whichever prompt it is handed: the stage-1
    /// shape for summaries, the OVERALL shape for review drafts. Review calls are counted into a
    /// file, because "it drafted nothing" looks identical whether or not the model was asked, and
    /// the dedupe test below is ABOUT how often it was asked.
    #[cfg(unix)]
    fn drafting_fixture(home: &std::path::Path) -> std::path::PathBuf {
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        let reviews_asked = home.join("reviews-asked");
        let claude = home.join("claude-both.sh");
        // The review prompt is the only one carrying the literal `OVERALL:`; the stage prompts ask
        // for KIND/LINE. Branching on the prompt is what lets ONE binary serve a pass that now
        // makes two different model calls per pull request.
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\ncase \"$4\" in\n  *OVERALL:*) echo x >> {count}; printf 'OVERALL: nothing to flag\\n';;\n  *) printf 'KIND: fix\\nLINE: it changes a thing.\\nEXPAND: no\\nFLAGS: none\\n';;\nesac\n",
                count = reviews_asked.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &claude,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        std::env::set_var("SKEIN_CLAUDE_BIN", &claude);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::{Read as _, Write as _};
                let mut stream = stream;
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let body = said
                    .split("\r\n\r\n")
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let node = |number: u64| {
                    format!(
                        r#"{{"number":{number},"title":"t","url":"u",
                           "isDraft":false,"author":{{"login":"someone"}},"headRefName":"feat",
                           "headRefOid":"sha{number}","baseRefName":"main",
                           "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                           "latestReviews":{{"nodes":[]}},
                           "commits":{{"nodes":[{{"commit":{{"committedDate":"2020-01-01T00:00:00Z"}}}}]}}}}"#
                    )
                };
                let answer = if head.contains("/user/teams") {
                    "[]".to_string()
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if body.contains("review-requested:") {
                    format!(r#"{{"data":{{"search":{{"nodes":[{}]}}}}}}"#, node(21))
                } else if body.contains("mentions:") {
                    format!(r#"{{"data":{{"search":{{"nodes":[{}]}}}}}}"#, node(22))
                } else if head.contains("/graphql") {
                    r#"{"data":{"search":{"nodes":[]}}}"#.to_string()
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        crate::repos::save_repos(&[serde_json::from_value(serde_json::json!({
            "id": "crit",
            "source": "https://github.com/acme/thing.git",
            "source_tree": "",
            "store": "",
            "read_prs": true,
        }))
        .unwrap()])
        .unwrap();
        // The queue micro-cache outlives a test's SKEIN_HOME; a stale hit would answer with a
        // queue read against another test's stub.
        crate::prq::invalidate("crit");
        reviews_asked
    }

    #[cfg(unix)]
    fn drafting_teardown() {
        for key in [
            "SKEIN_HOME",
            "SKEIN_REVIEW_AI",
            "SKEIN_CLAUDE_BIN",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::invalidate("crit");
        crate::prq::forget_host_token();
    }

    /// The reader opens summary and drafted review in one go: a pull request waiting on YOUR
    /// review comes out of the background pass with both stored, so expanding the row costs
    /// nothing and asks nothing. No HTTP pane involved — the pass alone must do it.
    #[cfg(unix)]
    #[test]
    fn the_reader_drafts_the_review_where_it_is_yours_to_give() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let _asked = drafting_fixture(home);

        let read = read_waiting();
        assert!(
            cached("crit", 21, "sha21").is_some(),
            "the pass did not summarise the PR waiting on you: {read:?}"
        );
        let drafted = critiqued("crit", 21);
        assert!(
            drafted.as_ref().is_some_and(|c| c.head_sha == "sha21"),
            "the pass summarised #21 but drafted no review for it — summary and draft must arrive together: {read:?}"
        );

        drafting_teardown();
    }

    /// Being mentioned is somebody talking ABOUT you. It gets no unrequested review draft — each
    /// draft is a paid model call, and the scope is the budget (same rule as `worth_reading`).
    ///
    /// Asserted end-to-end AND at the predicate: today `worth_reading` already keeps a
    /// mentioned-only PR out of the pass entirely, so the guard inside the pass only bites the day
    /// the reading rule widens — which is exactly when nobody will be looking at it.
    #[cfg(unix)]
    #[test]
    fn a_mention_is_not_a_request_for_a_drafted_review() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let _asked = drafting_fixture(home);

        let _ = read_waiting();
        assert!(
            critiqued("crit", 22).is_none(),
            "a PR you were only mentioned on got an unrequested review draft — a paid model call nobody asked for"
        );

        let pr = |author: &str, reasons: Vec<crate::prq::Reason>| crate::prq::Pr {
            number: 90,
            title: "t".into(),
            author: author.into(),
            url: String::new(),
            head_ref: "feat".into(),
            head_sha: "sha90".into(),
            base_ref: "main".into(),
            draft: false,
            updated_at: String::new(),
            committed_at: String::new(),
            settled: true,
            labels: Vec::new(),
            review_decision: String::new(),
            mergeable: None,
            additions: None,
            deletions: None,
            changed_files: None,
            checks: "none".into(),
            my_review: "none".into(),
            review_is_current: false,
            reasons,
            lane: crate::prq::Lane::NeedsYou,
            box_name: String::new(),
        };
        use crate::prq::Reason;
        // Yours to give: asked personally, asked through a team, already in the conversation as a
        // reviewer, or your own pull request.
        assert!(worth_critiquing(
            "crit",
            &pr("someone", vec![Reason::Reviewer]),
            "me"
        ));
        assert!(worth_critiquing(
            "crit",
            &pr("someone", vec![Reason::Team("infra".into())]),
            "me"
        ));
        assert!(worth_critiquing(
            "crit",
            &pr("someone", vec![Reason::Reviewed]),
            "me"
        ));
        assert!(
            worth_critiquing("crit", &pr("me", vec![Reason::Author]), "me"),
            "your own pull request is yours to review"
        );
        assert!(
            !worth_critiquing("crit", &pr("someone", vec![Reason::Mentioned]), "me"),
            "mentioned-only is not a request to review, and must not spend a draft"
        );

        drafting_teardown();
    }

    /// Never twice for one `(number, head_sha)`. The summary being wiped forces the pass to walk
    /// the same PR again — the exact spot where a missing dedupe re-buys the draft — and the count
    /// of model calls, not the presence of a draft, is what tells the two apart.
    #[cfg(unix)]
    #[test]
    fn a_review_already_drafted_at_this_head_is_not_bought_twice() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = drafting_fixture(home);

        let _ = read_waiting();
        let once = std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(once, 1, "the first pass should draft exactly one review");

        std::fs::remove_dir_all(crate::prq::review_dir("crit").join("summaries")).unwrap();
        let _ = read_waiting();
        let twice = std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(
            twice, 1,
            "a review already drafted at this head was drafted again — one model call per pass, for ever"
        );

        drafting_teardown();
    }

    /// The queue this feature landed on was already summarised at its current heads. If the pass
    /// only reaches PRs whose summary is still to be made, those rows never get a draft until
    /// their heads move — the reader would open summary-and-no-review for exactly the pull
    /// requests it was built for. The critique door must open on a cached summary too.
    #[cfg(unix)]
    #[test]
    fn a_summary_already_on_disk_still_earns_its_draft() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = drafting_fixture(home);

        // First pass: summary and draft both land, as the feature promises.
        let _ = read_waiting();
        assert!(cached("crit", 21, "sha21").is_some());
        assert!(critiqued("crit", 21).is_some());

        // The draft is gone, the summary is not — the pre-feature shape of every live row.
        std::fs::remove_dir_all(crate::prq::review_dir("crit").join("critiques")).unwrap();
        let _ = read_waiting();
        assert!(
            critiqued("crit", 21).is_some_and(|c| c.head_sha == "sha21"),
            "a PR summarised at this head before the feature landed never gets its draft — the \
             critique door only opens where a summary is still to be made"
        );
        let calls = std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(
            calls, 2,
            "the redraft is one model call, the cached summary none"
        );

        drafting_teardown();
    }

    /// What skein already holds is handed over in one go, and an older reading is marked, not lost.
    ///
    /// The bulk read exists because the pane used to learn what skein knew only by asking for one
    /// pull request at a time, down the path that COMPUTES a reading — so a draft's reading, an
    /// unsettled branch's reading, and everything past the sixth row were all hidden by limits meant
    /// to bound money. Reading from disk is not spending, and this is the call that says so.
    #[test]
    fn every_reading_on_disk_is_handed_over_at_once() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let put = |number: u64, head: &str, line: &str| {
            store(
                "demo",
                &Summary {
                    number,
                    head_sha: head.into(),
                    depth: Depth::Line,
                    line: line.into(),
                    detail: String::new(),
                    flags: Vec::new(),
                    signals: Vec::new(),
                    yours: Vec::new(),
                    others: 0,
                    unread_because: String::new(),
                    computed: true,
                },
            )
            .unwrap();
        };
        put(1, "aaa", "read at the current head");
        put(2, "old", "read before the last push");

        let asked = [
            (1, "aaa".to_string()),
            // #2 has moved since it was read.
            (2, "new".to_string()),
            // #3 has never been read at all.
            (3, "ccc".to_string()),
        ];
        let known = known("demo", &asked);

        assert_eq!(known.len(), 2, "a reading was lost or invented: {known:?}");
        let one = &known[&1];
        assert_eq!(one.summary.line, "read at the current head");
        assert!(!one.stale, "a reading of the current head was marked stale");
        assert!(
            !one.summary.computed,
            "a reading off disk claims to have cost a model call, so a budget that counts spending \
             is spent by answers that were free"
        );

        // The one that matters: a reading of an earlier commit is KEPT and marked, not dropped.
        // Dropped, everything skein knew about a pull request vanished the moment somebody pushed —
        // and only a person asking again could bring it back.
        let two = &known[&2];
        assert_eq!(two.summary.line, "read before the last push");
        assert!(
            two.stale,
            "a reading of an earlier commit was handed over as current"
        );

        // And nothing is invented for one that was never read.
        assert!(!known.contains_key(&3));

        std::env::remove_var("SKEIN_HOME");
    }

    /// An answer served from the cache does not report as having cost anything.
    ///
    /// The client keeps a budget for how many pull requests are read WITHOUT being asked for, and
    /// that budget counted REQUESTS. A request answered from the cache on disk costs nothing, so a
    /// page reload spent the whole allowance on free answers and the rows past it were never read —
    /// on any reload, for ever. Strictly worse than having no budget, which is the shape of mistake
    /// worth a test of its own: the limit was doing the opposite of its name.
    #[test]
    fn a_cached_summary_does_not_count_as_a_model_call() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let fresh = Summary {
            number: 4,
            head_sha: "abc".into(),
            depth: Depth::Line,
            line: "a change".into(),
            detail: String::new(),
            flags: Vec::new(),
            signals: Vec::new(),
            yours: Vec::new(),
            others: 0,
            unread_because: String::new(),
            computed: true,
        };
        store("demo", &fresh).unwrap();

        let read_back = cached("demo", 4, "abc").expect("the summary was stored");
        assert_eq!(
            read_back.line, "a change",
            "the summary itself did not survive"
        );
        assert!(
            !read_back.computed,
            "a summary read off disk claims to have cost a model call, so a budget that counts \
             spending is spent by answers that were free"
        );

        // And "unread" from the switch being off is not spending either — nothing was asked.
        let off = Summary::unread(4, "abc", "AI is off");
        assert!(!off.computed);
    }

    /// Pruning keeps exactly the readings that can still be read.
    ///
    /// Nothing pruned anything: one file per PR per head commit, and every push abandoned the last
    /// while merging abandoned them all. The two rules have to be told apart here, because getting
    /// the second one wrong deletes a live PR's reading — absence from this queue means "no longer
    /// involves you" at least as often as it means "closed", and the queue is personal.
    #[test]
    fn pruning_drops_replaced_commits_and_keeps_what_it_cannot_ask_about() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        // A GitHub that answers by number: #9 is closed, #8 is still open. Both are absent from the
        // lane below, which is the whole point — absence is the question, not the answer.
        let (base, asked) = stub_github();
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let dir = crate::prq::review_dir("demo").join("summaries");
        fs::create_dir_all(&dir).unwrap();
        let put = |name: &str| {
            fs::write(dir.join(format!("{name}.json")), "{}").unwrap();
        };
        put("7-aaaaaaaa"); // #7 is open at aaaaaaaa — the current reading
        put("7-ffffffff"); // two pushes ago: nobody will ever ask about that branch again
        put("7-bbbbbbbb"); // the commit it moved FROM — kept, because `previous` hands it on
        put("9-cccccccc"); // #9 left the lane because it CLOSED — old enough to be worth asking
        put("8-eeeeeeee"); // #8 left the lane and is still open — asked, and kept
        put("4-dddddddd"); // #4 left the lane too, but is too recent to spend a call on
        put("nonsense"); // not a summary; must be left alone rather than guessed at

        // Backdated past SETTLE, or the lookup is never reached and this test passes for a reason
        // that has nothing to do with what it claims to check — which is exactly what it did on its
        // first draft: sabotaging the failed-lookup arm changed nothing, because every file was new.
        let old = std::time::SystemTime::now() - (SETTLE + Duration::from_secs(60));
        // Explicit ordering between the two superseded readings, so "the newest" is a fact rather
        // than whatever order the filesystem happened to create them in.
        backdate(
            &dir.join("7-ffffffff.json"),
            std::time::SystemTime::now() - Duration::from_secs(3600),
        );
        for name in ["9-cccccccc", "8-eeeeeeee"] {
            let handle = fs::OpenOptions::new()
                .write(true)
                .open(dir.join(format!("{name}.json")))
                .unwrap();
            handle
                .set_times(fs::FileTimes::new().set_modified(old).set_accessed(old))
                .unwrap();
        }

        let gone = prune("demo", "acme/repo", &[(7, "aaaaaaaa".to_string())]);

        let left = |name: &str| dir.join(format!("{name}.json")).exists();
        assert!(
            left("7-aaaaaaaa"),
            "the reading of the commit the PR is AT was deleted"
        );
        // Exactly one superseded reading survives, and it is the newest. Not waste: the next pass
        // hands it to the model as "what skein already said", which is the difference between
        // re-reading forty files and accounting for what moved.
        assert!(
            left("7-bbbbbbbb"),
            "the reading of the commit this PR moved FROM was deleted, so the next read starts \
             from nothing — pruning and building-on-the-last-reading undoing each other"
        );
        assert!(!left("7-ffffffff"), "a reading two pushes back was kept");
        assert!(
            !left("9-cccccccc"),
            "the reading of a CLOSED pull request was kept — which is the \
             whole of what was asked for"
        );
        // The one that would lose real work. This queue is personal, so a PR leaving it usually
        // means it stopped involving you.
        assert!(
            left("8-eeeeeeee"),
            "a pull request that merely left your lane, and is still open, was deleted"
        );
        assert!(left("nonsense"), "a file that is not a summary was deleted");
        assert_eq!(
            gone, 2,
            "exactly the oldest superseded commit and the closed PR"
        );

        // And the recent one was never ASKED about — cheap is not free, and a call per abandoned
        // summary on every tab open is a cost nobody agreed to. Asserted on the requests actually
        // made, because "it survived" is also true of a file that was asked about and kept.
        let asked = asked.lock().unwrap().clone();
        assert!(
            !asked.iter().any(|p| p.contains("/pulls/4")),
            "a summary too recent to be worth a call was asked about anyway: {asked:?}"
        );
        assert!(
            asked.iter().any(|p| p.contains("/pulls/9")),
            "the old one was never asked about, so nothing here was tested: {asked:?}"
        );

        std::env::remove_var("SKEIN_GITHUB_API");
        std::env::remove_var("GH_TOKEN");
        crate::prq::forget_host_token();
    }

    /// A GitHub that cannot be reached deletes nothing it was not certain about.
    ///
    /// Its own test because the sibling above uses a working stub, which never reaches this arm —
    /// and this is the arm where being wrong is unrecoverable. Deleting a summary normally costs one
    /// re-read; deleting every summary in the repo because GitHub was down for a minute costs the
    /// lot, and the superseded rule must still work while it happens.
    #[test]
    fn a_github_that_cannot_be_reached_deletes_only_what_needs_no_asking() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        // Nothing listens here.
        std::env::set_var("SKEIN_GITHUB_API", "http://127.0.0.1:1");

        let dir = crate::prq::review_dir("demo").join("summaries");
        fs::create_dir_all(&dir).unwrap();
        for name in ["7-aaaaaaaa", "7-ffffffff", "7-bbbbbbbb", "9-cccccccc"] {
            fs::write(dir.join(format!("{name}.json")), "{}").unwrap();
        }
        backdate(
            &dir.join("7-ffffffff.json"),
            std::time::SystemTime::now() - Duration::from_secs(3600),
        );
        let old = std::time::SystemTime::now() - (SETTLE + Duration::from_secs(60));
        let handle = fs::OpenOptions::new()
            .write(true)
            .open(dir.join("9-cccccccc.json"))
            .unwrap();
        handle
            .set_times(fs::FileTimes::new().set_modified(old).set_accessed(old))
            .unwrap();

        let gone = prune("demo", "acme/repo", &[(7, "aaaaaaaa".to_string())]);
        assert_eq!(
            gone, 1,
            "only the oldest superseded commit, which needs nobody's opinion"
        );
        assert!(dir.join("7-aaaaaaaa.json").exists());
        assert!(
            dir.join("7-bbbbbbbb.json").exists(),
            "the keepsake was deleted"
        );
        assert!(!dir.join("7-ffffffff.json").exists());
        assert!(
            dir.join("9-cccccccc.json").exists(),
            "a summary was deleted on a lookup that never got an answer — an unreachable GitHub \
             would empty the whole cache, and nothing here is recoverable"
        );

        std::env::remove_var("SKEIN_GITHUB_API");
        std::env::remove_var("GH_TOKEN");
        crate::prq::forget_host_token();
    }

    /// Make a file look older than it is. Several call sites now, and a brace-heavy block inlined
    /// four times is one that drifts.
    fn backdate(path: &std::path::Path, to: std::time::SystemTime) {
        let handle = fs::OpenOptions::new().write(true).open(path).unwrap();
        handle
            .set_times(fs::FileTimes::new().set_modified(to).set_accessed(to))
            .unwrap();
    }

    /// A GitHub that answers "is this pull request open" by number.
    fn stub_github() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).ok();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    line.clear();
                }
                recorder.lock().unwrap().push(path.clone());
                let body = match path.ends_with("/pulls/9") {
                    true => r#"{"state":"closed"}"#,
                    false => r#"{"state":"open"}"#,
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), asked)
    }
    use super::*;

    /// The default that makes the queue worth opening. A fresh install, and an existing
    /// `config.json` written before this field existed, must both read as on — `#[serde(default)]`
    /// on a bool would silently make every upgraded user's queue unread.
    #[test]
    fn reading_prs_is_on_unless_you_turn_it_off() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::remove_var("SKEIN_REVIEW_AI");
        assert!(summaries_enabled(), "a fresh install must read PRs");

        // An older config.json, from before the field was added.
        std::fs::write(
            crate::config::skein_home().join("config.json"),
            br#"{"ai_enrichment":false}"#,
        )
        .unwrap();
        assert!(
            summaries_enabled(),
            "an upgraded config must not silently switch reading off"
        );

        std::env::set_var("SKEIN_REVIEW_AI", "off");
        assert!(!summaries_enabled(), "the env override must still win");
        std::env::remove_var("SKEIN_REVIEW_AI");
    }

    /// The two budgets are separate on purpose: enrichment runs unasked, reading a PR does not.
    #[test]
    fn reading_prs_does_not_depend_on_the_background_enrichment_switch() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::remove_var("SKEIN_REVIEW_AI");
        std::env::set_var("SKEIN_AI", "off");
        assert!(
            summaries_enabled(),
            "turning off board enrichment must not stop the review queue reading PRs"
        );
        std::env::remove_var("SKEIN_AI");
    }

    #[test]
    fn a_well_formed_verdict_parses() {
        let v = parse_stage1(
            "KIND: fix\nLINE: stops the parser crashing on empty input.\nEXPAND: no\nFLAGS: none",
        )
        .unwrap();
        assert_eq!(v.line, "stops the parser crashing on empty input.");
        assert!(!v.expand);
        assert!(v.flags.is_empty());
    }

    #[test]
    fn flags_are_kept_only_when_they_are_real_tripwires() {
        let v = parse_stage1("LINE: x\nEXPAND: yes\nFLAGS: default, interface, vibes").unwrap();
        assert_eq!(v.flags, vec!["default", "interface"]);
    }

    /// The direction that matters: anything other than a clean "no" must not read as "no".
    #[test]
    fn an_unclear_expand_answer_expands() {
        let v = parse_stage1("LINE: x\nEXPAND: probably not\nFLAGS: none").unwrap();
        assert!(v.expand, "an unrecognised verdict must escalate, not clear");
    }

    #[test]
    fn prose_that_ignored_the_format_is_not_a_summary() {
        assert!(parse_stage1("This PR looks fine to me, it just fixes a typo.").is_none());
        assert!(
            parse_stage1("LINE: something\n").is_none(),
            "a missing EXPAND is not a no"
        );
        assert!(
            parse_stage1("LINE:\nEXPAND: no").is_none(),
            "an empty line is not a summary"
        );
    }

    #[test]
    fn truncation_reports_itself_and_stays_on_a_char_boundary() {
        let (text, cut) = truncate("héllo world", 3);
        assert!(cut);
        assert!(text.len() <= 3);
        assert!(text.chars().all(|c| c != '\u{fffd}'));
        let (whole, uncut) = truncate("short", 100);
        assert_eq!(whole, "short");
        assert!(!uncut);
    }

    #[test]
    fn an_unread_summary_carries_its_reason_and_no_claim() {
        let s = Summary::unread(3, "abc", "AI is off");
        assert_eq!(s.depth, Depth::Unread);
        assert!(
            s.line.is_empty(),
            "unread must not carry a line anyone could act on"
        );
        assert_eq!(s.unread_because, "AI is off");
    }

    #[test]
    fn the_cache_key_is_the_head_commit() {
        let a = cache_path("r", 7, "aaa");
        let b = cache_path("r", 7, "bbb");
        assert_ne!(a, b, "two heads of one PR must not share a summary file");
        assert!(a.to_string_lossy().contains("7-aaa"));
    }

    #[test]
    fn a_hostile_sha_cannot_escape_the_cache_directory() {
        let p = cache_path("r", 1, "../../etc/passwd");
        assert!(!p.to_string_lossy().contains(".."), "{}", p.display());
    }

    #[test]
    fn ownership_of_a_repo_without_codeowners_narrows_nothing() {
        let repo = Repo {
            read_prs: false,
            id: "r".into(),
            source: "https://github.com/a/b".into(),
            source_tree: "/nonexistent-path-for-this-test".into(),
            store: String::new(),
            agent: "claude".into(),
            plane_project: String::new(),
            sync_connection: String::new(),
            review_queue: true,
            sync_gateway_url: String::new(),
        };
        let paths = vec!["src/a.rs".to_string()];
        assert_eq!(ownership(&repo, &["me".into()], &paths), (vec![], 0));
    }
}

#[cfg(test)]
mod critique_tests {
    use super::*;

    /// The cut lands between files and names what fell off — the answer to a live report of a
    /// review saying "the diff was truncated mid-file … so I can't confirm", a guess about a tail
    /// it could have simply been told.
    #[test]
    fn a_cut_diff_ends_at_a_file_boundary_and_names_what_is_missing() {
        let one =
            "diff --git a/kept.rs b/kept.rs\n--- a/kept.rs\n+++ b/kept.rs\n@@ -1 +1 @@\n+kept\n";
        let two =
            "diff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ b/gone.rs\n@@ -1 +1 @@\n+gone\n";
        let three =
            "diff --git a/also.rs b/also.rs\n--- a/also.rs\n+++ b/also.rs\n@@ -1 +1 @@\n+also\n";
        let diff = format!("{one}{two}{three}");
        // A limit that lands INSIDE the second file.
        let (head, cut) = truncate_diff(&diff, one.len() + 20);
        assert!(cut);
        assert!(
            head.contains("+kept") && !head.contains("+gone"),
            "the cut lands between files, never inside one: {head}"
        );
        assert!(
            head.contains("2 more files not shown — gone.rs, also.rs"),
            "what fell off is named, not guessed at: {head}"
        );

        // One file larger than the whole budget: mid-file is unavoidable, and said.
        let big = format!(
            "diff --git a/big.rs b/big.rs\n--- a/big.rs\n+++ b/big.rs\n{}",
            "+x\n".repeat(50)
        );
        let (head, cut) = truncate_diff(&big, 60);
        assert!(cut);
        assert!(
            head.contains("MID-FILE"),
            "an unavoidable mid-file cut says so: {head}"
        );

        // Under the limit: untouched.
        let (whole, cut) = truncate_diff(&diff, 10_000);
        assert!(!cut);
        assert_eq!(whole, diff);
    }

    /// The anchor validator against the shapes a real diff throws: context and added lines count
    /// on the right side, deleted lines and deleted files do not, and the counter follows the
    /// hunk headers rather than running on.
    #[test]
    fn only_lines_the_diff_shows_on_the_right_side_take_a_comment() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 fn main() {
+    let x = 1;
     println!(\"hi\");
 }
@@ -10,2 +11,1 @@
-gone
-also gone
+kept
diff --git a/dead.rs b/dead.rs
--- a/dead.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-everything
-left
";
        let map = commentable(diff);
        let a = map.get("src/a.rs").expect("the surviving file is present");
        // First hunk: new lines 1..=4. Second hunk: only line 11 (the two deletions have no right side).
        assert_eq!(
            a.iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 11],
            "right-side lines only, following the hunk headers"
        );
        assert!(
            !map.contains_key("dead.rs"),
            "a deleted file has no right side to comment on"
        );
    }

    /// The model's format parses into drafts, and anchoring is decided by the caller against the
    /// diff — never taken from the model's own claim.
    #[test]
    fn a_drafted_review_is_parsed_and_anchored_against_the_diff_not_the_models_word() {
        let raw = "\
OVERALL: one real problem.
FILE: src/a.rs
LINE: 2
COMMENT: x is unused, and hides the real fix.
Also spans lines.
---
FILE: src/a.rs
LINE: 99
COMMENT: this one points at a line the diff does not show.
---
";
        let c = parse_critique(raw).expect("a well-formed answer parses");
        assert_eq!(c.overall, "one real problem.");
        assert_eq!(c.comments.len(), 2);
        assert!(
            c.comments[0].text.contains("Also spans lines."),
            "a comment keeps its later lines: {:?}",
            c.comments[0].text
        );
        // An answer with no OVERALL did not follow the format: nothing is offered from it.
        assert!(parse_critique("FILE: x\nLINE: 1\nCOMMENT: y").is_none());
    }

    /// The one transformation between "what the person kept" and "what gets posted": anchored
    /// comments ride as line comments, unanchored join the body named by their file, and a
    /// dropped comment was dropped by never being passed in.
    #[test]
    fn what_is_posted_is_exactly_what_was_kept() {
        let kept = vec![
            Draft {
                path: "src/a.rs".into(),
                line: 2,
                anchored: true,
                text: "on the line".into(),
            },
            Draft {
                path: "src/b.rs".into(),
                line: 0,
                anchored: false,
                text: "about the change".into(),
            },
        ];
        let (body, anchored) = assemble_post("overall note", &kept);
        assert_eq!(anchored.len(), 1, "only the anchored comment rides as one");
        assert_eq!(anchored[0].path, "src/a.rs");
        assert_eq!(anchored[0].line, 2);
        assert!(
            body.contains("**src/b.rs**: about the change"),
            "the unanchored comment travels in the body, named: {body}"
        );
        assert!(body.starts_with("overall note"), "the note leads: {body}");
    }

    /// The whole draft path against a stubbed GitHub and a stubbed model: the diff is read, the
    /// comments anchored against it, the draft stored — and found again from disk, which is what
    /// makes it survive a server restart.
    #[test]
    fn a_draft_is_anchored_stored_and_found_again() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();

        // A GitHub that serves one diff.
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,4 @@\n fn main() {\n+    let x = 1;\n     println!(\"hi\");\n }\n";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let served = diff.to_string();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read as _, Write as _};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{served}",
                        served.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // A model that reviews it: one comment the diff shows, one it does not.
        let claude = home.join("claude.sh");
        std::fs::write(
            &claude,
            "#!/bin/sh\nprintf 'OVERALL: one real problem.\\nFILE: src/a.rs\\nLINE: 2\\nCOMMENT: x is unused.\\n---\\nFILE: src/a.rs\\nLINE: 99\\nCOMMENT: nowhere.\\n---\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(
            &claude,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        std::env::set_var("SKEIN_CLAUDE_BIN", &claude);

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "demo", "source": "https://github.com/acme/thing.git",
            "source_tree": "", "store": "",
        }))
        .unwrap();
        let pr = crate::prq::Pr {
            number: 11,
            title: "t".into(),
            author: "someone".into(),
            url: String::new(),
            head_ref: "feat".into(),
            head_sha: "sha11".into(),
            base_ref: "main".into(),
            updated_at: String::new(),
            committed_at: String::new(),
            settled: true,
            draft: false,
            labels: Vec::new(),
            review_decision: String::new(),
            mergeable: None,
            additions: None,
            deletions: None,
            changed_files: None,
            checks: "none".into(),
            my_review: "none".into(),
            review_is_current: false,
            reasons: Vec::new(),
            lane: crate::prq::Lane::NeedsYou,
            box_name: String::new(),
        };

        let drafted = critique(&repo, "acme/thing", &pr).expect("the draft path works end to end");
        assert_eq!(drafted.comments.len(), 2);
        assert!(
            drafted.comments[0].anchored,
            "line 2 is in the diff, so the comment anchors"
        );
        assert!(
            !drafted.comments[1].anchored,
            "line 99 is not in the diff — offered for the body, not guessed onto a line"
        );

        // Found again from disk — the property that makes a draft survive a restart.
        let found = critiqued("demo", 11).expect("the stored draft is found");
        assert_eq!(found.head_sha, "sha11");
        assert_eq!(found.comments, drafted.comments);

        for key in [
            "SKEIN_HOME",
            "SKEIN_REVIEW_AI",
            "SKEIN_CLAUDE_BIN",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
    }

    /// The write path, whole: a moved head is refused with the fix named, and what reaches GitHub
    /// is exactly what was handed in — the kept comments on their lines, the unanchored one in the
    /// body, the commit id pinned to the head the draft read.
    #[test]
    fn posting_refuses_a_moved_head_and_sends_exactly_what_was_kept() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();

        let posted = home.join("posted.json");
        let posted_at = posted.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read as _, Write as _};
                let mut buf = [0u8; 16384];
                let n = stream.read(&mut buf).unwrap_or(0);
                let said = String::from_utf8_lossy(&buf[..n]).to_string();
                let head = said.lines().next().unwrap_or_default().to_string();
                let body = said
                    .split("\r\n\r\n")
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let answer = if head.contains("/user/teams") {
                    "[]".to_string()
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("POST") && head.contains("/reviews") {
                    // The thing under test: record exactly what skein said.
                    std::fs::write(&posted_at, &body).unwrap();
                    "{}".to_string()
                } else if body.contains("review-requested") {
                    r#"{"data":{"search":{"nodes":[{"number":11,"title":"t","url":"u",
                       "isDraft":false,"author":{"login":"someone"},"headRefName":"feat",
                       "headRefOid":"sha11","baseRefName":"main",
                       "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                       "latestReviews":{"nodes":[]},
                       "commits":{"nodes":[{"commit":{"committedDate":"2020-01-01T00:00:00Z"}}]}}]}}}"#
                        .to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"search":{"nodes":[]}}}"#.to_string()
                } else {
                    "{}".to_string()
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "crit", "source": "https://github.com/acme/thing.git",
            "source_tree": "", "store": "",
        }))
        .unwrap();
        let kept = vec![
            Draft {
                path: "src/a.rs".into(),
                line: 2,
                anchored: true,
                text: "on the line".into(),
            },
            Draft {
                path: "src/b.rs".into(),
                line: 0,
                anchored: false,
                text: "about the change".into(),
            },
        ];

        // The head the queue reports is sha11; a draft of some earlier commit must not post.
        let refused = post_critique(&repo, 11, "old-sha", "note", &kept)
            .expect_err("a moved head must refuse");
        assert!(
            refused.contains("has moved") && refused.contains("draft it again"),
            "the refusal names the fix: {refused}"
        );
        assert!(!posted.exists(), "nothing reached GitHub on a refusal");

        post_critique(&repo, 11, "sha11", "note", &kept).expect("a matching head posts");
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&posted).unwrap()).unwrap();
        assert_eq!(
            sent["commit_id"], "sha11",
            "pinned to the commit the draft read"
        );
        assert_eq!(sent["event"], "COMMENT");
        let comments = sent["comments"]
            .as_array()
            .expect("line comments ride along");
        assert_eq!(
            comments.len(),
            1,
            "only the anchored comment sits on a line"
        );
        assert_eq!(comments[0]["path"], "src/a.rs");
        assert_eq!(comments[0]["line"], 2);
        assert_eq!(comments[0]["side"], "RIGHT");
        let said_body = sent["body"].as_str().unwrap();
        assert!(
            said_body.starts_with("note"),
            "the overall note leads: {said_body}"
        );
        assert!(
            said_body.contains("**src/b.rs**: about the change"),
            "the unanchored comment travels in the body, named: {said_body}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "GH_TOKEN"] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
    }
}

#[cfg(test)]
mod drafted_body_tests {
    use super::*;

    /// The live leak, verbatim shape: narration before the marker stays with the model.
    #[test]
    fn a_models_narration_never_reaches_the_composer() {
        let raw = "Publishing \"the flag thing\" isn't right for a PR comment — let me rewrite \
                   that as feedback in the reviewer's own voice.\nCOMMENT:\nReload recovery could \
                   keep its flag in the backend instead of sessionStorage, so a new tab recovers too.";
        assert_eq!(
            drafted_body(raw),
            "Reload recovery could keep its flag in the backend instead of sessionStorage, so a \
             new tab recovers too."
        );
        // No marker: the whole answer is the comment — the old contract, still honoured.
        assert_eq!(drafted_body("  just the comment.  "), "just the comment.");
    }
}
