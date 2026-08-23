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
    serde_json::from_str(&text).ok()
}

fn store(repo_id: &str, s: &Summary) -> Result<(), String> {
    let path = cache_path(repo_id, s.number, &s.head_sha);
    let dir = path.parent().ok_or("no parent")?.to_path_buf();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    write_atomic(&path, &dir, &bytes)
}

// ───────────────────────────── the diff ─────────────────────────────

/// How much diff each stage is willing to read.
///
/// Truncation is stated to the model rather than hidden, so a large PR produces "I only saw part of
/// this" instead of a confident summary of its first 40KB. A partial read is a reason to keep your
/// attention, not to spend it.
const STAGE1_BYTES: usize = 40_000;
const STAGE2_BYTES: usize = 140_000;

/// The PR's diff, and whether it was cut short.
fn pr_diff(slug: &str, number: u64, limit: usize) -> Result<(String, bool), String> {
    Ok(truncate(&crate::prq::pr_diff_text(slug, number)?, limit))
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
pub fn summarise(repo: &Repo, slug: &str, pr: &Pr, identities: &[String], force: bool) -> Summary {
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
        None,
        Duration::from_secs(60),
    ) {
        Ok(raw) => raw,
        // The reason, not a disjunction. "the model call failed or timed out" was the whole of what
        // this said, for four different problems with four different fixes — and it named the
        // timeout first for a failure that came back in two seconds.
        Err(unread) => return Summary::unread(pr.number, &pr.head_sha, &unread.say()),
    };
    let Some(verdict) = parse_stage1(&raw) else {
        return Summary::unread(pr.number, &pr.head_sha, "skein read it but could not make sense of its own answer, so it is not vouching for one.");
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
            Some("claude-sonnet-5"),
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
    let prior = cached(repo_id, pr.number, &pr.head_sha)
        .filter(|s| s.depth != Depth::Unread)
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
    claude_oneshot_with(&prompt, Some("claude-sonnet-5"), Duration::from_secs(180))
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
- Output the comment body ONLY — no headings, no quotes around it, no explanation of what you wrote.

Their notes: {intent}

{context}"#,
        intent = intent,
        context = context(slug, pr, repo),
    );
    claude_oneshot_with(&prompt, Some("claude-sonnet-5"), Duration::from_secs(180))
        .ok_or_else(|| "no draft came back — the model call failed or timed out.".into())
}

#[cfg(test)]
mod tests {
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
