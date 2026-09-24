// SKEIN-162: a verdict gives feedback where the eye is, and can be taken back.
//
// The measured failure this exists to keep dead: press approve → a toast lands ~900px from the
// bar you pressed → loadReview(true) → 2-4s of network → the whole pane rebuilds → the row still
// reads "you have not reviewed this". No undo, and a refusal was a toast that disappeared.
//
// The contracts, against the real functions lifted out of index.html (docs/parity.md §3):
//   * pressing a verdict posts NOTHING — the bar collapses in place to `✓ approved · undo · 8s`;
//   * undo inside the window cancels a request that never left the machine;
//   * the window's lapse posts exactly once, with the payload assembled at press time;
//   * a second verdict inside the window replaces the first — only the second posts;
//   * a refusal turns the bar to `✗ GitHub refused: … · try again · open on GitHub ↗` and STAYS,
//     and try-again re-fires the same payload;
//   * success marks the queue row done IN PLACE — no loadReview, no /api/review fetch;
//   * set aside (archivePr) rides the same shape: receipt, undo, one POST after the window.
//
// Timers are a driven clock, not sleeps: the suite advances time and watches what fires.
//
//   node tests/ui/undo.mjs
import { grab, harness, link, pure } from "./lift.mjs";

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
  const spans = opts.spans || [];
  // One open review thread's line, captured the same way (SKEIN-305). It is queried by
  // `data-thread` and never by `.revrow` — a pull request inside a stack is drawn as a `.step`, so
  // a selector reaching for the row around it finds nothing and the press dies silently.
  const thread = { html: "" };
  Object.defineProperty(thread, "outerHTML", { set(v) { thread.html = v; }, get() { return thread.html; } });
  let threadDrawn = true;   // is that line on screen? (a closed row is the case where it is not)
  const revpane = {
    querySelector: sel => sel.startsWith("[data-thread=") ? (threadDrawn ? thread : null)
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
    let revQueue = { prs: ${JSON.stringify(opts.prs || [])} };
    // Merge is the one act that asks first, and it is the only one that names a revision.
    const confirmed = () => true;
    let revOpen = new Set(), revSums = new Map(), revCommonChips = new Set();
    // A reading in flight is state of its own (SKEIN-333); the row's gist and its "updated" mark
    // both consult it, so a world that lifts either needs one even when nothing here fills it.
    let revInFlight = new Map();
    let revUpdated = new Set();
    const revFlows = new Map();
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
    ${grab("REV_UNDO_MS")}
    ${grab("revPending")}
    ${grab("revDecided")}
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
    ${grab("revKeyPr")}
    // The verdicts, on the row's own control strip — where the receipt replaces them.
    ${grab("revVerdictHtml")}
    ${grab("revMoved")}
    ${grab("revFlowChip")}
    ${grab("REV_MOVE_WORDS")}
    ${grab("revMove")}
    ${grab("revRail")}
    ${grab("revSize")}
    ${grab("revAge")}
    ${grab("revGist")}
    ${grab("revUpdatedChip")}
    // The mark a refused act leaves on the line (SKEIN-385): revRow calls it, so a world that
    // draws a row needs it, and its label map with it.
    ${grab("REV_ACT_NAME")}
    ${grab("revRefusedChip")}
    // SKEIN-251: the age cell renders the lane's own sort key, so the row needs the order.
    ${grab("revWaitedSince")}
    ${grab("revSortAt")}
    ${grab("revSortWord")}
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
      receipt: k => revReceiptHtml(k, revPending.get(k)),
      verdicts: () => revVerdictHtml(revQueue.prs[0] || { repo_id: "acme", number: 7 }),
      prs: () => revQueue.prs,
      decided: () => [...revDecided],
      renders: () => renders,
      reloads: () => reloads,
      toasts,
    };
  `;
  const made = new Function(
    "fetch", "document", "localStorage", "revpane", "setTimeout", "clearTimeout", "encodeURIComponent", "link", src,
  )(fetch, { getElementById: () => null }, localStorage, revpane, clk.setT, clk.clearT, encodeURIComponent, link);
  return { ...made, posts, advance: ms => clk.advance(ms), row: () => row.html,
           thread: () => thread.html, closeRow: () => { threadDrawn = false; },
           refuse: why => { answer = { ok: false, error: why }; }, accept: () => { answer = { ok: true, text: "approved" }; } };
}

const settle = async () => { for (let i = 0; i < 3; i++) await new Promise(r => setTimeout(r, 0)); };

const SHA = "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1";
const PR = { number: 7, repo_id: "acme", title: "the change", author: "sam", lane: "needs-you",
             url: "https://github.com/acme/skein/pull/7", updated_at: "2026-08-20T00:00:00Z",
             head_sha: SHA, my_review: "none", review_is_current: false, draft: false,
             reasons: ["reviewer"] };
// A queue with the one row every act below is about. The row IS the surface now: its control strip
// carries the verdicts, and the receipt replaces them in place.
const ONE = { prs: [{ ...PR }] };

// --- pressing approve is a receipt, not a request ----------------------------------------------
{
  const w = world({ prs: [{ ...PR }] });
  w.act("acme", 7, "approve");
  await settle();
  t.check("pressing approve posts nothing", w.posts.length, 0);
  t.check("the act is held, waiting", (w.pending("acme#7") || {}).state, "waiting");
  const strip = w.receipt("acme#7");
  t.check("the control strip collapsed to the receipt where the eye is",
    strip.includes("✓ approved") && strip.includes("undo (u)"), true);
  t.check("the countdown is visible and starts at the full window", strip.includes(">8s<"), true);
  t.check("no toast carried the success path", w.toasts.length, 0);
}

// --- undo inside the window cancels a request that never left ----------------------------------
{
  const w = world({ prs: [{ ...PR }] });
  w.act("acme", 7, "approve");
  w.undo("acme#7");
  await settle();
  t.check("undo posts nothing and clears the hold", [w.posts.length, w.pending("acme#7")], [0, undefined]);
  t.check("the strip returns to offering the verdicts", w.verdicts().includes("'approve'"), true);
  w.advance(20000);
  await settle();
  t.check("the cancelled timer never fires — still nothing posted", w.posts.length, 0);
}

// --- the lapse posts exactly once, with the payload the press assembled ------------------------
{
  const w = world({ prs: [{ ...PR }] });
  w.act("acme", 7, "approve");
  w.advance(7999);
  await settle();
  t.check("one millisecond before the window closes, nothing has posted", w.posts.length, 0);
  w.advance(1);
  await settle();
  t.check("the lapse posts exactly once, to the act route",
    w.posts.map(p => p.url), ["/api/repos/acme/review/7/act"]);
  const sent = w.posts[0].body;
  // A verdict names no revision: it carries no line numbers, so there is nothing to anchor and
  // nothing for the server to re-anchor against. `drafted_at` is the merge's business alone.
  t.check("the payload is the one the press assembled, and it names no revision",
    [sent.kind, sent.body, sent.drafted_at], ["approve", "", ""]);
  w.advance(60000);
  await settle();
  t.check("and once means once", w.posts.length, 1);
}

// --- a merge names the commit the row shows, and posts at once ---------------------------------
//
// The one act that carries `drafted_at` (SKEIN-365): `prwork::merge_by_hand` checks the sha it is
// given against the live head, so sending "" would leave the server to guess from a queue that may
// have lagged — and the reader would be refused for not having read the code they were looking at.
// Break it by making `mergeHead` "" in `revAct` and this is the check that goes red.
{
  const w = world({ prs: [{ ...PR }] });
  w.act("acme", 7, "merge");
  await settle();
  t.check("a merge posts immediately, with the row's own head sha",
    [w.posts.length, w.posts[0].body.kind, w.posts[0].body.drafted_at], [1, "merge", SHA]);
}

// --- a second verdict inside the window replaces the first -------------------------------------
{
  const w = world({ prs: [{ ...PR }] });
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
  const w = world({ prs: [{ ...PR }], spans: [span] });
  w.act("acme", 7, "approve");
  w.advance(1000);
  t.check("one second in, the receipt says 7s", span.got, "7s");
  w.advance(2000);
  t.check("three seconds in, it says 5s", span.got, "5s");
}

// --- failure wears the refusal and STAYS; try again re-fires the same payload ------------------
{
  const w = world({ prs: [{ ...PR }] });
  w.refuse("review cannot be requested from the author");
  w.act("acme", 7, "approve");
  w.advance(8000);
  await settle();
  t.check("the refusal is held, not dropped", (w.pending("acme#7") || {}).state, "failed");
  const strip = w.receipt("acme#7");
  t.check("the strip wears the refusal and its way out",
    strip.includes("✗ GitHub refused: review cannot be requested from the author")
      && strip.includes("try again") && strip.includes("open on GitHub ↗"), true);
  t.check("the link is the one the queue row carries",
    strip.includes("https://github.com/acme/skein/pull/7"), true);
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

// Two whole sections stood here and are gone with what they guarded.
//
// **SKEIN-273, "approve WITH the review it is showing"**, was reported live on #684: skein had read
// the commit and said "nothing to flag", the owner said "there were no comments so it was good to
// go so I want to approve with comments you have shown", and no control did it. The control existed
// because skein HELD a review a reader had to decide about. It no longer holds one — the session
// posts its own review to GitHub — so there is nothing to approve *with*. Approving is now the
// plain verdict on the row, which the sections above already hold.
//
// **SKEIN-305, "resolve is the one write"**, guarded the thread panel: a resolve on the thread's own
// key so two presses inside eight seconds could not clobber each other, and a real
// `unresolveReviewThread` behind undo. The panel is gone — a thread is a conversation about a line
// of code and is worth nothing away from the line — so the write it granted went with it. What is
// left of threads on this page is `moveNote`'s "N threads unresolved", which is a fact about whose
// move it is and grants nothing.

t.done();
