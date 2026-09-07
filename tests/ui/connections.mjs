// **What the cockpit spends to have ten pull requests read at once** (SKEIN-366).
//
// The owner reported it as "why is the image paste so slow". The upload was not slow: measured on
// his fleet, `POST /api/boxes/:name/upload` answered in 9-13 ms at every size, and the agent
// transport answered in 7-14 ms. The time was going in the BROWSER, before the request left.
//
// The cockpit is HTTP/1.1 only — `curl --http2` against `/api/health` still answers
// `HTTP/1.1 200 OK` — and browsers cap HTTP/1.1 at six connections per origin. A reading is a model
// call taking tens of seconds, and `REV_ASKED_PARALLEL` is ten, so one pressed stack read used to
// hold every connection the page had. Reproduced in a real browser against a build of `d48a4ce`, by
// opening N forced reads from inside the page and then timing one trivial `GET /api/health`:
//
//     reads in flight → /api/health
//       0 (idle)      → 21, 13, 12, 13, 12 ms
//       3             → 12 ms
//       6             → 12,814 ms
//       10            → 34,438 ms
//
// **The fix is not a smaller width, and this suite is written so that a smaller width would not
// pass it.** The owner's instruction, given twice, is that a read he asks for is not rationed — "If
// I ask for it, then it is unlimited" — so lowering `REV_ASKED_PARALLEL` would move the cliff
// rather than remove it. What changed is where the answer travels: `POST /review/:n/read` starts the
// reading and returns in milliseconds, and the reading itself arrives on the one `EventSource` the
// page already holds. So the assertions below are about the number of requests the page is HOLDING
// while ten readings run, and every one of them is checked against a precondition that ten readings
// really are running — a test on the read count alone would pass with the parallelism merely
// lowered, which is the fix the owner rejected.
//
// Needs Playwright's chromium: the six-connection cap is a browser behaviour and curl does not have
// it, which is exactly why the original diagnosis from code constants was not proof.
//
//   node tests/ui/connections.mjs

import { chromium } from "playwright";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { openDoor } from "./lift.mjs";
import { ledger } from "./harness/browser.mjs";
import { queueGitHub } from "./harness/github.mjs";
import { startServer } from "./harness/server.mjs";


// Ten, because that is `REV_ASKED_PARALLEL` — the width one pressed stack read opens, and the row of
// the table above where the cockpit stopped answering at all.
const READS = 10;
// How long each reading takes. It has to be long enough that a starved connection pool is visible in
// the wall clock of an unrelated request, and short enough that this suite is not a coffee break.
// A real reading is 35 seconds; three is the same shape at a twelfth of the cost.
const READ_SECONDS = 3;

const API_TOKEN = "c".repeat(64);
const authHeader = () => ({ Authorization: `Bearer ${API_TOKEN}` });

async function makeFixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "skein-connections-ui-"));
  const bin = path.join(root, "bin");
  const home = path.join(root, "home");
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(root, "sandboxes.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  // `read_prs: false` — read-ahead OFF, deliberately. This suite counts what the page holds while
  // readings SOMEBODY PRESSED are running, and skein's own pump reading three more in the
  // background would put requests in the count that the case is not about.
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    { id: "acme", source: "https://github.com/acme/thing.git",
      source_tree: path.join(root, "work"), read_prs: false,
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));

  // Enough pull requests that ten can be read at once — which is the whole subject.
  const prs = Array.from({ length: READS + 2 }, (_, i) => {
    const number = i + 1;
    return {
      number, title: `change number ${number}`, author: { login: "dana" },
      url: `https://github.com/acme/thing/pull/${number}`,
      // A STACK: each step branches off the one below it. That is what makes the button this suite
      // presses the real one — "read the 12 not yet read", at `REV_ASKED_PARALLEL` a time — rather
      // than a loop in the test calling the fetch directly, which would exercise neither the
      // control nor the width.
      headRefName: `feat-${number}`, headRefOid: `sha${number}`,
      baseRefName: number === 1 ? "main" : `feat-${number - 1}`,
      isDraft: false, updatedAt: "2026-08-20T00:00:00Z",
      latestReviews: { nodes: [] },
      commits: { nodes: [{ commit: { statusCheckRollup: null } }] },
    };
  });

  fs.mkdirSync(path.join(root, "work", ".github"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", ".github", "CODEOWNERS"), "src/ @me\n");
  fs.mkdirSync(path.join(root, "work", "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", "src", "parser.rs"), "const TIMEOUT: u64 = 5;\n");
  const wgit = (...a) => spawnSync("git", ["-C", path.join(root, "work"), ...a], { stdio: "ignore" });
  wgit("init", "-q", "-b", "main");
  wgit("add", "-A");
  wgit("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "the tree the mirror carries");

  const github = await queueGitHub(prs);

  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, `#!/bin/sh\ncase "$1" in ls) echo '[]'; exit 0 ;; esac\nexit 0\n`);
  fs.chmodSync(sbx, 0o755);

  // **A model that takes its time**, which is the only reason any of this is measurable. An instant
  // reading is never in flight, so a suite about what the page holds WHILE reading would have
  // nothing to hold and would pass with the bug in place.
  const claude = path.join(bin, "claude");
  fs.writeFileSync(claude, `#!/bin/sh
sleep ${READ_SECONDS}
printf 'KIND: fix\\nLINE: stops the parser crashing on empty input.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\nREVIEW:\\nOVERALL: nothing to flag\\n'
exit 0
`);
  fs.chmodSync(claude, 0o755);
  return { root, bin, home, github, sbx, claude };
}

// ---------- harness ----------
const { check, results, report } = ledger();

// ---------- run ----------
const fx = await makeFixture();
const door = await openDoor();
const port = door.port;
const { srv, log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,
    SKEIN_HOME: fx.home,
    // **Pinned, or it is `/boxes` — a real fleet, on the machine running this** (SKEIN-530).
    // `config::fleet_root` falls back to `/boxes` when this is unset, so a suite that pins only
    // `SKEIN_HOME` still reads placement records, gitgate requests and box sessions out of whatever
    // fleet the developer happens to be living in. `SKEIN_HOME` covers the store and nothing else.
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_GITHUB_API: fx.github.url,
    SKEIN_CLAUDE_BIN: fx.claude,
    SKEIN_NO_GH_SECRET: "1",
    PATH: `${fx.bin}:${process.env.PATH}`,
  },
});
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
page.setDefaultTimeout(8000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("console", m => { if (m.type() === "error") noise.push(`[console] ${m.text()}`); });

// **Every request the page has open, at any moment.** Playwright reports the browser's own requests,
// which is the only vantage point from which "what is this page holding" is answerable — the page's
// own JavaScript cannot see the connection pool it is queued behind.
const open = new Map();
page.on("request", r => open.set(r, Date.now()));
for (const done of ["requestfinished", "requestfailed"]) page.on(done, r => open.delete(r));
/** What is outstanding right now, without the things that are supposed to be long-lived. */
const outstanding = () => [...open.keys()].map(r => r.url()).filter(u => !u.includes("/api/events"));

const settle = (ms = 400) => page.waitForTimeout(ms);
const base = `http://127.0.0.1:${port}`;
/** What the server itself says it is reading. The authority — `review::readings` is the in-memory
 *  registry every model-spending path registers in, so this cannot be satisfied by a page that only
 *  believes it is reading. */
const readingNow = async () =>
  (await (await fetch(`${base}/api/review/reading`, { headers: authHeader() })).json()).length;
/** One trivial request, timed from inside the page — the owner's own reproduction. */
const healthMs = () => page.evaluate(async () => {
  const at = Date.now();
  await fetch("/api/health");
  return Date.now() - at;
});

await page.goto(`${base}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await settle(800);

console.log("\nthe pane");
await check("the queue arrives with enough pull requests to saturate the browser", async () => {
  await page.click("#revbtn");
  await settle(600);
  await page.evaluate(() => openReview("acme"));
  await page.waitForFunction(n => !revLoading && revQueue && (revQueue.prs || []).length >= n,
    READS, { timeout: 30000 });
});

// The control number. Everything below is "the same order of magnitude as at idle", and without
// this the comparison is against a constant somebody guessed.
let idle = 0;
await check("an unrelated request from the page is fast when nothing is being read", async () => {
  if (await readingNow() !== 0) throw new Error("something was already being read");
  const runs = [await healthMs(), await healthMs(), await healthMs()];
  idle = Math.min(...runs);
  console.log(`        idle: ${runs.join(", ")} ms`);
  if (idle > 2000) throw new Error(`even an idle cockpit is slow (${runs.join(", ")} ms) — this machine cannot measure the case`);
});

console.log(`\n${READS} readings at once`);
// **The owner's own press**, through the rendered control: open the stack and click "read the N not
// yet read". Nothing here calls `revFetchSummary` directly, so the width that governs how many
// readings this opens is the product's — `REV_ASKED_PARALLEL` — and a change to that number changes
// what this suite measures rather than sliding past it.
await check("pressing the stack's read-all control is what starts them", async () => {
  const row = await page.waitForSelector("#revpane .revrow.stack .revline", { timeout: 10000 });
  await row.click();
  await settle(400);
  const btn = await page.waitForSelector("#revpane .revrow.stack.open .stackread .revchip", { timeout: 10000 });
  const said = (await btn.textContent()).trim();
  if (!/read/.test(said)) throw new Error(`that is not the read control: ${said}`);
  await btn.click();
});
// Long enough for every reading to have reached the model, and far short of the first one finishing.
await settle(1500);

// The precondition for all three assertions below, and the reason none of them can be satisfied by
// simply reading fewer pull requests: the server is holding ten readings while they are measured.
let held = 0;
await check(`the server is holding all ${READS} readings while the rest is measured`, async () => {
  held = await readingNow();
  if (held !== READS) {
    throw new Error(`the case did not arise: the server reports ${held} readings in flight, not ${READS}`
      + " — if REV_ASKED_PARALLEL was lowered, that is the fix the owner rejected, and this suite exists to say so");
  }
});

// **The invariant itself**, and it is deliberately not "how many readings does the page allow": a
// test on the read count would pass with `REV_ASKED_PARALLEL` merely lowered, which is the fix the
// owner rejected. This one is about what the page is HOLDING, so it fails whether the readings are
// riding their own requests (the bug) or have been rationed to fit (the wrong fix, which leaves the
// remaining requests queued in the browser and still outstanding).
await check("and the page holds no request per reading — the connection budget does not scale with them", async () => {
  const outs = outstanding();
  // Three is generous: the live stream is excluded above, so what is left is at most a health tick
  // or an in-flight poll that happened to overlap the sample. Ten would mean each reading is still
  // riding its own request, which over HTTP/1.1 is every connection the browser has.
  if (outs.length > 3) {
    throw new Error(`the page is holding ${outs.length} requests while ${READS} readings run `
      + `(${held} of them reached the server): ${outs.join(", ")}`);
  }
});

await check("so an unrelated request is answered as quickly as it is at idle", async () => {
  const busy = await healthMs();
  console.log(`        with ${READS} readings in flight: ${busy} ms (idle ${idle} ms)`);
  // A reading takes READ_SECONDS. If the readings were holding the connections, this request would
  // wait behind one of them — the shape of the 12,814 ms and 34,438 ms in the table above. The
  // threshold is half a reading, which no queued request can beat and no unqueued one can miss.
  const ceiling = Math.max(idle * 20, (READ_SECONDS * 1000) / 2);
  if (busy > ceiling) {
    throw new Error(`it queued behind the readings: ${busy} ms against ${idle} ms at idle (ceiling ${Math.round(ceiling)} ms)`);
  }
});

await check("and the readings themselves still arrive, over the stream rather than on their own requests", async () => {
  await page.waitForFunction(n => {
    let got = 0;
    for (const pr of (revQueue.prs || []).slice(0, n)) {
      const s = revSums.get(pr.repo_id + "#" + pr.number);
      if (s && s !== "…" && !s.transient && s.depth !== "unread") got++;
    }
    return got === n;
  }, READS, { timeout: 60000 });
});

await check("no page errors along the way", () => {
  if (noise.length) throw new Error(noise.join("\n"));
});

// ---------- report ----------
const failed = report({ log });

await browser.close();
srv.kill();
fx.github.close();
if (!failed.length) fs.rmSync(fx.root, { recursive: true, force: true });
else console.log(`fixture kept for inspection: ${fx.root}`);
process.exit(failed.length ? 1 : 0);
