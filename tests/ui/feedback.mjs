// What the page says when something it asked for goes wrong, or takes a while.
//
// Five reports from the 2026-10-22 sweep, each about feedback rather than about the thing done:
//
// * **A refusal's reason** (SKEIN-1299). The same 500 read "couldn't save: Internal Server Error"
//   in Settings while its body said "Read-only file system", "could not record that decision" in
//   Questions and Package requests, and "could not load diff" in Diff. `refused` (boot.js) is the
//   one reader now. And Files showed a blank pane while it loaded, where Diff said it was asking.
// * **Where an error is said** (SKEIN-1265, SKEIN-1245). A failure went into a toast that hid
//   after 3.5 seconds and, at several call sites, was cut to its first line. An error — and the
//   agent-CLI update's several-line report — is a notice now (`notice`, boot.js), which keeps
//   every line and stays until it is closed.
// * **A second click** (SKEIN-1296). "Send answer" and "Approve & install" stayed live while their
//   request was in flight, and a double click sent it twice; an approval installs fleet-wide.
// * **A refusal taken for success** (SKEIN-1266). New box launched with defaults when the server
//   refused a choice made in the dialog, and Box settings said nothing about which of its five
//   writes had failed.
//
// Every request whose answer matters is a stubbed route, so the test decides what the server says
// and when; every write under `/api/fleet/` and `/api/boxes/` is answered by a stand-in rather than
// the server (SKEIN-1290).
//
//   node tests/ui/feedback.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const BOX = "feedback-box";
const API_TOKEN = "f".repeat(64);

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "skein-feedback-ui");
  const home = path.join(root, "home");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(path.join(root, "fleet"), { recursive: true });
  fs.writeFileSync(path.join(home, "config.json"), "{}");
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  fs.mkdirSync(path.join(root, "work"));
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    { id: "acme", source: "https://github.com/acme/thing.git", source_tree: path.join(root, "work"), read_prs: false,
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, "#!/usr/bin/env bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
  fs.chmodSync(sbx, 0o755);
  fs.writeFileSync(path.join(bin, "gh"), "#!/bin/sh\nexit 1\n", { mode: 0o755 });
  return { root, home, bin, sbx };
}

const { check, report } = ledger();

const fx = makeFixture();
const door = await openDoor();
const origin = `http://127.0.0.1:${door.port}`;
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,
    PATH: `${fx.bin}:${process.env.PATH}`,
    GH_TOKEN: "",
    SKEIN_GITHUB_API: "http://127.0.0.1:9",
  },
});

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1400, height: 900 } });
const page = await context.newPage();
page.setDefaultTimeout(5000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
// No terminal reaches a server from here: a box's socket is stood in for.
await page.routeWebSocket(/\/api\/boxes\/[^/]+\/terminal/, () => {});
// Every write under the fleet and the boxes is a stand-in's unless a check routes it itself.
await page.route(/\/api\/(fleet|boxes)\//, r => r.request().method() === "GET" ? r.fallback() : r.fulfill({ json: { ok: true } }));

await page.goto(`${origin}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await page.waitForFunction(() => typeof openSettings === "function" && typeof boxes !== "undefined");

const closeEverything = () => page.evaluate(() => {
  for (const o of document.querySelectorAll(".open[aria-hidden], #away.open, #load.open")) o.classList.remove("open");
  document.getElementById("notice")?.classList.remove("show");
});
/** Everything the page has said lately: the toast and the notice, as text. */
const said = () => page.evaluate(() => ["toast", "notice"].map(id => document.getElementById(id))
  .filter(el => el && el.classList.contains("show")).map(el => el.textContent).join(" | "));
const until = (fn, ms = 3000) => page.waitForFunction(fn, null, { timeout: ms });
const saying = text => page.waitForFunction(t => ["toast", "notice"].some(id => {
  const el = document.getElementById(id);
  return el && el.classList.contains("show") && el.textContent.includes(t);
}), text, { timeout: 3000 }).then(() => true, () => false);

// ---------- a refusal's reason (SKEIN-1299) ----------
console.log("\na refusal says why");

// Fails if `saveSettingsQuiet` throws `r.statusText` again: the toast says "Internal Server Error".
await check("a refused Settings save says the server's reason", async () => {
  await closeEverything();
  await page.route("**/api/settings", r => r.request().method() === "POST"
    ? r.fulfill({ status: 500, body: "Read-only file system" }) : r.fallback());
  try {
    await page.evaluate(() => openSettings("github"));
    await until(() => settingsModal().classList.contains("open"));
    await page.locator("#set-ghappid").fill("77");
    await page.locator("#set-go").click();
    if (!(await saying("Read-only file system"))) throw new Error(`said ${JSON.stringify(await said())}`);
  } finally {
    await page.unroute("**/api/settings");
    await page.evaluate(() => discardSettings());
  }
});

const asks = [{ id: "q-1", box: BOX, question: "Which port?", asked: "2026-10-22T10:00:00Z", state: "waiting" }];
const pkgs = [{ id: "p-1", box: BOX, kind: "apt", packages: ["libnss3"], asked: "2026-10-22T10:00:00Z", state: "pending" }];
await page.route("**/api/fleet/asks", r => r.fulfill({ json: asks }));
await page.route("**/api/fleet/substrate", r => r.request().method() === "GET" ? r.fulfill({ json: pkgs }) : r.fallback());

// Fails if `sendAskq` goes back to `throw 0` and a fixed sentence.
await check("a refused answer to a question says the server's reason", async () => {
  await closeEverything();
  await page.route(/\/api\/fleet\/asks\/[^/]+$/, r => r.fulfill({ status: 500, body: "the box's drop-box is full" }));
  try {
    await page.evaluate(() => openAskq());
    const reply = page.locator(`[id="aq-reply-${BOX}/q-1"]`);
    await reply.fill("8080");
    await page.locator("#askq button", { hasText: "Send answer" }).click();
    if (!(await saying("the box's drop-box is full"))) throw new Error(`said ${JSON.stringify(await said())}`);
  } finally { await page.unroute(/\/api\/fleet\/asks\/[^/]+$/); }
});

// Fails if `decideSubq` goes back to `throw 0` and a fixed sentence.
await check("a refused package approval says the server's reason", async () => {
  await closeEverything();
  await page.route(/\/api\/fleet\/substrate\/[^/]+$/, r => r.fulfill({ status: 409, body: "apt is busy in the sandbox" }));
  try {
    await page.evaluate(() => openSubq());
    await page.locator("#subq button", { hasText: "Approve" }).click();
    if (!(await saying("apt is busy in the sandbox"))) throw new Error(`said ${JSON.stringify(await said())}`);
  } finally { await page.unroute(/\/api\/fleet\/substrate\/[^/]+$/); }
});

// Fails if `loadDiff` parses a refusal as JSON and lands in its fixed "could not load diff".
await check("a refused diff says the server's reason in the pane", async () => {
  await closeEverything();
  await page.route(`**/api/boxes/${BOX}/diff`, r => r.fulfill({ status: 503, body: "the box is not running" }));
  try {
    await page.evaluate(box => openDiff(box), BOX);
    await until(() => document.getElementById("diffpane").textContent.includes("the box is not running"));
  } finally { await page.unroute(`**/api/boxes/${BOX}/diff`); }
});

// Fails if `loadFiles` leaves the pane as it was while it asks: it is blank, or the last box's.
await check("Files says it is asking the box while it waits", async () => {
  let release;
  const held = new Promise(res => { release = res; });
  await page.route(`**/api/boxes/${BOX}/files*`, async r => { await held; return r.fulfill({ json: { path: "", entries: [] } }); });
  try {
    await page.evaluate(box => openFiles(box), BOX);
    await until(() => document.getElementById("filespane").textContent.includes("asking the box"), 1500);
  } finally { release(); await page.unroute(`**/api/boxes/${BOX}/files*`); }
});

await page.evaluate(() => { view = { box: null, mode: "term", kind: "agent" }; applyView(); });

await check("no page errors while feedback was driven", () => {
  if (noise.length) throw new Error(noise.slice(0, 5).join(" | "));
});

// ---------- report ----------
const failed = report({ log });
if (failed.length) console.log(`fixture kept for inspection: ${fx.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
process.exit(failed.length || leftRunning.length ? 1 : 0);
