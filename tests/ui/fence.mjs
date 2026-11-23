// A fleet that does not own its volume, in a real browser against a real doorway (SKEIN-1539).
//
// The banner names the owner on both pages, the button asks first, Cancel changes nothing, a page
// whose owner has gone stale is refused and shows the new one, and a take-over brings the normal
// cockpit back with no manual restart.
//
// What fails it: the script tag gone from a page (the banner checks), the confirmation step skipped
// (the Cancel check), the route's owner comparison removed (the stale check), or the doorway restart
// not happening (the "comes back" check).
//
//   node tests/ui/fence.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { fixtureRoot, freshFixture, openDoor, serverBinary } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const API_TOKEN = "t".repeat(64);
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const root = freshFixture(fixtureRoot(), "ui-fence");
const home = path.join(root, "home");
const fleet = path.join(root, "fleet");
const dot = path.join(fleet, ".skein");
fs.mkdirSync(home, { recursive: true });
fs.mkdirSync(dot, { recursive: true });
fs.writeFileSync(path.join(home, "VERSION"), "1\n");
fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
fs.writeFileSync(path.join(home, "repos.json"), "[]");
fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");

/** Record `id` as the owner at `epoch`, as a claim and a record, the way `fence::flip` does. */
function ownedBy(id, epoch) {
  const claims = path.join(home, "fleet-owner.d");
  fs.mkdirSync(claims, { recursive: true });
  fs.symlinkSync(`v1:${id}:old-mac`, path.join(claims, String(epoch)));
  fs.writeFileSync(path.join(home, "fleet-owner"), JSON.stringify({ fleet_id: id, epoch, machine: "old-mac" }));
}
const owner = () => JSON.parse(fs.readFileSync(path.join(home, "fleet-owner"), "utf8"));
ownedBy("fleet-old", 1);

const doorway = path.join(dot, "server-doorway.py");
const server = path.join(dot, "skein-server");
fs.writeFileSync(doorway, fs.readFileSync(path.join(REPO, "src", "server-doorway.py")));
fs.symlinkSync(serverBinary(), server);

const { value, check, report } = ledger();
const browser = await chromium.launch();
let log = () => "";
const visible = (page, sel) => page.locator(sel).first().isVisible().catch(() => false);
const text = (page, sel) => page.locator(sel).first().textContent({ timeout: 3000 }).catch(() => "");
const until = async (page, f, ms = 10000) => {
  // By the clock: one check can itself take seconds.
  for (const end = Date.now() + ms; Date.now() < end;) { if (await f()) return true; await page.waitForTimeout(200); }
  return !!(await f());
};

async function suite() {
  const door = await openDoor();
  const { port } = door;
  const started = await startServer({
    door, token: API_TOKEN, ready: "/api/fleet/owner", // a fenced server answers 409 to /api/boxes
    env: {
      SKEIN_REGISTRY: path.join(root, "sandboxes.json"),
      SKEIN_HOME: home,
      SKEIN_FLEET_ROOT: fleet,
      SKEIN_SERVER_PORT: String(port),
    },
    program: ["python3", doorway, String(port), server, path.join(dot, "server.door")],
  });
  log = started.log;
  const open = async where => {
    const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
    page.setDefaultTimeout(8000);
    page.seen = erring(page, { say: (kind, what) => `${kind}: ${what}` });
    await page.goto(`http://127.0.0.1:${port}${where}?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
    await until(page, () => visible(page, "#fence-banner"));
    return page;
  };

  for (const where of ["/", "/v2"]) {
    const page = await open(where);
    value(`${where} shows the banner`, await visible(page, "#fence-take"), true);
    const said = await text(page, "#fence-owner");
    value(`${where} names the owner's fleet and machine`,
      said.includes("fleet-old") && said.includes("old-mac"), true);
    const whole = await text(page, "#fence-banner");
    value(`${where} says the old fleet must be stopped`, whole.includes("only if that fleet is stopped"), true);
    value(`${where} still hands over the command`, (await text(page, "#fence-claim")).includes("fleet-owner claim fleet-old"), true);
    await page.close();
  }

  const page = await open("/");
  await page.locator("#fence-take").click();
  value("the button asks first", await visible(page, "#fence-confirm"), true);
  value("and Cancel has the focus", await page.evaluate(() => document.activeElement?.id), "fence-cancel");
  value("asking took nothing", owner().fleet_id, "fleet-old");
  await page.locator("#fence-cancel").click();
  value("Cancel returns to the banner", await visible(page, "#fence-take"), true);
  value("and changed nothing", owner().fleet_id, "fleet-old");

  // The volume changes hands after this page read it.
  ownedBy("fleet-newer", 2);
  await page.locator("#fence-take").click();
  await page.locator("#fence-confirm").click();
  await until(page, async () => (await text(page, "#fence-note")).includes("owner changed"), 6000);
  value("a stale page is refused and says why", (await text(page, "#fence-note")).includes("owner changed"), true);
  value("and now names the new owner", (await text(page, "#fence-owner")).includes("fleet-newer"), true);
  value("and the volume stayed with it", owner().fleet_id, "fleet-newer");

  await page.locator("#fence-take").click();
  await page.locator("#fence-confirm").click();
  const back = await until(page, async () => {
    if (await visible(page, "#fence-banner")) return false;
    return await page.evaluate(() => fetch("/api/health").then(r => r.json()).then(h => !h.fenced)).catch(() => false);
  }, 45000);
  value("the take-over brings the normal cockpit back with no restart by hand", back, true);
  value("this fleet is the one owner", {
    mine: owner().fleet_id === fs.readFileSync(path.join(dot, "fleet-id"), "utf8").trim(),
    epoch: owner().epoch,
  }, { mine: true, epoch: 3 });
  value("the page raised no errors, fenced or after", page.seen.errors, []);
  await page.close();
}

// Bounded, so a page that never reaches a state fails by name instead of hanging the run.
const after = (ms, then) => new Promise((resolve, reject) => setTimeout(() => then(resolve, reject), ms).unref());
try {
  await Promise.race([suite(), after(100000, (_, reject) => reject(new Error("the suite did not finish in 100 s")))]);
} catch (e) {
  await check("the suite could run at all", () => { throw e; });
}

await Promise.race([browser.close(), after(5000, resolve => resolve())]);
const failed = report();
if (failed.length) console.log(`\nserver log:\n${log()}`);
const leftRunning = stopThenRemove([root], { keep: failed.length > 0 });
process.exit(failed.length || leftRunning.length ? 1 : 0);
