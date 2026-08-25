// Line-comment notes survive a moving head (SKEIN-214, the UI half).
//
// The owner's ask: reviews must stay postable when new commits land. The server re-anchors each
// comment by its line's TEXT against the live diff; the client's whole job is to SUPPLY that text
// plus the head the notes were drafted against (`drafted_at`) — and to stop refusing.
//
// Four contracts, against the real functions lifted out of index.html:
//   * a note created from a diff line carries the line's own text (marker stripped) and the sha
//     the reading showed;
//   * the act POST body carries comments[].text and one drafted_at;
//   * a moved head does NOT stop the post — it still goes, and the reading wears an informative
//     notice instead of a gate;
//   * notes saved before `text` existed (legacy localStorage) still post: text reads as "".
//
//   node tests/ui/revnotes.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed down to what the note flow touches.
function world(opts = {}) {
  const store = new Map(Object.entries(opts.stored || {}));
  const localStorage = {
    getItem: k => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, v),
    removeItem: k => store.delete(k),
  };
  const posts = [];
  const fetch = (url, init) => {
    posts.push({ url, body: init && init.body ? JSON.parse(init.body) : null });
    return Promise.resolve({ json: () => Promise.resolve({ ok: true, text: "approved" }) });
  };
  // Just enough document for the composer: created nodes answer querySelector with stable stubs,
  // so the test can type into the textarea and press Save.
  const document = {
    createElement: () => {
      const parts = new Map();
      const part = () => ({ onclick: null, value: "", focus() {}, addEventListener() {}, click() { this.onclick && this.onclick(); } });
      return {
        className: "", innerHTML: "",
        querySelector: sel => { if (!parts.has(sel)) parts.set(sel, part()); return parts.get(sel); },
        remove() {},
      };
    },
  };
  const src = `
    let revComposing = null;
    // The reading carries the commit it opened (SKEIN-254): revDiffs is keyed by the COMMIT the
    // diff is of, so a note's sha comes from the diff on screen and not from a per-PR slot that
    // never changes.
    let revReading = { repo: "acme", number: 7, head_sha: ${JSON.stringify(opts.at || "")} };
    let revQueue = { prs: [] };
    const revpane = null;
    // Keyed repo#number#sha (SKEIN-254). The option "at" is the commit the reading view opened,
    // and the diff files itself under its own head, so the two agree by construction, not by hand.
    const revDiffs = new Map(${JSON.stringify(
      opts.at ? [["acme#7#" + opts.at, { head_sha: opts.at, ...(opts.diff || {}) }]] : [])});
    const revNotes = new Map();
    const renderReview = () => {};
    const renderReviewNow = () => {};
    const closeReading = () => {};
    const loadReview = () => {};
    const revRow = () => "";
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
    ${grab("revNoteAdd")}
    ${grab("openReadingComposer")}
    ${grab("revPost")}
    ${grab("revAct")}
    ${grab("revHold")}
    ${grab("revUndo")}
    ${grab("revTick")}
    ${grab("revFire")}
    ${grab("revReceiptHtml")}
    ${grab("revMarkDone")}
    // revRepaintRow asks this for its data-rk selector (SKEIN-284).
    ${grab("revRkQuery")}
    ${grab("revRepaintRow")}
    // The pending paint routes a THREAD key to its own paint (SKEIN-305), so it needs the marker
    // that tells the two kinds of key apart. (No backticks: this whole world is a template literal.)
    ${grab("REV_THREAD_MARK")}
    ${grab("revThreadAt")}
    ${grab("revThreadPaint")}
    ${grab("revPendingPaint")}
    ${grab("revMovedNotice")}
    return {
      notesFor: k => revNotesFor(k),
      noteAdd: (k, p, l, b, x, s) => revNoteAdd(k, p, l, b, x, s),
      compose: ln => openReadingComposer(ln),
      act: (r, n, k) => revAct(r, n, k),
      movedNotice: (d, notes, head) => revMovedNotice(d, notes, head),
      toasts,
    };
  `;
  // SKEIN-162 holds every verdict for an undo window before it posts. These contracts are about
  // WHAT posts, not when — an immediate timer collapses the window so the payload assertions
  // read exactly as they did before the hold existed.
  const made = new Function("fetch", "document", "localStorage", "setTimeout", "clearTimeout", src)(
    fetch, document, localStorage, fn => { fn(); return 0; }, () => {});
  return { ...made, posts, store };
}

const settle = async () => { for (let i = 0; i < 3; i++) await new Promise(r => setTimeout(r, 0)); };

// --- a note born on a diff line carries the line's text and the reading's sha -------------------
{
  const w = world({ at: "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1" });
  const ln = { dataset: { file: "src/lib.rs", line: "12" }, textContent: "+    let x = 1;", after() {} };
  w.compose(ln);
  t.check("composing alone saves nothing — Save does", w.notesFor("acme#7").length, 0);
  w.noteAdd("acme#7", "src/lib.rs", "12", "why 1?", "    let x = 1;", "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1");
  t.check("a stored note carries body, anchor text and sha",
    w.notesFor("acme#7")[0],
    { path: "src/lib.rs", line: 12, body: "why 1?", text: "    let x = 1;", sha: "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1" });
}

// --- the composer's Save captures text + sha from the line and the reading ----------------------
{
  const boxes = [];
  const store = new Map();
  const localStorage = { getItem: k => store.get(k) ?? null, setItem: (k, v) => store.set(k, v), removeItem: k => store.delete(k) };
  const document = {
    createElement: () => {
      const parts = new Map();
      const part = () => ({ onclick: null, value: "", focus() {}, addEventListener() {} });
      const box = {
        className: "", innerHTML: "",
        querySelector: sel => { if (!parts.has(sel)) parts.set(sel, part()); return parts.get(sel); },
        remove() {},
      };
      boxes.push(box);
      return box;
    },
  };
  const src = `
    let revReading = { repo: "acme", number: 7, head_sha: "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1" };
    const revDiffs = new Map([["acme#7#aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1",
                              { head_sha: "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1" }]]);
    const revNotes = new Map();
    const renderReview = () => {};
    const renderReviewNow = () => {};
    ${grab("esc")}
    ${grab("revDiffKey")}
    ${grab("revReadingKey")}
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revNotesSave")}
    ${grab("revNoteAdd")}
    ${grab("openReadingComposer")}
    return { compose: ln => openReadingComposer(ln), notesFor: k => revNotesFor(k) };
  `;
  const made = new Function("document", "localStorage", src)(document, localStorage);
  const ln = { dataset: { file: "src/lib.rs", line: "12" }, textContent: "+    let x = 1;", after() {} };
  made.compose(ln);
  const box = boxes[0];
  box.querySelector("textarea").value = "why 1?";
  box.querySelector(".save").onclick();
  t.check("Save stores the note with the line's text (marker stripped) and the reading's sha",
    made.notesFor("acme#7"),
    [{ path: "src/lib.rs", line: 12, body: "why 1?", text: "    let x = 1;", sha: "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1" }]);
}

// --- the act POST carries comments[].text and one drafted_at ------------------------------------
{
  const sha = "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1";
  const w = world({ at: sha });
  w.noteAdd("acme#7", "src/lib.rs", "12", "why 1?", "    let x = 1;", sha);
  w.act("acme", 7, "approve");
  await settle();
  t.check("the act POST went to the act route", w.posts.map(p => p.url), ["/api/repos/acme/review/7/act"]);
  const sent = (w.posts[0] || {}).body || {};
  t.check("comments carry path/line/body AND the anchor text",
    sent.comments,
    [{ path: "src/lib.rs", line: 12, body: "why 1?", text: "    let x = 1;" }]);
  t.check("one drafted_at names the head the notes were drafted against", sent.drafted_at, sha);
}

// --- a moved head still posts, and the notice informs instead of gating -------------------------
{
  const drafted = "aaaa111aaaa111aaaa111aaaa111aaaa111aaaa1";
  const live = "bbbb222bbbb222bbbb222bbbb222bbbb222bbbb2";
  const w = world({ at: drafted });
  w.noteAdd("acme#7", "src/lib.rs", "12", "why 1?", "    let x = 1;", drafted);
  // The queue has since learned the branch moved; nothing about that may stop the post.
  w.act("acme", 7, "request-changes");
  await settle();
  t.check("a moved head does not stop the post — the request still goes", w.posts.length, 1);
  t.check("the moved post still says which head it was drafted against", ((w.posts[0] || {}).body || {}).drafted_at, drafted);
  const notice = w.movedNotice({ head_sha: drafted }, [{ sha: drafted }], live).replace(/\s+/g, " ");
  t.check("the reading wears the informative sentence, not a gate",
    notice.includes("the branch moved since you read") && notice.includes("matching lines will carry your comments")
      && notice.includes("aaaa111"),
    true);
  t.check("an unmoved head wears nothing", w.movedNotice({ head_sha: live }, [{ sha: live }], live), "");
}

// --- legacy notes without text still parse and still post ---------------------------------------
{
  const sha = "cccc333cccc333cccc333cccc333cccc333cccc3";
  const w = world({
    stored: { "skein.revnotes.acme#7": JSON.stringify([{ path: "src/old.rs", line: 3, body: "old note" }]) },
    at: sha,
  });
  t.check("a pre-text note parses", w.notesFor("acme#7").length, 1);
  w.act("acme", 7, "comment");
  await settle();
  const sent = (w.posts[0] || {}).body || {};
  t.check("a pre-text note posts with text as the empty string — displaced server-side, never a crash",
    sent.comments,
    [{ path: "src/old.rs", line: 3, body: "old note", text: "" }]);
  t.check("drafted_at falls back to the reading's own sha when no note carries one",
    sent.drafted_at, sha);
}

t.done();
