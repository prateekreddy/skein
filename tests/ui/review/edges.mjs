// The review pane's browser suite, a row at its edges: edge states, an expanded row, and one row
// failing to draw.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import { REDRAW_MS, check, mustSee, page, settle, until } from "./setup.mjs";
import { tallestLine } from "./row.mjs";

console.log("\nedge states");   // SKEIN-154 — both were correct prose and inert as affordances
// SKEIN-228. Re-analysis existed only behind the fold, ninth of nine chips, which from the outside
// is the same as not existing. The row that most needs it is one read against an EARLIER commit —
// the line already says so, and this is the answer to that sentence.
await check("a reading of an older commit offers its re-read on the line", async () => {
  // Both measurements go through `tallestLine`, which queries and measures in one page task — this
  // pair is the site that reported `0 → 28` while the queue it had just probed was 28px throughout
  // (SKEIN-751, and see `rowGeometry`).
  const before = await tallestLine();
  if (before === null) throw new Error("no row is drawn to measure against");
  const key = await page.evaluate(() => {
    // **Collapsed, written down rather than arrived at.** This check is about the control on the
    // COLLAPSED line — `revReadAgain` opens with `if (open) return ""`, so an expanded row correctly
    // offers nothing there — and the row it picked was whichever one an earlier check had left
    // open. It passed only while the fixture's summaries were failing to arrive: a row with no
    // usable reading was never the one this `find` chose. Repair the fixture and the check picks
    // the open row and fails, which is the check depending on an accident, not on the product.
    revOpen = new Set();
    revStackOpenKey = null;
    revStackStep = null;
    const pr = (revQueue.prs || []).find(p => {
      const s = revSums.get(rk(p));
      return p.lane === "needs-you" && s && s !== "…" && s.depth !== "unread";
    });
    if (!pr) throw new Error("no read row in your-move to make stale — the fixture stopped summarising");
    const s = revSums.get(rk(pr));
    // What the bulk payload says when the branch has moved under a reading skein already has.
    revSums.set(rk(pr), { ...s, stale: true, head_sha: "older" });
    renderReviewNow();
    return rk(pr);
  });
  const btn = await mustSee(`#revpane .revrow[data-rk="${key}"] .revread`, "the row's read control");
  const after = await tallestLine();
  if (after === null) throw new Error("the row went off screen between the two measurements");
  if (after > before)
    throw new Error(`the control changed the row's height: ${before} → ${after}`);
  // And it says nothing about the day's budget (SKEIN-352 copy pass): "never counted against the
  // day" is true of the BUDGET and reads on a control as "this is free", on a press that spends a
  // model call taking most of a minute. It says what it reads against instead; the sentence about
  // the budget survives in the one place the budget is the subject, which is the panel that says
  // the day's automatic reading has stopped.
  const title = await btn.getAttribute("title");
  if (/counted against the day/.test(title))
    throw new Error(`the control still claims something about the day's budget: ${title}`);
  if (!/against the commit that is there now/.test(title))
    throw new Error(`the control does not say what it reads against: ${title}`);

  // Pressing it asks the server the way a person asks — and does not toggle the fold underneath,
  // which is what a control inside a row whose whole line is a toggle would do by default.
  const fold = () => page.$(`#revpane .revrow[data-rk="${key}"].open`).then(Boolean);
  const wasOpen = await fold();
  const urls = [];
  const listen = r => urls.push(r.url());
  page.on("request", listen);
  await btn.click();
  // The request going out is what the press DOES, so the press is waited for on the request log —
  // which this check already holds. A beat closed the log while the press was still on its way and
  // reported "no manual read went out", which is the sentence for a control that does nothing.
  const wanted = () => urls.some(u => /review\/\d+\/read\?redraft=1$/.test(u));
  for (const deadline = Date.now() + REDRAW_MS; !wanted() && Date.now() < deadline; ) await settle(25);
  page.off("request", listen);
  // `redraft=1` since SKEIN-293: there is one control, and it always produces both halves. It
  // implies the forced read, so the marker that says what must come BACK is the one on the wire.
  if (!urls.some(u => /review\/\d+\/read\?redraft=1$/.test(u)))
    throw new Error(`no manual read went out: ${JSON.stringify(urls.filter(u => u.includes("/read")))}`);
  if (await fold() !== wasOpen)
    throw new Error("pressing the read control toggled the row it sits on");
});
// A queue you have cleared is the best moment this product has, and it used to be "nothing here."
// in the corner of a 1400 px page while another repo held ten. This drives the real filter and the
// real render: the your-move rows are moved to another repo, so acme genuinely has none.
await check("a cleared queue reads like one and names what the rest of the fleet holds", async () => {
  // The load `openReview` starts must land BEFORE the rows are moved, or it arrives a moment later
  // and puts the real queue back under the assertion.
  await page.evaluate(() => openReview("acme"));
  await page.waitForFunction(() => !revLoading && revQueue && (revQueue.prs || []).length,
    null, { timeout: 15000 });
  await page.evaluate(() => {
    // Each moved row takes its reading with it. `rk` is repo + number, so a row that changes repo
    // becomes a row nothing has read — and the pump would then ask the server about a repo that
    // does not exist, which is a 404 in the console and a check failing three sections later.
    // `moveOf`, not `lane`: what has to leave for this repo to be CLEAR is the your-move list, and
    // since SKEIN-302 that list is not the `needs-you` lane — a pull request you opened with a
    // thread open on it is in it, and leaving it behind leaves the queue non-empty.
    revQueue = { ...revQueue, prs: (revQueue.prs || []).map(p => {
      if (moveOf(p) !== "yours") return p;
      const moved = { ...p, repo_id: "lattice" };
      revSums.set(rk(moved), revSums.get(rk(p))
        || { number: p.number, head_sha: p.head_sha, depth: "unread", unread_because: "nobody asked" });
      return moved;
    }) };
    renderReview();
  });
  await settle(300);
  const head = await mustSee("#revpane .revclear-head", "the cleared-queue headline");
  if (!/acme is clear/.test((await head.textContent()).trim()))
    throw new Error(`the headline is not about the repo you are looking at: ${await head.textContent()}`);
  const next = await mustSee("#revpane .revclear-next .revclear-row", "the honest next thing");
  const said = (await next.textContent()).replace(/\s+/g, " ").trim();
  if (!/lattice/.test(said) || !/^\d/.test(said))
    throw new Error(`the other repo is not named with its count: ${said}`);
  // A headline you cannot read is not a headline: it must outrank the sentence under it.
  const sizes = await page.evaluate(() => [
    parseFloat(getComputedStyle(document.querySelector("#revpane .revclear-head")).fontSize),
    parseFloat(getComputedStyle(document.querySelector("#revpane .revclear-sub")).fontSize)]);
  if (!(sizes[0] > sizes[1])) throw new Error(`the cleared screen has no headline: ${sizes}`);
});
// And the failure that used to replace the queue. Provoked in the page rather than by breaking
// GitHub for the rest of the run: what is asserted is what the pane does with the state, and the
// state is exactly what `loadReview`'s catch now builds.
await check("a queue that could not be built keeps its rows, dimmed, and offers the way out", async () => {
  await page.evaluate(() => { loadReview(true); });
  await page.waitForFunction(() => !revLoading && revQueue && (revQueue.prs || []).length, null, { timeout: 15000 });
  const rows = await page.evaluate(() => {
    revQueue = { ...revQueue, error: "401 Bad credentials", remembered: true };
    renderReview();
    return document.querySelectorAll("#revpane .revrow").length;
  });
  if (!rows) throw new Error("the failure replaced the queue that was on screen");
  const box = await mustSee("#revpane .revfail", "the failure box");
  const said = (await box.textContent()).replace(/\s+/g, " ");
  if (!/401 Bad credentials/.test(said)) throw new Error(`it does not say what GitHub said: ${said}`);
  if (!/last read/.test(said)) throw new Error(`it does not say the rows are the remembered copy: ${said}`);
  await mustSee("#revpane .revfail .revchip:has-text('try again')", "the retry");
  await mustSee("#revpane .revfail .revchip:has-text('GitHub')", "the setting that would fix it");
  // The 55% is load-bearing: it is how you tell what you are looking at is not live without
  // reading anything.
  //
  // Queried and measured in ONE page task, for `rowGeometry`'s reason in row.mjs (SKEIN-751):
  // `$eval` is a query and then an evaluate, and a reading landing between them re-renders the
  // pane, so the second half is handed a lane no longer in the document — whose computed opacity is
  // "", which parsed as NaN and failed this check under load (SKEIN-1030).
  const dim = await page.evaluate(() => {
    const lane = document.querySelector("#revpane .revlane");
    return lane ? parseFloat(getComputedStyle(lane).opacity) : "no lane on screen";
  });
  if (!(dim < 1)) throw new Error(`the remembered rows are drawn as though they were live: ${dim}`);
  // Put the pane back, so this check costs the ones after it nothing.
  await page.evaluate(() => { openReview(""); loadReview(true); });
  await settle(600);
});

console.log("\nan expanded row");
// Seven checks stood between here and "one row failing", and five of them were about the vetting
// panel — the keep/drop surface over a review skein had drafted and was holding for a reader to
// post. They are gone with it, and named here because each was bought by a live report:
//
//   * **"a drafted review opens in the row it belongs to" (SKEIN-264)** — "posting comments button
//     doesn't work, they aren't responsive even if something is happening in the background."
//   * **"a draft of an earlier commit is still postable, and says what will happen" (SKEIN-244)** —
//     the server re-anchored by line text, so a moved head had to offer the post rather than
//     refuse it and restart the treadmill.
//   * **"a browser that refuses every dialog can no longer swallow the press"** — a browser where
//     somebody once ticked "prevent this page from creating additional dialogs" answers every later
//     `confirm` with a synchronous false, and the post evaporated.
//   * **"one control reads and drafts, and warns only where vetting would be lost" (SKEIN-293)** —
//     the press replaced a draft, so where the reader had kept, dropped or edited comments it said
//     so first. The half of it that is about the READ press — one control, asking
//     `/read?redraft=1`, held and undoable — is asserted in "a reading of an older commit offers its
//     re-read on the line" above, which drives the same button through the same DOM.
//   * **"opening a drafted review is instant and offline" (SKEIN-286)** — "when I clicked go
//     through 2 comments and post it started saying `reviewing the code… this reads the whole diff,
//     so it can take a few minutes`", because the panel discarded the draft the page was already
//     holding and re-fetched it.
//
// Two more went with SKEIN-273's control: **"the same approval is offered, and acknowledged, with
// the panel closed"** and **"skein's review can be approved WITH, from the block that shows it"**.
// skein holds no review to approve WITH; approving is the plain verdict on the row, and it is
// pressed and read below.
//
// And **"the receipt appears even with text selected in the panel, and the review posts"** is gone
// with the panel's post control. It guarded a real rule — §6 focus rule 2 defers `renderReview`
// while a SELECTION is live over the pane (measured in chromium: a click leaves `activeElement` on
// the button and kills a caret, but a selection survives the press), so a press whose receipt goes
// through a deferred render shows nothing to the reader who had highlighted a phrase before
// pressing. `revPendingPaint` forces `renderReviewNow` for exactly this reason. Nothing in this
// file drives a verdict with a live selection any more, and the rule is now covered only by the
// forcing call being read out of the source in tests/ui/review_return.mjs.
//
// The two checks below are the ones that survive: they were about the ROW, and merely happened to
// press a control that lived in the drafted-review block.
let openKey = null;
await check("an expanded row is opened on a pull request that is your move", async () => {
  await page.evaluate(() => { openReview(""); });
  await page.waitForFunction(() => revQueue && (revQueue.prs || []).some(p => p.lane === "needs-you"),
    null, { timeout: 20000 });
  openKey = await page.evaluate(() => {
    const pr = (revQueue.prs || []).find(p => p.lane === "needs-you");
    const key = pr.repo_id + "#" + pr.number;
    // **Set, not toggled, and not conditionally toggled either.** `toggleRevRow` closes a row that
    // is already open and opens exclusively otherwise, so "toggle unless it is open" depends on
    // exactly which row the checks above left open — and when that changed, this check failed
    // with the WRONG ROW expanded and a message about a CSS rule hiding a strip. The state this
    // needs is "this row, open, nothing else", which is what the pane's own exclusivity means, so
    // it is written down rather than arrived at.
    revOpen = new Set([key]);
    revStackOpenKey = null;
    revStackStep = null;
    renderReviewNow();
    return key;
  });
  // **Let the layout land before asserting on a box.** `renderReviewNow` returns having written the
  // DOM; whether the strip has a rectangle yet is the browser's business, and `mustSee` reports a
  // laid-out-but-not-yet-measured element as "in the DOM but not visible — a CSS rule is hiding
  // it", which sent me looking for a deleted stylesheet rule that never existed.
  //
  // One frame was the whole of this wait, and one frame is not enough on a machine running four
  // browser suites on four cores — which is precisely what CI does (`browser_suites::lanes`). It
  // failed there with that same CSS sentence, about the strip that was on its way (SKEIN-621). The
  // frame stays because it is the cheap common case; `mustSee` waits for the rectangle after it.
  await page.evaluate(() => new Promise(requestAnimationFrame));
  await mustSee(`#revpane .revrow.open[data-rk="${openKey}"] .revrowacts`,
    "the expanded row's control strip");
});
// **ONE read control on screen, counted where a person sees it** (SKEIN-372).
//
// The owner reported this once already — "reread the code and review the code are still 2 different
// buttons (they do the same thing, why are they different?)" — and SKEIN-335 removed the second door
// that had a different NAME without removing the second door. Measured again on his fleet with #20
// expanded: two buttons, both visible, both labelled exactly "read it again", both calling
// `revReadAgainPress`. One in the row's control strip, one at the end of the drafted-review
// section. His decision: keep the strip's, delete the other.
//
// The drafted-review section is gone, and with it the copy that was the offending second door — but
// the count is still the assertion, because the row draws `revReadAgainPress` from three places
// (`revRow`'s line control, `revDetail`'s unread branch and stale note, `revBody`'s strip) and any
// two of them visible at once is the same defect in another costume.
//
// **Why this assertion is in the browser and not in a node suite.** `tests/ui/conversation.mjs`
// counts `revReadAgainPress` in the source of ONE FUNCTION (`grab("revBody")`) and passed the whole
// time both buttons were on screen, because the other one is drawn by a different function. The
// thing a reader meets is a rendered row, so the count has to be of visible controls in one.
//
// **The state is written down, because "exactly one" is only a question in a state that offers
// one.** A row that is freshly read, not stale, and got its review back is entitled to offer NO
// re-read — `revReadAgain` returns "" for exactly that case — so counting on whatever state the
// fixture happened to leave asks "is it one?" of a row whose right answer is zero. This check
// passed for a year only because the fixture's mirror was broken and every summary came back
// unread; repairing the mirror turned it red without anything about the product changing.
//
// Stale and open, then: the one state in which a re-read must be offered, and offered once.
await check("an expanded row offers exactly one way to read it again", async () => {
  await page.evaluate(k => {
    revOpen = new Set([k]);
    revStackOpenKey = null;
    revStackStep = null;
    const s = revSums.get(k);
    revSums.set(k, { ...(s && s !== "…" ? s : { number: 0, depth: "expanded" }), stale: true, head_sha: "older" });
    renderReviewNow();
  }, openKey);
  await page.evaluate(() => new Promise(requestAnimationFrame));
  const controls = await page.evaluate(k => {
    const row = document.querySelector(`#revpane .revrow.open[data-rk="${CSS.escape(k)}"]`);
    if (!row) return null;
    return [...row.querySelectorAll("button")]
      .filter(b => (b.getAttribute("onclick") || "").includes("revReadAgainPress"))
      .filter(b => { const r = b.getBoundingClientRect(); return r.width > 0 && r.height > 0; })
      // The class and where it sits, not only the label: three different functions can draw this
      // control, and knowing WHICH two are on screen is the whole of the diagnosis.
      .map(b => `${b.textContent.replace(/\s+/g, " ").trim()} [${b.className}] in .${(b.closest("div") || {}).className || "?"}`);
  }, openKey);
  if (controls === null) throw new Error("the row is not expanded, so this would prove nothing");
  if (controls.length !== 1)
    throw new Error(`${controls.length} read controls on the expanded row: ${JSON.stringify(controls)}`);
});

// **WHICH SURFACE CARRIES THE ONE CONTROL — the question `revWantsRead` exists to answer, and the
// one nothing asked** (SKEIN-666).
//
// `revWantsRead` has exactly one reader: the chip in `.revrowacts`, drawn only when the answer is
// falsy, because when it is anything else the body already carries the move beside the sentence
// that explains it. Forced always-truthy AND forced always-falsy, this whole file stayed green —
// 94 of 94 — along with every node suite that reaches a row. Two doors and no door were the same
// colour, in both directions, which is the shape of a function no test reads.
//
// **Why the check above cannot bite on it, measured rather than assumed.** By the time that one
// runs the row is PENDING, so `.revrowacts` draws `revReceiptHtml` instead of the chip strip and
// the branch holding the chip is never evaluated. Its innerHTML in that state is the receipt span
// alone — `✓ commented — commented` — and the single control it counts comes from `.revstale`,
// which `revWantsRead` does not decide. It asserts a true thing about a state that cannot answer
// this question.
//
// So: clear the receipt, and assert WHERE the one control sits under each answer the function can
// give. The count alone is not enough — it is 1 in both states — and the location is the whole of
// what the function decides.
//
// **What makes each of these fail.** Forcing `revWantsRead` to return a truthy string empties the
// fresh row's strip, so the first sees 0. Forcing it to return `""` gives the stale row a second
// door beside `.revstale`, so the second sees 2. Both were run with the page rebuilt each time —
// it is `include_str!`-embedded, so an edit reaches no browser until `cargo build` — and each was
// seen to fail for its own reason before this was believed.
for (const [state, summary, anchor] of [
  ["has been read and is not stale", { number: 0, depth: "expanded", stale: false }, "revrowacts"],
  ["is stale", { number: 0, depth: "expanded", stale: true, head_sha: "older" }, "revstale"],
]) {
  await check(`an expanded row that ${state} offers one read control, in .${anchor}`, async () => {
    await page.evaluate(([k, sum]) => {
      revOpen = new Set([k]);
      revStackOpenKey = null;
      revStackStep = null;
      // An act in flight replaces the whole strip with its receipt, and a receipt carries no read
      // control — so a pending row cannot answer this question whatever `revWantsRead` says.
      revPending.delete(k);
      revSums.set(k, sum);
      renderReviewNow();
    }, [openKey, summary]);
    await page.evaluate(() => new Promise(requestAnimationFrame));
    const controls = await page.evaluate(k => {
      const row = document.querySelector(`#revpane .revrow.open[data-rk="${CSS.escape(k)}"]`);
      if (!row) return null;
      return [...row.querySelectorAll("button")]
        .filter(b => (b.getAttribute("onclick") || "").includes("revReadAgainPress"))
        .filter(b => { const r = b.getBoundingClientRect(); return r.width > 0 && r.height > 0; })
        .map(b => (b.closest("div") || {}).className || "?");
    }, openKey);
    if (controls === null) throw new Error("the row is not expanded, so this would prove nothing");
    if (controls.length !== 1 || !String(controls[0]).split(/\s+/).includes(anchor))
      throw new Error(
        `expected exactly one read control, in .${anchor}, but the row's are ${JSON.stringify(controls)}`);
  });
}

// **A FAILED READING IS STATED ONCE ON THE ROW A PERSON OPENED** (SKEIN-400).
//
// `unread_because` reached one open row three times over: cut to the gist column's width on the
// line, in full again in the body's "Not summarised — …", and a third time as that same line span's
// `title` — a tooltip repeating the text it was sitting on. This is the two-doors defect the read
// control on this very line already refuses (`revReadAgain` returns "" when the row is open,
// because "an open row draws the same act in its body"), one field along: the sentence, not the
// button.
//
// What each of the three is FOR is the whole question, and the answers are not the same. The line
// is the SCAN surface — one truncatable run, down a column of 29 rows. The body is where the whole
// sentence belongs, beside the move it implies. And a `title` on this page says what the visible
// text could not: `.mv`'s words for a glyph, `.revnum`'s repo behind a bare number, the `+N` chip's
// hidden remainder. So the reason is stated at whichever surface can hold it: the body when the row
// is open, the line when it is not — and, collapsed, its title, because `ai::Unread::say` writes
// 150–250 characters whose CURE is at the end ("…or set SKEIN_CLAUDE_BIN to its full path") and
// roughly 45 of them fit the column. An error cut there keeps its complaint and loses its fix.
//
// **Counted where a person sees it, and in the tooltips too.** The same rule as "an expanded row
// offers exactly one way to read it again" above: `conversation.mjs` counts a call in one
// function's source and cannot see a second copy another function draws. The tooltips are counted
// beside the text because a "fix" that moved the duplicate out of the text and into a `title=`
// would have changed nothing whatever for the reader.
//
// Fails on: `revGist` drawing `unread_because` on an open row — the defect, exactly; the body
// dropping it, which would leave the whole sentence on no surface at all; and the line's title
// coming back on an open row, which is the third copy in its original costume.
const REV400_WHY = "skein could not start `claude` (No such file or directory). It is on the PATH "
  + "of the process running skein-server, not yours — start skein-server from a shell that has it, "
  + "or set SKEIN_CLAUDE_BIN to its full path.";
// What the row held before this pair borrowed it, so the stack checks below meet the queue they
// were written against rather than one carrying a synthetic failure.
let rev400Was = null;
const rev400Unread = open => page.evaluate(([k, why, isOpen]) => {
  const s = revSums.get(k);
  revSums.set(k, { ...(s && s !== "…" ? s : { number: 0 }), depth: "unread",
    // Not the budget refusal: that branch draws a different body, with a different move in it.
    budget_stopped: false, stale: false, unread_because: why });
  revOpen = isOpen ? new Set([k]) : new Set();
  revStackOpenKey = null;
  revStackStep = null;
  renderReviewNow();
}, [openKey, REV400_WHY, open]);
await check("an open row states its failed reading once, and states the whole of it", async () => {
  rev400Was = await page.evaluate(k => {
    const s = revSums.get(k);
    return s && s !== "…" ? s : null;
  }, openKey);
  await rev400Unread(true);
  await page.evaluate(() => new Promise(requestAnimationFrame));
  // The row's own line must still mark the absence where the queue is scanned — a fix that simply
  // deleted the cell would satisfy "once" and lose the column's invariant (silence reads as
  // reassurance), so this is asserted as something SEEN before anything is counted.
  const mark = await mustSee(`#revpane .revrow.open[data-rk="${openKey}"] .gist.unknown`,
    "the open row's stated absence");
  const said = (await mark.textContent()).trim();
  if (said !== "not read")
    throw new Error(`the open row's line is not the bare state mark: ${JSON.stringify(said)}`);
  const seen = await page.evaluate(([k, why]) => {
    const row = document.querySelector(`#revpane .revrow.open[data-rk="${CSS.escape(k)}"]`);
    if (!row) return null;
    const shown = e => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
    const name = e => "." + (e.className || e.tagName);
    return {
      // The element that OWNS the text, not every ancestor of it: a text node's parent is the one
      // place the sentence is drawn, and counting ancestors would count the pane itself.
      visible: [...row.querySelectorAll("*")].filter(e => shown(e)
        && [...e.childNodes].some(n => n.nodeType === 3 && n.textContent.includes(why))).map(name),
      titles: [...row.querySelectorAll("[title]")]
        .filter(e => shown(e) && (e.getAttribute("title") || "").includes(why)).map(name),
    };
  }, [openKey, REV400_WHY]);
  if (seen === null) throw new Error("the row is not expanded, so this would prove nothing");
  if (seen.visible.length !== 1 || seen.titles.length)
    throw new Error(`the reason is on the open row ${seen.visible.length + seen.titles.length} times`
      + ` — text in ${JSON.stringify(seen.visible)}, tooltips on ${JSON.stringify(seen.titles)}`);
  if (seen.visible[0] !== ".revnosum")
    throw new Error(`the one statement is not the body's, which is where the move is: ${seen.visible[0]}`);
});
// The other half of the same decision, and the reason the tooltip is kept rather than deleted with
// the duplicate: collapsed, the line is the ONLY place this sentence is, and the column cuts it.
//
// Fails on: the `title` dropped from the collapsed gist (the reader loses the cure and cannot get
// it back without opening the row), the title carrying anything short of the server's whole
// sentence, or the cell ceasing to truncate — which would make the tooltip the duplication this
// pair is about, and is the one input that would retire it.
await check("a collapsed row keeps within reach the sentence its column cuts", async () => {
  await rev400Unread(false);
  await page.evaluate(() => new Promise(requestAnimationFrame));
  const gist = `#revpane .revrow[data-rk="${openKey}"] .gist.unknown`;
  await mustSee(gist, "the collapsed row's stated absence");
  // One page task, for the reason spelt out at "each row says in words why it needs you"
  // (SKEIN-751): `scrollWidth` and `clientWidth` are both 0 on a node a repaint has detached, so a
  // read that straddled one would report `cut: false` and blame the column for no longer cutting a
  // sentence it is still cutting.
  const got = await page.evaluate(s => {
    const e = document.querySelector(s);
    return e && {
      text: e.textContent.trim(),
      title: e.getAttribute("title") || "",
      // What a person can actually read of it: the cell is `overflow:hidden; text-overflow:ellipsis`,
      // so this is the gap between the sentence and the column.
      cut: e.scrollWidth > e.clientWidth,
    };
  }, gist);
  if (!got) throw new Error("the collapsed row's stated absence left the row before it was read");
  if (!got.text.startsWith("not read — ")) throw new Error(`the line does not carry it: ${got.text}`);
  if (!got.cut)
    throw new Error("the column no longer cuts this sentence, so the tooltip is now a duplicate");
  if (got.title !== REV400_WHY)
    throw new Error(`hover does not reach the whole of what was cut: ${JSON.stringify(got.title)}`);
  await page.evaluate(([k, was]) => {
    if (was) revSums.set(k, was); else revSums.delete(k);
    revOpen = new Set();
    renderReviewNow();
  }, [openKey, rev400Was]);
});

// **A box that did not answer in time leaves the next spend to the reader** (SKEIN-818).
//
// The server stops rather than spend the same budget again outside the box, and says so in
// `ai::Unread::BoxSlow`'s sentence with `stopped_at_box` beside it. The row's job is the owner's
// wording, verbatim — `Not read — ` and the sentence — and both ways on: the box again, or here
// instead. "read it here instead" must ask for exactly that, `here=1`, which the server reads as
// a forced, reviewed reading on skein's own disk. The start is answered with a refusal here so no
// reading is spent; what is asserted is what the press ASKED for.
//
// Fails on: the `stopped_at_box` branch dropped from `revDetail` (the generic "Not summarised — …"
// with one button comes back), either button missing, or the press not sending `here=1`.
const BOXSLOW_WHY = "its box did not answer within 15m, and skein stopped there rather than spend "
  + "the same again reading it outside the box.";
await check("a box that ran out of time offers the box again or here instead, and presses neither", async () => {
  const was = await page.evaluate(k => {
    const s = revSums.get(k);
    return s && s !== "…" ? s : null;
  }, openKey);
  await page.evaluate(([k, why]) => {
    const s = revSums.get(k);
    revSums.set(k, { ...(s && s !== "…" ? s : { number: 0 }), depth: "unread",
      budget_stopped: false, stopped_at_box: true, stale: false, unread_because: why });
    revOpen = new Set([k]);
    revStackOpenKey = null;
    revStackStep = null;
    renderReviewNow();
  }, [openKey, BOXSLOW_WHY]);
  const body = `#revpane .revrow.open[data-rk="${openKey}"] .revnosum`;
  await mustSee(body, "the open row's unread body");
  const got = await page.$eval(body, e => ({
    text: e.firstChild ? e.firstChild.textContent.trim() : "",
    chips: [...e.querySelectorAll(".revacts .revchip")].map(b => b.textContent.trim()),
  }));
  if (got.text !== "Not read — " + BOXSLOW_WHY)
    throw new Error(`the row does not say the approved sentence: ${JSON.stringify(got.text)}`);
  if (JSON.stringify(got.chips) !== JSON.stringify(["read it again", "read it here instead"]))
    throw new Error(`the row does not offer both ways on: ${JSON.stringify(got.chips)}`);
  let asked = "";
  await page.route("**/review/*/read*", route => {
    asked = route.request().url();
    return route.fulfill({ status: 409, body: "stand-in: no reading is spent by this check" });
  });
  try {
    await page.click(`${body} .revchip:has-text('read it here instead')`);
    for (let i = 0; i < 50 && !asked; i++) await new Promise(r => setTimeout(r, 50));
  } finally {
    await page.unroute("**/review/*/read*");
  }
  if (!/[?&]here=1(&|$)/.test(asked) || !/[?&]redraft=1(&|$)/.test(asked))
    throw new Error(`"read it here instead" did not ask for a reading here: ${JSON.stringify(asked)}`);
  await page.evaluate(([k, w]) => {
    if (w) revSums.set(k, w); else revSums.delete(k);
    revOpen = new Set();
    renderReviewNow();
  }, [openKey, was]);
});

// **SKEIN-284's actual shape**: the row that had no feedback was inside a STACK.
//
// `revStackSteps` draws a stacked pull request as a `.step`, and the `.revrow` around it carries
// the STACK's key — so `revRepaintRow`'s `.revrow[data-rk=…]` matched nothing and the press
// repainted nothing at all. Every check above passes because they all drive LOOSE rows; the owner's
// fleet is one 18-step stack, and every row they can press is a step. Their words, about the
// control that used to be here — "approve with this review button doesn't really have feedback. So
// when I click idk if it went through or not". That control is gone; the press is now the reader's
// own approve, in the step's control strip, and the rule it proves is unchanged.
//
// The stack is built out of the fixture's own pull requests, by basing one on another's branch —
// which is exactly what `revChains` reads (`base_ref` → `head_ref`), so this is the real stack
// renderer and not a stand-in for it.
await check("a verdict pressed on a stack step is acknowledged where it was pressed", async () => {
  const built = await page.evaluate(k => {
    const all = revQueue.prs || [];
    const step = all.find(p => (p.repo_id + "#" + p.number) === k);
    // Any other pull request in the same repo, brought into the same lane: a stack is a chain of
    // `base_ref` → `head_ref` within one lane, and this fixture has a single your-move row.
    const other = all.find(p => p !== step && p.repo_id === step.repo_id && p.lane !== "archived");
    if (!step || !other) return { why: `rows: ${all.length}, step ${!!step}, other ${!!other}` };
    other.lane = "needs-you";
    other.snoozed = false;
    // The opened row is the ROOT; the other is based on its branch, which is what makes a chain.
    other.base_ref = step.head_ref;
    revOpen = new Set();
    // No act in flight on the step: a strip that is already a receipt offers no verdict to press,
    // and this check is about the press. Written down rather than inherited, for the reason the
    // check above gives about `revOpen`.
    revPending.delete(k);
    renderReviewNow();
    const stackKey = [...revStacks.keys()][0];
    if (!stackKey) return { why: `no stack formed; filter=${revRepoFilter}, search=${revSearch}` };
    toggleRevStack(stackKey);
    toggleStackStep(k);
    return { stackKey };
  }, openKey);
  try {
    if (!built || !built.stackKey)
      throw new Error(`the fixture would not form a stack, so this check would prove nothing: ${built && built.why}`);
    await settle(200);
    const chip = "#revpane .revrow.stack .revrowacts .revchip.go";
    await mustSee(chip, "the approve control inside the opened stack step").catch(async e => {
      const seen = await page.evaluate(() => ({
        stacks: document.querySelectorAll("#revpane .revrow.stack").length,
        steps: document.querySelectorAll("#revpane .step").length,
        acts: [...document.querySelectorAll("#revpane .revrowacts")].map(a => a.textContent.replace(/\s+/g, " ").trim().slice(0, 120)),
      }));
      throw new Error(`${e.message} — the pane holds ${JSON.stringify(seen)}`);
    });
    await page.click(chip);
    // The receipt appearing where the press was made IS the subject of this check (SKEIN-284), so
    // it is what the press is waited for: 200ms decided it on the owner's report of a press with no
    // feedback, which is precisely the failure it would be reporting.
    await until(() => /✓ approved/.test(
      document.querySelector("#revpane .revrow.stack .revrowacts")?.textContent || ""), null,
      async () => `a press on a stack step left no receipt where it was pressed: ${
        ((await page.textContent("#revpane .revrow.stack .revrowacts").catch(() => "(no strip)")) || "")
          .replace(/\s+/g, " ").slice(0, 300)}`);
    const held = (await page.textContent("#revpane .revrow.stack .revrowacts")).replace(/\s+/g, " ");
    if (!/undo/.test(held)) throw new Error(`a held verdict with no way back: ${held}`);
    await page.click("#revpane .revrow.stack .revrowacts .revchip:has-text('undo')");
    await settle(200);
  } finally {
    // Put the queue back the way the checks below expect it — loose rows, no stack — whatever
    // happened above. A check that leaves the pane rearranged on FAILURE reports its own bug three
    // times, in the two checks after it as well as in itself.
    await page.evaluate(k => {
      for (const p of revQueue.prs || []) p.base_ref = "main";
      revStackOpenKey = null; revStackStep = null;
      revPending.delete(k);
      revOpen = new Set([k]);
      renderReviewNow();
    }, openKey);
    await settle(400);
  }
});

console.log("\none row failing");
// SKEIN-268, reported by the owner: "any small error anywhere in the review page just blanks the
// entire page and gives the error." The pane is one string assigned in one shot, so a throw in any
// of the helpers that string calls meant the assignment never ran. Injected here into the REAL
// `revRow`, in the real page, because the claim is about what a person is left looking at.
await check("a row that throws leaves the rest of the queue drawn and clickable", async () => {
  await page.evaluate(() => openReview(""));
  await page.waitForFunction(() => revQueue && (revQueue.prs || []).length >= 2, null, { timeout: 20000 });
  const target = await page.evaluate(() => {
    const pr = (revQueue.prs || [])[0];
    const key = pr.repo_id + "#" + pr.number;
    const real = window.revRow;
    window.__realRevRow = real;
    window.revRow = p => {
      if (p.repo_id + "#" + p.number === key) throw new TypeError("cannot read properties of undefined (reading 'map')");
      return real(p);
    };
    renderReview(true);
    return key;
  });
  const rows = await page.$$eval("#revpane .revrow", els => els.length);
  if (rows < 2) throw new Error(`the pane kept ${rows} rows — one throw took the queue with it`);
  const broken = await mustSee("#revpane .revrow.broken", "the row that could not be drawn");
  const said = (await broken.textContent()).replace(/\s+/g, " ");
  if (!said.includes(target)) throw new Error(`the broken row does not say which PR it is: ${said}`);
  if (!/cannot read properties of undefined/.test(said))
    throw new Error(`the reason is not in the page: ${said}`);
  // Still a queue you can work: a healthy row expands on click, and the keyboard still walks the
  // full list — a row dropped from `revNav` would shorten j/k for as long as the fault lasted.
  const healthy = await page.evaluate(k =>
    (revNav || []).find(x => x !== k && !revOpen.has(x)), target);
  if (!healthy) throw new Error("no healthy, closed row survived to click");
  await page.click(`#revpane .revrow[data-rk="${healthy}"] .revline`);
  await until(k => revOpen.has(k), healthy,
    async () => `clicking a healthy row did nothing: open=${await page.evaluate(() => [...revOpen])}`);
  const inNav = await page.evaluate(k => (revNav || []).includes(k), target);
  if (!inNav) throw new Error("the broken row left the keyboard's list, silently shortening j/k");
  // And the fault reached the person, rather than devtools.
  const toast = (await page.textContent("#toast").catch(() => "")) || "";
  if (!/something went wrong in the page/.test(toast))
    throw new Error(`nothing said a row had failed: ${JSON.stringify(toast)}`);
});
// The fault clears the way a real one does — the data stops being malformed — and the row comes
// back rather than staying broken until a reload.
await check("and the next render puts the row back", async () => {
  await page.evaluate(() => { window.revRow = window.__realRevRow; renderReview(true); });
  await settle(300);
  if (await page.$("#revpane .revrow.broken")) throw new Error("the row stayed broken after the fault cleared");
  const rows = await page.$$eval("#revpane .revrow", els => els.length);
  if (rows < 2) throw new Error(`the queue did not come back: ${rows} rows`);
});
