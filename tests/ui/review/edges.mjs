// The review pane's browser suite, a row at its edges: edge states, an expanded row, and one row
// failing to draw.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import { REDRAW_MS, check, mustSee, page, settle, until } from "./setup.mjs";
import { tallestLine } from "./row.mjs";

console.log("\nedge states");   // SKEIN-154 — both were correct prose and inert as affordances
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
    // `moveOf`, not `lane`: what has to leave for this repo to be CLEAR is the your-move list, and
    // since SKEIN-302 that list is not the `needs-you` lane — a pull request you opened with a
    // thread open on it is in it, and leaving it behind leaves the queue non-empty.
    revQueue = { ...revQueue, prs: (revQueue.prs || []).map(p => {
      if (moveOf(p) !== "yours") return p;
      return { ...p, repo_id: "lattice" };
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
  // **No queue load may land while the stack exists only in this page** (SKEIN-1307). The stack is
  // an edit to `revQueue`, and any `loadReview` that lands afterwards puts the server's copy back
  // and takes the stack with it — a load already in flight, or the stale-answer chase, which
  // re-asks at 4s, 8s, 16s… after a `fresh: false` answer. Under the instrumented coverage run that
  // happened inside the wait below: "the pane holds {"stacks":0,…}" (run 39871111922). The same two
  // steps `refreshQueue` in setup.mjs takes before its own load, for the same reason.
  await page.waitForFunction(() => !revLoading, null, { timeout: 20000 });
  await page.evaluate(() => { clearTimeout(revStaleTimer); revStaleTimer = null; });
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
