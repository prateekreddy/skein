// The review pane's browser suite, one row: its height and whose move it names, expanding it, the
// reading inside it, and the acts it offers.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import { check, mustSee, page, pressRow, settle, until } from "./setup.mjs";

console.log("\nthe row"); // SKEIN-156/157/158 — one height, whose-move, never silent
await check("every row states something in its gist — read, reading, or not read", async () => {
  const gists = await page.$$eval("#revpane .revrow .gist", els => els.map(e => e.textContent.trim()));
  if (!gists.length) throw new Error("no gist cells at all");
  const silent = gists.filter(g => !g);
  if (silent.length) throw new Error(`${silent.length} rows say nothing — silence reads as reassurance`);
});
await check("an unread row is visually a stated absence, not a short summary", async () => {
  const mark = await page.$eval("#revpane .gist.unknown", e => {
    const cs = getComputedStyle(e);
    return { deco: cs.textDecorationStyle, style: cs.fontStyle, text: e.textContent.trim() };
  });
  if (!mark.text.startsWith("not read")) throw new Error(`the absence does not say so: ${mark.text}`);
  if (mark.deco !== "dotted" || mark.style !== "italic")
    throw new Error(`the absence mark is not distinguishable at a glance: ${JSON.stringify(mark)}`);
});
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
await check("a summary landing moves no row", async () => {
  const before = await rowGeometry();
  await page.evaluate(() => {
    const pr = (revQueue.prs || []).find(p => !revSums.get(rk(p)));
    revSums.set(rk(pr || revQueue.prs[0]), { depth: "line",
      line: "a synthetic summary long enough to want a second line if anything would give it one",
      flags: [], yours: [], others: 0, head_sha: (pr || revQueue.prs[0]).head_sha });
    renderReview();
  });
  const after = await rowGeometry();
  for (const b of before) {
    const a = after.find(x => x.n === b.n && b.n);
    if (a && Math.abs(a.top - b.top) > 0)
      throw new Error(`row ${b.n} moved ${a.top - b.top}px when a summary landed`);
  }
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

// The tripwire marks belong on the collapsed line, because they are the reason to stop scrolling.
await check("a contract change is flagged where you can see it without opening", async () => {
  const rows = await page.$$eval("#revpane .revrow", els => els.map(e => ({
    text: e.querySelector(".revtitle")?.textContent.trim() || "",
    flags: [...e.querySelectorAll(".revtag.flag")].map(f => f.textContent.trim()),
  })));
  const timeout = rows.find(r => r.text.includes("default timeout"));
  const fix = rows.find(r => r.text.includes("null deref"));
  if (!timeout?.flags.includes("default")) throw new Error(`a changed default is not flagged: ${JSON.stringify(timeout)}`);
  if (fix?.flags.length) throw new Error(`a bug fix was flagged as a contract change: ${JSON.stringify(fix)}`);
});

console.log("\nexpanding");
// SKEIN-287. The queue payload carries the ROW shape now — the line, the flags, the depth, and
// whether a review is drafted — and the prose arrives when a row is opened, one row at a time.
// Measured on the owner's fleet: 153,381 bytes for thirty-nine readings, and that response holds one
// of the browser's per-origin connections for as long as it takes.
//
// SKEIN-704. The check below waits for a THIN reading, and that wait timed out on CI while passing
// everywhere else — because whether a thin reading ever lands is decided by a RACE, not by patience.
//
// `loadReview` starts `loadKnownSummaries` (the bulk row payload) and `loadWorkflows` in that order.
// `loadWorkflows` ends by pumping, and the pump reads every row `revSums` does not hold. So when the
// workflows answer arrives first, skein reads ahead for rows whose readings were already on disk;
// each read lands over the stream as a FULL reading; and the bulk payload, when it finally arrives,
// is discarded row by row by the guard that will not overwrite a full reading with a thinner one.
// Nothing thin ever lands, and the wait below has nothing left to wait for.
//
// Held open rather than slowed down, so this proves the ORDER and not a duration: the bulk payload
// is stopped in flight until the workflows answer has been on the page for a settle, and the
// assertion is that skein read nothing during that window. Reverting `revKnownHeard` fails it in
// 800ms with four reads named, which is how it was checked.
await check("skein does not read ahead before it has heard what it already holds", async () => {
  const reads = [];
  const spy = req => {
    if (req.method() === "POST" && /\/review\/\d+\/read/.test(req.url())) reads.push(new URL(req.url()).pathname);
  };
  let flows = 0;
  const flowspy = res => { if (/\/workflows$/.test(new URL(res.url()).pathname)) flows++; };
  let release = () => {};
  const gate = new Promise(r => { release = r; });
  const router = async route => {
    await gate;
    await route.continue().catch(() => {});
  };
  page.on("request", spy);
  page.on("response", flowspy);
  await page.route("**/review/summaries*", router);
  try {
    await page.evaluate(() => { revSums = new Map(); revFlows = new Map(); openReview(""); loadReview(true); });
    // Waited for, not assumed: the window this check is about opens when the workflows payload has
    // landed, because that is the answer whose `.then` pumps.
    for (const deadline = Date.now() + 10000; !flows && Date.now() < deadline; ) await settle(100);
    if (!flows) throw new Error("the workflows payload never landed, so there was no race to lose");
    // **And the page has APPLIED it, which is what the pump hangs off** (SKEIN-833). `settle(800)`
    // stood here, in front of the negative assertion below, and that is the wrong way round: the
    // reads this forbids are dispatched by `revPumpSummaries` inside the workflows `.then`, so a
    // box where that `.then` has not run inside 800ms reports "skein did not read ahead" about a
    // pump that had not been reached — green for the one reason nobody notices, and no evidence at
    // all that `revKnownHeard` is doing its job.
    //
    // `revFlows` is the page's own record, written on the line before the pump runs
    // (index.html:4484-4490) and cleared above so it cannot be answered by the load that opened this
    // pane. One entry per repo in the queue, and `.catch` fills it too, so there is no arrival this
    // can wait on for ever. A whole `.then` is one task; `page.evaluate` runs between tasks, so a
    // full `revFlows` is a pump that has already run — and a request it made was reported to this
    // process before the reply that said so.
    const repos = await page.evaluate(() => ((revQueue || {}).queues || []).length);
    if (!repos) throw new Error("the queue names no repo, so no workflows answer could pump anything");
    await page.waitForFunction(n => revFlows.size >= n, repos, { timeout: 20000 })
      .catch(async () => {
        const got = await page.evaluate(() => revFlows.size);
        throw new Error(`the page applied ${got} of ${repos} workflows answers — the pump this check `
          + "forbids was never reached, so its silence proves nothing");
      });
    if (reads.length)
      throw new Error(`skein read ahead with the bulk payload still in flight: ${reads.join(", ")}`);
  } finally {
    release();
    await page.unroute("**/review/summaries*", router).catch(() => {});
    page.off("request", spy);
    page.off("response", flowspy);
  }
  // And the payload it waited for is applied: the rows on disk arrive thinned, which is the whole
  // point of having waited.
  await page.waitForFunction(
    () => [...revSums.values()].some(s => s && s !== "…" && s.thin), null, { timeout: 20000 });
});

// Driven through the rendered row, and the request log is the assertion — the shape of the payload
// is invisible from the DOM, and what matters is which requests the page actually makes.
await check("the queue asks for rows, and a row asks for its own prose when it opens", async () => {
  const asked = [];
  const spy = req => {
    const u = new URL(req.url());
    if (/\/review\/(summaries|\d+\/summary)$/.test(u.pathname)) asked.push(u.pathname + u.search);
  };
  page.on("request", spy);
  /** Wait — up to `ms` — for something the request log is supposed to come to hold.
   *
   * It does not throw, and that is the point: the assertion stays exactly where it was and keeps
   * its own sentence, so a queue that really never asked still fails with "the queue never asked
   * for its readings at all". This only stops the check deciding that at a fixed 600 or 800 ms,
   * which is a number guessed on an idle machine — and CI runs four browser suites on four cores
   * (`browser_suites::lanes`), where a request the page has genuinely made can still be on its way
   * (SKEIN-621).
   *
   * It is used by both negative assertions below too, and that is the change SKEIN-833 made and
   * SKEIN-842 finished: a negative assertion over a request log is a claim about a window only
   * until you give it a later request to stand behind, and then it is a claim about an ORDER. There
   * is no fixed beat left in this check. */
  const until = async (got, ms = 8000) => {
    for (const deadline = Date.now() + ms; !got() && Date.now() < deadline; ) await settle(100);
  };
  try {
    await page.evaluate(() => { revSums = new Map(); openReview(""); loadReview(true); });
    // Until a THIN reading is on the page: that is the bulk payload's row shape having landed, and
    // it is what the rest of this check is about. Waiting for "any reading" would let the pump's own
    // answer — a full one, fetched for a row nothing had read — stand in for it.
    await page.waitForFunction(
      () => (revQueue?.prs || []).length > 0 && [...revSums.values()].some(s => s && s !== "…" && s.thin),
      null, { timeout: 20000 });
    await until(() => asked.some(u => u.includes("/summaries")));
    const bulk = asked.filter(u => u.includes("/summaries"));
    if (!bulk.length) throw new Error("the queue never asked for its readings at all");
    if (!bulk.every(u => u.includes("rows=1")))
      throw new Error(`the queue asked for the whole prose to draw a list: ${bulk.join(", ")}`);
    // A collapsed row draws its line from the row shape and fetches no PROSE. The pump's own reads
    // go to the same route and are not this — they are `Trigger::Unasked` analyses of rows nothing
    // has read yet, and they carry no `held` marker. Prose is the request with `held=1`.
    //
    // **The assertion about that is below, behind the first prose request there is**, and the
    // `settle(600)` that used to stand here is gone (SKEIN-842). It was the seventh of the family
    // SKEIN-833 converted six of, left because it is weaker rather than broken: the
    // `waitForFunction` above waits for the thin payload, so the queue really had been drawn. What
    // the beat could not do is tell a queue that asked for no prose from a request log this process
    // had not been told about yet — playwright reports a request over the CDP connection, and both
    // read as an empty log from here. So the claim is no longer "nothing had arrived by 600ms" but
    // "the reader's own press is what asked first", which is an order and cannot be answered by a
    // slow box.

    // Opening a row fetches its prose — and it is a request to REMEMBER, never to analyse.
    const key = await page.evaluate(() => {
      const pr = (revQueue.prs || []).find(p => {
        const s = revSums.get(p.repo_id + "#" + p.number);
        return s && s !== "…" && s.thin && s.depth !== "unread";
      });
      if (!pr) return null;
      const k = pr.repo_id + "#" + pr.number;
      if (!revOpen.has(k)) toggleRevRow(k);
      return k;
    });
    if (!key) throw new Error("no thinned row to open, so this check would prove nothing");
    const n = Number(key.slice(key.lastIndexOf("#") + 1));
    const perRow = () => asked.filter(u => /\/\d+\/summary/.test(u) && u.includes("held=1"));
    const askedFor = m => perRow().filter(u => u.includes(`/${m}/summary`));
    await until(() => perRow().length);
    const mine = perRow();
    if (!mine.length) throw new Error("opening a row did not fetch the prose the list left behind");
    // **And that request is the FIRST prose request in the log** (SKEIN-842) — the collapsed queue
    // above asked for none. The log is in arrival order and the press that opened the row came
    // after every render the collapsed queue made, so a prose fetch the list had made would sit in
    // front of this one. A log nothing whatever reaches now fails on the line above, naming it,
    // instead of reading like a queue that behaved.
    if (!new RegExp(`/${n}/summary`).test(mine[0]))
      throw new Error(`a collapsed queue fetched prose per row: ${mine[0]} was asked for before the `
        + `row the reader opened (#${n}) asked for its own — the whole log is ${perRow().join(", ")}`);
    // And it asked ONCE, which is the one case an order cannot decide: a collapsed queue that
    // fetched this row's prose and no other names the same row the press does, so it arrives first
    // and reads like the press. A count tells them apart — the press makes exactly one request, and
    // the pump never reads a row that already has a reading, which is what the assertion at the end
    // of this check already rests on. Seen to fail: with the page fetching prose for this row alone
    // while collapsed, the assertion above passes and this one names two.
    if (askedFor(n).length !== 1)
      throw new Error(`the row the reader opened has ${askedFor(n).length} prose requests behind `
        + `it, so one of them was asked for before the press: ${askedFor(n).join(", ")}`);
    // Belt and braces on the marker the filter above already used: an opened row must never be
    // able to reach a model call, on any head, at any hour of the budget.
    if (!mine.every(u => u.includes("held=1")))
      throw new Error(`an opened row could have spent a model call: ${mine.join(", ")}`);
    // And it landed: the brief the row shape cannot carry is on screen — WAITED for, because what
    // stands above this is the arrival of a REQUEST and this is about the answer to it (SKEIN-842).
    // The `settle(800)` that used to sit in front of the request wait was also, by accident, the
    // slack this read lived on; with the beat gone this read a body whose prose was still in flight
    // and called it missing. Seen to fail that way on a box running three other suites, which is
    // the box this tier actually runs on. A positive assertion behind a real wait still goes red
    // when the prose never comes — it just no longer goes red when it is merely late.
    await page.waitForFunction(() => {
      const el = document.querySelector("#revpane .revrow.open .revbody");
      return !!el && !/fetching the brief/.test(el.textContent);
    }, null, { timeout: 20000 }).catch(async () => {
      const body = (await page.textContent("#revpane .revrow.open .revbody").catch(() => "(no body)"))
        .replace(/\s+/g, " ");
      throw new Error(`the prose never arrived: ${body.slice(0, 200)}`);
    });

    // Asked once. A row that re-fetches on every render is the bulk payload's cost back in pieces.
    //
    // Counted for THIS row rather than over the whole log: `revFetchHeld` reaches the same route
    // with the same marker whenever a read the pump started lands (index.html:3428), and a total
    // that a neighbouring row can move is a total this assertion cannot read.
    const before = askedFor(n).length;
    await page.evaluate(() => renderReviewNow());
    // **A later request is what says the repaint's own is not coming** (SKEIN-833). `settle(400)`
    // stood here in front of a negative assertion over a request log, and it fails the wrong way
    // round: playwright reports a request over the CDP connection, so a box where that report is
    // still on its way at 400ms reports "the row did not ask again" about a fetch nothing had told
    // this process about yet — and a log nothing whatever reaches reads exactly the same from here.
    //
    // So the check makes a request it KNOWS must appear: a second thinned row, opened, which
    // fetches its prose the way the first one did. The log is in arrival order and the repaint went
    // first, so the second row's request cannot arrive in front of one the repaint made — and a log
    // that never fills now fails here, naming that, instead of passing below.
    const second = await page.evaluate(k => {
      const pr = (revQueue.prs || []).find(p => {
        const k2 = p.repo_id + "#" + p.number;
        const s = revSums.get(k2);
        return k2 !== k && !revInFlight.has(k2) && s && s !== "…" && s.thin && !s.waiting && s.depth !== "unread";
      });
      if (!pr) return null;
      const k2 = pr.repo_id + "#" + pr.number;
      if (!revOpen.has(k2)) toggleRevRow(k2);
      return pr.number;
    }, key);
    if (second === null)
      throw new Error("no second thinned row to open, so a repaint that asked again could not be told "
        + "from a request log nothing reaches");
    await until(() => askedFor(second).length);
    if (!askedFor(second).length)
      throw new Error(`opening a second row fetched no prose (${perRow().join(", ")}) — nothing is `
        + "reaching this log, so what it does not hold says nothing about the repaint");
    if (askedFor(n).length !== before)
      throw new Error(`the row asked again on a repaint: ${askedFor(n).join(", ")}`);
  } finally {
    page.off("request", spy);
  }
});
await check("a flagged PR opens to a brief, not to a diff", async () => {
  await pressRow("default timeout");
  // The brief arriving is what the press is waited for: it is fetched when the row opens (the check
  // above is about that request), so a beat here decides between "the brief is missing the section
  // that matters" and "the brief had not come back yet", which are not the same finding.
  await until(() => !!document.querySelector("#revpane .revrow.open .revbrief"), null,
    "the opened row drew no brief at all");
  const brief = (await page.$eval("#revpane .revrow.open .revbrief", e => e.textContent)).toLowerCase();
  if (!brief.includes("what changes in how it works"))
    throw new Error(`the brief is missing the section that matters: ${brief.slice(0, 120)}`);
});
// The scanner runs with no model at all and can only escalate. The stub `gh` serves a diff whose
// only change is a moved constant, so a signal must appear — and it must be visually separate from
// the model's prose, because "the diff says so" is a stronger claim than "a model thinks so".
await check("mechanical evidence is shown, and shown apart from the prose", async () => {
  const sig = await page.$eval("#revpane .revrow.open .revsignals", e => e.textContent).catch(() => "");
  if (!/found in the diff/i.test(sig)) throw new Error("the evidence block is missing");
  if (!/TIMEOUT/.test(sig)) throw new Error(`the moved constant was not found: ${sig}`);
  if (!/default/.test(sig)) throw new Error(`it was not classified as a default: ${sig}`);
});
// Stage 0 runs without any model, and it is what scopes the rest. If CODEOWNERS said `src/ @me`
// and the PR touched src/ and web/, the pane must say which half is yours.
await check("the brief says which of it you own", async () => {
  const owned = await page.$eval("#revpane .revrow.open .revowned", e => e.textContent).catch(() => "");
  if (!owned.includes("src/parser.rs")) throw new Error(`ownership was not applied: ${owned}`);
  if (!owned.includes("do not own")) throw new Error(`what it left out is not stated: ${owned}`);
});

console.log("\nreading");
// SKEIN-148/161: the pane used to contain no code, and approve was the first, highlighted button
// next to "Not read yet". The change is readable here now, and a verdict exists only beside it.
// **The verdict lives on the ROW now** (SKEIN-449). It used to be offered only inside the reading
// view, on the rule that no verdict comes from a surface that is not showing you the change
// (`docs/review-ux.md` §6). The reading view is going: the change is read on GitHub and a session
// does the reviewing, so the rule went with the surface that justified it. What must still hold is
// that everything the owner asked to keep is reachable from the row — "I want to be able to approve
// when I want with some comments or post some comments of my own and request changes or just
// comment" — and that there is still a way OUT to the change itself.
await check("the row offers every verdict, and a way out to the change", async () => {
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
// drafted). ⌘↵ presses the primary button — **ask** for an ask, **post to GitHub** for a comment,
// through the same 8-second undo — Esc presses cancel and keeps the words, and while the composer
// is busy both keys are ignored. Ctrl stands in for ⌘ here, as the handler says it must.
//
// Fails on: the `onkeydown` dropped from `#rev-compose` (no answer arrives, the composer stays
// open), the busy guard dropped (an Esc mid-answer closes it, or a second ask is sent), Esc
// clearing what was typed, the chord posting past the undo window, or either hint reworded.
const composerKey = () => page.evaluate(() => revComposing && revComposeStore(revComposing));
await check("⌘↵ in an ask presses ask, and the composer says so", async () => {
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('ask')");
  await until(() => !!document.getElementById("rev-compose"), null, "pressing ask drew no composer");
  const hint = await page.$eval("#revpane .revcompose .revacts .revkeys", e => e.textContent.trim());
  if (hint !== "⌘↵ asks · Esc closes") throw new Error(`the ask composer's hint is not the approved one: ${JSON.stringify(hint)}`);
  await page.fill("#rev-compose", "why 5 seconds?");
  await page.focus("#rev-compose");
  await page.keyboard.press("Control+Enter");
  await page.waitForSelector("#revpane .revanswer", { timeout: 15000 })
    .catch(() => { throw new Error("⌘↵ in the ask composer asked nothing"); });
  await mustSee("#revpane .revanswer", "the answer the chord asked for");
});
await check("while it is busy both keys are ignored, and Esc then closes it with the words kept", async () => {
  const acts = [];
  const seen = r => { if (/\/review\/\d+\/act$/.test(r.url())) acts.push(r.url()); };
  page.on("request", seen);
  try {
    await page.fill("#rev-compose", "a question still being typed");
    await page.evaluate(() => { revComposing.busy = true; renderReviewNow(); });
    await page.focus("#rev-compose");
    await page.keyboard.press("Escape");
    await page.keyboard.press("Control+Enter");
    await page.evaluate(() => new Promise(requestAnimationFrame));
    if (!(await page.$("#rev-compose"))) throw new Error("Esc closed a busy composer");
    if (acts.length) throw new Error(`⌘↵ on a busy composer sent ${acts.length} request(s)`);
  } finally {
    page.off("request", seen);
    await page.evaluate(() => { if (revComposing) { revComposing.busy = false; renderReviewNow(); } });
  }
  await page.focus("#rev-compose");
  await page.keyboard.press("Escape");
  await until(() => !document.querySelector("#revpane .revcompose"), null, "Esc did not close the composer");
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('ask')");
  await until(() => !!document.getElementById("rev-compose"), null, "the ask composer would not reopen");
  const kept = await page.$eval("#rev-compose", e => e.value);
  if (kept !== "a question still being typed") throw new Error(`Esc lost the words: ${JSON.stringify(kept)}`);
  // Leave the next check the composer it was written against: closed, and nothing remembered.
  const key = await composerKey();
  await page.click("#revpane .revcompose .revchip:has-text('cancel')");
  await page.evaluate(k => { try { localStorage.removeItem(k); } catch {} }, key);
  await until(() => !document.querySelector("#revpane .revcompose"), null, "cancel left the composer open");
});
await check("⌘↵ in a comment posts it through the undo window, not past it", async () => {
  await page.click("#revpane .revrow.open .revrowacts .revchip:has-text('comment')");
  await until(() => !!document.getElementById("rev-compose"), null, "pressing comment drew no composer");
  const hint = await page.$eval("#revpane .revcompose .revacts .revkeys", e => e.textContent.trim());
  if (hint !== "⌘↵ posts · Esc closes — your words are kept")
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
// **An answer or a draft from outside its box says so above itself** (SKEIN-819, the owner's option
// A; wording as corrected by the owner on 2026-09-24, because the fall-through still stands in the
// box's checkout and loses only the earlier reading). The server puts `outside` on the act's answer when the turn fell
// through to a local call; the fixture's `claude` is named by SKEIN_CLAUDE_BIN, which skips the box
// altogether, so the route is answered here instead — what is under test is what the composer DOES
// with `outside`, and the Rust side (`acted`) is tested where it is built.
//
// Fails on: the line not drawn, drawn in other words, not surviving the composer being closed and
// reopened (which is the reload path: both restore from `revComposeSave`'s copy), not retired by an
// answer from inside the box, the draft's line drawn below the text it is about, or "draft again"
// drafting from the draft instead of from the notes.
const OUTSIDE_WHY = "skein has no record of where that box is, so it was never started or it is gone";
await check("an answer or a draft from outside its box says so above it, and asking again can retire it", async () => {
  const sent = [];
  let outside = OUTSIDE_WHY;
  await page.route("**/review/*/act", route => {
    const body = JSON.parse(route.request().postData() || "{}");
    sent.push(body);
    const text = body.kind === "ask" ? "an answer from here" : "COMMENT drafted here";
    return route.fulfill({ contentType: "application/json",
      body: JSON.stringify(outside ? { ok: true, text, outside } : { ok: true, text }) });
  });
  const line = () => page.$eval("#revpane .revcompose .outsidebox b", e => e.textContent.trim()).catch(() => null);
  try {
    await page.click("#revpane .revrow.open .revacts .revchip:has-text('ask')");
    await until(() => !!document.getElementById("rev-compose"), null, "pressing ask drew no composer");
    await page.fill("#rev-compose", "is this safe?");
    await page.click("#revpane .revcompose .revchip:has-text('ask')");
    await page.waitForSelector("#revpane .revanswer", { timeout: 15000 });
    const want = `Answered outside its box — ${OUTSIDE_WHY}. It did not have its earlier reading of this pull request, so this answer comes from your question and the code alone.`;
    if ((await line()) !== want) throw new Error(`the ask's line is not the approved one: ${JSON.stringify(await line())}`);
    await mustSee("#revpane .revcompose .outsidebox .revchip:has-text('ask again')", "the ask-again button");
    // Closed and reopened: the copy a reload restores from.
    await page.evaluate(() => { const c = revComposing; revComposeClose(); revCompose(c.repo, c.number, c.kind); });
    await until(() => !!document.querySelector("#revpane .revcompose .outsidebox"), null,
      "the line did not survive the composer being reopened, so a reload loses the warning and keeps the answer");
    outside = "";
    await page.click("#revpane .revcompose .outsidebox .revchip:has-text('ask again')");
    await until(() => !document.querySelector("#revpane .revcompose .outsidebox"), null,
      "an answer from inside the box left the outside line standing");
    const askKey = await page.evaluate(() => revComposeStore(revComposing));
    await page.click("#revpane .revcompose .revchip:has-text('cancel')");
    await page.evaluate(k => { try { localStorage.removeItem(k); } catch {} }, askKey);

    outside = OUTSIDE_WHY;
    await page.click("#revpane .revrow.open .revrowacts .revchip:has-text('comment')");
    await until(() => !!document.getElementById("rev-compose"), null, "pressing comment drew no composer");
    await page.fill("#rev-compose", "the notes the draft is made from");
    await page.click("#revpane .revcompose .revchip:has-text('draft with skein')");
    await until(() => !!document.querySelector("#revpane .revcompose .outsidebox"), null, "a draft from outside its box drew no line");
    const drafted = `Drafted outside its box — ${OUTSIDE_WHY}. It did not have its earlier reading of this change, so check every claim about the code before you post it.`;
    if ((await line()) !== drafted) throw new Error(`the draft's line is not the approved one: ${JSON.stringify(await line())}`);
    const above = await page.evaluate(() => {
      const l = document.querySelector("#revpane .revcompose .outsidebox"), t = document.getElementById("rev-compose");
      return !!(l.compareDocumentPosition(t) & Node.DOCUMENT_POSITION_FOLLOWING);
    });
    if (!above) throw new Error("the draft's line is not above the draft it is about");
    sent.length = 0;
    await page.click("#revpane .revcompose .outsidebox .revchip:has-text('draft again')");
    for (let i = 0; i < 50 && !sent.length; i++) await new Promise(r => setTimeout(r, 50));
    if (!sent.length || sent[0].kind !== "draft" || sent[0].body !== "the notes the draft is made from")
      throw new Error(`"draft again" did not draft from the notes: ${JSON.stringify(sent)}`);
    await page.waitForFunction(() => revComposing && !revComposing.busy, null, { timeout: 5000 });
    const draftKey = await page.evaluate(() => revComposeStore(revComposing));
    await page.click("#revpane .revcompose .revchip:has-text('cancel')");
    await page.evaluate(k => { try { localStorage.removeItem(k); } catch {} }, draftKey);
    await until(() => !document.querySelector("#revpane .revcompose"), null, "cancel left the composer open");
  } finally {
    await page.unroute("**/review/*/act");
  }
});
// **A review that would have posted and could not says why, on the open row** (SKEIN-516). The
// sentence is the server's (`review::Summary::not_posted`), so what this holds is the drawing: in the
// open row, in the notice style `read_outside_box` uses, verbatim, and gone when the field is. What
// would make it fail: `revDetail` not drawing `s.not_posted`, drawing it escaped-and-altered or in a
// different element, or leaving it standing once the reading no longer carries it.
await check("a review that could not post says why on the open row, in the server's words", async () => {
  const said = "Not posted: GitHub would not give the skein App a token for this repository — check the App is installed on it.";
  const key = await page.$eval("#revpane .revrow.open", e => e.dataset.rk).catch(() => null);
  if (!key) throw new Error("no open row to draw the notice on");
  const had = await page.evaluate(([k, said]) => {
    const s = revSums.get(k);
    if (!s || typeof s !== "object") return false;
    revSums.set(k, { ...s, not_posted: said });
    renderReviewNow();
    return true;
  }, [key, said]);
  if (!had) throw new Error(`the open row ${key} holds no reading to carry the field`);
  try {
    const notice = await mustSee("#revpane .revrow.open .revstale.notposted", "the not-posted notice");
    const text = (await notice.locator("b").textContent()).trim();
    if (text !== said) throw new Error(`not the server's sentence, verbatim: ${JSON.stringify(text)}`);
  } finally {
    await page.evaluate(k => {
      const s = revSums.get(k);
      if (s && typeof s === "object") { const { not_posted, ...rest } = s; revSums.set(k, rest); }
      renderReviewNow();
    }, key);
  }
  await until(() => !document.querySelector("#revpane .revrow.open .revstale.notposted"), null,
    "the notice outlived the field it draws");
});
// Private by construction: an answer that might be published is a different, more careful, less
// useful answer — so asking must never look like a step on the way to posting.
await check("asking a question keeps the answer off GitHub", async () => {
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('ask')");
  await until(() => !!document.querySelector("#revpane .revcompose .revcl"), null,
    "pressing ask drew no composer");
  // Scoped to the composer: the drafted review that now arrives with the summary (one model call
  // since 2026-08-24) puts its own `.revcl` on screen above this one.
  const label = await page.$eval("#revpane .revcompose .revcl", e => e.textContent);
  if (!/stays between you and skein/i.test(label))
    throw new Error(`the composer does not promise privacy: ${label}`);
  await page.fill("#rev-compose", "why 5 seconds?");
  await page.click("#revpane .revcompose .revchip:has-text('ask')");
  await page.waitForSelector("#revpane .revanswer", { timeout: 15000 });
  await mustSee("#revpane .revanswer", "the private answer");
  // …and it must not have offered to publish it.
  const posts = await page.$$("#revpane .revcompose .revchip:has-text('post to GitHub')");
  if (posts.length) throw new Error("an ask offered to post its answer");
});
await check("a comment is drafted into the box you edit, not sent", async () => {
  await page.click("#revpane .revcompose .revchip:has-text('cancel')");
  await settle();
  // The composer opens from the ROW now (SKEIN-449), beside the verdict it will carry.
  await page.click("#revpane .revrow.open .revrowacts .revchip:has-text('comment')");
  await settle();
  await page.fill("#rev-compose", "ask them what happens to slow callers");
  await page.click("#revpane .revcompose .revchip:has-text('draft with skein')");
  await page.waitForFunction(
    () => /shortens/i.test(document.getElementById("rev-compose")?.value || ""),
    null, { timeout: 15000 });
  const posted = await page.$("#revpane .revcompose .revchip:has-text('post to GitHub')");
  if (!posted) throw new Error("a draft with no way to send it");
  await mustSee("#revpane .revcompose .revchip:has-text('post to GitHub')", "the post button");
});
await check("posting is a separate press from drafting", async () => {
  await page.click("#revpane .revcompose .revchip:has-text('post to GitHub')");
  // The composer going is what the press does; a beat here reads "still open" for a post that is
  // merely still in flight, which is the reported failure and not the one this check is about.
  await until(() => !document.querySelector("#revpane .revcompose"), null,
    "the composer stayed open, so it is unclear whether it sent");
});

export { tallestLine };
