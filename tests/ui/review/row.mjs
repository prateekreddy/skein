// The review pane's browser suite, one row: its height and whose move it names, expanding it, the
// reading inside it, and the acts it offers.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import { check, find, mustSee, page, pressRow, settle, until } from "./setup.mjs";

console.log("\nthe row"); // SKEIN-156/157/158 — one height, whose-move, never silent
/** Where every row is, and how tall — queried and measured in ONE page task.
 *
 *  **`$$eval` is the wrong tool for geometry, and this file already says why in prose** (SKEIN-751,
 *  at "each row says in words why it needs you"): it is two protocol calls, and a repaint landing
 *  between them hands the second one nodes that are no longer in the document — whose every
 *  rectangle is 0. Measured as a row of 0px, which is how "the control changed the row's height:
 *  0 → 28" was reported by a check whose own probe had read 28 one statement earlier, on a run
 *  where `loadWorkflows`' answer was still repainting the pane behind it. One `page.evaluate` that
 *  queries and measures together cannot straddle a repaint. */
const rowGeometry = () => page.evaluate(() => [...document.querySelectorAll("#revpane .revrow")]
  .map(e => ({
    n: e.querySelector(".revnum")?.textContent || "",
    top: e.getBoundingClientRect().top,
    line: e.querySelector(".revline")?.getBoundingClientRect().height ?? null,
  })));
/** The tallest row line on screen, or `null` when the queue is not drawn to be measured. */
const tallestLine = async () => {
  const rows = (await rowGeometry()).map(r => r.line).filter(h => h !== null);
  return rows.length ? Math.max(...rows) : null;
};
await check("a draft is not ready, and the fold states its own composition", async () => {
  // The draft is not hidden and not your move: it is a COUNT with its reason, one click open.
  // Named by `data-lane`: two groups fold now, and a bare `.revfold` finds whichever the document
  // reaches first — which since SKEIN-302 is waiting-on-others.
  const fold = await find("#revpane .revlane[data-lane='not-ready'] h4.revfold");
  if (!fold) throw new Error("there is no not-ready fold on screen");
  const said = await fold.textContent();
  if (!/1 draft/.test(said)) throw new Error(`the fold does not state its composition: ${said.trim()}`);
  // Read then pressed, with a `textContent` round trip in between — the gap SKEIN-716 is about.
  await fold.click();
  // The rows arriving is the group opening, and the check below reads them: a beat that expired
  // first reported the draft as "not in the lane at all", which is a sentence about the product
  // written from a fact about the box.
  await until(() => {
    const el = document.querySelector("#revpane .revlane[data-lane='not-ready']");
    return !!el && el.querySelectorAll(".revtitle").length > 0;
  }, null, "the not-ready group did not open, so the draft the check below reads is not on screen");
});
await check("rows are one line high, so a day fits on a screen", async () => {
  const h = await tallestLine();
  if (h === null) throw new Error("no row is drawn to be measured, so this says nothing about height");
  if (h > 30) throw new Error(`a row is ${h}px — at 29px, 28 rows fit above a 900px fold; at ${h}px they do not`);
});
await check("the left mark is whose move, and scarce — not the check dot", async () => {
  const dots = await page.$$("#revpane .revdot");
  if (dots.length) throw new Error("the check dot is still in the scan position");
  const yours = await page.$$("#revpane .mv.yours");
  const theirs = await page.$$("#revpane .mv.theirs, #revpane .mv.done");
  if (!yours.length) throw new Error("nothing on screen is marked as your move");
  if (!theirs.length) throw new Error("every row is lit — a mark true of every row is a texture");
});
// SKEIN-1028: this went red once under load and green alone, and the log that said which chip is
// gone, so the cause is not known. At this point the pane draws four rows, so ANY chip on two of
// them is "more than a third" — and several chip kinds (`● updated`, a workflow's) are placed by
// answers that land asynchronously. The failure now carries every row's chips, the page's own
// demoted set and the size of the queue it computed that from, so the next red names its cause.
// Read in one page task, so a render landing mid-read cannot hand back a half-replaced pane.
await check("no chip is true of more than a third of the queue", async () => {
  const seen = await page.evaluate(() => ({
    rows: [...document.querySelectorAll("#revpane .revrow")].map(e => ({
      n: e.querySelector(".revnum")?.textContent || "?",
      tags: [...e.querySelectorAll(".revtag")].map(t => t.textContent.trim()),
    })),
    common: [...revCommonChips],
    queued: (revQueue && revQueue.prs || []).length,
  }));
  const rows = seen.rows.map(r => r.tags.map(t => t.split(" ")[0]));
  const counts = {};
  for (const tags of rows) for (const t of new Set(tags)) counts[t] = (counts[t] || 0) + 1;
  const mass = Object.entries(counts).filter(([, n]) => n > rows.length / 3);
  if (mass.length) throw new Error(`chips worn by most of the queue: ${JSON.stringify(mass)} of ${rows.length} rows ` +
    `— every row: ${JSON.stringify(seen.rows)}; demoted by the page: ${JSON.stringify(seen.common)}; ` +
    `${seen.queued} in the queue it tallied from`);
});


console.log("\nexpanding");


console.log("\nreading");
// SKEIN-148/161: the pane used to contain no code, and approve was the first, highlighted button
// next to "Not read yet". The change is readable here now, and a verdict exists only beside it.
// **The verdict lives on the ROW now** (SKEIN-449). It used to be offered only inside the reading
// view, on the rule that no verdict comes from a surface that is not showing you the change
// (`docs/parity.md` §3). The reading view is going: the change is read on GitHub and a session
// does the reviewing, so the rule went with the surface that justified it. What must still hold is
// that everything the owner asked to keep is reachable from the row — "I want to be able to approve
// when I want with some comments or post some comments of my own and request changes or just
// comment" — and that there is still a way OUT to the change itself.
await check("the row offers every verdict, and a way out to the change", async () => {
  await pressRow("default timeout");
  await until(() => !!document.querySelector("#revpane .revrow.open .revrowacts"), null,
    "pressing the row did not open it");
  const own = await page.$$eval("#revpane .revrow.open .revrowacts .revchip",
    els => els.map(e => e.textContent.trim()));
  for (const want of ["approve", "request changes…", "comment…"]) {
    if (!own.includes(want))
      throw new Error(`the row does not offer ${want}: ${JSON.stringify(own)}`);
  }
  // An anchor, not a button: reading the change is leaving skein now, and it must say so by being
  // a link rather than something that looks like it opens a pane.
  const out = await page.$$eval("#revpane .revrow.open .revrowacts a.revchip",
    els => els.map(e => ({ text: e.textContent.trim(), href: e.getAttribute("href") })));
  const gh = out.find(o => /read on GitHub/i.test(o.text));
  if (!gh) throw new Error(`no way out to the change itself: ${JSON.stringify(out)}`);
  if (!/^https?:\/\//.test(gh.href || ""))
    throw new Error(`the way out does not go anywhere: ${JSON.stringify(gh)}`);
});

console.log("\nacts");
// **The composer is finished from the keyboard** (SKEIN-606, the owner's option A, approved as
// drafted). ⌘↵ presses **post to GitHub**, through the same 8-second undo, and Esc presses cancel
// and keeps the words. Ctrl stands in for ⌘ here, as the handler says it must.
//
// Fails on: the `onkeydown` dropped from `#rev-compose`, Esc clearing what was typed, the chord
// posting past the undo window, or the hint reworded.
const composerKey = () => page.evaluate(() => revComposing && revComposeStore(revComposing));
// The hint names the key this platform has (SKEIN-1187): the chord the checks below press is
// Control+Enter, and a hint saying ⌘ to somebody with no ⌘ key is the defect. Decided from the
// browser's own platform, not from the page's `glyph`, so a glyph that stopped translating fails.
const CHORD = await page.evaluate(() => /Mac|iP(hone|ad|od)/.test(navigator.platform || navigator.userAgent || "") ? "⌘↵" : "Ctrl+↵");
await check("⌘↵ in a comment posts it through the undo window, not past it", async () => {
  await page.click("#revpane .revrow.open .revrowacts .revchip:has-text('comment')");
  await until(() => !!document.getElementById("rev-compose"), null, "pressing comment drew no composer");
  const hint = await page.$eval("#revpane .revcompose .revacts .revkeys", e => e.textContent.trim());
  if (hint !== `${CHORD} posts · Esc closes — your words are kept`)
    throw new Error(`the comment composer's hint is not the approved one: ${JSON.stringify(hint)}`);
  const key = await composerKey();
  const rowKey = await page.evaluate(() => revComposing.repo + "#" + revComposing.number);
  await page.fill("#rev-compose", "a comment sent from the keyboard");
  await page.focus("#rev-compose");
  await page.keyboard.press("Control+Enter");
  await until(() => !document.querySelector("#revpane .revcompose"), null, "⌘↵ left the comment composer open");
  const held = await page.evaluate(k => { const p = revPending.get(k); return p && { kind: p.kind, state: p.state }; }, rowKey);
  if (!held || held.kind !== "comment" || held.state !== "waiting")
    throw new Error(`the chord did not hold a comment in its undo window: ${JSON.stringify(held)}`);
  // Taken back inside the window, so nothing reaches GitHub and the checks below meet the row as
  // they were written against it.
  await page.evaluate(k => revUndo(k), rowKey);
  await page.evaluate(k => { try { localStorage.removeItem(k); } catch {} }, key);
  await settle();
});
export { tallestLine };
