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
import { stopThenRemove } from "./harness/teardown.mjs";
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

// A GitHub the size of what THIS pane asks: one route. `update::ask_github` reads
// `/repos/{slug}/commits/{reference}` and wants a `sha` back. The other GitHub-touching suites
// stand up the shared queue-shaped stub on the same seam; this pane needs one route, so it answers
// one.
//
// **And it answers the way GitHub did on 2026-09-26 to a rotated token** (SKEIN-1172): a 401 "Bad
// credentials" to any `Authorization` at all, and the commit to a request carrying none. The server
// below is given a stored token, so every reading this suite sees has been refused once and asked
// again without it — and `heard` keeps each request's `Authorization`, or "" for none, to prove it.
const heard = [];
const createGitHub = sha => stub(({ url, req, send }) => {
  if (!/^\/repos\/[^/]+\/[^/]+\/commits\/[^/]+$/.test(url)) return false;
  const auth = req.headers.authorization || "";
  heard.push(auth);
  return auth ? send(401, { message: "Bad credentials", status: "401" }) : send(200, { sha });
});
const STORED_TOKEN = "skein-test-rotated-token";

const { value: check, report } = ledger();

// Fixed rather than read off the real repository (UI-3), so the checks below can assert an exact
// value instead of "GitHub said something" — 40 hex characters, the shape `same_revision`
// (src/update.rs:113) and the pane's abbreviation both expect a sha to have.
const REMOTE_SHA = "deadbeef".repeat(5);

const fx = makeFixture();
const door = await openDoor();
const github = await createGitHub(REMOTE_SHA);
await startServer({
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
    // The stored token, and the stub above refuses it (SKEIN-1172).
    GH_TOKEN: STORED_TOKEN,
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

// --- a stored token GitHub refuses: the check asks again without it, and the pane says so -----------
//
// (SKEIN-1172.) The sha above came through the refusal, which the first check here makes sure of:
// the stub heard the stored token and then a request with no `Authorization`. The note is the
// owner's sentence, verbatim.
//
// **What would make these fail**: `update::ask` returning the 401 instead of asking again — the
// sha check above fails and so does the first here, since nothing arrives bare; `github::config`
// sending `Authorization: Bearer ` for no token, which the stub refuses too; the note's line
// dropped from `renderUpdate`, or any drift in `UPDATE_WORDS.tokenRefused`, fails the second; a
// note drawn whatever `token_refused` says fails the third.
{
  const TOKEN_WORDS = "your stored GitHub token was refused — replace it under Settings → GitHub & keys";
  check("the stored token was tried, refused, and the answer came from an ask without it",
    [heard[0], heard.includes("")], [`Bearer ${STORED_TOKEN}`, true]);
  check("the pane says the stored token was refused, in the owner's words",
    await page.$eval("#upd-token", e => e.textContent.trim()).catch(() => ""), TOKEN_WORDS);
  const without = await page.evaluate(() => {
    updateState.skein.token_refused = false;
    renderUpdate();
    const shown = !!document.getElementById("upd-token");
    updateState.skein.token_refused = true;
    renderUpdate();
    return shown;
  });
  check("and says nothing of the kind when the token was not refused", without, false);
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

// --- the boxes still on an old agent CLI, and one restart per waiting box (SKEIN-1070, SKEIN-1071) ----
//
// The list is the server's (`/api/update-agents/boxes`) and asking it needs running boxes with agent
// sessions, which this fixture has none of — so the route is answered here, and what is under test
// is what the pane makes of an answer: the owner's approved wording, verbatim, and the restart
// control on a `waiting` box and nowhere else. The server's own half (who is listed, the re-check at
// the press, one box per route) is tested in `src/fleet/substrate.rs` and `src/bin/skein-server/agents.rs`.
//
// **What would make these fail**: dropping the `waiting` filter in `agentsBehindHtml` puts a button
// on box-b and box-c; any drift in `AGENTS_WORDS` from the approved text fails the string it
// changes; a press that sent more than the one box on its button fails the request count; mapping a
// refusal to the Failed wording fails the Refused check.
{
  const W = {
    several: "3 boxes are still running claude 2.1.278 and move to 2.1.280 at their next session:",
    one: "box-a is still running claude 2.1.278 and moves to 2.1.280 at its next session.",
    none: "every running box is on claude 2.1.280. Stopped boxes start on it.",
    unknown: "skein could not read when box-d's agent started, so it cannot say which claude it is running.",
    offer: "Stops claude in box-a and reopens its conversation on 2.1.280. box-a is waiting for you, so no turn is cut off, and nothing is sent to it. What its agent's terminal was running stops with it; anything started to outlive the terminal (nohup, setsid) keeps running.",
    notRunning: "Could not restart box-a: its agent is not running, so there is nothing to restart. Its next session starts on 2.1.280.",
    noReading: "Could not restart box-a: skein has no recent reading of its agent, so it cannot tell that it is waiting.",
    refused: "Not restarted: box-a started working after this list was drawn. It moves at its next session, and the button comes back when it is waiting again.",
    failed: "Could not restart box-a: box \"box-a\" is not running",
    done: "box-a is on claude 2.1.280",
  };
  const behind = (name, state) => ({ name, have: "2.1.278", state });
  let listed = null, answer = null;
  const pressed = [];
  await page.route("**/api/update-agents/boxes", r => r.fulfill({ json: { runtimes: listed } }));
  await page.route("**/api/update-agents/boxes/*/restart", r => {
    pressed.push(`${r.request().method()} ${new URL(r.request().url()).pathname}`);
    return r.fulfill({ json: answer });
  });
  const show = async runtimes => {
    listed = runtimes;
    await page.evaluate(() => loadAgentsBehind());
    await page.waitForTimeout(300);
    return page.$$eval("#upd-agents > *", els => els.map(e => ({
      cls: e.className, box: e.dataset.box || "",
      text: e.textContent.replace(/\s+/g, " ").trim(),
      parts: [...e.children].map(c => c.textContent.replace(/\s+/g, " ").trim()),
      buttons: [...e.querySelectorAll("button")].map(b => b.textContent.trim()),
    })));
  };
  const claude = (b, unknown = [], current = 1) =>
    [{ runtime: "claude", latest: "2.1.280", behind: b, unknown, current }];

  const several = await show(claude(
    [behind("box-a", "waiting"), behind("box-b", "working"), behind("box-c", "working")], ["box-d"]));
  const row = name => several.find(e => e.box === name && e.cls.includes("upd-agent")) || {};
  check("Several: the line is the approved wording", several[0]?.text, W.several);
  check("the waiting box offers its restart, with the approved button and offer",
    row("box-a").parts, ["box-a", "waiting for you", "Restart on 2.1.280", W.offer]);
  check("a working box never offers the restart, and says when it moves",
    [row("box-b").buttons, row("box-b").parts, row("box-c").buttons],
    [[], ["box-b", "working", "moves when this turn's session ends"], []]);
  check("Unknown: the line is the approved wording", several.some(e => e.text === W.unknown), true);
  check("no control restarts more than one box: one button per waiting box, and none for all",
    several.flatMap(e => e.buttons), ["Restart on 2.1.280"]);

  const one = await show(claude([behind("box-a", "waiting")]));
  check("One: the line is the approved wording", one[0]?.text, W.one);
  const oneWorking = await show(claude([behind("box-a", "working")]));
  check("One, working: the line, and no restart", [oneWorking[0]?.text, oneWorking.flatMap(e => e.buttons)], [W.one, []]);
  const none = await show(claude([], [], 3));
  check("None: the line is the approved wording", none.map(e => e.text), [W.none]);

  // A press on a box that turned working since the list was drawn: the server refuses it.
  // The list is drawn with box-a waiting; by the press the server reads it working.
  await show(claude([behind("box-a", "waiting")]));
  listed = claude([behind("box-a", "working")]);
  answer = { ok: false, why: "not-waiting", state: "working" };
  await page.click('#upd-agents button[data-box="box-a"]');
  await page.waitForTimeout(400);
  check("a press sends one request, for the box on its button",
    pressed, ["POST /api/update-agents/boxes/box-a/restart"]);
  check("a refused press says so in the approved words, and the button is gone while it works",
    [await page.$eval("#upd-agents .upd-agent-said", e => e.textContent.trim()).catch(() => ""),
      await page.$$eval("#upd-agents button", els => els.length)], [W.refused, 0]);
  const back = await show(claude([behind("box-a", "waiting")]));
  check("the button comes back when it is waiting again, and the refusal goes with it",
    [back.flatMap(e => e.buttons), back.some(e => e.cls.includes("upd-agent-said"))], [["Restart on 2.1.280"], false]);

  // Every state a refusal can come back with, and the sentence each one gets (the owner, 2026-09-24).
  // **What would make this fail**: moving a state between the sets in boot.js — `ended` read as "no
  // reading", `live` as not running, a turn state given a plain reason — or any drift in the two
  // plain sentences.
  const REFUSALS = [
    ["working", W.refused], ["compacting", W.refused], ["needs-input", W.refused],
    ["error", W.refused], ["done", W.refused],
    ["ended", W.notRunning],
    ["stale", W.noReading], ["live", W.noReading], ["idle", W.noReading], ["unknown", W.noReading],
    ["", W.noReading],
    // A state no probe writes today: it must still get a sentence, never its own name.
    ["somethingnew", W.noReading],
  ];
  const saidFor = [];
  for (const [state] of REFUSALS) {
    await show(claude([behind("box-a", "waiting")]));
    listed = claude([behind("box-a", state || "waiting")]);
    answer = { ok: false, why: "not-waiting", state };
    await page.click('#upd-agents button[data-box="box-a"]');
    await page.waitForTimeout(300);
    saidFor.push([state, await page.$eval("#upd-agents .upd-agent-said", e => e.textContent.trim()).catch(() => "")]);
  }
  check("each refused state gets its approved sentence", saidFor, REFUSALS);
  // **No state word reaches the page** (the owner, 2026-09-24). What would make this fail: a
  // fallback that names the state — `Could not restart box-a: idle` — for any state in the table.
  check("no refused state is told as the bare 'Could not restart box-a: <word>'",
    saidFor.filter(([, said]) => /^Could not restart box-a: [\w-]*\.?$/.test(said)).map(([state]) => state), []);
  pressed.length = 1;   // the table's presses are counted by its own check, not the tally below
  await show(claude([behind("box-a", "waiting")]));

  answer = { ok: false, why: "failed", error: 'box "box-a" is not running' };
  await page.click('#upd-agents button[data-box="box-a"]');
  await page.waitForTimeout(400);
  check("a failed press says why in the approved words",
    await page.$eval("#upd-agents .upd-agent-said", e => e.textContent.trim()).catch(() => ""), W.failed);

  answer = { ok: true };
  await page.click('#upd-agents button[data-box="box-a"]');
  await page.waitForTimeout(300);
  check("a restart that took is told in the approved toast",
    await page.$eval("#toast", e => e.textContent.trim()).catch(() => ""), W.done);
  check("three presses, three requests, each for box-a alone", pressed.length, 3);
  await page.unroute("**/api/update-agents/boxes/*/restart");
  await page.unroute("**/api/update-agents/boxes");
}

// --- and nothing threw ------------------------------------------------------------------------------
sayBlips();
check("the pane raised no page errors", errors, []);

await browser.close();
github.close();
const failed = report();
const leftRunning = stopThenRemove([fx.root]);
process.exit(failed.length || leftRunning.length ? 1 : 0);
