// A box's ask for a bigger disk allowance, decided on its card in the Decisions strip, against the
// real server reading a real drop-box (SKEIN-1425).
//
// The box wrote one file, `disk-allowance`, into its own package drop-box: a size and a reason. The
// card reads "<box> asks for 20 GB disk (now 10 GB, …): <reason>", the reason as plain text, with
// Approve and Deny on the card itself. Approve raises the box's allowance at once — the same setting
// the box-settings field writes — and the box's inbox gets the approved line.
//
//   node tests/ui/diskask.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";
const API_TOKEN = "skein-test-" + "t".repeat(53);
const BOX = "web-main";
const ID = "d-0000aaaa";

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-diskask");
  const home = path.join(root, "home");
  const fleet = path.join(root, "fleet");
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  // The box's ask, where `diskask` reads it: its own package drop-box, one file by one name.
  const dropBox = path.join(fleet, ".skein", "substrate", "requests", BOX);
  fs.mkdirSync(dropBox, { recursive: true });
  fs.writeFileSync(path.join(dropBox, "disk-allowance"), JSON.stringify({
    id: ID, size: "20g", why: "the search index is <b>14G</b> and grows", asked: "2026-10-31T10:00:00Z",
    // What the box says about itself, which is nobody's answer.
    state: "approved",
  }));
  fs.mkdirSync(path.join(fleet, BOX, "tree"), { recursive: true });
  return { root, home, fleet };
}

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
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const { errors, sayBlips } = erring(page, { say: (kind, text) => `${kind}: ${text}` });
const until = async (f, ms = 8000) => {
  for (let t = 0; t < ms; t += 200) { if (await f()) return true; await page.waitForTimeout(200); }
  return !!(await f());
};
const inbox = () => {
  const dir = path.join(fx.home, "boxes", BOX, "inbox");
  if (!fs.existsSync(dir)) return [];
  return fs.readdirSync(dir).sort().map(f => JSON.parse(fs.readFileSync(path.join(dir, f), "utf8")))
    .map(m => `${m.kind}: ${m.body}`);
};
const allowance = () => page.evaluate(async box => {
  const r = await fetch(`/api/boxes/${box}/settings`);
  return r.ok ? (await r.json()).effective_disk_mb : `HTTP ${r.status}`;
}, BOX);

try {
  await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  const strip = page.locator("#decisions");
  const card = strip.locator(".dcard[data-source=disk]");
  // **What would make this fail:** the ask not read from the box's drop-box, a box's own `state`
  // believed, or the row not drawn as a card of its own.
  check("the Decisions strip shows the box's disk ask, its own `approved` notwithstanding",
    await until(async () => (await card.count()) === 1), true);
  check("before anything is pressed the box has the fleet's 10 GB", await allowance(), 10240);

  // **What would make this fail:** the headline not `diskask::headline`'s, or the reason drawn
  // without `decisionEsc` — `<b>` then becomes bold.
  const body = (await card.locator(".aq-body").textContent()).trim();
  check("the card reads as the owner approved it, the box's reason as plain text", {
    shape: /^web-main asks for 20 GB disk \(now 10 GB(, using [0-9.]+ GB)?\): the search index is <b>14G<\/b> and grows$/.test(body),
    bold: await card.locator(".aq-body b").count(),
  }, { shape: true, bold: 1 });
  check("its buttons are Approve and Deny, on the card", await card.locator("button").allTextContents(),
    ["Approve", "Deny"]);

  // **What would make this fail:** the press not reaching `/api/boxes/:name/disk-ask`, the server
  // not going through `set_box_disk_limit`, or the strip not redrawn once it has
  // (`queue::forget_decisions` not called, so the cached row would linger for its window).
  await card.locator("button", { hasText: "Approve" }).click();
  check("once approved the Decisions strip is gone",
    await until(() => page.evaluate(() => document.getElementById("decisions").hidden), 5000), true);
  check("the box's allowance is 20 GB at once", await allowance(), 20480);
  await until(async () => inbox().length === 1);
  check("the box's inbox got the approved line once", inbox(), [`answer: disk request ${ID}, approved`]);

  sayBlips();
  check("the page raised no errors", errors, []);
} catch (e) {
  check("the suite could run at all", String((e && e.message) || e), "it ran");
}

await browser.close();
const failed = report();
if (failed.length) console.log(`\nserver log:\n${log()}`);
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 });
process.exit(failed.length || leftRunning.length ? 1 : 0);
