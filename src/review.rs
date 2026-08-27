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
    /// Why ownership could **not** be consulted for this summary — the repo was unreadable when it
    /// was computed ([`Ownership::Unreadable`], SKEIN-117). Empty when it was consulted, including
    /// when it was consulted and genuinely absent. Non-empty means `yours`/`others` are not claims
    /// — the pane must say "skein could not read the repo" rather than draw nothing owned — and
    /// the summary is never written to the cache, so a recovered mirror gets consulted on the
    /// next computation instead of being outvoted by a blind file for the life of the head.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ownership_unknown: String,
    /// Mechanical evidence from [`crate::contracts`] — what moved, found in the diff rather than
    /// reasoned about. Shown beside the brief because "the model thinks so" and "the diff says so"
    /// are different kinds of claim and you should be able to tell them apart.
    #[serde(default)]
    pub signals: Vec<crate::contracts::Signal>,
    /// Why there is no summary. Only set for [`Depth::Unread`], and written to be shown verbatim.
    pub unread_because: String,
    /// Why the commit that is there NOW was not read, when skein decided a round was not worth
    /// running (SKEIN-379). Empty on every other reading, which is nearly all of them.
    ///
    /// **This is the only thing standing between automatic rounds and unbounded spend.** Rounds run
    /// unasked, so something has to decide that a typo push is not a review; the owner's words are
    /// *"do new round when you think it is justified"*, and the judgement is made by the model that
    /// still remembers the argument rather than by a trigger list, which cannot tell a substantive
    /// reply from an acknowledgement.
    ///
    /// It rides beside a reading of an EARLIER commit, deliberately: the reader keeps the review
    /// they had, `Known::stale` still says it describes an older commit, and this says why skein
    /// chose not to replace it. Silence there would leave a row that looks current and is not.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub not_reread: String,
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
    /// The ONLY reason this row is unread is that the day's AUTOMATIC budget is spent. The
    /// machine-readable half of the refusal sentence: the pane detects it to render the read
    /// button prominently — the manual trigger the sentence invites, which is never budgeted
    /// (see [`Trigger`]). Omitted from the JSON when false, so older clients see no new key.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub budget_stopped: bool,
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
            budget_stopped: false,
            yours: Vec::new(),
            others: 0,
            ownership_unknown: String::new(),
            unread_because: because.to_string(),
            not_reread: String::new(),
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
        .map(|mut s| {
            // `cached`'s rule, and the third and last place that reads a `Summary` off disk
            // (SKEIN-292). What was written was computed WHEN it was written; a free answer
            // counted as a paid one is what let a page reload spend the whole day's allowance on
            // cache hits, and the invariant has to hold at every reader or the next caller
            // inherits the bug rather than the rule.
            s.computed = false;
            s
        })
        .filter(|s| s.depth != Depth::Unread)
}

fn store(repo_id: &str, s: &Summary) -> Result<(), String> {
    store_at(repo_id, &s.head_sha, s)
}

/// The same, filed under a commit that is not the one the reading describes.
///
/// **One caller, and it is the whole of why this exists** (SKEIN-379): when the round gate decides
/// a new commit is not worth reading, the reading of the EARLIER commit is filed under the new one
/// so the next poll is a cache hit. Without that the gate would be asked again every ten minutes
/// for a commit it has already judged, which is the spend it was built to stop.
///
/// The summary's own `head_sha` is left alone on purpose — it still names the commit it read, so
/// `Known::stale` goes on telling the truth and nothing has to remember to compare.
fn store_at(repo_id: &str, key_sha: &str, s: &Summary) -> Result<(), String> {
    let path = cache_path(repo_id, s.number, key_sha);
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
    /// The review skein holds for this pull request, riding the same bulk payload as the summary —
    /// the owner's ask (2026-08-24): "Show the critique as another section along with summary."
    /// Carried here so the pane renders the review section with NO per-row fetch; the per-row
    /// `/critique` GET stays for the keep/drop/post flow. Absent (and omitted from the JSON, so
    /// an older client simply never sees the key) only when nothing is drafted at all.
    ///
    /// **Whichever commit it read** (SKEIN-355). This used to be filtered to the row's head, and
    /// the doc here said a draft of an earlier commit "is not offered as if it read this one" —
    /// which is a labelling rule, and it was implemented by withholding. Reported live on #731:
    /// "it doesn't show the review at all, the text says review below but nothing exists … Is it
    /// because new commits were added that you dropped the review, I thought I was clear that
    /// should not happen, we even build a mechanism to post such reviews still." He is right about
    /// the mechanism: [`crate::prq::submit_review_with_comments`] re-anchors a drafted review
    /// against the live head by line text and folds what no longer matches into a body naming both
    /// commits, and [`post_critique`] stopped refusing a moved head in SKEIN-215. The filter here
    /// was the one thing that made that path unreachable from the pane.
    ///
    /// It is also the rule the reading beside it already follows: [`known`] deliberately keeps a
    /// SUMMARY whose commit has moved and marks it `stale`, "without it, everything skein knew
    /// about a pull request vanished from the pane the moment somebody pushed". One artefact of one
    /// visit — the summary and the review come out of the same model call
    /// ([`summarise_and_draft`]) — must not have two opposite rules.
    ///
    /// Which commit it read is [`Drafted::head_sha`], and the page compares it against the row's
    /// head to label it (`revDraftHeld` / `revDraftAtHead`, `src/web/index.html`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critique: Option<Critique>,
    /// The cheap flag beside the payload: is a drafted review here? `critique.comments.len()` is
    /// the count when it is. Says nothing about WHICH commit it read — that is
    /// [`Drafted::head_sha`], and reading this flag alone as "a review of the commit in front of
    /// you" is the mistake SKEIN-355 was the other half of.
    pub has_critique: bool,
    /// The drafted review reduced to the two facts a collapsed ROW draws — which commit it read,
    /// and how many comments it holds. Present exactly when [`Known::critique`] would be, and
    /// derived from it in [`Known::new`], so it can neither disagree with the review nor outlive
    /// it.
    ///
    /// It exists because [`Known::thin`] takes the review's PROSE out of the queue payload and the
    /// chip on the line still has to be drawable: `revReadyChip` needs the count, and
    /// `revDraftAtHead` (`src/web/index.html`) checks `head_sha` against the row's head to decide
    /// whether the chip says "review ready" or "review ready · earlier commit" — the labelling
    /// that replaced withholding the draft outright (SKEIN-355).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drafted: Option<Drafted>,
    /// Why there is NO drafted review, when a reading of this head exists and a review does not
    /// (SKEIN-275).
    ///
    /// The reason already existed and could not be reached: [`note_critique_tried`] writes it to
    /// `critique-tried.json` keyed `number-sha`, and [`worth_critiquing`] was the only reader —
    /// the loop it gates. So a row could sit draftless for the life of a head with the reason on
    /// disk and nothing able to say it, which is indistinguishable from a draft nobody ever asked
    /// for.
    ///
    /// Empty (and omitted from the JSON) in the two cases where it would be a claim rather than a
    /// record: a review IS drafted at this head, and nothing was ever attempted at it. "Never
    /// attempted" and "attempted and refused" are different answers and the page says which
    /// (`revNoDraftWhy`, `src/web/index.html`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub critique_because: String,
}

/// What a queue row says about a drafted review it is not carrying: the commit, the count, and
/// whether it has already been posted.
///
/// Deliberately NOT a smaller `Critique`. A second serialisation of one record is how the drafted
/// review and the summary came apart (SKEIN-243); these are scalars ABOUT a record, computed
/// once in [`Known::new`], and they cannot be mistaken for the review itself.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Drafted {
    /// The commit the review was drafted against.
    pub head_sha: String,
    /// How many comments it holds. Zero is a real answer — "nothing to flag" is a review somebody
    /// paid for — so the chip is earned by the review existing, not by this being non-zero.
    pub comments: usize,
    /// When skein posted this draft to GitHub, or empty (and omitted) if it never did — the row's
    /// half of [`Critique::posted`], so a COLLAPSED row can say "already posted" without the
    /// review's prose (SKEIN-364).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub posted_at: String,
    /// When the draft was written — [`Critique::written_at`], carried for the same reason: the
    /// page's floor for "this may already be on GitHub" compares it against the timestamps of the
    /// review threads YOU opened, and a collapsed row has to be able to ask that too.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub written_at: String,
}

impl Known {
    /// The one place a reading and the review beside it are assembled.
    ///
    /// Every derived field — `has_critique`, `drafted` — is computed here and nowhere else, so
    /// there is exactly one rule for what they mean. Three call sites used to spell
    /// `has_critique: critique.is_some()` for themselves.
    /// `tried` is the repo's critique-tried notes, read ONCE by the caller: [`known`] walks every
    /// pull request in the queue, and a file read per row would put the whole queue's worth of
    /// them on the payload path. `head_sha` is the ROW's head, not the summary's — for a stale
    /// reading the two differ, and the question being answered is about the commit in front of the
    /// reader.
    fn new(
        summary: Summary,
        stale: bool,
        critique: Option<Critique>,
        tried: &std::collections::BTreeMap<String, String>,
        head_sha: &str,
    ) -> Known {
        // Only where there is no review OF THIS COMMIT to show: the note is what happened on the
        // way to a draft, and a draft that landed at this head is the answer to the same question.
        //
        // **At this head, not merely present** (SKEIN-355). Now that a draft of an EARLIER commit
        // rides along, `critique.is_some()` stopped being the question this field is asking: a row
        // can hold last commit's review AND a note saying why nothing was drafted for the one in
        // front of the reader, and those are two different facts. The doc above already said the
        // rule in these words — "a review IS drafted at this head" — and only the code disagreed.
        let at_head = critique.as_ref().is_some_and(|c| c.head_sha == head_sha);
        let critique_because = match at_head {
            true => String::new(),
            false => tried
                .get(&format!("{}-{head_sha}", summary.number))
                .cloned()
                .unwrap_or_default(),
        };
        Known {
            summary,
            stale,
            has_critique: critique.is_some(),
            drafted: critique.as_ref().map(|c| Drafted {
                head_sha: c.head_sha.clone(),
                comments: c.comments.len(),
                posted_at: c.posted.as_ref().map(|p| p.at.clone()).unwrap_or_default(),
                written_at: c.written_at.clone(),
            }),
            critique,
            critique_because,
        }
    }

    /// The same reading with the PROSE taken out — what a queue ROW draws, and nothing else.
    ///
    /// Measured on the owner's fleet (2026-08-25): `GET /review/summaries` answered 153,381 bytes
    /// for thirty-nine stored readings, every one carrying its full brief, its signals and the
    /// whole drafted review — none of which a collapsed row draws. Reproduced locally at 155,167
    /// bytes against 12,055 for the same thirty-nine
    /// (`tests/server.rs::the_review_queue_payload_can_be_asked_for_rows_instead_of_prose`).
    ///
    /// The same measurement took 10.42 s, and that part is NOT this: locally the full payload is
    /// serialised in about four milliseconds, and with the queue's micro-cache cold both shapes
    /// wait the same for `prq::queue` to hear back from GitHub. The wait is SKEIN-291. What this
    /// buys is the bytes, and the time the connection carrying them is occupied.
    ///
    /// **It is the same struct and the same `Serialize`, with named fields emptied.** Not a second
    /// row type: two hand-maintained serialisations of one record is exactly what dropped the
    /// drafted review out of step with its summary (SKEIN-243), and a `Row` struct beside `Known`
    /// would be that mistake with a new name. Anything added to [`Summary`] therefore appears in
    /// both shapes until somebody decides otherwise — and
    /// `the_row_shape_carries_only_what_a_row_draws` fails the day it does, which is where that
    /// decision gets made.
    ///
    /// What goes, and where the page reads it (all of it behind the fold, in `revDetail` and
    /// `revDraftSection`): `detail` (`src/web/index.html:4625`), `signals` (`:4621`), `yours` and
    /// `others` (`:4614-4616`), `ownership_unknown` (`:4611`), and the whole `critique`
    /// (`:4643-4660`). What stays is the line, the flags, the depth and its reason, the head, and
    /// `drafted` — the row's own vocabulary.
    pub fn thin(mut self) -> Known {
        self.summary.detail = String::new();
        self.summary.signals = Vec::new();
        self.summary.yours = Vec::new();
        self.summary.others = 0;
        self.summary.ownership_unknown = String::new();
        self.critique = None;
        self
    }
}

/// One reading, and the review drafted at the same head — the shape [`known`] answers in bulk,
/// for the route that reads a single pull request.
///
/// It exists so the rule "the drafted review at THIS head" is written once. The bulk payload has
/// carried the draft since the model call was merged, and the single-PR route answered a bare
/// [`Summary`], so a reading somebody had just asked for came back with no mention of the review
/// produced in the same breath — the page learned about it a refresh later. The client compensated
/// by merging the two itself, which is a second implementation of this line, and a second
/// implementation of a payload rule is what dropped `serial` from a workflow twice.
///
/// `stale` is false by construction: the caller has just computed a reading FOR `head_sha`, so
/// there is no older vintage to disclose. `known` keeps its own arm for the cached-and-moved case.
pub fn known_at(repo_id: &str, summary: Summary, head_sha: &str) -> Known {
    // Unfiltered (SKEIN-355): whichever commit the drafted review read, it travels, and
    // [`Drafted::head_sha`] says which. See [`Known::critique`] for why withholding it was wrong.
    let critique = critiqued(repo_id, summary.number);
    Known::new(summary, false, critique, &critique_tried(repo_id), head_sha)
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
    // Once for the whole queue, not once per row.
    let tried = critique_tried(repo_id);
    for (number, head_sha) in prs {
        // The drafted review, whichever vintage IT turns out to be — off disk, costing nothing,
        // and now the same rule as the summary beside it (SKEIN-355). This used to be filtered to
        // `head_sha`, which is how a review that was bought, complete and postable vanished from
        // the pane the moment somebody pushed — the exact thing the paragraph above says keeping
        // the stale SUMMARY exists to prevent, applied to one half of one model call and not the
        // other. [`Drafted::head_sha`] carries which commit it read, and the page labels it.
        let critique = critiqued(repo_id, *number);
        if let Some(summary) = cached(repo_id, *number, head_sha) {
            out.insert(
                *number,
                Known::new(summary, false, critique, &tried, head_sha),
            );
            continue;
        }
        if let Some(summary) = newest_for(repo_id, *number) {
            out.insert(
                *number,
                Known::new(summary, true, critique, &tried, head_sha),
            );
        }
    }
    out
}

/// What skein already holds for ONE pull request, off disk, costing nothing — the prose behind a
/// thinned row, fetched when the row is opened.
///
/// **Why this is not `GET /review/:n/summary` as it stands.** That route computes: it goes through
/// [`visit`], which serves a reading cached at THIS head first (`src/review.rs:1570-1573`) and
/// otherwise falls through the scope and budget doors to a model call. So it answers an expanded
/// row correctly in the common case and wrongly in the one the queue deliberately keeps: a reading
/// of an EARLIER commit. [`known`] hands that reading over marked `stale`
/// (`src/review.rs:364-366`), and `visit` cannot — its cache lookup is keyed on the current head,
/// so it misses, and expanding the row would either spend a model call nobody asked for or come
/// back `unread`. [`known_at`] could not patch that either: it hard-codes `stale: false` and
/// filters the draft to the head it was given.
///
/// So this is [`known`] for one pull request, delegating rather than repeating it, and a miss is
/// an honest unread answer rather than a 404 — a row that opens onto a transport error is how a
/// reading that exists comes to look like a pull request nobody read.
pub fn held(repo_id: &str, number: u64, head_sha: &str) -> Known {
    known(repo_id, &[(number, head_sha.to_string())])
        .remove(&number)
        .unwrap_or_else(|| {
            // Nothing on disk at all, so there is no note to read either: the tried-notes are
            // written on the way to a DRAFT, and this arm is the case where no reading exists to
            // have drafted beside.
            Known::new(
                Summary::unread(
                    number,
                    head_sha,
                    "skein holds no reading of this pull request yet.",
                ),
                false,
                None,
                &std::collections::BTreeMap::new(),
                head_sha,
            )
        })
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

// ───────────────────────────── the day's analysis budget ─────────────────────────────
//
// FLEET-WIDE, per UTC day, counted HERE — at the model call — and nowhere else. One unit is one
// pull request ANALYSED: a `(number, head_sha)` that actually reached a model, whether that visit
// produced a one-liner, a brief, or a brief plus a drafted review. A cache hit is zero. The
// ceiling and its default (100) live on `Config::review_reads_per_day` — the owner's own number:
// "not more than 100 PRs a day (cache misses, actual analysis)".
//
// The client used to keep this budget (`REV_SUM_AUTO`/`revSumAuto` in index.html), and it was
// wrong twice over, in ways nobody should reintroduce:
//
// 1. **It counted REQUESTS, not model calls.** A request answered from the disk cache costs
//    nothing, so a page reload re-requested the six newest rows, took six free cache hits, and the
//    allowance was gone — rows seven onward were never read, on any reload, for ever. A limit on
//    spending has to count spending, and only the server knows whether a request reached a model.
// 2. **A button press reset it.** Every approve and set-aside reloaded the pane, and the reload
//    handed out a fresh allowance — one approve authorised six more reads (measured 6→12), thirty
//    acts a day up to 180 stage-1 calls, with no ceiling, while the reload-only reviewer got
//    nothing. A budget the client holds is a budget any client action can refill.
//
// So the ledger lives on disk beside the per-repo summary dirs, keyed by day with the per-repo
// attribution kept inside it (cheap, and it says where the money went), and every path that is
// about to spend a model call — the summary read, the drafted review, asked-for or background —
// checks the same number. One budget, not two. The file self-prunes: writing today's count drops
// every other day's key, so it never grows past one entry.
//
// Unlocked read-modify-write, deliberately: two analyses racing can overshoot the ceiling by the
// number in flight, which is bounded by the client's small parallelism and costs cents — a lock
// here would buy precision nothing needs.

/// The one ledger, above the per-repo dirs — the budget is the fleet's, not a repo's.
fn spend_path() -> PathBuf {
    crate::config::skein_home()
        .join("review")
        .join("reads-spent.json")
}

/// Today's key. UTC, so the budget resets at the same moment for everyone and a test can name a
/// day instead of sleeping through midnight.
fn utc_day() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// The ceiling, from settings. See `Config::review_reads_per_day` for the default and its why.
fn reads_per_day() -> u32 {
    crate::config::load_config().review_reads_per_day
}

/// day → repo → analyses. The inner map is attribution, the budget is the day's SUM.
type SpendLedger = std::collections::BTreeMap<String, std::collections::BTreeMap<String, u32>>;

fn spend_ledger() -> SpendLedger {
    fs::read_to_string(spend_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// How many pull requests the whole fleet has analysed on `day`.
fn reads_spent(day: &str) -> u32 {
    spend_ledger()
        .get(day)
        .map(|repos| repos.values().sum())
        .unwrap_or(0)
}

/// Count one analysed pull request against `day` under `repo_id`, and drop every other day's key
/// while here — the file is this day's tally, not a history, and pruning on write is what keeps
/// it one entry for ever.
fn note_read_spent(repo_id: &str, day: &str) {
    let path = spend_path();
    let mut all = spend_ledger();
    all.retain(|k, _| k == day);
    *all.entry(day.to_string())
        .or_default()
        .entry(repo_id.to_string())
        .or_insert(0) += 1;
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
        if let Ok(bytes) = serde_json::to_vec_pretty(&all) {
            let _ = write_atomic(&path, dir, &bytes);
        }
    }
}

/// The honest-absence sentence a budget-stopped row carries, shown verbatim by the pane — and it
/// INVITES the manual trigger, per the owner: "When limit is hit, surface and ask me to manually
/// trigger these." The machine-readable marker beside it is [`Summary::budget_stopped`], which
/// the pane uses to render the read button prominently.
fn budget_spent_because(spent: u32, budget: u32) -> String {
    format!(
        "today's automatic reading budget is spent ({spent}/{budget}) — press read to analyse \
         this one now; the budget resets at midnight UTC."
    )
}

/// Who wants this pull request analysed. **The budget's whole boundary**, so it is a named type
/// rather than a bool a call site can get backwards.
///
/// The owner's rule, verbatim: "Limit is only for automatic stuff, manually I can invoke as many
/// as I want." The ceiling is on skein's INITIATIVE, never on the person — so an [`Asked`] visit
/// (the read button, the draft button, a `force` re-read, any user-initiated route) neither
/// checks the counter nor increments it: a manual call must never be refused for budget, and
/// must never eat the automatic allowance. Only [`Unasked`] work — the background pass, the
/// pane's pump asking for rows nobody clicked — pays from and is stopped by the day's ledger.
///
/// A future route defaults the safe way round: the HTTP layer treats a request as `Unasked`
/// unless it explicitly carries the asked marker, so forgetting the marker gates a button rather
/// than un-gating a sweep.
///
/// [`Asked`]: Trigger::Asked
/// [`Unasked`]: Trigger::Unasked
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A person pressed something. No check, no count.
    Asked,
    /// Skein's own idea. Checked against the ledger, and counted into it.
    Unasked,
}

/// Is the day's budget already spent — for UNASKED work? The one question every model-spending
/// path asks, so the answer cannot drift between them. An [`Trigger::Asked`] visit is never over
/// budget by definition: the ceiling is on skein's initiative, not on the person.
fn over_budget(trigger: Trigger, day: &str) -> Option<String> {
    if trigger == Trigger::Asked {
        return None;
    }
    let budget = reads_per_day();
    let spent = reads_spent(day);
    (spent >= budget).then(|| budget_spent_because(spent, budget))
}

/// Count one analysed pull request — if this was skein's own initiative. An asked visit never
/// touches the ledger: the person's calls must not eat the automatic allowance.
fn note_spent_if_unasked(trigger: Trigger, repo_id: &str, day: &str) {
    if trigger == Trigger::Unasked {
        note_read_spent(repo_id, day);
    }
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

/// Three seconds of wall clock per KB of diff — how long the merged summary-and-review call gets.
///
/// **Why it is not one number any more.** It was a flat `300s`, and the sentence a large pull
/// request produced said exactly what was wrong with that: "`claude` was still going after 300s. A
/// larger diff needs longer than this call allows" — skein naming the cause and then doing nothing
/// with it (SKEIN-392). The merged call is the largest thing this module asks for, and what makes
/// it slow is what it was handed, so what it was handed is what sets the clock.
///
/// Both ends are derived rather than chosen. The floor is what every call used to get, so no diff
/// gets *less* time than before. The ceiling is what this rule gives the largest diff that can
/// arrive — [`CRITIQUE_BYTES`], the truncation just above — so it moves when that moves, and there
/// is no waiting for an answer to a question nobody can ask.
const MERGED_SECS_PER_KB: u64 = 3;
const MERGED_FLOOR: Duration = Duration::from_secs(300);

fn merged_budget(diff_len: usize) -> Duration {
    let ceiling = (CRITIQUE_BYTES as u64 / 1000) * MERGED_SECS_PER_KB;
    let want = (diff_len as u64 / 1000) * MERGED_SECS_PER_KB;
    Duration::from_secs(want.clamp(MERGED_FLOOR.as_secs(), ceiling))
}

/// What a merged call that did not come back leaves worth trying.
#[derive(Debug, PartialEq, Eq)]
enum AfterMerged {
    /// Ask again, smaller — the summary-only ladder over a fraction of the diff. The reader ends
    /// with a line they can act on instead of a sentence telling them to read it themselves.
    Narrow,
    /// Nothing narrower would help: the binary is not there, the sandbox is not answering, the CLI
    /// refused. A second call fails the same way, only faster, and spends the reader's minute.
    Stop,
}

/// The line is [`crate::ai::Unread`]'s own, drawn again here for the same reason it draws it for the
/// refusal cache: a timeout is a fact about this diff and the next attempt may differ, while every
/// other refusal is a fact about the setup and will not.
///
/// **Exhaustive, with no wildcard arm**, which is this module's neighbour's discipline and not a
/// style choice: `ai.rs` matches `Unread` without `_` everywhere and says why — "a new variant
/// stops this match compiling". A wildcard here would answer `Stop` for a variant nobody had
/// thought about, and the variant most likely to be added next is another way of saying "this was
/// too big", which is the one that must answer `Narrow`. The failure would be silent and would
/// look exactly like the bug SKEIN-392 fixed. (Found by skein's own sweep, on this commit.)
fn after_merged(unread: &crate::ai::Unread) -> AfterMerged {
    use crate::ai::Unread;
    match unread {
        Unread::Slow(_) => AfterMerged::Narrow,
        Unread::Missing { .. }
        | Unread::Unreachable { .. }
        | Unread::AbsentInSandbox { .. }
        | Unread::Refused { .. }
        | Unread::Silent => AfterMerged::Stop,
    }
}

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

/// What consulting CODEOWNERS answered — three ways, not two, and the third is the point
/// ([`crate::health::Level`]'s register).
///
/// Both empty-handed answers widen a summary's scope to everything, which is the safe direction
/// and unchanged. What they must NOT share is the sentence — "this repo has no CODEOWNERS" is the
/// repo's own answer, "skein could not read this repo" is an admission — or the cache: a summary
/// narrowed while the repo was unreadable froze "yours: none" for the life of its head commit,
/// and a mirror that recovered a minute later could never correct it (SKEIN-117).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    /// CODEOWNERS was read: these changed paths are yours, and this many are not.
    Owned { yours: Vec<String>, others: usize },
    /// The repo was read and genuinely has no CODEOWNERS (or nothing in it parses to a rule).
    /// No narrowing exists — full depth for everything, the normal case.
    NoCodeowners,
    /// The repo could not be read, so whether narrowing exists is unknown. Carries why, verbatim.
    Unreadable(String),
}

impl Ownership {
    /// The pair a [`Summary`] carries: paths that are yours, and how many are not. Empty-and-zero
    /// for both empty-handed answers — the widening is identical; only the sentence (and the
    /// cacheability) differs, and those read the enum itself.
    fn split(&self) -> (Vec<String>, usize) {
        match self {
            Ownership::Owned { yours, others } => (yours.clone(), *others),
            _ => (Vec::new(), 0),
        }
    }

    /// Why ownership could not be consulted — `None` when it was, including when it was consulted
    /// and genuinely does not exist.
    fn unread_why(&self) -> Option<&str> {
        match self {
            Ownership::Unreadable(why) => Some(why),
            _ => None,
        }
    }
}

/// Which changed paths are yours, and how many are not — or that no such answer exists, and
/// which of the two reasons why. Callers must not read an empty `yours` as "none of this is
/// yours", and must not remember anything decided on [`Ownership::Unreadable`].
pub fn ownership(repo: &Repo, identities: &[String], paths: &[String]) -> Ownership {
    let tree = match crate::repos::Tree::open_telling(repo) {
        Ok(tree) => tree,
        Err(why) => return Ownership::Unreadable(why),
    };
    let Some(co) = codeowners::load(|p| tree.read(p)) else {
        return Ownership::NoCodeowners;
    };
    let (mine, theirs) = co.partition(paths, identities);
    Ownership::Owned {
        yours: mine.into_iter().map(str::to_string).collect(),
        others: theirs.len(),
    }
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

fn stage1_prompt(pr: &Pr, owned: &Ownership, diff: &str, cut: bool) -> String {
    // Both empty-handed answers widen scope to the whole change — the safe direction, unchanged —
    // but the sentence says which one happened: "the repo has none" is the repo's answer, and
    // "skein could not look" is an admission the brief must not dress up as the other (SKEIN-117).
    let scope = match owned {
        Ownership::Owned { yours, others } if !yours.is_empty() => format!(
            "The reviewer owns these paths: {}. {} other changed path(s) are outside their ownership — mention them only in passing.",
            yours.join(", "),
            others
        ),
        Ownership::Unreadable(why) => format!(
            "Whether the reviewer owns any of this is unknown — the repo could not be read to consult CODEOWNERS ({why}). Treat the whole change as in scope."
        ),
        _ => String::from("This repo has no CODEOWNERS, or none of it is attributed — treat the whole change as in scope."),
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
/// Burst control, not a budget: the budgets are the SCOPE — pull requests somebody asked you to
/// review or that you opened yourself, in a repo you switched reading on for (the owner's own
/// answers, SKEIN-185 and SKEIN-265) — and the day's spend ceiling
/// (`Config::review_reads_per_day`). This only stops a queue that has been quiet all week from
/// firing thirty model calls in one minute when it finally settles.
///
/// Counted in LINES REPORTED, not in pull requests, so a row that produced a summary and a review
/// spends two of it: a pass works through two merged visits, three summary-only ones. That is the
/// behaviour that was already here — the merged call landed before this constant was re-read — and
/// it is left alone deliberately, because the thing it bounds is a burst and a stack of ten
/// therefore takes five passes rather than four.
const READ_PER_PASS: usize = 3;

/// Read the pull requests waiting on you, in the repos you asked skein to read, with nobody
/// watching.
///
/// **Three things bound this, and two of them are the owner's answers rather than my guesses:**
///
/// * a repo reads nothing until `read_prs` is switched on for it, so the feature costs exactly
///   nothing on a fleet nobody has opted in;
/// * only pull requests somebody asked you to review, **and the ones you opened yourself** — the
///   owner's decision of 2026-08-25 (SKEIN-265), on a fleet where every open pull request was
///   theirs and the surface therefore did nothing at all. Being mentioned is still not a request to
///   read. The scope is the first budget; the day's spend ceiling (`Config::review_reads_per_day`,
///   enforced inside [`summarise`] and the critique drafter) is the second, and this pass obeys the
///   same counter as every button press — one budget, not two. An authored row costs ONE unit for
///   summary and review together, because [`summarise`] merges them into a single call;
/// * not a draft, and not already read at this head. **Not settled** — the hour a branch had to
///   stand still before skein would read it was removed on the owner's instruction (2026-08-24)
///   once the budget became the money guard and re-anchoring made a draft against a moving head
///   postable; `worth_reading` no longer asks, and `read_waiting`'s own test reads an unsettled
///   branch on purpose.
///
/// Returns what it read, for the server's log.
pub fn read_waiting() -> Vec<String> {
    if !summaries_enabled() {
        return Vec::new();
    }
    let mut read = Vec::new();
    // Every switched-on repo's queue, read BEFORE anything is spent. The ordering below is
    // fleet-wide and cannot be applied to a repo whose queue has not been fetched yet, so the
    // gathering and the spending are two passes rather than one (SKEIN-276).
    let mut queues = Vec::new();
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
        queues.push((repo, queue));
    }
    // The budget is spent from the TOP of the pane, lane by lane, in the pane's own order —
    // not in the queue's transport order (`prq::newest_first`, number descending). Two lanes
    // are read now (see [`worth_a_visit`]) and each keeps ITS own sort, mirrored field for
    // field from `src/web/index.html`:
    //
    //   * **your move** first, oldest-waiting first ([`waited_since`], from `revWaitedSince`
    //     and the needs-you lane sort). Somebody else is blocked on these, so they take the
    //     day's money before anything of yours does — widening the scope must not push a
    //     colleague's review request behind a stack you opened this morning.
    //   * **their move** second, most-recently-updated first — `index.html`'s own rule for
    //     that lane, and its reason: "for your own PRs, what moved most recently is the right
    //     question".
    //
    // What the reader spends the day's money on must be the rows the person will read first,
    // or the ceiling starves exactly the pull requests at the top of the pane.
    //
    // **Across every repo at once, not repo by repo** (SKEIN-276). The sort used to sit inside
    // the `for repo in load_repos()` loop, so the order it produced was only the order *within*
    // one repo and the outer order was the order of `repos.json`. Every pass restarts at the
    // first repo in that file, so a repo with more unread rows than the day's ceiling meant the
    // repos below it were never reached at all that day — including a colleague's review
    // request, which is the exact row this ordering exists to protect. One list, one sort.
    let mut waiting: Vec<(&Repo, &crate::prq::Queue, &Pr)> = queues
        .iter()
        .flat_map(|(repo, queue)| queue.prs.iter().map(move |pr| (repo, queue, pr)))
        .collect();
    waiting.sort_by(|(_, _, a), (_, _, b)| {
        let rank = |pr: &Pr| u8::from(!matches!(pr.lane, crate::prq::Lane::NeedsYou));
        rank(a).cmp(&rank(b)).then_with(|| {
            if matches!(a.lane, crate::prq::Lane::NeedsYou) {
                waited_since(a).cmp(waited_since(b))
            } else {
                b.updated_at.cmp(&a.updated_at)
            }
        })
    });
    for (repo, queue, pr) in waiting {
        let identities = std::iter::once(queue.viewer.clone()).collect::<Vec<_>>();
        // The one doorway both halves share. Checked first so rows in lanes nobody reads cost
        // no disk scans at all.
        if !worth_a_visit(pr) {
            continue;
        }
        let read_it = worth_reading(&repo.id, pr);
        let had_draft = critiqued(&repo.id, pr.number).is_some_and(|c| c.head_sha == pr.head_sha);
        if read_it {
            if read.len() >= READ_PER_PASS {
                return read;
            }
            // Never `force`: a reading already on disk for this head is the answer, and asking
            // again would spend a model call to be told what skein already knows. Where the
            // review is yours to give, `summarise` drafts it INSIDE this same visit, off the
            // one diff download — see `draft_alongside`.
            let summary = summarise(repo, &queue.slug, pr, &identities, false, Trigger::Unasked);
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
            // Read, but BLIND: ownership could not be consulted, so `summarise` served this
            // one without caching it (SKEIN-117 — a stored blind summary would freeze
            // "yours: none" for the life of the head). Left unnoted, this pass would re-buy
            // the same degraded answer every ten minutes for ever; noted, only the pass
            // stands down. The tried-notes gate nothing but this loop — the pane's read
            // button and per-row requests go nowhere near them — so a recovered mirror is
            // consulted the next time anyone asks, and a new commit is a new key that reads
            // afresh either way.
            if summary.computed && !summary.ownership_unknown.is_empty() {
                note_tried(
                    &repo.id,
                    pr.number,
                    &pr.head_sha,
                    &format!("read blind — {}", summary.ownership_unknown),
                );
            }
            // Only what actually cost something is reported. A cache hit is not news, and a
            // line per cache hit would bury the ones that are.
            if summary.computed {
                read.push(format!("{}: read #{}", repo.id, pr.number));
            }
        }
        // The reader's second half — the door for a row whose summary is already on disk at
        // this head while its review is not (the visit above covers the rest, and
        // `worth_critiquing` re-checked here sees anything it just drafted or noted). Same
        // doorway reading uses — [`worth_a_visit`] at the top of the loop — and no settle
        // hour: the daily budget is the money guard now (owner decision, 2026-08-24), and
        // re-anchoring made a moving head postable.
        //
        // It re-runs the ONE reading rather than drafting beside the old summary (SKEIN-263).
        // It used to call a standalone drafter, which cost the same single unit and left the
        // row carrying a summary from one reading and a review from another, with the diff
        // downloaded twice and nothing making the two agree about what they saw. Forced,
        // because the summary on disk is exactly what must not be handed back here; the
        // replacement is written by the same call that wrote the review.
        //
        // Two ways a row arrives here, both real: a summary cached before the merged call
        // existed, and one summarised while the review was not yours to give — mentioned only
        // — that has since become yours.
        let draft_it = worth_critiquing(&repo.id, pr, &queue.viewer);
        if draft_it {
            if read.len() >= READ_PER_PASS {
                return read;
            }
            // Every failure mode is written down inside the visit — `summarise_and_draft`
            // notes the draft as tried on a spent call, `note_tried` below covers a summary
            // that could not be made — so a row that cannot be drafted is not re-bought every
            // ten minutes. Nothing to match on here: what happened is on disk.
            let again = visit(
                repo,
                &queue.slug,
                pr,
                &identities,
                true,
                Trigger::Unasked,
                Review::IfYours,
            );
            if matches!(again.depth, Depth::Unread) && again.computed {
                note_tried(&repo.id, pr.number, &pr.head_sha, &again.unread_because);
            }
        }
        // Reported off what is now on disk, whichever door drafted it.
        if !had_draft && critiqued(&repo.id, pr.number).is_some_and(|c| c.head_sha == pr.head_sha) {
            read.push(format!("{}: drafted a review for #{}", repo.id, pr.number));
        }
    }
    read
}

/// When this pull request started waiting on YOU — the your-move lane's sort key, ascending.
///
/// Mirrored, on purpose and field for field, from the pane's own `revWaitedSince` and the
/// needs-you lane sort in `src/web/index.html`: the head moving after your review is the moment
/// the PR came back to you (so `committed_at` when your review exists and is no longer current),
/// and `updated_at` is the honest fallback. Oldest first is the order on screen. Do not invent a
/// second order here — the background reader spends the day's budget by this key precisely so
/// that what gets read unasked is what sits at the top of the pane.
fn waited_since(pr: &Pr) -> &str {
    let moved = !pr.my_review.is_empty() && pr.my_review != "none" && !pr.review_is_current;
    if moved && !pr.committed_at.is_empty() {
        &pr.committed_at
    } else {
        &pr.updated_at
    }
}

/// Did YOU open this pull request? The queue's own answer: `Reason::Author` is the row that came
/// back from the `author:<you>` search (`src/prq.rs:700`), so this needs no viewer to ask.
///
/// [`worth_critiquing`] asks the same question from the other end (`pr.author == viewer`), because
/// there it already has the viewer in hand.
fn yours(pr: &Pr) -> bool {
    pr.reasons.contains(&crate::prq::Reason::Author)
}

/// Is this pull request in a lane skein reads at all, unasked? **The one doorway** — the background
/// pass, the reading predicate and the merged summary-and-review call all ask it, so the lane rule
/// cannot drift between them.
///
/// Two lanes, and the second is the owner's decision of 2026-08-25 (SKEIN-265), asked as "should an
/// authored pull request be read, drafted, or both, and in which lane" and answered **"both, in
/// waiting, on the same call"**:
///
///   * [`Lane::NeedsYou`] — somebody is waiting on your review. The lane this pass was built for.
///   * [`Lane::Waiting`] — **only when you opened it**. Not the whole lane: a PR you already
///     decided on sits here too, and it has had your attention already. `src/prq.rs:1190-1196`
///     files every pull request you authored here and nowhere else, which is why the fleet this was
///     reported on — ten open PRs, every one the owner's — got nothing at all from a reader that
///     only read `NeedsYou`.
///
/// [`Lane::NotReady`] and [`Lane::Archived`] stay out: not-ready is its author still changing the
/// answer, archived is you saying it will not move.
///
/// A **draft** is refused whichever lane it is in, and that is not about authorship: it is the
/// author saying the change is not finished. It has to be checked here rather than left to the
/// lane, because a draft you opened yourself is `Lane::Waiting`, not `Lane::NotReady` — the
/// yours-or-decided arm at `src/prq.rs:1190` outranks the draft arm below it.
///
/// [`Lane::NeedsYou`]: crate::prq::Lane::NeedsYou
/// [`Lane::Waiting`]: crate::prq::Lane::Waiting
/// [`Lane::NotReady`]: crate::prq::Lane::NotReady
/// [`Lane::Archived`]: crate::prq::Lane::Archived
fn worth_a_visit(pr: &Pr) -> bool {
    !pr.draft
        && match pr.lane {
            crate::prq::Lane::NeedsYou => true,
            crate::prq::Lane::Waiting => yours(pr),
            crate::prq::Lane::NotReady | crate::prq::Lane::Archived => false,
        }
}

/// Is the review of this pull request yours to give — and therefore worth drafting, unasked?
///
/// A narrower question than [`worth_reading`]'s, because the spend is bigger: a summary tells you
/// about a PR you are involved in for any reason, a drafted review presumes you will be the one
/// reviewing. Yours to give means asked (personally or through a team — a team request IS a
/// review request, same rule as `worth_reading`), already reviewing (you acted once and the PR is
/// still open), or your own pull request. Being mentioned is somebody talking *about* you, not a
/// request to review, and must never cost the model call a draft is.
///
/// It says nothing about the LANE — [`worth_a_visit`] is the one place that does, and every caller
/// asks both. That division is what the authored case turned on: "yours to give" has always
/// included your own pull request, so the thing that kept a review off every PR the owner opened
/// was never this predicate but the `Lane::NeedsYou` test its callers wrapped it in.
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

/// **The scope**: is this a pull request skein may read on its own at all?
///
/// Split out of [`worth_reading`] so the two questions it used to answer together can be asked
/// apart. This one is about the pull request and nothing else — who asked, and which lane — and
/// its answer does not change when skein reads or fails to read something. [`worth_reading`] adds
/// the thrift on top: do not buy a reading you already have, do not re-buy one that just failed.
///
/// [`unasked_scope`] needs exactly this half. Asking it the whole question refused the forced
/// re-read [`read_waiting`]'s draft door makes — the summary being on disk is the REASON that door
/// opens, so `cached(..).is_none()` there is a no rather than a yes.
///
/// Two ways in, and being **mentioned** is neither: somebody talking about you is not a request to
/// read. A team request IS a review request; the queue's own filter says so, and dropping those
/// would silently exclude exactly the pull requests the team query was added to find.
fn in_reading_scope(pr: &Pr) -> bool {
    let asked = pr.reasons.iter().any(|r| {
        matches!(
            r,
            crate::prq::Reason::Reviewer
                | crate::prq::Reason::Reviewed
                | crate::prq::Reason::Team(_)
        )
    });
    // The lane, and the draft rule, in one place for every caller.
    (asked || yours(pr)) && worth_a_visit(pr)
}

/// Is this a pull request skein should read for you, unasked?
///
/// Two ways in, and the lane each arrives in is [`worth_a_visit`]'s question, not this one's:
///
///   * somebody **asked** you to review it, and it is your move;
///   * **you opened it**, and it is therefore in the waiting lane. The owner's decision of
///     2026-08-25 (SKEIN-265): a summary of your own pull request is what the agent that wrote it
///     actually changed, which is exactly what its author does not know. It arrives on the SAME
///     model call as the drafted review — see `summarise`'s `draft_due` — so widening the scope
///     buys both halves for one unit of the day's budget, not two.
///
/// Being **mentioned** is still not a way in: somebody talking about you is not a request to read.
fn worth_reading(repo_id: &str, pr: &Pr) -> bool {
    in_reading_scope(pr)
        // NO settle gate, any more. It required an hour of quiet before reading — the owner's
        // decision (2026-08-24) removed it: the daily spend ceiling (`Config::review_reads_per_day`,
        // enforced in `summarise` and the critique drafter) is now THE money guard, and re-anchoring
        // by line text (SKEIN-214/215) made drafts against a moving head postable, which is what
        // made the hour obsolete. A head that moves again simply becomes a new cache key.
        // A reading of THIS head. One of an earlier commit is kept and shown as such (SKEIN-184),
        // but it is not a reason to leave the current one unread.
        && cached(repo_id, pr.number, &pr.head_sha).is_none()
        // Tried at this head and could not be read. Not for ever: a new commit is a new key, and
        // asking for it by hand goes nowhere near this.
        && !read_tried(repo_id).contains_key(&format!("{}-{}", pr.number, pr.head_sha))
}

/// May skein read this pull request **on its own**? The scope guard, enforced at the model call —
/// the same place, and for the same reason, as [`over_budget`].
///
/// The whole rule, in one sentence: **if you pressed it, it is free and unconditional; if skein
/// decided to read it, that happens only in a repo you switched read-ahead on for, only within
/// [`worth_reading`]'s scope, and it is counted against the day.** There is no third case — the
/// pane's own pump and the ten-minute pass are both "skein decided", and both arrive here as
/// [`Trigger::Unasked`].
///
/// It used to live in [`read_waiting`] alone (SKEIN-242), which made it a rule the *background*
/// obeyed rather than a rule about the repo: `GET /review/:n/summary` with no `asked` marker read
/// anything the caller named — a repo with read-ahead switched off, a pull request whose only
/// reason is that somebody mentioned you — and charged the day's ledger for it. The budget check
/// was moved server-side because "a budget the client holds is a budget any client action can
/// refill" (`src/review.rs:462-478`); the same argument is what puts the SCOPE here, because a
/// scope the client holds is a scope any client can widen.
///
/// [`read_waiting`] still asks [`worth_reading`] itself, before spending a GitHub call on a row
/// this would refuse. That is a shortcut, not a second rule: this is the doorway.
fn unasked_scope(repo: &Repo, pr: &Pr, trigger: Trigger) -> Option<String> {
    // A person pressing a button is not skein's initiative, so no scope applies — the same
    // boundary `over_budget` draws, and the owner's own rule for it: "Limit is only for automatic
    // stuff, manually I can invoke as many as I want."
    if trigger == Trigger::Asked {
        return None;
    }
    if !repo.read_prs {
        return Some(
            "skein does not read this repo on its own — press \"read it\" to read this one now, \
             or turn on \"read ahead\" for the repo to have skein read what is waiting on you and \
             what you opened."
                .into(),
        );
    }
    // The SCOPE half of `worth_reading` and not its thrift half: "skein already has this reading"
    // is a reason not to buy another, not a reason this pull request is out of bounds — and the
    // forced re-read behind `read_waiting`'s draft door exists precisely because the summary is
    // already on disk.
    if !in_reading_scope(pr) {
        return Some(
            "this is not one skein reads on its own — that is the pull requests somebody asked \
             you to review, and the ones you opened. Press \"read it\" to read this one now."
                .into(),
        );
    }
    None
}

/// Read this pull request, and draft the review too **where skein would have drafted it anyway**.
///
/// The conservative half of the pair. [`Review::IfYours`] means the review is drafted only when
/// [`worth_critiquing`] says yes, and its first no is "one is already drafted at this head" — so a
/// re-read through here KEEPS a review the reader may have vetted, kept comments from and dropped
/// comments from. That is the whole reason this is the default and
/// [`re_read_replacing_the_review`] is not.
pub fn summarise(
    repo: &Repo,
    slug: &str,
    pr: &Pr,
    identities: &[String],
    force: bool,
    trigger: Trigger,
) -> Summary {
    visit(repo, slug, pr, identities, force, trigger, Review::IfYours)
}

/// Read this pull request again and draft a NEW review, **replacing whatever is drafted at this
/// head**.
///
/// The other half of the pair, and it differs from [`summarise`] in exactly one argument. That is
/// the finding SKEIN-293 is about: since the drafter was merged (SKEIN-263) "re-read" and "review
/// the code" both come here, force a reading, download the diff once and spend one model call —
/// and on a row that already has a draft, one keeps it and the other throws it away. Nothing in
/// either name said so. The pair is named for the difference now, so the call site has to choose
/// it deliberately.
///
/// **Never reached except by a person who has been told what it costs them.** The pane asks first
/// where there are kept/dropped decisions to lose (the owner's decision, 2026-08-25: one control,
/// warning through the pane's own receipt rather than a native dialog). Nothing in the background
/// comes here — [`read_waiting`] and the pane's pump both go through [`summarise`].
///
/// `force` and [`Trigger::Asked`] are not choices the caller gets, because neither is meaningful
/// here. A cached reading returns from [`visit`] before anything is drafted, so a redraft that
/// honoured the cache would be a press that did nothing; and the day's ceiling is on skein's own
/// initiative, which this is by definition not.
pub fn re_read_replacing_the_review(
    repo: &Repo,
    slug: &str,
    pr: &Pr,
    identities: &[String],
) -> Summary {
    visit(
        repo,
        slug,
        pr,
        identities,
        true,
        Trigger::Asked,
        Review::Always,
    )
}

/// Whether this visit must come back with a drafted review as well as a summary.
///
/// The distinction exists because a review can be **asked for directly** — the panel's "draft
/// again" — on a pull request [`worth_critiquing`] would say no to: one already drafted at this
/// head (that is what "again" means), or one where the review was never yours to give. Asking is
/// its own authority, exactly as [`Trigger::Asked`] is for the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Review {
    /// Draft one if the review is yours to give and has not been drafted at this head. The
    /// background pass and the read button.
    IfYours,
    /// Draft one, whatever [`worth_critiquing`] thinks. Somebody pressed for a review.
    Always,
}

/// **One reading, whichever half you asked for.** The summary and the review are different tasks
/// needing different mindsets, but they are never separate surfaces and they are always wanted
/// together — so they are never two analyses (the owner's framing, 2026-08-25, SKEIN-263).
///
/// Every path that spends a model call on a pull request comes through here, and the ones that
/// want a review come through with [`Review::Always`]. There is no second drafter: there used to
/// be — `draft_critique`, its own prompt, its own diff download — reached by the panel's "draft
/// again" and by the pass's draft-only door, and it is what let the summary and the review of one
/// commit come from two different readings that never had to agree about what they saw.
fn visit(
    repo: &Repo,
    slug: &str,
    pr: &Pr,
    identities: &[String],
    force: bool,
    trigger: Trigger,
    review: Review,
) -> Summary {
    let said = spend_a_visit(repo, slug, pr, identities, force, trigger, review);
    // **A reading skein bought and could not make is written down HERE**, not only in
    // [`read_waiting`] (SKEIN-253).
    //
    // The tried-note is what stops a failure being re-bought, and it used to be written by the
    // background pass alone — so the pane's own pump, which is `Trigger::Unasked` and IS charged,
    // spent a budget unit per row per reload against a `claude` that fails instantly and for free.
    // Thirty rows and three reloads is the day's ceiling gone on zero summaries, and then every row
    // reads "today's automatic reading budget is spent (100/100)".
    //
    // Two conditions, and they are the same ones the pass already used. **`computed`**: only a
    // spent model call is worth not repeating — a diff that would not download costs one HTTP call
    // to retry, and noting it pins a bad network minute to the head sha as a permanent error row.
    // **`Unasked`**: a person pressing "read it" is saying they think it will work now, and what
    // they are told must never become a standing state. A new commit is a new key either way.
    if trigger == Trigger::Unasked && said.computed && matches!(said.depth, Depth::Unread) {
        note_tried(&repo.id, pr.number, &pr.head_sha, &said.unread_because);
    }
    said
}

/// The visit itself. Split from [`visit`] so that every way it can come back Unread passes the one
/// place that writes the tried-note, rather than each of the dozen returns below remembering to.
#[allow(clippy::too_many_arguments)]
/// **Every pull request skein is reading right now, and since when.**
///
/// It exists because a reading is the one thing skein does that takes most of a minute and shows
/// nothing while it runs. Reported live, twice, and the second time as the diagnosis rather than
/// the symptom: "click on reread or redraft doesn't really produce new review and summary", then
/// "even if it is doing work, I am unable to see, the fact that I am feeling that means the UX is
/// not good enough" (SKEIN-333).
///
/// **Why the server holds this and not the page.** A reading outlives the browser that asked for
/// it: it runs in a blocking task and finishes whether or not anybody is still looking. A marker
/// kept in the page is lost to a reload, a repo switch, or a second tab — so the reader who
/// reloads at 20 seconds is shown a calm row for the remaining 15 and concludes nothing happened,
/// which is the whole complaint. Held here, any page that asks is told what is in flight and when
/// it started.
///
/// **Every reading, not only pressed ones.** The owner's answer when asked whether skein's own
/// background reads should show too: yes, any read in flight shows. So this is registered inside
/// [`spend_a_visit`], the one door every model-spending path already comes through, rather than at
/// the route — which would have seen presses and missed the pump.
///
/// Keyed `repo_id#number`, the same key the page keys its rows on.
fn reading_now() -> &'static std::sync::Mutex<std::collections::HashMap<String, ReadingNow>> {
    static READING: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ReadingNow>>,
    > = std::sync::OnceLock::new();
    READING.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// One reading in flight. Named for the question it answers, because [`Reading`] in this module
/// is already the diff a visit read.
#[derive(Debug, Clone, Serialize)]
pub struct ReadingNow {
    pub repo_id: String,
    pub number: u64,
    /// Wall clock at the moment the purchase began, so a page that arrives late can still say how
    /// long it has been running. Milliseconds since the epoch, which is what the page's own clock
    /// speaks (`ageNow`).
    pub started_ms: i64,
    /// Did a person press for this, or is it skein's own initiative? The page draws the two
    /// differently — a press gets the full counter, the pump a quieter marker — and neither is
    /// inferable from the row itself.
    pub asked: bool,
}

/// Registered while a reading runs, removed however it ends.
///
/// A guard rather than a pair of calls: [`spend_a_visit`] returns from a dozen places and may
/// panic inside the model call, and an entry that outlives its reading is a row that says
/// "reading…" for ever with nothing able to clear it. `Drop` is the only thing that covers every
/// exit, including the ones added later.
struct ReadingGuard(String);

impl Drop for ReadingGuard {
    fn drop(&mut self) {
        reading_now()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

impl ReadingGuard {
    fn begin(repo_id: &str, number: u64, trigger: Trigger) -> ReadingGuard {
        let key = format!("{repo_id}#{number}");
        reading_now()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key.clone(),
                ReadingNow {
                    repo_id: repo_id.to_string(),
                    number,
                    started_ms: now_ms(),
                    asked: trigger == Trigger::Asked,
                },
            );
        ReadingGuard(key)
    }
}

/// What is being read right now, for the route that answers the page.
pub fn readings() -> Vec<ReadingNow> {
    let mut out: Vec<ReadingNow> = reading_now()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect();
    // Oldest first: the one that has been running longest is the one somebody is waiting on.
    out.sort_by_key(|r| (r.started_ms, r.number));
    out
}

/// One reading that has FINISHED, as the cockpit's live stream carries it back.
///
/// **Why a reading travels on the stream rather than on the reply to the request that asked for
/// it** (SKEIN-366). The cockpit is served over HTTP/1.1, browsers cap that at six connections per
/// origin, and a reading is a model call that takes tens of seconds — so a request left open until
/// its answer is ready is a connection held for tens of seconds. `REV_ASKED_PARALLEL = 10` in
/// `src/web/index.html` means one pressed stack read alone exceeds the cap, and everything else the
/// page does — the upload, the health tick, a second stack's progress — then queues in the BROWSER
/// behind it. Measured under Playwright against a build of `d48a4ce`: an unrelated
/// `GET /api/health` took 12 ms with three reads in flight, 12,814 ms with six, and 34,438 ms with
/// ten.
///
/// The fix is not a smaller width — the owner's rule is that a read he asks for is not rationed,
/// and a smaller number moves the cliff rather than removing it. It is to stop spending a
/// connection per reading: the page starts one with a request that returns in milliseconds and
/// picks the answer up here, on the `EventSource` it already holds. N concurrent readings then cost
/// one connection instead of N.
///
/// Carries the whole answer, not a nudge to go and fetch it: a "reading #12 is done" event would
/// put the N requests back, one per reading, which is the thing being removed.
#[derive(Debug, Clone, Serialize)]
pub struct ReadingDone {
    pub repo_id: String,
    pub number: u64,
    /// The same body `GET /review/:n/summary` answers with, serialised once here so this module
    /// does not have to name the route's response type. `None` when the read could not be made at
    /// all, in which case `error` says why.
    pub summary: Option<serde_json::Value>,
    /// Empty on success. The sentence the failed request would have carried in its body.
    pub error: String,
    /// Which queue the answer was built from — `fresh` or `remembered`, the same distinction the
    /// `x-skein-queue` header draws on the request-shaped route. A reading delivered over the
    /// stream has no headers, so the fact travels in the payload or not at all.
    pub queue: String,
    /// When that queue was taken, RFC 3339 — `x-skein-queue-as-of`'s value.
    pub as_of: String,
}

/// Where finished readings are announced. One sender, every open board subscribed.
///
/// Separate from `crate::stream`'s fleet channel on purpose: a reading is not a fact about the
/// fleet, it does not belong in a `Tick`, and the fleet producer's own capacity is sized for a
/// board that falls behind on box rows. The route merges the two streams onto one `EventSource`.
fn readings_said() -> &'static tokio::sync::broadcast::Sender<ReadingDone> {
    static SAID: std::sync::OnceLock<tokio::sync::broadcast::Sender<ReadingDone>> =
        std::sync::OnceLock::new();
    // Sixty-four, against a browser that reads its `EventSource` on every frame: a reading takes
    // tens of seconds to produce, so this is deep enough for every read a fleet could finish while
    // one tab was descheduled. A subscriber that still falls behind is told (`Lagged`), and the
    // page's own reconciler — an in-flight poll that finds a read no longer running — picks up
    // whatever the gap swallowed.
    SAID.get_or_init(|| tokio::sync::broadcast::channel(64).0)
}

/// Listen for readings as they finish.
pub fn subscribe_readings() -> tokio::sync::broadcast::Receiver<ReadingDone> {
    readings_said().subscribe()
}

/// Say that a reading finished. Dropped when nobody is listening, which is not an error: a reading
/// runs to completion and is written to disk whether or not a board is open to hear about it.
pub fn announce_reading(done: ReadingDone) {
    let _ = readings_said().send(done);
}

/// Milliseconds since the epoch. The page's clock speaks the same unit.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn spend_a_visit(
    repo: &Repo,
    slug: &str,
    pr: &Pr,
    identities: &[String],
    force: bool,
    trigger: Trigger,
    review: Review,
) -> Summary {
    // Somebody pressed "read it". Whatever the model refused with last time, they are entitled to
    // find out whether it still refuses — a standing refusal must never make a button do nothing.
    //
    // A PERSON pressed, though: the background pass forces a re-read of its own accord now (the
    // draft-only door, `read_waiting`), and an unattended pass has no standing to clear a refusal
    // nobody has seen. That rule was already written down beside the old standalone drafter; this
    // is where it belongs, because this is the only door left.
    if force && trigger == Trigger::Asked {
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
    // MAY skein read this at all, unasked, before asking whether it can AFFORD to. Scope first:
    // a repo the owner excluded must not be told what the fleet's budget is doing, and a row out
    // of scope is refused on a spent day and on a fresh one alike. `computed` stays false and the
    // ledger is untouched — nothing was read, so nothing is charged. See [`unasked_scope`].
    if let Some(because) = unasked_scope(repo, pr, trigger) {
        return Summary::unread(pr.number, &pr.head_sha, &because);
    }
    // **Tried at this commit already, and it cost a model call.** Free, and BEFORE the budget
    // check, because the honest answer to "why is this row not summarised" is what the model said —
    // reporting the day's ceiling instead would hide the thing that is actually broken behind a
    // number the person cannot act on.
    //
    // Read on `Unasked` only. That is the boundary this note has always had and the reason it is
    // safe to widen it from the pass to every unattended path: "read it" goes nowhere near it, so
    // a standing failure can never make a button do nothing (`ai::forget_refusal`'s rule). And
    // `computed` stays false, so saying this costs the day nothing.
    if trigger == Trigger::Unasked {
        if let Some(why) = read_tried(&repo.id).get(&format!("{}-{}", pr.number, pr.head_sha)) {
            return Summary::unread(
                pr.number,
                &pr.head_sha,
                &format!(
                    "{why} — skein already spent a reading on this commit and will not buy \
                     another by itself. Press \"read it\" to try again."
                ),
            );
        }
    }
    // Enforced where the money is spent, and only there. Everything above cost nothing — a cache
    // hit was already served, a switched-off repo asked for nothing — and everything below leads
    // to a model call. The check sits before the GitHub reads too: refusing after fetching the
    // diff would spend HTTP on an answer already known. `computed` stays false — nothing was
    // asked, so the day's budget is not charged for saying so, and the button can ask tomorrow.
    let day = utc_day();
    if let Some(because) = over_budget(trigger, &day) {
        let mut said = Summary::unread(pr.number, &pr.head_sha, &because);
        // The machine-readable half of the sentence's invitation: the pane shows the read
        // button prominently on this flag, and that button comes back `Asked` — un-budgeted.
        said.budget_stopped = true;
        return said;
    }
    // **From here down this reading is a purchase**, and every return above cost nothing: a cache
    // hit was served, a switch was off, the scope or the day's ceiling refused. So this is where
    // the row starts saying "reading…" and, when the guard drops, stops (SKEIN-333).
    //
    // Registered here rather than at the route so that skein's own background reads are announced
    // on the same terms as a press — the owner's answer when asked: any read in flight shows.
    let _reading = ReadingGuard::begin(&repo.id, pr.number, trigger);
    let paths = changed_paths(slug, pr.number);
    let owned = ownership(repo, identities, &paths);

    // ONE download, every reader. The RAW diff is fetched once and every consumer truncates its
    // own view of it: the scanner and stage 2 at [`STAGE2_BYTES`], stage 1 at [`STAGE1_BYTES`],
    // and the merged summary-and-review call at [`CRITIQUE_BYTES`]. Fetching per consumer would
    // cost up to three round trips to be told the same bytes.
    let raw = match crate::prq::pr_diff_text(slug, pr.number) {
        Ok(raw) => raw,
        Err(e) => {
            return Summary::unread(
                pr.number,
                &pr.head_sha,
                &format!("its diff could not be read: {e}"),
            )
        }
    };
    if raw.trim().is_empty() {
        return Summary::unread(
            pr.number,
            &pr.head_sha,
            "GitHub returned an empty diff for this PR.",
        );
    }
    let (full, deep_cut) = truncate_diff(&raw, STAGE2_BYTES);
    let signals = crate::contracts::scan(&full);

    // Where the review is yours to give, summary and review are ONE model call over the one
    // download — the owner's decision (2026-08-24): "combine summary with critique review …
    // merging summary and critique into one model call since they are always done together". The
    // merged call runs on the critique's (stronger) model because it is writing review comments;
    // rows needing a summary only — involved, but the review is not yours — keep the cheap
    // two-stage path below. Either visit is ONE unit of the day's budget: the unit is the pull
    // request analysed, not the number of things the analysis produced.
    //
    // The lane test is [`worth_a_visit`]'s, shared with the reader — which is what puts a pull
    // request you OPENED down this path rather than the cheap summary-only one (SKEIN-265, the
    // owner's "both, in waiting, on the same call"). Split, the two halves would cost two model
    // calls and two budget units for the one row: the reader's summary here, and the second door
    // in `read_waiting` drafting the review afterwards.
    let draft_due = match review {
        // Somebody pressed for a review. `worth_critiquing` is not consulted at all: it answers
        // "would skein draft this unasked", and both of its nos — already drafted at this head,
        // never yours to give — are things a person is entitled to overrule. This is the same
        // boundary `Trigger::Asked` draws for the money.
        Review::Always => true,
        Review::IfYours => identities
            .first()
            .is_some_and(|viewer| worth_a_visit(pr) && worth_critiquing(&repo.id, pr, viewer)),
    };
    if draft_due {
        // Counted the moment the model is about to be asked — a call that then fails still spent.
        note_spent_if_unasked(trigger, &repo.id, &day);
        return summarise_and_draft(repo, pr, &owned, &signals, &raw, trigger);
    }

    // One analysed pull request = one unit, counted at the call (a call that then fails still
    // spent — same boundary `computed` draws below). Stage 2, when stage 1 earns it, is the
    // second half of the SAME unit: the budget counts pull requests analysed, and one that needed
    // explaining must not cost double what a boring one did.
    note_spent_if_unasked(trigger, &repo.id, &day);
    // Stage 1, and stage 2 when stage 1 earns it. Extracted because this is now reached from TWO
    // places: here, and from the merged call when it runs out of time (`summarise_and_draft`) —
    // and both must be the same reading, not two ladders that drift apart.
    summarise_in_stages(repo, pr, &owned, &signals, &full, deep_cut)
}

/// The summary-only ladder: one cheap call over the first [`STAGE1_BYTES`], and a second, longer
/// one over [`STAGE2_BYTES`] when the first says this pull request needs explaining.
///
/// **Two callers, deliberately.** [`visit`] takes this path for a row whose review is not yours to
/// give. [`summarise_and_draft`] falls back to it when the merged summary-and-review call runs out
/// of time (SKEIN-392): the merged call is the largest thing this module asks for — the whole
/// [`CRITIQUE_BYTES`] diff, a summary AND a review with line comments in one answer — and the
/// reader who was told it was too big has nothing to do with a button that makes exactly that call
/// again. This is the reading skein gave before the two were merged, so its rules are already
/// written and already hold; what it does not produce is the review, and the tried-note the caller
/// leaves is what puts "no review — read again" on the row.
///
/// It never counts a budget unit of its own. The unit is the pull request analysed and the caller
/// counted it before the first call; a narrower second attempt at the same pull request is the
/// same unit, on the same rule that makes stage 2 free after stage 1.
fn summarise_in_stages(
    repo: &Repo,
    pr: &Pr,
    owned: &Ownership,
    signals: &[crate::contracts::Signal],
    full: &str,
    deep_cut: bool,
) -> Summary {
    let (diff, cut) = truncate(full, STAGE1_BYTES);
    let raw = match crate::ai::claude_oneshot_telling(
        &stage1_prompt(pr, owned, &diff, cut),
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
    for s in signals {
        if !flags.contains(&s.kind) {
            flags.push(s.kind.clone());
        }
    }
    let expand = verdict.expand || !signals.is_empty();

    let (yours, others) = owned.split();
    let mut summary = Summary {
        number: pr.number,
        head_sha: pr.head_sha.clone(),
        depth: if expand { Depth::Expanded } else { Depth::Line },
        // Reached only by having run the model.
        computed: true,
        budget_stopped: false,
        line: verdict.line.clone(),
        detail: String::new(),
        flags,
        signals: signals.to_vec(),
        yours,
        others,
        ownership_unknown: owned.unread_why().unwrap_or_default().to_string(),
        unread_because: String::new(),
        not_reread: String::new(),
    };

    if expand {
        // The whole diff and a longer budget, because this is the pass whose output you will
        // actually decide from. The stronger model is named here rather than in the env so a pinned
        // `$SKEIN_AI_MODEL` still overrides both stages together.
        match claude_oneshot_with(
            &stage2_prompt(pr, &verdict, &summary.yours, signals, full, deep_cut),
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
    // Cached ONLY when everything that would narrow it was actually consulted. A summary
    // computed while the repo was unreadable answered "yours: none" for lack of sight, not as a
    // fact — stored, it carried that blindness for the life of this head, and a mirror that
    // recovered a minute later could never correct it (SKEIN-117, the durable half of the bug).
    // So it is served — the person still gets their summary now — and NOT written down: the next
    // computation consults whatever can be read then. That recomputation is a second unit of the
    // day's budget, and that is correct — the first unit bought a degraded answer, not this one.
    // It does not loop the background pass either: `read_waiting` writes a tried-note at this
    // head for a blind reading, and the tried-notes gate only the pass — the pane's requests go
    // nowhere near them, which is exactly the door a recovered mirror is consulted through.
    if summary.ownership_unknown.is_empty() {
        let _ = store(&repo.id, &summary);
    }
    summary
}

/// The merged visit: ONE model call over the one downloaded diff, producing the summary AND the
/// drafted review — for rows whose review is yours to give (the owner's decision, 2026-08-24; the
/// caller has already gated on [`worth_critiquing`] and counted the budget unit).
///
/// Runs on the critique's (stronger) model, because it is writing review comments a person will
/// post under their name; the cheap model keeps only the summary-only rows. Strict on both halves,
/// in the module's usual direction: a summary that did not follow the format is [`Depth::Unread`]
/// and no draft is stored (a PR that could not be summarised earns no draft — same rule as the
/// two-stage path); a summary that parsed WITHOUT a review section stores the summary and notes
/// the draft as tried — the call was spent, and leaving it unnoted would re-buy the whole visit
/// every pass.
fn summarise_and_draft(
    repo: &Repo,
    pr: &Pr,
    owned: &Ownership,
    signals: &[crate::contracts::Signal],
    raw_diff: &str,
    trigger: Trigger,
) -> Summary {
    // The review's byte budget, not the summary's: the review is the reader that cannot say
    // anything about a file it never saw, so the merged call gets the most diff either consumer
    // would have been given.
    let (diff, cut) = truncate_diff(raw_diff, CRITIQUE_BYTES);
    let spent_unread = |why: &str| {
        // The draft is noted as tried too: this visit WAS the draft attempt, and without the note
        // the pass's draft-only door would buy the same failure again next pass.
        note_critique_tried(&repo.id, pr.number, &pr.head_sha, why);
        let mut said = Summary::unread(pr.number, &pr.head_sha, why);
        said.computed = true;
        said
    };
    // **This reading is a conversation, not a question** (SKEIN-393), and it is THE PULL REQUEST'S
    // conversation rather than this call's (SKEIN-376). The review is asked to account for its own
    // coverage on a second turn, and a second turn needs the first one to have been named; naming
    // it after the pull request instead of after the moment means the next round resumes what this
    // one left rather than paying to be told the same change again.
    let (talk, at) = conversation_of(repo, pr.number, &pr.head_sha);
    // **Is this round worth running at all** (SKEIN-379). Only when skein has read this pull
    // request before — there is nothing to judge a first reading against — and only unasked: a
    // press is never rationed, which the owner has said twice.
    let earlier = match trigger {
        Trigger::Asked => None,
        _ => previous(&repo.id, pr.number, &pr.head_sha).filter(|s| s.depth != Depth::Unread),
    };
    let gate = earlier
        .as_ref()
        .map(|s| gate_paragraph(&s.head_sha, &pr.head_sha))
        .unwrap_or_default();
    let answer = match crate::ai::claude_in_conversation(
        &merged_prompt(pr, owned, signals, &diff, cut, &gate),
        review_model(Some("claude-sonnet-5")).as_deref(),
        merged_budget(diff.len()),
        &talk,
        &at,
    ) {
        Ok(answer) => answer,
        // **Out of time is not the end of the reading** (SKEIN-392). This call carries the whole
        // [`CRITIQUE_BYTES`] diff and is asked for a summary AND a review with line comments over
        // it; when it does not come back, the thing to do is the reading skein gave before the two
        // were merged — smaller diff, cheaper model, and no review. The reader gets a line.
        //
        // Not a second budget unit: the unit is the pull request analysed, and the caller counted
        // it before the first call. Same rule that makes stage 2 free after stage 1.
        Err(unread) if after_merged(&unread) == AfterMerged::Narrow => {
            // The call WAS the draft attempt and it is gone, so the absence is written down: the
            // pass's draft-only door will not re-buy the same timeout every ten minutes, and the
            // row carries "no review — read again" with this as its reason (SKEIN-371).
            note_critique_tried(
                &repo.id,
                pr.number,
                &pr.head_sha,
                &format!(
                    "{} What follows is a shorter read with no review in it — press draft to \
                     spend a whole call on the review alone.",
                    unread.say()
                ),
            );
            let (full, deep_cut) = truncate_diff(raw_diff, STAGE2_BYTES);
            let mut narrower = summarise_in_stages(repo, pr, owned, signals, &full, deep_cut);
            if narrower.depth == Depth::Unread {
                // BOTH attempts are the answer. The shorter one's own sentence alone would send the
                // reader to look at a 60-second call, which was never the thing that was slow.
                narrower.unread_because = format!(
                    "{} A shorter read of the first {}KB did not get there either: {}",
                    unread.say(),
                    STAGE1_BYTES / 1000,
                    narrower.unread_because,
                );
            }
            return narrower;
        }
        Err(unread) => return spent_unread(&unread.say()),
    };
    // **The gate said no, and that is the whole answer.** Checked before the format parse, because
    // a one-line refusal is deliberately not in the review's format and would otherwise be read as
    // a model that ignored its instructions.
    if let Some(because) = earlier.as_ref().and_then(|_| no_round(&answer)) {
        let mut kept = earlier.expect("only reachable with an earlier reading");
        // Filed under the commit that was NOT read, so the next poll is a cache hit and the gate is
        // asked once per commit rather than every ten minutes — which is the spend it exists to
        // stop. `head_sha` is left naming the commit this reading actually describes, so
        // `Known::stale` goes on being true without anyone having to remember to compare.
        kept.not_reread = match because.is_empty() {
            true => format!("skein did not re-read {}.", short(&pr.head_sha)),
            false => format!("skein did not re-read {} — {because}", short(&pr.head_sha)),
        };
        // The turn was spent, so the day is charged for it. It is one cheap turn against a whole
        // round, which is the trade, but a ledger that under-counts is worse than no ledger.
        kept.computed = true;
        let _ = store_at(&repo.id, &pr.head_sha, &kept);
        return kept;
    }
    let Some((verdict, detail, critique)) = parse_merged(&answer) else {
        return spent_unread(
            "skein read it but could not make sense of its own answer, so it is not vouching for one.",
        );
    };
    // The second turn. Only ever adds; see [`sweep`]. Still ONE budget unit — the unit is the pull
    // request analysed, the same rule that makes stage 2 free after stage 1 — so nothing is counted
    // here.
    let critique = sweep(&talk, &at, critique);
    // The scanner escalates and never clears — same rule as the two-stage path, see there.
    let mut flags = verdict.flags.clone();
    for s in signals {
        if !flags.contains(&s.kind) {
            flags.push(s.kind.clone());
        }
    }
    let expand = verdict.expand || !signals.is_empty();
    if expand && detail.trim().is_empty() {
        // Flagged as needing your judgement and not explained: unread, never presented as
        // summarised — the same inversion the two-stage path refuses when stage 2 fails.
        return spent_unread(&format!(
            "this one needs explaining ({}) and skein could not do it — read it yourself.",
            verdict.line
        ));
    }
    let (yours, others) = owned.split();
    let summary = Summary {
        number: pr.number,
        head_sha: pr.head_sha.clone(),
        depth: if expand { Depth::Expanded } else { Depth::Line },
        computed: true,
        budget_stopped: false,
        line: verdict.line.clone(),
        detail,
        flags,
        signals: signals.to_vec(),
        yours,
        others,
        ownership_unknown: owned.unread_why().unwrap_or_default().to_string(),
        unread_because: String::new(),
        not_reread: String::new(),
    };
    // Same rule and same reason as the two-stage path (see the comment there): a summary whose
    // ownership could not be consulted is served but never cached, so a recovered mirror is
    // consulted on the next computation instead of being outvoted by a blind file (SKEIN-117).
    if summary.ownership_unknown.is_empty() {
        let _ = store(&repo.id, &summary);
    }
    match critique {
        // Vetted against the very diff the model read, exactly as the standalone drafter vets.
        //
        // Stored even when the summary above was computed blind, deliberately: an unknown
        // ownership only ever WIDENED the review's scope — the safe direction, more scrutiny
        // rather than less — so the draft is not degraded the way the summary's ownership claim
        // is, and the draft-once-per-head discipline is the money guard worth keeping.
        Some(drafted) => {
            if let Err(fail) = vet_and_store_critique(repo, pr, drafted, &diff, cut) {
                note_critique_tried(&repo.id, pr.number, &pr.head_sha, &fail);
            }
        }
        // The summary parsed and the review did not: the call was spent, so the absence is noted
        // — the button still drafts on request, same as every other tried note.
        None => note_critique_tried(
            &repo.id,
            pr.number,
            &pr.head_sha,
            "the merged answer carried no usable review section — press draft to try again.",
        ),
    }
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
// 3. **A review of one commit says so when it lands on another.** The draft carries the head sha
//    it read AND each comment's line text; a moved head is not refused (that refusal was a
//    treadmill on any actively-pushed PR) — the comments re-anchor against the live head by their
//    text, the displaced fold into the body, and the posted record names both commits.

/// One drafted review comment. `line` is a NEW-side line number; `anchored` says the diff actually
/// shows that line, which is GitHub's own condition for accepting the comment there.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Draft {
    pub path: String,
    pub line: u64,
    pub anchored: bool,
    pub text: String,
    /// The content of the line this comment sits on, exactly as the drafted diff showed it with
    /// the `+`/` ` marker stripped. This is the durable anchor [`crate::prq::re_anchor`] searches
    /// the LIVE head's diff for when the branch has moved since drafting — a line NUMBER is a
    /// coordinate into one commit's diff and dies with it; the line's text survives a rebase, a
    /// force-push, an insertion above it (the same field human line comments carry,
    /// `prq::ReviewComment::text`, SKEIN-214). Filled by the drafter from the diff it vetted the
    /// comment against, so it is only ever set for a line the diff actually proved.
    ///
    /// `#[serde(default)]` so drafts persisted before this field existed still parse — with empty
    /// text, which re-anchoring deliberately treats as "nothing to search for": on a moved head
    /// every such comment is displaced into the review body naming the drafted commit. Harmless,
    /// and honest — displacement costs a little reading, a guessed anchor costs trust.
    #[serde(default)]
    pub line_text: String,
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
    /// When this draft was WRITTEN, RFC3339 in UTC — stamped by [`store_critique`], and by
    /// [`critiqued`] from the file's own mtime for drafts persisted before this field existed.
    ///
    /// It is not decoration and it is not an audit trail: it is one half of the comparison that
    /// answers "might this review already be on GitHub?" for a draft posted by some path that
    /// wrote no receipt. See [`Critique::posted`].
    ///
    /// `#[serde(default)]` so an older file still parses — as empty, which the page reads as "no
    /// floor available" rather than as a date.
    #[serde(default)]
    pub written_at: String,
    /// **skein posted this review, and here is the receipt** (SKEIN-364).
    ///
    /// The defect, reported live on #691: *"it shows the review while the review was already
    /// submitted and shows up in comments basically this is prone to giving the same comments
    /// again and again."* The draft stayed on disk after posting, [`worth_critiquing`] refuses to
    /// draft a second one at a head it has already drafted, and so the pane went on offering the
    /// post control for a review GitHub already had. Every press repeated it.
    ///
    /// **Why a receipt rather than deleting the draft, and why a receipt rather than asking
    /// GitHub.** Deleting it would leave the row saying nothing where a review somebody paid for
    /// used to be — the SKEIN-355 failure with a different cause. And GitHub cannot be asked
    /// precisely: `Pr::my_review` comes from `latestOpinionatedReviews`, which EXCLUDES `COMMENTED`
    /// — the verdict [`post_critique`] posts under — and `Pr::review_threads` carries
    /// `id/resolved/outdated/author/started_at/url` and deliberately no bodies
    /// (`src/prq.rs:1468-1471`), so nothing in the payload can be matched against a drafted
    /// comment's text. What skein knows exactly is what skein itself did, which is this.
    ///
    /// The threads are still worth something and the page uses them as a FLOOR, never as this:
    /// review threads YOU opened after [`Critique::written_at`] mean a review of yours may already
    /// say these things, which is a caution rather than a receipt (`revDraftEchoes`).
    ///
    /// `#[serde(default)]`, so every draft written before this existed reads back as un-posted —
    /// the safe direction: an un-posted draft that was in fact posted still gets the floor's
    /// warning, where a posted draft read as un-posted would be silently withheld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted: Option<Posted>,
}

/// The receipt for a drafted review that reached GitHub: when, and onto which commit.
///
/// `onto` is the LIVE head the post landed on, which is not always [`Critique::head_sha`] — a
/// review drafted before the branch moved posts onto the commit that is there now, re-anchored by
/// line text ([`crate::prq::submit_review_with_comments`]). Recording both is what lets the pane
/// say "posted onto abc1234" about a review of def5678 without either sha being a guess.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Posted {
    /// RFC3339 in UTC, seconds precision — the same shape GitHub uses for
    /// `ReviewThread::started_at`, so the two are comparable as strings.
    pub at: String,
    /// The commit the review was posted against.
    pub onto: String,
    /// What it went as — `comment` or `approve`.
    ///
    /// **Because the two are different acts, and only one of them is a repeat** (SKEIN-397).
    /// Posting a review and then approving WITH it is the press SKEIN-369 exists to make work: the
    /// second one changes the pull request's approval state, which the first did not. Sending the
    /// same verdict twice changes nothing and leaves two identical reviews on somebody's pull
    /// request, in the reader's name, where they cannot quietly be taken back.
    ///
    /// `#[serde(default)]` for receipts written before this field existed — and an empty value is
    /// read as "unknown", which refuses BOTH. That is deliberate: the two errors are not the same
    /// size. A refused approval is recoverable in one press; a duplicate review is not recoverable
    /// at all.
    #[serde(default)]
    pub as_verdict: String,
}

/// Now, in the one format this file compares timestamps in: RFC3339, UTC, seconds.
///
/// The shape matters more than the precision. GitHub hands back `createdAt` as
/// `2026-08-26T10:12:23Z`, and the page's floor compares a draft's `written_at` against those
/// strings directly — which is only sound while both are UTC, zero-offset and the same width.
fn stamp_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The same instant, from a file's mtime — what [`critiqued`] stamps a draft written before
/// [`Critique::written_at`] existed with, so the floor has something to compare rather than a
/// blank. A clock that cannot be read leaves it blank, which the page treats as "no floor".
fn stamp_of(at: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
    let (at, path) = best?;
    let mut c: Critique = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    // A draft persisted before [`Critique::written_at`] existed still has to be datable, or the
    // page's floor for "you may already have posted this" has nothing to compare against. The
    // file's own mtime IS when it was written, and it is already in hand here.
    if c.written_at.is_empty() {
        c.written_at = stamp_of(at);
    }
    Some(c)
}

/// Write the draft down. `written_at` is stamped HERE rather than by each drafter, so there is one
/// answer to when a review was written and no caller can forget to give it one.
///
/// `&mut`, so the stamp lands on the caller's copy too. A drafter that stored a review and then
/// handed the value straight back — [`vet_and_store_critique`] does exactly that — would otherwise
/// return a record with no `written_at` while the file on disk had one, and the page would be
/// looking at whichever of the two happened to reach it.
fn store_critique(repo_id: &str, c: &mut Critique) -> Result<(), String> {
    if c.written_at.is_empty() {
        c.written_at = stamp_now();
    }
    let path = critique_path(repo_id, c.number, &c.head_sha);
    let dir = path.parent().ok_or("no parent")?.to_path_buf();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    write_atomic(
        &path,
        &dir,
        &serde_json::to_vec_pretty(&*c).map_err(|e| e.to_string())?,
    )
}

/// Record that this draft reached GitHub (SKEIN-364).
///
/// Written after the post is accepted and never before: a receipt for a review GitHub refused
/// would withhold the post control for a review that is not there, which is the SKEIN-355 failure
/// wearing SKEIN-364's clothes.
///
/// It rewrites the draft where it lies — keyed by the commit the draft READ, which is the file's
/// name — rather than storing a second record beside it. A receipt that can go missing from its
/// review is a receipt that can be shown for the wrong one.
///
/// Silent when there is nothing on disk to mark: a post is a post whether or not skein kept the
/// draft, and failing the press over a bookkeeping write would lose the review that just landed.
fn note_critique_posted(
    repo_id: &str,
    number: u64,
    drafted_head: &str,
    onto: &str,
    as_verdict: &str,
) {
    let path = critique_path(repo_id, number, drafted_head);
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut c) = serde_json::from_str::<Critique>(&text) else {
        return;
    };
    c.posted = Some(Posted {
        at: stamp_now(),
        onto: onto.to_string(),
        as_verdict: as_verdict.to_string(),
    });
    let _ = store_critique(repo_id, &mut c);
}

/// **The one parser for "which lines of a unified diff does the NEW file show, and what is on
/// them"** — `(path, new-side line number, content with the diff marker stripped)`, in the order
/// the diff lists them.
///
/// Exactly the lines GitHub accepts a RIGHT-side review comment on: context and added lines count,
/// a deleted line exists only on the left, and a deleted file has no right side at all.
///
/// **Why it is one function and not two** (SKEIN-233). This fact used to be parsed twice — here for
/// vetting a drafted comment ([`commentable`], which decides `Draft::anchored` and
/// `Draft::line_text`), and again in `prq::re_anchor` for placing that same comment against the
/// LIVE diff after the head moved. Two parsers of one grammar drift, and these did:
///
///   * `\ No newline at end of file`. git emits that marker in the MIDDLE of a hunk whenever the
///     old file lacked a trailing newline and the new one has one — routine in JSON, `.env`,
///     generated files and fixtures. The vetting parser had no case for it, so it fell through to
///     an `else` that cleared `in_hunk` and **discarded every remaining line of that hunk**. Every
///     comment the model drafted below the marker was then vetted as unanchorable, and
///     [`assemble_post`] folded it into the review body as `**path**: …` prose. The review still
///     posted and still looked fine; it had simply stopped being a line review for that file.
///   * `+++ path` with no `b/` prefix. The vetting parser required `b/` exactly and treated any
///     other `+++ ` as a deleted file, so such a diff commented on nothing at all.
///   * A hunk line carrying no marker at all. One parser read it as context, the other as the end
///     of the hunk.
///
/// The grammar below is the union, taking the safer reading at each divergence — and the point is
/// that there is now nowhere for a second reading to live. `prq::re_anchor` calls this.
///
/// The content rides along because it is what a draft stores as each comment's durable anchor
/// ([`Draft::line_text`]): the number places the comment today, the text finds it again after the
/// branch moves.
pub fn right_side_lines(diff: &str) -> Vec<(String, u64, String)> {
    let mut out = Vec::new();
    let mut path: Option<String> = None;
    let mut new_line: u64 = 0;
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            path = None;
            in_hunk = false;
        } else if !in_hunk && line.starts_with("+++ ") {
            // `b/` is git's convention and not part of the path; `+++ /dev/null` is a deleted file,
            // which has no right side to comment on. The `!in_hunk` guard is what keeps an ADDED
            // line whose own text begins `++ ` from being read as a file header.
            let name = line["+++ ".len()..].trim();
            path =
                (name != "/dev/null").then(|| name.strip_prefix("b/").unwrap_or(name).to_string());
        } else if !in_hunk && line.starts_with("--- ") {
            // The old-file header; only the +++ side names what RIGHT comments attach to.
        } else if line.starts_with("@@") {
            // `@@ -a,b +c,d @@` — only `+c` matters here. A header this cannot read leaves
            // `in_hunk` false rather than counting from a number nobody supplied.
            in_hunk = false;
            if let Some(plus) = line.split_whitespace().find(|w| w.starts_with('+')) {
                let start = plus[1..].split(',').next().unwrap_or("");
                if let Ok(n) = start.parse::<u64>() {
                    new_line = n;
                    in_hunk = true;
                }
            }
        } else if in_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                if let Some(p) = &path {
                    out.push((p.clone(), new_line, rest.to_string()));
                }
                new_line += 1;
            } else if line.starts_with('\\') || line.starts_with('-') {
                // `\ No newline…` is a note ABOUT the previous line, not a line of either file, so
                // it moves no counter and ends no hunk. `-` lines live only in the old file.
            } else {
                // Context: a leading space, or the entirely empty line git emits for blank context.
                let rest = line.strip_prefix(' ').unwrap_or(line);
                if let Some(p) = &path {
                    out.push((p.clone(), new_line, rest.to_string()));
                }
                new_line += 1;
            }
        }
    }
    out
}

/// [`right_side_lines`] indexed the way vetting asks the question: path → line → content.
///
/// A projection and nothing else. It holds no grammar of its own, which is the whole of SKEIN-233:
/// the vetter and the re-anchorer now cannot disagree about what a diff says, because only one of
/// them reads it.
fn commentable(
    diff: &str,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<u64, String>> {
    let mut map: std::collections::BTreeMap<String, std::collections::BTreeMap<u64, String>> =
        Default::default();
    for (path, line, content) in right_side_lines(diff) {
        map.entry(path).or_default().insert(line, content);
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
            // Same rule: the anchor text comes from the DIFF in `draft_critique`, never from the
            // model's own claim about what a line says.
            line_text: String::new(),
        });
    }
    Some(Critique {
        number: 0,
        head_sha: String::new(),
        overall,
        comments,
        truncated: false,
        // Neither is the parser's to say: `written_at` is stamped when the draft is STORED, and a
        // review the model has only just produced has not been posted to anything.
        written_at: String::new(),
        posted: None,
    })
}

/// The merged prompt: triage, brief, and review in ONE answer — the stage-1 rules, the stage-2
/// headings, and the critique's comment discipline, over one diff. See [`summarise_and_draft`]
/// for why one call.
/// How long the sweep gets. It resends nothing — the diff, the review and the reasoning are all
/// already in the session — so this is time to think about work already done rather than time to
/// read. Sized under stage 2's budget for that reason.
const SWEEP_SECS: u64 = 180;

/// The second turn: the review is asked to account for what it covered, before anybody sees it.
///
/// **The failure this exists for**, in the owner's words (2026-08-26): "someone else finding issues
/// we couldn't is a bigger failure". Everything in [`merged_prompt`]'s review half pushes toward
/// saying less, which is right and which a model can also satisfy by opening three of eleven
/// changed files. The prompt now states that standard; this is what checks the answer against it.
///
/// **It asks for named things, not "anything else?".** The open question is an invitation to
/// manufacture, and manufacturing is the failure the precision wording exists to prevent — buying
/// recall with precision is not a trade, it is the same bug from the other side. So the sweep asks
/// which files went unread, puts the failure classes against each one, and is told in as many words
/// that finding nothing new is the expected answer.
///
/// **It can only add.** Every failure — the turn refusing, running out of time, answering in a
/// shape that will not parse — returns the review exactly as turn 1 wrote it. A sweep that could
/// lose a finding would be worse than no sweep, and the reader is told nothing about it either way:
/// this is skein checking its own work, and a sentence about a sweep that did not run names no move
/// the reader could make.
/// Which conversation a pull request's readings belong to, and the directory it is filed under.
///
/// The directory is the one this repo's readings already live in ([`crate::prq::review_dir`]), for
/// the reason [`crate::ai::Turn`] gives: Claude Code keys a session on the working directory, so
/// the conversation has to run somewhere stable and per-repo or `--resume` will never find it. The
/// repo's bare mirror was the other candidate and is the wrong one — creating it when it is absent
/// would leave a directory that `repos::mirror_ok` reads as a half-made clone, so a session would
/// be bought at the price of breaking the thing boxes clone from.
///
/// It is NOT where the code being reviewed lives; nothing is checked out here. That is SKEIN-395,
/// and it is a different problem — this one is only about the conversation being findable twice.
fn conversation_of(repo: &Repo, number: u64, head_sha: &str) -> (String, PathBuf) {
    let at = review_dir(&repo.id).join("trees").join(number.to_string());
    // The directory is the conversation's address (SKEIN-376), so it is made whether or not the
    // checkout below succeeds and it never moves. A cwd that changed with the weather would file
    // round two's session somewhere round one cannot be found.
    let _ = fs::create_dir_all(&at);
    stand_the_change_up(repo, &at, head_sha);
    (crate::ai::conversation_for(&repo.id, number), at)
}

/// Put the code being reviewed where the reviewer can read it — **or leave nothing at all**
/// (SKEIN-395).
///
/// Measured 2026-08-26, the same prompt over the same 26KB diff, run twice with only the working
/// directory different: with no checkout the reviewer made ZERO tool calls in one turn, read 29,579
/// tokens and cost $0.56; standing in a checkout it made 30 calls over 31 turns, read 2,288,629
/// tokens and cost $1.58. It is not aimless with one — it ran `git show --stat` to find what moved,
/// grepped for the types the diff mentions, read the changed file around each hunk, and followed
/// the caller into another file. That is the behaviour that found a wildcard match over
/// `ai::Unread` in skein's own code, which a diff-only reader had no way to see. The owner chose
/// the depth over the 2.8x: "give it the checkout".
///
/// **Exactly this commit, or an empty directory.** A checkout at the WRONG commit is the one
/// outcome worse than no checkout at all — it is SKEIN-395's own second possibility, a reviewer
/// confidently describing code that is not in this pull request, which is the failure hardest to
/// notice and worst for trust. A pull request from a fork has no branch in the mirror and cannot be
/// stood up at all; that must read as "nothing here", never as "here is the base branch".
///
/// Best-effort throughout: every failure leaves the directory empty and the reading goes ahead
/// exactly as it did before this existed. The reviewer is worth paying for; it is not worth
/// refusing a reading over.
fn stand_the_change_up(repo: &Repo, at: &std::path::Path, head_sha: &str) {
    // A sha skein did not get from GitHub is not a commit to go looking for.
    if head_sha.len() < 7 || !head_sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return;
    }
    let git = |args: &[&str], secs: u64| {
        let mut c = std::process::Command::new("git");
        c.arg("-C").arg(at).args(args);
        bounded_output(&mut c, "git", Duration::from_secs(secs))
            .ok()
            .filter(|o| o.status.success())
    };
    if !at.join(".git").exists() {
        let Ok(mirror) = crate::repos::ensure_mirror(repo) else {
            return;
        };
        let mut clone = std::process::Command::new("git");
        clone
            .args(["clone", "--quiet"])
            .arg(&mirror)
            .arg(at)
            // `.` because `clone` names the destination itself and `-C` would fight it.
            .current_dir(".");
        if bounded_output(&mut clone, "git clone", Duration::from_secs(300))
            .ok()
            .filter(|o| o.status.success())
            .is_none()
        {
            return;
        }
    }
    // **Detached, at the commit, and nowhere else.** `--detach` because there is no branch to be on
    // and moving one would be a write to something a person owns; `git checkout <sha> -- .` would
    // leave the index describing a different commit.
    //
    // Tried before any fetch, because the overwhelmingly common case is a commit already here: the
    // clone brought the whole history down and a second round of the same head needs nothing new.
    if git(&["checkout", "--quiet", "--detach", head_sha], 120).is_none() {
        // **Not here yet, so go and get it — from GITHUB, not from the mirror.** Found on the rig
        // (2026-08-27), and it is the difference between this feature working and quietly doing
        // nothing: the checkout's origin is the mirror, so fetching it only ever asks a mirror that
        // may itself be days behind. Nothing on the reading path refreshes the mirror — `skein
        // pull` and a box start do — so a pull request pushed since the last one has no branch
        // here, the checkout stays empty, and the reviewer silently goes back to reading the diff
        // alone. Measured on the rig: the mirror was 16 hours old and did not carry the head of the
        // pull request being read.
        //
        // Two hops, in the order that costs least: the mirror is fetched from its remote, then the
        // checkout from the mirror. Only ever reached when the commit is genuinely absent, so an
        // ordinary round still pays nothing.
        if crate::repos::fetch_mirror(repo).is_ok() {
            let _ = git(&["fetch", "--quiet", "origin"], 300);
        }
        if git(&["checkout", "--quiet", "--detach", head_sha], 120).is_none() {
            // It really is not here — a fork's head, or a branch deleted since. Empty is the honest
            // answer, and the previous round's checkout must not be left behind wearing this
            // round's name: the reviewer would read it and be wrong about which change it is
            // looking at.
            clear_the_tree(at);
            return;
        }
    }
    // What a `git checkout` of a moved head leaves behind: the file deleted in this commit is still
    // sitting there from the last one, and the reviewer reads it as part of the change.
    let _ = git(&["clean", "--quiet", "-fdx"], 120);
}

/// Empty it, keeping the directory itself — it is the conversation's address (SKEIN-376) and losing
/// it would lose every earlier round with it.
fn clear_the_tree(at: &std::path::Path) {
    let Ok(entries) = fs::read_dir(at) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match path.is_dir() {
            true => {
                let _ = fs::remove_dir_all(&path);
            }
            false => {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

fn sweep(id: &str, at: &std::path::Path, first: Option<Critique>) -> Option<Critique> {
    let first = first?;
    let answer = crate::ai::claude_in_turn(
        SWEEP_PROMPT,
        review_model(Some("claude-sonnet-5")).as_deref(),
        Duration::from_secs(SWEEP_SECS),
        crate::ai::Turn::Resuming { id, at },
    )
    .ok()?;
    let found = parse_critique(&answer)?;
    Some(fold_sweep(first, found))
}

/// What the sweep is allowed to do to the review: add findings it did not already carry, and
/// nothing else.
///
/// Pure, and separate from the call, because this is where a sweep could go wrong in a way nobody
/// would see — a review is not diffed against anything before it reaches a person, so a fold that
/// dropped a finding would look exactly like a review that never made one.
///
/// Same file and same line is the same finding, whatever the second turn called it. Matching on the
/// text instead would let a rewording of one point through as two, which is the padding this whole
/// design is built to avoid. The first turn's OVERALL stands: it describes the change, while the
/// sweep's describes the sweep, and the reader asked about the change.
fn fold_sweep(mut first: Critique, found: Critique) -> Critique {
    let already: std::collections::HashSet<(String, u64)> = first
        .comments
        .iter()
        .map(|c| (c.path.clone(), c.line))
        .collect();
    for c in found.comments {
        if !already.contains(&(c.path.clone(), c.line)) {
            first.comments.push(c);
        }
    }
    first
}

/// What the sweep asks. Every clause is load-bearing; see [`sweep`] for why the open question is
/// not one of them.
const SWEEP_PROMPT: &str = r###"Before that review is shown to the reviewer, account for what it actually covered. You have the diff above — do not ask for it again, and do not restate any of it.

Work through this in order:
1. List every file the diff touches. For each one, say honestly whether you read its changed hunks or skimmed past them.
2. Go back, in the diff above, to the ones you skimmed.
3. For every file, put each of these against what it changed: bugs, correctness risks, races, security holes, data loss, unhandled error paths that can actually fail, misleading names that will cause a wrong call later, real performance traps.
4. Note anything you considered raising and decided against, and why. Those do NOT go in the review.

Then report ONLY what is genuinely NEW — a real problem you did not already raise. Every rule from the review still holds: no style, no formatting, no praise, no hedged maybes, nothing that restates what the diff does, nothing raised twice in different words.

Finding nothing new is the expected outcome and the correct answer. Say so and add no comments. Do not add a comment to show that you looked.

Answer in EXACTLY this format and nothing else:
OVERALL: <"nothing new", or one sentence on what this pass added>
Then one block per NEW review comment, each ended by a line containing only three dashes:
FILE: <the path exactly as it appears in the diff>
LINE: <the line number IN THE NEW FILE this is about — count from the +start in the nearest @@ header. 0 if it is about the change as a whole>
COMMENT: <the comment. Say what is wrong and what to do instead. May span lines.>
---"###;

/// **The round gate** (SKEIN-379) — asked as the FIRST TURN of the round, never as a call of its
/// own, which is the whole design. Resuming costs almost nothing because the context is warm, and
/// the answer when it is "no" is one line; when it is "yes" the same turn produces the round, so a
/// round that IS worth running costs exactly what it costs today.
///
/// **It is the only thing between automatic rounds and unbounded spend.** Rounds run unasked and
/// there is no counter behind this — the owner's words: *"analyse the new messages as they come and
/// figure if new round is needed or not. Doing a round is expensive so do new round when you think
/// it is justified."* A mechanical trigger list cannot tell a substantive reply from an
/// acknowledgement, which is exactly why the judgement is made by the thing that still remembers
/// the argument.
///
/// The rule in the paragraph is the owner's, in his words (2026-08-26): *"don't run a round on
/// every commit, wait till there is enough or till things stabilized, generally user might post
/// comments on PR once they are done but not always true though."* So the gate is told to wait for
/// the change to settle, told that an author's comment is usually — not always — the sign that they
/// are finished, and told to judge rather than to match.
///
/// **A press never reaches here.** The owner has said twice that what he asks for is not rationed,
/// so [`Trigger::Asked`] skips the gate entirely and always runs the round.
fn gate_paragraph(read_at: &str, now_at: &str) -> String {
    format!(
        "You have read this pull request before, in this conversation. You read it at {read}; it \
         is now at {now}.\n\n\
         Before reviewing it again, decide whether a fresh round is worth what it costs. DO NOT run \
         one on every commit. Wait until there is enough to be worth reading, or until the change \
         has stopped moving: somebody pushing a series of commits is still working. A comment from \
         the author is usually the sign that they are done, though not always.\n\n\
         What earns a round is a change to what your review would SAY — code you have not judged, a \
         point you raised addressed or argued with, a decision reversed. What does not: a typo, a \
         rebase, a formatting pass, a commit message, a rename you have already accounted for, work \
         that is visibly still in progress.\n\n\
         If this does not warrant a round, answer with exactly one line and nothing else:\n\
         NO-ROUND: <one sentence, addressed to the reviewer, saying what changed and why it does \
         not need a fresh review>\n\n\
         Otherwise ignore this paragraph completely and answer in the format below.\n\n",
        read = short(read_at),
        now = short(now_at),
    )
}

/// The gate's refusal, and the sentence it gave for it. `None` for any other answer.
///
/// **Anchored at the START of the answer**, because a real review is entitled to contain the words
/// "no round" in its prose, and a gate that matched anywhere would throw away a review that had
/// just been paid for.
fn no_round(answer: &str) -> Option<String> {
    let rest = answer.trim_start().strip_prefix("NO-ROUND")?;
    Some(
        rest.trim_start_matches([':', '-', ' '])
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
    )
}

/// Seven characters of a commit, the length this file shows one at everywhere else.
fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn merged_prompt(
    pr: &Pr,
    owned: &Ownership,
    signals: &[crate::contracts::Signal],
    diff: &str,
    cut: bool,
    gate: &str,
) -> String {
    // The same three-way sentence as `stage1_prompt`, in this prompt's register: both
    // empty-handed answers keep the whole change in scope, and only the wording tells a repo
    // with no CODEOWNERS from a repo skein could not read (SKEIN-117).
    let scope = match owned {
        Ownership::Owned { yours, others } if !yours.is_empty() => format!(
            "The reviewer owns these paths: {}. {} other changed path(s) are outside their ownership — go deep on theirs, stay brief elsewhere.",
            yours.join(", "),
            others
        ),
        Ownership::Unreadable(why) => format!(
            "Whether the reviewer owns any of this is unknown — the repo could not be read to consult CODEOWNERS ({why}). Treat the whole change as in scope."
        ),
        _ => String::from("This repo has no CODEOWNERS, or none of it is attributed — treat the whole change as in scope."),
    };
    let evidence = if signals.is_empty() {
        String::new()
    } else {
        format!(
            "Scanning the diff mechanically found these moved, which is not in dispute — explain what each means for someone using this:\n{}\n",
            signals
                .iter()
                .map(|s| format!("- {} ({})", s.what, s.file))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        r###"{gate}You are reading a pull request for a senior engineer whose review this is. Produce BOTH halves in one answer: a triage summary of what the change means, and an actual review of the code.

For the SUMMARY half: they review to stay informed, not to catch bugs — mechanism, product, architecture and user level, never functions or line-level edits. Expand ONLY if the change moves something's contract or behaviour. The tripwires are:
- behaviour: an existing feature now does something different
- interface: a flag, route, config key, env var, file format or public API changed
- default: a default value or default-on/off choice changed
- architecture: a mechanism was replaced, removed, or its responsibility moved
- ux: what a person sees or has to do changed
A bug fix, a test, a refactor with no behaviour change, docs, or a dependency bump does NOT expand, however large the diff. When you are genuinely unsure, expand.

For the REVIEW half: comment ONLY on actual problems and improvements that matter — bugs, correctness risks, races, security holes, data loss, unhandled error paths that can actually fail, misleading names that will cause a wrong call later, real performance traps. Do not manufacture findings to seem thorough; no style, no formatting, no praise, no hedged maybes, no restating what the diff does. The reviewer will keep or drop each comment and post the kept ones under their own name. An empty review is a valid review.

This is your one pass, and other people review this change too. A real problem someone else raises that was visible in the diff below is the worst outcome this review has — worse than needing a second round, and it is the one way an empty review becomes the wrong answer. What prevents it is COVERAGE, not volume: open every changed file, and put each failure class above against what you actually read rather than against what you noticed first. Padding with maybes to feel thorough makes this worse, not safer — it spends the reviewer's attention, which is the thing you are here to protect.

PR #{number}: {title}
Author: {author}
Branch {head} into {base}.
{scope}
{evidence}{cut_note}

Answer in EXACTLY this format and nothing else:
KIND: <fix|feature|refactor|docs|chore>
LINE: <one sentence, plain English, saying what this changes and why it matters. For a fix, say what was broken.>
EXPAND: <yes|no>
FLAGS: <comma-separated from: {flags} — or "none" when EXPAND is no>
DETAIL:
<when EXPAND is yes: plain prose under the headings "## What it does", "## What changes in how it works", and — only for a genuinely close call — "## Worth your call", omitting any heading with nothing true to say. When EXPAND is no: the single word none>
REVIEW:
OVERALL: <one sentence on the change as a whole, or "nothing to flag">
Then one block per review comment, each ended by a line containing only three dashes:
FILE: <the path exactly as it appears in the diff>
LINE: <the line number IN THE NEW FILE this is about — count from the +start in the nearest @@ header. 0 if it is about the change as a whole>
COMMENT: <the comment. Say what is wrong and what to do instead. May span lines.>
---

--- diff ---
{diff}"###,
        number = pr.number,
        title = pr.title,
        author = pr.author,
        head = pr.head_ref,
        base = pr.base_ref,
        scope = scope,
        evidence = evidence,
        cut_note = if cut {
            "NOTE: the diff below was cut at a byte cap — you are seeing part of the change. The cut names the files that are missing; do not guess about them, say the summary and review do not cover them, and if what you can see is not enough to triage, answer EXPAND: yes."
        } else {
            ""
        },
        flags = FLAGS.join(", "),
        diff = diff,
        // Empty on a first reading and on every press, so this prompt is byte-for-byte what it was
        // before the gate existed unless there is actually a round to judge (SKEIN-379).
        gate = gate,
    )
}

/// Parse the merged answer into its two halves: the triage verdict plus brief, and the review.
///
/// Strict where strictness protects attention, forgiving where it protects paid work: the SUMMARY
/// fields are the same strict [`parse_stage1`] (a model that ignored the format ignored the
/// instructions — `None` here is the whole answer refused), while a missing or unparseable
/// REVIEW section comes back as `Ok` with `None` — the summary still stands, and the caller notes
/// the draft as tried because the call was spent either way.
fn parse_merged(text: &str) -> Option<(Verdict, String, Option<Critique>)> {
    // Everything before the first `REVIEW:` line is the summary's; everything after is the
    // review's, in exactly the shape [`parse_critique`] already reads.
    let (summary_part, review_part) = match text.split_once("\nREVIEW:") {
        Some((head, tail)) => (head, Some(tail)),
        None => (text, None),
    };
    let verdict = parse_stage1(summary_part)?;
    let detail = summary_part
        .split_once("DETAIL:")
        .map(|(_, d)| d.trim())
        .filter(|d| !d.is_empty() && !d.eq_ignore_ascii_case("none"))
        .map(str::to_string)
        .unwrap_or_default();
    Some((verdict, detail, review_part.and_then(parse_critique)))
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
            // The line's own content, captured when the draft was vetted against the diff — the
            // anchor `prq::re_anchor` searches the live head for if the branch has moved by the
            // time this posts. Empty (a draft persisted before `line_text` existed) means the
            // re-anchor displaces it into the body instead of guessing, which is the safe shape.
            text: d.line_text.clone(),
        })
        .collect();
    (body, anchored)
}

/// What a verdict is called in a receipt. Deliberately not `Debug`: this string is written to disk
/// and compared against on the next press, so it must not change when somebody renames a variant.
fn verdict_name(v: crate::prq::Verdict) -> &'static str {
    match v {
        crate::prq::Verdict::Approve => "approve",
        crate::prq::Verdict::RequestChanges => "request-changes",
        crate::prq::Verdict::Comment => "comment",
    }
}

/// Has this exact draft already gone to GitHub as this? The sentence to show, or `None` to send.
///
/// Reads the draft AT THE HEAD IT WAS DRAFTED FOR — `critique_path`'s own key — so this can only
/// ever refuse the thing that was actually sent. A re-read at a moved head writes a different file
/// and is not touched by this.
///
/// The sentence carries what the receipt knows: when it went, and onto which commit. Both are facts
/// the reader needs to go and look, and a refusal without them is just a door that will not open.
fn already_sent(
    repo_id: &str,
    number: u64,
    head_sha: &str,
    verdict: crate::prq::Verdict,
) -> Option<String> {
    let text = fs::read_to_string(critique_path(repo_id, number, head_sha)).ok()?;
    let posted = serde_json::from_str::<Critique>(&text).ok()?.posted?;
    let went = posted.at.clone();
    let onto = posted.onto.chars().take(7).collect::<String>();
    // An old receipt does not say what it went as, and is read as "unknown" — refusing both. The
    // errors are not the same size: a refused approval costs one press, a duplicate review is on
    // somebody's pull request under the reader's name for good.
    if posted.as_verdict.is_empty() {
        return Some(format!(
            "skein already posted this review at {went}, onto {onto} — but not which verdict it \
             went as, so it will not send it again. Approve without it, or read the change again \
             to draft a new review."
        ));
    }
    if posted.as_verdict != verdict_name(verdict) {
        // A different act: posting the review and then approving WITH it is SKEIN-369's press, and
        // the approval changes something the comment did not.
        return None;
    }
    Some(format!(
        "skein already posted this review at {went}, onto {onto}. Sending it again would leave a \
         second identical review on the pull request, so it was not sent. Read the change again to \
         draft a new one."
    ))
}

/// Post what the person kept, and nothing else — the whole write path, so the rules live where
/// they can be proven: a review drafted at one commit posts onto the live one by re-anchoring
/// each kept comment's line text (displacing what no longer matches, naming the drafted sha), and
/// the payload is assembled from the VETTED comments handed in, never from what was stored.
/// `verdict` is what the review is SUBMITTED as. `Comment` is the ordinary post — skein's words
/// said to the author with no verdict attached — and `Approve` is the "approve with this review"
/// press, which is the same artefact reaching GitHub under a verdict (SKEIN-369).
///
/// **It is a parameter rather than a second function, and that is the whole of SKEIN-369.** The
/// approve press used to go down its own path — `/review/:n/act` straight into
/// `prq::submit_review_with_comments` — which posted the identical review and wrote no receipt. So
/// approving with skein's review left the draft looking unposted: the row went on saying "review
/// ready · N" with "go through N comments and post…", and pressing that said every comment to the
/// author a second time. That is exactly the report SKEIN-364 was filed for (#691, "it shows the
/// review while the review was already submitted"), reachable by the other button. Two write paths
/// for one artefact is what let them diverge, so there is one.
pub fn post_critique(
    repo: &Repo,
    number: u64,
    head_sha: &str,
    overall: &str,
    kept: &[Draft],
    verdict: crate::prq::Verdict,
) -> Result<String, String> {
    // **A press posts, or it fails for a reason about posting** (SKEIN-272). This used to open with
    // `prq::queue(repo, false)?` — a full refresh past its sixty-second cache, viewer lookup and
    // five membership searches included — for two facts a refresh is not the way to learn. The `?`
    // on that line converted "skein could not re-read your queue" into "your review was not
    // posted", and said so in the refresh's words: the owner pressed post and was told five
    // membership searches were missing, about a repository they had not asked after.
    //
    // It also asked whether the pull request was IN the queue, and refused when it was not. Open,
    // drafted and absent from the membership searches is exactly a PR you authored and were never
    // asked to review, so that refusal could turn down a PR the pane had just rendered a draft for.
    // It existed only to reach the head sha below; the two go together.
    let slug = crate::prq::slug_for_write(repo)?;
    // **The receipt is read here, not only written below** (SKEIN-397). It was written and never
    // consulted, so the only thing stopping a second post was the pane declining to draw the
    // control — which holds for a person pressing deliberately and does nothing for a double-click
    // before the row re-renders, a retried request, or a second tab open on the same row. Measured
    // on the rig against real GitHub: post, receipt written, post again, TWO identical reviews on
    // the pull request. That is the shape of the owner's original report — two byte-identical
    // reviews 35 seconds apart, which is a resend and not a decision.
    //
    // Keyed by the commit the draft READ, which is the file's own name, so a draft re-read at a new
    // head is a different draft and stays postable. The guard is "this draft, already sent as
    // this", never "this pull request already has a review".
    if let Some(said) = already_sent(&repo.id, number, head_sha, verdict) {
        return Err(said);
    }
    let (body, anchored) = assemble_post(overall, kept);
    // A moved head is no longer refused (it used to be — a dynamically moving PR made "draft it
    // again" a treadmill, SKEIN-215): each kept comment carries its line's text, so the submit
    // path re-anchors against the LIVE head's diff exactly as human line comments do (SKEIN-214).
    // Lines that survive post at their new numbers; the displaced fold into the body naming the
    // drafted commit, and the record says what was actually reviewed either way.
    // The live head, read now — what `commit_id` must name, and what `head_sha` below is compared
    // against to decide whether anything needs re-anchoring. The fallback must not be `head_sha`
    // itself: a sha compared against itself is never "moved", nothing re-anchors, and vetted
    // comments post at line numbers computed against a diff that no longer exists (SKEIN-230).
    // `remembered_head` is what this machine already holds — no network call, so the fallback
    // cannot fail the post — and `None` when it holds nothing, rather than an invented sha.
    let seen_at = crate::prq::remembered_head(&repo.id, number);
    let head =
        crate::prq::head_to_post_against(&slug, number, seen_at.as_deref().unwrap_or(head_sha));
    let said = crate::prq::submit_review_with_comments(
        &slug, number, &head, verdict, &body, &anchored,
        // The head the draft read. Equal to the live head in the common case, in which case
        // nothing re-anchors and nothing is annotated.
        head_sha,
    )?;
    // The receipt, after the `?` and not before it (SKEIN-364): a draft is marked posted only once
    // GitHub has actually taken it. Keyed by the commit the draft READ — `head_sha` here, which is
    // the file's own name — while `head` is where it landed, and the two differ exactly when the
    // branch moved between drafting and posting.
    note_critique_posted(&repo.id, number, head_sha, &head, verdict_name(verdict));
    crate::prq::invalidate(&repo.id);
    Ok(said)
}

/// Draft an actual review of the PR, because somebody pressed for one.
///
/// **It is a [`visit`], not a second analysis** (SKEIN-263). This used to be its own drafter with
/// its own prompt and its own diff download, which meant the summary on the row and the review
/// under it could describe two different readings of the same commit — the divergence merging
/// them was meant to end. Now one forced visit produces both, over one download, on one model
/// call, and stores both; this function hands back the half the caller asked for.
///
/// `force`, so the reading on disk is not what comes back: "draft again" means read it again.
/// [`Review::Always`], so [`worth_critiquing`]'s two nos — already drafted at this head, never
/// yours to give — do not stand against a person asking. [`Trigger::Asked`], so the day's ceiling
/// neither refuses nor counts it: the limit is on skein's initiative only (see [`Trigger`]).
///
/// `identities` is the viewer, for the summary half's ownership attribution — the same slice
/// [`summarise`] takes, from the same queue the caller already read.
pub fn critique(
    repo: &Repo,
    slug: &str,
    pr: &Pr,
    identities: &[String],
) -> Result<Critique, String> {
    if !summaries_enabled() {
        return Err(
            "reading PRs is switched off — turn \"Read pull requests\" back on in Settings → Boxes."
                .into(),
        );
    }
    // The same visit the read route makes with `redraft=1`, spelled once. Two callers wanting
    // one reading is what this function used to be the second copy of.
    let summary = re_read_replacing_the_review(repo, slug, pr, identities);
    if let Some(drafted) = critiqued(&repo.id, pr.number).filter(|c| c.head_sha == pr.head_sha) {
        return Ok(drafted);
    }
    // No draft on disk for this head, so say which half failed rather than a single shrug. A
    // summary that came back unread carries its own reason; a summary that parsed while the review
    // did not leaves the reason in the tried-note `summarise_and_draft` writes.
    Err(if matches!(summary.depth, Depth::Unread) {
        summary.unread_because
    } else {
        critique_tried(&repo.id)
            .remove(&format!("{}-{}", pr.number, pr.head_sha))
            .unwrap_or_else(|| "skein read it but drafted no review of it — try again.".into())
    })
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

/// **There is no standalone drafter any more** (SKEIN-263). `draft_critique` and
/// `draft_critique_from` lived here: their own prompt, their own diff download, reached by the
/// panel's "draft again" and by `read_waiting`'s draft-only door. Both callers go through
/// [`visit`] now, so a summary and the review beside it always come from the same reading of the
/// same diff. Nothing needs a review WITHOUT a summary, which was the only thing that would have
/// kept a second path alive.
///
/// `CritiqueFail` went with them: it carried a `spent` flag so the standalone drafter could tell a
/// failure that cost a model call from one that did not, and decide whether to write a tried-note.
/// The one caller left is inside [`summarise_and_draft`], which is past the call by definition —
/// the model has already answered — so every failure here is spent and the note is unconditional.
///
/// The vetting itself: anchor every comment against the diff the model actually read, keep the
/// proof, pin the draft to this pull request and head, and write it down.
fn vet_and_store_critique(
    repo: &Repo,
    pr: &Pr,
    mut drafted: Critique,
    diff: &str,
    cut: bool,
) -> Result<Critique, String> {
    let lines = commentable(diff);
    for d in &mut drafted.comments {
        // The vetting keeps only lines the diff proves — and takes the proof with it: the line's
        // own content is stored as the anchor that lets this draft survive the head moving before
        // it is posted (see [`Draft::line_text`]). An unanchored comment gets none, truthfully:
        // there is no line the diff vouches for.
        let proven = (d.line > 0)
            .then(|| lines.get(&d.path).and_then(|m| m.get(&d.line)))
            .flatten();
        d.anchored = proven.is_some();
        d.line_text = proven.cloned().unwrap_or_default();
    }
    drafted.number = pr.number;
    drafted.head_sha = pr.head_sha.clone();
    drafted.truncated = cut;
    // Spent: the model already answered, and losing the write is worth noting rather than
    // re-buying the answer next pass.
    store_critique(&repo.id, &mut drafted)?;
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

/// Test-only: read one HTTP request off a socket properly — headers to the blank line, then
/// exactly Content-Length bytes of body. The fixed-size single read the stubs used truncated the
/// batched GraphQL request when SKEIN-209 folded five searches into one call, and a stub that
/// answers a half-read request answers the wrong question.
#[cfg(test)]
fn read_request(stream: &std::net::TcpStream) -> (String, String) {
    use std::io::{BufRead as _, Read as _};
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    reader.read_line(&mut head).ok();
    let mut length = 0usize;
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        if line.trim().is_empty() {
            break;
        }
        if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = n.trim().parse().unwrap_or(0);
        }
        line.clear();
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok();
    }
    (
        head.trim_end().to_string(),
        String::from_utf8_lossy(&body).into_owned(),
    )
}

#[cfg(test)]
mod tests {

    // ── what skein is reading right now (SKEIN-333) ────────────────────────────────────────────
    //
    // The pane draws a row's "⟳ reading again… 22s" from this registry, so an entry that outlives
    // its reading is a row that says "reading" for ever with nothing able to clear it.

    /// Every case here keys on its OWN pull request number and never on the size of the registry:
    /// it is process-wide by design — one skein, one set of readings in flight — and `cargo test`
    /// runs these on several threads at once, so a length is a number another test is changing
    /// underneath you. Measured: this suite passed alone and failed in the full run before the
    /// assertions were keyed this way.
    #[test]
    fn a_reading_is_announced_while_it_runs_and_forgotten_however_it_ends() {
        {
            let _g = super::ReadingGuard::begin("acme", 684, super::Trigger::Asked);
            let now = super::readings();
            let mine = now
                .iter()
                .find(|r| r.repo_id == "acme" && r.number == 684)
                .expect("a reading in flight is visible while its guard lives");
            assert!(mine.asked, "a pressed read reports itself as asked");
            assert!(
                mine.started_ms > 0,
                "and says when it started, so the page can count locally"
            );
        }
        assert!(
            !super::readings().iter().any(|r| r.number == 684),
            "and is gone the moment the reading ends"
        );
    }

    /// **A reading that PANICS still clears.** This is why the registry is a `Drop` guard and not a
    /// pair of insert/remove calls: [`spend_a_visit`] returns from a dozen places and the model call
    /// inside it can unwind, and every one of those exits has to leave the row clearable.
    #[test]
    fn a_reading_that_panics_does_not_leave_the_row_reading_for_ever() {
        let fell_over = std::panic::catch_unwind(|| {
            let _g = super::ReadingGuard::begin("acme", 999, super::Trigger::Unasked);
            assert!(
                super::readings().iter().any(|r| r.number == 999),
                "in flight before the panic"
            );
            panic!("the model call fell over");
        });
        assert!(fell_over.is_err(), "the panic is not swallowed");
        assert!(
            !super::readings().iter().any(|r| r.number == 999),
            "and the row is not left saying it is being read"
        );
    }

    /// **Skein's own reads are announced on the same terms as a press** — the owner's answer when
    /// asked whether background reads should show: yes, any read in flight shows.
    #[test]
    fn a_read_skein_started_itself_is_announced_and_says_so() {
        let _g = super::ReadingGuard::begin("acme", 715, super::Trigger::Unasked);
        let mine = super::readings()
            .into_iter()
            .find(|r| r.number == 715)
            .expect("the pump's own read is in flight too");
        assert!(
            !mine.asked,
            "and is distinguishable from one somebody pressed, which the row cannot infer"
        );
    }

    /// **Where the guard is begun is the whole of its coverage**, and it cannot be reached from a
    /// test: everything past it needs GitHub and a model. So this reads the source.
    ///
    /// The placement is load-bearing twice over. Registered inside `spend_a_visit` it covers EVERY
    /// model-spending path — the pump's reads as well as the route's — where the same two lines at
    /// the route would have seen presses and missed the pump. And registered after the cheap
    /// refusals it announces only readings that are actually being bought: a cache hit, a repo out
    /// of scope or a spent day all return above it, and a spinner over one of those would be the
    /// page claiming skein was working when it had already declined.
    #[test]
    fn the_guard_is_begun_where_the_reading_becomes_a_purchase() {
        let src = include_str!("review.rs");
        let body = src
            .split_once("fn spend_a_visit(")
            .expect("spend_a_visit is still called that")
            .1;
        let begin = body
            .find("ReadingGuard::begin")
            .expect("the visit registers itself");
        let diff = body
            .find("pr_diff_text")
            .expect("the visit still downloads the diff");
        let budget = body
            .find("over_budget")
            .expect("the visit still checks the day's budget");
        let cached = body
            .find("if let Some(hit) = cached(")
            .expect("the visit still serves the cache");
        assert!(
            begin < diff,
            "announced BEFORE the download, or the row is silent for the slowest part"
        );
        assert!(
            budget < begin,
            "and after the budget refusal, which costs nothing and reads nothing"
        );
        assert!(
            cached < begin,
            "and after the cache hit, which is not a reading at all"
        );
    }

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
            reasons: vec![reason],
            lane,
            ..crate::prq::blank_pr(number, &format!("sha{number}"))
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

        // **Yours, in the waiting lane** — the owner's decision, SKEIN-265. `Lane::Waiting` is the
        // only lane a pull request you opened is ever in (`src/prq.rs:1190-1196`), so refusing it
        // was refusing every pull request on a fleet where the owner writes them all.
        assert!(
            worth_reading("demo", &pr(3, Reason::Author, Lane::Waiting)),
            "a pull request you opened yourself is not read — on a fleet where every PR is yours, \
             that is the whole surface doing nothing"
        );
        // Mentioned in a comment is not a request to review.
        assert!(!worth_reading(
            "demo",
            &pr(4, Reason::Mentioned, Lane::NeedsYou)
        ));
        // The rest of the waiting lane stays out: somebody ELSE's pull request you have already
        // decided on has had your attention, and the widening is about authorship, not the lane.
        assert!(
            !worth_reading("demo", &pr(5, Reason::Reviewer, Lane::Waiting)),
            "a PR you already decided on was read again — the waiting lane was widened wholesale \
             instead of for the ones you wrote"
        );
        assert!(!worth_reading(
            "demo",
            &pr(6, Reason::Reviewer, Lane::Archived)
        ));
        // Set aside by hand, and yours: still out. Archived is you saying it will not move.
        assert!(
            !worth_reading("demo", &pr(10, Reason::Author, Lane::Archived)),
            "a pull request you set aside was read anyway"
        );

        // A draft is the author saying it is not finished.
        let mut draft = pr(7, Reason::Reviewer, Lane::NeedsYou);
        draft.draft = true;
        assert!(!worth_reading("demo", &draft));
        // And your OWN draft, which is the case authorship could have swallowed: `src/prq.rs:1190`
        // files it in `Lane::Waiting` rather than `Lane::NotReady`, so the lane alone would have
        // let it through and only the draft test keeps it out.
        let mut mine_draft = pr(11, Reason::Author, Lane::Waiting);
        mine_draft.draft = true;
        assert!(
            !worth_reading("demo", &mine_draft),
            "a draft you opened was read — a draft is you saying it is not finished, whoever wrote it"
        );

        // A branch still being pushed to IS read now — the settle hour is gone (owner decision,
        // 2026-08-24). The daily budget is the money guard, and re-anchoring by line text made a
        // draft against a moving head postable; a head that moves again is just a new cache key.
        let mut moving = pr(8, Reason::Reviewer, Lane::NeedsYou);
        moving.settled = false;
        assert!(
            worth_reading("demo", &moving),
            "an unsettled branch was refused a reading — the settle gate was removed, the budget \
             is the guard"
        );

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
                ownership_unknown: String::new(),
                unread_because: String::new(),
                not_reread: String::new(),
                computed: true,
                budget_stopped: false,
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
                use std::io::Write as _;
                let mut stream = stream;
                let (head, body) = read_request(&stream);
                let (status, answer) = if head.contains("/pulls/11 HTTP")
                    && diff_broken_flag.exists()
                {
                    (500u16, r#"{"message":"transient"}"#.to_string())
                } else if head.contains("/user/teams") {
                    (200, "[]".to_string())
                } else if head.contains("/user") {
                    (200, r#"{"login":"me"}"#.to_string())
                } else if body.contains("review-requested") {
                    // One pull request, waiting on your review, unread. The batched wire
                    // (SKEIN-209): q0 review-requested, q1 reviewed-by, q2 author, q3 mentions.
                    (
                        200,
                        r#"{"data":{"q0":{"nodes":[{"number":11,"title":"t","url":"u",
                       "isDraft":false,"author":{"login":"someone"},"headRefName":"feat",
                       "headRefOid":"sha11","baseRefName":"main",
                       "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                       "latestReviews":{"nodes":[]},
                       "commits":{"nodes":[{"commit":{"committedDate":"2020-01-01T00:00:00Z"}}]}}]},
                       "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                            .to_string(),
                    )
                } else if head.contains("/graphql") {
                    (200, r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string())
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

        // A real checkout behind the fixture repo, so its mirror is readable and summaries are
        // not computed blind — a blind summary is deliberately never cached (SKEIN-117), and this
        // test's dedupe steps are about the cache and the tried-notes, not about blindness. The
        // slug still comes from `source` (`gitgate::repo_slug` reads the URL first), so the queue
        // stub is untouched by the local tree.
        let checkout = home.join("checkout");
        checkout_fixture(&checkout);
        crate::repos::save_repos(&[serde_json::from_value(serde_json::json!({
            "id": "demo",
            "source": "https://github.com/acme/thing.git",
            "source_tree": checkout.to_string_lossy(),
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
    /// are only mentioned — and a `claude` that answers whichever prompt it is handed: the MERGED
    /// summary-and-review shape when the prompt carries `REVIEW:` (a yours-to-give visit), the
    /// OVERALL shape for a standalone review draft, the stage-1 shape otherwise. Review-drafting
    /// calls are counted into a file, because "it drafted nothing" looks identical whether or not
    /// the model was asked, and the dedupe tests below are ABOUT how often it was asked. Every
    /// request's first line is also appended to `home/hits`, so a test can count DOWNLOADS —
    /// the one-diff-fetch rule is about the wire, not about what landed on disk.
    ///
    /// Both pull requests carry a commit date of NOW: the settle hour is gone (owner decision,
    /// 2026-08-24), so the pass must read and draft a branch that is still moving.
    #[cfg(unix)]
    fn drafting_fixture(home: &std::path::Path) -> std::path::PathBuf {
        drafting_fixture_for(home, "crit", false)
    }

    /// The same wire, serving the OTHER shape this pass has to work: a queue where every pull
    /// request is one YOU opened (`q2`, the `author:` search) — #31 proposed and #32 still a
    /// draft, both in `Lane::Waiting` because that is where `src/prq.rs:1190` files what you wrote.
    ///
    /// One fixture rather than two, because the thing the authored tests assert is a COUNT of model
    /// calls and diff downloads, and a second stub would be a second place for that accounting to
    /// be wrong in.
    #[cfg(unix)]
    fn authored_fixture(home: &std::path::Path) -> std::path::PathBuf {
        drafting_fixture_for(home, "mine", true)
    }

    #[cfg(unix)]
    fn drafting_fixture_for(
        home: &std::path::Path,
        repo_id: &'static str,
        authored: bool,
    ) -> std::path::PathBuf {
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        let reviews_asked = home.join("reviews-asked");
        let sweeps = home.join("sweeps");
        let claude = home.join("claude-both.sh");
        // The merged prompt is the only one carrying the literal `REVIEW:`; the standalone
        // critique prompt carries `OVERALL:` without it; the stage prompts ask for KIND/LINE.
        // Branching on the prompt is what lets ONE binary serve every call the pass makes.
        std::fs::write(
            &claude,
            format!(
                concat!(
                    // **The prompt is the LAST argument, not the fourth.** It was `$4` until a
                    // reading became a conversation (SKEIN-393) and the command line grew
                    // `--session-id <uuid>` between the model and the prompt. A fixture pinned to
                    // an argument POSITION answers the wrong question the moment skein passes a
                    // flag, and it fails silently: the stub matched nothing, printed nothing, and
                    // nine tests reported that the pass had read nothing at all.
                    "#!/bin/sh\nfor a in \"$@\"; do p=\"$a\"; done\ncase \"$p\" in\n",
                    // The second turn (SKEIN-393). It is answered "nothing new", which is what the
                    // prompt says the expected outcome is — so the fixture exercises the path a
                    // real sweep takes most of the time, and a sweep that invented findings here
                    // would be the fixture teaching the assertions the wrong shape.
                    //
                    // Counted in its OWN file: the merged/critique count is what the SKEIN-263
                    // tripwire reads, and adding a third word to it would rewrite what nine
                    // existing assertions mean rather than leaving them saying what they said.
                    "  *\"account for what it actually covered\"*) echo sweep >> {sweeps}; printf 'OVERALL: nothing new\\n';;\n",
                    // Both halves of the merged answer carry the ORDINAL of the model call that
                    // produced them, so a test can prove the summary on the row and the review
                    // under it came out of the same reading (SKEIN-263) rather than merely both
                    // existing. `wc -l` on the count file after appending IS this call's number.
                    "  *\"REVIEW:\"*) echo merged >> {count}; n=$(wc -l < {count} | tr -d ' '); printf 'KIND: fix\\nLINE: reading %s of this change.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\nREVIEW:\\nOVERALL: nothing to flag in reading %s\\nFILE: src/a.rs\\nLINE: 0\\nCOMMENT: about the change as a whole.\\n---\\n' \"$n\" \"$n\";;\n",
                    // The standalone critique prompt, which nothing reaches any more (SKEIN-263
                    // deleted the drafter). Kept as a TRIPWIRE: a second drafter coming back would
                    // put a `critique` line in this count, and the assertions that read it as
                    // `["merged", "merged"]` would say so.
                    "  *OVERALL:*) echo critique >> {count}; printf 'OVERALL: nothing to flag\\n';;\n",
                    "  *) printf 'KIND: fix\\nLINE: it changes a thing.\\nEXPAND: no\\nFLAGS: none\\n';;\n",
                    "esac\n"
                ),
                count = reviews_asked.display(),
                sweeps = sweeps.display()
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
        let hits = home.join("hits");
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let mut stream = stream;
                let (head, body) = read_request(&stream);
                // Every request's first line, so a test can count downloads on the wire.
                {
                    use std::io::Write as _;
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&hits)
                    {
                        let _ = writeln!(f, "{head}");
                    }
                }
                // A commit dated NOW: the settle hour is gone, and the pass must read a branch
                // that is still moving.
                let committed =
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let node = |number: u64, author: &str, draft: bool| {
                    format!(
                        r#"{{"number":{number},"title":"t","url":"u",
                           "isDraft":{draft},"author":{{"login":"{author}"}},"headRefName":"feat",
                           "headRefOid":"sha{number}","baseRefName":"main",
                           "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                           "latestReviews":{{"nodes":[]}},
                           "commits":{{"nodes":[{{"commit":{{"committedDate":"{committed}"}}}}]}}}}"#
                    )
                };
                // The batched wire (SKEIN-209): q0 review-requested, q1 reviewed-by, q2 author,
                // q3 mentions — #21 waits on your review, #22 only mentions you. In the authored
                // shape, q2 instead: #31 yours and proposed, #32 yours and still a draft.
                let answer = if head.contains("/user/teams") {
                    "[]".to_string()
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if body.contains("review-requested:") && authored {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[{},{}]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(31, "me", false),
                        node(32, "me", true)
                    )
                } else if body.contains("review-requested:") {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[{}]}}}}}}"#,
                        node(21, "someone", false),
                        node(22, "someone", false)
                    )
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string()
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

        // A real checkout behind the fixture repo — same reason as `what_it_reads_unwatched`'s:
        // the drafting tests assert what lands in the CACHE, and a repo whose mirror cannot be
        // read has its summaries served without being cached (SKEIN-117). The slug still comes
        // from `source`, so the GitHub stub is untouched.
        let checkout = home.join("checkout");
        checkout_fixture(&checkout);
        crate::repos::save_repos(&[serde_json::from_value(serde_json::json!({
            "id": repo_id,
            "source": "https://github.com/acme/thing.git",
            "source_tree": checkout.to_string_lossy(),
            "store": "",
            "read_prs": true,
        }))
        .unwrap()])
        .unwrap();
        // The queue micro-cache outlives a test's SKEIN_HOME; a stale hit would answer with a
        // queue read against another test's stub.
        crate::prq::invalidate(repo_id);
        reviews_asked
    }

    #[cfg(unix)]
    fn drafting_teardown() {
        drafting_teardown_for("crit")
    }

    #[cfg(unix)]
    fn drafting_teardown_for(repo_id: &str) {
        for key in [
            "SKEIN_HOME",
            "SKEIN_REVIEW_AI",
            "SKEIN_CLAUDE_BIN",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::invalidate(repo_id);
        crate::prq::forget_host_token();
    }

    /// The reader opens summary and drafted review in one go: a pull request waiting on YOUR
    /// review comes out of the background pass with both stored, so expanding the row costs
    /// nothing and asks nothing. No HTTP pane involved — the pass alone must do it.
    ///
    /// And "in one go" is asserted on the wire, not on the disk: ONE diff download and ONE model
    /// call for the whole visit (the merged summary-and-review call, the owner's decision
    /// 2026-08-24) — the fixture records every request head and every `claude` invocation
    /// precisely so this cannot regress into fetch-twice or ask-twice. The fixture's commit
    /// dates are NOW, so this also proves an unsettled branch is read: the settle hour is gone.
    #[cfg(unix)]
    #[test]
    fn the_reader_drafts_the_review_where_it_is_yours_to_give() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = drafting_fixture(home);

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

        // One model call made both. The stub tags each invocation with which prompt it saw.
        let calls = std::fs::read_to_string(&asked).unwrap_or_default();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            vec!["merged"],
            "summary and review must be ONE merged model call, and only one"
        );
        // **And the reading swept itself** (SKEIN-393). Counted apart from the line above, because
        // these are two different facts: how many times the diff was READ, and how many times the
        // review was checked against what it covered. The first is the money; the second is the
        // recall, and it rides on the first one's context rather than buying its own.
        let swept = std::fs::read_to_string(home.join("sweeps")).unwrap_or_default();
        assert_eq!(
            swept.lines().count(),
            1,
            "the review was shown to the reader without ever being asked what it did not open — \
             which is the whole of SKEIN-393, and it fails silently: a review that skimmed three \
             of eleven files looks exactly like one that read them all"
        );
        // One download fed it. The REST diff endpoint for #21 is `GET …/pulls/21` — the files
        // listing (`/pulls/21/files`) is a different, cheaper question and not counted.
        let hits = std::fs::read_to_string(home.join("hits")).unwrap_or_default();
        let diff_fetches = hits
            .lines()
            .filter(|l| l.starts_with("GET") && l.contains("/pulls/21 "))
            .count();
        assert_eq!(
            diff_fetches, 1,
            "one head must cost one diff download for both outputs: {hits}"
        );

        drafting_teardown();
    }

    /// **The pull requests you wrote yourself are read and reviewed, on one call.** The shape this
    /// whole feature was reported on: a fleet whose only open pull requests are the owner's, where
    /// every row answered "not summarised" for ever because the reader worked `Lane::NeedsYou` and
    /// a PR you opened is never in it (`src/prq.rs:1190`).
    ///
    /// The owner's decision, asked as read/draft/both and in which lane: **"both, in waiting, on
    /// the same call"** (SKEIN-265). So all three halves are asserted at once, and the third is the
    /// one that costs money if it is wrong:
    ///
    ///   * the summary is on disk for the authored PR;
    ///   * the drafted review is too, at the same head;
    ///   * and it was **ONE** model call and **ONE** diff download for both — not a summary now and
    ///     a review from the second door afterwards, which is the same row for two budget units.
    ///
    /// The draft #32 is the control: yours as well, in the same lane, and refused — because a draft
    /// is the author saying it is not finished, and that has nothing to do with who wrote it.
    #[cfg(unix)]
    #[test]
    fn your_own_pull_requests_are_read_and_reviewed_in_one_call() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = authored_fixture(home);

        let read = read_waiting();
        assert!(
            cached("mine", 31, "sha31").is_some(),
            "the pass read nothing on a queue of pull requests you opened — the surface that \
             reported this bug does nothing at all: {read:?}"
        );
        assert!(
            critiqued("mine", 31).is_some_and(|c| c.head_sha == "sha31"),
            "your own pull request was summarised and not reviewed — the decision was both: {read:?}"
        );

        // Your own DRAFT is still refused, and refused before the wire: no summary, no review, and
        // no diff downloaded to decide it with. `src/prq.rs:1190` puts it in `Lane::Waiting` beside
        // #31, so nothing but the draft test itself is keeping it out. Asserted BEFORE the call
        // count below, because a doorway that lost its draft rule shows up there as a second model
        // call, and "two calls" is the wrong sentence for it.
        assert!(
            cached("mine", 32, "sha32").is_none() && critiqued("mine", 32).is_none(),
            "a draft you opened was read — a draft is the author saying it is not finished"
        );

        // One call, and it was the MERGED one: the stub tags each invocation with the prompt shape
        // it saw, so "merged" twice or "merged" then "critique" both fail here rather than being
        // invisible in a passing disk assertion.
        let calls = std::fs::read_to_string(&asked).unwrap_or_default();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            vec!["merged"],
            "summary and review of your own PR must be ONE merged model call — one budget unit, \
             not two"
        );
        let hits = std::fs::read_to_string(home.join("hits")).unwrap_or_default();
        let diff_fetches = hits
            .lines()
            .filter(|l| l.starts_with("GET") && l.contains("/pulls/31 "))
            .count();
        assert_eq!(
            diff_fetches, 1,
            "one head must cost one diff download for both outputs: {hits}"
        );

        // And nothing was downloaded to decide it with either.
        assert_eq!(
            hits.lines()
                .filter(|l| l.starts_with("GET") && l.contains("/pulls/32 "))
                .count(),
            0,
            "a draft's diff was downloaded before it was refused: {hits}"
        );

        // And not again on the next pass: the reading and the draft on disk at this head are the
        // answer. Same money rule as every other door here.
        let _ = read_waiting();
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .count(),
            1,
            "your own pull request was re-read on the next pass — one model call every ten \
             minutes, for ever"
        );

        // **The live shape this was reported in** (SKEIN-265, the fleet check of 2026-08-25): a
        // stack whose rows were all read by hand — `Trigger::Asked`, which never consults
        // `worth_reading` — and one row that ended up with a summary and no review. The second
        // door is the whole fix, and it has to open for a pull request you opened yourself or that
        // row stays draftless until its head moves.
        std::fs::remove_dir_all(crate::prq::review_dir("mine").join("critiques")).unwrap();
        let _ = read_waiting();
        assert!(
            critiqued("mine", 31).is_some_and(|c| c.head_sha == "sha31"),
            "a pull request of yours that was already summarised never gets its review — the \
             second door does not open for your own rows"
        );
        // And it opens onto the SAME reading (SKEIN-263). It used to open onto a standalone
        // drafter — a second prompt over a second download, whose review had no reason to agree
        // with the summary already sitting on the row. `merged` twice is the whole assertion: the
        // second visit re-read the pull request and wrote both halves from that one answer.
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .collect::<Vec<_>>(),
            vec!["merged", "merged"],
            "the review was drafted by a second, separate analysis of the same commit"
        );
        // Both halves came out of that ONE call, checked by their own text — the identity
        // SKEIN-263 asks for. The fixture's model stamps each answer with the ordinal of the call
        // that produced it, so "reading 2" on the summary AND on the review is the proof that the
        // row's summary is the one this review was written beside. Under the standalone drafter
        // the summary stayed at "reading 1" while the review came from somewhere else entirely.
        let redrafted = cached("mine", 31, "sha31").expect("the re-read stored its summary");
        assert_eq!(
            redrafted.line, "reading 2 of this change.",
            "the row kept the summary from the FIRST reading while the review came from the second"
        );
        let review = critiqued("mine", 31).expect("the re-read stored its review");
        assert!(
            review.overall.contains("reading 2"),
            "the review came from a different reading than the summary beside it: {}",
            review.overall
        );

        drafting_teardown_for("mine");
    }

    /// Two repos, a budget that reaches neither the end of the first — and the row somebody else
    /// is blocked on lives in the SECOND (SKEIN-276).
    ///
    /// The lane ordering landed with SKEIN-265 and landed *inside* the `for repo in load_repos()`
    /// loop, which made it an ordering within one repository and left the outer order as the order
    /// of `repos.json`. Every pass restarts at the top of that file, so a repo whose unread rows
    /// outlast one pass — a stack you rebased this morning does it easily — means the repos below
    /// it are never reached at all. The row that never gets read is a colleague's review request,
    /// which is the exact row the ordering exists to put first.
    ///
    /// So: `busy` is registered first and holds four pull requests you opened; `quiet` is
    /// registered second and holds one waiting on your review. `READ_PER_PASS` is 3 lines and an
    /// authored visit reports two, so the pass stops inside `busy` — and the assertion is that
    /// `quiet`'s #51 was read anyway, and read FIRST.
    #[cfg(unix)]
    #[test]
    fn a_review_request_in_the_last_repo_is_read_before_a_stack_you_opened_in_the_first() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        two_repo_fixture(home);

        let read = read_waiting();
        assert!(
            read.iter().any(|l| l == "quiet: read #51"),
            "the review request in the last repo of the registry was never reached — the busy \
             repo above it spent the whole pass: {read:?}"
        );
        assert_eq!(
            read.first().map(String::as_str),
            Some("quiet: read #51"),
            "the pass spent its first unit on a repo's registry position rather than on the row \
             somebody is blocked on: {read:?}"
        );
        // And the ordering did not simply invert into "last repo first": the same pass goes on to
        // spend what is left on `busy`, which is where the rest of the budget belongs.
        assert!(
            read.iter().any(|l| l.starts_with("busy: read #")),
            "nothing in the busy repo was read at all — the fleet-wide order dropped a repo \
             instead of ranking it: {read:?}"
        );

        drafting_teardown_for("busy");
        crate::prq::invalidate("quiet");
    }

    /// A GitHub serving two repositories from one stub, keyed on the `repo:` term the batched
    /// search carries (`prq::one_request` builds `repo:{slug} is:pr is:open {search}`):
    /// `acme/busy` answers the `author:` alias with four pull requests you opened, `acme/quiet`
    /// answers the `review-requested:` alias with one waiting on you. Registered in that order,
    /// because the bug being asserted against is registry order.
    #[cfg(unix)]
    fn two_repo_fixture(home: &std::path::Path) {
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        let claude = home.join("claude-both.sh");
        std::fs::write(
            &claude,
            "#!/bin/sh\nfor a in \"$@\"; do p=\"$a\"; done\n# The second turn asks a different question and must get a different answer: handed\n# the merged text back, parse_critique reads its summary LINE: as a comment anchor\n# and the review grows a finding nobody wrote (SKEIN-393).\ncase \"$p\" in\n  *\"account for what it actually covered\"*) printf 'OVERALL: nothing new\\n'; exit 0;;\nesac\nprintf 'KIND: fix\\nLINE: a reading.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\nREVIEW:\\nOVERALL: nothing to flag\\nFILE: src/a.rs\\nLINE: 0\\nCOMMENT: about the change.\\n---\\n'\n",
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
                use std::io::Write as _;
                let mut stream = stream;
                let (head, body) = read_request(&stream);
                let committed =
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let node = |number: u64, author: &str, updated: &str| {
                    format!(
                        r#"{{"number":{number},"title":"t","url":"u",
                           "isDraft":false,"author":{{"login":"{author}"}},"headRefName":"feat",
                           "headRefOid":"sha{number}","baseRefName":"main",
                           "updatedAt":"{updated}","reviewDecision":"REVIEW_REQUIRED",
                           "latestReviews":{{"nodes":[]}},
                           "commits":{{"nodes":[{{"commit":{{"committedDate":"{committed}"}}}}]}}}}"#
                    )
                };
                let answer = if head.contains("/user/teams") {
                    "[]".to_string()
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if body.contains("repo:acme/busy") {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[{},{},{},{}]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(41, "me", "2024-01-04T00:00:00Z"),
                        node(42, "me", "2024-01-03T00:00:00Z"),
                        node(43, "me", "2024-01-02T00:00:00Z"),
                        node(44, "me", "2024-01-01T00:00:00Z"),
                    )
                } else if body.contains("repo:acme/quiet") {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(51, "someone", "2024-01-05T00:00:00Z"),
                    )
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string()
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

        // A real checkout behind both, for the same reason every other reader test has one: a repo
        // whose mirror cannot be read is summarised BLIND and blind summaries are never cached
        // (SKEIN-117), which would turn this ordering assertion into a caching one.
        let checkout = home.join("checkout");
        checkout_fixture(&checkout);
        let repo = |id: &str, slug: &str| {
            serde_json::from_value::<Repo>(serde_json::json!({
                "id": id,
                "source": format!("https://github.com/{slug}.git"),
                "source_tree": checkout.to_string_lossy(),
                "store": "",
                "read_prs": true,
            }))
            .unwrap()
        };
        crate::repos::save_repos(&[repo("busy", "acme/busy"), repo("quiet", "acme/quiet")])
            .unwrap();
        crate::prq::invalidate("busy");
        crate::prq::invalidate("quiet");
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
            author: author.into(),
            reasons,
            lane: crate::prq::Lane::NeedsYou,
            ..crate::prq::blank_pr(90, "sha90")
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

    /// **Asking for a review again re-runs the one reading** (SKEIN-263).
    ///
    /// The panel's "draft again" used to call a standalone drafter: its own prompt, its own diff
    /// download, its own budget unit. The summary already on the row stayed where it was, so the
    /// row could show a summary from one reading of a commit and, underneath it, a review from
    /// another — two analyses that never had to agree about what they saw, which is exactly the
    /// divergence merging summary and review was meant to end.
    ///
    /// The identity is asserted by TEXT, not by both halves merely existing: the fixture's model
    /// stamps every answer with the ordinal of the call that produced it, so "reading 2" on the
    /// summary AND on the review is the proof they came out of the same call. `["merged",
    /// "merged"]` is the second half of it — a `critique` line in that list would mean a standalone
    /// drafter had come back.
    ///
    /// Sabotage: make `critique` pass `Review::IfYours` and "the review a person pressed for was
    /// not re-drafted" fails — `worth_critiquing` says no to a head it has already drafted, which
    /// is precisely what "again" overrules. Make it pass `force: false` and "asking again handed
    /// back the reading already on disk" fails.
    #[cfg(unix)]
    #[test]
    fn asking_for_a_review_again_re_reads_rather_than_drafting_beside_the_old_summary() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = drafting_fixture(home);
        let day = utc_day();

        // The background pass reads #21 once: summary and review, one merged call.
        let _ = read_waiting();
        assert_eq!(
            cached("crit", 21, "sha21")
                .expect("the pass stored a summary")
                .line,
            "reading 1 of this change."
        );
        assert!(critiqued("crit", 21)
            .expect("and a review")
            .overall
            .contains("reading 1"));
        assert_eq!(reads_spent(&day), 1);

        // A person presses "draft again" on that row.
        let repo = crate::repos::load_repos()
            .into_iter()
            .find(|r| r.id == "crit")
            .unwrap();
        let pr = budget_pr(21, "sha21");
        let again = critique(&repo, "acme/thing", &pr, &["me".into()])
            .expect("the panel's draft-again works end to end");
        assert!(
            again.overall.contains("reading 2"),
            "the review a person pressed for was not re-drafted: {}",
            again.overall
        );
        assert_eq!(
            cached("crit", 21, "sha21")
                .expect("the re-read stored its summary")
                .line,
            "reading 2 of this change.",
            "asking again handed back the reading already on disk, or wrote a review beside it"
        );
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .collect::<Vec<_>>(),
            vec!["merged", "merged"],
            "the re-draft was a second, separate analysis of the same commit"
        );
        assert_eq!(
            reads_spent(&day),
            1,
            "a review a person asked for was charged to the day's automatic allowance"
        );
        // One reading, one download — the merged call's whole point. Two readings, two.
        let hits = std::fs::read_to_string(home.join("hits")).unwrap_or_default();
        assert_eq!(
            hits.lines()
                .filter(|l| l.starts_with("GET") && l.contains("/pulls/21 "))
                .count(),
            2,
            "a reading downloaded the diff more than once: {hits}"
        );

        drafting_teardown();
    }

    /// The two controls the pane offered differ in ONE respect, and this is it (SKEIN-293).
    ///
    /// The owner's question, live 2026-08-25: "when I click re read, does it give review as well?
    /// If so why is there separate re read and review the code buttons?" The answer from the code
    /// is that since the drafter was merged (SKEIN-263) both force a reading, download the diff
    /// once and spend one model call — and on a row that ALREADY has a draft, `summarise` keeps it
    /// and [`re_read_replacing_the_review`] throws it away. Nothing in either label said so.
    ///
    /// So it is asserted rather than described, on the one row where it is visible: the same pull
    /// request, at the same head, read twice. The stub numbers each reading, so "kept" and
    /// "replaced" are different strings rather than a judgement.
    ///
    /// The conservative one is the DEFAULT, and that is the load-bearing half: the drafted review
    /// is a thing the reader edits — kept comments, dropped comments — and a re-read that silently
    /// replaced it would destroy that vetting with no warning. The replacing read exists so the
    /// pane can offer it AFTER saying what it costs.
    #[cfg(unix)]
    #[test]
    fn re_reading_keeps_a_vetted_review_and_only_the_replacing_read_discards_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = drafting_fixture(home);

        // Reading 1: the pass reads #21 and drafts a review with it.
        let _ = read_waiting();
        assert!(
            critiqued("crit", 21)
                .expect("the pass drafted a review")
                .overall
                .contains("reading 1"),
            "the fixture did not produce a review to protect"
        );

        let repo = crate::repos::load_repos()
            .into_iter()
            .find(|r| r.id == "crit")
            .unwrap();
        let pr = budget_pr(21, "sha21");

        // "Re-read" — a forced reading through the DEFAULT door. The review the reader may have
        // vetted survives untouched. That is the property worth protecting.
        let again = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            true,
            Trigger::Asked,
        );
        assert!(
            critiqued("crit", 21)
                .expect("the review is still there")
                .overall
                .contains("reading 1"),
            "a plain re-read replaced a review the reader may have vetted, with no warning — the \
             exact harm the conservative default exists to prevent"
        );
        // And it is not even the same ANALYSIS. `worth_critiquing` says no here (a draft exists at
        // this head), so the visit falls to the cheap two-stage summary path — a different prompt,
        // on a weaker model — rather than the merged reading. The stub answers that path with a
        // fixed sentence and never touches the merged-call counter, which is how a test can tell
        // the two apart at all. So on a row that already has a draft the controls do not merely
        // differ in what they KEEP; they buy different readings, and neither label said either.
        assert_eq!(
            again.line, "it changes a thing.",
            "a re-read of an already-drafted row did not take the two-stage summary path"
        );
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .collect::<Vec<_>>(),
            vec!["merged"],
            "a re-read that kept the draft still paid for a merged reading"
        );

        // The replacing read. One merged call, and the draft is now the new one.
        let replacing = re_read_replacing_the_review(&repo, "acme/thing", &pr, &["me".into()]);
        assert_eq!(
            replacing.line, "reading 2 of this change.",
            "the replacing read did not take the merged path"
        );
        assert!(
            critiqued("crit", 21)
                .expect("and a review came with it")
                .overall
                .contains("reading 2"),
            "the replacing read did not replace the drafted review, so the pane's warning would \
             be about a loss that never happens"
        );
        assert_eq!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .collect::<Vec<_>>(),
            vec!["merged", "merged"],
            "the replacing read was not one merged analysis — a second drafter is back"
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
                    ownership_unknown: String::new(),
                    unread_because: String::new(),
                    not_reread: String::new(),
                    computed: true,
                    budget_stopped: false,
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
            ownership_unknown: String::new(),
            unread_because: String::new(),
            not_reread: String::new(),
            computed: true,
            budget_stopped: false,
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

    /// Run git in `dir`, with an identity, and refuse to continue if it failed.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// A committed checkout at `dir`, with `src/a.rs` and `web/b.js` — and no CODEOWNERS unless
    /// the test adds one and commits again.
    fn checkout_fixture(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("web")).unwrap();
        std::fs::write(dir.join("src").join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.join("web").join("b.js"), "// b").unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "one"]);
    }

    /// A repo registered against `checkout`, adopted in place, so its mirror reads from disk.
    fn repo_at(id: &str, checkout: &std::path::Path) -> Repo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "source": checkout.to_string_lossy(),
            "source_tree": checkout.to_string_lossy(),
            "store": "",
        }))
        .unwrap()
    }

    /// Ownership answers three ways, and the three are distinguishable: CODEOWNERS attributes, the
    /// repo genuinely has none, or the repo could not be read at all. The third used to answer as
    /// the second — `(vec![], 0)`, by its own doc "deliberately indistinguishable" — so a repo
    /// skein could not look at read as a repo nobody owns (SKEIN-117). All three still narrow
    /// nothing or narrow honestly; what they may no longer do is wear each other's sentence.
    #[test]
    fn ownership_tells_no_codeowners_from_a_repo_it_could_not_read() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        let paths = vec!["src/a.rs".to_string(), "web/b.js".to_string()];

        // Could not read: no mirror, and none can be made from a path that does not exist.
        let blind = repo_at(
            "blind",
            &(home.as_ref() as &std::path::Path).join("nowhere"),
        );
        match ownership(&blind, &["me".into()], &paths) {
            Ownership::Unreadable(why) => assert!(!why.is_empty(), "could-not-read must say why"),
            other => {
                panic!("an unreadable repo answered {other:?} instead of saying it could not look")
            }
        }

        // Genuinely none: the repo was read, and the absence is its own answer.
        let checkout = (home.as_ref() as &std::path::Path).join("checkout");
        checkout_fixture(&checkout);
        let repo = repo_at("readable", &checkout);
        assert_eq!(
            ownership(&repo, &["me".into()], &paths),
            Ownership::NoCodeowners,
            "a repo with no CODEOWNERS is the normal case, not a failure to look"
        );

        // Present: the split, with the paths that are yours named and the rest counted.
        std::fs::create_dir_all(checkout.join(".github")).unwrap();
        std::fs::write(checkout.join(".github").join("CODEOWNERS"), "src/ @me\n").unwrap();
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-q", "-m", "owners"]);
        crate::repos::fetch_mirror(&repo).unwrap();
        assert_eq!(
            ownership(&repo, &["me".into()], &paths),
            Ownership::Owned {
                yours: vec!["src/a.rs".into()],
                others: 1
            }
        );

        std::env::remove_var("SKEIN_HOME");
    }

    // ─────────────────── the day's analysis budget ───────────────────

    /// The ledger's own arithmetic, no model anywhere near it: units accumulate across repos into
    /// ONE fleet-wide day (spend in one repo counts against every repo — the owner's ceiling is
    /// "100 PRs a day", not per anything), the day rolling over restores the full allowance
    /// without waiting for it, and writing a new day prunes the old one out of the file.
    #[test]
    fn the_budget_is_one_fleet_wide_ledger_that_resets_by_day_key() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        note_read_spent("repo-a", "2026-08-23");
        note_read_spent("repo-a", "2026-08-23");
        note_read_spent("repo-b", "2026-08-23");
        assert_eq!(
            reads_spent("2026-08-23"),
            3,
            "two repos' analyses sum into the one fleet-wide day"
        );
        // The rollover, by key — no sleeping through midnight.
        assert_eq!(
            reads_spent("2026-08-24"),
            0,
            "a new day starts with the whole allowance"
        );
        note_read_spent("repo-a", "2026-08-24");
        assert_eq!(reads_spent("2026-08-24"), 1);
        let raw = std::fs::read_to_string(spend_path()).unwrap();
        assert!(
            !raw.contains("2026-08-23"),
            "writing a new day must prune the old one — the file is a tally, not a history: {raw}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// A cache hit spends nothing: the whole reason the counter lives on the server. The client's
    /// allowance counted requests, six cache hits ate it on every reload, and rows 7–29 were never
    /// read. Here the same request shape — summarise, unasked — answers from disk and the ledger
    /// does not move; the stub `claude` counts its invocations to prove the model was not even
    /// consulted.
    #[cfg(unix)]
    #[test]
    fn a_cache_hit_spends_none_of_the_days_budget() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        let asked = home.join("asked");
        let claude = home.join("claude-count.sh");
        std::fs::write(
            &claude,
            format!("#!/bin/sh\necho x >> {}\nprintf 'KIND: fix\\nLINE: x.\\nEXPAND: no\\nFLAGS: none\\n'\n", asked.display()),
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
        let pr = budget_pr(4, "abc");
        store(
            "demo",
            &Summary {
                number: 4,
                head_sha: "abc".into(),
                depth: Depth::Line,
                line: "already read".into(),
                detail: String::new(),
                flags: Vec::new(),
                signals: Vec::new(),
                yours: Vec::new(),
                others: 0,
                ownership_unknown: String::new(),
                unread_because: String::new(),
                not_reread: String::new(),
                computed: true,
                budget_stopped: false,
            },
        )
        .unwrap();

        let s = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(s.line, "already read", "the cache answered");
        assert_eq!(
            reads_spent(&utc_day()),
            0,
            "a summary served from disk decremented the budget — the exact client bug, reborn on \
             the server"
        );
        assert!(
            !asked.exists(),
            "the model was consulted for an answer already on disk"
        );

        for key in ["SKEIN_HOME", "SKEIN_REVIEW_AI", "SKEIN_CLAUDE_BIN"] {
            std::env::remove_var(key);
        }
    }

    /// **Nothing read off disk says it cost a model call** (SKEIN-292).
    ///
    /// [`cached`] has forced `computed = false` since the day a page reload spent the whole
    /// allowance on cache hits, with the reason written beside it. The rule is not about that one
    /// function though: it is about every way a stored reading comes back, and there were three
    /// readers and only two of them obeyed it. [`previous`] deserialised straight from the file, so
    /// a reading handed to the next prompt claimed to have cost something.
    ///
    /// Harmless the day it was found — nothing downstream read the flag — which is exactly the
    /// argument for pinning it now, because the day something does read it the failure is a budget
    /// that spends itself on remembering.
    ///
    /// The last assertion is the one that stops a FOURTH reader inheriting the bug instead of the
    /// rule: it counts the deserialisations in this file and insists each is followed by the line
    /// that forces the flag down.
    #[test]
    fn a_reading_off_disk_never_says_it_cost_a_model_call() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // Stored as it is stored for real: computed, because when it was written it was.
        store(
            "vintage",
            &Summary {
                number: 3,
                head_sha: "old".into(),
                depth: Depth::Line,
                line: "it changes a thing".into(),
                detail: String::new(),
                flags: Vec::new(),
                signals: Vec::new(),
                yours: Vec::new(),
                others: 0,
                ownership_unknown: String::new(),
                unread_because: String::new(),
                not_reread: String::new(),
                computed: true,
                budget_stopped: false,
            },
        )
        .unwrap();

        // The head has moved. The bulk payload still hands the reading over, marked stale — and
        // that is the path the item is about.
        let bulk = known("vintage", &[(3, "new".to_string())]);
        let row = bulk
            .get(&3)
            .expect("the earlier reading is still handed over");
        assert!(
            row.stale,
            "the reading is of an earlier commit and must say so"
        );
        assert!(
            !row.summary.computed,
            "a reading served from disk reported that it cost a model call — the exact bug \
             `cached`'s comment exists to prevent, one function along"
        );
        // The single-PR route goes the same way, and so does the reading at its own head.
        assert!(!held("vintage", 3, "new").summary.computed);
        assert!(
            !cached("vintage", 3, "old")
                .expect("stored at its own head")
                .computed
        );
        // And the lookup that fills the prompt's "what skein already said" slot.
        assert!(
            !previous("vintage", 3, "new")
                .expect("the earlier reading is what the next prompt is built on")
                .computed,
            "the reading handed to the next prompt claimed to have cost something"
        );

        // Every reader of a stored `Summary`, pinned. Asserted against the source because the
        // thing that goes wrong is a NEW one being written without the line — which no runtime
        // test of the three that exist could ever notice.
        let src = std::fs::read_to_string("src/review.rs").expect("this file");
        // Assembled at runtime so the needle never appears in this file as a literal — a source
        // check that matches its own search term counts itself, and then the number it reports is
        // about the test rather than about the code.
        let needle = format!("{}from_str::<Summary>", "serde_json::");
        let readers: Vec<String> = src
            .split(&needle)
            .skip(1)
            .map(|after| after.chars().take(600).collect())
            .collect();
        assert_eq!(
            readers.len(),
            3,
            "a reading is deserialised somewhere new; every one of them must force `computed` down"
        );
        for (i, reader) in readers.iter().enumerate() {
            assert!(
                reader.contains("computed = false"),
                "reader {i} hands back a stored summary still claiming it cost a model call: \
                 {reader}"
            );
        }

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A model that fails every time is bought once per commit, not once per row per reload**
    /// (SKEIN-253).
    ///
    /// The tried-note is what stops a failure being re-bought, and it used to gate the background
    /// pass alone. The pane's own pump sends no `asked` marker, so its requests are
    /// `Trigger::Unasked` and ARE charged — and a `claude` that is not logged in fails instantly
    /// and for free, so thirty rows over three reloads spent the day's ceiling on zero summaries,
    /// after which every row read "today's automatic reading budget is spent".
    ///
    /// Three asks, and each one is a different rule:
    ///
    ///   * the first spends a unit and asks the model, which is right — nothing knew yet;
    ///   * the second spends NOTHING, asks nothing, and comes back carrying what the model said;
    ///   * the third is a person pressing "read it", which goes nowhere near the note. A standing
    ///     failure must never make a button do nothing (`ai::forget_refusal`'s rule), and an asked
    ///     read is un-budgeted besides.
    #[cfg(unix)]
    #[test]
    fn a_model_that_always_fails_is_not_re_bought_on_every_reload() {
        let _g = crate::testutil::env_lock();
        let _hold = crate::github::HoldClear::new();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();

        // A GitHub that serves a diff, so the visit reaches the model rather than stopping at the
        // download — the failure this is about is the model's, and a transport failure is
        // deliberately NOT noted (`computed` is the boundary).
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let mut stream = stream;
                let _ = read_request(&stream);
                let body = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-fn a() {}\n+fn a() { b() }\n";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        std::env::set_var("SKEIN_GITHUB_API", &base);

        // A model that runs, answers, and answers nothing the parser knows — the shape of a
        // `claude` that is not logged in: instant, free, and a failure every single time. Counted,
        // because "it returned nothing" looks identical whether or not it was asked.
        let asked = home.join("asked");
        let claude = home.join("claude-broken.sh");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\necho x >> {}\nprintf 'no format here'\n",
                asked.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &claude,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .unwrap();
        std::env::set_var("SKEIN_CLAUDE_BIN", &claude);

        let repo: crate::repos::Repo = serde_json::from_value(serde_json::json!({
            "id": "burn", "source": "https://github.com/acme/thing.git",
            "source_tree": "", "store": "", "read_prs": true,
        }))
        .unwrap();
        let pr = budget_pr(4, "abc");
        let calls = || {
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .count()
        };

        let first = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert!(
            matches!(first.depth, Depth::Unread) && first.computed,
            "{first:?}"
        );
        assert_eq!(
            calls(),
            1,
            "the first ask must reach the model — nothing knew anything yet"
        );
        assert_eq!(
            reads_spent(&utc_day()),
            1,
            "a spent model call must be counted, however fast it failed"
        );

        // The reload. Same row, same head, and this is the one that used to cost a unit per row.
        let again = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(
            calls(),
            1,
            "the model was asked again about a commit it had already refused — one budget unit per \
             row per reload, until the day's ceiling is gone and no row has a summary"
        );
        assert_eq!(
            reads_spent(&utc_day()),
            1,
            "the reload charged the day for a reading nobody bought"
        );
        assert!(
            !again.computed,
            "a refusal served from a note must not be reported as a reading that cost something"
        );
        assert!(
            again.unread_because.contains("could not make sense")
                && again.unread_because.contains("read it"),
            "the row must carry what the MODEL said, and how to try again — not a budget number \
             that hides it: {}",
            again.unread_because
        );
        assert!(
            !again.budget_stopped,
            "a standing failure was reported as the day's budget running out"
        );

        // And a person pressing the button goes nowhere near any of it.
        let pressed = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_eq!(
            calls(),
            2,
            "\"read it\" did nothing — a standing failure must never make a button dead"
        );
        assert!(matches!(pressed.depth, Depth::Unread));
        assert_eq!(
            reads_spent(&utc_day()),
            1,
            "an asked read was charged to the automatic allowance"
        );

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

    /// The durable half of SKEIN-117: a summary computed while the repo could not be read is
    /// SERVED — the person still gets their summary now — but never cached, so a mirror that
    /// recovers is consulted on the next computation. Under the old unconditional cache write,
    /// the blind answer ("yours: none", marked with nothing) froze for the life of the head:
    /// restore `let _ = store(&repo.id, &summary);` without the `ownership_unknown` guard and
    /// two assertions below fail — `cached(..).is_none()` after the blind visit, and the
    /// recovered visit's `yours`, which the stale cache answers with the blind emptiness.
    ///
    /// The visit is a PERSON pressing "read it" (`Trigger::Asked`). It used to be `Unasked`, on a
    /// pull request whose only reason is that somebody mentioned you, in a repo with read-ahead
    /// off — which is precisely the visit SKEIN-242 says skein must refuse, and `unasked_scope`
    /// now does. What the test is about is blindness, not scope, so it presses the button.
    ///
    /// The ledger is asserted at zero for the same reason: an asked read is never counted. That
    /// not caching has a PRICE — a second visit recomputes rather than serving the blind answer —
    /// is what the recovered visit's own `yours` proves, and what an unasked visit costs for it is
    /// `one_analysed_pull_request_is_one_unit_whatever_it_produced`'s assertion, not this one's.
    #[cfg(unix)]
    #[test]
    fn a_summary_computed_blind_is_served_but_never_cached() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
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
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();

        // A GitHub that answers: the diff for #5, and its changed files — one the CODEOWNERS
        // below will attribute, one it will not.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let mut stream = stream;
                let (head, _) = read_request(&stream);
                let answer = if head.contains("/pulls/5/files") {
                    r#"[{"filename":"src/a.rs"},{"filename":"web/b.js"}]"#.to_string()
                } else if head.contains("/pulls/5 ") {
                    "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n".to_string()
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

        // The repo's checkout does not exist yet: the mirror cannot be made, so skein is blind.
        let checkout = home.join("checkout");
        let repo = repo_at("heals", &checkout);
        let mut pr = budget_pr(5, "sha5");
        // Mentioned, not a reviewer: the review is not yours to give, so the visit takes the
        // two-stage path and the stage-1 stub above answers it.
        pr.reasons = vec![crate::prq::Reason::Mentioned];

        let blind = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_ne!(
            blind.depth,
            Depth::Unread,
            "a readable diff must still be summarised while the repo is not: {}",
            blind.unread_because
        );
        assert!(
            !blind.ownership_unknown.is_empty(),
            "the summary must say ownership was not consulted, not draw nothing owned"
        );
        assert!(
            cached("heals", 5, "sha5").is_none(),
            "the blind summary was written down — a recovered mirror can never correct it"
        );
        assert_eq!(
            reads_spent(&utc_day()),
            0,
            "a read a person pressed for was counted against the day's automatic allowance"
        );

        // The mirror recovers: the checkout appears, CODEOWNERS and all.
        checkout_fixture(&checkout);
        std::fs::create_dir_all(checkout.join(".github")).unwrap();
        std::fs::write(checkout.join(".github").join("CODEOWNERS"), "src/ @me\n").unwrap();
        git(&checkout, &["add", "-A"]);
        git(&checkout, &["commit", "-q", "-m", "owners"]);

        let healed = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert!(
            healed.ownership_unknown.is_empty(),
            "the recovered mirror was not consulted: {}",
            healed.ownership_unknown
        );
        assert_eq!(
            healed.yours,
            vec!["src/a.rs".to_string()],
            "the recomputed summary must carry what CODEOWNERS actually attributes"
        );
        assert_eq!(healed.others, 1);
        assert!(
            cached("heals", 5, "sha5").is_some(),
            "a summary computed with everything consulted must be cached as ever"
        );
        assert_eq!(
            reads_spent(&utc_day()),
            0,
            "two reads a person pressed for ate into the automatic allowance"
        );

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

    /// At the ceiling the model is NEVER reached for unasked work, and the row carries the
    /// invitation: the budget sentence with the numbers, the `budget_stopped` marker the pane
    /// keys the prominent read button on, `computed: false` (nothing was spent saying no), and
    /// no count moved. Both spending paths — the summary visit and the standalone draft — refuse
    /// through the same gate.
    #[cfg(unix)]
    #[test]
    fn at_the_ceiling_unasked_work_never_reaches_the_model_and_the_row_invites_the_button() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = drafting_fixture(home);
        std::fs::write(
            crate::config::skein_home().join("config.json"),
            br#"{"review_reads_per_day":2}"#,
        )
        .unwrap();
        let day = utc_day();
        note_read_spent("elsewhere", &day);
        note_read_spent("elsewhere", &day);

        // The background pass finds a full queue and reads NOTHING.
        let read = read_waiting();
        assert!(
            read.is_empty(),
            "over budget, and the pass still read: {read:?}"
        );
        assert!(
            !asked.exists(),
            "the summariser was reached with the day's budget spent"
        );
        assert!(cached("crit", 21, "sha21").is_none());
        assert!(critiqued("crit", 21).is_none());

        // The unasked per-row request — the pane's pump — gets the honest refusal, shaped for
        // the affordance.
        let pr = budget_pr(21, "sha21");
        let s = summarise(
            &crate::repos::load_repos()
                .into_iter()
                .find(|r| r.id == "crit")
                .unwrap(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(s.depth, Depth::Unread);
        assert!(
            s.budget_stopped,
            "the machine-readable marker the pane keys the button on is missing"
        );
        assert!(
            s.unread_because.contains("press read") && s.unread_because.contains("(2/2)"),
            "the refusal must invite the manual trigger, with the numbers: {}",
            s.unread_because
        );
        assert!(
            !s.computed,
            "saying 'budget spent' must not itself count as spending"
        );
        assert_eq!(reads_spent(&day), 2, "a refusal moved the counter");

        drafting_teardown();
    }

    /// The owner's boundary, both halves: "Limit is only for automatic stuff, manually I can
    /// invoke as many as I want." At 2/2 an ASKED analysis still runs — the model is reached and
    /// a real summary (and its merged draft) comes back — and the ledger stays at 2 afterwards:
    /// a manual call is never refused for budget and never eats the automatic allowance.
    #[cfg(unix)]
    #[test]
    fn an_asked_analysis_ignores_the_ceiling_and_leaves_it_untouched() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let asked = drafting_fixture(home);
        std::fs::write(
            crate::config::skein_home().join("config.json"),
            br#"{"review_reads_per_day":2}"#,
        )
        .unwrap();
        let day = utc_day();
        note_read_spent("elsewhere", &day);
        note_read_spent("elsewhere", &day);

        let repo = crate::repos::load_repos()
            .into_iter()
            .find(|r| r.id == "crit")
            .unwrap();
        let pr = budget_pr(21, "sha21");
        let s = summarise(
            &repo,
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_ne!(
            s.depth,
            Depth::Unread,
            "a manual call was refused for budget: {}",
            s.unread_because
        );
        assert!(
            std::fs::read_to_string(&asked)
                .unwrap_or_default()
                .lines()
                .count()
                >= 1,
            "the model was never reached for the asked call"
        );
        assert_eq!(
            reads_spent(&day),
            2,
            "the manual call ate the automatic allowance — asked work must not be counted"
        );

        drafting_teardown();
    }

    /// **What skein reads on its own is skein's answer, not the caller's** (SKEIN-242).
    ///
    /// `read_prs` and [`worth_reading`] used to live in [`read_waiting`] alone, which made them a
    /// rule the BACKGROUND obeyed rather than a rule about the repo. `GET /review/:n/summary`
    /// without an `asked` marker went straight to [`summarise`], which consulted neither: the
    /// pane's pump therefore read pull requests in repos where read-ahead was switched off, and
    /// pull requests whose only reason is that somebody mentioned you, and CHARGED the day's
    /// ledger for both. The budget check was moved server-side because a budget the client holds
    /// is a budget any client action can refill; the scope is here for the same reason.
    ///
    /// Both refusals are asserted on the WIRE as well as in the answer — a refused visit must not
    /// even download the diff — and both are followed by the same visit with the button pressed,
    /// because a scope that also gated a person pressing "read it" would be the opposite bug.
    ///
    /// Sabotage: delete the `!repo.read_prs` arm of `unasked_scope` and
    /// "read-ahead off: an unasked visit is refused" fails; delete the `worth_reading` arm and
    /// "a mention is not a reason to read on skein's initiative" fails; return `None` for
    /// `Trigger::Asked` unconditionally-in-reverse (drop the early return) and both
    /// "…and the same visit, pressed, reads it" assertions fail.
    #[cfg(unix)]
    #[test]
    fn skein_reads_on_its_own_only_where_you_switched_reading_on() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let _asked = drafting_fixture(home);
        let day = utc_day();
        let load = || {
            crate::repos::load_repos()
                .into_iter()
                .find(|r| r.id == "crit")
                .expect("the fixture repo")
        };
        let diff_reads = |n: u64| {
            std::fs::read_to_string(home.join("hits"))
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with("GET") && l.contains(&format!("/pulls/{n} ")))
                .count()
        };

        // ---- read-ahead OFF, on a pull request squarely inside the scope ----
        crate::repos::set_read_prs("crit", false).unwrap();
        let pr = budget_pr(21, "sha21");
        let s = summarise(
            &load(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(
            s.depth,
            Depth::Unread,
            "read-ahead off: an unasked visit is refused"
        );
        assert!(
            s.unread_because.contains("read ahead"),
            "the refusal must name the switch that would change it: {}",
            s.unread_because
        );
        assert!(
            !s.budget_stopped,
            "a scope refusal wore the budget marker — the pane would offer tomorrow to somebody \
             whose repo will still be switched off tomorrow"
        );
        assert!(!s.computed, "saying no must not count as reading");
        assert_eq!(reads_spent(&day), 0, "a refusal moved the ledger");
        assert!(cached("crit", 21, "sha21").is_none());
        assert_eq!(
            diff_reads(21),
            0,
            "a refused visit still downloaded the diff"
        );

        // ...and the same visit, pressed, reads it — and is still free.
        let s = summarise(
            &load(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_ne!(
            s.depth,
            Depth::Unread,
            "read-ahead off refused a person pressing read it: {}",
            s.unread_because
        );
        assert_eq!(
            reads_spent(&day),
            0,
            "a read a person pressed for ate the automatic allowance"
        );

        // ---- read-ahead ON, but the only reason is that somebody mentioned you ----
        crate::repos::set_read_prs("crit", true).unwrap();
        let mut pr = budget_pr(22, "sha22");
        pr.reasons = vec![crate::prq::Reason::Mentioned];
        let s = summarise(
            &load(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Unasked,
        );
        assert_eq!(
            s.depth,
            Depth::Unread,
            "a mention is not a reason to read on skein's initiative"
        );
        assert!(
            s.unread_because.contains("not one skein reads on its own"),
            "the refusal must say whose scope this is: {}",
            s.unread_because
        );
        assert_eq!(reads_spent(&day), 0, "a mention was charged to the day");
        assert_eq!(
            diff_reads(22),
            0,
            "a refused visit still downloaded the diff"
        );

        // ...and the same visit, pressed, reads it.
        let s = summarise(
            &load(),
            "acme/thing",
            &pr,
            &["me".into()],
            false,
            Trigger::Asked,
        );
        assert_ne!(
            s.depth,
            Depth::Unread,
            "a mention could not be read even by hand: {}",
            s.unread_because
        );
        assert_eq!(reads_spent(&day), 0);

        drafting_teardown();
    }

    /// One unasked analysis is exactly one unit — the merged visit produced a summary AND a
    /// drafted review, and the ledger moved by one, attributed to the repo that spent it.
    #[cfg(unix)]
    #[test]
    fn one_analysed_pull_request_is_one_unit_whatever_it_produced() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        let _asked = drafting_fixture(home);

        let _ = read_waiting();
        assert!(cached("crit", 21, "sha21").is_some());
        assert!(critiqued("crit", 21).is_some());
        assert_eq!(
            reads_spent(&utc_day()),
            1,
            "summary plus drafted review is ONE analysed pull request, not two units"
        );
        // And the attribution names the repo.
        let raw = std::fs::read_to_string(spend_path()).unwrap();
        assert!(
            raw.contains("crit"),
            "the ledger lost the attribution: {raw}"
        );

        drafting_teardown();
    }

    /// The budget is spent from the TOP of the PANE, lane by lane. Within **your move**, oldest-
    /// waiting first (`revWaitedSince` in index.html, mirrored by `waited_since`) — never
    /// newest-number and never most-recently-touched. Three rows there, budget for two: the two
    /// that have waited longest are read, the most recently touched is not.
    ///
    /// And a fourth row that is **yours** — read now (SKEIN-265), sitting in the waiting lane, and
    /// older than every other row — comes AFTER all of them, however long it has been there. That
    /// is the ordering half of widening the scope: a colleague blocked on your review must not end
    /// up behind a stack you opened this morning, and `waited_since` alone would have put your own
    /// pull request first.
    #[cfg(unix)]
    #[test]
    fn the_budget_is_spent_on_the_lanes_oldest_waiting_rows_first() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
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
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let (head, body) = read_request(&stream);
                // Three PRs waiting on your review. updatedAt is the waited-since key here (no
                // review of yours on any): #31 has waited longest, then #33, then #32 — while
                // the NUMBER order and the most-recently-touched order both put #32/#33 first.
                let node = |number: u64, updated: &str, author: &str| {
                    format!(
                        r#"{{"number":{number},"title":"t","url":"u",
                           "isDraft":false,"author":{{"login":"{author}"}},"headRefName":"feat",
                           "headRefOid":"sha{number}","baseRefName":"main",
                           "updatedAt":"{updated}","reviewDecision":"REVIEW_REQUIRED",
                           "latestReviews":{{"nodes":[]}},
                           "commits":{{"nodes":[{{"commit":{{"committedDate":"2020-01-01T00:00:00Z"}}}}]}}}}"#
                    )
                };
                let answer = if head.contains("/user/teams") {
                    "[]".to_string()
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if body.contains("review-requested:") {
                    format!(
                        r#"{{"data":{{"q0":{{"nodes":[{},{},{}]}},"q1":{{"nodes":[]}},"q2":{{"nodes":[{}]}},"q3":{{"nodes":[]}}}}}}"#,
                        node(31, "2020-01-01T00:00:00Z", "someone"),
                        node(32, "2020-01-03T00:00:00Z", "someone"),
                        node(33, "2020-01-02T00:00:00Z", "someone"),
                        // Yours, and the oldest row in the queue by a year.
                        node(34, "2019-01-01T00:00:00Z", "me"),
                    )
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string()
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
        // A readable checkout behind the repo, as in `drafting_fixture` and for the same reason:
        // this test asserts WHICH rows landed in the cache, and a repo whose mirror cannot be
        // read has its summaries served without being cached (SKEIN-117).
        let checkout = home.join("checkout");
        checkout_fixture(&checkout);
        crate::repos::save_repos(&[serde_json::from_value(serde_json::json!({
            "id": "ord", "source": "https://github.com/acme/thing.git",
            "source_tree": checkout.to_string_lossy(), "store": "", "read_prs": true,
        }))
        .unwrap()])
        .unwrap();
        crate::prq::invalidate("ord");
        std::fs::write(
            crate::config::skein_home().join("config.json"),
            br#"{"review_reads_per_day":2}"#,
        )
        .unwrap();
        // Every head already has its drafted review, so each visit is summary-only: WHICH rows
        // are read is then purely the ordering under test, with one unit per row.
        for n in [31u64, 32, 33, 34] {
            store_critique(
                "ord",
                &mut Critique {
                    number: n,
                    head_sha: format!("sha{n}"),
                    overall: "nothing to flag".into(),
                    comments: Vec::new(),
                    truncated: false,
                    written_at: String::new(),
                    posted: None,
                },
            )
            .unwrap();
        }

        let _ = read_waiting();
        assert!(
            cached("ord", 31, "sha31").is_some(),
            "the row that waited longest was skipped"
        );
        // Before the rest: the oldest row in the whole queue is the one YOU opened, and it is not
        // what the budget bought. Asserted here so a lost lane rank says whose row took the money.
        assert!(
            cached("ord", 34, "sha34").is_none(),
            "a pull request YOU opened took the day's budget from a colleague's review request — \
             your own rows are read with what is left, not first"
        );
        assert!(
            cached("ord", 33, "sha33").is_some(),
            "the second-longest waiting row was skipped"
        );
        assert!(
            cached("ord", 32, "sha32").is_none(),
            "the most recently touched row was read ahead of the queue's top — the budget is \
             being spent from the wrong end"
        );

        for key in [
            "SKEIN_HOME",
            "SKEIN_REVIEW_AI",
            "SKEIN_CLAUDE_BIN",
            "SKEIN_GITHUB_API",
            "GH_TOKEN",
        ] {
            std::env::remove_var(key);
        }
        crate::prq::invalidate("ord");
        crate::prq::forget_host_token();
    }

    /// A Pr literal for budget tests — the queue's own fields, one place.
    fn budget_pr(number: u64, head: &str) -> crate::prq::Pr {
        crate::prq::Pr {
            reasons: vec![crate::prq::Reason::Reviewer],
            lane: crate::prq::Lane::NeedsYou,
            ..crate::prq::blank_pr(number, head)
        }
    }

    // ── the reviewer stands in the change (SKEIN-395) ─────────────────────────────────────────
    //
    // Measured, not assumed: the same prompt over the same diff, run twice with only the working
    // directory different — no checkout gave ZERO tool calls in one turn ($0.56); a checkout gave
    // 30 calls over 31 turns ($1.58), reading the changed file around each hunk and following the
    // caller into another file. The owner chose the depth. What these hold is the safety property
    // that makes it worth having: exactly this commit, or nothing.

    /// Two commits, and the reviewer sees the one being reviewed — including after the branch
    /// moves, which is the round-two case and the one where a leftover file reads as part of the
    /// change.
    #[test]
    fn the_reviewer_stands_in_the_commit_being_reviewed_and_not_the_one_before_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let (repo, first, second) = a_repo_with_two_commits(home);

        let (_, at) = super::conversation_of(&repo, 7, &first);
        assert_eq!(
            fs::read_to_string(at.join("only-in-first.txt"))
                .ok()
                .as_deref(),
            Some("one\n"),
            "the reviewer is not standing in the commit it was asked to review — with nothing to \
             read it makes no tool calls at all, which is the whole of what this buys"
        );

        // **Something the reviewer itself left.** `claude` writes into the directory it runs in —
        // scratch files, a `.claude` of its own — and a round that inherits the last round's litter
        // shows it to the reviewer as part of the change. Tracked deletions are git's job and it
        // does them; this is the part that is nobody's unless it is asked for.
        fs::write(at.join("scratch-from-the-last-round.txt"), "litter\n").unwrap();

        // The branch moves. `only-in-first.txt` is deleted in the second commit, and a checkout
        // that left it behind would show the reviewer a file this change does not contain.
        let (_, again) = super::conversation_of(&repo, 7, &second);
        assert_eq!(
            again, at,
            "the checkout moved, so the conversation moved with it and every earlier round is \
             filed where the next resume will not look (SKEIN-376)"
        );
        assert!(
            at.join("only-in-second.txt").exists(),
            "the second commit's own file is missing, so the reviewer is reading the commit before \
             the one under review"
        );
        assert!(
            !at.join("only-in-first.txt").exists(),
            "a file this commit deletes is still sitting in the checkout, so the reviewer reads it \
             as part of the change — that is a review confidently wrong about the code, which is \
             worse than no checkout at all"
        );
        assert!(
            !at.join("scratch-from-the-last-round.txt").exists(),
            "an untracked file from the previous round is still in the checkout, so the reviewer \
             reads litter as part of the change — the same wrongness as a stale tracked file, and \
             the one git will not clear on its own"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// **A commit that landed since the mirror was last fetched is still stood up** — found on the
    /// rig (2026-08-27), where it was the difference between this feature working and quietly doing
    /// nothing at all.
    ///
    /// The checkout's origin is the MIRROR, so fetching it only asks a mirror that may itself be
    /// behind, and nothing on the reading path refreshes one. A pull request pushed since the last
    /// `skein pull` therefore had no branch anywhere skein could see, the checkout stayed empty,
    /// and the reviewer silently went back to reading the diff alone — with no failure anywhere,
    /// which is why only standing it up against a real repository caught it.
    #[test]
    fn a_commit_pushed_since_the_mirror_was_fetched_is_still_stood_up() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let (repo, _, second) = a_repo_with_two_commits(home);
        // A reading happens, so the mirror and the checkout both exist and are current.
        let (_, at) = super::conversation_of(&repo, 7, &second);
        assert!(
            at.join("only-in-second.txt").exists(),
            "the fixture never stood up"
        );

        // Now somebody pushes. The mirror knows nothing about it — exactly the rig's state, where
        // the mirror was sixteen hours old and did not carry the head of the pull request skein
        // was reading.
        let src = home.join("origin");
        let git_src = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&src)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        fs::write(src.join("pushed-after-the-mirror.txt"), "three\n").unwrap();
        git_src(&["add", "-A"]);
        git_src(&["commit", "-qm", "three"]);
        let third = git_src(&["rev-parse", "HEAD"]);

        let (_, again) = super::conversation_of(&repo, 7, &third);
        assert_eq!(again, at, "the conversation's address moved");
        assert!(
            at.join("pushed-after-the-mirror.txt").exists(),
            "a commit pushed since the mirror was last fetched left the checkout empty, so the \
             reviewer reads the diff alone and the whole checkout does nothing — and nothing \
             anywhere fails, which is why this is a test and not a bug report"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// **A commit skein cannot get is an EMPTY directory, never the wrong one.** A pull request
    /// from a fork has no branch in the mirror; standing the reviewer in the base branch and
    /// letting it believe that is the change is SKEIN-395's second possibility, the failure hardest
    /// to notice and worst for trust.
    #[test]
    fn a_commit_the_mirror_does_not_have_leaves_nothing_rather_than_the_wrong_code() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");

        let (repo, first, _) = a_repo_with_two_commits(home);
        let (_, at) = super::conversation_of(&repo, 9, &first);
        assert!(
            at.join("only-in-first.txt").exists(),
            "the fixture never stood up"
        );

        // A head skein was told about and the mirror has never heard of — a fork's.
        let (_, same) = super::conversation_of(&repo, 9, &"b".repeat(40));
        assert_eq!(same, at, "the conversation's address moved");
        assert!(
            !at.join("only-in-first.txt").exists(),
            "a pull request whose commit skein could not get was reviewed against whatever the \
             checkout happened to hold — the reviewer describes code that is not in this change"
        );
        assert!(
            at.is_dir(),
            "the directory itself was removed, taking every earlier round of the conversation \
             filed under it (SKEIN-376)"
        );

        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// A repo skein has mirrored, with two commits: the first adds a file the second deletes.
    fn a_repo_with_two_commits(home: &std::path::Path) -> (Repo, String, String) {
        let src = home.join("origin");
        fs::create_dir_all(&src).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&src)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        fs::write(src.join("only-in-first.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "one"]);
        let first = git(&["rev-parse", "HEAD"]);
        fs::remove_file(src.join("only-in-first.txt")).unwrap();
        fs::write(src.join("only-in-second.txt"), "two\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "two"]);
        let second = git(&["rev-parse", "HEAD"]);

        let repo: Repo = serde_json::from_value(serde_json::json!({
            "id": "acme",
            "source": src.to_string_lossy(),
            "source_tree": src.to_string_lossy(),
            "store": "",
        }))
        .unwrap();
        crate::repos::ensure_mirror(&repo).expect("the fixture repo is mirrored");
        (repo, first, second)
    }

    // ── the round gate (SKEIN-379) ────────────────────────────────────────────────────────────
    //
    // Rounds run unasked and nothing counts them, so this is the only thing between automatic
    // re-reading and unbounded spend. It is the FIRST TURN of the round rather than a call beside
    // it: cheap when the answer is no, because the context is warm and the output is one line, and
    // free when the answer is yes, because that same turn produces the round.

    /// The gate's refusal is read only where it is the whole answer. A review is entitled to
    /// contain those words in its prose, and a gate that matched anywhere would throw away a review
    /// that had just been paid for.
    #[test]
    fn the_gate_refuses_a_round_only_when_that_is_the_entire_answer() {
        assert_eq!(
            super::no_round("NO-ROUND: a rebase, nothing that changes the review.").as_deref(),
            Some("a rebase, nothing that changes the review."),
            "the gate said no and skein did not hear it, so a typo push bought a whole round"
        );
        assert_eq!(
            super::no_round("  NO-ROUND\nignored").as_deref(),
            Some(""),
            "a bare refusal with no sentence was not read as a refusal at all"
        );
        assert!(
            super::no_round(
                "KIND: fix\nLINE: it moves a thing.\nREVIEW:\nOVERALL: there is NO-ROUND for this \
                 in the design.\n"
            )
            .is_none(),
            "a review that used those words in its own prose was thrown away as a refusal — the \
             round was paid for and then discarded"
        );
    }

    /// The paragraph carries the owner's rule, not a paraphrase of it. Each clause below is one he
    /// gave, and a gate missing any of them judges by a different rule than the one he stated.
    #[test]
    fn the_gate_asks_what_the_owner_asked_it_to_ask() {
        let g = super::gate_paragraph("9c1de07abc", "4f2ab1cdef");
        for (needle, why) in [
            (
                "9c1de07",
                "the gate is not told which commit it read, so it cannot say what moved",
            ),
            ("4f2ab1c", "the gate is not told which commit it is judging"),
            (
                "stopped moving",
                "the gate is not told to wait for the change to settle — the \
                owner's \"wait till there is enough or till things stabilized\"",
            ),
            (
                "still working",
                "the gate is not told that a series of commits means somebody is \
                mid-flight, so it will run a round on every push",
            ),
            (
                "author",
                "the gate is not told that a comment from the author usually means they \
                are done, which is the owner's own signal for a round being due",
            ),
            (
                "NO-ROUND",
                "the gate is not told how to refuse, so a refusal arrives in a shape \
                nothing reads and the round is bought anyway",
            ),
        ] {
            assert!(g.contains(needle), "{why}: {g}");
        }
    }

    /// The whole of it, driven through a real spawn: a round the gate turns down keeps the reading
    /// skein had, says why, and is filed under the commit it did NOT read so the next poll is free.
    #[test]
    fn a_round_that_is_not_worth_running_keeps_the_reading_it_had_and_is_asked_once() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testutil::env_lock();
        crate::ai::forget_refusal();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_REVIEW_AI", "on");
        std::env::set_var("HOME", home);

        // What skein said last time, at the commit the branch has since moved off.
        let mut before = super::Summary::unread(7, "9c1de07abc", "");
        before.depth = super::Depth::Line;
        before.line = "adds a bounds check the caller already makes.".into();
        super::store("acme", &before).unwrap();

        // A stub that refuses the round when the gate paragraph is there, and reviews when it is
        // not — so the SAME binary proves both directions, and the assertions cannot pass because
        // the model always says one thing. It scans its arguments rather than counting them
        // (SKEIN-396).
        let bin = home.join("claude");
        std::fs::write(
            &bin,
            "#!/bin/sh\nfor a in \"$@\"; do p=\"$a\"; done\ncase \"$p\" in\n\
             \x20 *\"account for what it actually covered\"*) printf 'OVERALL: nothing new\\n';;\n\
             \x20 *\"You have read this pull request before\"*) printf 'NO-ROUND: a rebase and a \
             comment typo, nothing that changes the review.\\n';;\n\
             \x20 *) printf 'KIND: fix\\nLINE: a fresh reading.\\nEXPAND: no\\nFLAGS: \
             none\\nDETAIL:\\nnone\\nREVIEW:\\nOVERALL: nothing to flag\\n';;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("SKEIN_CLAUDE_BIN", &bin);

        let repo = repo_at("acme", home);
        let pr = crate::prq::blank_pr(7, "4f2ab1cdef");
        let out = super::summarise_and_draft(
            &repo,
            &pr,
            &super::Ownership::NoCodeowners,
            &[],
            "diff --git a/x b/x\n",
            super::Trigger::Unasked,
        );

        assert_eq!(
            out.line, before.line,
            "the gate turned the round down and skein threw the reading away anyway — the reader \
             is left with nothing where they had a review"
        );
        assert!(
            out.not_reread.contains("4f2ab1c") && out.not_reread.contains("rebase"),
            "the row does not say WHICH commit went unread or why, so a deliberate choice reads as \
             neglect: {:?}",
            out.not_reread
        );
        assert_eq!(
            out.head_sha, "9c1de07abc",
            "the kept reading was relabelled as describing the commit it never read, so the row \
             stops reporting itself as stale and the reader cannot tell"
        );
        assert!(
            super::cached("acme", 7, "4f2ab1cdef").is_some(),
            "nothing was filed under the commit the gate judged, so the next poll asks the gate \
             again — and the round the gate exists to save is spent on asking whether to save it"
        );

        // **A press is never rationed** — the owner has said so twice. `Asked` skips the gate.
        let asked = super::summarise_and_draft(
            &repo,
            &pr,
            &super::Ownership::NoCodeowners,
            &[],
            "diff --git a/x b/x\n",
            super::Trigger::Asked,
        );
        assert_eq!(
            asked.line, "a fresh reading.",
            "somebody pressed read and the gate answered instead of the review — what the reader \
             asks for is never rationed"
        );

        for key in ["SKEIN_HOME", "SKEIN_REVIEW_AI", "SKEIN_CLAUDE_BIN", "HOME"] {
            std::env::remove_var(key);
        }
        crate::ai::forget_refusal();
    }

    // ── a review that already went (SKEIN-397) ────────────────────────────────────────────────
    //
    // Found on the rig against real GitHub, not in a test: post, receipt written, post again, TWO
    // identical reviews on the pull request. The receipt existed the whole time and nothing read it.

    fn posted_draft(repo: &str, number: u64, head: &str, as_verdict: &str) {
        let mut c = super::Critique {
            number,
            head_sha: head.into(),
            overall: "one real problem.".into(),
            comments: Vec::new(),
            truncated: false,
            written_at: "2026-08-26T17:00:00Z".into(),
            posted: Some(super::Posted {
                at: "2026-08-26T17:39:32Z".into(),
                onto: "2463ac17d1d96fc40d0f16319a73bfac19fecbd8".into(),
                as_verdict: as_verdict.into(),
            }),
        };
        super::store_critique(repo, &mut c).expect("the fixture draft is written");
    }

    /// The whole of SKEIN-397: the same review, sent as the same thing, does not go a second time —
    /// and the refusal carries what the receipt knows, because a door that will not open and will
    /// not say why is worse than one that does neither.
    #[test]
    fn a_review_already_sent_as_this_does_not_go_to_github_twice() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        posted_draft("acme", 22, "2463ac17d1d9", "comment");

        let said = super::already_sent("acme", 22, "2463ac17d1d9", crate::prq::Verdict::Comment)
            .expect(
                "a review skein had already posted was sent to GitHub a second time — the receipt \
                 naming when it went was on disk and nothing read it",
            );
        assert!(
            said.contains("17:39:32") && said.contains("2463ac1"),
            "the refusal does not say when it went or onto which commit, so the reader cannot go \
             and look at the review skein is refusing to send again: {said}"
        );

        // **A different verdict is a different act.** Posting the review and then approving WITH it
        // is the press SKEIN-369 exists to make work: the approval changes the pull request's
        // state, which the comment did not. A guard that blocked it would break that press while
        // looking like caution.
        assert!(
            super::already_sent("acme", 22, "2463ac17d1d9", crate::prq::Verdict::Approve).is_none(),
            "approving with a review already posted as a comment was refused — that is SKEIN-369's \
             press, and it does something the first post did not"
        );

        // A draft re-read at a moved head is a DIFFERENT draft, in its own file, and must still go.
        assert!(
            super::already_sent("acme", 22, "0c8590debfc5", crate::prq::Verdict::Comment).is_none(),
            "a review drafted against a newer commit was refused because an older one had been \
             posted — the guard is about this draft, never about this pull request"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// A receipt written before skein recorded the verdict refuses BOTH. The two errors are not the
    /// same size: a refused approval costs one press, a duplicate review is on somebody's pull
    /// request under the reader's name for good.
    #[test]
    fn a_receipt_that_cannot_say_what_it_went_as_refuses_both() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        posted_draft("acme", 23, "aaaa1111", "");

        for v in [crate::prq::Verdict::Comment, crate::prq::Verdict::Approve] {
            let said = super::already_sent("acme", 23, "aaaa1111", v).unwrap_or_else(|| {
                panic!(
                    "an old receipt does not record what it went as, and this sent anyway — the \
                     safe direction when skein cannot tell is not to post"
                )
            });
            assert!(
                said.contains("Approve without it") || said.contains("read the change again"),
                "the refusal names no way forward: {said}"
            );
        }
        std::env::remove_var("SKEIN_HOME");
    }

    // ── the second turn (SKEIN-393) ────────────────────────────────────────────────────────────
    //
    // The review accounts for its own coverage before anybody sees it. Three things decide whether
    // that is worth anything, and all three are testable without a model: what the turn is told
    // (the flags), what it is asked (the prompt), and what it is allowed to do to the review it
    // came back to (the fold).

    fn drafted(path: &str, line: u64, text: &str) -> super::Draft {
        super::Draft {
            path: path.into(),
            line,
            anchored: true,
            text: text.into(),
            line_text: String::new(),
        }
    }

    fn critique_of(comments: Vec<super::Draft>) -> super::Critique {
        super::Critique {
            number: 7,
            head_sha: "abc1234".into(),
            overall: "one real problem.".into(),
            comments,
            truncated: false,
            written_at: String::new(),
            posted: None,
        }
    }

    /// A sweep can ADD. Everything else it might do to a review is a defect nobody would see: a
    /// review is not diffed against anything before it reaches a person, so a finding dropped here
    /// is indistinguishable from a finding never made.
    #[test]
    fn the_sweep_can_only_add_to_the_review_it_came_back_to() {
        let first = critique_of(vec![
            drafted("src/a.rs", 12, "this is wrong"),
            drafted("src/b.rs", 3, "and so is this"),
        ]);
        let found = critique_of(vec![
            // The same finding, in different words — the shape a second pass produces most often.
            drafted("src/a.rs", 12, "line 12 looks incorrect to me"),
            drafted("src/c.rs", 40, "the error path here cannot fire"),
        ]);
        let folded = super::fold_sweep(first.clone(), found);

        for c in &first.comments {
            assert!(
                folded
                    .comments
                    .iter()
                    .any(|f| f.path == c.path && f.line == c.line && f.text == c.text),
                "the sweep lost a finding turn 1 made — {} line {}",
                c.path,
                c.line
            );
        }
        assert!(
            folded.comments.iter().any(|c| c.path == "src/c.rs"),
            "the sweep found something new and it did not reach the review, which is the whole \
             point of making a second turn at all"
        );
        assert_eq!(
            folded.comments.len(),
            3,
            "the same file and line came back reworded and was added a second time: that is the \
             padding the sweep exists to avoid, arriving from the sweep itself"
        );
        assert_eq!(
            folded.overall, first.overall,
            "the sweep's OVERALL replaced the review's — the reader asked about the change, and \
             the sweep's sentence is about the sweep"
        );
    }

    /// What the second turn is TOLD, and what every other call is not. `Alone` adding a flag would
    /// change every model call skein makes, silently, from a change about reviews.
    #[test]
    fn a_turn_names_its_conversation_and_a_lone_call_says_nothing() {
        use crate::ai::Turn;
        let at = std::path::Path::new("/tmp/skein-turn-test");
        assert!(
            Turn::Alone.args().is_empty(),
            "a call that belongs to no conversation grew a flag, so this changed every other \
             model call skein makes"
        );
        assert_eq!(
            Turn::Opening { id: "abc", at }.args(),
            vec!["--session-id", "abc"],
            "the first turn does not name the conversation it is opening, so the second cannot \
             find it"
        );
        assert_eq!(
            Turn::Resuming { id: "abc", at }.args(),
            vec!["--resume", "abc"],
            "the second turn opens a NEW conversation instead of resuming — which fails on a \
             collision and, worse, costs the whole diff again when it does not"
        );
    }

    // ── the pull request's own conversation (SKEIN-376) ───────────────────────────────────────
    //
    // Measured against the installed CLI on 2026-08-26, and both halves matter:
    //   * `--resume` on an id it does not hold exits 1 with "No conversation found with session
    //     ID: <id>" and spends nothing — which is what makes trying the resume first affordable;
    //   * a session opened in one directory and resumed from ANOTHER gets that same answer, and
    //     resumed from the directory that opened it answers from memory. That is the failure this
    //     item exists for: unpinned, every resume misses and the feature does nothing while
    //     looking like it works.

    /// The id is derived from the pull request, so two rounds of the same one meet in the same
    /// conversation and two different ones never do.
    #[test]
    fn a_pull_request_reads_under_an_id_derived_from_it_and_not_from_the_moment() {
        let a = crate::ai::conversation_for("acme", 41);
        assert_eq!(
            a,
            crate::ai::conversation_for("acme", 41),
            "the same pull request produced two different conversation ids, so the second round \
             cannot resume what the first one left and every round is a cold read"
        );
        assert_ne!(
            a,
            crate::ai::conversation_for("acme", 42),
            "two pull requests share one conversation, so a review can answer about the wrong \
             change"
        );
        assert_ne!(
            a,
            crate::ai::conversation_for("other", 41),
            "the same number in two repos shares one conversation — the id is keyed on the number \
             alone, so it depends on where the call runs to stay correct"
        );
        assert_eq!(
            a.len(),
            36,
            "the id is not uuid-shaped and --session-id is documented as taking one: {a}"
        );
        assert!(
            a.chars().filter(|c| *c == '-').count() == 4
                && a.chars().all(|c| c == '-' || c.is_ascii_hexdigit()),
            "the id is not hex-and-dashes: {a}"
        );
    }

    /// **The pin, which is the one that ships broken without being noticed.** A conversation is
    /// filed under the directory the call ran in, so a turn that does not carry one resumes
    /// nothing — and nothing fails, it just quietly costs the whole diff every round.
    #[test]
    fn a_turn_in_a_conversation_carries_the_directory_it_is_filed_under() {
        use crate::ai::Turn;
        let at = std::path::Path::new("/tmp/skein-turn-test");
        assert_eq!(
            Turn::Opening { id: "abc", at }.at(),
            Some(at),
            "the first turn does not say where it runs, so it opens the conversation wherever the \
             server happens to have been started"
        );
        assert_eq!(
            Turn::Resuming { id: "abc", at }.at(),
            Some(at),
            "the resuming turn does not say where to look, so it looks in the server's directory \
             and is told there is no such conversation"
        );
        assert_eq!(
            Turn::Alone.at(),
            None,
            "a call in no conversation was pinned to a directory anyway, which changes where every \
             other model call skein makes runs"
        );
    }

    /// Where a pull request's conversation lives, and that it is there to be run in.
    ///
    /// **Per PULL REQUEST, not per repo** (SKEIN-395): the directory is now also the checkout the
    /// reviewer stands in, and two pull requests sharing one would have their heads fighting over
    /// it — a reading of #7 could be looking at #9's code. That is why the address moved down a
    /// level rather than the checkout being bolted onto the side of it.
    #[test]
    fn a_pull_requests_conversation_is_filed_where_it_can_be_stood_up_and_nowhere_shared() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("SKEIN_NO_GH_SECRET", "1");
        let (repo, head, _) = a_repo_with_two_commits(home);

        let (id, at) = super::conversation_of(&repo, 7, &head);
        assert_eq!(id, crate::ai::conversation_for("acme", 7));
        assert!(
            at.is_dir(),
            "the directory the conversation is filed under does not exist, so the spawn that \
             opens it fails before it starts: {}",
            at.display()
        );
        let (_, other) = super::conversation_of(&repo, 9, &head);
        assert_ne!(
            at, other,
            "two pull requests share one directory, so they share a checkout — a reading of one \
             can be standing in the other's code, and their sessions are filed together"
        );
        std::env::remove_var("SKEIN_HOME");
        std::env::remove_var("SKEIN_NO_GH_SECRET");
    }

    /// The sweep asks for NAMED things. "Anything else?" is an invitation to manufacture, and
    /// manufacturing is the precision failure — buying recall with it is not a trade, it is the
    /// same bug from the other side.
    #[test]
    fn the_sweep_asks_what_went_unread_and_says_that_finding_nothing_is_correct() {
        let p = super::SWEEP_PROMPT;
        assert!(
            p.contains("skimmed") && p.contains("List every file"),
            "the sweep does not ask which files went unread, so it cannot tell a review that \
             covered everything from one that covered three files well"
        );
        assert!(
            p.contains("Finding nothing new is the expected outcome"),
            "nothing tells the sweep that an empty answer is the right one, so a turn asked to \
             look again will find something to say"
        );
        assert!(
            p.contains("do not ask for it again"),
            "the sweep does not say the diff is already here, so the cheap turn can ask for the \
             expensive thing back"
        );
        assert!(
            p.contains("no hedged maybes") && p.contains("nothing raised twice"),
            "the review's own discipline was not carried into the sweep, so the second turn is \
             free to pad what the first was stopped from padding"
        );
    }

    /// **Both pressures, or the prompt only has one.**
    ///
    /// Every sentence in the REVIEW half used to point one way: comment only on real problems, do
    /// not manufacture findings, an empty review is valid. That is the whole of what stops a review
    /// padded with maybes — and it is also the whole of what a model satisfies by opening three of
    /// eleven changed files and saying little. Nothing in it asked for coverage.
    ///
    /// The owner named the failure that wording permits (2026-08-26): "someone else finding issues
    /// we couldn't is a bigger failure". So the counter-pressure is in, and the two have to travel
    /// together — this asserts BOTH, because either one deleted leaves a prompt that reliably fails
    /// in one direction, and neither absence is visible in an answer that looks well-formed.
    #[test]
    fn the_review_prompt_carries_both_pressures_or_it_only_has_one() {
        let pr = crate::prq::blank_pr(7, "abc1234");
        let prompt = super::merged_prompt(
            &pr,
            &super::Ownership::NoCodeowners,
            &[],
            "diff --git a/a b/a",
            false,
            "",
        );

        assert!(
            prompt.contains("someone else raises") && prompt.contains("worst outcome"),
            "the review does not know that being scooped by a person is the failure it is \
             avoiding, so nothing in it argues for opening a file it did not feel drawn to"
        );
        assert!(
            prompt.contains("COVERAGE, not volume"),
            "the recall pressure names no remedy, and the remedy a model reaches for unprompted \
             is more findings — which is the precision failure, bought with the recall fix"
        );
        assert!(
            prompt.contains("Do not manufacture findings")
                && prompt.contains("An empty review is a valid review"),
            "the precision guard is gone: with only the recall pressure left, a review that found \
             nothing has an incentive to invent something"
        );
    }

    /// The merged answer parses into both halves; a summary without a review section keeps the
    /// summary and reports no critique; prose that ignored the format is nothing at all.
    #[test]
    fn a_merged_answer_parses_into_summary_and_review() {
        let full = "KIND: feature\nLINE: adds a thing.\nEXPAND: yes\nFLAGS: behaviour\nDETAIL:\n## What it does\nIt does a thing.\nREVIEW:\nOVERALL: one real problem.\nFILE: src/a.rs\nLINE: 2\nCOMMENT: this is wrong.\n---\n";
        let (verdict, detail, critique) = parse_merged(full).expect("a well-formed merged answer");
        assert_eq!(verdict.line, "adds a thing.");
        assert!(verdict.expand);
        assert_eq!(verdict.flags, vec!["behaviour"]);
        assert!(detail.contains("## What it does"), "{detail}");
        let critique = critique.expect("the review half is here");
        assert_eq!(critique.overall, "one real problem.");
        assert_eq!(critique.comments.len(), 1);
        assert_eq!(critique.comments[0].path, "src/a.rs");

        // No review section: the summary stands, the critique is honestly absent (the caller
        // notes it as tried — the call was spent).
        let (v, d, c) =
            parse_merged("KIND: fix\nLINE: fixes a thing.\nEXPAND: no\nFLAGS: none\nDETAIL:\nnone")
                .expect("the summary half alone still parses");
        assert_eq!(v.line, "fixes a thing.");
        assert!(!v.expand);
        assert!(d.is_empty(), "the word 'none' is not a brief: {d}");
        assert!(c.is_none());

        // Prose that ignored the format is not a summary — same strictness as stage 1.
        assert!(parse_merged("This PR looks fine to me.").is_none());
    }

    /// The bulk payload carries the drafted review beside the summary — the owner's "show the
    /// critique as another section along with summary" with NO per-row fetch — and only when the
    /// draft is of the CURRENT head: a stale draft is not offered as if it read this commit. The
    /// wire shape is asserted too, because "an older client simply ignores it" is a claim about
    /// keys.
    #[test]
    fn the_bulk_payload_carries_the_current_heads_critique_beside_the_summary() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let put = |number: u64, head: &str| {
            store(
                "demo",
                &Summary {
                    number,
                    head_sha: head.into(),
                    depth: Depth::Line,
                    line: "read".into(),
                    detail: String::new(),
                    flags: Vec::new(),
                    signals: Vec::new(),
                    yours: Vec::new(),
                    others: 0,
                    ownership_unknown: String::new(),
                    unread_because: String::new(),
                    not_reread: String::new(),
                    computed: true,
                    budget_stopped: false,
                },
            )
            .unwrap();
        };
        put(1, "aaa");
        put(2, "bbb");
        store_critique(
            "demo",
            &mut Critique {
                number: 1,
                head_sha: "aaa".into(),
                overall: "one real problem.".into(),
                comments: vec![Draft {
                    path: "src/a.rs".into(),
                    line: 2,
                    anchored: true,
                    text: "on the line".into(),
                    line_text: "    let x = 1;".into(),
                }],
                truncated: false,
                written_at: String::new(),
                posted: None,
            },
        )
        .unwrap();
        // #2's draft reads an EARLIER commit: it must not ride the payload as current.
        store_critique(
            "demo",
            &mut Critique {
                number: 2,
                head_sha: "old".into(),
                overall: "stale".into(),
                comments: Vec::new(),
                truncated: false,
                written_at: String::new(),
                posted: None,
            },
        )
        .unwrap();

        let known = known("demo", &[(1, "aaa".to_string()), (2, "bbb".to_string())]);
        let one = &known[&1];
        assert!(one.has_critique);
        let riding = one.critique.as_ref().expect("the draft rides the payload");
        assert_eq!(riding.overall, "one real problem.");
        assert_eq!(riding.comments.len(), 1);
        let wire = serde_json::to_value(one).unwrap();
        assert_eq!(wire["has_critique"], true);
        assert_eq!(wire["critique"]["comments"][0]["path"], "src/a.rs");
        assert_eq!(wire["critique"]["comments"][0]["line"], 2);
        assert_eq!(wire["critique"]["comments"][0]["text"], "on the line");

        // **A draft of an EARLIER commit travels, and says which commit** (SKEIN-355). It used to
        // be filtered out here, and the owner met the consequence on #731: a review that was
        // bought, complete and postable, absent from the pane with nothing said. The rule the old
        // assertion was protecting — never offered AS a review of this one — is kept by
        // `drafted.head_sha` disagreeing with the row's head, which is what the page labels from.
        let two = &known[&2];
        assert!(
            two.has_critique && two.critique.is_some(),
            "a drafted review on disk was withheld because the branch had moved"
        );
        let wire = serde_json::to_value(two).unwrap();
        assert_eq!(
            wire["drafted"]["head_sha"], "old",
            "the payload must say WHICH commit the review read, or the page cannot label it: {wire}"
        );
        assert_eq!(
            wire["head_sha"], "bbb",
            "…and the row's own head is the other half of that comparison: {wire}"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// One pull request, read now, answers the same shape the bulk read answers (SKEIN-236).
    ///
    /// The merged model call produces the summary and the review together, so a route that answers
    /// only the summary makes the page wait for a refresh to learn about the other half — and the
    /// client that compensates becomes a second place deciding what "the draft at THIS head" means.
    #[test]
    fn a_reading_asked_for_now_carries_the_review_drafted_with_it() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let summary = |number: u64, head: &str| Summary {
            number,
            head_sha: head.into(),
            depth: Depth::Line,
            computed: true,
            budget_stopped: false,
            line: "it changes a thing.".into(),
            detail: String::new(),
            flags: Vec::new(),
            signals: Vec::new(),
            yours: Vec::new(),
            others: 0,
            ownership_unknown: String::new(),
            unread_because: String::new(),
            not_reread: String::new(),
        };
        store_critique(
            "demo",
            &mut Critique {
                number: 7,
                head_sha: "now".into(),
                overall: "one thing to look at.".into(),
                comments: Vec::new(),
                truncated: false,
                written_at: String::new(),
                posted: None,
            },
        )
        .unwrap();

        let fresh = known_at("demo", summary(7, "now"), "now");
        assert!(
            fresh.has_critique && fresh.critique.is_some(),
            "the review drafted in the same call did not ride the answer"
        );
        assert!(
            !fresh.stale,
            "a reading computed FOR this head cannot be a reading of an earlier one"
        );

        // The same rule the bulk payload keeps (SKEIN-355): a draft of an earlier commit rides,
        // labelled by the commit it read, rather than being withheld.
        let moved = known_at("demo", summary(7, "later"), "later");
        assert!(
            moved.has_critique && moved.critique.is_some(),
            "the single-PR route dropped a drafted review the bulk route keeps"
        );
        let wire = serde_json::to_value(&moved).unwrap();
        assert_eq!(
            wire["drafted"]["head_sha"], "now",
            "the review read `now` and the row is at `later` — the payload has to say so: {wire}"
        );
        // …and the summary is still the whole of what it was, flattened as the bulk shape flattens.
        assert_eq!(wire["number"], 7);
        assert_eq!(wire["head_sha"], "later");

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A row can tell "skein tried and was refused" from "nothing bought one yet"** (SKEIN-275).
    ///
    /// The reason has been on disk the whole time and could not be reached: `note_critique_tried`
    /// writes `critique-tried.json` keyed `number-sha`, and `worth_critiquing` — the loop it gates
    /// — was its only reader. No route served it, so `known` could not carry it, and a row sat
    /// draftless for the life of a head looking exactly like a row nobody had asked about.
    ///
    /// Three states, and the test insists on all three, because the two silent ones are what make
    /// the loud one mean anything:
    ///
    ///   * attempted at THIS head and refused → the note's own words ride the row;
    ///   * attempted at an EARLIER head → nothing, because the note is keyed to the commit and a
    ///     refusal about a commit that has been replaced says nothing about this one;
    ///   * a review IS drafted → nothing, because the draft is the answer to the same question.
    #[test]
    fn a_row_says_why_no_review_was_drafted_when_one_was_tried_and_refused() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let reading = |number: u64, head: &str| Summary {
            number,
            head_sha: head.into(),
            depth: Depth::Line,
            line: "it changes a thing".into(),
            detail: String::new(),
            flags: Vec::new(),
            signals: Vec::new(),
            yours: Vec::new(),
            others: 0,
            ownership_unknown: String::new(),
            unread_because: String::new(),
            not_reread: String::new(),
            computed: true,
            budget_stopped: false,
        };
        store("why", &reading(5, "aaa")).unwrap();
        store("why", &reading(6, "bbb")).unwrap();
        note_critique_tried("why", 5, "aaa", "the model would not answer");
        // Keyed to a commit that has been replaced: it must not speak about `bbb`.
        note_critique_tried("why", 6, "OLD", "a refusal about a different commit");

        let rows = known("why", &[(5, "aaa".to_string()), (6, "bbb".to_string())]);
        assert_eq!(
            rows[&5].critique_because, "the model would not answer",
            "the reason was on disk and the row could not say it — which reads as a review nobody \
             ever asked for"
        );
        assert_eq!(
            rows[&6].critique_because, "",
            "a refusal recorded against a commit that has been replaced was reported as if it \
             were about this one"
        );

        // It reaches the page under that name, and only when there is something to say.
        let wire = serde_json::to_value(&rows[&5]).unwrap();
        assert_eq!(wire["critique_because"], "the model would not answer");
        assert!(
            serde_json::to_value(&rows[&6])
                .unwrap()
                .get("critique_because")
                .is_none(),
            "an empty reason must stay off the wire rather than render as a blank chip"
        );

        // And a row that HAS a review says nothing: the draft is the answer to the same question.
        store_critique(
            "why",
            &mut Critique {
                number: 5,
                head_sha: "aaa".into(),
                overall: "one real problem".into(),
                comments: Vec::new(),
                truncated: false,
                written_at: String::new(),
                posted: None,
            },
        )
        .unwrap();
        let rows = known("why", &[(5, "aaa".to_string())]);
        assert!(
            rows[&5].has_critique && rows[&5].critique_because.is_empty(),
            "a row with a drafted review still explains why it has none"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// A reading with everything in it — brief, signals, ownership, a drafted review.
    fn fat(number: u64, head: &str) -> Known {
        Known::new(
            Summary {
                number,
                head_sha: head.into(),
                depth: Depth::Expanded,
                line: "the request timeout default drops from 30s to 5s.".into(),
                detail: "## What it does\n\nShortens how long a request waits.\n".into(),
                flags: vec!["default".into(), "behaviour".into()],
                yours: vec!["src/parser.rs".into()],
                others: 3,
                ownership_unknown: String::new(),
                signals: vec![crate::contracts::Signal {
                    kind: "default".into(),
                    what: "TIMEOUT moved from 30 to 5".into(),
                    file: "src/parser.rs".into(),
                    symbol: "TIMEOUT".into(),
                }],
                unread_because: String::new(),
                not_reread: String::new(),
                computed: false,
                budget_stopped: false,
            },
            false,
            Some(Critique {
                number,
                head_sha: head.into(),
                overall: "one real problem.".into(),
                // TWO comments, not one: the chip draws a COUNT, and a fixture with one comment
                // cannot tell a count apart from a yes/no.
                comments: vec![
                    Draft {
                        path: "src/a.rs".into(),
                        line: 2,
                        anchored: true,
                        text: "on the line".into(),
                        line_text: "    let x = 1;".into(),
                    },
                    Draft {
                        path: "src/b.rs".into(),
                        line: 40,
                        anchored: false,
                        text: "and this one is not anchored".into(),
                        line_text: String::new(),
                    },
                ],
                truncated: false,
                written_at: String::new(),
                posted: None,
            }),
            // A critique IS present, so `critique_because` is empty whatever is in here — which is
            // what keeps the exact key list in `the_row_shape_carries_only_what_a_row_draws`
            // unchanged.
            &std::collections::BTreeMap::new(),
            head,
        )
    }

    /// The queue payload carries what a ROW draws, and stops there (SKEIN-287).
    ///
    /// The baseline this exists to end: 153,381 bytes for thirty-nine readings, every one carrying
    /// a brief of several thousand characters, its signals and its whole drafted review — none of
    /// which a collapsed row draws. The prose is read one row at a time, when a row is opened.
    ///
    /// The key set is asserted EXACTLY, and that strictness is the point of the test rather than
    /// an accident of writing it: `thin` empties fields by name, so a new prose field on
    /// [`Summary`] would ride the row payload silently. This fails on the day one is added, which
    /// is where somebody decides whether it belongs on the line or behind the fold.
    #[test]
    fn the_row_shape_carries_only_what_a_row_draws() {
        let wire = serde_json::to_value(fat(1, "aaa").thin()).unwrap();
        let mut keys: Vec<&str> = wire
            .as_object()
            .expect("a reading is an object")
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "computed",
                "depth",
                "detail",
                "drafted",
                "flags",
                "has_critique",
                "head_sha",
                "line",
                "number",
                "others",
                "signals",
                "stale",
                "unread_because",
                "yours",
            ],
            "the row payload gained or lost a field: {wire}"
        );

        // The prose, gone — every one of these is drawn only behind the fold, by `revDetail` and
        // `revDraftSection` (`src/web/index.html:4611-4660`).
        assert_eq!(wire["detail"], "", "the brief is riding every queue row");
        assert_eq!(
            wire["signals"],
            serde_json::json!([]),
            "what the diff proved is riding every queue row"
        );
        assert_eq!(
            wire["yours"],
            serde_json::json!([]),
            "the owned-paths list is riding every queue row"
        );
        assert_eq!(
            wire["others"], 0,
            "the not-yours count is riding every queue row"
        );
        assert!(
            wire.get("critique").is_none(),
            "the whole drafted review is riding every queue row: {wire}"
        );

        // And what the line itself is made of, kept: the gist, the tripwire chips, the depth and
        // its reason, the head the stale rule compares — and the two facts the "review ready" chip
        // is drawn from.
        assert_eq!(
            wire["line"],
            "the request timeout default drops from 30s to 5s."
        );
        assert_eq!(wire["flags"], serde_json::json!(["default", "behaviour"]));
        assert_eq!(wire["depth"], "expanded");
        assert_eq!(wire["head_sha"], "aaa");
        assert_eq!(wire["has_critique"], true);
        assert_eq!(wire["drafted"]["head_sha"], "aaa");
        assert_eq!(wire["drafted"]["comments"], 2);
    }

    /// A row and the reading behind it are ONE serialisation, so they cannot come apart.
    ///
    /// SKEIN-243 is why this is asserted rather than argued: the drafted review and the summary
    /// were assembled in two places and stopped agreeing about which commit had been read. `thin`
    /// is the same struct with named fields emptied, and this is the check that says so — every
    /// key a row carries holds the value the full reading holds, or it is one of the named prose
    /// fields that was emptied.
    #[test]
    fn a_row_never_disagrees_with_the_reading_behind_it() {
        let full = serde_json::to_value(fat(1, "aaa")).unwrap();
        let row = serde_json::to_value(fat(1, "aaa").thin()).unwrap();
        let emptied = ["detail", "signals", "yours", "others"];
        for (key, value) in row.as_object().unwrap() {
            if emptied.contains(&key.as_str()) {
                continue;
            }
            assert_eq!(
                Some(value),
                full.get(key),
                "the row's `{key}` is not what the full reading says it is"
            );
        }
        // The drafted review is dropped from the row, and the two facts left in its place are true
        // of the review that was dropped — the chip's whole claim.
        assert_eq!(
            row["drafted"]["head_sha"], full["critique"]["head_sha"],
            "the row names a commit the drafted review did not read"
        );
        assert_eq!(
            row["drafted"]["comments"],
            full["critique"]["comments"].as_array().unwrap().len(),
            "the chip's count is not the number of comments the review holds"
        );
    }

    /// Opening a row hands over the prose skein already has — including for a reading of an
    /// EARLIER commit, which is the case the computing route cannot answer.
    ///
    /// `GET /review/:n/summary` goes through [`visit`], whose cache lookup is keyed on the CURRENT
    /// head: on a pull request that has moved since it was read, that misses and falls through to
    /// a model call. The queue deliberately keeps and marks that reading ([`known`]), so a row
    /// showing it must be able to open it without buying a new one.
    #[test]
    fn a_row_opens_onto_the_prose_skein_already_holds() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        let mut old = fat(4, "before").summary;
        old.detail = "the brief written against the commit before the push.".into();
        store("demo", &old).unwrap();
        store_critique(
            "demo",
            &mut Critique {
                number: 4,
                head_sha: "before".into(),
                overall: "read at the older commit.".into(),
                comments: Vec::new(),
                truncated: false,
                written_at: String::new(),
                posted: None,
            },
        )
        .unwrap();

        // The branch has moved: `after` is the head now, and nothing has been read at it.
        let opened = held("demo", 4, "after");
        assert!(
            opened.stale,
            "a reading of an earlier commit was handed over as current"
        );
        assert_eq!(
            opened.summary.detail, "the brief written against the commit before the push.",
            "expanding a row that has a reading found no prose in it"
        );
        assert!(
            !opened.summary.signals.is_empty(),
            "the signals behind the fold were not handed over"
        );
        // The review of that same earlier commit opens WITH it (SKEIN-355) — one visit produced
        // both, and hiding half of it is what left #731 saying "review below" with nothing under
        // the heading. Which commit it read is on the payload, so the page labels rather than
        // pretends.
        assert!(
            opened.critique.is_some() && opened.has_critique,
            "expanding a row with a stale reading found the reading and not the review beside it"
        );
        assert_eq!(
            opened.drafted.as_ref().map(|d| d.head_sha.as_str()),
            Some("before"),
            "a review of the previous commit must arrive named as one, never as a review of this"
        );

        // And a pull request skein has never read is an honest unread answer, not an error: a row
        // that opens onto a transport failure is how a reading that exists comes to look like a
        // pull request nobody read.
        let never = held("demo", 9, "zzz");
        assert_eq!(never.summary.depth, Depth::Unread);
        assert!(!never.summary.unread_because.is_empty());
        assert_eq!(never.summary.head_sha, "zzz");

        std::env::remove_var("SKEIN_HOME");
    }

    // ── a reading that ran out of time (SKEIN-392) ─────────────────────────────────────────────
    //
    // Reported from the live fleet: "Not summarised — `claude` was still going after 300s. A larger
    // diff needs longer than this call allows; nothing is wrong with the model. Read this one
    // yourself." Two decisions were behind that sentence and neither existed — how long the call
    // gets, and what is left to try when it does not come back. Both are pure functions now, which
    // is why they can be asserted here rather than by waiting five minutes for a clock.

    /// The floor is what every call used to get, so nothing lost time; the ceiling is what the
    /// biggest diff that can arrive earns, so nobody waits for an answer to a question that cannot
    /// be asked. Both are DERIVED — the ceiling from [`super::CRITIQUE_BYTES`] — so this test also
    /// fails if that truncation moves and the clock does not follow it.
    #[test]
    fn the_reading_budget_grows_with_the_diff_and_stops_where_the_diff_stops() {
        let secs = |n: usize| super::merged_budget(n).as_secs();
        assert_eq!(
            secs(0),
            300,
            "an empty diff got less than the flat budget every call used to have"
        );
        assert_eq!(
            secs(50_000),
            300,
            "a small diff was given less than the old flat budget"
        );
        assert!(
            secs(150_000) > secs(50_000),
            "a diff three times the size got no more time than the small one, which is the bug"
        );
        assert_eq!(
            secs(super::CRITIQUE_BYTES),
            super::merged_budget(super::CRITIQUE_BYTES * 4).as_secs(),
            "the ceiling is not the largest diff that can reach this call: a truncated diff is \
             capped at CRITIQUE_BYTES, so more time than that buys nothing"
        );
        assert!(
            secs(super::CRITIQUE_BYTES) >= 900,
            "the largest diff skein will read got under fifteen minutes to read it"
        );
    }

    /// The one refusal with a smaller second attempt in it. Every other one is about the SETUP — a
    /// binary that is not there, a sandbox that is not answering — and asking again with less diff
    /// fails identically, one more minute later.
    #[test]
    fn a_slow_read_is_narrowed_and_every_other_refusal_stops() {
        use crate::ai::Unread;
        assert_eq!(
            super::after_merged(&Unread::Slow(std::time::Duration::from_secs(300))),
            super::AfterMerged::Narrow,
            "a call that ran out of time led nowhere, which is the row that says `read this one \
             yourself` with no way to"
        );
        for refusal in [
            Unread::Missing {
                bin: "claude".into(),
                why: "not found".into(),
            },
            Unread::Unreachable {
                sandbox: "fleet".into(),
                why: "no route".into(),
            },
            Unread::AbsentInSandbox {
                bin: "claude".into(),
                sandbox: "fleet".into(),
            },
            Unread::Refused {
                code: "1".into(),
                said: "not logged in".into(),
            },
            Unread::Silent,
        ] {
            assert_eq!(
                super::after_merged(&refusal),
                super::AfterMerged::Stop,
                "a second, smaller call was spent on a refusal a smaller diff cannot fix: {refusal:?}"
            );
        }
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
            a.keys().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 11],
            "right-side lines only, following the hunk headers"
        );
        // And each carries its content, marker stripped — the anchor a draft stores so its
        // comments can be found again on a moved head.
        assert_eq!(a[&2], "    let x = 1;", "an added line's own text");
        assert_eq!(a[&3], "    println!(\"hi\");", "a context line's own text");
        assert_eq!(a[&11], "kept");
        assert!(
            !map.contains_key("dead.rs"),
            "a deleted file has no right side to comment on"
        );
    }

    /// **A `\\ No newline at end of file` marker does not end the hunk it sits in** (SKEIN-233).
    ///
    /// git emits that marker in the middle of a hunk whenever the old file lacked a trailing
    /// newline and the new one has one — routine in JSON, `.env`, generated files and fixtures.
    /// The vetting parser had no case for it and fell through to an `else` that cleared `in_hunk`,
    /// so it saw ONE line of this diff where the re-anchorer saw four. Every drafted comment below
    /// the marker was then vetted unanchorable and `assemble_post` folded it into the review body
    /// as prose: the review still posted, and had quietly stopped being a line review.
    ///
    /// Written against both views on purpose. `right_side_lines` is now the only parser and
    /// `commentable` is its projection, so this asserts the sequence AND the map — the two shapes
    /// that used to be produced by two different readings of the same grammar.
    #[test]
    fn a_no_newline_marker_does_not_swallow_the_rest_of_its_hunk() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,4 @@
 fn main() {}
-let old = 1;
\\ No newline at end of file
+let new = 1;
+let after = 2;
+let last = 3;
";
        assert_eq!(
            right_side_lines(diff),
            vec![
                ("src/a.rs".to_string(), 1, "fn main() {}".to_string()),
                ("src/a.rs".to_string(), 2, "let new = 1;".to_string()),
                ("src/a.rs".to_string(), 3, "let after = 2;".to_string()),
                ("src/a.rs".to_string(), 4, "let last = 3;".to_string()),
            ],
            "the marker is a note about the previous line, not the end of the hunk"
        );
        let map = commentable(diff);
        let a = map.get("src/a.rs").expect("the file is commentable at all");
        assert_eq!(
            a.keys().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 4],
            "vetting saw fewer lines than re-anchoring, so every draft below the marker posts as \
             prose instead of on its line"
        );
        assert_eq!(a[&4], "let last = 3;", "the anchor text a draft stores");
    }

    /// **`+++ path` with no `b/` names the same file `+++ b/path` does** (SKEIN-233).
    ///
    /// The vetting parser required the `b/` exactly and treated every other `+++ ` as a deleted
    /// file, so a diff written without git's prefix — `git diff --no-prefix`, and every unified
    /// diff not produced by git — was commentable nowhere at all. Silent: a review with no
    /// anchored comments looks exactly like a review the model chose not to put on lines.
    #[test]
    fn a_diff_header_without_the_b_prefix_still_names_a_file_to_comment_on() {
        let diff = "\
diff --git src/a.rs src/a.rs
--- src/a.rs
+++ src/a.rs
@@ -1,1 +1,2 @@
 fn main() {}
+let added = 1;
";
        let map = commentable(diff);
        let a = map
            .get("src/a.rs")
            .expect("a diff written without git's b/ prefix was commentable nowhere");
        assert_eq!(a.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(a[&2], "let added = 1;");
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
                line_text: "    let x = 1;".into(),
            },
            Draft {
                path: "src/b.rs".into(),
                line: 0,
                anchored: false,
                text: "about the change".into(),
                line_text: String::new(),
            },
        ];
        let (body, anchored) = assemble_post("overall note", &kept);
        assert_eq!(anchored.len(), 1, "only the anchored comment rides as one");
        assert_eq!(anchored[0].path, "src/a.rs");
        assert_eq!(anchored[0].line, 2);
        assert_eq!(
            anchored[0].text, "    let x = 1;",
            "the line's own text travels to the wire comment — it is the re-anchor's only handle \
             on a moved head"
        );
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

        // A model that reads it: the MERGED answer, because there is no standalone drafter left to
        // answer the bare review format (SKEIN-263) — a summary and, under `REVIEW:`, one comment
        // the diff shows and one it does not.
        let claude = home.join("claude.sh");
        std::fs::write(
            &claude,
            "#!/bin/sh\nfor a in \"$@\"; do p=\"$a\"; done\n# The second turn asks a different question and must get a different answer: handed\n# the merged text back, parse_critique reads its summary LINE: as a comment anchor\n# and the review grows a finding nobody wrote (SKEIN-393).\ncase \"$p\" in\n  *\"account for what it actually covered\"*) printf 'OVERALL: nothing new\\n'; exit 0;;\nesac\nprintf 'KIND: fix\\nLINE: it adds a binding.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\nREVIEW:\\nOVERALL: one real problem.\\nFILE: src/a.rs\\nLINE: 2\\nCOMMENT: x is unused.\\n---\\nFILE: src/a.rs\\nLINE: 99\\nCOMMENT: nowhere.\\n---\\n'\n",
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
            reasons: Vec::new(),
            lane: crate::prq::Lane::NeedsYou,
            ..crate::prq::blank_pr(11, "sha11")
        };

        let drafted = critique(&repo, "acme/thing", &pr, &["me".into()])
            .expect("the draft path works end to end");
        assert_eq!(drafted.comments.len(), 2);
        assert!(
            drafted.comments[0].anchored,
            "line 2 is in the diff, so the comment anchors"
        );
        assert_eq!(
            drafted.comments[0].line_text, "    let x = 1;",
            "the drafted line's own content is stored with the comment — the anchor that lets \
             this draft post after the branch moves"
        );
        assert!(
            !drafted.comments[1].anchored,
            "line 99 is not in the diff — offered for the body, not guessed onto a line"
        );
        assert_eq!(
            drafted.comments[1].line_text, "",
            "no line the diff vouches for, no anchor text — an unanchored comment must not carry \
             a text it could falsely re-anchor by"
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

    /// The write path, whole, both heads. A matching head posts exactly what was handed in — the
    /// kept comments on their lines, the unanchored one in the body, the commit id the live head.
    /// A MOVED head no longer refuses (the old refusal made any actively-pushed PR a "draft it
    /// again" treadmill — SKEIN-215): the wire shows the comment re-anchored by its line's text to
    /// its new number, the drafted sha named in the body, and a comment with no line text — a
    /// draft persisted before `line_text` existed — displaced into the body rather than guessed.
    ///
    /// It is also where `prq::head_to_post_against`'s FALLBACK is proven, which is worth knowing
    /// before changing the stub: this GitHub answers `/pulls/11` with a diff whatever is asked of
    /// it, so the live head read fails here, and "pinned to the live head" below is what catches a
    /// failed read being allowed to post an empty `commit_id` instead of the remembered sha. The
    /// live read succeeding is the sibling test, which stubs the two media types apart.
    ///
    /// The remembered queue seeded below is where that fallback now comes from. Since SKEIN-272 a
    /// post reads no queue, so the sha it falls back to is what this machine already holds rather
    /// than one a refresh fetched on the way past — and `queue_within` deliberately remembers
    /// nothing under `cfg!(test)`, so a test that wants the state a real post runs in has to say
    /// so. It is not scaffolding: a draft exists only because the pane rendered this queue.
    #[test]
    fn posting_a_moved_head_re_anchors_by_line_text_instead_of_refusing() {
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
                use std::io::Write as _;
                let (head, body) = read_request(&stream);
                let answer = if head.contains("/user/teams") {
                    "[]".to_string()
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("POST") && head.contains("/reviews") {
                    // The thing under test: record exactly what skein said.
                    std::fs::write(&posted_at, &body).unwrap();
                    "{}".to_string()
                } else if head.starts_with("GET") && head.contains("/pulls/11 ") {
                    // The LIVE head's diff, fetched only when the head moved: an insertion above
                    // has pushed the drafted line from 2 to 3.
                    "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,5 @@\n fn main() {\n+    // a new line above\n+    let x = 1;\n     println!(\"hi\");\n }\n"
                        .to_string()
                } else if body.contains("review-requested") {
                    // The batched wire (SKEIN-209): the queue's PR at its LIVE head, sha11.
                    r#"{"data":{"q0":{"nodes":[{"number":11,"title":"t","url":"u",
                       "isDraft":false,"author":{"login":"someone"},"headRefName":"feat",
                       "headRefOid":"sha11","baseRefName":"main",
                       "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                       "latestReviews":{"nodes":[]},
                       "commits":{"nodes":[{"commit":{"committedDate":"2020-01-01T00:00:00Z"}}]}}]},
                       "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                        .to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string()
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
        // What the pane left behind when it rendered this repo — the head skein last SAW, and the
        // only second opinion a post has when GitHub will not say what the live head is.
        crate::prq::remember_for_test(&crate::prq::Queue {
            repo_id: "crit".into(),
            slug: "acme/thing".into(),
            trunk: "main".into(),
            viewer: "me".into(),
            ai: false,
            prs: vec![serde_json::from_value(serde_json::json!({
                "number": 11, "title": "t", "author": "someone", "url": "u",
                "head_ref": "feat", "head_sha": "sha11", "base_ref": "main",
                "draft": false, "updated_at": "", "committed_at": "",
                "checks": "passing", "my_review": "none", "review_is_current": false,
                "reasons": [], "lane": "needs-you", "box_name": "b",
            }))
            .unwrap()],
            blind_spots: Vec::new(),
            as_of: String::new(),
            fresh: false,
            // Complete, so `prune` may read an absence as evidence (SKEIN-231, `Queue::whole`).
            whole: true,
        });
        let kept = vec![
            Draft {
                path: "src/a.rs".into(),
                line: 2,
                anchored: true,
                text: "on the line".into(),
                // What `draft_critique` stored from the diff it vetted against: the drafted
                // line's own content, the durable anchor.
                line_text: "    let x = 1;".into(),
            },
            Draft {
                path: "src/b.rs".into(),
                line: 0,
                anchored: false,
                text: "about the change".into(),
                line_text: String::new(),
            },
        ];

        // A matching head posts untouched — nothing re-anchors, nothing is annotated.
        post_critique(
            &repo,
            11,
            "sha11",
            "note",
            &kept,
            crate::prq::Verdict::Comment,
        )
        .expect("a matching head posts");
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&posted).unwrap()).unwrap();
        assert_eq!(sent["commit_id"], "sha11", "pinned to the live head");
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
        assert!(
            !said_body.contains("read at"),
            "a head that did not move gets no moved-head annotation: {said_body}"
        );

        // The head the queue reports is sha11; a draft of an earlier commit POSTS ANYWAY —
        // SKEIN-215 — re-anchored to the live diff by its line's text.
        std::fs::remove_file(&posted).unwrap();
        post_critique(
            &repo,
            11,
            "aaaaaaa2222",
            "note",
            &kept,
            crate::prq::Verdict::Comment,
        )
        .expect("a moved head posts instead of refusing");
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&posted).unwrap()).unwrap();
        assert_eq!(
            sent["commit_id"], "sha11",
            "the commit id is the LIVE head, never the drafted one"
        );
        let comments = sent["comments"].as_array().expect("the comment survived");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["path"], "src/a.rs");
        assert_eq!(
            comments[0]["line"], 3,
            "the insertion above pushed the drafted line from 2 to 3, and the comment followed \
             its text there"
        );
        let said_body = sent["body"].as_str().unwrap();
        assert!(
            said_body.contains("(read at aaaaaaa, posted against sha11)"),
            "the record names the drafted head and the posted one: {said_body}"
        );

        // A draft persisted before `line_text` existed parses with it empty…
        let old: Draft = serde_json::from_str(
            r#"{"path":"src/a.rs","line":2,"anchored":true,"text":"on the line"}"#,
        )
        .unwrap();
        assert_eq!(old.line_text, "", "an absent field is empty, not an error");
        // …and on a moved head everything displaces into the body — harmless and honest: with no
        // text to search for, a guessable anchor does not exist.
        std::fs::remove_file(&posted).unwrap();
        post_critique(
            &repo,
            11,
            "aaaaaaa2222",
            "note",
            &[old],
            crate::prq::Verdict::Comment,
        )
        .expect("an old draft still posts on a moved head");
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&posted).unwrap()).unwrap();
        assert!(
            sent.get("comments").is_none(),
            "nothing anchors without line text: {sent}"
        );
        let said_body = sent["body"].as_str().unwrap();
        assert!(
            said_body.contains("Reviewed at aaaaaaa — the branch has moved since")
                && said_body.contains("src/a.rs:2 — on the line"),
            "the displaced comment folds into the body naming the drafted sha: {said_body}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "GH_TOKEN"] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
    }

    /// The queue's sha is not the live head, and a review must not be posted against it
    /// (SKEIN-230).
    ///
    /// This is the window the bug lived in, modelled directly: GitHub's search answers the queue
    /// with `stale111`, and `GET /pulls/11` — the live read — answers `live222`. The draft was read
    /// from that same queue, so it carries `stale111` too. Trusting the queue makes the two agree,
    /// `moved` reads false, nothing re-anchors, and the vetted comment posts at the line number it
    /// had in a diff that no longer exists — with GitHub resolving it against the CURRENT diff and
    /// the pane reporting success. Every assertion below fails in that world.
    ///
    /// A stub of its own because this one has to answer the SAME path two ways, on the `Accept`
    /// header: the diff media type for `pr_diff_text`, JSON for `live_head_sha`.
    #[test]
    fn a_review_posts_against_the_live_head_not_the_one_the_queue_remembers() {
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
                use std::io::{BufRead as _, Read as _, Write as _};
                // Headers and all, unlike `read_request` — the Accept header IS the dispatch here.
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut length = 0usize;
                let mut line = String::new();
                reader.read_line(&mut head).ok();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse().unwrap_or(0);
                    }
                    head.push_str(&line);
                    line.clear();
                }
                let mut raw = vec![0u8; length];
                if length > 0 {
                    reader.read_exact(&mut raw).ok();
                }
                let body = String::from_utf8_lossy(&raw).into_owned();

                let answer = if head.contains("/user/teams") {
                    "[]".to_string()
                } else if head.contains("/user") {
                    r#"{"login":"me"}"#.to_string()
                } else if head.starts_with("POST") && head.contains("/reviews") {
                    std::fs::write(&posted_at, &body).unwrap();
                    "{}".to_string()
                } else if head.contains("/pulls/11") && head.contains("application/vnd.github.diff")
                {
                    // The LIVE head's diff: an insertion above has pushed the drafted line
                    // from 2 to 3.
                    "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,5 @@\n fn main() {\n+    // a new line above\n+    let x = 1;\n     println!(\"hi\");\n }\n"
                        .to_string()
                } else if head.contains("/pulls/11") {
                    // The live read. This is what the queue's answer below is a minute behind.
                    r#"{"head":{"sha":"live222"}}"#.to_string()
                } else if body.contains("review-requested") {
                    // The queue, still holding the head from before the push.
                    r#"{"data":{"q0":{"nodes":[{"number":11,"title":"t","url":"u",
                       "isDraft":false,"author":{"login":"someone"},"headRefName":"feat",
                       "headRefOid":"stale111","baseRefName":"main",
                       "updatedAt":"2020-01-01T00:00:00Z","reviewDecision":"REVIEW_REQUIRED",
                       "latestReviews":{"nodes":[]},
                       "commits":{"nodes":[{"commit":{"committedDate":"2020-01-01T00:00:00Z"}}]}}]},
                       "q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#
                        .to_string()
                } else if head.contains("/graphql") {
                    r#"{"data":{"q0":{"nodes":[]},"q1":{"nodes":[]},"q2":{"nodes":[]},"q3":{"nodes":[]}}}"#.to_string()
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
            "id": "stale", "source": "https://github.com/acme/thing.git",
            "source_tree": "", "store": "",
        }))
        .unwrap();
        let kept = vec![Draft {
            path: "src/a.rs".into(),
            line: 2,
            anchored: true,
            text: "on the line".into(),
            line_text: "    let x = 1;".into(),
        }];

        // `stale111` is what the pane had when the draft was read — the same sha the queue is
        // still serving, which is exactly why comparing the two proves nothing.
        post_critique(
            &repo,
            11,
            "stale111",
            "note",
            &kept,
            crate::prq::Verdict::Comment,
        )
        .expect("the review posts");
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&posted).unwrap()).unwrap();
        assert_eq!(
            sent["commit_id"], "live222",
            "commit_id must name the head GitHub holds now, not the one the queue remembers"
        );
        let comments = sent["comments"]
            .as_array()
            .expect("the comment rides a line");
        assert_eq!(
            comments[0]["line"], 3,
            "the branch moved, so the comment re-anchors by its line text — posting it at 2 \
             would put vetted words on whatever now occupies line 2"
        );
        let said_body = sent["body"].as_str().unwrap();
        assert!(
            said_body.contains("(read at stale11, posted against live222)"),
            "a moved head must say so on the record: {said_body}"
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

    /// **A read that fails must not fail a write** (SKEIN-272). Reported live: the owner pressed
    /// "post comments" and got `queue_within`'s sentence — five membership searches missing, about
    /// a repository they had not asked after — and nothing was posted. They posted it by hand.
    ///
    /// This GitHub refuses everything a queue refresh asks for: the viewer lookup, the membership
    /// searches, the repo lookup. Only the write and the live-head read answer. Before SKEIN-272
    /// the `?` on `prq::queue(repo, false)` meant nothing reached the wire at all.
    #[test]
    fn a_post_lands_even_when_the_queue_cannot_be_read() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        let home = home.as_ref() as &std::path::Path;
        std::env::set_var("SKEIN_HOME", home);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        crate::prq::forget_renames();

        let posted = home.join("posted.json");
        let posted_at = posted.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let (head, body) = read_request(&stream);
                let (code, answer) = if head.starts_with("POST") && head.contains("/reviews") {
                    std::fs::write(&posted_at, &body).unwrap();
                    (200, "{}".to_string())
                } else if head.starts_with("GET") && head.contains("/pulls/11 ") {
                    (200, r#"{"head":{"sha":"sha11"}}"#.to_string())
                } else {
                    // Everything a refresh would ask for: dead, the way the edge was that day.
                    (
                        502,
                        "<html><head><title>502 Bad Gateway</title></head></html>".to_string(),
                    )
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
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
        let kept = vec![Draft {
            path: "src/a.rs".into(),
            line: 2,
            anchored: true,
            text: "on the line".into(),
            line_text: "    let x = 1;".into(),
        }];

        let said = post_critique(
            &repo,
            11,
            "sha11",
            "note",
            &kept,
            crate::prq::Verdict::Comment,
        )
        .expect("a queue that will not load must not swallow a vetted review");
        assert!(said.contains("posted the review"), "{said}");
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&posted).unwrap()).unwrap();
        assert_eq!(sent["commit_id"], "sha11", "still pinned to the live head");
        assert_eq!(sent["body"], "note");
        assert_eq!(
            sent["comments"].as_array().map(Vec::len),
            Some(1),
            "the vetted comment must ride along, not be dropped with the queue: {sent}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "GH_TOKEN"] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_renames();
    }

    /// **A review skein has posted knows it, and the row can say so** (SKEIN-364).
    ///
    /// The owner, on #691: "it shows the review while the review was already submitted and shows up
    /// in comments basically this is prone to giving the same comments again and again. Isn't it
    /// easy to detect this and avoid?" It was not detectable at all: the draft stayed on disk
    /// exactly as it was, `worth_critiquing` refuses to draft a second one at a head it has already
    /// drafted, and nothing anywhere recorded that the post had happened.
    ///
    /// It cannot be asked of GitHub either, which is why the receipt is skein's own: `my_review`
    /// comes from `latestOpinionatedReviews` and excludes the COMMENTED verdict this posts under,
    /// and `Pr::review_threads` carries no comment bodies (`src/prq.rs:1468-1471`). What is
    /// provable is what skein itself did.
    ///
    /// The counter-case is in the same test on purpose: a SECOND draft, never posted, must come
    /// back with no receipt from the same payload — otherwise "posted" is a property of the code
    /// path rather than of the review, and the row would stop offering reviews nobody has sent.
    #[test]
    fn a_posted_review_carries_a_receipt_and_an_unposted_one_does_not() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        crate::prq::forget_renames();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let (head, _body) = read_request(&stream);
                // The live head, so `head_to_post_against` gets a real answer and `onto` is
                // GitHub's sha rather than the fallback's.
                let answer = match head.starts_with("POST") && head.contains("/reviews") {
                    true => "{}",
                    false => r#"{"head":{"sha":"live999"}}"#,
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

        // Two drafts, identical but for their number: one gets posted, the other never does.
        for number in [11u64, 12] {
            store_critique(
                "crit",
                &mut Critique {
                    number,
                    head_sha: format!("sha{number}"),
                    overall: "one real problem.".into(),
                    comments: Vec::new(),
                    truncated: false,
                    written_at: String::new(),
                    posted: None,
                },
            )
            .unwrap();
            store(
                "crit",
                &Summary {
                    number,
                    head_sha: format!("sha{number}"),
                    line: "a line".into(),
                    detail: String::new(),
                    flags: Vec::new(),
                    signals: Vec::new(),
                    yours: Vec::new(),
                    others: 0,
                    ownership_unknown: String::new(),
                    depth: Depth::Line,
                    unread_because: String::new(),
                    not_reread: String::new(),
                    computed: false,
                    budget_stopped: false,
                },
            )
            .unwrap();
        }

        post_critique(
            &repo,
            11,
            "sha11",
            "one real problem.",
            &[],
            crate::prq::Verdict::Comment,
        )
        .expect("the review posts");

        let receipt = critiqued("crit", 11)
            .expect("the draft is still on disk after posting")
            .posted
            .expect("skein posted this review and recorded nothing");
        assert!(
            !receipt.at.is_empty(),
            "a receipt with no time cannot be shown to the reader"
        );
        assert_eq!(
            receipt.onto, "live999",
            "the receipt must name the commit the review LANDED on, not the one it read"
        );

        // And the row's own vocabulary carries it, because the queue payload takes the review's
        // prose out (`Known::thin`) and a collapsed row still has to say "already posted".
        let rows = known("crit", &[(11, "sha11".into()), (12, "sha12".into())]);
        assert!(
            !rows[&11]
                .drafted
                .as_ref()
                .expect("the posted draft rides the row")
                .posted_at
                .is_empty(),
            "the row cannot tell that this review is already on GitHub"
        );
        assert!(
            rows[&12]
                .drafted
                .as_ref()
                .expect("the unposted draft rides the row too")
                .posted_at
                .is_empty(),
            "a review nobody has posted was marked as posted, so the reader can no longer send it"
        );
        let wire = serde_json::to_value(&rows[&12]).unwrap();
        assert!(
            wire["drafted"].get("posted_at").is_none(),
            "not posted must be an ABSENT key, never an empty string a client could print: {wire}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "GH_TOKEN"] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_renames();
    }

    /// **Approving with skein's review is the same write, and leaves the same receipt** (SKEIN-369).
    ///
    /// The draft used to reach GitHub by two presses down two paths. "Post N comments as one
    /// review" went through [`post_critique`], which records the post; "approve with this review"
    /// went to `/review/:n/act` and straight into `prq::submit_review_with_comments`, which records
    /// nothing. So the identical review was submitted, the author read it, and the row went on
    /// saying "review ready · N" with "go through N comments and post…" — pressing which said every
    /// comment a second time. That is the owner's #691 report, which SKEIN-364 fixed for one of the
    /// two buttons.
    ///
    /// Both halves are asserted, because either alone passes with the bug: that GitHub was asked
    /// for an APPROVAL (otherwise the two presses do the same thing and one of them is a lie), and
    /// that the receipt was written (otherwise the review is offered again).
    #[test]
    fn approving_with_skeins_review_posts_it_as_an_approval_and_records_that_it_went() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        crate::prq::forget_renames();

        let posted = home.join("posted.json");
        let posted_at = posted.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let (head, body) = read_request(&stream);
                let answer = match head.starts_with("POST") && head.contains("/reviews") {
                    true => {
                        std::fs::write(&posted_at, &body).unwrap();
                        "{}"
                    }
                    false => r#"{"head":{"sha":"sha11"}}"#,
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
        store_critique(
            "crit",
            &mut Critique {
                number: 11,
                head_sha: "sha11".into(),
                overall: "nothing to flag.".into(),
                comments: Vec::new(),
                truncated: false,
                written_at: String::new(),
                posted: None,
            },
        )
        .unwrap();

        post_critique(
            &repo,
            11,
            "sha11",
            "nothing to flag.",
            &[],
            crate::prq::Verdict::Approve,
        )
        .expect("approving with the review posts it");

        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&posted).unwrap()).unwrap();
        assert_eq!(
            sent["event"], "APPROVE",
            "the press says approve, so GitHub has to be asked for an approval: {sent}"
        );
        assert_eq!(
            sent["body"], "nothing to flag.",
            "the approval carries skein's own words, which is the whole of what the press promises"
        );
        assert!(
            critiqued("crit", 11)
                .expect("the draft is still on disk after approving with it")
                .posted
                .is_some(),
            "approving with the review left it looking unposted, so the row will offer to say it \
             all a second time — SKEIN-369, and #691 by the other button"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "GH_TOKEN"] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_renames();
    }

    /// **A draft that predates the timestamp is still datable** (SKEIN-364), because the page's
    /// floor for "you may have posted this already" compares `written_at` against the timestamps of
    /// the review threads you opened — and every draft on the owner's disk right now was written
    /// before the field existed. The file's own mtime is the answer, and it is already in hand.
    #[test]
    fn a_draft_written_before_the_stamp_existed_still_says_when_it_was_written() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        // Written the way an older skein wrote them: no `written_at` key at all.
        let path = critique_path("old", 3, "sha3");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"number":3,"head_sha":"sha3","overall":"o","comments":[],"truncated":false}"#,
        )
        .unwrap();

        // And the copy a DRAFTER hands back carries the same stamp as the file it just wrote.
        // `vet_and_store_critique` stores and then returns the value, so a stamp that only landed
        // on the written copy would give the page two different answers depending on whether the
        // review reached it straight from the model call or off disk a refresh later.
        let mut fresh = Critique {
            number: 4,
            head_sha: "sha4".into(),
            overall: "o".into(),
            comments: Vec::new(),
            truncated: false,
            written_at: String::new(),
            posted: None,
        };
        store_critique("old", &mut fresh).unwrap();
        assert!(
            !fresh.written_at.is_empty(),
            "the stamp landed on the file and not on the record the drafter hands back"
        );
        assert_eq!(
            critiqued("old", 4).map(|c| c.written_at),
            Some(fresh.written_at.clone()),
            "the record in hand and the record on disk say different things about one review"
        );

        let back = critiqued("old", 3).expect("a file from an older skein still parses");
        assert!(
            back.posted.is_none(),
            "a draft from before receipts existed must read as UNPOSTED — the safe direction, \
             because the other one withholds a review nobody sent"
        );
        assert!(
            back.written_at.starts_with("20") && back.written_at.ends_with('Z'),
            "no date to compare against, so the floor has nothing to stand on: {:?}",
            back.written_at
        );
        // The shape matters as much as the value: GitHub's `createdAt` is compared against this as
        // a STRING, so an offset or a different width would silently make every comparison wrong.
        assert_eq!(
            back.written_at.len(),
            20,
            "not `YYYY-MM-DDTHH:MM:SSZ`, so it does not order against GitHub's timestamps: {:?}",
            back.written_at
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// **A row can hold last commit's review AND the reason nothing was drafted for this one**
    /// (SKEIN-355 meeting SKEIN-275). `critique_because` used to be emptied whenever any critique
    /// was present, which was the same question as "at this head" only while the payload filtered
    /// drafts to the head. It no longer does, so the two had to come apart.
    #[test]
    fn an_older_draft_does_not_swallow_the_reason_nothing_was_drafted_at_this_head() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);

        store(
            "demo",
            &Summary {
                number: 8,
                head_sha: "before".into(),
                line: "read before the push.".into(),
                detail: "read before the push.".into(),
                flags: Vec::new(),
                signals: Vec::new(),
                yours: Vec::new(),
                others: 0,
                ownership_unknown: String::new(),
                depth: Depth::Line,
                unread_because: String::new(),
                not_reread: String::new(),
                computed: false,
                budget_stopped: false,
            },
        )
        .unwrap();
        store_critique(
            "demo",
            &mut Critique {
                number: 8,
                head_sha: "before".into(),
                overall: "the review of the earlier commit.".into(),
                comments: Vec::new(),
                truncated: false,
                written_at: String::new(),
                posted: None,
            },
        )
        .unwrap();
        note_critique_tried("demo", 8, "after", "the model would not answer");

        let rows = known("demo", &[(8, "after".to_string())]);
        let row = &rows[&8];
        assert!(
            row.critique.is_some(),
            "the review of the earlier commit was withheld again"
        );
        assert_eq!(
            row.critique_because, "the model would not answer",
            "the row holds an older review and no reason for the missing current one, which is \
             the exact absence SKEIN-275 exists to close"
        );

        std::env::remove_var("SKEIN_HOME");
    }

    /// And when a post DOES fail, the sentence is about posting. The one the owner was shown named
    /// a repository, five membership searches and a refresh — none of which they had asked for, and
    /// none of which was what went wrong from where they stood (SKEIN-272).
    #[test]
    fn a_post_that_fails_says_something_about_posting() {
        let _g = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        std::env::set_var("SKEIN_HOME", home.as_ref() as &std::path::Path);
        std::env::set_var("GH_TOKEN", "gho_test");
        crate::prq::forget_host_token();
        crate::prq::forget_renames();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::Write as _;
                let (head, _body) = read_request(&stream);
                let (code, answer) = match head.starts_with("POST") && head.contains("/reviews") {
                    true => (
                        403,
                        r#"{"message":"Resource not accessible by integration"}"#,
                    ),
                    false => (200, r#"{"head":{"sha":"sha11"}}"#),
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
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
        // A draft on disk, so the refusal has something it could wrongly mark posted.
        store_critique(
            "crit",
            &mut Critique {
                number: 11,
                head_sha: "sha11".into(),
                overall: "note".into(),
                comments: Vec::new(),
                truncated: false,
                written_at: String::new(),
                posted: None,
            },
        )
        .unwrap();

        let why = post_critique(
            &repo,
            11,
            "sha11",
            "note",
            &[],
            crate::prq::Verdict::Comment,
        )
        .expect_err("GitHub refused the post, so the post failed");

        assert!(
            why.contains("Resource not accessible"),
            "the reason is GitHub's own words about the write: {why}"
        );
        // **A refused post writes no receipt** (SKEIN-364). The receipt is what takes the post
        // control away, so one written for a review GitHub never took would withhold a review that
        // is not there — SKEIN-355's failure, arrived at from the opposite direction.
        assert!(
            critiqued("crit", 11)
                .expect("the draft survives a refusal")
                .posted
                .is_none(),
            "a review GitHub refused was marked as posted, so the reader can no longer post it"
        );
        assert!(
            !why.contains("membership") && !why.contains("refresh"),
            "the reader is being told about a queue refresh they did not ask for: {why}"
        );

        for key in ["SKEIN_HOME", "SKEIN_GITHUB_API", "GH_TOKEN"] {
            std::env::remove_var(key);
        }
        crate::prq::forget_host_token();
        crate::prq::forget_renames();
    }
}
