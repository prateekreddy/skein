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
  // A second box that `sbx ls` does NOT report (so: not running) whose host clone holds nothing but
  // .git — exactly the shape that made the Files tab show a blank pane while the box had a tree.
  const bare = path.join(root, "bare-clone");
  fs.mkdirSync(path.join(bare, ".git", "refs"), { recursive: true });
  fs.writeFileSync(path.join(bare, ".git", "HEAD"), "ref: refs/heads/master\n");
  fs.writeFileSync(path.join(root, "sandboxes.json"), JSON.stringify({
    [BOX]: { branch: "main", dir: ws, lastSeen: new Date().toISOString(), status: "" },
    "bare-box": { branch: "master", dir: bare, lastSeen: new Date().toISOString(), status: "" },
  }));
  // a real git repo, so a verify can fingerprint what it checked the way it would in a box
  const git = (...a) => spawnSync("git", ["-C", ws, ...a], { stdio: "ignore" });
  const commit = (m) => git("-c", "user.email=smoke@test", "-c", "user.name=smoke", "commit", "-qm", m);
  git("init", "-q");
  git("add", "-A");
  commit("fixture");
  // A real `origin` with a real base branch, because the diff is measured against the REMOTE base
  // now — a fixture with only local refs would pass while the thing under test never ran.
  const remote = path.join(root, "remote.git");
  spawnSync("git", ["init", "-q", "--bare", "-b", "master", remote], { stdio: "ignore" });
  git("remote", "add", "origin", remote);
  git("push", "-q", "origin", "HEAD:master");
  git("fetch", "-q", "origin");
  // …then move the branch ahead of it, so the patch has to come from the merge-base and not HEAD.
  fs.writeFileSync(path.join(ws, "docs", "guide.md"), "# Guide\n\nBack to [the readme](../README.md).\n\nA committed change, ahead of origin/master.\n");
  git("add", "-A");
  commit("work on the branch");
  fs.writeFileSync(path.join(ws, "notes.txt"), "plain text\nand an uncommitted edit\n");
  // Stand-in for sbx: the cockpit asks it for the fleet, and must never reach the real one. It
  // serves `exec` too, running the command in the fixture workspace the way a box would — so the
  // verify path (wrapper script, exit-code marker, stored record) runs for real, without a sandbox.
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, `#!/bin/sh
case "$1" in
  ls)   echo '[{"name":"${BOX}","status":"running","agent":"claude","workspace":"${ws}"}]'; exit 0 ;;
  exec) shift; shift; cd "${ws}" || exit 1; HOME="${root}/boxhome" exec "$@" ;;
esac
exit 0
`);
  fs.chmodSync(sbx, 0o755);
  // the box's own conversation record — what the Transcript tab reads instead of the screen
  const proj = path.join(root, "boxhome", ".claude", "projects", "-fixture");
  fs.mkdirSync(proj, { recursive: true });
  // 300KB of bookkeeping FIRST, so a 256KB window lands mid-file: that exercises the partial-line
  // drop and the "load older" paging, which a small fixture would silently skip
  const filler = Array.from({ length: 4000 }, (_, i) =>
    JSON.stringify({ type: "mode", mode: "default", n: i, pad: "y".repeat(50) })).join("\n") + "\n";
  fs.writeFileSync(path.join(proj, "session.jsonl"), [
    JSON.stringify({ type: "user", timestamp: "2026-07-31T07:00:00Z", message: { role: "user", content: "the oldest message in the record" } }),
    filler.trimEnd(),
    JSON.stringify({ type: "user", timestamp: "2026-07-31T08:10:00Z", message: { role: "user", content: "why did the board go blank" } }),
    JSON.stringify({ type: "assistant", timestamp: "2026-07-31T08:10:04Z", message: { role: "assistant", content: [
      { type: "thinking", thinking: "private reasoning that is not the conversation" },
      { type: "text", text: "Because the **box rebooted** and tmux started fresh." },
      { type: "tool_use", name: "Bash", input: { command: "uptime -s" } }] } }),
    JSON.stringify({ type: "user", timestamp: "2026-07-31T08:10:05Z", message: { role: "user", content: [{ type: "tool_result", content: "X".repeat(5000) }] } }),
  ].join("\n") + "\n");
  // the check command a Verify runs — fails on purpose, and writes to BOTH streams, so the test
  // proves the exit code survives and stderr is folded into what you read
  fs.mkdirSync(path.join(root, "home"), { recursive: true });
  fs.writeFileSync(path.join(root, "home", "config.json"), JSON.stringify({
    check_command: "echo building the thing; echo 'boom: the wheels came off' >&2; exit 3",
  }));
  // a registered repo, so the settings pane has a card to open and edit
  fs.writeFileSync(path.join(root, "home", "repos.json"), JSON.stringify([
    { id: "smoke", source: "/src/smoke", work: ws, store: path.join(root, "store"), agent: "claude",
      check: "", plane_project: "", sync_connection: "" },
  ]));
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
// Refusals this run provokes ON PURPOSE and asserts elsewhere: the workspace-escape guard,
// removing a connection a repo still uses, and probing that the retired collision route is gone.
// Everything else counts as noise.
const EXPECTED_404 = /\/file\?path=outside|\/files\?path=outside|\/api\/repos\/[^/]+\/settings|\/api\/sync\/connections|\/api\/collisions/;
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
await check("the listing says the box's own tree answered", async () => {
  await openTab("files");
  const d = await page.evaluate(b => fetch(`/api/boxes/${b}/files?path=`).then(r => r.json()), BOX);
  if (d.source !== "box") throw new Error(`a running box must be read from the box itself, got "${d.source}"`);
  if (await page.$("#filespane .fsrc.host")) throw new Error("no host-clone badge should show for a live box");
});
await check("a fallback tree announces itself in the pane, not just a tooltip", async () => {
  // The Files tab shipped reading the host clone as if it were the box's tree. The badge alone
  // hid the reason behind a hover; provenance has to be readable without pointing at anything.
  const d = await page.evaluate(n => fetch(`/api/boxes/${n}/files?path=`).then(r => r.json()), "bare-box");
  if (d.source !== "host") throw new Error(`a stopped box's listing comes from the host, got "${d.source}"`);
  if (!/host clone/.test(d.note || "")) throw new Error(`and must say so in words: "${d.note}"`);
});
await check("a clone with nothing but .git says so instead of looking broken", async () => {
  await page.evaluate(() => showBox("bare-box", "files"));
  await settle(1200);
  const note = await text("#filespane .fnote");
  if (!/no files in it/.test(note)) throw new Error(`an empty checkout must explain itself, got "${note}"`);
  if (!/host clone/.test(await text("#filespane .fcrumb"))) throw new Error("it must say which tree it read");
  await mustSee("#filespane .fsrc.host", "the host-clone badge");
  await page.evaluate(b => showBox(b, "files"), BOX);
  await settle(900);
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
await check("the diff is measured against the remote base branch, and says so", async () => {
  // The old path ran `git -C <host clone>`, which for a clone-mode box is a different checkout on
  // a different branch — a confidently wrong answer. This asserts the box computed it.
  const banner = await mustSee("#diffpane .dbase", "the base banner");
  const said = (await banner.textContent()).trim();
  if (!/origin\/master/.test(said)) throw new Error(`should name the remote base it used, got "${said}"`);
  const body = await text("#diffpane");
  if (!/ahead of origin\/master/.test(body))
    throw new Error("a committed change on the branch is missing — the range started at HEAD, not the merge-base");
  if (!/an uncommitted edit/.test(body))
    throw new Error("uncommitted work is missing from the patch");
  const d = await page.evaluate(n => fetch(`/api/boxes/${n}/diff`).then(r => r.json()), BOX);
  if (d.source !== "box") throw new Error(`the running box should answer for itself, got "${d.source}"`);
  if (d.note) throw new Error(`a current answer has nothing to explain, got "${d.note}"`);
  if (d.base !== "origin/master") throw new Error(`unexpected base "${d.base}"`);
});
await check("the collision radar is gone, not merely hidden", async () => {
  const r = await page.evaluate(() => fetch("/api/collisions").then(r => r.status));
  if (r !== 404) throw new Error(`/api/collisions still answers ${r}`);
  const leftovers = await page.evaluate(() => typeof loadCollisions);
  if (leftovers !== "undefined") throw new Error("the radar's client code is still loaded");
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

console.log("\nwork tracking");
await check("nothing offers to spend a token before one is configured", async () => {
  await openTab("diff");
  if (await page.$("#dtrack"))
    throw new Error("Track work is offered with no connection configured — it can only fail");
});
await check("an unconfigured host says what a connection is, not just that it's missing", async () => {
  await page.click('header .kbtn[title^="Settings"]');
  await settle();
  await page.click('.set-navi[data-pane="tracking"]');
  await settle(300);
  await mustSee("#set-conn-add", "the add-a-connection button");
  const note = await text("#set-syncnote");
  if (!/no connections yet/i.test(note)) throw new Error(`unconfigured should say so plainly, got "${note}"`);
});
await check("a connection is a gateway and its token in one card", async () => {
  await page.click("#set-conn-add");
  await settle(300);
  const card = await mustSee('.ccard[data-conn=""]', "the new connection card");
  await page.fill('.ccard[data-conn=""] [data-field="label"]', "smoke tracker");
  await page.fill('.ccard[data-conn=""] [data-field="gateway_url"]', "https://mcp.smoke.example/");
  await page.fill('.ccard[data-conn=""] [data-field="token"]', "plane_api_smoke_secret");
  await page.click('.ccard[data-conn=""] [data-saveconn]');
  await page.waitForSelector('.ccard[data-conn="smoke-example"] .cstate.ready', { timeout: 5000 });
  const label = await page.$eval('.ccard[data-conn="smoke-example"] [data-field="label"]', e => e.value);
  if (label !== "smoke tracker") throw new Error(`the label should survive the save, got "${label}"`);
});
await check("the stored token is never handed back to the browser", async () => {
  // The one credential whose leak bypasses every lease in the fleet. It must not arrive here at
  // all — not in the field, not in the status, not anywhere in the settings response.
  const sel = '.ccard[data-conn="smoke-example"] [data-field="token"]';
  const field = await page.$eval(sel, el => el.value);
  if (field) throw new Error(`the token field was pre-filled with "${field}" — it must never round-trip`);
  const type = await page.$eval(sel, el => el.type);
  if (type !== "password") throw new Error(`the token field is type="${type}", so it is shoulder-readable`);
  const [sync, settings] = await page.evaluate(async () =>
    Promise.all([fetch("/api/sync").then(r => r.text()), fetch("/api/settings").then(r => r.text())]));
  if (/plane_api_/.test(sync + settings))
    throw new Error("a Plane token reached the browser through /api/sync or /api/settings");
  if (!/"token_set":true/.test(sync)) throw new Error(`/api/sync must report whether one is stored: ${sync}`);
  await mustSee('.ccard[data-conn="smoke-example"] [data-forget]', "the Forget button, once one is stored");
});
await check("editing the URL doesn't quietly forget the token", async () => {
  // A blank token field means "I came here to change the URL". Reading it as "delete my
  // credential" would break every box on the connection for a one-character edit.
  const url = '.ccard[data-conn="smoke-example"] [data-field="gateway_url"]';
  await page.fill(url, "https://mcp.smoke.example/mcp");
  await page.press(url, "Tab");
  await settle(400);
  await mustSee('.ccard[data-conn="smoke-example"] .cstate.ready', "still ready after a URL-only edit");
  await page.keyboard.press("Escape");
});

console.log("\nsettings");
await check("a repo is a card that says what it's configured to do", async () => {
  await page.click('header .kbtn[title^="Settings"]');
  await settle();
  await mustSee("#settings.open", "the settings dialog");
  const card = await mustSee('.rcard[data-card="smoke"]', "the repo card");
  const tags = await card.$$eval(".rtag", els => els.map(e => e.textContent));
  if (!tags.some(t => /building the thing/.test(t))) throw new Error(`the inherited check should show: ${tags}`);
  if (await page.$(".rcard.open")) throw new Error("cards should start collapsed");
});
await check("opening it shows labelled fields, not bare inputs", async () => {
  await page.click('.rcard[data-card="smoke"] .rhead');
  await settle(300);
  const titles = await page.$$eval('.rcard[data-card="smoke"] .set-title', els => els.map(e => e.textContent.replace("saved","").trim()));
  for (const want of ["Check command", "Plane project", "Work tracking"])
    if (!titles.includes(want)) throw new Error(`missing field "${want}" — got ${titles}`);
});
await check("a repo picks a connection instead of restating half of one", async () => {
  // Typing a gateway URL here could only ever name half a connection: the token that mints at it
  // lives with the gateway, so the repo has to select the pair.
  const sel = '.rcard[data-card="smoke"] select[data-key="sync_connection"]';
  const picker = await mustSee(sel, "the connection picker");
  const options = await picker.$$eval("option", els => els.map(e => [e.value, e.textContent]));
  if (options[0][0] !== "" || !/not tracked/i.test(options[0][1]))
    throw new Error(`"not tracked" has to be sayable, got ${JSON.stringify(options)}`);
  if (!options.some(([v]) => v === "smoke-example"))
    throw new Error(`the configured connection should be offered, got ${JSON.stringify(options)}`);
  await page.selectOption(sel, "smoke-example");
  await page.waitForFunction(() => [...document.querySelectorAll('.rcard[data-card="smoke"] .rtag')].some(t => /smoke tracker/.test(t.textContent)), null, { timeout: 5000 });
  await mustSee('.rcard[data-card="smoke"].open', "the card stays open after saving");
  const saved = await fetch(`http://127.0.0.1:${port}/api/repos`).then(r => r.json());
  if (saved.find(r => r.id === "smoke").sync_connection !== "smoke-example")
    throw new Error("the picked connection should be what's stored");
});
await check("a connection in use is not removed out from under its repos", async () => {
  await page.click('.set-navi[data-pane="tracking"]');
  await settle(300);
  const used = await page.$eval('.ccard[data-conn="smoke-example"] .cuse', e => e.textContent);
  if (!/smoke/.test(used)) throw new Error(`the card should name who depends on it, got "${used}"`);
  page.once("dialog", d => d.accept());   // the "remove it anyway?" confirm
  await page.click('.ccard[data-conn="smoke-example"] [data-dropconn]');
  await settle(500);
  await mustSee('.ccard[data-conn="smoke-example"]', "the connection survives a refused removal");
  const toasted = await text("#toast");
  if (!/smoke/.test(toasted)) throw new Error(`the refusal should name the repo, got "${toasted}"`);
  await page.click('.set-navi[data-pane="repos"]');
  await settle(300);
});
await check("a per-box ceiling is settable, and says what happens without one", async () => {
  // The fleet fields configure a POOL; these two configure what one box may take out of it. Without
  // the second pair there is no ceiling at all, and a runaway build kills other boxes' agents — so
  // the pane has to make the per-box cap findable and say what it is protecting against.
  await page.click('.set-navi[data-pane="workflow"]');
  await settle(300);
  await mustSee("#set-boxmax", "the per-box hard cap");
  await mustSee("#set-boxhigh", "the per-box throttle");
  const why = await page.$eval("#set-boxmax", e => e.closest(".set-field").querySelector(".desc").textContent);
  for (const claim of ["runaway", "another box"])
    if (!why.includes(claim)) throw new Error(`the cap should explain "${claim}": ${why}`);
  // Blank must read as "derived", never as "unlimited" — the difference is a fleet that survives a
  // bad build and one that doesn't.
  const hint = await page.$eval("#set-boxmax", e => e.placeholder);
  if (!/%/.test(hint)) throw new Error(`blank should show what it derives to, got "${hint}"`);
  // Applying these is live; applying fleet memory is a rebuild. Both are offered, and they must not
  // look like the same button.
  const cheap = await page.$eval("#set-applylimits", e => e.closest(".set-field").querySelector(".desc").textContent);
  if (!/live/.test(cheap)) throw new Error(`the live path should say so: ${cheap}`);
  const dear = await page.$eval("#set-resize", e => e.closest(".set-field").querySelector(".desc").textContent);
  if (!/rebuild/i.test(dear)) throw new Error(`the destructive path should say so: ${dear}`);
});
await check("AI enrichment is a visible setting, not folklore in an env var", async () => {
  // It existed for months as $SKEIN_AI only, so nobody knew it was there. The toggle has to be
  // findable, and it has to say what would actually happen — "on" with no `claude` on PATH is a
  // state a checkbox alone can never show.
  await page.click('.set-navi[data-pane="workflow"]');
  await settle(300);
  const box = await mustSee("#set-ai", "the AI enrichment toggle");
  if (await box.isChecked()) throw new Error("it must default to off — the calls share the fleet's rate limit");
  const why = await page.$eval("#set-ai", e => e.closest(".set-row").querySelector(".desc").textContent);
  for (const claim of ["rate-limit", "Continue N", "subscription"])
    if (!why.includes(claim)) throw new Error(`the description should explain "${claim}": ${why}`);
  const note = await text("#set-ainote");
  if (!/^off —/.test(note)) throw new Error(`it should report its real state, got "${note}"`);
  await page.click("#set-ai");
  await page.click("#set-go");
  await settle(600);
  const cfg = await page.evaluate(() => fetch("/api/settings").then(r => r.json()));
  if (cfg.ai_enrichment !== true) throw new Error("the toggle didn't persist");
  const h = await page.evaluate(() => fetch("/api/health").then(r => r.json()));
  if (!/^on/.test(h.ai.detail)) throw new Error(`doctor should now say it's on, got "${h.ai.detail}"`);
  if (h.ai.ok !== true) throw new Error("opt-in-and-off is not a fault, so this must never report unhealthy");
  // Save closed the dialog — put the pane back where the following checks expect it.
  await page.click('header .kbtn[title^="Settings"]');
  await settle();
  await page.click('.set-navi[data-pane="repos"]');
  await settle(300);
});
await check("the pane doesn't pretend Save applies to repo cards", async () => {
  const shown = await page.$$eval("#settings .set-foot .primary", els => els.filter(e => e.offsetParent).length);
  if (shown) throw new Error("Save is offered on a pane whose fields already saved themselves");
  if (!/saves as you leave a field/.test(await text("#set-hint"))) throw new Error("nothing says when these save");
  // and it comes back on a pane that IS a form
  await page.click('.set-navi[data-pane="workflow"]');
  await settle(250);
  await mustSee("#settings .set-foot .primary", "Save on the Workflow pane");
  await page.click('.set-navi[data-pane="repos"]');
  await settle(250);
});
await check("a gateway that isn't a URL is refused, not stored", async () => {
  await page.click('.set-navi[data-pane="tracking"]');
  await settle(300);
  const field = '.ccard[data-conn="smoke-example"] [data-field="gateway_url"]';
  await page.fill(field, "mcp.example.net");
  await page.press(field, "Tab");
  await settle(900);
  const sync = await fetch(`http://127.0.0.1:${port}/api/sync`).then(r => r.json());
  if (sync.connections[0].gateway_url !== "https://mcp.smoke.example/mcp")
    throw new Error(`a rejected value must not overwrite the stored one, got ${sync.connections[0].gateway_url}`);
  await page.keyboard.press("Escape");
});
await check("Track work appears once THIS box's repo has a usable connection", async () => {
  // The payoff, and the reason readiness is per box: "some connection somewhere is ready" says
  // nothing about this one.
  await openTab("diff");
  const btn = await mustSee("#dtrack", "Track work, now that the repo picks a ready connection");
  const title = await btn.getAttribute("title");
  if (!/smoke tracker/.test(title)) throw new Error(`it should name the backlog it would mint at, got "${title}"`);
});

await check("settings is usable while a box is open (the docked layout hides nothing of it)", async () => {
  // `body.docked footer {display:none}` — written for the fleet's key hints — also matched the
  // settings dialog's own <footer>, so Save and Cancel vanished whenever any box tab was open.
  await page.click('header .kbtn[title^="Settings"]');   // the previous check closed it
  await settle();
  await page.click('.set-navi[data-pane="workflow"]');
  await settle(250);
  for (const [sel, what] of [["#settings .set-foot .primary", "Save"], ["#settings .set-navi", "the pane rail"],
                             ["#settings .set-scroll", "the pane body"], ["#settings .set-x", "the close button"]])
    await mustSee(sel, what);
  await page.keyboard.press("Escape");
});

console.log("\ntranscript");
await check("the conversation renders from the box's own record, not the screen", async () => {
  await openTab("tx");
  await mustSee("#txpane .txwrap", "the transcript pane");
  const body = await text("#txpane");
  if (!/why did the board go blank/.test(body)) throw new Error("the human's message is missing");
  if (!/box rebooted/.test(body)) throw new Error("the agent's reply is missing");
  await mustSee("#txpane .txmsg.assistant .txtext strong", "markdown rendered in the reply");
});
await check("tool calls are summarised and their payloads left out", async () => {
  const body = await text("#txpane");
  if (!/Bash\(uptime -s\)/.test(body)) throw new Error("the tool call should be named with what it ran");
  if (/XXXXXXXXXX/.test(body)) throw new Error("a tool_result payload leaked into the view");
  if (/private reasoning/.test(body)) throw new Error("thinking is not the conversation and must not render");
});
await check("a record bigger than the window offers to page backwards", async () => {
  if (!/on disk/.test(await text("#txpane .txhead"))) throw new Error("no size/what-was-read line");
  await mustSee("#txpane .txmore", "the load-older button");
  const body = await text("#txpane");
  if (/the oldest message in the record/.test(body)) throw new Error("the first window should not reach the oldest message");
});
await check("loading older reaches the beginning and says so", async () => {
  await page.click("#txpane .txmore");
  await settle(1200);
  const body = await text("#txpane");
  if (!/the oldest message in the record/.test(body)) throw new Error("paging back did not reach the oldest message");
  if (!/why did the board go blank/.test(body)) throw new Error("paging back lost the newest messages");
  if (!/the beginning of this record/.test(await text("#txpane .txolder")))
    throw new Error("a fully-read record must say so rather than keep offering more");
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

// ---------- the mouth ----------
// The one section here that does not assert something visible, because there is nothing to see: the
// output is sound. Everything else about the rule still holds — this is the real page in a real
// browser running the real `notify()`, with only the speaker itself replaced by a recorder. A stub
// of the page's own logic would agree with whatever the page happened to do.
console.log("\nvoice");
const spoken = () => page.evaluate(() => window.__said.splice(0));
await check("the speaker is wired to a switch of its own", async () => {
  await page.evaluate(() => {
    window.__said = [];
    // Record instead of speak. Headless Chromium has no voices, so a real utterance would be
    // silently dropped and every assertion below would pass on an empty room.
    speechSynthesis.speak = u => window.__said.push(u.text);
    Object.defineProperty(speechSynthesis, "pending", { get: () => false, configurable: true });
    // The board only speaks while you are looking elsewhere, which in a headless run is ambiguous —
    // pin it, so this tests the announcement and not Playwright's idea of focus.
    document.hasFocus = () => false;
  });
  await page.click("#voice");
  const said = await spoken();
  if (!said.length) throw new Error("turning voice on said nothing — Safari needs that gesture to speak later");
  if (!/voice on/i.test(said[0])) throw new Error(`expected a confirmation, got ${JSON.stringify(said[0])}`);
});
await check("a box that starts asking says what it is asking", async () => {
  const said = await page.evaluate(() => {
    // Two snapshots: the first seeds the prior state (a fresh tab must not announce a fleet that
    // was already paused), the second is the turn.
    const working = [{ name: "example-box-1", state: "working", pause: "none" }];
    const asking = [{ name: "example-box-1", state: "needs-input", pause: "ask", blocked_kind: "permission",
                      headline: "Run `rm -rf /boxes/smoke/tree/build`?" }];
    notify(working); window.__said.length = 0;
    notify(asking);
    return window.__said.slice();
  });
  if (said.length !== 1) throw new Error(`expected one sentence, got ${JSON.stringify(said)}`);
  const line = said[0];
  if (!/era s 6/i.test(line)) throw new Error(`the name is unspoken or unreadable: ${JSON.stringify(line)}`);
  if (!/wants permission/.test(line)) throw new Error(`the kind of ask is missing: ${JSON.stringify(line)}`);
  if (!/rm -rf build/.test(line)) throw new Error(`the ask itself is missing or the path was read out: ${JSON.stringify(line)}`);
  if (/`|\/boxes\//.test(line)) throw new Error(`unspeakable text survived: ${JSON.stringify(line)}`);
});
await check("a burst becomes a count, not a monologue", async () => {
  const said = await page.evaluate(() => {
    const calm = ["a", "b", "c"].map(n => ({ name: n, state: "working", pause: "none" }));
    const turned = [{ name: "a", state: "needs-input", pause: "ask", headline: "one?" },
                    { name: "b", state: "needs-input", pause: "ask", headline: "two?" },
                    { name: "c", state: "done", pause: "none" }];
    notify(calm); window.__said.length = 0;
    notify(turned);
    return window.__said.slice();
  });
  if (said.length !== 1) throw new Error(`a burst must collapse to one utterance, got ${JSON.stringify(said)}`);
  if (!/2 of 3 boxes need you/.test(said[0])) throw new Error(`expected the owed count, got ${JSON.stringify(said[0])}`);
});
await check("silence when nothing turned, and when you are looking at the board", async () => {
  const said = await page.evaluate(() => {
    const asking = [{ name: "example-box-1", state: "needs-input", pause: "ask", headline: "still?" }];
    notify(asking); window.__said.length = 0;
    notify(asking);                       // same state twice: nothing turned, nothing to say
    document.hasFocus = () => true;
    notify([{ name: "example-box-1", state: "done", pause: "none" }]);   // a real turn, but you are here
    document.hasFocus = () => false;
    return window.__said.slice();
  });
  if (said.length) throw new Error(`it spoke when it should not have: ${JSON.stringify(said)}`);
});
await check("the switch silences it", async () => {
  await page.click("#voice");            // off
  const said = await page.evaluate(() => {
    window.__said.length = 0;
    notify([{ name: "example-box-1", state: "working", pause: "none" }]);
    notify([{ name: "example-box-1", state: "needs-input", pause: "ask", headline: "anything?" }]);
    return window.__said.slice();
  });
  if (said.length) throw new Error(`silenced and still talking: ${JSON.stringify(said)}`);
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
// SKEIN_KEEP=1 leaves the fixture behind so you can point a server at it and look at the thing
if (!failed.length && !process.env.SKEIN_KEEP) fs.rmSync(fx.root, { recursive: true, force: true });
else if (!failed.length) console.log(`fixture kept (SKEIN_KEEP): ${fx.root}`);
process.exit(failed.length ? 1 : 0);
