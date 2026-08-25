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
import { grab, harness } from "./lift.mjs";

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
  const revpane = {
    querySelector: sel => sel === ".readbar" ? bar : sel.startsWith(".revrow") ? row : null,
    querySelectorAll: sel => sel === "[data-undo-left]" ? spans : [],
  };
  const store = new Map();
  const localStorage = {
    getItem: k => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, v),
    removeItem: k => store.delete(k),
  };
  const src = `
    let revComposing = null;
    // SKEIN-159's keyboard state, referenced by revRow (sel/flash/held) and revHold (last act).
    let revSel = null, revFlash = "", revLastActKey = "";
    let revReading = ${opts.reading ? `{ repo: "acme", number: 7 }` : "null"};
    let revQueue = { prs: ${JSON.stringify(opts.prs || [])} };
    let revOpen = new Set(), revSums = new Map(), revCommonChips = new Set();
    const revFlows = new Map();
    const revDiffs = new Map(Object.entries(${JSON.stringify(opts.diffs || {})}));
    const revNotes = new Map();
    let renders = 0;
    const renderReview = () => { renders++; };
    let reloads = 0;
    const loadReview = () => { reloads++; };
    const revBody = () => "";
    const toggleRevRow = () => {};
    const toasts = [];
    const toast = said => toasts.push(said);
    ${grab("esc")}
    ${grab("rk")}
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
    // A row says whether a review is drafted for it (SKEIN-216), so drawing one needs these.
    ${grab("revCrits")}
    ${grab("revDraftedReview")}
    ${grab("revReadyChip")}
    // The row's own read control (SKEIN-228).
    ${grab("revReadAgain")}
    ${grab("revRow")}
    ${grab("archivePr")}
    return {
      act: (r, n, k) => revAct(r, n, k),
      archive: (r, n, on) => archivePr(r, n, on),
      undo: k => revUndo(k),
      retry: k => revRetry(k),
      pending: k => revPending.get(k),
      bar: () => revBarHtml("acme", 7),
      receipt: k => revReceiptHtml(k, revPending.get(k)),
      note: (key, path, line, body, text, sha) => { revNotesFor(key).push({ path, line, body, text, sha }); revNotesSave(key); },
      notes: key => revNotesFor(key).length,
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
           refuse: why => { answer = { ok: false, error: why }; }, accept: () => { answer = { ok: true, text: "approved" }; } };
}

const settle = async () => { for (let i = 0; i < 3; i++) await new Promise(r => setTimeout(r, 0)); };

const SHA = "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1";
const READING = { reading: true, diffs: { "acme#7": { head_sha: SHA, diff: "+x" } } };
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

t.done();
