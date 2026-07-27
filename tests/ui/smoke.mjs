// Browser smoke test: launch the real `skein-server` against a throwaway workspace and walk the
// cockpit the way a person does — click the tabs, step into a folder, open a doc.
//
// Why this exists: the Files tab shipped with every directory row *rendered and then hidden* by an
// unrelated CSS rule (`body.docked .dir`), so no folder could be entered and nothing inside one was
// reachable. Nothing caught it — not the unit tests, not `tests/server.rs` (the API was perfect),
// not clippy. Only a browser could see it. So the rule here is: **assert what is VISIBLE**, never
// what merely exists in the DOM. `mustSee` is the whole point of this file; `page.$` is not enough.
//
// Run:  node tests/ui/smoke.mjs          (see tests/ui/README.md for the one-time setup)
// Exits non-zero on the first failure per check, prints a summary, and leaves a screenshot behind.

import { chromium } from "playwright";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const BOX = "smoke-box";

// ---------- fixture: a tiny workspace with the shapes that have actually broken ----------
function makeFixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "skein-ui-"));
  const ws = path.join(root, "workspace");
  fs.mkdirSync(path.join(ws, "docs", "nested"), { recursive: true });
  fs.writeFileSync(path.join(ws, "README.md"), "# smoke\n\nRead [the guide](docs/guide.md).\n");
  fs.writeFileSync(path.join(ws, "notes.txt"), "plain text\n");
  fs.writeFileSync(path.join(ws, "docs", "guide.md"), "# Guide\n\nBack to [the readme](../README.md).\n");
  fs.writeFileSync(path.join(ws, "docs", "nested", "deep.md"), "# Deep\n");
  fs.symlinkSync(path.join(ws, "docs"), path.join(ws, "linked"));  // inside: an ordinary directory
  fs.symlinkSync("/etc", path.join(ws, "outside"));                // outside: must be refused
  fs.writeFileSync(path.join(root, "sandboxes.json"), JSON.stringify({
    [BOX]: { branch: "main", dir: ws, lastSeen: new Date().toISOString(), status: "" },
  }));
  // a real git repo, so a verify can fingerprint what it checked the way it would in a box
  const git = (...a) => spawnSync("git", ["-C", ws, ...a], { stdio: "ignore" });
  git("init", "-q");
  git("-c", "user.email=smoke@test", "-c", "user.name=smoke", "add", "-A");
  git("-c", "user.email=smoke@test", "-c", "user.name=smoke", "commit", "-qm", "fixture");
  // Stand-in for sbx: the cockpit asks it for the fleet, and must never reach the real one. It
  // serves `exec` too, running the command in the fixture workspace the way a box would — so the
  // verify path (wrapper script, exit-code marker, stored record) runs for real, without a sandbox.
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, `#!/bin/sh
case "$1" in
  ls)   echo '[{"name":"${BOX}","status":"running","agent":"claude","workspace":"${ws}"}]'; exit 0 ;;
  exec) shift; shift; cd "${ws}" || exit 1; exec "$@" ;;
esac
exit 0
`);
  fs.chmodSync(sbx, 0o755);
  // the check command a Verify runs — fails on purpose, and writes to BOTH streams, so the test
  // proves the exit code survives and stderr is folded into what you read
  fs.mkdirSync(path.join(root, "home"), { recursive: true });
  fs.writeFileSync(path.join(root, "home", "config.json"), JSON.stringify({
    check_command: "echo building the thing; echo 'boom: the wheels came off' >&2; exit 3",
  }));
  return { root, ws, sbx, bin };
}

const freePort = () => new Promise(res => {
  const s = createServer();
  s.listen(0, "127.0.0.1", () => { const { port } = s.address(); s.close(() => res(port)); });
});

async function startServer(fx, port) {
  const build = spawnSync("cargo", ["build", "--bin", "skein-server"], { cwd: REPO, stdio: "inherit" });
  if (build.status !== 0) throw new Error("cargo build failed");
  const srv = spawn(path.join(REPO, "target/debug/skein-server"), {
    cwd: REPO,
    stdio: ["ignore", "pipe", "pipe"],
    env: {
      ...process.env,
      SKEIN_ADDR: `127.0.0.1:${port}`,
      SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
      SKEIN_LS_CMD: `${fx.sbx} ls --json`,   // verbatim through `sh -c` — the args matter
      SKEIN_HOME: path.join(fx.root, "home"),   // keep probe/kit installs out of the real store
      SKEIN_NO_GH_SECRET: "1",
      PATH: `${fx.bin}:${process.env.PATH}`,    // `sbx` resolves to the stub, never the real CLI
    },
  });
  let log = "";
  srv.stdout.on("data", d => { log += d; });
  srv.stderr.on("data", d => { log += d; });
  for (let i = 0; i < 100; i++) {
    try { if ((await fetch(`http://127.0.0.1:${port}/api/boxes`)).ok) return srv; } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  srv.kill();
  throw new Error(`server never came up on ${port}\n${log}`);
}

// ---------- the check harness ----------
const results = [];
let page;
async function check(name, fn) {
  try { await fn(); results.push([true, name]); console.log(`  ok    ${name}`); }
  catch (e) { results.push([false, name]); console.log(`  FAIL  ${name}\n        ${String(e.message || e).split("\n")[0]}`); }
}
/** The point of this file: present in the DOM is not enough — it has to be on screen. */
async function mustSee(sel, why) {
  const el = await page.$(sel);
  if (!el) throw new Error(`${why}: no element matches ${sel}`);
  const box = await el.boundingBox();
  if (!box || box.width === 0 || box.height === 0)
    throw new Error(`${why}: ${sel} is in the DOM but not visible (zero box) — a CSS rule is hiding it`);
  return el;
}
const text = async sel => ((await page.$eval(sel, e => e.textContent).catch(() => "")) || "").trim();
const settle = (ms = 700) => page.waitForTimeout(ms);
const openTab = async mode => { await page.evaluate(m => showBox(BOXNAME, m), mode); await settle(900); };

// ---------- run ----------
const fx = makeFixture();
const port = await freePort();
const srv = await startServer(fx, port);
const browser = await chromium.launch();
page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
// A failing check must report in seconds, not sit on Playwright's 30s default: when the page is
// broken, several checks fail at once and the whole run has to stay quick enough to keep running.
page.setDefaultTimeout(4000);
const noise = [];
const EXPECTED_404 = /\/file\?path=outside|\/files\?path=outside/;   // the refusal we assert below
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("console", m => { if (m.type() === "error" && !EXPECTED_404.test(m.location()?.url || "")) noise.push(`[console] ${m.text()}`); });
page.on("response", r => { if (r.status() >= 500) noise.push(`[${r.status()}] ${r.url()}`); });

await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: "domcontentloaded" });
await page.evaluate(b => { window.BOXNAME = b; }, BOX);
await page.waitForSelector("#fleet [data-name]", { timeout: 10000 });
await settle();

console.log("\nfleet");
await check("the box shows up as a row you can see", async () => {
  const row = await mustSee(`#fleet .row[data-name="${BOX}"]`, "fleet row");
  if (!(await row.textContent()).includes(BOX)) throw new Error("row does not name the box");
});

console.log("\nfiles");
await openTab("files");
await check("the tab opens and the pane fills", () => mustSee("#filespane .flist", "file list"));
await check("README opens by itself and renders as markdown", async () => {
  await mustSee("#filespane .fmd", "rendered markdown");
  if ((await text("#filespane .fmd h1")) !== "smoke") throw new Error("README did not render");
});
// THE regression: directories were rendered, then hidden by `body.docked .dir`
await check("folders are visible, not just present in the DOM", async () => {
  await mustSee('#filespane .fent[data-d="docs"]', "the docs/ folder");
  const dirs = await page.$$eval("#filespane .fent[data-d]", els => els.length);
  if (dirs < 3) throw new Error(`expected docs/, linked/ and outside/, saw ${dirs}`);
});
await check("a symlinked directory lists as a directory", () => mustSee('#filespane .fent[data-d="linked"]', "linked/"));
await check("clicking a folder steps into it", async () => {
  await page.click('#filespane .fent[data-d="docs"]');
  await settle();
  if (!(await text("#filespane .fcrumb")).includes("docs")) throw new Error("breadcrumb did not follow");
  await mustSee('#filespane .fent[data-f="docs/guide.md"]', "guide.md inside docs/");
});
await check("an .md inside a folder opens and renders", async () => {
  await page.click('#filespane .fent[data-f="docs/guide.md"]');
  await settle();
  if ((await text("#filespane .fmd h1")) !== "Guide") throw new Error("guide.md did not render");
});
await check("a relative link in a doc navigates the browser", async () => {
  await page.click('#filespane .fmd a[href="../README.md"]');
  await settle();
  if ((await text("#filespane .fmd h1")) !== "smoke") throw new Error("the link did not land on README");
  if ((await text("#filespane .fcrumb")).includes("docs")) throw new Error("the list did not follow the link out of docs/");
});
await check("a refused path explains itself without stranding you", async () => {
  await page.click('#filespane .fent[data-d="outside"]');
  await settle();
  await mustSee("#filespane .flist", "the list you were browsing");
  const note = await text("#filespane .note");
  if (!/escapes the workspace/.test(note)) throw new Error(`expected the server's reason, got "${note}"`);
});
await check("the breadcrumb goes back to the root", async () => {
  await page.click('#filespane .fent[data-d="docs"]');
  await settle();
  await page.click('#filespane .fcrumb a[data-p=""]');
  await settle();
  await mustSee('#filespane .fent[data-f="README.md"]', "README at the root");
});
await check("the open file survives leaving the tab and coming back", async () => {
  await page.click('#filespane .fent[data-f="notes.txt"]');
  await settle();
  await openTab("diff");
  await openTab("files");
  if (!(await text("#filespane .fraw")).includes("plain text")) throw new Error("the file did not reopen");
});

console.log("\nthe other tabs");
await check("diff renders something rather than an empty pane", async () => {
  await openTab("diff");
  await mustSee("#diffpane .diff", "the diff pane");
  if (!(await text("#diffpane"))) throw new Error("the diff pane is blank");
});
await check("session renders something rather than an empty pane", async () => {
  await openTab("session");
  await mustSee("#sesspane", "the session pane");
  if (!(await text("#sesspane"))) throw new Error("the session pane is blank");
});
await check("the terminal pane is on screen", async () => {
  await openTab("term");
  await mustSee("#termarea", "the terminal area");
});
await check("settings opens, and its panes switch to fields you can see", async () => {
  await page.click('header .kbtn[title^="Settings"]');
  await settle();
  await mustSee("#settings.open", "the settings dialog");
  await mustSee('.set-pane[data-pane="repos"].on', "the repos pane it opens on");
  await page.click('.set-navi[data-pane="github"]');
  await settle(300);
  await mustSee("#set-sshkey", "the SSH key field on the GitHub pane");
  await page.keyboard.press("Escape");
});

console.log("\nverify");
await check("Verify is offered on the box, and nothing ran it for me", async () => {
  await openTab("diff");
  await mustSee("#dverify", "the Verify button");
  if (await page.$(`#fleet .row[data-name="${BOX}"] .vchip:not([style*='none'])`))
    throw new Error("a check result appeared without anyone asking for one — verify must never self-trigger");
});
await check("a failing check reports its exit code and both output streams", async () => {
  await page.click("#dverify");
  await page.waitForSelector("#vout.open", { timeout: 20000 });
  const meta = await text("#vout-meta"), body = await text("#vout-body");
  if (!/exit 3/.test(meta)) throw new Error(`expected the check's own exit code, got "${meta}"`);
  if (!/building the thing/.test(body)) throw new Error("stdout is missing from the output");
  if (!/wheels came off/.test(body)) throw new Error("stderr was not folded in — the useful half of a failure");
  await page.keyboard.press("Escape");
});
await check("the result lands on the row as a chip you can reopen", async () => {
  // scope to OUR box: skein keeps the sandbox it runs inside on the board too, and that row is first
  const sel = `#fleet .row[data-name="${BOX}"] .vchip`;
  await page.waitForFunction(s => document.querySelector(s)?.textContent?.includes("failed"), sel, { timeout: 8000 });
  const chip = await mustSee(sel, "the check chip");
  if (!(await chip.getAttribute("title")).includes("echo building")) throw new Error("the chip must name the command behind it");
  await chip.click();
  await page.waitForSelector("#vout.open", { timeout: 8000 });
  await page.keyboard.press("Escape");
});

console.log("\nquiet");
await check("no page errors and no 5xx along the way", () => {
  if (noise.length) throw new Error(noise.join(" | "));
});

// ---------- report ----------
const failed = results.filter(([ok]) => !ok);
if (failed.length) {
  const shot = path.join(fx.root, "failure.png");
  await page.screenshot({ path: shot, fullPage: false });
  console.log(`\n${failed.length} of ${results.length} checks failed`);
  console.log(`screenshot: ${shot}\nfixture kept for inspection: ${fx.root}`);
} else {
  console.log(`\nall ${results.length} checks passed`);
}
await browser.close();
srv.kill();
if (!failed.length) fs.rmSync(fx.root, { recursive: true, force: true });
process.exit(failed.length ? 1 : 0);
