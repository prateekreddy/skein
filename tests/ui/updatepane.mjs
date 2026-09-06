// Settings -> Update in a real browser (SKEIN-486).
//
// **This suite exists because the node tests all passed and the pane was still wrong.** They cover
// the pure halves — the verdict, the CLI row — and both of the defects below live in the wiring
// between a fetch, a timer and a table cell, which is exactly what a stubbed document cannot see.
// Found by opening the page and reading it, which is the whole argument for a browser tier.
//
//   node tests/ui/updatepane.mjs
import { chromium } from "playwright";
import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { serverBinary, openDoor } from "./lift.mjs";

const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const API_TOKEN = "t".repeat(64);

// The smallest fixture that serves a settings dialog: a store, a home, and a token. No box, no
// tmux, no transcript — the Update pane asks about skein, not about the fleet's contents.
function makeFixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "ui-updatepane-"));
  fs.mkdirSync(path.join(root, "home"), { recursive: true });
  fs.writeFileSync(path.join(root, "home", "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(root, "home", "config.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(root, "home", "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  return { root };
}

// A GitHub the size of what THIS pane asks: one route. `update::ask_github` (src/update.rs:198)
// reads `/repos/{slug}/commits/{reference}` and wants a `sha` back — everything else it might ask
// (auth, rate limit) it never touches, since `available()` treats "no token" as the ordinary case
// (src/update.rs:159-163). Same seam the other GitHub-touching suites already stand one of these up
// on (UI-3): `SKEIN_GITHUB_API` is where `github::api_base` looks before it falls back to the real
// api.github.com (src/github.rs:187-191) — `review.mjs:41`, `actfail.mjs:66` and `connections.mjs:58`
// (the last two byte-identical) each start their own for the routes THEY need. This one answers only
// the route this pane's regression is about, rather than growing a second copy of theirs.
function createGitHub(sha) {
  const server = http.createServer((req, res) => {
    const url = req.url.split("?")[0];
    const send = (code, payload) => {
      const text = JSON.stringify(payload);
      res.writeHead(code, { "Content-Type": "application/json" });
      res.end(text);
    };
    if (/^\/repos\/[^/]+\/[^/]+\/commits\/[^/]+$/.test(url)) return send(200, { sha });
    send(404, { message: `no stub for ${url}` });
  });
  return new Promise(resolve => {
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({ url: `http://127.0.0.1:${port}`, close: () => server.close() });
    });
  });
}

async function startServer(fx, door, github) {
  const srv = spawn(serverBinary(), {
    cwd: REPO,
    stdio: door.stdio,
    env: {
      ...process.env,
      ...door.env,
      SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
      SKEIN_HOME: path.join(fx.root, "home"),
      SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
      SKEIN_NO_GH_SECRET: "1",
      // Without these two the pane's `/api/update` asks the real, unauthenticated api.github.com
      // about the owner's own repository (UI-3) — offline, behind a proxy, or after 60
      // requests/hour that route answers nothing, and the checks below have nothing to read. Both
      // point at the fixture instead, so the request never leaves the machine.
      SKEIN_GITHUB_API: github.url,
      SKEIN_SOURCE_URL: "https://github.com/acme/skein.git",
    },
  });
  door.close();
  let log = "";
  srv.stdout.on("data", d => { log += d; });
  srv.stderr.on("data", d => { log += d; });
  for (let i = 0; i < 100; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${door.port}/api/boxes`, {
        headers: { Authorization: `Bearer ${API_TOKEN}` }, signal: AbortSignal.timeout(2000),
      });
      if (r.ok) return { srv, log: () => log };
    } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  srv.kill();
  throw new Error(`server never came up on ${door.port}\n${log}`);
}

const results = [];
function check(name, got, want) {
  const ok = JSON.stringify(got) === JSON.stringify(want);
  results.push([ok, name]);
  console.log(ok ? `  ok    ${name}` : `  FAIL  ${name}\n        got ${JSON.stringify(got)} want ${JSON.stringify(want)}`);
}

// Fixed rather than read off the real repository (UI-3), so the checks below can assert an exact
// value instead of "GitHub said something" — 40 hex characters, the shape `same_revision`
// (src/update.rs:113) and the pane's abbreviation both expect a sha to have.
const REMOTE_SHA = "deadbeef".repeat(5);

const fx = makeFixture();
const door = await openDoor();
const github = await createGitHub(REMOTE_SHA);
const { srv } = await startServer(fx, door, github);
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const errors = [];
page.on("pageerror", e => errors.push(`pageerror: ${e.message}`));
page.on("console", m => { if (m.type() === "error") errors.push(`console: ${m.text()}`); });

await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await page.waitForTimeout(1200);
await page.evaluate(() => openSettings("update"));
await page.waitForTimeout(600);

// --- it is a pane somebody can reach, and it is on screen -----------------------------------------
//
// `mustSee` reasoning, from smoke.mjs: present in the DOM is not enough. A pane with a zero box is
// a pane a CSS rule is hiding, which is indistinguishable from a working one to every other check.
{
  const nav = await page.$$eval(".set-navi", els => els.map(e => e.textContent.trim()));
  check("Update is one of the settings panes", nav.some(n => n.startsWith("Update")), true);
  const box = await (await page.$("#set-update"))?.boundingBox();
  check("and it is actually on screen, not merely in the DOM", !!box && box.width > 0 && box.height > 0, true);
}

// --- the remote arrives LATE, and the pane has to notice -------------------------------------------
//
// THE REGRESSION THIS SUITE WAS WRITTEN FOR. `update::available` answers from a remembered reading
// and refreshes behind the caller, so the first read of a cold server always says "not asked yet".
// A pane that rendered that once said `github unknown` for as long as it stayed open — while the
// API had the sha the whole time. Every unit test passed.
//
// **What would make this fail**: deleting the re-read at the end of `loadUpdate`. Then the cell
// stays "unknown" and this check reports the difference between what the page knows and what it
// shows, which is the only place that difference is visible.
//
// Deterministic now (UI-3): the fixture GitHub above always answers `REMOTE_SHA`, so the first
// check is a real assertion — it names the exact sha rather than "GitHub answered at all, or there
// is nothing to assert about", which passed by staying silent whenever the suite had no network.
{
  let cell = "", api = null;
  for (let i = 0; i < 40; i++) {
    await page.waitForTimeout(400);
    api = await page.evaluate(async () => (await (await fetch("/api/update")).json()));
    cell = await page.$eval("#set-update .set-revs tr:nth-child(3) td", e => e.textContent.trim()).catch(() => "");
    if (api?.skein?.remote && cell && cell !== "unknown") break;
  }
  check("GitHub answered with the fixture's sha", api?.skein?.remote, REMOTE_SHA);
  check("and the pane shows it rather than staying on the first empty reading",
    !!api?.skein?.remote && api.skein.remote.startsWith(cell.replace(/…/g, "")) && cell !== "unknown", true);
}

// --- a revision is abbreviated by cutting the SHA, never the string --------------------------------
//
// `git describe --always --dirty` appends a marker, and a blind twelve characters rendered
// `89cf36b-dirty` as `89cf36b-dirt` — which reads as a sha fragment and drops the one fact build.rs
// says matters as much as the revision.
//
// Asserted as a PREFIX of the de-dirtied value rather than against a literal, because what this
// binary's revision is depends on the tree it was built from. `89cf36b-dirt` is not a prefix of
// `89cf36b`, so the bug fails this on a clean checkout and a dirty one alike.
{
  const api = await page.evaluate(async () => (await (await fetch("/api/update")).json()));
  const shown = await page.$eval("#set-update .set-revs tr:nth-child(1) td", e => e.textContent.trim());
  const bare = String(api?.skein?.running || "").replace(/-dirty$/, "");
  check("the running revision is a prefix of the real one, never a truncated marker",
    [!!bare, bare.startsWith(shown), shown.length > 0], [true, true, true]);
}

// --- and nothing threw ------------------------------------------------------------------------------
check("the pane raised no page errors", errors, []);

await browser.close();
srv.kill();
github.close();
try { fs.rmSync(fx.root, { recursive: true, force: true }); } catch {}

const bad = results.filter(([ok]) => !ok);
if (bad.length) { console.log(`\n${bad.length} failed`); process.exit(1); }
console.log("\nall good");
