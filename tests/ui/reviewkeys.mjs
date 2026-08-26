// SKEIN-151/159: the review queue answers the keyboard, and focus survives the queue redrawing
// itself (docs/review-ux.md §6).
//
// Two halves, matching the two items:
//
//   * ROUTING — `shortcutFor`'s REVIEW table, imported from cockpit/src/keys.mjs and driven the
//     way the page drives it: review keys resolve to review actions, fleet keys resolve to
//     NOTHING while the pane is the surface (SKEIN-151's done-when: no keypress in the pane may
//     change the fleet selection behind it), ⌘K/⌘N/? pass, the g chords compose, and `m` does
//     not exist.
//
//   * FOCUS — the real `renderReview`/`revKey*` functions lifted out of index.html, against a DOM
//     stub that models the one browser behaviour this is all about: replacing `revpane.innerHTML`
//     KILLS a focused child — focus falls to BODY and the caret dies. §6's one-line assertion,
//     verbatim: type into the composer, fire renderReview(), and `document.activeElement.id` must
//     still be `rev-compose` with `selectionStart` unchanged. Plus: selection is a PR NUMBER that
//     survives a re-sort and a summary landing, expansion is exclusive, `e` rides the hold/undo
//     receipt and `u` takes it back, `a` in the queue refuses out loud, g1…g9 follow the picker's
//     order.
//
//   node tests/ui/reviewkeys.mjs
import { draftRules, grab, harness, pure } from "./lift.mjs";
import { shortcutFor, ACTIONS } from "../../cockpit/src/keys.mjs";

const t = harness();
const REV = { pane: "review" };
const key = (k, extra = {}) => ({ key: k, ...extra });

// ---- routing: one table, one guard, and nothing reaches the fleet map behind the pane ---------

{
  t.check("j is the queue's next-row, not the fleet's", shortcutFor(key("j"), REV), "rev-next");
  t.check("Enter opens the reading view, not a terminal", shortcutFor(key("Enter"), REV), "rev-open");
  t.check("o opens things HERE — GitHub is g h", shortcutFor(key("o"), REV), "rev-open");
  t.check("e is set-aside", shortcutFor(key("e"), REV), "rev-aside");
  t.check("u is undo", shortcutFor(key("u"), REV), "rev-undo");
  t.check("/ is the queue's own search", shortcutFor(key("/"), REV), "rev-search");
  t.check("n walks the undecided", shortcutFor(key("n"), REV), "rev-next-undecided");
  t.check("→ enters a stack", shortcutFor(key("ArrowRight"), REV), "rev-into");

  // SKEIN-151's done-when, at the deciding layer: every key the FLEET binds resolves to no fleet
  // action while the review pane is the surface. `?` is the one deliberate pass-through.
  const fleetActs = ["deselect", "next", "previous", "open", "diff", "filter",
                     "next-needs-you", "load", "previous-session", "next-session"];
  const leaks = [];
  for (const k of ["Escape", "j", "ArrowDown", "k", "ArrowUp", "Enter", "o", "d", "/", "]",
                   "L", "l", "[", "}", "?"]) {
    const got = shortcutFor(key(k), REV);
    if (fleetActs.includes(got)) leaks.push(`${k}→${got}`);
  }
  t.check("no key in the pane resolves to a fleet action", leaks, []);
  t.check("keys the fleet binds and review does not are shadowed, not passed",
    [shortcutFor(key("d"), REV), shortcutFor(key("l"), REV), shortcutFor(key("}"), REV)],
    [null, null, null]);
  t.check("? still reaches the key sheet", shortcutFor(key("?"), REV), "keys");
  t.check("⌘K and ⌘N pass, as everywhere",
    [shortcutFor(key("k", { metaKey: true }), REV), shortcutFor(key("n", { ctrlKey: true }), REV)],
    ["palette", "new-box"]);
  t.check("the typing guard holds in the pane too",
    ["j", "e", "a", "u"].map(k => shortcutFor(key(k), { ...REV, inField: true })),
    [null, null, null, null]);

  // The g chords: the second key is decided by the table, the page only remembers "g was pressed".
  t.check("g opens a chord", shortcutFor(key("g"), REV), "rev-chord");
  t.check("g3 is the third repo", shortcutFor(key("3"), { ...REV, pending: "g" }), "rev-repo-3");
  t.check("gr is the repo picker", shortcutFor(key("r"), { ...REV, pending: "g" }), "rev-repo-menu");
  t.check("gh is GitHub", shortcutFor(key("h"), { ...REV, pending: "g" }), "rev-github");
  t.check("a fumbled chord is a lost keystroke, never an act",
    [shortcutFor(key("q"), { ...REV, pending: "g" }), shortcutFor(key("j"), { ...REV, pending: "g" })],
    [null, null]);

  // The absence that matters most: merge is UNBOUND — the one act this pane cannot undo, and one
  // letter must not land a commit on a base branch (§6, "three deliberate absences").
  t.check("m goes nowhere", shortcutFor(key("m"), REV), null);
  t.check("m goes nowhere even as a chord", shortcutFor(key("m"), { ...REV, pending: "g" }), null);
  t.check("no action smells of merge", ACTIONS.filter(a => /merge/.test(a)), []);
}

// ---- the page's half: renderReview + revKey*, real, against a focus-modelling DOM -------------

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

function world(opts = {}) {
  const posts = [];
  const fetch = (url, init) => {
    if (init && init.method === "POST") posts.push({ url, body: init.body ? JSON.parse(init.body) : null });
    if (/\/diff$/.test(url)) {
      return Promise.resolve({ json: () => Promise.resolve({ head_sha: "s", diff: "+x", cut: false }) });
    }
    return Promise.resolve({ ok: true, json: () => Promise.resolve({}), text: () => Promise.resolve("{}") });
  };
  const clk = clock();

  // The DOM, stubbed down to the one behaviour under test: innerHTML replacement kills a focused
  // child. `contains` says what is inside the pane; the setter enforces the browser's rule.
  const bodyEl = { tagName: "BODY", id: "" };
  const composer = { tagName: "TEXTAREA", id: "rev-compose", value: "", selectionStart: 0,
                     focus() { dom.active = composer; } };
  const search = { tagName: "INPUT", id: "", className: "revsearch", value: "",
                   focus() { dom.active = search; } };
  const dom = { active: bodyEl, renders: 0 };
  const revpane = {
    _html: "",
    scrollTop: 0,
    contains: el => el === composer || el === search,
    set innerHTML(v) {
      this._html = v;
      dom.renders++;
      // What a real browser does to a focused child of a replaced subtree.
      if (revpane.contains(dom.active)) {
        if (dom.active.selectionStart != null) dom.active.selectionStart = 0;
        dom.active = bodyEl;
      }
    },
    get innerHTML() { return this._html; },
  };
  const selection = { isCollapsed: true, anchorNode: null };
  const windowStub = { getSelection: () => selection, opened: [], open(url) { this.opened.push(url); } };
  const documentStub = {
    get activeElement() { return dom.active; },
    body: { classList: { remove() {}, toggle() {} } },
    getElementById: id => (id === "rev-compose" ? composer : null),
  };
  const localStorage = { store: {}, getItem(k) { return this.store[k] ?? null; }, setItem(k, v) { this.store[k] = v; }, removeItem(k) { delete this.store[k]; } };

  const body = `
    // Whose move a pull request is, from cockpit/src/move.mjs — the one place the rule lives
    // (SKEIN-302), so a world that stubbed it would be testing a second copy of it.
    ${pure("move")}
    const repos = ${JSON.stringify((opts.repos || ["alpha", "beta", "gamma"]).map(id => ({ id })))};
    let view = { box: null, mode: "review", kind: "agent" };
    const applyView = () => {};
    const loadReview = () => { renderReview(); };   // the queue is injected, never fetched here
    const revFetchSummary = () => {};
    const revRenderNotes = () => {};
    const revComposeHtml = () => "";
    const revCritiqueHtml = () => "";
    // Something the pane draws OUTSIDE any row, made to throw on demand — the picker, a lane
    // heading and this are all in the one big template, and none of them is covered by the per-row
    // guard (SKEIN-268).
    let headThrows = false;
    const revModsHtml = () => { if (headThrows) throw new TypeError("boom above the rows"); return ""; };
    const revEditHtml = () => "";
    const revReadChip = () => "";
    const revModsCount = () => "";
    const revScopeRepo = () => "";
    const revAgo = () => "now";
    const revDetail = () => "";
    const revFlowBox = () => "";
    const renderRevBadge = () => {};
    const loadKnownSummaries = () => {};
    const loadWorkflows = () => {};
    const renderDiff = txt => '<div class="diff">' + txt + '</div>';
    const revBody = () => "";
    let revEdit = null, revModsOpen = false;
    let revStaleTries = 0;
    const REV_STALE_TRIES = 5;
    let revSums = new Map();
    // A reading in flight is state of its own (SKEIN-333); the row's gist and its "updated" mark
    // both consult it, so a world that lifts either needs one even when nothing here fills it.
    let revInFlight = new Map();
    let revUpdated = new Set();
    // Read-ahead ON for both fixture repos, which is what a person who wants their queue read has
    // pressed. It is the pump's SCOPE (review::unasked_scope, SKEIN-242) and therefore also what
    // revReadAgain reads to decide whether a row is about to be read anyway: an empty map here
    // would put a read control on every unread row in this suite.
    const revFlows = new Map([["alpha", { read_prs: true }], ["beta", { read_prs: true }]]);
    const revCrits = new Map();
    let revComposing = null;
    const toasts = [];
    const toast = said => toasts.push(said);
    ${grab("rk")}
    ${grab("revQueue")}
    ${grab("revRepoFilter")}
    ${grab("revFilter")}
    ${grab("revOpen")}
    ${grab("revLoading")}
    ${grab("revHeld")}
    ${grab("revSeen")}
    ${grab("revSearch")}
    ${grab("revCommonChips")}
    ${grab("revReading")}
    ${grab("revReturnScroll")}
    ${grab("revDiffs")}
    ${grab("revDiffKey")}
    ${grab("revReadingKey")}
    ${grab("revDiffRead")}
    // SKEIN-254: opening a change asks for the COMMIT it opened, and can be told to load a newer one.
    ${grab("revDiffBusy")}
    ${grab("revReadingLoad")}
    ${grab("revReloadReading")}
    ${grab("revNotes")}
    ${grab("revSel")}
    ${grab("revSelAt")}
    ${grab("revNav")}
    ${grab("revStacks")}
    ${grab("revFlash")}
    ${grab("revChord")}
    ${grab("revLastActKey")}
    ${grab("revHunkAt")}
    ${grab("revFileAt")}
    ${grab("revRenderQueued")}
    ${grab("REV_UNDO_MS")}
    ${grab("revPending")}
    ${grab("revDecided")}
    ${grab("REV_LANES")}
    ${grab("revNotReadyOpen")}
    ${grab("toggleNotReady")}
    ${grab("revNotReadyWhy")}
    // The two groups below your move both fold now (SKEIN-302), so the pane needs both switches
    // and the sentence each heading states its composition with.
    ${grab("revTheirsOpen")}
    ${grab("toggleTheirs")}
    ${grab("REV_FOLDS")}
    ${grab("revFolds")}
    ${grab("revFoldOpen")}
    ${grab("revTheirsWhy")}
    ${grab("revNavSettle")}
    ${grab("revRenderHeld")}
    ${grab("revRenderFlush")}
    ${grab("revStackKey")}
    ${grab("revStackNext")}
    ${grab("revStackLane")}
    ${grab("revStackOpenKey")}
    ${grab("revStackStep")}
    // revChains reads the repo's trunk to know where a stack stops (SKEIN-288).
    ${grab("revTrunkOf")}
    ${grab("revChains")}
    ${grab("revStackName")}
    ${grab("revMisnamed")}
    ${grab("toggleRevStack")}
    // Opening a row or a step fetches the prose the row shape left behind (SKEIN-287).
    ${grab("revLoadReading")}
    ${grab("toggleStackStep")}
    ${grab("toggleRevRow")}
    // A step's number is its depth, and it says "at least" when the bottom is out of sight.
    ${grab("REV_UNROOTED_WHY")}
    ${grab("revStepNo")}
    // What a stack's run says on its COLLAPSED row (SKEIN-370). Lifted with the row rather than
    // stubbed: the row asking for it is exactly what was missing, and a stub would hide that.
    let revStackRuns = new Map();
    ${grab("revStackRunGist")}
    ${grab("revStackRow")}
    // The stack's read control and progress (SKEIN-337) live above the steps. These suites are
    // about the step LIST, so the block is stubbed rather than lifted — conversation.mjs and
    // stackread.mjs hold what it draws.
    const revStackRunHtml = () => "";
    ${grab("revStackSteps")}
    ${grab("revWaitedSince")}
    ${grab("revReadBand")}
    ${grab("revMatchesFilter")}
    ${grab("revMatchesSearch")}
    ${grab("revSearchSet")}
    ${grab("revMoved")}
    ${grab("revFlowChip")}
    ${grab("REV_MOVE_WORDS")}
    ${grab("revMove")}
    ${grab("revRail")}
    ${grab("revSize")}
    ${grab("revAge")}
    ${grab("revGist")}
    // A row says whether a review is drafted for it (SKEIN-216).
    ${grab("revDraftedReview")}
    // SKEIN-251: the age cell renders the lane's own sort key, so the row needs the order.
    ${grab("revWaitedSince")}
    ${grab("revSortAt")}
    ${grab("revSortWord")}
    ${draftRules()}
    ${grab("revReadyChip")}
    // SKEIN-275: the row also states the ABSENCE of a drafted review, so revRow needs it.
    ${grab("revNoDraftWhy")}
    ${grab("revNoDraftChip")}
    // The row's own read control (SKEIN-228), and the two questions it asks about the pump's scope.
    ${grab("revReadsAhead")}
    ${grab("revSkeinsToRead")}
    // A reading is not a review (SKEIN-371): a step skein read and could not review must not count
    // as read, and must offer its own retry.
    ${grab("revNoReviewCameBack")}
    ${grab("revReadAgain")}
    ${grab("revUpdatedChip")}
    ${grab("revRow")}
    ${grab("revNotesStore")}
    ${grab("revNotesFor")}
    ${grab("revNotesSave")}
    ${grab("revNotesClear")}
    ${grab("revReceiptHtml")}
    ${grab("revMarkDone")}
    ${grab("revRepaintRow")}
    // The pending paint routes a THREAD key to its own paint (SKEIN-305), so it needs the marker
    // that tells the two kinds of key apart. (No backticks: this whole world is a template literal.)
    ${grab("REV_THREAD_MARK")}
    ${grab("revThreadAt")}
    ${grab("revThreadPaint")}
    ${grab("revPendingPaint")}
    ${grab("revBarHtml")}
    ${grab("revMovedNotice")}
    ${grab("readingFiles")}
    ${grab("renderReading")}
    ${grab("openReading")}
    ${grab("closeReading")}
    ${grab("openReview")}
    ${grab("renderReview")}
    // SKEIN-268: the paint is guarded, each row is guarded, and a fault is said out loud.
    ${grab("lastPageError")}
    ${grab("reportPageError")}
    ${grab("revRowBrokenHtml")}
    ${grab("revRowSafe")}
    ${grab("revRenderFailed")}
    ${grab("revRenderPane")}
    // A press's own render, which rule 2 does NOT defer (SKEIN-264).
    ${grab("renderReviewNow")}
    ${grab("revHold")}
    ${grab("revUndo")}
    ${grab("revTick")}
    ${grab("revFire")}
    ${grab("revRetry")}
    ${grab("archivePr")}
    ${grab("revRkQuery")}
    ${grab("revKeyPr")}
    ${grab("revKeySelect")}
    ${grab("revKeyShowSel")}
    ${grab("revKeyShow")}
    ${grab("revKeyUndecided")}
    ${grab("revKey")}
    ${grab("revKeyQueue")}
    ${grab("revKeyReading")}
    ${grab("revKeyHunks")}
    ${grab("revKeyHunk")}
    ${grab("revKeyMark")}
    ${grab("revKeyHunkLine")}
    ${grab("revKeyFiles")}
    ${grab("revKeyFile")}
    // The page's keydown glue, replicated: one letter of chord memory, the table decides.
    let chord = "";
    const press = (k, mods) => {
      const pending = chord;
      chord = "";
      const action = shortcutFor({ key: k, ...(mods || {}) }, { pane: "review", pending });
      if (!action) return null;
      if (action === "rev-chord") { chord = "g"; return action; }
      if (action.startsWith("rev-")) revKey(action);
      return action;
    };
    return {
      press,
      render: force => renderReview(force),
      now: () => renderReviewNow(),
      breakHead: on => { headThrows = on; },
      open: id => openReview(id),
      setQueue: q => { revQueue = q; revHeld = "*"; },
      sums: (k, v) => revSums.set(k, v),
      sel: () => revSel,
      setSel: k => { revSel = k; revSelAt = Math.max(0, revNav.indexOf(k)); },
      nav: () => revNav.slice(),
      flash: () => revFlash,
      openKeys: () => [...revOpen],
      stackOpen: () => revStackOpenKey,
      row: k => toggleRevRow(k),
      stack: k => toggleRevStack(k),
      pending: k => revPending.get(k),
      compose: () => { revComposing = { repo: "alpha", number: 5, kind: "comment", text: "", answer: "", busy: false }; },
      reading: () => revReading,
      repoFilter: () => revRepoFilter,
      queued: () => revRenderQueued,
      flush: () => revRenderFlush(),
      show: el => revKeyShow(el),
      decided: k => revDecided.add(k),
      toasts,
    };
  `;
  const made = new Function(
    "fetch", "document", "localStorage", "revpane", "window", "setTimeout", "clearTimeout",
    "encodeURIComponent", "esc", "shortcutFor", body,
  )(fetch, documentStub, localStorage, revpane, windowStub, clk.setT, clk.clearT,
    encodeURIComponent, String, shortcutFor);
  return { ...made, posts, dom, composer, search, selection, windowStub, revpane,
           advance: ms => clk.advance(ms), renders: () => dom.renders,
           pane: () => revpane.innerHTML };
}

// The your-move lane orders by how long a row has waited, OLDEST first — so ascending dates by
// number make nav order equal number order, and every expectation below readable.
const PR = (n, over = {}) => ({
  number: n, repo_id: "alpha", title: `change ${n}`, author: "sam", lane: "needs-you",
  head_ref: `b${n}`, base_ref: "main", head_sha: `sha${n}`, draft: false,
  url: `https://github.com/alpha/pull/${n}`, updated_at: `2026-08-${String(n).padStart(2, "0")}T00:00:00Z`,
  my_review: "none", review_is_current: false, reasons: ["reviewer"], ...over });

const Q = prs => ({ ai: true, prs, blind_spots: [], queues: [], failed: [], fresh: true });

// The class list of one row in the painted queue, by its data-rk.
const rowClass = (html, k) => {
  const m = html.match(new RegExp(`<div class="([^"]*)" data-rk="${k.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"`));
  return m ? m[1] : null;
};

// ---- selection is a PR number: it survives a re-sort and a summary landing --------------------
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7), PR(9)]));
  w.render();
  w.press("j"); w.press("j");
  t.check("j j selects the second row by its number", w.sel(), "alpha#7");
  // Moving the selection is two class flips, never a render — so the paint is asserted after the
  // next render, which is exactly when a full repaint could lose it.

  // A summary lands: the queue re-renders. Nothing about the selection may move.
  w.sums("alpha#5", { depth: "line", line: "routine", flags: [], head_sha: "sha5" });
  w.render();
  t.check("a summary landing moves the selection nowhere", w.sel(), "alpha#7");
  t.check("and the selected row still paints selected", (rowClass(w.pane(), "alpha#7") || "").includes("sel"), true);

  // The lane re-sorts under the keys (the waited-longest ordering is by updated_at): age #5 so
  // the order becomes 7, 9, 5. The selection follows the NUMBER, not the position.
  w.setQueue(Q([PR(5, { updated_at: "2026-08-19T00:00:00Z" }), PR(7), PR(9)]));
  w.render();
  t.check("a re-sort moves the row, and the selection rides it", w.sel(), "alpha#7");
  t.check("to its new position", w.nav().indexOf("alpha#7"), 0);
  w.press("j");
  t.check("j continues from the row's NEW position", w.sel(), w.nav()[1]);
}

// ---- a selected row that leaves the list hands the selection over by position, with a flash ----
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7), PR(9)]));
  w.render();
  w.press("j"); w.press("j");   // alpha#7, position 2
  w.setQueue(Q([PR(5), PR(9)]));   // #7 merged away
  w.render();
  t.check("the row that took its place inherits the selection", w.sel(), w.nav()[1]);
  t.check("and it flashed once", (rowClass(w.pane(), w.sel()) || "").includes("flash"), true);
  w.render();
  t.check("once means once — the flash is not a state", (rowClass(w.pane(), w.sel()) || "").includes("flash"), false);
}

// ---- §6's one-line assertion, verbatim: the composer survives renderReview() ------------------
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7)]));
  w.render();
  w.compose();
  // Type four characters into the composer.
  w.composer.value = "hmm."; w.composer.selectionStart = 4; w.composer.focus();
  const before = w.renders();
  w.render();   // a summary lands, the 4s re-poll fires — every one of them ends here
  t.check("activeElement.id is still rev-compose", w.dom.active.id, "rev-compose");
  t.check("selectionStart is unchanged", w.composer.selectionStart, 4);
  t.check("the pane did not repaint under the caret", w.renders(), before);
  t.check("the render is owed, not dropped", w.queued(), true);
  w.render(); w.render();
  t.check("more renders while focused coalesce into the one owed", w.renders(), before);
  // The reader lets go: the owed render lands, once.
  w.dom.active = { tagName: "BODY", id: "" };
  w.flush();
  t.check("blur lands exactly the one coalesced render", w.renders(), before + 1);
  t.check("and nothing is owed after it", w.queued(), false);
}

// ---- a text selection over the pane holds the render too (§6 rule 3's sibling) ----------------
{
  const w = world();
  w.setQueue(Q([PR(5)]));
  w.render();
  const before = w.renders();
  w.selection.isCollapsed = false;
  w.selection.anchorNode = w.composer;   // any node the pane contains
  w.render();
  t.check("a live text selection defers the render", [w.renders(), w.queued()], [before, true]);
  w.selection.isCollapsed = true;
  w.flush();
  t.check("collapsing it lands the owed render", w.renders(), before + 1);
}

// ---- a throw above the rows keeps the last good queue, and says so out loud (SKEIN-268) ------
//
// The per-row guard covers the rows; this is everything else in the one big template — the picker,
// the lane headings, the failure boxes. `revpane.innerHTML = …` is the LAST statement, so a throw
// before it means the assignment never runs, and what was on screen stays on screen. That is the
// right outcome and it used to be an accident; now it is the design, and the reason is said.
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7)]));
  w.render();
  const drawn = w.pane();
  const before = w.renders();
  if (!drawn.length) throw new Error("the fixture drew nothing, so this check would prove nothing");

  w.breakHead(true);
  // Caught HERE as well, so an unguarded render fails as a named check rather than as a stack trace
  // that takes the rest of the suite with it — a suite that crashes says less about what broke than
  // one that reports.
  let escaped = null;
  try { w.render(); } catch (e) { escaped = String((e && e.message) || e); }
  t.check("a throw above the rows never escapes the render", escaped, null);
  t.check("and does not blank the pane", w.pane(), drawn);
  t.check("because the assignment is never reached", w.renders(), before);
  // `w.toasts` is the world's own array, already on the returned object — a second key of the same
  // name would have been silently shadowed by it, which is how this check first read `undefined`.
  t.check("and the reader is told, rather than devtools",
    (w.toasts[w.toasts.length - 1] || "").includes("boom above the rows"), true);
  // Once, not once per render: the same fault fires on every poll, and forty toasts about one bug
  // is how people learn to ignore toasts.
  const said = w.toasts.length;
  w.render();
  w.render();
  t.check("and told once, not once per render", w.toasts.length, said);

  w.breakHead(false);
  w.render();
  t.check("and the next good render paints again", w.renders(), before + 1);
}

// ---- but the READER'S OWN press is never deferred (SKEIN-264) --------------------------------
//
// Rule 2 exists to protect a caret from a render NOBODY ASKED FOR — a summary landing, the 4s
// re-poll. Applied to the reader's own press it does the opposite of its job: reported live as
// "posting comments button doesn't work, they aren't responsive even if something is happening in
// the background". The critique panel is a stack of textareas, so a reader who had selected a
// phrase in a drafted comment and pressed post got no "posting…", no disabled chip, nothing —
// while the request was genuinely in flight.
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7)]));
  w.render();
  w.compose();
  w.composer.value = "hmm."; w.composer.selectionStart = 4; w.composer.focus();
  const before = w.renders();
  w.render();
  t.check("a render nobody asked for still waits for the caret", w.renders(), before);
  w.now();
  t.check("the reader's own press paints inside the frame instead", w.renders(), before + 1);
  t.check("and leaves nothing owed behind it", w.queued(), false);

  // Through a real handler, which is where the report came from — the press is a press whether the
  // page routes it through `renderReviewNow` directly or through a handler that calls it.
  w.composer.focus();
  const at = w.renders();
  w.row("alpha#5");
  t.check("opening a row paints it, caret in the composer or not", w.renders() > at, true);
}

// The same for a text SELECTION, which is the shape the owner actually hit: a phrase selected
// inside a drafted comment, then a press.
{
  const w = world();
  w.setQueue(Q([PR(5)]));
  w.render();
  w.selection.isCollapsed = false;
  w.selection.anchorNode = w.composer;
  const before = w.renders();
  w.render();
  t.check("a selection over the pane still defers a background render", w.renders(), before);
  w.now();
  t.check("and does not defer the press", w.renders(), before + 1);
}

// ---- expansion is exclusive: at most one row or stack open ------------------------------------
{
  const w = world();
  const stacked = [PR(1, { head_ref: "s-01", base_ref: "main" }),
                   PR(2, { head_ref: "s-02", base_ref: "s-01" }),
                   PR(3, { head_ref: "s-03", base_ref: "s-02" })];
  w.setQueue(Q([PR(5), PR(7), ...stacked]));
  w.render();
  w.row("alpha#5");
  w.row("alpha#7");
  t.check("opening a second row closes the first", w.openKeys(), ["alpha#7"]);
  w.stack("stack:alpha#1");
  t.check("opening a stack closes every row", [w.openKeys(), w.stackOpen()], [[], "stack:alpha#1"]);
  w.row("alpha#5");
  t.check("opening a row closes the stack", [w.openKeys(), w.stackOpen()], [["alpha#5"], null]);
}

// ---- → enters the stack on its next actionable step; ← leaves at the head ---------------------
{
  const w = world();
  const stacked = [PR(1, { head_ref: "s-01", base_ref: "main", lane: "waiting" }),
                   PR(2, { head_ref: "s-02", base_ref: "s-01" }),
                   PR(3, { head_ref: "s-03", base_ref: "s-02" })];
  w.setQueue(Q(stacked));
  w.render();
  w.press("j");
  t.check("the stack is one row, selected", w.sel(), "stack:alpha#1");
  w.press("ArrowRight");
  t.check("→ opens it and lands on the next actionable step", [w.stackOpen(), w.sel()],
    ["stack:alpha#1", "alpha#2"]);
  w.press("j");
  t.check("j walks the steps", w.sel(), "alpha#3");
  w.press("j");
  t.check("and clamps at the last step rather than falling out", w.sel(), "alpha#3");
  w.press("Escape");
  t.check("esc leaves the stack, selection back at its head", [w.stackOpen(), w.sel()],
    [null, "stack:alpha#1"]);
}

// ---- ↵ opens the reading view; esc returns ----------------------------------------------------
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7)]));
  w.render();
  w.press("j");
  w.press("Enter");
  // And at the COMMIT that row was showing (SKEIN-254): the reading view is pointed at a commit,
  // not at a pull request, so the diff it caches can be missed by a moved head instead of outliving
  // one for the life of the tab.
  t.check("enter opens the reading view for the selected number, at the commit it was showing",
    w.reading(), { repo: "alpha", number: 5, head_sha: "sha5" });
  w.press("Escape");
  t.check("esc comes back to the queue", w.reading(), null);
  t.check("with the selection intact", w.sel(), "alpha#5");
}

// ---- n/N skip what this session already decided -----------------------------------------------
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7), PR(9)]));
  w.render();
  w.press("j");        // alpha#5
  w.decided("alpha#7");
  w.render();
  w.press("n");
  t.check("n skips the decided row", w.sel(), "alpha#9");
  w.press("N");
  t.check("N walks back, also skipping it", w.sel(), "alpha#5");
}

// ---- e rides the hold/undo receipt; u takes it back; the lapse posts once ---------------------
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7)]));
  w.render();
  w.press("j");        // alpha#5
  w.press("e");
  t.check("e holds a set-aside instead of posting", [(w.pending("alpha#5") || {}).state, w.posts.length],
    ["waiting", 0]);
  t.check("the toast names the way back", w.toasts.some(s => /u undoes/.test(s)), true);
  t.check("selection advances to the next decision", w.sel(), "alpha#7");
  w.press("u");
  t.check("u cancels the act that never left", w.pending("alpha#5"), undefined);
  t.check("and the selection returns to the row", w.sel(), "alpha#5");
  w.advance(20000);
  t.check("the cancelled hold never posts", w.posts.length, 0);
  // Again, and let the window lapse: exactly one POST, to the archive route.
  w.press("e");
  w.advance(8000);
  t.check("the lapse posts the set-aside once", w.posts.map(p => p.url),
    ["/api/repos/alpha/review/5/archive"]);
}

// ---- a in the queue refuses, out loud (§6, second deliberate absence) -------------------------
{
  const w = world();
  w.setQueue(Q([PR(5)]));
  w.render();
  w.press("j");
  const before = w.posts.length;
  w.press("a");
  t.check("a acts on nothing from the queue", [w.pending("alpha#5"), w.posts.length], [undefined, before]);
  t.check("and says why, where a silent key would read as broken",
    w.toasts.some(s => /open it first/.test(s)), true);
}

// ---- g1…g9 follow the picker's order; gh opens GitHub here-and-only-here via g ----------------
{
  const w = world();
  w.setQueue(Q([PR(5), PR(7, { repo_id: "beta", url: "https://github.com/beta/pull/7" })]));
  w.render();
  t.check("g is a chord, not an act", w.press("g"), "rev-chord");
  w.press("2");
  t.check("g2 filters to the second repo in the picker's order", w.repoFilter(), "beta");
  t.check("and the selection lands on its first row", w.sel(), w.nav()[0]);
  w.press("g"); w.press("h");
  t.check("gh opens the selected pull request on GitHub", w.windowStub.opened,
    ["https://github.com/beta/pull/7"]);
}

// ---- the opened thing lands at 25% of the viewport (§6 focus rule 4) --------------------------
{
  const w = world();
  // A pane 900 high at the top of the screen, the expansion's top edge at 800: the correction is
  // 800 − 900·0.25 = 575, putting the expansion at exactly a quarter of the viewport.
  w.revpane.getBoundingClientRect = () => ({ top: 0, bottom: 900, height: 900 });
  w.show({ getBoundingClientRect: () => ({ top: 800, bottom: 843 }) });
  t.check("the expansion scrolls to ~25% of the viewport, not merely into view", w.revpane.scrollTop, 575);
}

t.done();
