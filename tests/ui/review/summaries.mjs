// The review pane's browser suite, what a row says about its pull request: the summary, the
// conversation, who still owes an approval, and a link a third party wrote.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import { HOSTILE_CHECK_URL, REDRAW_MS, check, find, fold, fx, mustSee, page, refreshQueue, settle, unfold, until } from "./setup.mjs";

console.log("\nsummaries");
// The gist is the product at thirty a day: the collapsed row has to say what the PR *is*, so that
// most of the queue never needs opening at all.
await check("a bug fix states itself on the collapsed row", async () => {
  await page.waitForFunction(
    () => [...document.querySelectorAll("#revpane .gist")].some(e => /parser/.test(e.textContent)),
    null, { timeout: 15000 });
  const gists = await page.$$eval("#revpane .gist", els => els.map(e => e.textContent.trim()));
  if (!gists.some(g => g.includes("crashing on empty input")))
    throw new Error(`no one-line summary on the row: ${JSON.stringify(gists)}`);
});
// Five checks stood here, in the browser, and all five were about a review skein HELD.
//
// **"a drafted review says so on the row, and opens beside the summary" (SKEIN-216)** proved the
// chip and the section: summary and review are one model call, one payload carries both, so the
// queue could say a review was waiting without a request of its own — where before, the only way to
// find out was to open a row and press "review the code…", once per row, on a queue of thirty. It
// checked the chip against `revCommonChips`'s demotion in the same breath, and that the chip had a
// CSS rule of its own (the `overlays.mjs` failure: markup, handlers and tests, and no paint).
//
// **The four under it (SKEIN-275)** were the absence beside it: eighteen read, seventeen drafted,
// one with a full summary and `critique: null`, and from the pane nothing at all — a row missing a
// chip its neighbours had. They pinned each of the three reasons a reading can carry no review, that
// the section stating an absence offers no VERDICT (§6: you cannot approve from a surface that is
// not showing you the change), and that a review nobody ever asked for says so rather than sounding
// pending.
//
// There is no drafted review on the row now, so there is no absence of one to state. The session
// posts its review to GitHub, and GitHub is where it is read. The row's own three helpers —
// `noDraftRow`, `setNoDraft`, `restoreNoDraft` — went with them; nothing else used them.
//
// What a row still says about a READING, which is a different thing and still skein's: the gist, its
// stated absence, and the re-read control, are asserted below and in tests/ui/review_return.mjs.


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
await check("a draft is not read unless you ask, and says so rather than looking failed", async () => {
  // Every non-draft in this lane has a gist by now (the check above waited for one). A draft that
  // was going to be read would have been read in the same pass.
  const rows = await page.$$eval("#revpane .revrow", els => els.map(e => ({
    title: e.querySelector(".revtitle")?.textContent.trim() || "",
    draft: !!e.querySelector(".revtag.draft"),
    gist: e.querySelector(".gist")?.textContent.trim() || "",
    unread: !!e.querySelector(".gist.unknown"),
  })));
  const wip = rows.find(r => r.title.includes("still moving things around"));
  if (!wip) throw new Error(`the draft is not in the lane at all: ${JSON.stringify(rows.map(r => r.title))}`);
  if (!wip.draft) throw new Error("the draft is not marked as one, so the rule cannot be seen either");
  // The gist is never empty now — an unrequested reading shows as the stated absence, not a line.
  if (!wip.unread || !wip.gist.startsWith("not read"))
    throw new Error(`a draft was read without being asked, or hides that it was not: ${wip.gist}`);

  // And opening it explains WHY rather than reading as a failure — three different reasons land in
  // that space and only one of them is a setting to change.
  await page.click(`#revpane .revrow:has-text("still moving things around") .revline`);
  // The body this check reads, rather than 300ms and then a `$eval` whose absence Playwright
  // reports as a missing element — which is the same sentence for "the row says nothing" and "the
  // row has not opened yet".
  await until(() => [...document.querySelectorAll("#revpane .revrow")]
    .some(r => /still moving things around/.test(r.textContent || "") && r.querySelector(".revnosum")),
    null, "opening the draft drew no stated reason");
  const said = await page.$eval(`#revpane .revrow:has-text("still moving things around") .revnosum`,
    e => e.textContent.trim());
  if (!/draft/i.test(said) || !/ready/i.test(said))
    throw new Error(`a draft must say it is being left alone until it is ready, got: ${said}`);
  // The way to have one anyway is right there.
  const button = await page.$(`#revpane .revrow:has-text("still moving things around") .revnosum .revchip`);
  if (!button) throw new Error("no way to ask for it by hand");
  await page.click(`#revpane .revrow:has-text("still moving things around") .revline`);
  await settle(200);
});

console.log("\nthe conversation");
// SKEIN-304. The owner: "I also want to see comment history so that convo is seen from here
// directly." PR-level comments only — "not keyed on lines… if they are inline comments then link
// out. If they are normal comments then just show it here and also link out." The pull request this
// drives carries an inline thread as well as the comment, so what it asserts is a comment's text
// reaching the screen out of a payload that holds both kinds.
await check("a PR-level comment's text is readable without leaving skein", async () => {
  await page.click(`#revpane .revrow:has-text("tenant seam") .revline`);
  await settle(600);
  await mustSee(`#revpane .revrow:has-text("tenant seam") .revconv`, "the conversation block");
  const said = await page.$eval(`#revpane .revrow:has-text("tenant seam") .revcomment-body`,
    e => e.textContent.trim());
  if (said !== "Can we ship this before Friday?")
    throw new Error(`the comment's text is not on screen: ${JSON.stringify(said)}`);
  const href = await page.$eval(`#revpane .revrow:has-text("tenant seam") .revcomment-head a`,
    e => e.getAttribute("href"));
  if (!/issuecomment-9$/.test(href || "")) throw new Error(`and it does not link out: ${href}`);
});
// Two checks stood here, and both were about inline review threads on the panel.
//
// **"an inline thread is who, when and a way to it — never its words"** held the asymmetry SKEIN-304
// was drawn around: a PR-level comment renders its TEXT (the check above), and a thread rendered
// only who opened it, when, the link out, and a tally of the resolved ones. It was the guard on the
// place a payload that grew comment bodies would start showing them on screen.
//
// **"a resolve pressed inside a stack gives its receipt on the thread's own line" (SKEIN-305)** was
// the only write SKEIN-300 granted on this panel. It was pressed on a STACKED row on purpose:
// `revStackSteps` draws a pull request inside a stack as a `.step` and the `.revrow` around it
// carries the STACK's key, so a repaint reaching for `.revrow` found nothing and the press did
// nothing visible at all — SKEIN-284, reported by an owner whose every open pull request is one
// 18-step stack: "when I click idk if it went through or not". Every test passed throughout,
// because the suite drove loose rows.
//
// The thread panel is gone. A thread is a conversation about a line of code and is worth nothing
// away from the line, so it is read and answered on GitHub, and skein neither draws one nor writes
// to one. The surgical-repaint rule that check was really about is alive on the reader's own
// verdict — "a verdict pressed on a stack step is acknowledged where it was pressed", below, presses
// on the same stacked row and counts the same whole-pane rebuilds.


console.log("\nwho still owes an approval"); // SKEIN-306
await check("the PR you opened names who is still to approve it", async () => {
  await unfold("theirs");
  await page.click(`#revpane .revrow:has-text("store layout") .revline`);
  await settle(600);
  const el = await mustSee(`#revpane .revrow:has-text("store layout") .revapprovals`, "the approvals line");
  const said = (await el.textContent()).replace(/\s+/g, " ").trim();
  if (!said.includes("waiting on @dana and the acme/core team"))
    throw new Error(`it does not name who is outstanding: ${said}`);
  // And it does NOT cry incomplete, because this queue saw the teams. `revTeamsBlind` reads the
  // queue's blind spots (`revTeamsBlind`, src/web/index.html:5822), so the note below is drawn on a condition —
  // and a roster that hedges when it has everything is the same lie facing the other way.
  if (/incomplete/.test(said))
    throw new Error(`a roster built on a whole queue still says it is short: ${said}`);
  await page.click(`#revpane .revrow:has-text("store layout") .revline`);
  await settle(300);
});
// SKEIN-262's gap, where a short list does real harm: without `read:org` a team asked to review
// arrives from GitHub with no slug and is dropped, so a roster read as whole is how somebody
// concludes an approval has landed that never will.
fx.github.refuseTeams(true);
await refreshQueue();
await check("and it says the list is short when skein could not see your teams", async () => {
  await unfold("theirs");
  await page.click(`#revpane .revrow:has-text("store layout") .revline`);
  await settle(600);
  const el = await mustSee(`#revpane .revrow:has-text("store layout") .revapprovals`, "the approvals line");
  const said = (await el.textContent()).replace(/\s+/g, " ").trim();
  if (!said.includes("incomplete") || !/read:org/.test(said))
    throw new Error(`a roster that could not see teams must say so: ${said}`);
  await page.click(`#revpane .revrow:has-text("store layout") .revline`);
  await settle(300);
});
fx.github.refuseTeams(false);
await refreshQueue();
await fold("theirs");


console.log("\nthe link a third party wrote"); // SKEIN-602
// **`esc` is not a URL guard, and a failing check's link is the one URL an outsider writes.**
//
// `esc` encodes `& < > " '`, which is exactly right for text and is not a judgement about a scheme:
// `javascript:alert(1)` contains none of those five characters and survives it byte for byte. So
// every anchor the page builds goes through `link`, which asks `safeHref` first and degrades to the
// link's own TEXT when the answer is no (src/web/index.html, `link`).
//
// These run in a browser rather than over the page source because the source cannot answer the
// question a reader has. `a.href` and `a.protocol` below are the URL PARSER's answer — entities
// decoded, base resolved — which is the string a click actually follows, and it is where both
// bypasses this guard has had were hiding. The fixture's hostile `detailsUrl` travels the whole way
// here: GraphQL answer → `prq::checks::failing_contexts` → `/api/review` → the row.
await unfold("theirs");
await page.click(`#revpane .revrow:has-text("store layout") .revline`);
// The meta line is what the four checks below read, and `$$eval` answers `[]` for a row that has
// not opened yet — which they report as "the https-linked check drew no anchor at all", a sentence
// about the guard written from a fact about the box.
await until(() => [...document.querySelectorAll("#revpane .revrow")]
  .some(r => /store layout/.test(r.textContent || "") && r.querySelector(".revmeta a")), null,
  "the store-layout row never opened onto its checks, so nothing below is about the link guard");
const META = `#revpane .revrow:has-text("store layout") .revmeta`;
const REFUSED = "deploy (staging)";
/** Every anchor in that row's meta line, as the browser parses it. */
const checkLinks = () => page.$$eval(`${META} a`, els =>
  els.map(a => ({ text: a.textContent.trim(), href: a.href, protocol: a.protocol })));

await check("a failing check linked with https is a live link to exactly that URL", async () => {
  const links = await checkLinks();
  const built = links.find(l => l.text === "build (nightly)");
  if (!built) throw new Error(`the https-linked check drew no anchor at all: ${JSON.stringify(links)}`);
  // Exactly, because a guard that quietly rewrites the good case is a guard nobody will keep.
  if (built.href !== "https://ci.example/1")
    throw new Error(`a link the guard should pass came out as ${JSON.stringify(built.href)}`);
});

await check("and one linked with javascript: is its name in plain text, with no anchor and no URL", async () => {
  const live = (await checkLinks()).find(l => l.text === REFUSED);
  if (live) throw new Error(`a javascript: check is still an anchor: ${JSON.stringify(live)}`);
  const meta = await page.$eval(META, e => ({ text: e.textContent, html: e.innerHTML }));
  // Refused is not dropped. The NAME is what the row is for — it says which check is red — and a
  // guard that took it away would let anyone delete a row's information by writing a scheme nobody
  // follows.
  if (!meta.text.includes(REFUSED))
    throw new Error(`the refused check lost its name along with its link: ${meta.text.trim()}`);
  // And the string itself never reaches the document. Escaped and inert is not the same as absent,
  // and absent is the one a later change cannot get wrong.
  if (/javascript:|__followedHostileCheck/i.test(meta.html))
    throw new Error(`the refused URL is in the row's markup after all: ${meta.html}`);
});

await check("no anchor anywhere in the pane has a scheme this page would follow nowhere", async () => {
  // The property, not the instance: eight sites build anchors and this asks all of them at once, so
  // a ninth added without the guard is caught by a check nobody had to remember to extend.
  const bad = await page.$$eval("#revpane a[href]", els => els
    .map(a => ({ protocol: a.protocol, href: a.href, text: a.textContent.trim().slice(0, 40) }))
    .filter(l => !["http:", "https:", "mailto:"].includes(l.protocol)));
  if (bad.length) throw new Error(`${bad.length} anchor(s) the browser would follow elsewhere: ${JSON.stringify(bad)}`);
});

await check("the same URL through `esc` alone does run, which is what the guard is for", async () => {
  // The control, and the reason the three above are not asserting against a browser that had
  // already made them true. If a `javascript:` href built the OLD way is inert in this chromium,
  // then nothing here is testing the guard, and this check says so instead of passing quietly.
  //
  // **The click and the answer are two turns, because the navigation is one.** Measured in this
  // chromium: reading the sentinel in the same evaluate as the click reports `false` even when the
  // URL does run — a `javascript:` navigation is queued, not performed inline — which is a control
  // that fails while the thing it is controlling for works perfectly. So the page is clicked, the
  // task is allowed to land, and only then is the question asked.
  await page.evaluate(url => {
    delete window.__followedHostileCheck;
    const host = document.createElement("div");
    host.id = "s602bait-host";
    // The shape the fix replaced, byte for byte — the page's own `esc`, quoting an attribute it
    // cannot judge. No `target`, deliberately: see the click check below for what a `_blank` one
    // does instead.
    host.innerHTML = `<a id="s602bait" href="${esc(url)}">bait</a>`;
    document.body.append(host);
    document.getElementById("s602bait").click();
  }, HOSTILE_CHECK_URL);
  // The sentinel is the navigation having run, so that is what the click waits for rather than a
  // 400ms beat. Caught rather than thrown: this check has its own sentence for a browser that never
  // runs it — the one that says the three checks above are then testing nothing — and it is a
  // better failure than a timeout.
  await page.waitForFunction(() => window.__followedHostileCheck === 1, null, { timeout: REDRAW_MS })
    .catch(() => {});
  const ran = await page.evaluate(() => {
    const ran = window.__followedHostileCheck === 1;
    document.getElementById("s602bait-host")?.remove();
    delete window.__followedHostileCheck;
    return ran;
  });
  if (!ran) throw new Error(
    "a `javascript:` href written with `esc` alone did not execute in this browser — so the checks " +
    "above are about a browser that refuses these anyway, and the page's guard is untested");
});

await check("and clicking where the refused check is drawn runs nothing and opens nothing", async () => {
  // **Both answers, because either one alone is a check that cannot fail here.**
  //
  // `link` writes `target="_blank"`, and measured in this chromium a `_blank` anchor whose href is
  // `javascript:…` opens an EMPTY popup and runs nothing in the opener — so with the guard deleted
  // the sentinel below stays undefined and a sentinel-only check would stay green over a live
  // hostile link. The popup is what changes, so the popup is asserted; the sentinel stays because
  // it is what changes if that browser behaviour ever does.
  const popups = [];
  const onPopup = p => popups.push(p.url());
  page.on("popup", onPopup);
  try {
    await page.evaluate(() => { delete window.__followedHostileCheck; });
    // The pixel a reader aims at, found from the text itself: a refused check has no element of its
    // own to address, which is the whole of what "degraded to its text" means. `$eval` rather than
    // `evaluate`, because `META` is a Playwright selector — `:has-text()` is not something
    // `document.querySelector` can parse, and asking it to throws rather than missing.
    const box = await page.$eval(META, (meta, name) => {
      const walk = document.createTreeWalker(meta, NodeFilter.SHOW_TEXT);
      for (let n = walk.nextNode(); n; n = walk.nextNode()) {
        const at = n.textContent.indexOf(name);
        if (at < 0) continue;
        const r = document.createRange();
        r.setStart(n, at);
        r.setEnd(n, at + name.length);
        const { x, y, width, height } = r.getBoundingClientRect();
        return { x, y, width, height };
      }
      return null;
    }, REFUSED);
    if (!box || !box.width) throw new Error(`${REFUSED} is not drawn anywhere a reader could click`);
    await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
    await settle(400);
    if (await page.evaluate(() => window.__followedHostileCheck === 1))
      throw new Error("clicking the refused check ran the URL a third party wrote");
    if (popups.length) throw new Error(`clicking the refused check opened ${JSON.stringify(popups)}`);
  } finally {
    page.off("popup", onPopup);
    await page.evaluate(() => { delete window.__followedHostileCheck; });
  }
});

// Put the row and the group back the way the checks below expect to find them. The click above may
// have toggled the row, so this asks rather than assumes.
if (await page.$(`#revpane .revrow:has-text("store layout") .revbody`)) {
  await page.click(`#revpane .revrow:has-text("store layout") .revline`);
  await settle(300);
}
await fold("theirs");
