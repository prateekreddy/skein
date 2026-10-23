// The board and a box's toolbar at the three widths people use them at — a phone (390px), a laptop
// (1280px) and a desktop (1600px) — and the board's own behaviour that a sweep of those widths found
// broken (SKEIN-1262, 1263, 1291, 1293, 1294, 1295, 1297, 1300).
//
// The server is real and its fixture is empty; the FLEET is not. `/api/boxes` and `/api/events` are
// answered by the page's own routes with four boxes chosen to be the awkward cases — a question too
// long for one line, a box signed out with nothing else to say, a working box and an idle one —
// because nothing in a fixture can make a real box be any of those. Everything else the page asks
// for goes to the server.
//
// Every claim here is a measurement of a page a person could be looking at: a rectangle, what is at
// a point, whether a dialog was raised, what reached the network. Each check names the change that
// would make it fail.
//
//   node tests/ui/layout.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";
const API_TOKEN = "skein-test-" + "l".repeat(53);

const root = freshFixture(fixtureRoot(), "ui-layout");
const home = path.join(root, "home");
const fleet = path.join(root, "fleet");
fs.mkdirSync(home, { recursive: true });
fs.mkdirSync(fleet, { recursive: true });
fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
fs.writeFileSync(path.join(home, "repos.json"), "[]");
fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");

const door = await openDoor();
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: { SKEIN_REGISTRY: path.join(root, "sandboxes.json"), SKEIN_HOME: home, SKEIN_FLEET_ROOT: fleet },
});
const base = `http://127.0.0.1:${door.port}`;

const QUESTION = "Should I drop the legacy fixtures table and migrate every row into the new schema now, "
  + "or keep both tables until the next release so the old readers keep working while we move them over?";
const BOXES = [
  { name: "web-feat-auth", repo: "web", branch: "feat/session-token-storage", state: "needs-input", tier: 0,
    age_secs: 120, agent: "claude", dir: "/w/web/feat-auth", diff: { files: 6, ins: 124, del: 18 },
    pause: "needs-input", headline: QUESTION, task: "Wiring the login flow" },
  // Signed out, and nothing else to say: no headline, no task — the row SKEIN-1291 is about.
  { name: "web-signed-out", repo: "web", branch: "fix/login", state: "blocked", tier: 0, age_secs: 60,
    agent: "claude", blocked_kind: "auth", pause: "none", headline: "", task: "" },
  { name: "web-main", repo: "web", branch: "master", state: "working", tier: 3, age_secs: 5, agent: "claude",
    dir: "/w/web/main", diff: { files: 9, ins: 312, del: 44 }, pause: "none", headline: "", task: "Refactoring the diff parser" },
  { name: "web-idle", repo: "web", branch: "spike/ocr", state: "idle", tier: 4, age_secs: 9000, agent: "claude",
    pause: "none", headline: "", task: "" },
];
const FILES = { path: "", entries: [
  { name: "docs", dir: true },
  { name: "a-really-long-file-name-that-goes-on-and-on-well-past-any-phone.generated.snapshot.json", size: 123456 },
  { name: "README.md", size: 2048 },
] };

const { check, value, report } = ledger();
const browser = await chromium.launch();

/** A page on the board at `w`×`h`, the fleet answered by this suite. `hold` delays the fleet's
 *  answers by that many ms; `touch` makes it a touch screen (`pointer: coarse`). */
async function board(w, h, { hold = 0, touch = false } = {}) {
  const context = await browser.newContext({ viewport: { width: w, height: h }, hasTouch: touch, isMobile: touch });
  const page = await context.newPage();
  const seen = erring(page, { say: (kind, text) => `${kind}: ${text}` });
  const later = () => new Promise(r => setTimeout(r, hold));
  await page.route("**/api/boxes", async r => { await later(); await r.fulfill({ json: BOXES }).catch(() => {}); });
  await page.route("**/api/events", async r => {
    await later();
    await r.fulfill({ contentType: "text/event-stream",
      body: `event: snapshot\ndata: ${JSON.stringify({ event: "snapshot", boxes: BOXES })}\n\n` }).catch(() => {});
  });
  await page.route("**/api/repos", r => r.fulfill({ json: [{ id: "web", source: "https://github.com/acme/web.git", work: "/w/web" }] }));
  await page.route("**/api/boxes/*/files*", r => r.fulfill({ json: FILES }));
  await page.route("**/api/boxes/*/settings", r => r.fulfill({ json: {} }));
  await page.goto(`${base}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  if (!hold) await page.waitForFunction(() => document.querySelectorAll("#fleet .row").length >= 4, null, { timeout: 15000 });
  return { page, context, ...seen };
}
const rect = (page, sel) => page.evaluate(s => {
  const e = document.querySelector(s); if (!e) return null;
  const r = e.getBoundingClientRect();
  return { left: Math.round(r.left), right: Math.round(r.right), top: Math.round(r.top), bottom: Math.round(r.bottom),
           width: Math.round(r.width), height: Math.round(r.height) };
}, sel);
/** Is `sel` the element a click at its own centre would land on? Not merely "in the DOM", and not
 *  merely "has a box": on screen, uncovered. */
const hittable = (page, sel) => page.evaluate(s => {
  const e = document.querySelector(s); if (!e) return "absent";
  const r = e.getBoundingClientRect();
  if (!r.width || !r.height) return "no box";
  const x = r.left + r.width / 2, y = r.top + r.height / 2;
  if (x < 0 || y < 0 || x > innerWidth || y > innerHeight) return `off screen at ${Math.round(x)},${Math.round(y)}`;
  const at = document.elementFromPoint(x, y);
  return at && (at === e || e.contains(at)) ? "yes" : `covered by ${at ? at.className || at.tagName : "nothing"}`;
}, sel);
const row = name => `#fleet .row[data-name="${name}"]`;

try {
  // --- SKEIN-1293: a slow board is "loading", never the first run --------------------------------
  {
    const { page, context } = await board(1280, 800, { hold: 6000 });
    // The health poll answers at once and redraws the board; that redraw used to be the first one, and
    // drew an empty fleet's first-run checklist over a fleet that simply had not been heard from yet.
    await page.waitForFunction(() => typeof lastHealth !== "undefined" && lastHealth !== null, null, { timeout: 10000 });
    await page.waitForTimeout(300);
    // **What would make this fail:** `render` drawing `firstRunHtml` for an empty `boxes` before any
    // snapshot (dropping the `fleetKnown` gate) — the checklist and "0 boxes" are back.
    value("before the fleet arrives, the board says it is loading — no first-run checklist, no '0 boxes'",
      await page.evaluate(() => ({ firstRun: !!document.querySelector("#fleet .fr"),
        loading: (document.querySelector("#fleet .empty")?.textContent || "").trim(),
        counts: document.getElementById("counts").textContent.trim() })),
      { firstRun: false, loading: "Loading the board…", counts: "loading…" });
    await page.waitForFunction(() => document.querySelectorAll("#fleet .row").length >= 4, null, { timeout: 15000 });
    value("and once it arrives, the boxes replace it", await page.evaluate(() => ({
      loading: !!document.querySelector("#fleet .empty"),
      counts: document.querySelector("#counts .seg")?.textContent.trim() })),
      { loading: false, counts: "4 boxes" });
    await context.close();
  }

  // --- the board at 1280: rows -------------------------------------------------------------------
  {
    const { page, context, errors, sayBlips } = await board(1280, 800);
    // SKEIN-1291. **What would make this fail:** toggling `has-head` on the headline text alone —
    // the chip is set inside a `.headline` that is `display:none`, and the row looks fine.
    value("a signed-out box with no headline shows its 'sign in' chip, on screen",
      { hit: await hittable(page, `${row("web-signed-out")} .pchip`),
        text: await page.evaluate(s => document.querySelector(s)?.textContent, `${row("web-signed-out")} .pchip`) },
      { hit: "yes", text: "sign in" });

    // SKEIN-1295. **What would make this fail:** no `title` on `.htext` — the full question is
    // nowhere on the page but the DOM.
    value("a long question can be read in full from its row",
      await page.evaluate(s => document.querySelector(s)?.title, `${row("web-feat-auth")} .htext`), QUESTION);
    const lines = await page.evaluate(s => {
      const e = document.querySelector(s);
      return Math.round(e.getBoundingClientRect().height / parseFloat(getComputedStyle(e).lineHeight || "16"));
    }, `${row("web-feat-auth")} .htext`);
    // **What would make this fail:** the needs-you headline left at one `nowrap` line.
    check("and in the needs-you pin it gets two lines before it is cut", async () => {
      if (lines !== 2) throw new Error(`the question takes ${lines} line(s)`);
    });
    // Hovering the question shows its text, not the resource card over it. **What would make this
    // fail:** arming the card from anywhere on the row, the headline included.
    await page.hover(`${row("web-feat-auth")} .htext`);
    await page.waitForTimeout(700);
    const overText = await page.evaluate(() => document.getElementById("rpop")?.classList.contains("on") || false);
    await page.mouse.move(5, 5); await page.waitForTimeout(100);
    await page.hover(`${row("web-feat-auth")} .name`);
    await page.waitForTimeout(700);
    const overName = await page.evaluate(() => document.getElementById("rpop")?.classList.contains("on") || false);
    value("hovering the question does not open the resource card over it; hovering the name still does",
      { overText, overName }, { overText: false, overName: true });
    await page.mouse.move(5, 5);

    // SKEIN-1300: the board is reachable by keyboard. **What would make this fail:** rows without a
    // tabindex — Tab walks the header and the section buttons and never lands on a box.
    let landed = "";
    for (let i = 0; i < 60 && !landed; i++) {
      await page.keyboard.press("Tab");
      landed = await page.evaluate(() => document.activeElement?.classList.contains("row") ? document.activeElement.dataset.name : "");
    }
    await page.keyboard.press("Enter");
    await page.waitForTimeout(300);
    value("Tab reaches a box on the board, and Enter opens it",
      { landed: !!landed, opened: await page.evaluate(() => view.box) }, { landed: true, opened: landed || "(none)" });
    value("the per-repo ＋ is drawn, not a glyph a phone's fonts lack, and it is a button",
      await page.evaluate(() => { const a = document.querySelector('.group[data-sid="repo:web"] .gadd');
        return { tag: a?.tagName, svg: !!a?.querySelector("svg"), text: a?.textContent.trim() }; }),
      { tag: "BUTTON", svg: true, text: "" });

    // SKEIN-1262: restarting a WORKING agent asks first; an idle one restarts at once.
    const restarts = [];
    await page.route("**/api/boxes/*/restart-agent", async r => {
      restarts.push(decodeURIComponent(new URL(r.request().url()).pathname.split("/")[3]));
      await new Promise(res => setTimeout(res, 1500));
      await r.fulfill({ json: { ok: true } }).catch(() => {});
    });
    const asked = [];
    page.on("dialog", d => { asked.push(d.message()); d.dismiss(); });
    await page.evaluate(() => openDiff("web-main"));
    await page.waitForTimeout(300);
    await page.click("#drestart");
    await page.waitForTimeout(400);
    // **What would make this fail:** `restartAgent` posting without asking — the confirm is skipped
    // and the restart is sent.
    value("Restart on a working box asks first, names the cost, and a No sends nothing",
      { asked: asked.length, says: /working right now/.test(asked[0] || "") && /cut off/.test(asked[0] || ""), sent: restarts.slice() },
      { asked: 1, says: true, sent: [] });
    await page.evaluate(() => openDiff("web-idle"));
    await page.waitForTimeout(300);
    await page.click("#drestart");
    await page.waitForTimeout(300);
    // **What would make this fail:** asking on every box — a question in front of the cheap case.
    // And the button held down while the restart is in flight, so a second press cannot follow it.
    value("Restart on an idle box goes straight away, and is held down while it is in flight",
      { asked: asked.length, sent: restarts.slice(),
        held: await page.evaluate(() => { const b = document.getElementById("drestart"); return { disabled: b.disabled, text: b.textContent }; }) },
      { asked: 1, sent: ["web-idle"], held: { disabled: true, text: "Restarting…" } });
    await page.waitForFunction(() => !document.getElementById("drestart")?.disabled, null, { timeout: 8000 }).catch(() => {});
    value("and it comes back once the restart has answered",
      await page.evaluate(() => document.getElementById("drestart")?.disabled), false);
    sayBlips();
    value("the board raised no page errors at 1280", errors, []);
    await context.close();
  }

  // --- SKEIN-1294: the box toolbar fits at 1280 and at 1600 --------------------------------------
  for (const [w, h] of [[1280, 800], [1600, 1000]]) {
    const { page, context } = await board(w, h);
    await page.evaluate(() => openDiff("web-main"));
    await page.waitForTimeout(400);
    // **What would make this fail:** the dockbar back on one `nowrap` line — at 1280 it ended at
    // x=1354 and Destroy and Close were past the edge.
    value(`at ${w}px, Destroy and Close are on screen and clickable`,
      { destroy: await hittable(page, "#ddestroy"), close: await hittable(page, "#dclose") },
      { destroy: "yes", close: "yes" });
    value(`at ${w}px, the box's name and branch are not cut to a letter`,
      await page.evaluate(() => [".nm", ".br"].map(s => { const e = document.querySelector("#dockbar " + s);
        return e.scrollWidth <= e.clientWidth + 1; })), [true, true]);
    await context.close();
  }

  // --- the phone ---------------------------------------------------------------------------------
  {
    const { page, context, errors, sayBlips } = await board(390, 844, { touch: true });
    // SKEIN-1297. **What would make this fail:** the four gauges stacked, the key hints and the
    // palette's Ctrl+K back — the first box started two-thirds down the screen.
    const first = await rect(page, "#fleet .row");
    check("on a phone, the first box starts in the top third of the screen", async () => {
      if (!first || first.top > 844 / 3) throw new Error(`the first row's top is at ${first && first.top}`);
    });
    // **What would make this fail:** the path stub taking the room — the branch read "f…".
    value("a row's branch is not cut to a letter, and the path stub is not drawn",
      await page.evaluate(s => { const r = document.querySelector(s);
        return { branch: r.querySelector(".branch").getBoundingClientRect().width >= 48,
                 dir: r.querySelector(".dir").getBoundingClientRect().width }; }, row("web-feat-auth")),
      { branch: true, dir: 0 });
    // SKEIN-1300. **What would make this fail:** the section controls at their old 15–24px.
    value("on a touch screen, the section's sync and ＋ are a thumb's size",
      await page.evaluate(() => [".gpull", ".gadd"].map(s => { const r = document.querySelector('.group[data-sid="repo:web"] ' + s).getBoundingClientRect();
        return r.width >= 40 && r.height >= 40; })), [true, true]);

    // SKEIN-1263: the Files tab. **What would make this fail:** `.flist` at a fixed 260px beside the
    // file view, or the name not truncating — the size column is pushed out of the row.
    await page.evaluate(() => openFiles("web-main"));
    await page.waitForSelector("#filespane .fent[data-f]");
    const long = '#filespane .fent[data-f^="a-really-long"]';
    value("on a phone, the Files list takes the width, a long name ends in an ellipsis, and its size stays on screen",
      await page.evaluate(s => { const e = document.querySelector(s), n = e.querySelector(".fnm"), z = e.querySelector(".fsz");
        return { list: document.querySelector("#filespane .flist").getBoundingClientRect().width >= 360,
                 cut: n.scrollWidth > n.clientWidth && getComputedStyle(n).textOverflow === "ellipsis",
                 named: n.title.startsWith("a-really-long"),
                 size: z.getBoundingClientRect().right <= innerWidth && z.getBoundingClientRect().width > 0 }; }, long),
      { list: true, cut: true, named: true, size: true });
    // The view toggle fits, or scrolls inside itself. **What would make this fail:** its buttons at
    // their desktop padding with no overflow — Transcript was drawn past x=390.
    value("the box's view toggle stays within the screen",
      await page.evaluate(() => document.querySelector("#dockbar .seg-toggle").getBoundingClientRect().right <= innerWidth),
      true);

    // SKEIN-1263: a review row. Drawn by the page's own `revRow` into the review pane, so the CSS
    // being measured is the CSS a queue gets. **What would make this fail:** the five fixed columns —
    // the title was 12px wide at 400px.
    await page.evaluate(() => {
      const pr = { number: 1234, repo_id: "web", title: "Make the review pane usable on a phone so a person can triage from anywhere" };
      view = { box: null, repo: "web", mode: "review", kind: "agent" }; applyView();
      revpane.innerHTML = `<div class="revwrap"><div class="revlane">${revRow(pr)}</div></div>`;
    });
    await page.waitForTimeout(200);
    const title = await rect(page, "#revpane .revtitle");
    check("on a phone, a review row's title has room to be read", async () => {
      if (!title || title.width < 200) throw new Error(`the title is ${title && title.width}px wide`);
    });
    sayBlips();
    value("the board raised no page errors on a phone", errors, []);
    await context.close();
  }

  // --- SKEIN-1263: popups on a short phone screen -----------------------------------------------
  {
    const { page, context } = await board(390, 664, { touch: true });
    for (const [name, open, button] of [
      ["Add a repository", () => addRepoPrompt(), "#addrepo button.primary"],
      ["Box settings", () => openBoxSettings("web-main"), "#bs-go"],
    ]) {
      await page.evaluate(open);
      await page.waitForTimeout(500);
      // A person scrolls the card to reach its button; the button must then be on screen, uncovered.
      // **What would make this fail:** a card with no max-height and no overflow — its button sits
      // below the window's bottom edge and nothing scrolls to it.
      await page.evaluate(s => document.querySelector(s)?.scrollIntoView({ block: "nearest" }), button);
      await page.waitForTimeout(150);
      value(`${name}'s button can be reached on a 390×664 screen`, await hittable(page, button), "yes");
      await page.keyboard.press("Escape");
      await page.evaluate(() => { for (const id of ["addrepo", "boxset"]) document.getElementById(id)?.classList.remove("open"); });
    }
    await context.close();
  }

  // --- SKEIN-1300: phone Settings keeps its footer on screen ------------------------------------
  {
    const { page, context } = await board(390, 844, { touch: true });
    // The longest pane, so its content is taller than the screen.
    await page.evaluate(() => openSettings("github"));
    await page.waitForTimeout(500);
    // **What would make this fail:** `.set-main` without `min-height:0` in the one-column layout —
    // the column grows to its content and the footer sits far below the dialog's bottom.
    value("on a phone, Settings' footer is inside the dialog and on screen",
      await page.evaluate(() => { const f = document.querySelector("#settings .set-foot").getBoundingClientRect(),
        s = document.querySelector("#settings .set-shell").getBoundingClientRect();
        return f.bottom <= s.bottom + 1 && f.bottom <= innerHeight && f.height > 0; }), true);
    await context.close();
  }
} catch (e) {
  await check("the suite could run at all", () => { throw e; });
}

await browser.close();
const failed = report({ log });
const leftRunning = stopThenRemove([root], { keep: failed.length > 0 });
process.exit(failed.length || leftRunning.length ? 1 : 0);
