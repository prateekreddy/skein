// A drafted review is SHOWN when the branch has moved, and STOPS being offered once it is posted
// (SKEIN-355, SKEIN-364).
//
// Both are the same missing fact, which is why they are one suite: a drafted review has to know
// whether it has been POSTED, which is a different question from whether it is CURRENT. Fixing
// either one on its own breaks the other — hide drafts more eagerly and 355 comes back, keep every
// draft for ever and 364 does.
//
// The two reports, from live use on the owner's own fleet:
//
//   * #731 — "it doesn't show the review at all, the text says review below but nothing exists. Why
//     is that? Is it because new commits were added that you dropped the review, I thought I was
//     clear that should not happen, we even build a mechanism to post such reviews still." He is
//     right about the mechanism: `prq::submit_review_with_comments` re-anchors a drafted review
//     onto the live head by line text and folds what no longer matches into a body naming both
//     commits. The payload filter in `review::known` was the only thing making it unreachable.
//   * #691 — "it shows the review while the review was already submitted and shows up in comments
//     basically this is prone to giving the same comments again and again. Isn't it easy to detect
//     this and avoid?" The draft stays on disk after posting and `worth_critiquing` will not draft
//     a second one at the same head, so the post control went on being offered for a review GitHub
//     already had.
//
// **What this suite refuses to be satisfied by.** Two whole classes of check on this pane turned
// out to be vacuous, both because the marker they looked for was present in the state the check was
// supposed to reject. So every "is the review shown" assertion here looks for DEEP_IN_THE_REVIEW —
// a string that appears only in the overall note, past the heading, past every chip, and reachable
// only by a full render of the section.
//
//   node tests/ui/revdraft.mjs
import { draftRules, grab, harness } from "./lift.mjs";

const t = harness();

// Only ever printed by a full render of the review body. A check that greps for "review" or for the
// sha is answered by the chip and by the heading; this is answered by neither.
const DEEP_IN_THE_REVIEW = "DEEP-IN-THE-REVIEW-the-lock-is-taken-twice-on-the-error-path";
const DEEP_IN_A_COMMENT = "DEEP-IN-A-COMMENT-this-returns-before-the-unlock";

const HEAD = "eeb866f5d8c8";
const DRAFTED_AT = "a40e3ea97d86";
const WROTE_AT = "2026-08-26T09:00:00Z";

function critique(over = {}) {
  return {
    number: 731, head_sha: HEAD, truncated: false,
    overall: DEEP_IN_THE_REVIEW,
    written_at: WROTE_AT,
    comments: [
      { path: "src/audit.rs", line: 40, anchored: true, text: DEEP_IN_A_COMMENT, line_text: "  lock();" },
      { path: "src/lib.rs", line: 0, anchored: false, text: "and the caller cannot tell" },
    ],
    ...over,
  };
}

// The row exactly as `review::Known` serialises it: the summary flattened, `has_critique` and
// `drafted` — the row's own vocabulary, which survives `Known::thin` — and the critique itself,
// which does not. `drafted` is DERIVED from the critique here for the same reason `Known::new`
// derives it there: a fixture where the two disagree is a payload the server cannot produce, and a
// suite built on one proves nothing about the page.
function row(k, over = {}) {
  return {
    depth: "line", line: "moves the audit write behind the lock", flags: [], head_sha: HEAD,
    has_critique: true,
    drafted: {
      head_sha: k.head_sha, comments: (k.comments || []).length,
      posted_at: (k.posted || {}).at || "", written_at: k.written_at || "",
    },
    critique: k,
    ...over,
  };
}

function world() {
  const body = `
    let revSums = new Map();
    let revCrits = new Map();
    let revPending = new Map();
    let revOpen = new Set();
    let revUpdated = new Set();
    let revInFlight = new Map();
    let revQueue = { queues: [{ repo_id: "acme", viewer: "me" }], prs: [] };
    let revCommonChips = new Set();
    const rk = pr => pr.repo_id + "#" + pr.number;
    const revReceiptHtml = () => "<span class=receipt>held</span>";
    const revNotesFor = () => [];
    ${draftRules()}
    ${grab("revDraftedReview")}
    ${grab("revNoDraftWhy")}
    ${grab("revReviewToPost")}
    ${grab("revApproveWithReviewHtml")}
    ${grab("revDraftVintageHtml")}
    ${grab("revDraftSection")}
    ${grab("revReadyChip")}
    // A reading is not a review (SKEIN-371): the two things a row draws when a review was bought
    // here and did not come back — the mark, and the control that buys it again.
    ${grab("revNoReviewCameBack")}
    ${grab("revNoDraftChip")}
    const revFlows = new Map([["acme", { read_prs: false }]]);
    ${grab("revReadsAhead")}
    ${grab("revSkeinsToRead")}
    ${grab("revReadAgain")}
    ${grab("revCritActsHtml")}
    return {
      // The two marks a collapsed line can carry about a missing review.
      noDraftChip: pr => revNoDraftChip(pr),
      readControl: pr => revReadAgain(pr),
      put: (pr, s) => revSums.set(rk(pr), s),
      section: pr => revDraftSection(pr),
      chip: pr => revReadyChip(pr),
      why: pr => revNoDraftWhy(pr),
      held: pr => revDraftHeld(pr),
      review: pr => revDraftedReview(pr),
      older: (pr, k) => revDraftIsOlder(pr, k),
      posted: pr => revDraftPosted(pr),
      echoes: (pr, at) => revDraftEchoes(pr, at),
      toPost: pr => revReviewToPost(pr),
      // §4's minority rule, driven rather than described: a kind worn by most of the queue leaves
      // the collapsed line.
      commons: ks => { revCommonChips = new Set(ks); },
      // The vetting panel's action strip, which is the OTHER place a review is sent from.
      vet: (pr, k) => { revCrits.set(rk(pr), { open: true, busy: false, posting: false,
                          critique: k, drop: new Set(), posted: "" });
                        return revCritActsHtml(pr); },
    };
  `;
  return new Function("esc", "console", "renderReviewNow", "renderReview", body)(
    s => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;"),
    console, () => {}, () => {});
}

const PR = { repo_id: "acme", number: 731, head_sha: HEAD, reasons: ["reviewer"], review_threads: [] };

// ── 0. a review that was bought here and did not come back ────────────────────────────────────
//
// SKEIN-371. On the owner's 20-step stack, ten rows carried a summary, `has_critique: false` and
// `critique_because: "skein could not reach the fleet sandbox … so the model was never asked"` —
// and each of them rendered exactly like a reviewed one, with no control anywhere on the line. The
// pane said "✓ read the stack 10 of 10 done" over them.
//
// Two marks, and they are not the same mark. The chip (SKEIN-275) is what a reader SCANS, and §4
// rations it away the moment most of the queue wears it — which is exactly the reported case, ten
// rows of twenty. So the control has to carry the sentence as well as the move, or the row goes
// back to saying nothing in the one case that matters.
{
  const w = world();
  const unreviewed = { depth: "line", line: "moves the audit write behind the lock", flags: [],
                       head_sha: HEAD, has_critique: false,
                       critique_because: "skein could not reach the fleet sandbox" };
  w.put(PR, unreviewed);

  const control = w.readControl(PR);
  t.check("the row offers the read that would buy the missing half",
    control.includes("revReadAgainPress('acme', 731)"), true);
  t.check("and its label says what is missing, not just that a press exists",
    control.includes("no review — read again"), true);
  t.check("carrying the reason skein was given", control.includes("could not reach the fleet sandbox"), true);
  // The chip is the SCANNING mark and it is rationed by §4: on the owner's stack ten rows of twenty
  // wore it, so `revCommonChips` takes it off the line and the control is the only thing left
  // saying the review is missing. That is why the label above has to say it and not just "re-read".
  t.check("the chip states the absence too, while the queue leaves room for it",
    w.noDraftChip(PR).includes("no review"), true);
  w.commons(["nodraft"]);
  t.check("and when most of the queue wears it the chip goes, leaving the control to say it",
    [w.noDraftChip(PR), w.readControl(PR).includes("no review — read again")], ["", true]);
  w.commons([]);

  // The counter-case: nothing ever tried to draft a review here, so there is nothing to retry and
  // the chip is the honest mark. Without this, "no review" becomes a property of every undrafted
  // row and the pane starts offering presses that answer the same way every time.
  const never = { ...unreviewed, critique_because: "" };
  w.put(PR, never);
  t.check("a row nothing ever tried to review offers no retry", w.readControl(PR), "");
  t.check("and keeps the mark that states the absence",
    w.noDraftChip(PR).includes("no review"), true);
}

// ── 1. a review of the commit in front of you, which is the case that always worked ────────────
{
  const w = world();
  const k = critique();
  w.put(PR, row(k));

  t.check("a current review is shown", w.section(PR).includes(DEEP_IN_THE_REVIEW), true);
  t.check("under the heading that says it read this commit",
    w.section(PR).includes("skein's review of this commit"), true);
  t.check("and it says nothing about an earlier revision, because there is not one",
    w.section(PR).includes("Written against the previous revision"), false);
  t.check("the chip claims no vintage either", w.chip(PR).includes("earlier commit"), false);
  t.check("and the row does not also say no review was drafted", w.why(PR), null);
}

// ── 2. a review of an EARLIER commit is shown, named, and still postable (SKEIN-355) ───────────
//
// This is #731. Measured on his fleet: queue head eeb866f5d8c8, stored critique head a40e3ea97d86,
// two comments, overall intact — and `has_critique=None, drafted=None, critique=ABSENT` on the row.
{
  const w = world();
  const k = critique({ head_sha: DRAFTED_AT });
  w.put(PR, row(k));
  const html = w.section(PR);

  // The whole of the defect, in one assertion: the review was on disk and the pane said nothing.
  t.check("a review drafted before the latest commits is SHOWN, not dropped",
    html.includes(DEEP_IN_THE_REVIEW), true);
  t.check("with its comments, which is the part that costs money to produce",
    html.includes(DEEP_IN_A_COMMENT), true);

  // …and the thing the old filter was protecting, kept as a LABEL rather than as a deletion.
  t.check("the heading names the commit it read, so it is never read as a review of this one",
    html.includes(`skein's review of <code>${DRAFTED_AT.slice(0, 7)}</code>, an earlier commit`), true);
  t.check("in the owner's own words — written against the previous revision",
    html.includes("Written against the previous revision"), true);
  t.check("and the sha is on the line, so it can be checked rather than believed",
    html.includes(`<code>${DRAFTED_AT.slice(0, 7)}</code>`), true);

  // Postable. The mechanism he is talking about — re-anchoring by line text — already exists in
  // `prq::submit_review_with_comments`; the payload rule was the only thing hiding it.
  t.check("posting it is still offered", html.includes("go through 2 comments and post…"), true);
  t.check("and the pane says what posting it will do to the comments",
    /still match go/.test(html) && /travel in the review note/.test(html), true);

  const post = w.toPost(PR);
  t.check("what would be sent names the commit the draft read, which is what re-anchors it",
    post.draftedAt, DRAFTED_AT);
  t.check("and carries the line text each comment was drafted against",
    post.comments[0].text, "  lock();");

  // On the line you scan, so a queue of them can be told apart without opening every row.
  t.check("the chip stays, because the review exists", w.chip(PR).includes("review ready · 2"), true);
  t.check("and says which vintage it is", w.chip(PR).includes("earlier commit"), true);
  t.check("wearing its own treatment rather than the current review's",
    w.chip(PR).includes('class="revtag ready older"'), true);

  // The absence-with-a-reason block must not fire beside a review that is plainly there.
  t.check("the row does not say skein drafted no review while showing one", w.why(PR), null);
}

// ── 3. a review already POSTED stops being offered (SKEIN-364) ─────────────────────────────────
//
// This is #691. The receipt is skein's own — written by `review::post_critique` after GitHub
// accepted it — because GitHub cannot be asked precisely: `my_review` comes from
// `latestOpinionatedReviews`, which excludes the COMMENTED verdict a critique posts under, and
// `review_threads` carries no comment bodies at all.
{
  const w = world();
  const k = critique({ posted: { at: "2026-08-26T10:12:23Z", onto: HEAD } });
  w.put(PR, row(k));
  const html = w.section(PR);

  t.check("a posted review is still SHOWN — what skein said is worth reading either way",
    html.includes(DEEP_IN_THE_REVIEW), true);
  t.check("and says it is already on GitHub", html.includes("Already posted to GitHub"), true);
  t.check("naming when, so the reader can find it there",
    html.includes("2026-08-26T10:12:23Z"), true);
  t.check("and not a commit, because it landed on the one it read",
    html.includes(", onto <code>"), false);

  // The whole point: the press that would say it all a second time is gone.
  t.check("the way to post it again is gone", html.includes("and post…"), false);
  t.check("and so is approving with it, which would send the same words",
    html.includes("approve with this review"), false);
  t.check("what is left is the one thing that makes sense — buy a new reading",
    html.includes("read it again, and draft a new one"), true);

  t.check("the chip says posted, not ready", w.chip(PR).includes("review posted"), true);
  t.check("so a scan of the queue does not read it as work waiting",
    w.chip(PR).includes("review ready"), false);

  // The vetting panel is the OTHER surface that sends this review, reached by opening a row whose
  // draft went out in an earlier session — where the page's own memory of its press is empty and
  // only the stored receipt knows.
  const strip = w.vet(PR, k);
  t.check("the panel's strip offers no post either", strip.includes("as one review"), false);
  t.check("and says why, where the control was", strip.includes("posted to GitHub"), true);
}

// ── 4. the counter-case, in the same shape ─────────────────────────────────────────────────────
//
// Without this, "posted" is a property of the code path rather than of the review, and every draft
// nobody has sent would be withheld — which is SKEIN-355 again, arrived at from the other side.
{
  const w = world();
  const k = critique();
  w.put(PR, row(k));

  t.check("a review nobody has posted is still offered", w.section(PR).includes("and post…"), true);
  t.check("with no receipt claiming otherwise",
    w.section(PR).includes("Already posted to GitHub"), false);
  t.check("and the chip says it is ready", w.chip(PR).includes("review ready · 2"), true);
  t.check("the panel's strip offers the post",
    w.vet(PR, k).includes("post 2 comments as one review"), true);
  t.check("and the row shape carries no receipt to read", w.posted(PR), null);
}

// ── 5. the floor, for a draft posted by a path that wrote no receipt ───────────────────────────
//
// Every draft on the owner's disk right now was written before receipts existed. `review_threads`
// carries `author` and `started_at` and NOTHING ELSE — no path, no line, no body — so this cannot
// claim those threads ARE this review, and it does not: it warns, and leaves the post working.
{
  const w = world();
  const k = critique();
  const mine = { id: "t1", author: "me", started_at: "2026-08-26T10:12:23Z" };
  const before = { id: "t0", author: "me", started_at: "2026-08-26T08:00:00Z" };
  const theirs = { id: "t2", author: "someone", started_at: "2026-08-26T10:12:23Z" };

  t.check("line comments of yours after the draft was written are a reason to look",
    w.echoes({ ...PR, review_threads: [mine] }, WROTE_AT), { n: 1, when: mine.started_at });
  t.check("ones from BEFORE it was written cannot be it",
    w.echoes({ ...PR, review_threads: [before] }, WROTE_AT), null);
  t.check("and somebody else's are not yours to have posted",
    w.echoes({ ...PR, review_threads: [theirs] }, WROTE_AT), null);
  t.check("a draft with no written_at has nothing to compare, so it says nothing",
    w.echoes({ ...PR, review_threads: [mine] }, ""), null);

  const pr = { ...PR, review_threads: [mine] };
  w.put(pr, row(k));
  const html = w.section(pr);
  t.check("the warning is drawn where the review is",
    html.includes("You may have posted this already"), true);
  t.check("saying exactly what it knows and not more",
    html.includes("skein has no receipt for it"), true);
  // It warns; only a receipt refuses. A floor that took the control away would lose a review the
  // reader wrote his own line notes beside — which is the ordinary case, not the rare one.
  t.check("and the post is still offered, because a floor is not a receipt",
    html.includes("go through 2 comments and post…"), true);

  // And a RECEIPT silences the floor, rather than the row saying both things at once.
  w.put(pr, row(critique({ posted: { at: "2026-08-26T11:00:00Z", onto: HEAD } })));
  t.check("a review with a receipt says the receipt, not the guess",
    [w.section(pr).includes("Already posted to GitHub"),
     w.section(pr).includes("You may have posted this already")], [true, false]);
}

// ── 5b. ready and posted are demoted apart (SKEIN-364 meeting §4) ─────────────────────────────
//
// The minority rule takes a chip off the line when most of the queue wears it. Counted as ONE kind,
// a lane skein has drafted for would take the mark off the handful already SENT — which are the
// minority, and the ones whose chip changes what you do (nothing).
{
  const w = world();
  const posted = critique({ posted: { at: "2026-08-26T10:12:23Z", onto: HEAD } });

  w.put(PR, row(critique()));
  w.commons(["ready"]);
  t.check("a queue mostly drafted-and-waiting takes the ready chip off the line",
    w.chip(PR), "");

  w.put(PR, row(posted));
  t.check("but a review already posted keeps its mark, because it is not the common one",
    w.chip(PR).includes("review posted"), true);

  w.commons(["posted"]);
  t.check("and the demotion works the other way round too", w.chip(PR), "");
  w.put(PR, row(critique()));
  t.check("with a waiting review still marked while posted is the common kind",
    w.chip(PR).includes("review ready"), true);
}

// ── 6. the two questions stay apart ────────────────────────────────────────────────────────────
//
// Posted and current are different facts, and the row has to be able to hold any combination.
{
  const w = world();
  const older = critique({ head_sha: DRAFTED_AT });
  const olderPosted = critique({ head_sha: DRAFTED_AT, posted: { at: "2026-08-26T10:12:23Z", onto: HEAD } });

  t.check("an older review is older whatever its posting state",
    [w.older(PR, older), w.older(PR, olderPosted)], [true, true]);
  t.check("and a current one is current whatever its posting state",
    [w.older(PR, critique()), w.older(PR, critique({ posted: { at: "x", onto: HEAD } }))], [false, false]);

  w.put(PR, row(olderPosted));
  const html = w.section(PR);
  t.check("a review that is both says both",
    [html.includes("an earlier commit"), html.includes("Already posted to GitHub")], [true, true]);
  // Two shas, because a review of an earlier commit lands on the one that is there now. A receipt
  // naming only the drafted sha sends the reader to a commit the review is not on.
  t.check("and the receipt names the commit it LANDED on, not only the one it read",
    html.includes(`, onto <code>${HEAD.slice(0, 7)}</code>`), true);
  t.check("and is not offered for posting, because posted wins over postable",
    html.includes("and post…"), false);

  // A row with no head of its own cannot decide the question, and must not guess: labelling a
  // current review as stale is the same misinformation as hiding a stale one, pointing the other
  // way.
  t.check("an unknown head is not an older one", w.older({ repo_id: "acme", number: 731 }, older), false);
}

t.done();
