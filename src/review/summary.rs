//! What a reading of a pull request IS, before anything has been read.
//!
//! The vocabulary the rest of this module and the whole cockpit speak in: how much skein is
//! prepared to vouch for ([`Depth`]), what it vouched for ([`Summary`]), what a queue row is
//! shown of that ([`Known`]), and what consulting CODEOWNERS answered ([`Ownership`]) — the one
//! stage-0 fact that scopes every prompt written later.
//!
//! Nothing here reads a pull request or spends anything. It is the shape the answers come back in,
//! which is why the module's own rule — a failure may only ever leave [`Depth::Unread`] — is
//! stated in these types rather than guarded at the call sites.

use crate::codeowners;
use crate::repos::Repo;
use serde::{Deserialize, Serialize};

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
    /// **This reading did not run in its box** (SKEIN-799). Empty on every reading that did, which
    /// is nearly all of them.
    ///
    /// The reading itself succeeded, which is why nothing said so before: `ai::claude_in_turn`
    /// tries the pull request's own review box, falls through to a local spawn on any failure to
    /// reach it, and the local spawn answers. There is no `Unread` on that path and
    /// [`Summary::unread_because`] stays empty, so a downgraded reading was indistinguishable from
    /// an ordinary one at every surface. Three things went with the box, and `docs/pr-review.md`
    /// §11 is about the first two: the isolation — the reading holds a GitHub WRITE token while
    /// reading a change somebody else wrote, and in a box it can reach almost nothing — and the
    /// checkout, which is the commit under review rather than whatever directory the server is
    /// standing in.
    ///
    /// **The third is why the sentence has a second half.** `review::checkout::sweep` is a
    /// `Turn::Resuming` that resends nothing, so a sweep whose session is not there comes back
    /// empty and leaves [`Summary::swept`] false — and §7c makes an approval wait on `swept`. A
    /// box lost between turn one and the sweep does not merely downgrade a reading; it can put an
    /// approval permanently out of reach. So [`outside_box_notice`] appends that consequence when
    /// it has actually happened, and says nothing about sweeping when it has not.
    ///
    /// **Composed here rather than at each surface, and stored whole.** It is written to be shown
    /// verbatim, exactly as `unread_because` is, because the surface that most needs it cannot
    /// build it: the cockpit is handed `swept` only when it is TRUE (`skip_serializing_if`), so a
    /// page composing the second half itself could not tell "no sweep accounted for it" from "this
    /// skein is too old to say" — which is the reading `swept`'s own doc forbids.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub read_outside_box: String,
    /// Why the commit that is there NOW was not read (SKEIN-444). Empty on every other reading,
    /// which is nearly all of them.
    ///
    /// A round runs when somebody asks for one — the author re-requesting your review on GitHub,
    /// or you pressing re-read. A pull request that stays in scope because you reviewed it ONCE
    /// (`Reason::Reviewed` never expires) would otherwise buy a round on every push for ever, and
    /// that is the spend this stops.
    ///
    /// It exists because the alternative reads as neglect. Without this sentence the row shows a
    /// reading of an older commit and nothing to say why, so a deliberate choice is
    /// indistinguishable from skein having failed. The way out is named in the same breath: the
    /// re-read press is never rationed, which the owner has said twice.
    ///
    /// It rides beside a reading of an EARLIER commit, deliberately: the reader keeps the review
    /// they had, `Known::stale` still says it describes an older commit, and this says why skein
    /// has not replaced it. Silence there would leave a row that looks current and is not.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub not_reread: String,
    /// **Why this review did not post, when it would have** (SKEIN-516). Empty on every reading
    /// that posted, that had no reason to, and that acts as the owner — nearly all of them.
    ///
    /// Set from the one refusal that is skein's own: `review_identity = "app"`, and the App could
    /// not be acted as (`review::asking::Refused`). The reading still ran and its findings are in
    /// the brief; what did not happen is the post, and without this the row reads exactly like a
    /// review that posted. Composed on the server and drawn verbatim, like
    /// [`Summary::read_outside_box`], so the owner's approved words have one spelling.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub not_posted: String,
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
    /// **Which of the repository's owed checks this change fired** — `docs/pr-review.md` §8,
    /// spelled as [`crate::owed::Check::spelled`].
    ///
    /// Computed here rather than by the engine because this is the only place the whole diff is in
    /// hand: `prwork::facts_of` has a `prq::Pr` and no bytes of the change. Recorded against the
    /// sha for the same reason everything else in this struct is — the triggers are a property of
    /// one tree.
    ///
    /// **`None` is not the empty list, and the difference is the guard.** A summary written before
    /// this field existed, or by a skein that did not compute it, deserialises to `None` — which
    /// `prwork` reads as *unknown* so neither `checks-owed` nor `checks-settled` holds and a
    /// verdict waits. An empty `Some` is a diff that fired nothing, which lets a verdict through.
    /// A plain `Vec` could not tell those apart, and would have read every reading made before
    /// today as "nothing is owed" — the one direction §8 exists to close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owed_triggered: Option<Vec<String>>,
    /// **Did what this reading raised have to block?** — the sweep's second answer, and
    /// `docs/pr-review.md` §7b's other half.
    ///
    /// `Some(true)` the review as it stands is a refusal, `Some(false)` it is not, `None` nobody
    /// asked or the answer could not be read. Three-valued for the same reason
    /// `Summary::owed_triggered` is `Option`: the findings live on GitHub and skein keeps no copy
    /// (§5), so this file is the ONLY place the answer exists — and a fact-set that answered
    /// "nothing blocks" from a sweep that never ran would be an approval granted by silence.
    ///
    /// Recorded at the sha it was computed from, like `owed_triggered`, and cleared by
    /// [`Known::thin`] for the same reason: it is engine state and no row draws it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings_block: Option<bool>,
    /// The ONLY reason this row is unread is that the day's AUTOMATIC budget is spent. The
    /// machine-readable half of the refusal sentence: the pane detects it to render the read
    /// button prominently — the manual trigger the sentence invites, which is never budgeted
    /// (see [`Trigger`]). Omitted from the JSON when false, so older clients see no new key.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub budget_stopped: bool,
    /// **The reading stopped because its box did not answer in time** (SKEIN-818) — the
    /// machine-readable half of [`crate::ai::Unread::BoxSlow`]'s sentence, for the same reason
    /// `budget_stopped` is the machine-readable half of its own: the pane keys a control on it.
    /// Here that is the second of the row's two ways on, "read it here instead", which is a
    /// deliberate spend outside the box that skein declined to make by itself. Omitted from the
    /// JSON when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped_at_box: bool,
    /// **Did the sweep run to the end on this reading?** — the only evidence in the tree that a
    /// pass covered the whole change (`docs/pr-review.md` §7c).
    ///
    /// §7c says `ReadingWhole` "needs no new field on the reading at all", and that was written
    /// from half the code: [`sweep`] discards its own answer (`let _ =`), and nothing else here
    /// records coverage. Not one of `depth`, `line`, `detail`, `flags`, `yours` or `others` is
    /// about what was READ — they are about what was found — so coverage was recoverable only by
    /// inference, and the inference available was "a summary exists, so presumably it looked",
    /// which is the exact shape of the failure §7c exists to stop: the box that posted an approval
    /// and a refusal 53 seconds apart had read the change too, just not all of it.
    ///
    /// **What it claims is only what happened.** [`SWEEP_PROMPT`] asks the review to list every
    /// file the change touches, say honestly which it skimmed, and **go back and read those** — so
    /// a sweep that answers has, on its own instructions, accounted for every changed file at this
    /// commit. That is the whole claim. It is not a model's opinion of its own thoroughness, and
    /// it is not asked for one.
    ///
    /// **Absence is unknown, never covered**, in both directions that matter. `#[serde(default)]`
    /// is `false`, so every reading already cached on disk — written before this field existed —
    /// keeps deserialising and answers "no sweep spoke for me", which is the fail-closed value.
    /// And `false` here may never be read as "the sweep ran and found the pass partial": a sweep
    /// that refuses, times out or answers nothing lands on the same `false`, and those are
    /// blindness. `crate::prwork::facts_of_in` maps this to `Option<bool>` under that rule and can
    /// only ever produce `Some(true)` or `None`.
    ///
    /// This is `review.rs`'s own rule in a field: **AI may only add scrutiny, never remove it.**
    /// The only way to `true` is a sweep that ran and answered; every failure keeps the pull
    /// request at full attention and leaves `Act::PostApproval` unreachable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub swept: bool,
}

impl Summary {
    /// The honest empty answer: this PR has not been read, and here is why.
    pub(super) fn unread(number: u64, head_sha: &str, because: &str) -> Self {
        Summary {
            number,
            head_sha: head_sha.to_string(),
            depth: Depth::Unread,
            line: String::new(),
            detail: String::new(),
            flags: Vec::new(),
            signals: Vec::new(),
            // **`None` and never `Some(vec![])`**: an unread pull request is one skein did not
            // look at, so it has no answer about what its diff owes. An empty list here would say
            // "nothing is owed" on the strength of a reading that never happened, which is the
            // widening direction the field exists to refuse.
            owed_triggered: None,
            findings_block: None,
            // Whether getting here cost anything is the caller's to say: an unread summary is
            // written both by a model call that failed (it did) and by the switch being off (it did
            // not). The paths that spent one set `computed` on the way out: `spent_unread` inside
            // `summarise_and_draft`, and the two failure arms of `summarise_in_stages`.
            computed: false,
            budget_stopped: false,
            stopped_at_box: false,
            // No reading happened, so no sweep did either. Never `true` from here: this is the
            // constructor for every failure, and the failure direction is fixed.
            swept: false,
            yours: Vec::new(),
            others: 0,
            ownership_unknown: String::new(),
            unread_because: because.to_string(),
            // **Empty even on a reading that lost its box**, and the gap is deliberate rather than
            // overlooked: `claude_in_turn` carries the reason out on [`crate::ai::Answered`],
            // which only exists on the path where the local reading SUCCEEDED. A reading that lost
            // its box and then failed here as well has `unread_because` — a sentence about the
            // failure, with its own cure — and that is the one to show.
            read_outside_box: String::new(),
            not_reread: String::new(),
            not_posted: String::new(),
        }
    }
}

/// **What a reader is told when a reading lost its box** — the sentence itself, written once.
///
/// The wording is the owner's and is not paraphrased anywhere else; `why` is
/// [`crate::ai::outside_box_because`]'s plain-language reason, which is the half that varies.
///
/// `swept` decides only whether the second sentence is there at all. It is not a hedge: `false`
/// means no sweep spoke for this reading, and §7c makes that the difference between an approval
/// being reachable and not — see [`Summary::read_outside_box`] for why the composition is here and
/// not at the surfaces.
pub(super) fn outside_box_notice(why: &str, swept: bool) -> String {
    let said = format!(
        "Read outside its box — {why}. The commit under review was not checked out, and this \
         round's conversation was not kept."
    );
    match swept {
        true => said,
        false => format!("{said} No sweep accounted for it, so an approval stays out of reach."),
    }
}

/// The composed notice, for a test in another module that must not spell it out itself.
///
/// `prwork::perform` asserts that its journal line and this sentence never say the sweep's
/// consequence twice, which is a claim about THIS text — so a copy of it written into that test
/// would be a copy that stops agreeing, and the assertion would go on passing against a sentence
/// nobody ships.
#[cfg(test)]
pub(crate) fn summary_notice_for_test() -> String {
    outside_box_notice("its box is gone", false)
}

/// A reading skein already has, and whether it is of the commit that is there now.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Known {
    #[serde(flatten)]
    pub summary: Summary,
    /// True when this reading describes an earlier commit — the branch has moved since.
    pub stale: bool,
}

impl Known {
    /// A reading, and whether the branch has moved since.
    ///
    /// It used to assemble a reading AND the review beside it — `has_critique`, `drafted`, `sent`,
    /// `critique_because`, and the rule for which of them meant what. There is no review beside it
    /// now: the session posts its own to GitHub, so the review lives on the pull request and this
    /// carries what a reader's own pane draws.
    pub(super) fn new(summary: Summary, stale: bool) -> Known {
        Known { summary, stale }
    }

    /// The same reading with the PROSE taken out — what a queue ROW draws, and nothing else.
    ///
    /// Measured on the owner's fleet (2026-08-25): `GET /review/summaries` answered 153,381 bytes
    /// for thirty-nine stored readings, every one carrying its full brief, its signals and the
    /// whole drafted review — none of which a collapsed row draws. Reproduced locally at 155,167
    /// bytes against 12,055 for the same thirty-nine
    /// (`tests/server/requests.rs::the_review_queue_payload_can_be_asked_for_rows_instead_of_prose`).
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
    /// What goes, and where the page reads it (all of it behind the fold, in `revDetail`):
    /// `detail`, `signals`, `yours` and `others`, `ownership_unknown`. What stays is the line, the
    /// flags, the depth and its reason, and the head — the row's own vocabulary.
    pub fn thin(mut self) -> Known {
        self.summary.detail = String::new();
        self.summary.signals = Vec::new();
        self.summary.yours = Vec::new();
        self.summary.others = 0;
        self.summary.ownership_unknown = String::new();
        // **Behind the fold, like the brief** (SKEIN-799/400). It is a paragraph, and the surface
        // that draws it is `revDetail` — which fetches the whole reading when a row opens. Left on
        // the row payload it would ride thirty-nine readings for a reader who is not there, and
        // `the_row_shape_carries_only_what_a_row_draws` is what noticed: it failed the moment the
        // field was added, before this line was.
        self.summary.read_outside_box = String::new();
        // Behind the fold for the same reason: `revDetail` draws it, from the whole reading.
        self.summary.not_posted = String::new();
        // Engine state, and a row draws none of it: which of §8's checks a diff fired is read by
        // `prwork::facts_of_in` off the cache on disk, never off a queue payload. Cleared here
        // rather than left to `skip_serializing_if`, because on a reading that ran it is `Some`
        // and would ride every row of every queue for the sake of a reader that is not there.
        self.summary.owed_triggered = None;
        self.summary.findings_block = None;
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
/// `stale` is DERIVED, not declared. The caller has normally just computed a reading for
/// `head_sha`, so the answer is normally false — but "normally" is not a thing to hard-code. The
/// gate can return the READING IT ALREADY HAD, still named by the older commit it describes, and a
/// `false` written in by hand there tells the reader a superseded reading is current and hides the
/// `not_reread` line that would have explained it. Same defect and same fix as SKEIN-433 in
/// [`known`]; this arm was missed then. Comparing costs nothing and cannot go out of date.
pub fn known_at(_repo_id: &str, summary: Summary, head_sha: &str) -> Known {
    let stale = summary.head_sha != head_sha;
    Known::new(summary, stale)
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
    pub(super) fn split(&self) -> (Vec<String>, usize) {
        match self {
            Ownership::Owned { yours, others } => (yours.clone(), *others),
            _ => (Vec::new(), 0),
        }
    }

    /// Why ownership could not be consulted — `None` when it was, including when it was consulted
    /// and genuinely does not exist.
    pub(super) fn unread_why(&self) -> Option<&str> {
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
#[cfg(test)]
mod tests {
    use super::*;

    /// **The notice names the unreachable approval only when it is true** (SKEIN-799).
    ///
    /// The second sentence is not a hedge and not decoration: `swept` false is what makes
    /// `Act::PostApproval` unreachable (§7c), and a reading that lost its box between turn one and
    /// the sweep is exactly how that happens with nothing on screen. Said on every notice it would
    /// be a warning nobody could act on; left off the ones that need it, it is the silence the
    /// item was filed about.
    ///
    /// **What makes it fail:** appending the clause unconditionally (the first `assert!` below),
    /// or never appending it (the last). The owner's wording is asserted verbatim here because it
    /// IS the specification — this is the one place it is written down in the tree.
    #[test]
    fn the_notice_names_the_unreachable_approval_only_when_no_sweep_spoke() {
        let swept = outside_box_notice("its box is gone", true);
        assert_eq!(
            swept,
            "Read outside its box — its box is gone. The commit under review was not checked \
             out, and this round's conversation was not kept.",
            "the sentence a reader is shown is not the one that was specified"
        );
        assert!(
            !swept.to_lowercase().contains("approval"),
            "a reading a sweep DID account for was told an approval is out of reach: {swept}"
        );

        let unswept = outside_box_notice("its box is gone", false);
        assert!(
            unswept.starts_with(&swept),
            "the two notices disagree about everything but the sweep: {unswept}"
        );
        assert_eq!(
            unswept,
            format!("{swept} No sweep accounted for it, so an approval stays out of reach."),
            "the consequence that makes this more than cosmetic was not said"
        );
    }

    /// **The notice survives the trip through the cache**, and its absence does not break one
    /// written before it existed (SKEIN-799).
    ///
    /// Both halves matter and only one is obvious. `prwork::facts_of_in` and the open row both
    /// read a reading back off disk, so a field that does not survive `serde` is a field the
    /// reader never sees. And every reading already filed by this fleet was written without the
    /// key: `#[serde(default)]` is what keeps those deserialising at all, and the value it
    /// defaults to — empty, meaning "ran where it was addressed" — is the one that claims nothing.
    ///
    /// **What makes it fail:** dropping `#[serde(default)]` (the second half stops deserialising
    /// at all) or `skip_serializing_if` going with the field's own serialisation (the first).
    #[test]
    fn a_reading_that_lost_its_box_still_says_so_after_a_trip_through_the_cache() {
        let mut filed = crate::review::testkit::fat(7, "aaa").summary;
        filed.read_outside_box = outside_box_notice("its box is gone", false);
        let wire = serde_json::to_string(&filed).expect("a reading serialises");
        let back: Summary = serde_json::from_str(&wire).expect("and reads back");
        assert_eq!(
            back.read_outside_box, filed.read_outside_box,
            "the notice did not survive being filed, so nothing downstream can ever show it"
        );

        // A reading filed before this field existed. Written by taking the key back out of the
        // very JSON above, rather than by hand: a fixture that spelled the old shape itself would
        // be asserting against a file no skein ever wrote.
        let mut older: serde_json::Value = serde_json::from_str(&wire).unwrap();
        older
            .as_object_mut()
            .expect("a reading is an object")
            .remove("read_outside_box")
            .expect("the key was there to remove, or this half is about nothing");
        let old: Summary =
            serde_json::from_value(older).expect("a reading filed before the field still reads");
        assert_eq!(
            old.read_outside_box, "",
            "a reading written before this existed came back claiming something about its box"
        );
    }
    use crate::review::testkit::*;

    /// The default that makes the queue worth opening. A fresh install, and an existing
    /// `config.json` written before this field existed, must both read as on — `#[serde(default)]`
    /// on a bool would silently make every upgraded user's queue unread.
    #[test]
    fn reading_prs_is_on_unless_you_turn_it_off() {
        let _lock = crate::testutil::env_lock();
        let home = crate::testutil::tempdir();
        // Bound after `home`, so the pin goes back before the directory it names is removed.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path)
            .unset("SKEIN_REVIEW_AI");
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
        // Bound after `home`, so the pin goes back before the directory it names is removed.
        let mut env = crate::testutil::env_pins();
        env.set("SKEIN_HOME", home.as_ref() as &std::path::Path)
            .unset("SKEIN_REVIEW_AI")
            .set("SKEIN_AI", "off");
        assert!(
            summaries_enabled(),
            "turning off board enrichment must not stop the review queue reading PRs"
        );
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
                "flags",
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
    }
}
