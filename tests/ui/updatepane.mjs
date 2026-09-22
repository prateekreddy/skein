// Settings -> Update in a real browser (SKEIN-486).
//
// **This suite exists because the node tests all passed and the pane was still wrong.** They cover
// the pure halves — the verdict, the CLI row — and both of the defects below live in the wiring
// between a fetch, a timer and a table cell, which is exactly what a stubbed document cannot see.
// Found by opening the page and reading it, which is the whole argument for a browser tier.
//
//   node tests/ui/updatepane.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { stub } from "./harness/github.mjs";
import { startServer } from "./harness/server.mjs";
const API_TOKEN = "t".repeat(64);

// The smallest fixture that serves a settings dialog: a store, a home, and a token. No box, no
// tmux, no transcript — the Update pane asks about skein, not about the fleet's contents.
function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-updatepane");
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
// (src/update.rs:159-163). The other GitHub-touching suites stand up the shared queue-shaped stub
// on the same seam; this pane needs one route, so it answers one.
const createGitHub = sha => stub(({ url, send }) =>
  /^\/repos\/[^/]+\/[^/]+\/commits\/[^/]+$/.test(url) && send(200, { sha }));

const { value: check, report } = ledger();

// Fixed rather than read off the real repository (UI-3), so the checks below can assert an exact
// value instead of "GitHub said something" — 40 hex characters, the shape `same_revision`
// (src/update.rs:113) and the pane's abbreviation both expect a sha to have.
const REMOTE_SHA = "deadbeef".repeat(5);

const fx = makeFixture();
const door = await openDoor();
const github = await createGitHub(REMOTE_SHA);
const { srv } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_HOME: path.join(fx.root, "home"),
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    // Without these two the pane's `/api/update` asks the real, unauthenticated api.github.com
    // about the owner's own repository (UI-3) — offline, behind a proxy, or after 60 requests/hour
    // that route answers nothing, and the checks below have nothing to read. Both point at the
    // fixture instead, so the request never leaves the machine.
    SKEIN_GITHUB_API: github.url,
    SKEIN_SOURCE_URL: "https://github.com/acme/skein.git",
  },
});
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
// The page's own errors, with the browser's own complaints about its transport kept apart and
// reported rather than failing this run — the distinction is structural, not a list of spellings;
// see `harness/browser.mjs::erring` (SKEIN-998, SKEIN-1010).
const { errors, sayBlips } = erring(page, { say: (kind, text) => `${kind}: ${text}` });

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
  // A locator rather than a handle (SKEIN-716). This pane redraws when the late remote lands — that
  // is the whole subject of this suite — so a handle taken here and measured on the next round trip
  // can be measuring a node the redraw has already replaced, and `boundingBox` on a detached node
  // throws rather than answering. The bound is explicit and short because the FAILING answer has to
  // stay prompt: a pane a CSS rule is hiding must still be reported in about a second, not after
  // Playwright's 30s default.
  const box = await page.locator("#set-update").first().boundingBox({ timeout: 1500 }).catch(() => null);
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
sayBlips();
check("the pane raised no page errors", errors, []);

await browser.close();
srv.kill();
github.close();
try { fs.rmSync(fx.root, { recursive: true, force: true }); } catch {}

process.exit(report().length ? 1 : 0);
