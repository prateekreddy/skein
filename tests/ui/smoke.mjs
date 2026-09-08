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
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { boxlikeNamespace, openDoor } from "./lift.mjs";
import { ledger, seeing, settler, texter } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";

const BOX = "smoke-box";

// ---------- fixture: a tiny workspace with the shapes that have actually broken ----------
async function makeFixture() {
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
  //
  // **Every call is checked, and the push is aimed before it is fired.** On 2026-08-22 a commit
  // authored `smoke <smoke@test>` with the message `fixture` landed on this repository's own
  // `master`, taking its tree from 155 files to 6 — the fixture below, pushed over the default
  // branch. Whatever put it there, two things in this function let it: `stdio: "ignore"` with the
  // status discarded, so a failed `git init` was indistinguishable from a good one and every later
  // `git -C ws` would resolve to whatever repository encloses `ws`; and `remote add origin`, which
  // fails when an `origin` already exists and leaves the *existing* one for the push to use.
  //
  // So a fixture that is not its own fresh repository, or an `origin` that is not the throwaway
  // bare below, is now a thrown error rather than a push to somebody's real remote.
  const git = (...a) => {
    const out = spawnSync("git", ["-C", ws, ...a], { encoding: "utf8" });
    if (out.status !== 0) {
      throw new Error(`fixture: git ${a.join(" ")} failed (${out.status}): ${(out.stderr || "").trim()}`);
    }
    return (out.stdout || "").trim();
  };
  const commit = (m) => git("-c", "user.email=smoke@test", "-c", "user.name=smoke", "commit", "-qm", m);
  git("init", "-q");
  // `git init` is not proof on its own: it succeeds inside an enclosing worktree too. This asks the
  // repository that `-C ws` actually resolves to where it keeps its objects, and the only acceptable
  // answer is `ws` itself.
  const gitDir = path.resolve(ws, git("rev-parse", "--git-dir"));
  if (gitDir !== path.join(ws, ".git")) {
    throw new Error(`fixture: ${ws} resolves to the repository at ${gitDir}, not one of its own`);
  }
  git("add", "-A");
  commit("fixture");
  // A real `origin` with a real base branch, because the diff is measured against the REMOTE base
  // now — a fixture with only local refs would pass while the thing under test never ran.
  const remote = path.join(root, "remote.git");
  spawnSync("git", ["init", "-q", "--bare", "-b", "master", remote], { stdio: "ignore" });
  // `set-url` after `add`, so an `origin` that somehow already exists is corrected rather than left
  // in place by a failed `add`.
  spawnSync("git", ["-C", ws, "remote", "add", "origin", remote], { stdio: "ignore" });
  git("remote", "set-url", "origin", remote);
  const origin = git("remote", "get-url", "origin");
  if (path.resolve(origin) !== path.resolve(remote)) {
    throw new Error(`fixture: origin is ${origin}, not the throwaway remote ${remote} — refusing to push`);
  }
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
  // `exec` runs the script the way a box would, so the verify path (wrapper script, exit-code marker,
  // stored record) runs for real without a sandbox. The script is the LAST argument whatever the
  // prefix — skein addresses a box through its placement, so the real argv is
  // `sbx exec <sandbox> nsenter … -- bash -lc <script>` and there is no namespace here to enter.
  // The script already carries its own `cd <tree>` and `export HOME=<home>` from `Place::wrap`, and
  // the placement record above points both at this fixture, so it lands in the right place by itself.
  fs.writeFileSync(sbx, `#!/usr/bin/env bash
case "$1" in
  ls)   echo '[{"name":"${BOX}","status":"running","agent":"claude","workspace":"${ws}"}]'; exit 0 ;;
  exec) exec bash -c "\${@: -1}" ;;
esac
exit 0
`);
  fs.chmodSync(sbx, 0o755);
  // the box's own conversation record — what the Transcript tab reads instead of the screen
  // Under the box's host state dir, because that is where a placed box's record lives: box-session.sh
  // binds `~/.claude/projects` in from the host, so skein reads it there rather than shelling into the
  // box — which is what lets a STOPPED box still show its conversation.
  const proj = path.join(root, "home", "boxes", BOX, "claude-projects", "-fixture");
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
  fs.mkdirSync(path.join(root, "home"), { recursive: true });
  fs.writeFileSync(path.join(root, "home", "config.json"), JSON.stringify({}));
  // The placement record: what makes a sandbox one of skein's boxes. Without it the box is `foreign`
  // and the board hides it — which is the point of the flag, and used to be free because an unplaced
  // name resolved to "a sandbox called that", the per-VM model.
  //
  // **And a real namespace to be entered through.** `ns_pid: process.pid` with no stamp was enough
  // while the fake `sbx` ran whatever it was handed; there is no hop now (SKEIN-576), so an
  // unstamped record is refused at the crossing's own guard and every pane falls back to the host
  // clone — which the Files tab then reports, correctly and unhelpfully. `boxlikeNamespace` is the
  // same bwrap namespace `place`'s own crossing test uses.
  fs.mkdirSync(path.join(root, "home", "places"), { recursive: true });
  const fleetRoot = path.join(root, "fleet");
  const sock = path.join(fleetRoot, BOX, "session.sock");
  fs.mkdirSync(path.join(fleetRoot, BOX), { recursive: true });
  const box = await boxlikeNamespace(root);
  fs.writeFileSync(
    path.join(root, "home", "places", `${BOX}.json`),
    JSON.stringify({
      sandbox: "skein-fleet",
      ns_pid: box.ns_pid,
      ns_start: box.ns_start,
      generation: box.generation,
      home: path.join(root, "boxhome"),
      tree: ws,
      sock,
    }),
  );
  // A real tmux server on that socket. `fleet_liveness` asks tmux whether the box has a session, so
  // without one the box reads as Stopped and every pane falls back to the host clone — which is what
  // the Files tab then says, correctly and unhelpfully.
  spawnSync("tmux", ["-S", sock, "new-session", "-d", "-s", "skein-agent", "sleep 600"], { stdio: "ignore" });
  fs.writeFileSync(path.join(root, "home", "api-token"), API_TOKEN, { mode: 0o600 });
  // a registered repo, so the settings pane has a card to open and edit
  fs.writeFileSync(path.join(root, "home", "repos.json"), JSON.stringify([
    { id: "smoke", source: "/src/smoke", work: ws, store: path.join(root, "store"), agent: "claude",
      check: "", plane_project: "", sync_connection: "" },
  ]));
  return { root, ws, sbx, bin, boxlike: box.child };
}

// The fleet's API token. Written by the fixture rather than read back after startup: the server
// mints one on first use, and a test that raced that would be flaky for a reason having nothing to
// do with what it is testing. The auth path itself is still exercised end to end — the browser gets
// its cookie from `?t=`, exactly as a person does, and every direct fetch carries the bearer.
const API_TOKEN = "t".repeat(64);
const apiToken = () => API_TOKEN;
const authHeader = () => ({ Authorization: `Bearer ${API_TOKEN}` });

// ---------- the check harness ----------
const { check, results, report } = ledger();
// Bound to the page below, once it exists — every one of them asks a question of it.
let page, mustSee, text, settle;
const openTab = async mode => { await page.evaluate(m => showBox(BOXNAME, m), mode); await settle(900); };

// ---------- run ----------
const fx = await makeFixture();
const door = await openDoor();
const port = door.port;
const { srv, log } = await startServer({
  door,
  token: apiToken(),
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,   // verbatim through `sh -c` — the args matter
    SKEIN_HOME: path.join(fx.root, "home"),   // keep probe/kit installs out of the real store
    // The fixture's own fleet root. `fleet_liveness` scans `<root>/<box>/session.sock` and asks
    // tmux, so this is what makes the box read as Running — pointed at the real /boxes it would
    // answer for whatever boxes this machine happens to be running.
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    PATH: `${fx.bin}:${process.env.PATH}`,    // `sbx` resolves to the stub, never the real CLI
  },
});
const browser = await chromium.launch();
page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
mustSee = seeing(page);
text = texter(page);
settle = settler(page, 700);
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

await page.goto(`http://127.0.0.1:${port}/?t=${apiToken()}`, { waitUntil: "domcontentloaded" });
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
  if (d.vantage !== "box") throw new Error(`a running box must be read from the box itself, got "${d.vantage}"`);
  // `reach` is the other half and they are not the same question: this listing is `enter`, and a
  // screen scrape of the same box would be `socket` — both `vantage: "box"`, neither as cheap or
  // as fresh as the other. Asserted because a field nothing reads is a field the next rename drops.
  if (d.reach !== "enter") throw new Error(`a listing is read by entering the box, got "${d.reach}"`);
  if (await page.$("#filespane .fsrc.host")) throw new Error("no host-clone badge should show for a live box");
});
await check("a fallback tree announces itself in the pane, not just a tooltip", async () => {
  // The Files tab shipped reading the host clone as if it were the box's tree. The badge alone
  // hid the reason behind a hover; provenance has to be readable without pointing at anything.
  const d = await page.evaluate(n => fetch(`/api/boxes/${n}/files?path=`).then(r => r.json()), "bare-box");
  if (d.vantage !== "host") throw new Error(`a stopped box's listing comes from the host, got "${d.vantage}"`);
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
  if (d.vantage !== "box") throw new Error(`the running box should answer for itself, got "${d.vantage}"`);
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
  await page.click('header .kbtn[aria-label^="Settings"]');
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
  await page.click('header .kbtn[aria-label^="Settings"]');
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
  await page.click('header .kbtn[aria-label^="Settings"]');
  await settle();
  await mustSee("#settings.open", "the settings dialog");
  const card = await mustSee('.rcard[data-card="smoke"]', "the repo card");
  if (await page.$(".rcard.open")) throw new Error("cards should start collapsed");
});
await check("opening it shows labelled fields, not bare inputs", async () => {
  await page.click('.rcard[data-card="smoke"] .rhead');
  await settle(300);
  const titles = await page.$$eval('.rcard[data-card="smoke"] .set-title', els => els.map(e => e.textContent.replace("saved","").trim()));
  for (const want of ["Plane project", "Work tracking"])
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
  const saved = await fetch(`http://127.0.0.1:${port}/api/repos`, { headers: authHeader() }).then(r => r.json());
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
  await page.click('.set-navi[data-pane="fleet"]');
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
  // Applying these is live; applying fleet memory is not offered at all, because it is a destroy
  // and skein is inside the thing it would destroy (architecture §7.5). The two rows must not read
  // as the same affordance: one is a button that acts, the other says whose job it is instead.
  const cheap = await page.$eval("#set-applylimits", e => e.closest(".set-field").querySelector(".desc").textContent);
  if (!/live/.test(cheap)) throw new Error(`the live path should say so: ${cheap}`);
  if (await page.$("#set-resize"))
    throw new Error("the rebuild button is back — pressing it destroys the sandbox serving this page");
  const dear = await page.$eval("#set-resize-infleet", e => e.querySelector(".desc").textContent);
  if (!/host/i.test(dear)) throw new Error(`the destructive path should say whose job it is: ${dear}`);
});
await check("AI enrichment is a visible setting, not folklore in an env var", async () => {
  // It existed for months as $SKEIN_AI only, so nobody knew it was there. The toggle has to be
  // findable, and it has to say what would actually happen — "on" with no `claude` on PATH is a
  // state a checkbox alone can never show.
  await page.click('.set-navi[data-pane="boxes"]');
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
  if (h.ai.level !== "satisfied") throw new Error(`opt-in-and-off is not a fault, so this must never report unhealthy: ${h.ai.level}`);
  // Save closed the dialog — put the pane back where the following checks expect it.
  await page.click('header .kbtn[aria-label^="Settings"]');
  await settle();
  await page.click('.set-navi[data-pane="repos"]');
  await settle(300);
});
await check("the pane doesn't pretend Save applies to repo cards", async () => {
  const shown = await page.$$eval("#settings .set-foot .primary", els => els.filter(e => e.offsetParent).length);
  if (shown) throw new Error("Save is offered on a pane whose fields already saved themselves");
  if (!/saves as you leave a field/.test(await text("#set-hint"))) throw new Error("nothing says when these save");
  // and it comes back on a pane that IS a form
  await page.click('.set-navi[data-pane="boxes"]');
  await settle(250);
  await mustSee("#settings .set-foot .primary", "Save on a pane that is a form");
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
  const sync = await fetch(`http://127.0.0.1:${port}/api/sync`, { headers: authHeader() }).then(r => r.json());
  if (sync.connections[0].gateway_url !== "https://mcp.smoke.example/mcp")
    throw new Error(`a rejected value must not overwrite the stored one, got ${sync.connections[0].gateway_url}`);
  await page.keyboard.press("Escape");
});
await check("Track work appears once THIS box's repo has a usable connection", async () => {
  // The payoff, and the reason readiness is per box: "some connection somewhere is ready" says
  // nothing about this one.
  //
  // It lives in box settings rather than the dock since cd9f5b8 — the dock had grown a row of
  // buttons mostly irrelevant to the box you were looking at, and this was the worst of them.
  // The test kept clicking at the old address for months, which is why it is worth saying where
  // the button is and not only that it exists.
  await page.click(`#fleet .row[data-name="${BOX}"] .rowcog, #fleet .row[data-name="${BOX}"]`);
  await settle(300);
  await page.evaluate(name => window.openBoxSettings?.(name), BOX);
  await settle(500);
  const btn = await mustSee("#bs-wire", "Wire up, now that the repo picks a ready connection");
  const label = (await btn.textContent() || "") + (await btn.getAttribute("title") || "");
  if (!/smoke tracker/.test(label))
    throw new Error(`it should name the backlog it would mint at, got "${label}"`);
  // Closed explicitly, not with Escape. These checks run in sequence against one page, so a dialog
  // left open is not this test failing — it is the next four failing, somewhere else, for a reason
  // that has nothing to do with them. Which is exactly what happened on the first run of this edit.
  await page.evaluate(() => window.closeBoxSettings?.());
  await settle(200);
});

await check("settings is usable while a box is open (the docked layout hides nothing of it)", async () => {
  // `body.docked footer {display:none}` — written for the fleet's key hints — also matched the
  // settings dialog's own <footer>, so Save and Cancel vanished whenever any box tab was open.
  await page.click('header .kbtn[aria-label^="Settings"]');   // the previous check closed it
  await settle();
  await page.click('.set-navi[data-pane="boxes"]');
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

// ---------- the mouth ----------
// The one section here that does not assert something visible, because there is nothing to see: the
// output is sound. Everything else about the rule still holds — this is the real page in a real
// browser running the real `notify()`, with only the speaker itself replaced by a recorder. A stub
// of the page's own logic would agree with whatever the page happened to do.
console.log("\nvoice");
const spoken = () => page.evaluate(() => window.__said.splice(0));

// Both announcements below are gated behind `OWED_GRACE_MS` (you have to have been away a while)
// and `OWED_SETTLE_MS` (the box has to have stopped flickering) — introduced by c5f86a6 to stop the
// voice repeating itself, and never reflected here, so these checks asserted pre-c5f86a6 behaviour
// and failed for months. Real time cannot be waited out twice in a smoke run, and `awaySince` is a
// module-scope `let` no test can assign; it is only ever compared against `Date.now`, which one can.
// So the clock moves instead of the calendar.
const runClock = () => page.evaluate(() => {
  let t = Date.now();
  window.__realNow = Date.now;
  Date.now = () => t;
  window.__advance = ms => { t += ms; };
});
const stopClock = () => page.evaluate(() => { if (window.__realNow) Date.now = window.__realNow; });

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
await runClock();
await check("a box that starts asking says what it is asking", async () => {
  const said = await page.evaluate(() => {
    // Two snapshots: the first seeds the prior state (a fresh tab must not announce a fleet that
    // was already paused), the second is the turn.
    const working = [{ name: "proj-s6", state: "working", pause: "none" }];
    const asking = [{ name: "proj-s6", state: "needs-input", pause: "ask", blocked_kind: "permission",
                      headline: "Run `rm -rf /boxes/smoke/tree/build`?" }];
    notify(working); window.__said.length = 0;
    // Past the grace window and long enough for the new state to have settled.
    window.__advance(60000);
    notify(asking);
    window.__advance(60000);
    notify(asking);
    return window.__said.slice();
  });
  if (said.length !== 1) throw new Error(`expected one sentence, got ${JSON.stringify(said)}`);
  const line = said[0];
  if (!/proj s 6/i.test(line)) throw new Error(`the name is unspoken or unreadable: ${JSON.stringify(line)}`);
  if (!/wants permission/.test(line)) throw new Error(`the kind of ask is missing: ${JSON.stringify(line)}`);
  if (!/rm -rf build/.test(line)) throw new Error(`the ask itself is missing or the path was read out: ${JSON.stringify(line)}`);
  if (/`|\/boxes\//.test(line)) throw new Error(`unspeakable text survived: ${JSON.stringify(line)}`);
});
await check("a burst becomes a count, not a monologue", async () => {
  // Three boxes turn at once. The property is that you never hear three sentences — not that you
  // hear exactly one: a finished box is announced on its own tick and the standing debt waits for
  // the next, deliberately, so the `done` sentence is not buried inside a list ("let it stand alone
  // and say this on the next one"). Two ticks, one sentence each, neither of them a monologue.
  const [first, second] = await page.evaluate(() => {
    const calm = ["a", "b", "c"].map(n => ({ name: n, state: "working", pause: "none" }));
    const turned = [{ name: "a", state: "needs-input", pause: "ask", headline: "one?" },
                    { name: "b", state: "needs-input", pause: "ask", headline: "two?" },
                    { name: "c", state: "done", pause: "none" }];
    notify(calm); window.__said.length = 0;
    window.__advance(60000);
    notify(turned);
    const one = window.__said.splice(0);
    window.__advance(60000);
    notify(turned);
    return [one, window.__said.splice(0)];
  });
  if (first.length !== 1 || !/c finished/.test(first[0]))
    throw new Error(`the finished box is said alone, got ${JSON.stringify(first)}`);
  if (second.length !== 1)
    throw new Error(`the standing debt must be one sentence, got ${JSON.stringify(second)}`);
  // Two get named — spoken, so `a` and `b` rather than a count. Past `NAME_AT_MOST` it collapses,
  // and `cockpit/test/announce.test.mjs` is where that boundary is pinned. What this asserts is the
  // property the check is named for: ONE sentence, not one per box.
  if (!/^a and b need you$/.test(second[0]))
    throw new Error(`expected one sentence naming both, got ${JSON.stringify(second[0])}`);
});
await check("silence when nothing turned, and when you are looking at the board", async () => {
  const said = await page.evaluate(() => {
    const asking = [{ name: "proj-s6", state: "needs-input", pause: "ask", headline: "still?" }];
    notify(asking); window.__said.length = 0;
    notify(asking);                       // same state twice: nothing turned, nothing to say
    document.hasFocus = () => true;
    notify([{ name: "proj-s6", state: "done", pause: "none" }]);   // a real turn, but you are here
    document.hasFocus = () => false;
    return window.__said.slice();
  });
  if (said.length) throw new Error(`it spoke when it should not have: ${JSON.stringify(said)}`);
});
await check("the switch silences it", async () => {
  await page.click("#voice");            // off
  const said = await page.evaluate(() => {
    window.__said.length = 0;
    notify([{ name: "proj-s6", state: "working", pause: "none" }]);
    notify([{ name: "proj-s6", state: "needs-input", pause: "ask", headline: "anything?" }]);
    return window.__said.slice();
  });
  if (said.length) throw new Error(`silenced and still talking: ${JSON.stringify(said)}`);
});

// The ear. Headless Chromium has no recogniser, so one is stood in — everything downstream of the
// transcript is the page's own code, which is where the whole design lives.
// The clock goes back to the wall before anything that involves a real timer — a frozen `Date.now`
// is right for stepping over a 20s grace window and wrong for everything else.
await stopClock();
await check("holding the key opens an ear, and releasing it acts on what was said", async () => {
  await page.evaluate(() => {
    window.__started = 0;
    window.SpeechRecognition = class {
      constructor() { window.__rec = this; }
      start() { window.__started++; }
      stop() { this.onresult?.({ resultIndex: 0, results: [Object.assign([{ transcript: "what needs me" }], { isFinal: true })] }); this.onend?.(); }
    };
    window.__said = [];
    speechSynthesis.speak = u => window.__said.push(u.text);
  });
  // The previous check turned the speaker OFF, and `say()` is gated on that switch — so without
  // this the mic opens, hears correctly, and answers into a muted page, which reads as the mic
  // being broken. This check is about the ear, so it states the mouth it needs rather than
  // inheriting whatever the check before it happened to leave.
  await page.evaluate(() => { if (!voiceOn) document.getElementById("voice").click(); });
  await page.evaluate(() => { window.__said.length = 0; });
  // Press and hold: the mic must NOT open on the keydown itself, or every ⌥-chord would trip it.
  await page.keyboard.down("AltRight");
  if (await page.evaluate(() => window.__started)) throw new Error("a tap opened the mic — ⌥ chords would trip it constantly");
  await page.waitForTimeout(400);
  if (!(await page.evaluate(() => window.__started))) throw new Error("holding the key never opened the mic");
  await page.keyboard.up("AltRight");
  await settle(300);
  const said = await page.evaluate(() => window.__said.splice(0));
  if (!said.length) throw new Error(`"what needs me" did nothing`);
});
await check("what it heard is on screen, so a misfire is visible", async () => {
  const strip = await mustSee("#vstrip.show", "the heard-it strip");
  if (!(await strip.textContent()).includes("what needs me"))
    throw new Error("the strip does not show the words it acted on");
});
await check("a chord is not a held mic", async () => {
  await page.evaluate(() => { window.__started = 0; });
  await page.keyboard.down("AltRight");
  await page.keyboard.press("BracketLeft");   // ⌥[ — a real tab chord this page binds
  await page.waitForTimeout(400);
  await page.keyboard.up("AltRight");
  if (await page.evaluate(() => window.__started)) throw new Error("⌥[ opened the microphone");
});

// ---------- the gate ----------
//
// A box on this fleet reached `host.docker.internal:7878` and got a 200, which made every route
// below an unauthenticated way to undo gitgate — file a write request, approve it, receive a token.
// So the thing worth testing is not that the cockpit still works (everything above covers that) but
// that an unauthenticated caller is *refused*, and that the refusal is total rather than per-route.
console.log("\nthe API gate");
const bare = p => fetch(`http://127.0.0.1:${port}${p}`);
await check("an unauthenticated read is refused", async () => {
  const r = await bare("/api/boxes");
  if (r.status !== 401) throw new Error(`expected 401, got ${r.status}`);
});
await check("and so is an unauthenticated write", async () => {
  // The exact call in the escalation: approve a write request nobody's owner approved.
  const r = await fetch(`http://127.0.0.1:${port}/api/fleet/git-grants/anything`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ approve: true, hours: 0 }),
  });
  if (r.status !== 401) throw new Error(`expected 401, got ${r.status}`);
});
await check("un-scoping a box needs the token too", async () => {
  const r = await fetch(`http://127.0.0.1:${port}/api/boxes/${BOX}/git-scope`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ scope: "fleet" }),
  });
  if (r.status !== 401) throw new Error(`expected 401, got ${r.status}`);
});
await check("a wrong token is no better than none", async () => {
  const r = await fetch(`http://127.0.0.1:${port}/api/boxes`, { headers: { Authorization: `Bearer ${"x".repeat(64)}` } });
  if (r.status !== 401) throw new Error(`expected 401, got ${r.status}`);
});
// Every new route is guarded by default — the layer covers the router, so the only way to be open
// is to be named in `open_to_all`. This asserts that list is what it says it is.
await check("the page and its vendored assets stay open, so a visitor sees something", async () => {
  for (const p of ["/", "/vendor/xterm.js", "/vendor/marked.js"]) {
    const r = await bare(p);
    if (!r.ok) throw new Error(`${p} should be served without a token, got ${r.status}`);
  }
});
// The page being open is only half of "a visitor sees something". Measured on a real first run, the
// other half was a board stuck on "reconnecting…" with four 401s in a console nobody opens and no
// mention anywhere that a token exists — which is precisely what a teammate handed the bare URL got.
await check("a visitor with no token is told what they need, not left on an empty board", async () => {
  const fresh = await browser.newContext();          // no cookie: the state a bookmark lands in
  const visitor = await fresh.newPage();
  await visitor.goto(`http://127.0.0.1:${port}/`, { waitUntil: "domcontentloaded" });
  await visitor.waitForSelector("#noauth", { state: "visible", timeout: 8000 });
  const said = await visitor.locator("#noauth").innerText();
  // The instruction has to be actionable on its own: what is missing, and the command that fixes it.
  for (const want of ["token", "?t=", "api-token"]) {
    if (!said.includes(want)) throw new Error(`the refusal never mentions ${want}: ${said}`);
  }
  await fresh.close();
});
await check("the token in a URL is exchanged for a cookie and then dropped from it", async () => {
  const r = await fetch(`http://127.0.0.1:${port}/?t=${apiToken()}`, { redirect: "manual" });
  if (r.status !== 303) throw new Error(`expected a redirect, got ${r.status}`);
  if (r.headers.get("location") !== "/") throw new Error("the token must not survive in the URL");
  const cookie = r.headers.get("set-cookie") || "";
  if (!cookie.includes("HttpOnly")) throw new Error("page script must not be able to read it back");
  if (!cookie.includes("SameSite=Strict")) throw new Error("SameSite=Strict is what closes cross-site POSTs");
});
await check("a wrong token in the URL sets no cookie", async () => {
  const r = await fetch(`http://127.0.0.1:${port}/?t=${"z".repeat(64)}`, { redirect: "manual" });
  if (r.headers.get("set-cookie")) throw new Error("a guess must not be handed a session");
});

console.log("\nfleet gauges");
// SKEIN-577. The strip is the thing a person looks at to decide whether to raise a ceiling, and
// nothing asserted it draws anything at all. Its one former mention was `#gauges.ga.tp`, which went
// with the transport row in SKEIN-573 — and that selector could never have covered the strip as a
// separable property anyway, because it fails identically whether the strip or the row is missing.
//
// Driven through the REAL path: the real `skein-server` serving the real page bytes, the page's own
// `loadResources` and `gaugeRow`, the real stylesheet deciding whether `.gauges.on` is visible.
// Exactly one thing is fixtured — the answer to `/api/fleet/resources`. It has to be: with no fleet
// sandbox named `fleet_resources` returns `None`, and with one it execs a script inside it, so an
// assertion against whatever this machine is running is either environment-dependent or unfailable.
// Faked in the BROWSER rather than in the server, for the reason `actfail.mjs` gives about its own
// refusals: what is under test is what the PAGE does with an answer, not how the answer was made.
//
// **The figures are asserted, never a sentinel.** A check that looked only for `#gauges.on`, or
// counted `.ga` rows, stays green while every bar draws an empty label — which is the failure this
// file exists to refuse, in the same family as the hidden `.dir` rows it was written for.
{
  // MiB throughout, which is what `FleetResources` carries: the script behind it prints
  // `int(kB/1024)` for memory and `df -Pm` for the two disks, and `GIB` divides by 1024 again.
  const FULL = {
    mem_total: 32768, mem_used: 20480, boxes: 12288, docker: 4096,
    disk_total: 20480, disk_used: 8192,
    images_total: 40960, images_used: 30720,
    cpus: 8, load1: 3.5, load5: 1.25, workload_max: 24576, stale: false,
  };
  let served = FULL;
  await page.route("**/api/fleet/resources", route => route.fulfill({
    status: 200, contentType: "application/json", body: JSON.stringify(served),
  }));

  // Redraw from a known answer and WAIT for the strip to carry the expected number of rows, rather
  // than sleeping and hoping: `loadResources` fetches, so the DOM lands a tick after the call
  // returns and a fixed wait would pass or fail on machine speed.
  //
  // The wait is turned back into a sentence about the strip, because "Timeout 4000ms exceeded" is
  // what a reversed drop-the-empty-gauge rule would otherwise report — true, and useless for
  // telling that reversal apart from a page that never drew at all.
  const draw = async (r, rows) => {
    served = r;
    await page.evaluate(() => loadResources());
    try {
      await page.waitForFunction(
        want => document.querySelectorAll("#gauges .ga").length === want, rows, { timeout: 4000 });
    } catch {
      const drew = await page.$$eval("#gauges .ga", gs => gs.map(g => g.querySelector(".gk").textContent.trim()));
      throw new Error(`expected ${rows} gauges, the strip drew ${drew.length}: ${drew.join(",") || "none"}`);
    }
  };
  // Everything a reader can actually take off one row: its key, the figures on its right, the
  // hover text, the heat mark, and the geometry of the bar itself.
  const strip = () => page.$$eval("#gauges .ga", rows => rows.map(row => ({
    key: row.querySelector(".gk").textContent.trim(),
    figure: row.querySelector(".gv").textContent.trim(),
    hint: row.getAttribute("title") || "",
    heat: row.className.replace("ga", "").trim(),
    segs: [...row.querySelectorAll(".gseg")].map(s => ({
      cls: s.className.replace("gseg", "").trim(), width: s.style.width,
    })),
  })));

  await check("the strip is on screen once the sandbox has answered", async () => {
    await draw(FULL, 4);
    await mustSee("#gauges.on", "the fleet gauge strip");
  });

  await check("every gauge states its own figures, not just a bar", async () => {
    const got = Object.fromEntries((await strip()).map(g => [g.key, g.figure]));
    const want = { mem: "20.0/32.0G", disk: "8.0/20.0G", images: "30.0/40.0G", cpu: "3.5/8" };
    for (const [key, figure] of Object.entries(want)) {
      if (got[key] !== figure) {
        throw new Error(`${key} reads ${JSON.stringify(got[key])}, expected ${JSON.stringify(figure)}`);
      }
    }
  });

  // The first of the two decisions SKEIN-577 asks to pin. `boxes` and `docker` share one pool taken
  // first-come, so they are drawn apart and summed against ONE allowance — not given a slice each.
  // Reversing that shows up in both places this looks: the hint would state two allowances instead
  // of one 16-of-24, and a segment measured against the workload's ceiling rather than the VM's
  // whole memory would be 50.0% where it is 37.5%.
  await check("boxes and docker are summed against one ceiling, not given a slice each", async () => {
    const mem = (await strip()).find(g => g.key === "mem");
    if (!/16\.0G of 24\.0G allowed/.test(mem.hint)) {
      throw new Error(`the hint does not state one combined allowance: ${JSON.stringify(mem.hint)}`);
    }
    const geometry = mem.segs.map(s => `${s.cls}:${s.width}`).join(" ");
    if (geometry !== "boxes:37.5% docker:12.5% other:12.5%") {
      throw new Error(`the memory bar is not drawn against the whole VM: ${geometry}`);
    }
  });

  // The second. `df` cannot always see the fleet root, and a gauge drawn at zero reads as a FULL
  // disk to anyone glancing at it — which is the opposite of the truth and worse than silence.
  await check("a gauge with no denominator is dropped, not drawn at zero", async () => {
    await draw({ ...FULL, disk_total: 0, disk_used: 0, images_total: 0, images_used: 0 }, 2);
    const got = await strip();
    const keys = got.map(g => g.key).join(",");
    if (keys !== "mem,cpu") throw new Error(`expected only the gauges with a denominator, got ${keys}`);
    const zeroed = got.find(g => /0\.0\/0\.0G/.test(g.figure));
    if (zeroed) throw new Error(`${zeroed.key} was drawn at zero instead of dropped: ${zeroed.figure}`);
  });

  await check("and the gauges that do have one still carry their figures", async () => {
    const got = Object.fromEntries((await strip()).map(g => [g.key, g.figure]));
    if (got.mem !== "20.0/32.0G" || got.cpu !== "3.5/8") {
      throw new Error(`the surviving gauges lost their figures: ${JSON.stringify(got)}`);
    }
  });

  // The mark is the whole point of glancing at this strip, so it has to be earned rather than worn.
  // Asserted in both directions in one check, because a class that is always on proves nothing:
  // the comfortable fleet above must be unmarked, and only this one is hot.
  await check("memory close to the ceiling is marked, and a comfortable fleet is not", async () => {
    await draw(FULL, 4);
    const calm = (await strip()).find(g => g.key === "mem");
    if (calm.heat) throw new Error(`a fleet at 62.5% is marked ${JSON.stringify(calm.heat)}`);
    await draw({ ...FULL, mem_used: 31000, boxes: 28000, docker: 2000 }, 4);
    const hot = (await strip()).find(g => g.key === "mem");
    if (hot.heat !== "hot") throw new Error(`a fleet at 94.6% is marked ${JSON.stringify(hot.heat)}`);
    if (hot.figure !== "30.3/32.0G") throw new Error(`the hot gauge reads ${hot.figure}`);
  });

  // Left as this suite found it, so nothing after it inherits a strip full of invented figures.
  await page.unroute("**/api/fleet/resources");
  await page.evaluate(() => loadResources());
}

console.log("\nquiet");
await check("no page errors and no 5xx along the way", () => {
  if (noise.length) throw new Error(noise.join(" | "));
});

// ---------- report ----------
const shot = path.join(fx.root, "failure.png");
if (results.some(([ok]) => !ok)) await page.screenshot({ path: shot, fullPage: false });
const failed = report({ log });
if (failed.length) console.log(`screenshot: ${shot}\nfixture kept for inspection: ${fx.root}`);
await browser.close();
srv.kill();
// The fixture's tmux server outlives the process that started it, so it has to be killed by name —
// a stray `sleep 600` per run would otherwise pile up on a developer's machine.
spawnSync("tmux", ["-S", path.join(fx.root, "fleet", BOX, "session.sock"), "kill-server"], { stdio: "ignore" });
// The box-like namespace this suite's crossings entered. A `sleep` left behind under bwrap outlives
// the suite and is what a leak sweep finds.
fx.boxlike.kill("SIGKILL");
// SKEIN_KEEP=1 leaves the fixture behind so you can point a server at it and look at the thing
if (!failed.length && !process.env.SKEIN_KEEP) fs.rmSync(fx.root, { recursive: true, force: true });
else if (!failed.length) console.log(`fixture kept (SKEIN_KEEP): ${fx.root}`);
process.exit(failed.length ? 1 : 0);
