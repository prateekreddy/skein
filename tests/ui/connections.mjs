// **What the cockpit spends to have ten pull requests read at once** (SKEIN-366).
//
// The owner reported it as "why is the image paste so slow". The upload was not slow: measured on
// his fleet, `POST /api/boxes/:name/upload` answered in 9-13 ms at every size, and the agent
// transport answered in 7-14 ms. The time was going in the BROWSER, before the request left.
//
// The cockpit is HTTP/1.1 only — `curl --http2` against `/api/health` still answers
// `HTTP/1.1 200 OK` — and browsers cap HTTP/1.1 at six connections per origin. A reading is a model
// call taking tens of seconds, and `REV_ASKED_PARALLEL` is ten, so one pressed stack read used to
// hold every connection the page had. Reproduced in a real browser against a build of `358d97f`, by
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
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { finding, ledger } from "./harness/browser.mjs";
import { queueGitHub } from "./harness/github.mjs";
import { startServer } from "./harness/server.mjs";


// Ten, because that is `REV_ASKED_PARALLEL` — the width one pressed stack read opens, and the row of
// the table above where the cockpit stopped answering at all.
const READS = 10;
// How long each reading takes. It has to be long enough that a starved connection pool is visible in
// the wall clock of an unrelated request, and short enough that this suite is not a coffee break.
// A real reading is 35 seconds; three is the same shape at a twelfth of the cost.
const READ_SECONDS = 3;
// How many pull requests the fixture queues. Two more than the pump's width on purpose: the stack
// is deeper than one round of readings, which is what makes the width the product's rather than the
// fixture's. Named here because the budget at the foot of this file counts rounds with it.
const PRS = READS + 2;

const API_TOKEN = "c".repeat(64);
const authHeader = () => ({ Authorization: `Bearer ${API_TOKEN}` });

async function makeFixture() {
  const root = freshFixture(fixtureRoot(), "skein-connections-ui");
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
  const prs = Array.from({ length: PRS }, (_, i) => {
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
// `whole`, because the diagnosis the last check prints is a line per pull request that did not
// arrive, and the default keeps only a message's FIRST line (`harness/browser.mjs`) — which would
// throw away the half that says whether a reading was late or lost (SKEIN-766).
const { check, results, report } = ledger({ whole: true });

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
    // **Pinned, or this suite is pointed at the machine's own fleet** (SKEIN-530).
    // `util::fleet_root` — `src/util.rs`, and it has never been in `config` — refuses an unpinned
    // TEST process (SKEIN-690), which the server is whenever `cargo test` runs this suite through
    // `tests/browser_suites.rs`, since `$SKEIN_TEST` reaches it through the spawn; run by hand
    // with no marker it answers `/boxes` instead, a real fleet on any machine running skein.
    // Either way `SKEIN_HOME` is not enough on its own: it covers the store, while placement
    // records, gitgate requests and box sessions live under the fleet root.
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_GITHUB_API: fx.github.url,
    SKEIN_CLAUDE_BIN: fx.claude,
    PATH: `${fx.bin}:${process.env.PATH}`,
  },
});
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
page.setDefaultTimeout(8000);
// The locator form of `page.$` / `page.waitForSelector` — what a check keeps when it is going to
// read from or act on what it found. See `harness/browser.mjs::finding` (SKEIN-716).
const find = finding(page);
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

// **What the browser was SENT on that stream, as against what the cockpit made of it**
// (SKEIN-766). The last check in this file could say only that a reading had not reached the page,
// and that is the one fact which cannot tell its two failures apart. A reading still being computed
// on a loaded box is LATE, and wants a budget that measures the term it multiplies; a reading the
// server finished and the page was never told about is LOST, and wants the stream mended —
// widening the budget would hide it for ever. So both ends are recorded and the failure says which
// happened, rather than leaving it to the next person's afternoon.
//
// A subclass installed before the page's own script runs, so it covers every `EventSource` the page
// opens — the one `connect` makes at load and any the browser makes after a reconnect. It is
// deliberately not read out of `revSums`, which holds what the cockpit CONCLUDED rather than what
// arrived.
//
// **And it can hold a frame back and then put it back.** The listener goes on inside the
// constructor, so it runs before the page's own `reading` listener and `stopImmediatePropagation`
// takes that frame away from the cockpit exactly as a dropped one would; `window.__release` then
// dispatches it again, marked so this listener lets it through the second time. That pair is the
// deterministic reproduction at the foot of this file — one run in three is not something anybody
// can iterate against.
//
// `window.__hold` is read at delivery rather than closed over, so a phase can arm it after the page
// has loaded. `$SKEIN_CONNECTIONS_HOLD` arms it from the start and nothing releases it, which is a
// frame simply lost — what to reach for when reading this suite's failure by hand.
await page.addInitScript(hold => {
  window.__readings = [];
  window.__hold = hold;         // the pull request whose frame is kept from the cockpit, or 0
  window.__heldFrames = [];     // what has been kept, with the socket to put it back on
  window.__release = () => {
    const kept = window.__heldFrames.splice(0);
    for (const { es, data } of kept) {
      const again = new MessageEvent("reading", { data });
      again.__replayed = true;
      es.dispatchEvent(again);
    }
    return kept.length;
  };
  const Real = window.EventSource;
  window.EventSource = class extends Real {
    constructor(...args) {
      super(...args);
      this.addEventListener("reading", e => {
        let d = null;
        try { d = JSON.parse(e.data); } catch {}
        const keep = !e.__replayed && window.__hold > 0 && d && d.number === window.__hold;
        window.__readings.push({
          key: d ? `${d.repo_id}#${d.number}` : "(unparseable)",
          at: Date.now(),
          error: (d && d.error) || "",
          depth: (d && d.summary && d.summary.depth) || "",
          held_back: !!keep,
          replayed: !!e.__replayed,
        });
        if (keep) { window.__heldFrames.push({ es: this, data: e.data }); e.stopImmediatePropagation(); }
      });
    }
  };
}, Number(process.env.SKEIN_CONNECTIONS_HOLD || 0));

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
  const row = await find("#revpane .revrow.stack .revline", { within: 10000 });
  if (!row) throw new Error("there is no stack row on screen to open");
  await row.click();
  await settle(400);
  // Read, then pressed, with a `textContent` round trip between them — the gap a repaint fits in
  // (SKEIN-716). A locator resolves the control again when it presses it.
  const btn = await find("#revpane .revrow.stack.open .stackread .revchip", { within: 10000 });
  if (!btn) throw new Error("the open stack row has no read control");
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

// **The budget is derived from this machine, and the timeout says what it saw** (SKEIN-743). It was
// a flat 60 s, guessed twice over: long enough to be no use as a signal on a quiet box, and
// reported as `page.waitForFunction: Timeout 60000ms exceeded`, which cannot tell a stream that
// never delivered from a machine that had not finished the model calls yet.
//
// What a round of readings costs is not a guess. `revStackPump` runs `REV_ASKED_PARALLEL` steps at
// a time over the stack's PRS, so the READS this check watches can take `ROUNDS` of them; a round
// is one READ_SECONDS model call plus a round trip to start each reading and another to stream it
// back, and `idle` is what one round trip costs HERE, measured above. `SLACK` is the only judgement
// in it, and it is a large one because the term it multiplies models the cost rather than measures
// it.
//
// Measured while writing this at `2fbf3ad`, by instrumenting this very wait: the readings arrive in
// 11.1-12.5 s across a ninefold spread in `idle` — 11.1 and 11.6 s alone (idle 13-15 ms), 11.4 s
// with the browser tier four lanes deep on one core (idle 41 ms), 12.5 s with six spinners besides
// on that core (idle 126 ms). Two rounds of `sleep 3` dominate it, so the budget is ~33 s where the
// box is quiet — failing in half the time the flat number took — and passes the old 60 s once a
// round trip here costs more than about 150 ms.
const ROUNDS = Math.ceil(PRS / READS);
const SLACK = 5;
const budget = SLACK * ROUNDS * (READ_SECONDS * 1000 + 2 * READS * idle);
/** How many of the first N pull requests hold a reading the page could show. */
const arrived = () => page.evaluate(n => {
  let got = 0;
  for (const pr of (revQueue.prs || []).slice(0, n)) {
    const s = revSums.get(pr.repo_id + "#" + pr.number);
    if (s && s !== "…" && !s.transient && s.depth !== "unread") got++;
  }
  return got;
}, READS);

/** Everything this suite can find out about a reading that has not shown up, from BOTH ends.
 *
 *  Four sources, because no one of them separates LATE from LOST. `revSums` is what the cockpit
 *  concluded. `__readings` is what the browser was actually sent. `/api/review/reading` is whether
 *  the server is still working on it. `?held=1` is whether the server ever wrote a reading for it
 *  at all — a request that can never spend a model call (`review::held`), so asking cannot change
 *  the answer it asks about.
 *
 *  One `page.evaluate` doing its own lookups rather than a read per row: two protocol calls around
 *  a page that is still settling readings would report a state that never existed (SKEIN-751). */
const whyMissing = async () => {
  const held = await (await fetch(`${base}/api/review/reading`, { headers: authHeader() })).json();
  const stillReading = new Set(held.map(r => `${r.repo_id}#${r.number}`));
  const rows = await page.evaluate(n => {
    const out = [];
    for (const pr of (revQueue.prs || []).slice(0, n)) {
      const key = pr.repo_id + "#" + pr.number;
      const s = revSums.get(key);
      if (s && s !== "…" && !s.transient && s.depth !== "unread") continue;
      out.push({
        key, repo: pr.repo_id, number: pr.number,
        holds: s === undefined ? "nothing at all"
          : s === "…" ? 'the "…" its press wrote, still unanswered'
          : s.transient ? `a transient failure — ${s.unread_because || "no reason given"}`
          : s.depth === "unread" ? `an unread row — ${s.unread_because || "no reason given"}`
          : `a reading of depth ${s.depth}`,
        waiting: revReadWaits.has(key),
        events: (window.__readings || []).filter(e => e.key === key),
      });
    }
    return out;
  }, READS);
  const said = [];
  for (const r of rows) {
    let disk = "";
    let onDisk = false;
    const got = await fetch(
      `${base}/api/repos/${encodeURIComponent(r.repo)}/review/${r.number}/summary?held=1`,
      { headers: authHeader() });
    if (!got.ok) {
      disk = `the server would not say what it holds (HTTP ${got.status}: ${(await got.text()).split("\n")[0]})`;
    } else {
      const s = await got.json();
      onDisk = !!(s && s.depth && s.depth !== "unread");
      disk = onDisk
        ? `the server DOES hold a ${s.depth} reading of ${String(s.head_sha).slice(0, 12)}`
        : `the server holds NO reading for it (${(s && s.unread_because) || "no reason given"})`;
    }
    const bad = r.events.find(e => e.error);
    const verdict =
      stillReading.has(r.key) ? "LATE — the server has not finished this one"
      : r.events.some(e => e.held_back) ? "LOST on purpose, and the page never recovered it"
      : bad ? `FAILED — the server published an error for it: ${bad.error}`
      : r.events.length
        ? `DELIVERED AND DROPPED — the stream carried a ${r.events[0].depth || "?"} reading and the page does not hold it`
      : onDisk ? "LOST — the server wrote the reading and the page was never told"
      : "NEITHER — nothing was published for it and nothing was written";
    said.push(`${r.key}: ${verdict}; the page holds ${r.holds}`
      + `${r.waiting ? ", still waiting on its press" : ""}; the stream delivered `
      + `${r.events.length ? `${r.events.length} frame(s) for it` : "nothing for it"}; ${disk}`);
  }
  return said;
};

await check("and the readings themselves still arrive, over the stream rather than on their own requests", async () => {
  // Polled from here rather than by `waitForFunction`, so that the count is a number this check can
  // still read when the budget runs out — the whole point being to say how far it got.
  const deadline = Date.now() + budget;
  let got = 0;
  while ((got = await arrived()) < READS && Date.now() < deadline) await settle(250);
  if (got === READS) return;
  // Which half failed, in the message rather than in the next person's afternoon — and NAMED, per
  // pull request, because "9 of 10" is the count that started SKEIN-766 and could not settle it.
  const still = await readingNow();
  const why = await whyMissing();
  throw new Error(`${got} of ${READS} readings reached the page in ${Math.round(budget / 1000)}s `
    + `(${ROUNDS} rounds of ${READ_SECONDS}s, idle ${idle} ms) — the server is still holding ${still}\n`
    + (why.length ? why.join("\n")
      // Nothing missing by the time the diagnosis asked, which is itself the answer: the page
      // repaired itself in the moments after the budget ran out, so this one was LATE and the
      // budget is the wrong size rather than the stream being at fault.
      : "and yet every one of them had arrived by the time this asked — the page finished just "
        + "after the budget ran out, so this run was LATE rather than short a reading"));
});

// **The same loss, made to happen** (SKEIN-766, SKEIN-754 before it). One reading of ten went
// missing about one loaded run in three — measured here as 1 of 12 four-lane runs on one core and 0
// of 12 unloaded — and 1-in-3 is not a check anybody can iterate against. So this keeps one
// reading's frame from the cockpit until the page's own recovery has given up on that reading, then
// puts the frame back. The row has to hold the reading afterwards. Before the fix it kept
// `revFetchHeld`'s "skein holds no reading of this pull request yet" for good, because
// `revReadSettle` dropped a reading nobody was waiting for any more.
//
// **Every step is waited for rather than slept through**, which is what makes it fail every time on
// a quiet box and a loaded one alike. The loss needs two things, and both are conditions this suite
// can watch for:
//
//   * the page must have SEEN the server holding this reading — `revPollInFlight` marks `seen` on
//     the wait, and settles only a wait it has seen;
//   * that poll must then settle the wait, which it does the moment the server stops listing the
//     reading — and the server stops listing it BEFORE it announces the answer, because
//     `api_review_read` drops the registration when the read returns and announces afterwards.
//
// It costs one more reading, which is one more `sleep 3`.
console.log("\nand a frame that lands after the page has given up on it");
const step = await page.evaluate(() => {
  const p = [...revStacks.values()][0].steps[0];
  return { key: p.repo_id + "#" + p.number, number: p.number };
});
await check("a step already read can be read again, with this suite keeping its frame back", async () => {
  await page.evaluate(n => { window.__hold = n; }, step.number);
  const row = await find(`#revpane .step[data-rk="${step.key}"]`, { within: 10000 });
  if (!row) throw new Error(`the open stack draws no row for ${step.key}`);
  await row.click();
  await settle(400);
  // The row's own control, not `revFetchSummary` — a press is what leaves the wait this is about.
  const again = await find(
    `#revpane .step[data-rk="${step.key}"] + .revbody .revrowacts .revchip:has-text("read it again")`,
    { within: 10000 });
  if (!again) throw new Error(`the opened step ${step.key} offers no way to read it again`);
  await again.click();
  // Seen by the page's own poll. Without this the rest would be a coin toss: a wait the poll never
  // saw is one it will not settle, and then there is nothing for the frame to arrive too late for.
  await page.waitForFunction(k => revReadWaits.get(k)?.seen === true, step.key, { timeout: 30000 });
});

await check("the page's own poll gives up on that reading before its frame arrives", async () => {
  // The server has stopped listing it and the poll has settled the wait — with the answer still
  // held back here, so nothing else can have settled it.
  await page.waitForFunction(k => !revReadWaits.has(k), step.key, { timeout: 30000 });
  const kept = await page.evaluate(() => (window.__heldFrames || []).length);
  if (kept !== 1) throw new Error(`this suite is holding ${kept} frames back, not the one it pressed for`);
});

await check("and the reading lands when its frame finally does", async () => {
  const put = await page.evaluate(() => window.__release());
  if (put !== 1) throw new Error(`${put} frames went back to the page, not the one held`);
  await page.waitForFunction(k => {
    const s = revSums.get(k);
    return !!(s && s !== "…" && !s.transient && s.depth !== "unread");
  }, step.key, { timeout: 10000 }).catch(async () => {
    const held = await page.evaluate(k => {
      const s = revSums.get(k);
      return s === undefined ? "nothing at all" : s === "…" ? 'the "…" its press wrote'
        : s.transient ? `a transient failure — ${s.unread_because || "no reason given"}`
        : s.depth === "unread" ? `an unread row — ${s.unread_because || "no reason given"}`
        : `a reading of depth ${s.depth}`;
    }, step.key);
    throw new Error(`the frame went back to the page and ${step.key} still holds ${held} — a reading `
      + "the page stopped waiting for is still the answer, and dropping it is how one goes missing");
  });
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
