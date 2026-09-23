// "Restart on new build" in Settings -> Update, against a real doorway (SKEIN-1029).
//
// **The old build is running and the new one is installed**, which is the state an install that
// could not swap the cockpit leaves behind, and the state the owner asked for one button out of.
// So this suite builds that state for real rather than describing it to a lifted function:
//
// - a real `src/server-doorway.py`, at the path `fleet::server_doorway_path` names, holding this
//   suite's port and running the server at `fleet::server_path` behind it;
// - the OLD build there first: this tree's own `skein-server`;
// - the NEW build renamed into place over it once the page is up, as an install renames one: the
//   same binary with its revision stamp rewritten to one no build of this tree can carry. Only the
//   stamp differs, so the only thing the checks can be reading is which file the doorway ran.
//
// Then one click, and the page has to end up served by the new build and say so, with the doorway
// that held the port the whole time still the same process. **What would make that fail:** the
// reload not being sent (`update::restart` never calling `fleet::reload_server`): the old build
// goes on answering, the pane's wait runs out, and the "now running" check below fails by name.
//
// The second half is a server no doorway is behind — started the way every other suite starts
// one. A reload would replace nothing that is answering, so the button has to say that and name
// the process holding the port, with the command that stops it, rather than send anything.
//
//   node tests/ui/restart.mjs
import { chromium } from "playwright";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { fixtureRoot, freshFixture, openDoor, serverBinary } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { stub } from "./harness/github.mjs";
import { startServer } from "./harness/server.mjs";
const API_TOKEN = "t".repeat(64);
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

const root = freshFixture(fixtureRoot(), "ui-restart");

/** A store and a fleet root beside it, as `startServer` requires, under `root`. */
function makeFleet(name) {
  const home = path.join(root, `home-${name}`);
  const fleet = path.join(root, `fleet-${name}`);
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(path.join(fleet, ".skein"), { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  // A sandbox name, because the doorway's stamp is read through `place::own_sandbox`, which runs
  // on this machine either way.
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  return { home, fleet, dot: path.join(fleet, ".skein") };
}

// The old build and the new one. The new one is the old one's bytes with the revision stamp
// `build.rs` compiles in replaced, same length, so `--version` and `/api/health` both say it.
const old = serverBinary();
const OLD = /\(([^)]*)\)\s*$/.exec(execFileSync(old, ["--version"], { encoding: "utf8" }))?.[1] || "";
if (!/^[0-9a-f]{7,}/.test(OLD)) {
  throw new Error(`restart.mjs needs a build stamped with a git revision, and ${old} says ${JSON.stringify(OLD)}`);
}
const NEW = OLD.replace(/^[0-9a-f]+/, h => [...h].map(c => ((parseInt(c, 16) + 8) % 16).toString(16)).join(""));
const newBytes = (() => {
  const bytes = fs.readFileSync(old);
  const from = Buffer.from(OLD), to = Buffer.from(NEW);
  for (let at = bytes.indexOf(from); at !== -1; at = bytes.indexOf(from, at + from.length)) to.copy(bytes, at);
  return bytes;
})();

/** Rename a new build into place at `server`, the way an install does. */
function install(server) {
  fs.writeFileSync(`${server}.new`, newBytes, { mode: 0o755 });
  fs.renameSync(`${server}.new`, server);
}

const github = await stub(({ url, send }) =>
  /^\/repos\/[^/]+\/[^/]+\/commits\/[^/]+$/.test(url) && send(200, { sha: "deadbeef".repeat(5) }));
const envFor = (fx, port) => ({
  SKEIN_REGISTRY: path.join(root, "sandboxes.json"),
  SKEIN_HOME: fx.home,
  SKEIN_FLEET_ROOT: fx.fleet,
  SKEIN_GITHUB_API: github.url,
  SKEIN_SOURCE_URL: "https://github.com/acme/skein.git",
  // The port the doorway's stamp is compared against — `fleet::server_sandbox_port`.
  SKEIN_SERVER_PORT: String(port),
});
fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");

const { check, value, report } = ledger();
const browser = await chromium.launch();
const running = [];
const logs = [];

/** Open Settings -> Update on `port` and wait for the pane to have read `/api/update` once. */
async function openPane(port) {
  const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
  const seen = erring(page, { say: (kind, text) => `${kind}: ${text}` });
  await page.goto(`http://127.0.0.1:${port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(800);
  await page.evaluate(() => openSettings("update"));
  return { page, ...seen };
}
const until = async (page, f, ms = 8000) => {
  for (let t = 0; t < ms; t += 250) { if (await f()) return true; await page.waitForTimeout(250); }
  return !!(await f());
};
const said = page => page.$eval("#upd-restart-said", e => e.firstChild.textContent.trim()).catch(() => "");
const buttonShown = async page => {
  const box = await page.locator("#upd-restart").first().boundingBox({ timeout: 500 }).catch(() => null);
  return !!box && box.width > 0 && box.height > 0;
};
const health = port => fetch(`http://127.0.0.1:${port}/api/health`, {
  headers: { Authorization: `Bearer ${API_TOKEN}` }, signal: AbortSignal.timeout(10000),
}).then(r => r.json()).then(h => h.build).catch(() => "");

try {
  // --- behind a doorway: one click, and the page is served by the new build ----------------------
  {
    const fx = makeFleet("door");
    const doorway = path.join(fx.dot, "server-doorway.py");
    const server = path.join(fx.dot, "skein-server");
    const stamp = path.join(fx.dot, "server.door");
    fs.writeFileSync(doorway, fs.readFileSync(path.join(REPO, "src", "server-doorway.py")));
    fs.symlinkSync(old, server);
    const door = await openDoor();
    const { port } = door;
    // `python3` by that name, and the doorway by its full path: `fleet::reload_command`'s pattern
    // is `^python[0-9.]* <doorway>( |$)`, which is how the doorway re-execs itself too.
    const { srv, log } = await startServer({
      door, token: API_TOKEN, env: envFor(fx, port),
      program: ["python3", doorway, String(port), server, stamp],
    });
    running.push(srv);
    logs.push(["behind a doorway", log]);
    const doorPid = () => fs.readFileSync(stamp, "utf8").trim().split(/\s+/)[0];
    const doorBefore = doorPid();

    const { page, errors, sayBlips } = await openPane(port);
    await until(page, async () => (await page.$("#set-update .set-revs")) !== null);
    value("with the running build installed, there is no restart to offer",
      { build: await health(port), button: await buttonShown(page) }, { build: OLD, button: false });

    install(server);
    await page.evaluate(() => loadUpdate());
    await until(page, () => buttonShown(page));
    value("once a new build is installed, the pane offers to restart onto it",
      { installed: await page.evaluate(() => updateState.skein.installed), button: await buttonShown(page) },
      { installed: NEW, button: true });

    // A mark on this page, so "the page is served by the new build" cannot be satisfied by a reload
    // that threw the page away and loaded a fresh one.
    await page.evaluate(() => { window.__before_restart = 1; });
    await page.locator("#upd-restart").first().click();
    let text = "";
    await until(page, async () => (text = await said(page)).startsWith("Now running"), 45000);
    // **The check the sabotage fails by name.** With the reload never sent, the pane's own wait runs
    // out and it says "The restart did not take: … still served by <old>".
    value("one click, and the page says it is now running the new build", text, `Now running ${NEW}.`);
    value("and the page really is served by it, from the same page and the same doorway",
      { build: await health(port), samePage: await page.evaluate(() => window.__before_restart === 1),
        sameDoorway: doorPid() === doorBefore },
      { build: NEW, samePage: true, sameDoorway: true });
    value("and with nothing newer installed, the offer is gone", await buttonShown(page), false);
    sayBlips();
    value("the pane raised no page errors while its server was replaced", errors, []);
    await page.close();
  }

  // --- no doorway: nothing is sent, and the pane names what holds the port ------------------------
  {
    const fx = makeFleet("bare");
    install(path.join(fx.dot, "skein-server"));
    const door = await openDoor();
    const { port } = door;
    const { srv, log } = await startServer({ door, token: API_TOKEN, env: envFor(fx, port) });
    running.push(srv);
    logs.push(["no doorway", log]);
    const { page, errors, sayBlips } = await openPane(port);
    await until(page, () => buttonShown(page));
    await page.locator("#upd-restart").first().click();
    let text = "";
    await until(page, async () => (text = await said(page)).startsWith("Not restarted"));
    value("with no doorway behind the server, the press says so and names the process on the port",
      { says: text.startsWith(`Not restarted: no skein doorway holds port ${port}, so this server (pid ${srv.pid})`),
        command: await page.$eval("#upd-restart-cmd", e => e.textContent.trim()).catch(() => ""),
        stillServing: await health(port) },
      { says: true, command: `kill ${srv.pid}`, stillServing: OLD });
    sayBlips();
    value("and raised no page errors", errors, []);
    await page.close();
  }
} catch (e) {
  await check("the suite could run at all", () => { throw e; });
}

await browser.close();
for (const srv of running) srv.kill();
github.close();
const failed = report();
if (failed.length) for (const [which, log] of logs) console.log(`\nserver log (${which}):\n${log()}`);
else { try { fs.rmSync(root, { recursive: true, force: true }); } catch {} }
process.exit(failed.length ? 1 : 0);
