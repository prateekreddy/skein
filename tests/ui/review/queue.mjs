// The review pane's browser suite, the queue as a whole: the badge, opening the pane, one list for
// both roles, a queue that says what it could not see, and the filter.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import { authHeader, check, fold, fx, laneHead, laneTitles, mustSee, page, port, pressFilter, refreshQueue, settle, unfold, until } from "./setup.mjs";

console.log("\nthe badge");
// The queue is only useful if you learn a PR is waiting without going looking. Polled slowly, so
// this waits for the first tick rather than assuming it has already happened.
await check("the count reaches the button without opening the pane", async () => {
  await page.waitForFunction(() => document.querySelector("#revbtn .revbadge"), null, { timeout: 15000 });
  const badge = await mustSee("#revbtn .revbadge", "the review badge");
  const n = (await badge.textContent()).trim();
  // Three, and they are the three rows the your-move list below is asserted to hold: #1 unreviewed,
  // #3 approved and re-requested, and #6 — the one YOU opened with a thread open on it. The draft
  // is NOT here (it is counted in the not-ready fold, where nothing hides), and neither is #4, your
  // own pull request with nothing outstanding but red checks. This count is the whole point of
  // SKEIN-139: the badge says what you can act on now, not everything with your name near it.
  //
  // **It was two until SKEIN-323**, and #6 was the missing one: the poll behind `/api/review/counts`
  // answered with `Lane::NeedsYou`, which is the reviewer's lane, and a pull request you opened is
  // never in that lane however stuck it is. So the button read 2 while the list under it held 3,
  // and opening the pane silently corrected the number. The rows now travel on the poll and
  // `yourMoveCount` folds the pane's own rule over them, which is why this assertion — taken before
  // anything is clicked — is the one that proves it.
  if (n !== "3") throw new Error(`expected the three your-move PRs, got ${JSON.stringify(n)}`);
});
await check("and its tooltip names the repo the count came from", async () => {
  const title = await page.$eval("#revbtn", e => e.title);
  if (!/acme: 3/.test(title)) throw new Error(`the breakdown is missing: ${title}`);
});
// Turning a repo off must stop skein asking GitHub about it — while still saying that is why there is
// nothing to show. It used to vanish from the counts entirely, which made "nothing needs you" and
// "skein never looked" the same empty badge; someone whose only repo had its queue off saw a clean
// board with no way to find out why.
await check("switching a repo's queue off silences it, and says so", async () => {
  const off = await fetch(`http://127.0.0.1:${port}/api/repos/acme/settings`, {
    method: "POST", headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify({ review_queue: false }),
  });
  if (!off.ok) throw new Error(`the setting was refused: ${await off.text()}`);
  const counts = await (await fetch(`http://127.0.0.1:${port}/api/review/counts`, { headers: authHeader() })).json();
  const acme = counts.find(c => c.repo_id === "acme");
  if (!acme) throw new Error(`the repo disappeared instead of reporting: ${JSON.stringify(counts)}`);
  if (!/switched off/.test(acme.skipped || "")) {
    throw new Error(`it must name why it was not looked at: ${JSON.stringify(acme)}`);
  }
  // Not polled, and not a fault: nothing was asked of GitHub, and a deliberate switch is not an error.
  if (acme.needs_you !== 0 || acme.error) throw new Error(`unexpected: ${JSON.stringify(acme)}`);
  // …and back on, because every check below this one needs the queue.
  await fetch(`http://127.0.0.1:${port}/api/repos/acme/settings`, {
    method: "POST", headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify({ review_queue: true }),
  });
});

console.log("\nopening");
await check("the header carries a way in", () => mustSee("#revbtn", "the review button"));
await check("clicking it opens a pane you can actually see", async () => {
  await page.click("#revbtn");
  await settle(900);
  await mustSee("#revpane.on .revwrap", "the review pane");
});
// The queue must dock without a box: this is the one view in the dock that is repo-scoped, and
// `applyView` gates rendering on `docked`.
await check("the dock opens even though no box is running", async () => {
  const docked = await page.evaluate(() => document.body.classList.contains("docked"));
  if (!docked) throw new Error("body is not .docked, so the pane has nowhere to render");
});

console.log("\none list, both roles");
// SKEIN-300/302. The top of the pane used to be `Lane::NeedsYou` — the REVIEWER's question — so a
// pull request the owner had opened was "waiting" by definition however stuck it was, and the one
// screen built to answer "what needs me" could not answer it about half their work. It is now ONE
// list mixing both roles, with the two groups under it folded to a count.
await check("an unreviewed PR is your move", async () => {
  const titles = await laneTitles("yours");
  if (!titles) throw new Error("there is no 'your move' list on screen");
  if (!titles.some(t => t.includes("null deref"))) throw new Error(`not in your-move: ${JSON.stringify(titles)}`);
});
await check("and so is a PR YOU opened, when a review thread is open on it", async () => {
  const titles = await laneTitles("yours");
  if (!titles.some(t => t.includes("tenant seam")))
    throw new Error(`your own blocked pull request is not in the list: ${JSON.stringify(titles)}`);
});
await check("each row says in words why it needs you, and the two roles read differently", async () => {
  const said = await page.$$eval("#revpane .revlane[data-lane='yours'] .revrow", els => els.map(e => ({
    title: e.querySelector(".revtitle")?.textContent.trim() || "",
    why: e.querySelector(".revwhy")?.textContent.trim() || "",
  })));
  const seam = said.find(r => r.title.includes("tenant seam"));
  const deref = said.find(r => r.title.includes("null deref"));
  if (seam?.why !== "1 thread unresolved")
    throw new Error(`your own row does not say why: ${JSON.stringify(seam)}`);
  if (deref?.why !== "review not given")
    throw new Error(`a review request does not say why: ${JSON.stringify(deref)}`);
  // The colour is read in the SAME page task that finds the element (SKEIN-751). `renderReview`
  // replaces `#revpane`'s whole innerHTML (src/web/index.html:4014) and several timers can fire it
  // while a check is mid-read; `getComputedStyle` of a node that is no longer in the document
  // answers "" for every property, so a read that straddles that repaint calls words that are on
  // screen invisible. That is what this check did on 3 of 7 four-lane runs.
  //
  // A locator did not close it, which is the part worth writing down. `mustSee` answers with one
  // (SKEIN-716), and a locator is re-resolved for an ACT — but `locator.evaluate` is TWO protocol
  // calls, `waitForSelector({state:"attached"})` and then `handle.evaluate` — `Locator._withElement`
  // in playwright-core 1.62.0, which is
  // `grep -n '_withElement' tests/ui/node_modules/playwright-core/lib/coreBundle.js` — and the
  // function is handed a node the repaint between those two calls has already detached.
  // `page.$eval` splits the same way. Against a page repainting on a 0ms timer, 400 reads each: `locator.evaluate` answered invisible 162 times, `page.$eval` 83, and
  // the form below 0. `mustSee` still does the waiting; it just no longer carries a node across
  // the gap, and a colour can now only be read off an element `document` handed over an instant
  // earlier — which is the property this check is actually about.
  const sel = "#revpane .revlane[data-lane='yours'] .revwhy";
  await mustSee(sel, "the why on a row");
  const colour = await page.evaluate(s => {
    const e = document.querySelector(s);
    return e && getComputedStyle(e).color;
  }, sel);
  if (colour === null) throw new Error("the why left the row between being seen and being read");
  if (!colour || colour === "rgba(0, 0, 0, 0)") throw new Error("the why is in the DOM and invisible");
});
// THE RULE MOST LIKELY TO BE "FIXED" BY SOMEBODY WHO HAS NOT READ IT (SKEIN-303). The owner,
// verbatim: "CI pass isn't your responsibility, that is of whoever merges — unless ci-queue tag is
// attached and it fails then… But this ci-queue thing is very specific to this repo. So I don't
// want to include that in generic workflow."
//
// #4 and #6 are both yours and both red. #6 is in the list because a thread is open on it; #4 has
// nothing open and must not be there. If this fails and the change that broke it taught the rule
// about `checks`, the change is wrong — the ci-queue behaviour arrives as repo configuration.
await check("a PR you opened that is only RED is never your move", async () => {
  const yours = await laneTitles("yours");
  if (yours.some(t => t.includes("store layout")))
    throw new Error(`a red pull request of yours was promoted by its checks: ${JSON.stringify(yours)}`);
  await unfold("theirs");
  const theirs = await laneTitles("theirs");
  if (!theirs.some(t => t.includes("store layout")))
    throw new Error(`it is not in waiting-on-others either — where did it go? ${JSON.stringify(theirs)}`);
  // …and it really is red, so the check above is about the rule and not about a missing rollup.
  const red = await page.$$eval("#revpane .revlane[data-lane='theirs'] .revrow", els => els.map(e => e.outerHTML));
  if (!red.some(h => h.includes("store layout"))) throw new Error("the row is not drawn at all");
});
await check("a PR you approved on its current head is waiting on others, not asking again", async () => {
  const titles = await laneTitles("theirs");
  if (!titles?.some(t => t.includes("retry flag"))) throw new Error(`not in waiting-on-others: ${JSON.stringify(titles)}`);
});
await check("waiting on others is a fold that states what it is made of", async () => {
  const said = await laneHead("theirs");
  if (!/you opened/.test(said || "")) throw new Error(`the group does not say its composition: ${said}`);
  // Closed again, so what follows sees the queue a reader opens on — and waited for, because the
  // fold not having landed inside a fixed beat is indistinguishable here from a heading that does
  // not fold at all, and the difference is a fact about the box (SKEIN-802).
  await page.click("#revpane .revlane[data-lane='theirs'] h4.revfold");
  await until(() => {
    const el = document.querySelector("#revpane .revlane[data-lane='theirs']");
    return !el || el.querySelectorAll(".revtitle").length === 0;
  }, null, "clicking the heading did not fold it back");
});
// The case the whose-move rule exists for. It used to be the head SHA that brought a row back;
// since SKEIN-354 it is GitHub's own re-request, because comparing commits took the owner's
// approval off him on any push at all — measured on his live queue, `review_is_current` was false
// on all 26 rows including the two he had approved himself.
await check("GitHub asking again brings it back", async () => {
  const titles = await laneTitles("yours");
  if (!titles.some(t => t.includes("default timeout")))
    throw new Error(`an approval GitHub re-requested did not return: ${JSON.stringify(titles)}`);
});
// …and it must be distinguishable from a PR you have never seen, on the collapsed line. Otherwise
// the two rows look identical at exactly the moment the difference matters.
await check("and the row says why it came back, without being opened", async () => {
  const rows = await page.$$eval("#revpane .revrow", els => els.map(e => ({
    text: e.querySelector(".revtitle")?.textContent.trim() || "",
    moved: !!e.querySelector(".revtag.moved"),
  })));
  const back = rows.find(r => r.text.includes("default timeout"));
  const fresh = rows.find(r => r.text.includes("null deref"));
  if (!back?.moved) throw new Error("a re-review carries no mark on its collapsed row");
  if (fresh?.moved) throw new Error("a PR you never reviewed is marked as having moved");
});

console.log("\nhonesty");
// **The queue this fixture serves is complete, and that is asserted rather than assumed.**
//
// `whole` travels on every per-repo queue in the merged payload (`prq::Queue::whole` in
// `src/prq/types.rs` → `revMergeQueues` keeps `m.queues` verbatim, src/web/index.html:3164), so this
// reads the page's own copy: the flag reaching the browser is what makes any behaviour keyed on it
// possible at all. Before the stub answered `/user/teams`, this was false on every queue in this
// file — and a page rule that fires on every test is indistinguishable from one that is wrong.
await check("the queue this fixture serves saw everything there was", async () => {
  const q = await page.evaluate(() => ((revQueue || {}).queues || []).find(x => x.repo_id === "acme"));
  if (!q) throw new Error("acme built no queue at all, so there is nothing to be whole");
  if (q.whole !== true)
    throw new Error("the stubbed GitHub did not answer wholly, so nothing here can tell a partial "
      + `queue from the fixture's own gap: ${JSON.stringify(q.blind_spots)}`);
});

// The `read:org` gap, driven rather than permanent. It used to be the fixture's only shape; the two
// checks below are the ones it is genuinely about, so they ask for it and hand it back.
fx.github.refuseTeams(true);
await refreshQueue();
await check("a queue that cannot see your teams says so, visibly", async () => {
  const el = await mustSee("#revpane .revblind", "the blind-spot banner");
  const t = (await el.textContent()).toLowerCase();
  if (!t.includes("team")) throw new Error(`the banner does not name what is missing: ${t}`);
  // A warning that cannot be acted on is shown for ever, so the cure travels in the sentence.
  if (!t.includes("gh auth refresh")) throw new Error(`it does not name the cure: ${t}`);
});
// SKEIN-164. The `read:org` gap is PERMANENT — true on every load until somebody runs that command
// — and it used to be drawn in exactly the treatment "the queue could not be built" uses. A
// constant in the alarm's clothes is what teaches the eye to skip the alarm, and the queue's
// willingness to shout is the best thing about it.
await check("a standing gap is amber and quiet; the alarm is kept for skein failing", async () => {
  const paint = await page.evaluate(() => {
    // The tokens themselves, resolved by the browser, so this compares what is drawn rather than
    // two spellings of the same hex.
    const tok = name => { const s = document.createElement("span"); s.style.color = `var(--${name})`;
      document.body.append(s); const c = getComputedStyle(s).color; s.remove(); return c; };
    const blind = getComputedStyle(document.querySelector("#revpane .revblind"));
    // The failure treatment is probed rather than provoked: this queue is healthy, and the point is
    // that the two are drawn differently, not that this run can produce a 401.
    const box = document.createElement("div");
    box.className = "revfail";
    document.getElementById("revpane").append(box);
    const fail = getComputedStyle(box);
    const out = {
      rule: blind.borderLeftColor, ruled: blind.borderLeftWidth, boxed: blind.borderTopWidth,
      failRule: fail.borderLeftColor, failBoxed: fail.borderTopWidth,
      waiting: tok("waiting"), error: tok("error"),
      alarms: document.querySelectorAll("#revpane .revfail").length - 1,
    };
    box.remove();
    return out;
  });
  if (paint.rule !== paint.waiting)
    throw new Error(`the standing gap is not amber: ${JSON.stringify(paint)}`);
  if (parseFloat(paint.boxed) !== 0 || parseFloat(paint.ruled) === 0)
    throw new Error(`the standing gap still wears a box rather than a rule: ${JSON.stringify(paint)}`);
  if (paint.failRule !== paint.error || parseFloat(paint.failBoxed) === 0)
    throw new Error(`the failure treatment lost its orange box: ${JSON.stringify(paint)}`);
  if (paint.alarms) throw new Error("a standing condition is drawn as a failure");
});
// Whole again for everything below, and restored OUT HERE rather than at the end of a check: an
// assertion that throws would otherwise leave every remaining check in the file reading a partial
// queue, which is the state this work exists to get out of.
fx.github.refuseTeams(false);
await refreshQueue();

console.log("\nfilter");
await check("'mine' shows what you opened and hides what you did not", async () => {
  await pressFilter("author", "mine");
  // Both of yours are here, in the two different groups the rule puts them in — so the filter is
  // asserted across the split rather than only where the rows happen to be drawn.
  await unfold("theirs");
  const shown = await page.$$eval("#revpane .revtitle", els => els.map(e => e.textContent.trim()));
  if (!shown.some(t => t.includes("store layout"))) throw new Error("your own PR vanished");
  if (!shown.some(t => t.includes("tenant seam"))) throw new Error("your own blocked PR vanished");
  if (shown.some(t => t.includes("null deref"))) throw new Error("someone else's PR survived the filter");
});
await check("'all' brings everything back", async () => {
  await pressFilter("all", "all");
  await unfold("theirs");
  await unfold("not-ready");
  const shown = await page.$$eval("#revpane .revtitle", els => els.map(e => e.textContent.trim()));
  if (shown.length < 6) throw new Error(`expected all six PRs, saw ${shown.length}: ${JSON.stringify(shown)}`);
});
// **Folded back OUT HERE rather than at the end of the check above** (SKEIN-802), for the reason the
// teams seam is handed back out here a hundred lines up: a check that throws never reaches the rest
// of its own body, and these two groups being open is state every check below inherits. One
// `unfold` that came up empty under load left them open, and twenty later checks went red behind
// it — including the whole keyboard section, which walks `revNav` and so was walking a queue two
// rows longer than the one it was written against.
//
// `fold` is idempotent and says nothing when the group is not there, so this is safe wherever the
// check above got to; when a group is on screen and will not close, it fails HERE, naming the group
// that is stuck, rather than as twenty checks about something else.
await fold("theirs");
await fold("not-ready");
