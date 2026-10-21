// Hiding a warning at the top of the cockpit for a while (SKEIN-1244), in a real page.
//
// The owner's decisions, each a check below: every one of the four rows has a ✕; a plain press
// hides it for an hour and ▾ offers "for 1 hour", "for 4 hours" and "until tomorrow"; a hidden
// row stays hidden across a reload, in this browser only; it comes back when its time is up, or at
// once when a different problem arrives; the ✕ never does the row's own job; and a browser that
// will not store anything still shows every row.
//
// What the rule IS lives in `cockpit/src/dismiss.mjs` and its node test. This is the half a node
// test cannot see: a button inside a button, a press that reaches the row underneath, a menu the
// keyboard cannot leave, a reload that forgets, a clock nothing moves. So the page is real, the
// server is real, and only the three answers that put a warning up are supplied by the suite —
// `/api/health` and `/api/review/counts` through `page.route`, and the staleness row by closing the
// page's own event stream, which is what a dead server does to it. Time is Playwright's clock, so
// an hour passes in a call rather than in an hour.
//
//   node tests/ui/dismiss.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const API_TOKEN = "skein-test-" + "d".repeat(53);

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-dismiss");
  const home = path.join(root, "home");
  const fleet = path.join(root, "fleet");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(fleet, { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  return { root, home, fleet };
}

// What /api/health answers: one counted check failing, and one runtime's login dead. Mutable, so a
// check can put a second problem up while the first is hidden.
const LABELS = ["registry", "sbx", "disk"].map(key => ({ key, label: key }));
const bad = what => ({ level: "unsatisfied", detail: `${what}. do something about it`, fix: "do something" });
let health = {
  ok: false, counted: ["registry", "sbx", "disk"], labels: LABELS,
  registry: bad("the registry is unreachable"),
  expired_logins: [{ runtime: "claude", expired_at: "2026-10-21T08:00:00Z", witness: "credential" }],
};
let counts = [{ repo_id: "alpha", needs_you: 0, error: "", skipped: "",
                stopped: [{ number: 7, why: "CI failed. see the run" }] }];

const fx = makeFixture();
const door = await openDoor();
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: fx.fleet,
  },
});

const { value: check, report } = ledger();
const browser = await chromium.launch();
const URL = `http://127.0.0.1:${door.port}/?t=${API_TOKEN}`;
const HOUR = 60 * 60 * 1000;

// One page: the clock installed before the first script runs, the two answers routed, and spies on
// the three actions a row's own button takes — recorded and then passed through, so the real
// action still happens and the suite can say which ones were asked for.
async function open(context) {
  const page = await context.newPage({ viewport: { width: 1400, height: 900 } });
  await page.clock.install({ time: new Date("2026-10-21T09:00:00") });
  await page.route("**/api/health", r => r.fulfill({ json: health }));
  await page.route("**/api/review/counts", r => r.fulfill({ json: counts }));
  const blame = erring(page, { say: (kind, text) => `${kind}: ${text}` });
  await page.goto(URL, { waitUntil: "domcontentloaded" });
  await spy(page);
  return { page, ...blame };
}
// Again after every reload, which starts a fresh page.
const spy = page => page.evaluate(() => {
  window.__acts = [];
  for (const name of ["openSettings", "openReview", "openLoginTerminal"]) {
    const real = window[name];
    window[name] = (...args) => { window.__acts.push([name, ...args]); return real(...args); };
  }
});

const until = async (page, f, ms = 8000) => {
  for (let t = 0; t < ms; t += 100) { if (await f()) return true; await page.waitForTimeout(100); }
  return !!(await f());
};
// Up and on screen; up but hidden; or not drawn at all — three different answers, never two.
const shown = (page, id) => page.evaluate(id => {
  const e = document.getElementById(id);
  if (!e) return "absent";
  const r = e.getBoundingClientRect();
  return r.width > 0 && r.height > 0 ? "shown" : "hidden";
}, id);
const acts = page => page.evaluate(() => window.__acts.splice(0));
const stored = (page, id) => page.evaluate(id => {
  const v = localStorage.getItem(`skein.dismiss.${id}`);
  return v && { ...JSON.parse(v), now: Date.now() };
}, id);
// The next poll, now, rather than in fifteen seconds or three minutes.
const repoll = async page => {
  await page.evaluate(() => { loadHealth(); pollReviewCounts(); });
  await page.waitForTimeout(400);
};

try {
  const context = await browser.newContext();
  // One row's part failing must not cost the others theirs: each part runs to its end or says
  // where it stopped, and the next starts with no menu left open.
  const part = async (name, fn) => {
    try { await fn(); } catch (e) {
      check(`${name}: its checks ran to the end`, String((e && e.message) || e).split("\n")[0], "they did");
      await page.evaluate(() => banMenuClose(false)).catch(() => {});
    }
  };
  const { page, errors, sayBlips } = await open(context);

  // ---- every row is up, and every row has its ✕ and its ▾ --------------------------------------
  // Fails if any of the four rows is drawn without the control, or the control is hidden by CSS.
  for (const id of ["healthban", "loginban", "trainban"]) {
    await until(page, async () => (await shown(page, id)) === "shown");
  }
  check("the three polled rows are up",
    [await shown(page, "healthban"), await shown(page, "loginban"), await shown(page, "trainban")],
    ["shown", "shown", "shown"]);
  check("and each has a ✕ and a ▾ on screen",
    await page.evaluate(() => ["healthban", "loginban", "trainban"].map(id => {
      const seen = sel => { const e = document.querySelector(`#${id} ${sel}`); const r = e?.getBoundingClientRect(); return !!r && r.width > 0; };
      return [seen(".banx"), seen(".banmore")];
    })),
    [[true, true], [true, true], [true, true]]);
  // Fails if the row is a button again with the ✕ inside it: the parser would move the inner one out.
  check("no button holds another button",
    await page.evaluate(() => document.querySelectorAll("button button").length), 0);

  await part("the environment row", async () => {
    // ---- the row's own action still works, and the ✕ is not it ------------------------------------
    // Fails if the sentence stops being the way to the diagnostics pane.
    await page.click("#healthban .healthgo");
    check("pressing the environment row's sentence opens the diagnostics pane", await acts(page),
      [["openSettings", "diag"]]);
    // The dialog opens once its reads land; closed after that, so it covers nothing below.
    await until(page, () => page.evaluate(() => document.getElementById("settings").classList.contains("open")));
    await page.evaluate(() => closeSettings());
    // Fails if the ✕'s press is not stopped, or the ✕ sits inside the row's button.
    await page.click("#healthban .banx");
    check("pressing its ✕ hides the row and opens nothing",
      [await shown(page, "healthban"), await acts(page)], ["hidden", []]);
    check("for an hour", await stored(page, "healthban").then(r => r && [r.ids, r.until - r.now <= HOUR, r.until - r.now > HOUR - 5000]),
      [["check:registry"], true, true]);
    check("and only that row", [await shown(page, "loginban"), await shown(page, "trainban")], ["shown", "shown"]);

    // ---- a reload remembers ---------------------------------------------------------------------
    // Fails if the dismissal is kept only in the page's memory.
    await page.reload({ waitUntil: "domcontentloaded" });
    await spy(page);
    await until(page, async () => (await shown(page, "loginban")) === "shown");
    await until(page, async () => (await shown(page, "healthban")) !== "absent");
    check("after a reload the hidden row is still hidden, and the others are not",
      [await shown(page, "healthban"), await shown(page, "loginban")], ["hidden", "shown"]);

    // ---- the same problem with different words stays hidden; a different problem shows ----------
    // Fails if the fingerprint is the row's text: the detail changed and the problem did not.
    health = { ...health, registry: bad("the registry has been unreachable for 46s") };
    await repoll(page);
    check("the same check failing with a new sentence stays hidden", await shown(page, "healthban"), "hidden");
    // Fails if the fingerprint ignores what failed (a constant), or compares only the first id.
    health = { ...health, sbx: bad("the sandbox runtime is missing") };
    await repoll(page);
    check("another check failing shows the row at once", await shown(page, "healthban"), "shown");

    // ---- time: hidden before the hour is up, shown after --------------------------------------------
    await page.click("#healthban .banx");
    await page.clock.fastForward(59 * 60 * 1000);
    await repoll(page);
    check("59 minutes later it is still hidden", await shown(page, "healthban"), "hidden");
    // Fails if the expiry is ignored.
    await page.clock.fastForward(2 * 60 * 1000);
    await repoll(page);
    check("past the hour it is back, the problem still being true", await shown(page, "healthban"), "shown");

    // And a record already past its time when the page loads — the reload half of the same rule.
    await page.click("#healthban .banx");
    await page.evaluate(() => {
      const k = "skein.dismiss.healthban";
      const r = JSON.parse(localStorage.getItem(k));
      localStorage.setItem(k, JSON.stringify({ ...r, until: Date.now() - 1000 }));
    });
    await page.reload({ waitUntil: "domcontentloaded" });
    await spy(page);
    await until(page, async () => (await shown(page, "healthban")) === "shown");
    check("a dismissal that ran out while the page was closed shows the row on load",
      await shown(page, "healthban"), "shown");

  });
  await part("the login row", async () => {
    // ---- the menu, by keyboard: ▾ opens it, focus is on its first choice, arrows move, Enter picks --
    await page.focus("#loginban .banmore");
    await page.keyboard.press("Enter");
    await until(page, async () => (await page.$("#banmenu")) !== null);
    check("▾ opens a menu of the three lengths, focused on the first",
      await page.evaluate(() => ({
        items: [...document.querySelectorAll("#banmenu [role=menuitem]")].map(b => b.textContent),
        focused: document.activeElement?.textContent,
        expanded: document.querySelector("#loginban .banmore").getAttribute("aria-expanded"),
      })),
      { items: ["for 1 hour", "for 4 hours", "until tomorrow"], focused: "for 1 hour", expanded: "true" });
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Enter");
    check("choosing 'for 4 hours' hides the login row and opens no login terminal",
      [await shown(page, "loginban"), await page.$("#banmenu").then(m => !!m), await acts(page)],
      ["hidden", false, []]);
    check("for four hours", await stored(page, "loginban").then(r => r && [r.ids, Math.round((r.until - r.now) / 60000)]),
      [["login:claude"], 240]);
    // A different runtime's login dying is a different problem.
    health = { ...health, expired_logins: [...health.expired_logins,
      { runtime: "codex", expired_at: "2026-10-21T08:30:00Z", witness: "credential" }] };
    await repoll(page);
    check("another runtime's login dying shows the row at once", await shown(page, "loginban"), "shown");
    // Fails if the row's own button stopped working.
    await page.click("#loginban .loginfix >> nth=0");
    check("and its own 'log in' still opens the login", (await acts(page)).map(a => a[0]), ["openLoginTerminal"]);
    await page.evaluate(() => closeLoginTerminal());

  });
  await part("the train row", async () => {
    // ---- the menu, by pointer; Escape gives focus back; the train row's own click still works -----
    await page.click("#trainban .banmore");
    await until(page, async () => (await page.$("#banmenu")) !== null);
    await page.keyboard.press("Escape");
    check("Escape closes the menu and puts focus back on the ▾",
      await page.evaluate(() => [!!document.getElementById("banmenu"), document.activeElement?.classList.contains("banmore")]),
      [false, true]);
    check("having hidden nothing", await shown(page, "trainban"), "shown");
    await page.click("#trainban .banmore");
    await page.click("#banmenu >> text=until tomorrow");
    check("'until tomorrow' hides the train row until local midnight, opening no review",
      [await shown(page, "trainban"), await acts(page),
       await page.evaluate(() => {
         const r = JSON.parse(localStorage.getItem("skein.dismiss.trainban"));
         const d = new Date(r.until);
         return [r.ids, d.getHours(), d.getMinutes(), d.getDate() !== new Date().getDate()];
       })],
      ["hidden", [], [["alpha#7"], 0, 0, true]]);
    // Fails if the train's fingerprint is the repo alone, or the reason text.
    counts = [{ ...counts[0], stopped: [...counts[0].stopped, { number: 9, why: "conflict" }] }];
    await repoll(page);
    check("another pull request stopping shows the train row at once", await shown(page, "trainban"), "shown");
    await page.click("#trainban .trainrepo");
    check("and its repo still opens the review queue", (await acts(page)).map(a => a[0]), ["openReview"]);

  });
  await part("the staleness row", async () => {
    // ---- the staleness row: one outage is one problem ---------------------------------------------
    // The page's own stream closed, as a dead server leaves it; the clock moved past the threshold.
    await page.evaluate(() => es.close());
    await page.clock.fastForward(31_000);
    await until(page, async () => (await shown(page, "staleban")) === "shown");
    check("a quiet board raises the staleness row, with its ✕",
      [await shown(page, "staleban"), await page.evaluate(() => !!document.querySelector("#staleban .banx"))],
      ["shown", true]);
    const before = await page.evaluate(() => document.querySelector("#staleban .banbody").textContent);
    await page.click("#staleban .banx");
    await page.clock.fastForward(10_000);
    await page.waitForTimeout(2500);
    const after = await page.evaluate(() => document.querySelector("#staleban .banbody").textContent);
    // Fails if the fingerprint is the row's text, whose seconds count has moved on.
    check("its ✕ hides it for the rest of that outage, however the count moves",
      [await shown(page, "staleban"), before !== after, /^board not updated for \d+s — reconnecting…$/.test(after)],
      ["hidden", true, true]);
    // The board comes back, then goes quiet again: a new outage, well inside the hour.
    await page.evaluate(() => connect());
    await until(page, async () => (await shown(page, "staleban")) === "absent");
    check("a live board takes the row down", await shown(page, "staleban"), "absent");
    await page.evaluate(() => es.close());
    await page.clock.fastForward(31_000);
    await until(page, async () => (await shown(page, "staleban")) === "shown");
    check("and the next outage shows it at once", await shown(page, "staleban"), "shown");
    await page.evaluate(() => connect());
  });

  sayBlips();
  check("the page raised no errors", errors, []);
  await context.close();

  // ---- a browser that will not store a dismissal still shows every row ---------------------------
  // Every read and write of a dismissal throws, as storage blocked by a browser does. Only those
  // keys: what the rest of the page does with storage is not this item's to change.
  const refusing = await browser.newContext();
  await refusing.addInitScript(() => {
    const deny = k => { if (String(k).startsWith("skein.dismiss.")) throw new DOMException("storage refused", "SecurityError"); };
    for (const name of ["getItem", "setItem", "removeItem"]) {
      const real = Storage.prototype[name];
      Storage.prototype[name] = function (k, ...rest) { deny(k); return real.call(this, k, ...rest); };
    }
  });
  const second = await open(refusing);
  const p2 = second.page;
  health = { ...health, sbx: undefined, expired_logins: [] };
  await until(p2, async () => (await shown(p2, "healthban")) === "shown");
  // Fails if a throwing read is taken as "dismissed", or escapes and stops the row being drawn.
  check("with storage refused, the row is shown", await shown(p2, "healthban"), "shown");
  await p2.click("#healthban .banx");
  await repoll(p2);
  check("and its ✕ still hides it in this tab", await shown(p2, "healthban"), "hidden");
  await p2.reload({ waitUntil: "domcontentloaded" });
  await until(p2, async () => (await shown(p2, "healthban")) === "shown");
  check("and with nothing remembered, a reload shows it again", await shown(p2, "healthban"), "shown");
  second.sayBlips();
  check("the refusing page raised no errors of its own", second.errors, []);
  await refusing.close();
} catch (e) {
  check("the suite could run at all", String((e && e.stack) || e), "it ran");
}

await browser.close();
const failed = report({ log });
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 });
process.exit(failed.length || leftRunning.length ? 1 : 0);
