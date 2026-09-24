// The review pane's browser suite, the keyboard: what each key does, what it refuses, and what it
// leaves alone.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import { check, page, pressRow, until } from "./setup.mjs";

console.log("\nthe keyboard");
// SKEIN-151/159, docs/parity.md §3 Zero bindings before this, on a surface used thirty times
// a day — and with boxes present the fleet's keys were worse than dead: `j` moved a selection
// BEHIND the pane and `↵` navigated out of review entirely, which is data-loss-shaped with a
// composer open. What is asserted here is the browser half; the table's own routing lives in
// `cockpit/test/keys.test.mjs`.

/** The rk of whatever the keyboard has selected, as the pane paints it. */
const selectedRk = () => page.$eval("#revpane .revrow.sel, #revpane .step.sel", e => e.dataset.rk)
  .catch(() => null);

/** Press a key that MOVES the selection, and answer with where it moved to.
 *
 *  **A keyboard-driven selection move is an observable DOM change, so it is awaited as one**
 *  (SKEIN-801). `revKeySelect` takes the `sel` class off one row and puts it on another, right
 *  there in the handler and without a render (src/web/index.html:6547) — so the painted selection
 *  IS the keypress having been handled, and there is nothing else for a beat to have been waiting
 *  for. What stood here was a bare read after the press in one check and a fixed 200ms in its
 *  neighbour; under a full `cargo test --all` with the tier and several gate runs beside it, the
 *  read came back before the handler had run and the check reported "j did not move" about a
 *  keypress that was still in flight.
 *
 *  `was` is where the selection is now, so this cannot be satisfied by the selection it started
 *  from: `null` means "anywhere at all", which is the first press of the section. */
const selectionAfter = async (key, was) => {
  await page.keyboard.press(key);
  await until(w => {
    const el = document.querySelector("#revpane .revrow.sel, #revpane .step.sel");
    return !!el && el.dataset.rk !== w;
  }, was, async () => was === null
    ? `${key} selected nothing — the pane has no visible selection`
    : orTheQueueMoved(`${key} did not move the selection: it is still ${JSON.stringify(await selectedRk())}`));
  return selectedRk();
};
/** Press a key that must put the selection on a NAMED row, and say which row it reached instead. */
const selectionReaches = async (key, want, why) => {
  await page.keyboard.press(key);
  await until(w => {
    const el = document.querySelector("#revpane .revrow.sel, #revpane .step.sel");
    return !!el && el.dataset.rk === w;
  }, want, async () => orTheQueueMoved(`${why}: it is on ${JSON.stringify(await selectedRk())}`));
};
/** The keys' navigable list, as the pane rebuilt it at the last render. */
const navNow = () => page.evaluate(() => [...(revNav || [])]);
/** The list the keyboard section started on, taken once the section's first press has landed. */
let navAtTheStart = null;
/** `sentence`, and — if the queue moved underneath while the keys were walking it — that instead.
 *
 *  **The two failures a walk can have are not the same finding.** j and k move within `revNav`, and
 *  every render rebuilds that list: a row arriving, leaving or changing lane moves the selection on
 *  its own (`revNavSettle`, src/web/index.html:3771). So "k did not come back to the row j left" is
 *  true both of a broken `k` and of a row that had gone while it walked — and only the first is
 *  this check's subject. Seen once here in eleven runs, where the same run also drew a pane with no
 *  rows in it two sections later: the queue had reloaded under the section, and the message blamed
 *  the keyboard. This does not make either outcome green; it makes them different sentences. */
const orTheQueueMoved = async (sentence) => {
  const now = await navNow();
  if (!navAtTheStart || JSON.stringify(now) === JSON.stringify(navAtTheStart)) return sentence;
  return `${sentence} — and the queue moved under the keyboard while it walked: `
    + `${JSON.stringify(navAtTheStart)} → ${JSON.stringify(now)}, so the row it was walking is not `
    + "the row it started on";
};

/** Press a key that must change NOTHING, and wait until the page has HANDLED it.
 *
 *  An assertion about an absence needs the event to have happened, or it is answered by a keypress
 *  still in flight — which is a check that cannot fail rather than one that fails late, and the
 *  worse of the two (CONTRIBUTING, rule 3). There is no DOM change to wait for by definition, so
 *  what is waited for is the page's own listeners having run over the event: this counter is a
 *  `keydown` listener on `document` registered AFTER the page's (src/web/index.html:10716), and
 *  listeners on one target run in registration order — nothing in the page calls
 *  `stopImmediatePropagation` (`grep -c stopImmediatePropagation src/web/index.html` → 0), so
 *  nothing can take an event away from this one that the page itself has not already seen. */
await page.evaluate(() => {
  window.__keysHandled = 0;
  document.addEventListener("keydown", () => { window.__keysHandled++; });
});
const pressAndBeHandled = async (key) => {
  const before = await page.evaluate(() => window.__keysHandled);
  await page.keyboard.press(key);
  await until(n => window.__keysHandled > n, before,
    `${key} never reached the page's own key handling at all`);
};

await check("j selects a row, and the selection is visible", async () => {
  await selectionAfter("j", null);
  navAtTheStart = await navNow();
  const lit = await page.$$eval("#revpane .revrow.sel", els => els.length);
  if (lit !== 1) throw new Error(`${lit} rows are lit at once; a selection is one row`);
});
await check("j and k walk it, and k at the top stays at the top", async () => {
  const first = await selectedRk();
  const second = await selectionAfter("j", first);
  if (second === first) throw new Error(`j did not move: still ${second}`);
  // Back to the row j left, named — not merely "somewhere else", which a `k` that walked past it
  // would also satisfy.
  await selectionReaches("k", first, "k did not come back to the row j left");
  // The one press here that must do nothing, so it is the one press that is waited for rather than
  // watched: a selection that has not moved because the key has not arrived is the same reading as
  // a selection the page correctly refused to move.
  await pressAndBeHandled("k");
  if ((await selectedRk()) !== first) {
    throw new Error(await orTheQueueMoved("k walked off the top of the queue"));
  }
});
// SKEIN-151's done-when, and the reason this work is not cosmetic. Dispatched synchronously so no
// 2s fleet poll can land between the seed and the reading: what runs is the page's own global
// keydown listener, on a fleet that has a selection to lose.
await check("no keypress in the pane changes the fleet selection behind it", async () => {
  const said = await page.evaluate(() => {
    boxes = [{ name: "box-a", state: "idle" }, { name: "box-b", state: "idle" }];
    order = ["box-a", "box-b"];
    sel = "box-a";
    const before = sel, mode = view.mode;
    for (const k of ["j", "k", "Enter", "d", "]", "l", "}"]) {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }));
    }
    const said = { before, after: sel, mode, modeAfter: view.mode,
                   tabs: document.querySelectorAll("#tabs .tab").length, opened: revOpen.size };
    // ↵ opened the row HERE, which is the other half of the same rule; fold it again so the checks
    // below are looking at a plain queue.
    for (const k of [...revOpen]) toggleRevRow(k);
    return said;
  });
  if (!said.opened) throw new Error("↵ did not open the selected row");
  if (said.after !== said.before)
    throw new Error(`the fleet selection moved behind the pane: ${said.before} → ${said.after}`);
  if (said.modeAfter !== said.mode)
    throw new Error(`a keypress navigated out of review: ${said.mode} → ${said.modeAfter}`);
  if (said.tabs) throw new Error(`↵ opened a terminal from inside the review pane (${said.tabs} tabs)`);
});
// §6's first focus rule, in a real browser: the selection is a PR number, so the summaries still
// landing under it move it nowhere. The measured failure was an index-based selection drifting off
// the row you are looking at every time one arrived.
//
// **The summaries are MADE to land, and the landing is waited for** (SKEIN-833). What stood here
// was a wait for one `.gist` with `.catch(() => {})` around it — so a reading already on screen from
// the load fifty checks ago satisfied it in a frame, and a reading that never came satisfied it too
// — followed by `settle(600)`. Both ends fail the wrong way: this is a negative assertion, so a
// window nothing arrives inside reports the rule holding rather than reporting that nothing
// arrived. The selection was then trivially where it had been, because nothing had landed under it.
//
// `revKnownHeard` is the page's own record of the bulk row payload arriving — cleared at the
// request, added in the answer's `.then`, one entry per repo (index.html:4363, :4380) — and it is
// the same fact `revPumpSummaries` waits on rather than a mark invented for this check.
await check("the selection survives the summaries landing under it", async () => {
  const before = await selectedRk();
  if (!before) throw new Error("nothing is selected, so there is no selection for a summary to move");
  const repos = await page.evaluate(() => {
    const ids = ((revQueue || {}).queues || []).map(q => q.repo_id);
    revKnownHeard.clear();
    for (const id of ids) loadKnownSummaries(id);
    return ids;
  });
  if (!repos.length) throw new Error("the queue names no repo to ask for readings — nothing could land");
  await page.waitForFunction(n => revKnownHeard.size >= n, repos.length, { timeout: 20000 })
    .catch(async () => {
      const heard = await page.evaluate(() => [...revKnownHeard]);
      throw new Error(`the readings never landed — ${JSON.stringify(heard)} of `
        + `${JSON.stringify(repos)} answered, so nothing arrived under the selection`);
    });
  // And they are DRAWN: `loadKnownSummaries` ends in a render, and a render is what moved an
  // index-based selection. An assertion made before it would be about nothing.
  await page.waitForFunction(
    () => [...document.querySelectorAll("#revpane .gist")].some(e => /parser/.test(e.textContent)),
    null, { timeout: 20000 })
    .catch(() => { throw new Error("no reading is drawn in the queue, so nothing landed under the selection"); });
  const after = await selectedRk();
  if (after !== before) throw new Error(`a summary landing moved the selection ${before} → ${after}`);
});
// §6's one-line assertion, verbatim — the whole of SKEIN-159's done-when, in the browser it was
// measured in: focused:"rev-compose",caret:4 → focused:BODY,caret:0 on every re-render.
await check("a half-typed comment survives renderReview()", async () => {
  await pressRow("null deref");
  // **The row this check is about, open** — which is what the beat here was standing in for, and
  // did not assert: `.revrow.open` below is whichever row happens to be open when the click lands,
  // so an early click drove a different row's composer and a late one drove none (SKEIN-801's
  // family). `page.click` and `page.fill` wait for their own elements, so the two beats that
  // followed are gone rather than converted.
  await until(() => {
    const row = document.querySelector("#revpane .revrow.open");
    return !!row && /null deref/.test(row.textContent || "");
  }, null, async () => "pressing the null-deref row did not open it: the open row is "
    + `${JSON.stringify(await page.$eval("#revpane .revrow.open .revtitle", e => e.textContent.trim()).catch(() => null))}`);
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('ask')");
  await page.fill("#rev-compose", "half a thought");
  const said = await page.evaluate(() => {
    const el = document.activeElement;
    const before = { id: el.id, start: el.selectionStart };
    renderReview();                                   // a summary lands, the stale re-poll fires
    const now = document.activeElement;
    return { before, after: { id: now.id, start: now.selectionStart }, text: (now.value || "") };
  });
  if (said.after.id !== "rev-compose")
    throw new Error(`focus died on re-render: ${said.before.id} → ${said.after.id}`);
  if (said.after.start !== said.before.start)
    throw new Error(`the caret moved on re-render: ${said.before.start} → ${said.after.start}`);
  if (said.text !== "half a thought") throw new Error(`what was typed did not survive: ${said.text}`);
  await page.click("#revpane .revcompose .revchip:has-text('cancel')");
  await until(() => !document.querySelector("#revpane .revcompose"), null,
    "cancel left the composer on screen");
});
await check("and the row it was typed in is still the only one open", async () => {
  const open = await page.$$eval("#revpane .revrow.open", els => els.length);
  if (open !== 1) throw new Error(`${open} rows are open — expansion is exclusive (§2.5)`);
  await page.click("#revpane .revrow.open .revline");   // leave the queue collapsed for what follows
  // Waited for, because "collapsed" is what the checks below are handed: ↵ toggles, so one row left
  // open here is a later check pressing ↵ to CLOSE a row and then asserting on whichever other row
  // was open (SKEIN-802's lesson about inherited state, one section along).
  await until(() => !document.querySelector("#revpane .revrow.open"), null,
    "the row would not fold, so every check below this one starts on a different queue");
});
await check("m is unbound, and nothing happens when it is pressed", async () => {
  const before = await page.evaluate(() => document.querySelectorAll("#revpane .revreceipt").length);
  // An absence, so the press is waited for rather than the absence: see `pressAndBeHandled`.
  await pressAndBeHandled("m");
  const after = await page.evaluate(() => ({
    receipts: document.querySelectorAll("#revpane .revreceipt").length,
    dialog: !!document.querySelector("dialog[open]"),
  }));
  if (after.receipts !== before) throw new Error("m started an act — merge must be chip-only");
  if (after.dialog) throw new Error("m opened the merge confirm — the key must not exist at all");
});
/** What the toast says, for the refusals below. */
const toastSaid = () => page.$eval("#toast", e => e.textContent).catch(() => "");
/** Empty the toast, so that what the NEXT key puts there is that key's answer and not the one
 *  before it still on screen. `toast()` rewrites `innerHTML` unconditionally
 *  (src/web/index.html) — it does not dedupe — so a cleared toast that fills again was filled by
 *  the press being waited for. Without this, `[` is checked against a sentence `]` left behind. */
const clearToast = () => page.evaluate(() => {
  const t = document.getElementById("toast");
  if (t) t.textContent = "";
});
await check("a refuses out loud rather than approving, and names where approve lives", async () => {
  await clearToast();
  await page.keyboard.press("a");
  // The refusal IS what `a` does, so it is what the press is waited for. A fixed beat here read the
  // toast early on a busy box and reported that a key had said nothing (SKEIN-801's shape).
  await until(() => /approve is a chip on the row/i.test(
    document.getElementById("toast")?.textContent || ""), null,
    async () => `a said nothing about why it did not approve: ${JSON.stringify(await toastSaid())}`);
  const receipts = await page.$$("#revpane .revreceipt");
  if (receipts.length) throw new Error("a approved from a keystroke");
});
// The reading view's own keys outlived it (CKP-7): `c` and `r` drafted on a hunk, `]` and `[`
// walked files. They stay bound so they do not fall through to the fleet map behind the pane, which
// means each is a key that arrives and does nothing unless it answers. A bare `return;` in any of
// these four cases has to fail here rather than pass quietly — which used to rest on the toast the
// PREVIOUS check left on screen, and rested with it on a fixed beat. **The toast is emptied before
// each press instead**, which is both halves at once: a silent key leaves it empty and fails, and
// the sentence a key does write can be waited for rather than sampled. `]` and `[` want the same
// sentence, so without the clear the second of them was checked against the first's answer.
await check("the reading view's orphaned keys say where the thing they addressed went", async () => {
  for (const [key, want] of [
    ["c", /comment… is a chip on the row/i],
    ["r", /request changes… is a chip on the row/i],
    ["]", /does not show the diff/i],
    ["[", /does not show the diff/i],
  ]) {
    await clearToast();
    await page.keyboard.press(key);
    await until(w => new RegExp(w.source, w.flags).test(
      document.getElementById("toast")?.textContent || ""), { source: want.source, flags: want.flags },
      async () => `${key} said ${JSON.stringify(await toastSaid())}, wanted ${want}`);
  }
  const receipts = await page.$$("#revpane .revreceipt");
  if (receipts.length) throw new Error("one of them started an act — all four are refusals");
});
await check("/ puts the caret in the queue's own search", async () => {
  await page.keyboard.press("/");
  await until(() => /revsearch/.test(document.activeElement?.className || ""), null,
    async () => `/ did not focus the search: ${JSON.stringify(
      await page.evaluate(() => document.activeElement?.className || ""))}`);
  await page.keyboard.press("Escape");
  // **The caret leaving is waited for, and it is not housekeeping.** While the search box holds
  // focus every key below this goes into it as TEXT rather than to the pane — `j` types a `j` — and
  // `revRenderHeld` defers every render nobody asked for on top of that. A beat that expired first
  // on a busy box handed the next check a queue that does not answer its keyboard.
  await until(() => !/revsearch/.test(document.activeElement?.className || ""), null,
    "esc did not take the caret out of the search, so the keys below go into it as text");
});
await check("e sets a row aside on the hold, and u takes it back", async () => {
  // The selection is what `e` acts on, so the press is waited for rather than the move: a `j` that
  // cannot go further is still a `j` this check can work with, and a `j` that has not arrived is not.
  await pressAndBeHandled("j");
  const aside = await selectedRk();
  await page.keyboard.press("e");
  // The row greying IS the act — `e` holds it and repaints that one row (`revPendingPaint`), so
  // there is nothing else the old 400ms could have been waiting for.
  await until(k => {
    const el = document.querySelector(`#revpane .revrow[data-rk="${k}"]`);
    return !!el && el.classList.contains("held");
  }, aside, "the row did not grey in place — e must not rebuild the pane");
  const held = await page.evaluate(() => document.getElementById("toast")?.textContent || "");
  if (!/u undoes/.test(held)) throw new Error(`the receipt does not name the key back: ${held}`);
  await page.keyboard.press("u");
  await until(k => !!document.querySelector(`#revpane .revrow[data-rk="${k}"]:not(.held)`), aside,
    "u did not take the held act back");
});
// ↵ opens the row and esc folds it again. The verdicts live in the expansion (`revVerdictHtml`),
// so the rule survives the reading view that used to carry it: no verdict from a surface that is
// not showing you the change — and `a` above refuses because a keystroke is never that surface.
await check("↵ opens the selected row, and the verdicts are in it", async () => {
  // ↵ opens whatever is SELECTED, so the selection has to have moved before it is pressed: ↵ on a
  // selection j has not reached yet opens a different row, and this check then reads that row's
  // chips and passes on the wrong one.
  await pressAndBeHandled("j");
  await page.keyboard.press("Enter");
  await page.waitForSelector("#revpane .revrow.open .revrowacts", { timeout: 20000 });
  const chips = await page.$$eval("#revpane .revrow.open .revrowacts .revchip",
    els => els.map(e => e.textContent.trim()));
  for (const want of ["approve", "request changes…", "comment…", "merge"]) {
    if (!chips.includes(want)) throw new Error(`the open row does not offer ${want}: ${JSON.stringify(chips)}`);
  }
});
await check("esc folds the row and the selection stays where it was", async () => {
  const was = await selectedRk();
  // ↵ toggles, and the check above left this row open — so open it only if something folded it.
  if (!await page.$("#revpane .revrow.open")) {
    await page.keyboard.press("Enter");
    await page.waitForSelector("#revpane .revrow.open", { timeout: 20000 });
  }
  await page.keyboard.press("Escape");
  await until(() => !document.querySelector("#revpane .revrow.open"), null,
    "esc did not fold the row");
  const now = await selectedRk();
  if (now !== was) throw new Error(`the selection moved on the way out: ${was} → ${now}`);
});
// The absences are only deliberate if they are stated. A key sheet missing `m` reads exactly like
// a key sheet that forgot it.
await check("the key sheet states the three deliberate absences", async () => {
  await page.evaluate(() => openSettings("keys"));
  // **The sheet being on screen and WRITTEN**, rather than half a second of hoping it is. Three
  // conditions and not one, because `#set-keys` is static markup — it is in the document before
  // anything is opened (src/web/index.html:1865) and only filled when the pane draws (:9336), so a
  // wait for the element alone is a wait that cannot fail, which is worse than the beat it replaces.
  await until(() => {
    const pane = document.querySelector(".set-pane[data-pane='keys']");
    const sheet = document.getElementById("set-keys");
    return !!pane && pane.classList.contains("on")
      && !!sheet && (sheet.textContent || "").trim().length > 0;
  }, null, "the key sheet never opened, or opened empty");
  const text = await page.$eval("#set-keys", e => e.textContent);
  if (!/unbound/.test(text) || !/base branch/.test(text))
    throw new Error("the sheet does not say that merge is unbound, or why");
  if (!/not showing you the change|open the row first|refused here/.test(text))
    throw new Error("the sheet does not say why a does nothing in the queue");
  if (!/g\s*h|GitHub/.test(text)) throw new Error("the sheet does not name g h as the way to GitHub");
  await page.evaluate(() => closeSettings());
  // `closeSettings` takes the `open` class off `#settings` and nothing else
  // (src/web/index.html:9281, and `settingsModal` is that element) — the sheet's own markup stays in
  // the document, which is why the close is asked of the modal rather than of the sheet.
  await until(() => !document.getElementById("settings")?.classList.contains("open"), null,
    "the settings sheet would not close, and it is over the pane every check below reads");
});

// The reload path, found by the keyboard section above and fixed in `openReview`: `view.repo` is
// `"*"` while the queue shows every repo, `restoreSessions` hands that saved view back on the next
// snapshot, and read as a repo id it matches nothing — a header, an empty lane, and no rows.
await check("a restored view of every repo is every repo, not a repo named *", async () => {
  await page.evaluate(() => openReview("*"));
  // `openReview` starts a load; what this check reads is the queue that load draws, so it is the
  // load that is waited for. The 600ms here was the whole difference between "the restored queue is
  // empty" meaning the sentinel poisoned the filter — the bug — and meaning the answer had not
  // arrived yet.
  await page.waitForFunction(() => !revLoading && revQueue && (revQueue.prs || []).length,
    null, { timeout: 20000 });
  const said = await page.evaluate(() => ({
    filter: revRepoFilter, stored: localStorage.getItem("skein.reviewRepo"),
    rows: document.querySelectorAll("#revpane .revrow").length,
  }));
  if (said.filter !== "") throw new Error(`the sentinel became a filter: ${JSON.stringify(said)}`);
  if (said.stored === "*") throw new Error("and it was written back to the store, poisoning reloads");
  if (!said.rows) throw new Error(`the restored queue is empty: ${JSON.stringify(said)}`);
});
