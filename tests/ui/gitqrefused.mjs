// A write request answered twice, from the panel a person actually presses (SKEIN-1034).
//
// `gitgate::decide` answers a request once: a second answer is refused with a sentence —
// "request r1 is already granted — a request is answered once; to take back a grant, revoke it".
// The refusal is the server's half and `gitgate::tests::a_request_is_answered_once_and_a_second_
// answer_changes_nothing` holds it. This is the page's half, and it lives in the wiring between a
// POST, its body, a toast and a redraw — which a lifted `decideGitq` against a stubbed fetch can
// only claim, because the stub decides what the refusal looks like. Here the server does.
//
// The state is the one that produces a second answer in life: a row drawn as pending, then
// answered somewhere else (another tab, another person) before this one is pressed.
//
//   node tests/ui/gitqrefused.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
const API_TOKEN = "t".repeat(64);
const BOX = "web-main";
const ID = "r1";

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-gitqrefused");
  const home = path.join(root, "home");
  const fleet = path.join(root, "fleet");
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  // The box's ask, where `gitgate::requests_dir` reads it: one directory per box.
  const asked = path.join(fleet, ".skein", "gitgate", "requests", BOX);
  fs.mkdirSync(asked, { recursive: true });
  fs.writeFileSync(path.join(asked, `${ID}.json`), JSON.stringify({
    id: ID, box: BOX, repo: "acme/thing", reason: "fix the shared type",
    asked: "2026-09-23T09:00:00Z", state: "pending",
  }));
  return { root, home, fleet };
}

const fx = makeFixture();
const door = await openDoor();
const { srv, log } = await startServer({
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
const card = () => page.locator("#gitq .msg", { hasText: "acme/thing" }).first();
const grant = () => card().locator("button", { hasText: "Grant write" });

try {
  await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(800);
  await page.evaluate(() => openGitq());
  check("the request is drawn as pending, with a Grant button to press",
    await until(async () => (await grant().count()) === 1), true);

  // Answered elsewhere while this row was on screen — the host's own record of it, exactly as
  // `gitgate::write_decision` would have written it (`decision_path`: <home>/gitgate/<box>/<id>.json).
  const decided = path.join(fx.home, "gitgate", BOX);
  fs.mkdirSync(decided, { recursive: true });
  fs.writeFileSync(path.join(decided, `${ID}.json`), JSON.stringify({
    id: ID, box: BOX, repo: "acme/thing", reason: "fix the shared type",
    asked: "2026-09-23T09:00:00Z", state: "granted", decided: "2026-09-23T09:05:00Z",
  }));

  await grant().click();

  // **What would make this fail:** `decideGitq` throwing on `!r.ok` and toasting its own generic
  // "could not record that decision", which is what it did before — the server's sentence never
  // reached the page.
  const said = "request r1 is already granted — a request is answered once; to take back a grant, revoke it";
  let toast = "";
  await until(async () => {
    toast = await page.$eval("#toast", e => e.textContent.trim()).catch(() => "");
    return toast === said;
  });
  check("a refused answer shows the server's reason, not a generic failure", toast, said);

  // **What would make this fail:** no `loadGitq()` on the refused path. The row would go on saying
  // `pending` with a Grant button that can only ever be refused again.
  await until(async () => (await grant().count()) === 0);
  check("and the row is redrawn in its real state, with nothing left to press",
    { grantButtons: await grant().count(),
      state: await card().locator(".sq-state").first().textContent().catch(() => "") },
    { grantButtons: 0, state: "granted" });

  sayBlips();
  check("the panel raised no page errors", errors, []);
} catch (e) {
  check("the suite could run at all", String((e && e.message) || e), "it ran");
}

await browser.close();
srv.kill();
const failed = report();
if (failed.length) console.log(`\nserver log:\n${log()}`);
else { try { fs.rmSync(fx.root, { recursive: true, force: true }); } catch {} }
process.exit(failed.length ? 1 : 0);
