// The review pane's browser suite, the merge a person presses and a refresh that did not see
// everything.
//
// One part of `tests/ui/review.mjs`, which runs its parts in order against the page `./setup.mjs`
// opened. Not a suite on its own: every part inherits the queue as the part before it left it, the
// way the single file this was cut from did.

import { check, fx, mustSee, page, refreshQueue } from "./setup.mjs";

console.log("\nthe merge a person presses");
// **A merge is the one press this pane cannot take back, so its confirmation is the last place a
// mistake can be caught** (SKEIN-365). `Merge #1?` told the reader the single thing they already
// knew — that they had pressed merge. The two facts it never carried are which commit lands and
// where it lands, and both are on the row the chip was drawn from.
//
// The commit is not decoration. It travels as `drafted_at`, and `prwork::merge_by_hand` refuses a
// merge whose branch has moved since — quoting back the sha it was handed. So a dialog naming one
// sha while another travels would produce a refusal about a commit the reader was never shown, and
// the checks below assert the two are the same value.
//
// **The presses go through the chip's own handler from `page.evaluate`.** The confirmation is a
// NATIVE dialog: `window.confirm` has to be replaced both to answer it and to read the question,
// and a press dispatched any other way is answered by Playwright's own dismissal instead. Each
// check finds the chip in the DOM first, so nothing here presses a control a reader could not.
const pressMerge = async (yes) => page.evaluate(say => {
  const chip = [...document.querySelectorAll("#revpane .revrowacts .revchip")]
    .find(e => e.textContent.trim() === "merge" && !e.disabled);
  if (!chip) throw new Error("no merge chip on the open row to press");
  const real = window.confirm;
  let asked = "";
  window.confirm = q => { asked = q; return say; };
  try { chip.click(); } finally { window.confirm = real; }
  return asked;
}, yes);
/** Wait for the act to stop being in flight, and hand back what became of it. */
const mergeOutcome = async () => {
  await page.waitForFunction(
    () => ["posted", "failed"].includes((revPending.get("acme#1") || {}).state),
    null, { timeout: 15000 });
  return page.evaluate(() => {
    const p = revPending.get("acme#1") || {};
    return { state: p.state, error: p.error || "", said: p.said || "" };
  });
};
await check("the merge confirmation names the commit on the row and the branch it lands on", async () => {
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await page.evaluate(() => toggleRevRow("acme#1"));
  await page.waitForSelector("#revpane .revrow.open .revrowacts .revchip", { timeout: 15000 });
  await mustSee("#revpane .revrow.open .revrowacts .revchip:has-text('merge')", "the merge chip");
  const shown = await page.evaluate(() => (revKeyPr("acme#1") || {}).head_sha || "");
  if (!shown) throw new Error("the row names no commit, so the question has nothing to quote");
  const asked = await pressMerge(false);
  if (!asked) throw new Error("pressing merge asked nothing at all");
  if (!asked.includes("#1")) throw new Error(`the question does not name the pull request: ${asked}`);
  if (!asked.includes(shown.slice(0, 7)))
    throw new Error(`the question does not name the commit on screen (${shown}): ${asked}`);
  if (!/into main\b/.test(asked))
    throw new Error(`the question does not say which branch it lands on: ${asked}`);
  // Answered "no", so nothing may have been sent: the dialog is a gate, not a notice.
  if (await page.evaluate(() => revPending.size))
    throw new Error("a refused confirmation started the merge anyway");
});
// The commit both checks below are about: the one the row named when the merge was pressed, read
// out of the page rather than written into it, so what is pinned is the value the product chose.
let READ_AT = "";
// The sha the question quotes is the sha that travels. A press that sent nothing at all would leave
// the server to fall back to `prq::remembered_head` — a commit the dialog never mentioned — so this
// pins the two together at the one moment they can be seen to agree: GitHub is moved to exactly the
// commit the row names, and only a request carrying that sha is accepted.
await check("a merge sends the commit the question named, and it goes through", async () => {
  const row = await page.evaluate(() => (revKeyPr("acme#1") || {}).head_sha || "");
  fx.github.moveTo(1, row);
  const asked = await pressMerge(true);
  if (!asked.includes(row.slice(0, 7)))
    throw new Error(`the question named a commit that is not the row's: ${asked}`);
  const out = await mergeOutcome();
  if (out.state !== "posted")
    throw new Error(`the merge of the commit on the row was refused: ${out.error}`);
  const acts = (await page.$eval("#revpane .revrowacts", e => e.textContent)).replace(/\s+/g, " ");
  if (!/merged/.test(acts)) throw new Error(`the row does not say it merged: ${acts}`);
  READ_AT = row;
});

// The same pull request merges again here, which no real GitHub would allow
// that far: `merge_by_hand` compares the live head against the sha it was sent and returns before
// any request, so what this drives is the check in front of the merge rather than the merge.
await check("and a branch that moved since you read it is refused, naming the commit you were shown", async () => {
  await page.evaluate(() => { revPending.clear(); renderReviewNow(); });
  // A push lands. The page has no idea: its row still names the commit the last poll saw.
  fx.github.moveTo(1, "deadbeef00112233");
  const asked = await pressMerge(true);
  const out = await mergeOutcome();
  if (out.state !== "failed")
    throw new Error(`a merge of a commit the branch has left went through: ${out.said}`);
  if (!/branch moved since you read it/.test(out.error))
    throw new Error(`the refusal is not in skein's words: ${out.error}`);
  // The sha in the refusal is the sha in the question. A page that sent nothing would be refused
  // too — quoting the queue's row, a commit the dialog never mentioned — and that is the failure
  // this line exists to tell apart from the fix.
  if (!out.error.includes(READ_AT.slice(0, 7)) || !asked.includes(READ_AT.slice(0, 7)))
    throw new Error(`the refusal and the question name different commits: asked ${asked} / said ${out.error}`);
  if (!out.error.includes("deadbee"))
    throw new Error(`the refusal does not say where the branch is now: ${out.error}`);
  const acts = (await page.$eval("#revpane .revrowacts", e => e.textContent)).replace(/\s+/g, " ");
  if (!/GitHub refused/.test(acts) || !/branch moved/.test(acts))
    throw new Error(`the refusal never reached the reader: ${acts}`);
});

console.log("\na refresh that did not see everything");
// **What the pane does today with an incomplete queue that found nothing.**
//
// Both seams at once: no membership search answers anything, and `/user/teams` is refused — so
// `queue_within` starts from `answered = !teams_unknown` false (`src/prq/refresh.rs`) and the queue
// arrives with `whole: false`. The pull requests behind that refusal are ABSENT, not known to be
// gone: a team could have asked you for a review and this refresh cannot say either way.
//
// These two checks assert what the page DOES, not what it should do. The calm screen's headline is
// decided in one place — `revUnasked` (src/web/index.html:3465) — and it reads `queues`, `failed`
// and `skipped`, never `whole`. Nothing else in the page reads it either: the single `.whole` in
// src/web/index.html (`grep -n "\.whole" src/web/index.html`) is a spelling picker at line 4504.
// So an empty partial queue is drawn byte for byte like an empty complete one, and these are the
// two sentences that would have to change.
fx.github.emptyQueue(true);
fx.github.refuseTeams(true);
// Every repo, no filter and no search: the calm screen is the answer `revLaneEmpty` reserves for
// exactly that (src/web/index.html:3438), and a filter or a search gets the plain line instead.
// The forced read is what carries the seams above onto the page — `openReview` re-asks the queue
// UNFORCED, and unforced is answered from the last whole queue skein remembered.
await page.evaluate(() => { openReview(""); setRevFilter("all"); revSearchSet(""); });
await refreshQueue();
await check("an empty queue that could not see everything still says nothing is waiting on you", async () => {
  const q = await page.evaluate(() => ((revQueue || {}).queues || []).find(x => x.repo_id === "acme"));
  if (!q || q.whole !== false)
    throw new Error(`the refresh came back whole, so this check is not about what it says: ${JSON.stringify(q)}`);
  if ((await page.evaluate(() => ((revQueue || {}).prs || []).length)) !== 0)
    throw new Error("pull requests survived the emptied searches, so the screen under test is not the empty one");
  const head = await page.$eval("#revpane .revclear-head", e => e.textContent.replace(/\s+/g, " ").trim());
  if (head !== "Nothing is waiting on you.")
    throw new Error(`the headline moved — this check pins what the page does today: ${JSON.stringify(head)}`);
  // What the reader IS told. The standing blind spot is still drawn above the headline, and it is
  // the only thing on this screen that contradicts it — amber prose under a positive claim.
  const blind = (await page.textContent("#revpane .revblind").catch(() => "")) || "";
  if (!/read:org/.test(blind))
    throw new Error(`nothing on screen says the queue was short: ${JSON.stringify(blind)}`);
});
await page.evaluate(() => openReview("acme"));
await refreshQueue();
await check("and with the one repo chosen it still says that repo is clear", async () => {
  const q = await page.evaluate(() => ((revQueue || {}).queues || []).find(x => x.repo_id === "acme"));
  if (!q || q.whole !== false)
    throw new Error(`the refresh came back whole, so this check is not about what it says: ${JSON.stringify(q)}`);
  const head = await page.$eval("#revpane .revclear-head", e => e.textContent.replace(/\s+/g, " ").trim());
  if (head !== "acme is clear.")
    throw new Error(`the scoped headline moved — this check pins what the page does today: ${JSON.stringify(head)}`);
});
// Whole and full again, so the screenshot at the bottom is of a queue rather than of this.
fx.github.emptyQueue(false);
fx.github.refuseTeams(false);
await page.evaluate(() => openReview(""));
await refreshQueue();
