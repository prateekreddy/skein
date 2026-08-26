// SKEIN-162: a verdict gives feedback where the eye is, and can be taken back.
//
// The measured failure this exists to keep dead: press approve → a toast lands ~900px from the
// bar you pressed → loadReview(true) → 2-4s of network → the whole pane rebuilds → the row still
// reads "you have not reviewed this". No undo, and a refusal was a toast that disappeared.
//
// The contracts, against the real functions lifted out of index.html (docs/review-ux.md §7.1):
//   * pressing a verdict posts NOTHING — the bar collapses in place to `✓ approved · undo · 8s`;
//   * undo inside the window cancels a request that never left the machine;
//   * the window's lapse posts exactly once, with the payload assembled at press time
//     (comments and drafted_at included);
//   * a second verdict inside the window replaces the first — only the second posts;
//   * a refusal turns the bar to `✗ GitHub refused: … · try again · open on GitHub ↗` and STAYS,
//     and try-again re-fires the same payload;
//   * success marks the queue row done IN PLACE — no loadReview, no /api/review fetch;
//   * set aside (archivePr) rides the same shape: receipt, undo, one POST after the window.
//
// Timers are a driven clock, not sleeps: the suite advances time and watches what fires.
//
//   node tests/ui/undo.mjs
import { grab, harness, pure } from "./lift.mjs";

const t = harness();

// The clock the page's setTimeout/clearTimeout run on. advance(ms) runs everything that falls
// due, in order — so "7999ms posts nothing, 8000ms posts once" is a statement, not a race.
function clock() {
  let now = 0, seq = 1;
  const due = new Map();
  return {
    setT: (fn, ms) => { const id = seq++; due.set(id, { fn, at: now + (ms || 0) }); return id; },
    clearT: id => due.delete(id),
    advance(ms) {
      const end = now + ms;
      for (;;) {
        const next = [...due.entries()].filter(([, x]) => x.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (!next) break;
        now = next[1].at;
        due.delete(next[0]);
        next[1].fn();
      }
      now = end;
    },
  };
}

// The page's world, stubbed down to what the hold/undo/fire flow touches.
function world(opts = {}) {
  const posts = [];
  let answer = { ok: true, text: "approved" };
  const fetch = (url, init) => {
    posts.push({ url, body: init && init.body ? JSON.parse(init.body) : null });
    const a = answer;
    return Promise.resolve({ json: () => Promise.resolve(a) });
  };
  const clk = clock();
  // The one queue row the repaint may touch, and the reading bar it may replace — each captures
  // what was written into it, so "repainted in place" is observable without a browser.
  const row = { html: "" };
  Object.defineProperty(row, "outerHTML", { set(v) { row.html = v; }, get() { return row.html; } });
  let barHtml = "";
  const bar = {};
  Object.defineProperty(bar, "outerHTML", { set(v) { barHtml = v; } });
  const spans = opts.spans || [];
  // One open review thread's line, captured the same way (SKEIN-305). It is queried by
  // `data-thread` and never by `.revrow` — a pull request inside a stack is drawn as a `.step`, so
  // a selector reaching for the row around it finds nothing and the press dies silently.
  const thread = { html: "" };
  Object.defineProperty(thread, "outerHTML", { set(v) { thread.html = v; }, get() { return thread.html; } });
  let threadDrawn = true;   // is that line on screen? (a closed row is the case where it is not)
  const revpane = {
    querySelector: sel => sel === ".readbar" ? bar
      : sel.startsWith("[data-thread=") ? (threadDrawn ? thread : null)
      : sel.startsWith(".revrow") ? row : null,
    querySelectorAll: sel => sel === "[data-undo-left]" ? spans : [],
  };
  const store = new Map();
  const localStorage = {
    getItem: k => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, v),
    removeItem: k => store.delete(k),
  };
  const src = `
    // Whose move a pull request is, from cockpit/src/move.mjs — the one place the rule lives
    // (SKEIN-302), so a world that stubbed it would be testing a second copy of it.
    ${pure("move")}
    let revComposing = null;
    // SKEIN-159's keyboard state, referenced by revRow (sel/flash/held) and revHold (last act).
    let revSel = null, revFlash = "", revLastActKey = "";
    let revReading = ${opts.reading ? `{ repo: "acme", number: 7, head_sha: ${JSON.stringify(opts.at || "")} }` : "null"};
    let revQueue = { prs: ${JSON.stringify(opts.prs || [])} };
    let revOpen = new Set(), revSums = new Map(), revCommonChips = new Set();
    // A reading in flight is state of its own (SKEIN-333); the row's gist and its "updated" mark
    // both consult it, so a world that lifts either needs one even when nothing here fills it.
    let revInFlight = new Map();
    let revUpdated = new Set();
    const revFlows = new Map();
    // Keyed repo#number#sha (SKEIN-254): the diff is filed under the COMMIT it is a diff of, and
    // the reading view asks for the commit it opened, so a moved head simply misses.
    const revDiffs = new Map(${JSON.stringify(
      opts.at ? [["acme#7#" + opts.at, { head_sha: opts.at, diff: "+x" }]] : [])});
    const revNotes = new Map();
    let renders = 0;
    const renderReview = () => { renders++; };
    const renderReviewNow = () => { renders++; };
    let reloads = 0;
    const loadReview = () => { reloads++; };
    const revBody = () => "";
    const toggleRevRow = () => {};
    const toasts = [];
    const toast = said => toasts.push(said);
    ${grab("esc")}
    ${grab("rk")}
    ${grab("revDiffKey")}
    ${grab("revReadingKey")}
    ${grab("revDiffRead")}
    ${grab("REV_UNDO_MS")}
    ${grab("revPending")}
    ${grab("revDecided")}
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revNotesSave")}
    ${grab("revNotesClear")}
    ${grab("revPost")}
    ${grab("revAct")}
    ${grab("revHold")}
    ${grab("revUndo")}
    ${grab("revTick")}
    ${grab("revFire")}
    ${grab("revRetry")}
    ${grab("revReceiptHtml")}
    ${grab("revMarkDone")}
    // revRepaintRow asks this for its data-rk selector (SKEIN-284).
    ${grab("revRkQuery")}
    ${grab("revRepaintRow")}
    ${grab("revPendingPaint")}
    ${grab("revBarHtml")}
    ${grab("revMoved")}
    ${grab("revFlowChip")}
    ${grab("REV_MOVE_WORDS")}
    ${grab("revMove")}
    ${grab("revRail")}
    ${grab("revSize")}
    ${grab("revAge")}
    ${grab("revGist")}
    ${grab("revUpdatedChip")}
    // A row says whether a review is drafted for it (SKEIN-216), so drawing one needs these.
    ${grab("revCrits")}
    ${grab("revDraftedReview")}
    // SKEIN-273: skein's review block approves WITH the review it is showing. The act is a verdict
    // and rides this file's hold, so it is proven here rather than beside the critique panel.
    ${grab("revReviewToPost")}
    ${grab("revApproveWithReview")}
    ${grab("revApproveWithReviewHtml")}
    ${grab("revDraftSection")}
    ${grab("revCritActsHtml")}
    // SKEIN-251: the age cell renders the lane's own sort key, so the row needs the order.
    ${grab("revWaitedSince")}
    ${grab("revSortAt")}
    ${grab("revSortWord")}
    ${grab("revDraftAtHead")}
    ${grab("revReadyChip")}
    // SKEIN-275: the row also states the ABSENCE of a drafted review, so revRow needs it.
    ${grab("revNoDraftWhy")}
    ${grab("revNoDraftChip")}
    // The row's own read control (SKEIN-228).
    ${grab("revReadAgain")}
    ${grab("revRow")}
    ${grab("archivePr")}
    // SKEIN-305: resolve is the one write this panel grants, and it rides the same hold, undo and
    // eight seconds as every other act — under its OWN key, because a hold replaces wholesale and
    // the row's key would make two presses cancel each other. (No backticks in this block: the
    // whole world is one template literal and one would end it mid-world.)
    ${grab("revAgo")}
    ${grab("REV_THREAD_MARK")}
    ${grab("revThreadKey")}
    ${grab("revThreadAt")}
    ${grab("revThreadHtml")}
    ${grab("revResolveThread")}
    ${grab("revThreadPaint")}
    ${grab("revThreadDone")}
    return {
      act: (r, n, k) => revAct(r, n, k),
      resolve: (r, n, id) => revResolveThread(r, n, id),
      threadKey: (r, n, id) => r + "#" + n + "#thread:" + id,
      threadHtml: (n, id) => {
        const pr = revQueue.prs.find(p => p.number === n);
        return revThreadHtml(pr, (pr.review_threads || []).find(t => t.id === id));
      },
      threadResolved: (n, id) =>
        (revQueue.prs.find(p => p.number === n).review_threads || []).find(t => t.id === id).resolved,
      archive: (r, n, on) => archivePr(r, n, on),
      undo: k => revUndo(k),
      retry: k => revRetry(k),
      pending: k => revPending.get(k),
      bar: () => revBarHtml("acme", 7),
      receipt: k => revReceiptHtml(k, revPending.get(k)),
      note: (key, path, line, body, text, sha) => { revNotesFor(key).push({ path, line, body, text, sha }); revNotesSave(key); },
      notes: key => revNotesFor(key).length,
      // SKEIN-273. "read" is the draft as the row's read-only section sees it — off the summary
      // payload, exactly where revDraftedReview looks; "vetting" is the same draft with the
      // keep/drop panel open on it, which is the copy that wins when both could answer.
      // (No backticks anywhere in this block: the whole world is one template literal.)
      read: (key, critique) => revSums.set(key, { has_critique: true, critique }),
      vetting: (key, critique, drop) =>
        revCrits.set(key, { open: true, busy: false, posting: false, critique,
                            drop: new Set(drop || []), posted: "", hold: null, said: "" }),
      approveWith: (r, n) => revApproveWithReview(r, n),
      willPost: pr => revReviewToPost(pr),
      section: pr => revDraftSection(pr),
      critActs: pr => revCritActsHtml(pr),
      prs: () => revQueue.prs,
      decided: () => [...revDecided],
      renders: () => renders,
      reloads: () => reloads,
      toasts,
    };
  `;
  const made = new Function(
    "fetch", "document", "localStorage", "revpane", "setTimeout", "clearTimeout", "encodeURIComponent", src,
  )(fetch, { getElementById: () => null }, localStorage, revpane, clk.setT, clk.clearT, encodeURIComponent);
  return { ...made, posts, advance: ms => clk.advance(ms), row: () => row.html, barHtml: () => barHtml,
           thread: () => thread.html, closeRow: () => { threadDrawn = false; },
           refuse: why => { answer = { ok: false, error: why }; }, accept: () => { answer = { ok: true, text: "approved" }; } };
}

const settle = async () => { for (let i = 0; i < 3; i++) await new Promise(r => setTimeout(r, 0)); };

const SHA = "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1";
const READING = { reading: true, at: SHA };
const PR = { number: 7, repo_id: "acme", title: "the change", author: "sam", lane: "needs-you",
             url: "https://github.com/acme/skein/pull/7", updated_at: "2026-08-20T00:00:00Z",
             my_review: "none", review_is_current: false, draft: false, reasons: ["reviewer"] };

// --- pressing approve is a receipt, not a request ----------------------------------------------
{
  const w = world(READING);
  w.act("acme", 7, "approve");
  await settle();
  t.check("pressing approve posts nothing", w.posts.length, 0);
  t.check("the act is held, waiting", (w.pending("acme#7") || {}).state, "waiting");
  const bar = w.bar();
  t.check("the bar collapsed to the receipt where the eye is",
    bar.includes("✓ approved") && bar.includes("undo (u)"), true);
  t.check("the countdown is visible and starts at the full window", bar.includes(">8s<"), true);
  t.check("no toast carried the success path", w.toasts.length, 0);
}

// --- undo inside the window cancels a request that never left ----------------------------------
{
  const w = world(READING);
  w.act("acme", 7, "approve");
  w.undo("acme#7");
  await settle();
  t.check("undo posts nothing and clears the hold", [w.posts.length, w.pending("acme#7")], [0, undefined]);
  t.check("the bar returns to its normal state", w.bar().includes("'approve'"), true);
  w.advance(20000);
  await settle();
  t.check("the cancelled timer never fires — still nothing posted", w.posts.length, 0);
}

// --- the lapse posts exactly once, with the payload the press assembled ------------------------
{
  const w = world(READING);
  w.note("acme#7", "src/lib.rs", 12, "why 1?", "let x = 1;", SHA);
  w.act("acme", 7, "approve");
  w.advance(7999);
  await settle();
  t.check("one millisecond before the window closes, nothing has posted", w.posts.length, 0);
  w.advance(1);
  await settle();
  t.check("the lapse posts exactly once, to the act route",
    w.posts.map(p => p.url), ["/api/repos/acme/review/7/act"]);
  const sent = w.posts[0].body;
  t.check("the payload is the one the press assembled — verdict, comments, drafted_at",
    [sent.kind, sent.comments.length, sent.drafted_at], ["approve", 1, SHA]);
  w.advance(60000);
  await settle();
  t.check("and once means once", w.posts.length, 1);
  t.check("posting the verdict cleared the notes it carried", w.notes("acme#7"), 0);
}

// --- a second verdict inside the window replaces the first -------------------------------------
{
  const w = world(READING);
  w.act("acme", 7, "approve");
  w.advance(3000);
  w.act("acme", 7, "request-changes");
  w.advance(20000);
  await settle();
  t.check("only the replacing verdict posts, and only once",
    w.posts.map(p => p.body.kind), ["request-changes"]);
}

// --- the countdown ticks where the receipt is --------------------------------------------------
{
  const span = { got: "", getAttribute: () => "acme#7", set textContent(v) { this.got = v; } };
  const w = world({ ...READING, spans: [span] });
  w.act("acme", 7, "approve");
  w.advance(1000);
  t.check("one second in, the receipt says 7s", span.got, "7s");
  w.advance(2000);
  t.check("three seconds in, it says 5s", span.got, "5s");
}

// --- failure wears the refusal and STAYS; try again re-fires the same payload ------------------
{
  const w = world(READING);
  w.refuse("review cannot be requested from the author");
  w.act("acme", 7, "approve");
  w.advance(8000);
  await settle();
  t.check("the refusal is held, not dropped", (w.pending("acme#7") || {}).state, "failed");
  const bar = w.bar();
  t.check("the bar wears the refusal and its way out",
    bar.includes("✗ GitHub refused: review cannot be requested from the author")
      && bar.includes("try again") && bar.includes("open on GitHub ↗"), true);
  t.check("with no queue url, the link is built from the slug",
    bar.includes("https://github.com/acme/pull/7"), true);
  t.check("a failed verdict never marks the row done", w.decided(), []);
  w.advance(120000);
  await settle();
  t.check("the refusal does not time out — it stays until acted on",
    [(w.pending("acme#7") || {}).state, w.posts.length], ["failed", 1]);
  w.accept();
  w.retry("acme#7");
  await settle();
  t.check("try again re-fires immediately, byte-identical",
    [w.posts.length, JSON.stringify(w.posts[1].body) === JSON.stringify(w.posts[0].body)], [2, true]);
  t.check("and this time it lands", (w.pending("acme#7") || {}).state, "posted");
}

// --- success marks the queue row done in place — no reload, no rebuild -------------------------
{
  const w = world({ prs: [{ ...PR }] });
  w.act("acme", 7, "approve");
  w.advance(8000);
  await settle();
  t.check("the row element itself was repainted",
    w.row().includes('data-rk="acme#7"'), true);
  t.check("and wears the done look in place — green dot, struck title",
    w.row().includes('class="revrow done"') && w.row().includes('mv done'), true);
  t.check("the pr object agrees, so any later full render agrees",
    [w.prs()[0].my_review, w.prs()[0].review_is_current], ["approved", true]);
  t.check("no loadReview was asked for", w.reloads(), 0);
  t.check("no /api/review fetch fired — the act was the only request",
    w.posts.map(p => p.url), ["/api/repos/acme/review/7/act"]);
}

// --- set aside rides the same shape: receipt, undo, one POST after the window ------------------
{
  const w = world({ prs: [{ ...PR }] });
  w.archive("acme", 7, true);
  await settle();
  t.check("set aside posts nothing on the press", w.posts.length, 0);
  const r = w.receipt("acme#7");
  t.check("the control became the receipt", r.includes("✓ set aside") && r.includes("undo (u)"), true);
  w.undo("acme#7");
  w.advance(20000);
  await settle();
  t.check("undo means it was never set aside", [w.posts.length, w.prs()[0].lane], [0, "needs-you"]);
  w.archive("acme", 7, true);
  w.advance(8000);
  await settle();
  t.check("the lapse posts the one archive request",
    [w.posts.length, w.posts[0].url, w.posts[0].body], [1, "/api/repos/acme/review/7/archive", { on: true }]);
  t.check("the row greys in place rather than the pane rebuilding",
    [w.prs()[0].lane, w.row().includes("done"), w.reloads()], ["archived", true, 0]);
}

// --- SKEIN-273: skein's review block approves WITH the review it is showing --------------------
//
// Reported live on #684. skein had read the commit and said "nothing to flag — this is pure
// composition wiring"; the owner said "in this case, there were no comments so it was good to go so
// I want to approve with comments you have shown", and there was no control that did it —
// `review::post_critique` hardcodes `Verdict::Comment` (src/review.rs:2194), so a drafted review
// could be posted as a comment and in no other way.
//
// The act is a VERDICT, so it rides this file's hold rather than the critique panel's own, and that
// is why it is proven here: `revPending` is what `u` reaches (index.html:5324), what marks the row
// approved in place, and what makes a second press inside the window replace the first instead of
// posting one review twice.
//
// What did NOT move, asserted elsewhere so this stays about the new act: the bare queue row offers
// no verdict (review_return.mjs, "no verdict is reachable from a row that is not showing the diff")
// and `a` outside the reading view still refuses out loud (reviewkeys.mjs:616).
const FLAG_NOTHING = { number: 7, head_sha: SHA, truncated: false,
                       overall: "nothing to flag — this is pure composition wiring", comments: [] };
const DRAFT = { number: 7, head_sha: SHA, truncated: false, overall: "the lock is taken twice", comments: [
  { path: "src/seam.rs", line: 46, anchored: true, text: "membership() is cached per-request",
    line_text: "let tenant = req.session();" },
  // No such line in the diff, so it cannot sit on one — the panel's own "will travel in the review
  // body" tag, kept as a promise by folding it into the body.
  { path: "src/gone.rs", line: 3, anchored: false, text: "this file is not in the diff", line_text: "" },
  { path: "src/nit.rs", line: 9, anchored: true, text: "a nit you dropped", line_text: "let z = 0;" },
]};

{
  const w = world({ prs: [{ ...PR, head_sha: SHA }] });
  w.read("acme#7", FLAG_NOTHING);
  const pr = w.prs()[0];
  const offered = w.section(pr);
  t.check("skein's review block offers the approval its own words would make",
    offered.includes(">approve with this review</button>"), true);
  t.check("and says, beside the control, what pressing it puts on GitHub",
    offered.includes("approve posts skein's note above as the approval"), true);
  t.check("naming the commit that reading was of",
    offered.includes("the reading is of <code>aaaa111</code>"), true);
  t.check("with the exact body it will send on the control itself",
    offered.includes("posts exactly this as the approval:")
      && offered.includes("nothing to flag — this is pure composition wiring"), true);

  w.approveWith("acme", 7);
  await settle();
  t.check("pressing it sends nothing — the eight seconds are the reader's", w.posts.length, 0);
  t.check("the act is held as the verdict it is, on the pull request's own hold",
    [(w.pending("acme#7") || {}).state, (w.pending("acme#7") || {}).kind], ["waiting", "approve"]);
  const held = w.section(pr);
  t.check("and the block that offered it wears the receipt, in place",
    held.includes("✓ approved") && held.includes("undo (u)") && held.includes(">8s<"), true);
  t.check("with the control gone, so a second press cannot double-post",
    held.includes("approve with this review"), false);
  t.check("the press repainted the row itself, and asked for no render that could be deferred",
    [w.row().includes('data-rk="acme#7"'), w.renders()], [true, 0]);

  w.advance(7999);
  await settle();
  t.check("a millisecond before the window closes, GitHub still knows nothing", w.posts.length, 0);
  w.advance(1);
  await settle();
  t.check("the lapse posts exactly once, to the act route",
    w.posts.map(p => p.url), ["/api/repos/acme/review/7/act"]);
  const sent = w.posts[0].body;
  t.check("as an approval — the verdict the pane could not reach before", sent.kind, "approve");
  t.check("carrying skein's own sentence as the approval body",
    sent.body.startsWith("nothing to flag — this is pure composition wiring"), true);
  // The owner's decision, 2026-08-25 (SKEIN-285): "It should be as if I am writing it." The body
  // used to end with a trailer naming skein and the commit it read. The approval goes out under
  // their account, to their colleague, and it reads as theirs — so the body is the review's own
  // words and stops there.
  t.check("and nothing after it — the approval reads as the reader's own",
    sent.body, "nothing to flag — this is pure composition wiring");
  t.check("against the commit skein read", sent.drafted_at, SHA);
  t.check("and the queue row is approved in place, without a reload",
    [w.prs()[0].my_review, w.prs()[0].review_is_current, w.reloads()], ["approved", true, 0]);
}

{
  const w = world({ prs: [{ ...PR, head_sha: SHA }] });
  w.read("acme#7", FLAG_NOTHING);
  w.approveWith("acme", 7);
  w.undo("acme#7");
  w.advance(20000);
  await settle();
  t.check("undo inside the window means GitHub never hears of it",
    [w.posts.length, w.pending("acme#7")], [0, undefined]);
  t.check("and skein's review block offers the approval again",
    w.section(w.prs()[0]).includes("approve with this review"), true);
}

{
  const w = world({ prs: [{ ...PR, head_sha: SHA }] });
  w.vetting("acme#7", DRAFT, [2]);          // the nit dropped in the panel, the other two kept
  const pr = w.prs()[0];
  const acts = w.critActs(pr);
  t.check("the vetting panel offers the approval beside posting the same review as a comment",
    [acts.includes(">approve with this review</button>"), acts.includes("as one review")], [true, true]);
  t.check("and says how many line comments ride with the verdict",
    acts.includes("carrying 1 line comment,"), true);

  w.approveWith("acme", 7);
  const after = w.critActs(pr);
  t.check("a held verdict takes BOTH controls away — one review must not post twice",
    [after.includes("approve with this review"), after.includes("as one review")], [false, false]);
  t.check("and the panel wears the verdict's receipt where the press was",
    after.includes("✓ approved") && after.includes("undo (u)"), true);

  w.advance(8000);
  await settle();
  const sent = w.posts[0].body;
  t.check("the kept anchored comment travels as a line comment, anchored by its line's own text",
    sent.comments,
    [{ path: "src/seam.rs", line: 46, body: "membership() is cached per-request", text: "let tenant = req.session();" }]);
  t.check("the dropped one travels nowhere",
    JSON.stringify(sent).includes("a nit you dropped"), false);
  t.check("and the unanchored one folds into the body under its file, as review::assemble_post does",
    sent.body.includes("**src/gone.rs**: this file is not in the diff"), true);
}

{
  const w = world({ prs: [{ ...PR, head_sha: SHA }] });
  w.read("acme#7", FLAG_NOTHING);
  w.note("acme#7", "src/lib.rs", 12, "why 1?", "let x = 1;", SHA);
  t.check("a line comment you wrote yourself is not swept into skein's approval unannounced",
    w.section(w.prs()[0]).includes("Your 1 line comment stays waiting"), true);
  w.approveWith("acme", 7);
  w.advance(8000);
  await settle();
  t.check("and it is not in what posted", w.posts[0].body.comments, []);
  t.check("nor taken away from you — the reading still owes it a verdict of yours",
    w.notes("acme#7"), 1);
}

{
  const w = world({ prs: [{ ...PR, head_sha: SHA }] });
  // Everything dropped and nothing written: there is no review left to approve WITH, and a bare
  // approval is what the reading view's `approve` is for.
  w.vetting("acme#7", { ...DRAFT, overall: "   " }, [0, 1, 2]);
  w.approveWith("acme", 7);
  await settle();
  t.check("an approval with nothing left in it is refused, not signed by skein alone",
    [w.posts.length, w.pending("acme#7")], [0, undefined]);
  t.check("and says why, where the press was",
    w.toasts.some(s => /nothing kept/.test(s)), true);
}

// ---- SKEIN-305: resolve is the one write, on its own key, with a real undo ----
//
// The route (`POST …/review/:number/thread`) sends `unresolveReviewThread` for `resolved: false`, so
// firing and retracting would both be genuine. It is still HELD, like every other act here: inside
// the window nothing has left the machine, which is strictly better than two mutations.
const THREADED = (over = {}) => ({
  ...PR, head_sha: SHA, reasons: ["author"], review_threads_total: 2,
  review_threads: [
    { id: "PRRT_open", resolved: false, outdated: false, author: "dana",
      started_at: "2026-08-20T00:00:00Z", url: "https://github.com/acme/skein/pull/7#discussion_r1" },
    { id: "PRRT_two", resolved: false, outdated: false, author: "sam",
      started_at: "2026-08-20T01:00:00Z", url: "https://github.com/acme/skein/pull/7#discussion_r2" },
  ],
  ...over,
});
{
  const w = world({ prs: [THREADED()] });
  const key = w.threadKey("acme", 7, "PRRT_open");
  w.resolve("acme", 7, "PRRT_open");
  t.check("nothing has left the machine inside the window", w.posts.length, 0);
  t.check("and the receipt is on the thread's own line, where the press was",
    [w.thread().includes("thread resolved"), w.thread().includes("undo (u)")], [true, true]);
  // THE KEY. `revPending` is keyed by rk(pr) and `revHold` replaces wholesale, so a thread press
  // filed under the row's key would mean the second resolve silently dropping the first — and a
  // resolve and an approve cancelling each other.
  w.resolve("acme", 7, "PRRT_two");
  t.check("a second thread's press does not clobber the first",
    [!!w.pending(key), !!w.pending(w.threadKey("acme", 7, "PRRT_two"))], [true, true]);

  w.advance(8000);
  await settle();
  const post = w.posts.find(p => /\/thread$/.test(p.url));
  t.check("the press reaches the route the server registered",
    [post.url, post.body], ["/api/repos/acme/review/7/thread", { thread_id: "PRRT_open", resolved: true }]);
  t.check("the thread is marked resolved on the object the pane draws from",
    w.threadResolved(7, "PRRT_open"), true);
  // The row repaints, not the pane: the unresolved count, the "N threads unresolved" on the
  // collapsed line and whether the row is your move at all are answers that just changed.
  t.check("and the row is repainted in place", w.row().includes('data-rk="acme#7"'), true);
  t.check("resolving decides no verdict on the pull request",
    [w.prs()[0].my_review, w.prs()[0].review_is_current], ["none", false]);
}
{
  // The other half of the same key rule: a resolve and a VERDICT on one pull request are two acts,
  // and under the row's key each would have cancelled the other.
  const w = world({ prs: [THREADED()] });
  const key = w.threadKey("acme", 7, "PRRT_open");
  w.resolve("acme", 7, "PRRT_open");
  w.act("acme", 7, "approve");
  t.check("a verdict on the same pull request does not cancel a held resolve",
    [!!w.pending(key), !!w.pending("acme#7")], [true, true]);
  w.advance(8000);
  await settle();
  t.check("and both land, each on its own route",
    w.posts.map(p => p.url).sort(),
    ["/api/repos/acme/review/7/act", "/api/repos/acme/review/7/thread"]);
}
{
  const w = world({ prs: [THREADED()] });
  const key = w.threadKey("acme", 7, "PRRT_open");
  w.resolve("acme", 7, "PRRT_open");
  w.undo(key);
  w.advance(8000);
  await settle();
  t.check("undo inside the window means GitHub never hears of the resolve",
    [w.posts.length, w.pending(key)], [0, undefined]);
  t.check("and the thread is still open", w.threadResolved(7, "PRRT_open"), false);
  t.check("with its resolve offered again",
    w.threadHtml(7, "PRRT_open").includes("revResolveThread"), true);
}
{
  const w = world({ prs: [THREADED()] });
  w.refuse("Could not resolve review thread: Resource not accessible by integration");
  w.resolve("acme", 7, "PRRT_open");
  w.advance(8000);
  await settle();
  t.check("a refusal STAYS on the line it was pressed on, with the way out",
    [w.thread().includes("GitHub refused"), w.thread().includes("not accessible"),
     w.thread().includes("try again")], [true, true, true]);
  t.check("and the thread is not marked resolved on a request that failed",
    w.threadResolved(7, "PRRT_open"), false);
}
{
  // The window lapses with the row closed under it. Nothing on screen to repaint, and the pane must
  // NOT be rebuilt for a receipt nobody is looking at — that is a caret out of somebody's composer.
  const w = world({ prs: [THREADED()] });
  w.resolve("acme", 7, "PRRT_open");
  w.closeRow();
  w.advance(8000);
  await settle();
  t.check("a resolve whose line has left the screen still posts", w.posts.length, 1);
  t.check("and does not rebuild the pane to say so", w.renders(), 0);
}

t.done();
