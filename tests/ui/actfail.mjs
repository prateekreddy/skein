// **An act GitHub refused is on screen, and stays there** (SKEIN-385).
//
// Measured under Playwright against a build of `d48a4ce`, on `acme/testbed#20` — a
// pull request that genuinely cannot merge. Press `merge`, accept the confirmation, and the server
// answers:
//
//     200 /api/repos/testbed/review/20/act
//     {"error":"GitHub said 405: Pull Request has merge conflicts","ok":false}
//
// The page then polled every 250ms for twenty seconds for any text mentioning the failure —
// `conflict`, `405`, `could not merge`, `failed`, `refused`. The only matches were the pull
// request's own title. From the reader's side, pressing merge did nothing at all.
//
// **The sabotage this is written against**, and the reason it is a browser suite rather than a
// lifted world: the act route is made to refuse EVERY press, and each assertion below names the
// element that must carry the reason. A test that only checks the request went out passes with the
// whole failure path deleted, which is the state it was written in — `undo.mjs` already asserts the
// refusal receipt against the lifted functions, and it passed throughout, because what broke is the
// paint, not the logic. Only a real DOM can tell those apart.
//
// The refusal is faked in the BROWSER (`page.route`), not in the GitHub stub, on purpose: what is
// under test is what the page does with `ok:false`, and every kind of refusal — a conflict, a
// protected base, a missing approval, a lost race — arrives in exactly this shape. Fixing the one
// GitHub answer this was reported with would leave the others silent.
//
//   node tests/ui/actfail.mjs

import { chromium } from "playwright";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:net";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { serverBinary } from "./lift.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const API_TOKEN = "d".repeat(64);
const authHeader = () => ({ Authorization: `Bearer ${API_TOKEN}` });

// GitHub's own words for the case this was reported on, passed through by `crate::github` as
// `GitHub said {status}: {message}`. The test asserts on what the READER can see of it, never on
// this exact string being reproduced letter for letter — translating it into a sentence about
// conflicts is a separate, welcome change, and it must not break this suite.
const REFUSAL = "GitHub said 405: Pull Request has merge conflicts";

const freePort = () => new Promise(res => {
  const s = createServer();
  s.listen(0, "127.0.0.1", () => { const { port } = s.address(); s.close(() => res(port)); });
});

/// A GitHub the size of what this suite asks for. Same seam as `review.mjs` and `connections.mjs`:
/// skein reads the API, so the stub is an API.
async function createGitHub(prs) {
  const DIFF = "diff --git a/src/parser.rs b/src/parser.rs\n--- a/src/parser.rs\n+++ b/src/parser.rs\n"
    + "@@\n-    let head = input.chars().next().unwrap();\n"
    + "+    let Some(head) = input.chars().next() else { return Ok(()) };\n";
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", c => { body += c; });
    req.on("end", () => {
      const send = (code, payload, type = "application/json") => {
        res.writeHead(code, { "Content-Type": type });
        res.end(typeof payload === "string" ? payload : JSON.stringify(payload));
      };
      const url = req.url.split("?")[0];
      if (url === "/user") return send(200, { login: "me" });
      if (url === "/user/teams") return send(403, { message: "Requires read:org" });
      if (url === "/graphql") {
        const vars = JSON.parse(body || "{}").variables || {};
        const data = {};
        for (const [name, value] of Object.entries(vars)) {
          if (!/^q\d+$/.test(name)) continue;
          data[name] = { nodes: /review-requested:/.test(String(value)) ? prs : [] };
        }
        return send(200, { data });
      }
      if (/^\/repos\/[^/]+\/[^/]+\/pulls\/\d+\/files$/.test(url)) {
        return send(200, [{ filename: "src/parser.rs" }]);
      }
      if (/^\/repos\/[^/]+\/[^/]+\/pulls\/\d+$/.test(url)) return send(200, DIFF, "text/plain");
      send(404, { message: `no stub for ${url}` });
    });
  });
  return new Promise(resolve => {
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({ url: `http://127.0.0.1:${port}`, close: () => server.close() });
    });
  });
}

async function makeFixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "skein-actfail-ui-"));
  const bin = path.join(root, "bin");
  const home = path.join(root, "home");
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(root, "sandboxes.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  // `read_prs: false` — skein reads nothing here on its own. This suite is about what a PRESS
  // shows, and a background reading landing mid-assertion repaints the pane for its own reasons.
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    { id: "acme", source: "https://github.com/acme/thing.git",
      source_tree: path.join(root, "work"), read_prs: false,
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));

  // Two loose pull requests, both off `main` — not a stack. The receipt has a different home in a
  // stack (`revStackSteps` draws a step, not a `.revrow`), and that case has its own suite; this
  // one is about the reading view's bar and the plain row.
  const prs = [1, 2].map(number => ({
    number, title: `change number ${number}`, author: { login: "dana" },
    url: `https://github.com/acme/thing/pull/${number}`,
    headRefName: `feat-${number}`, headRefOid: `sha${number}`, baseRefName: "main",
    isDraft: false, updatedAt: "2026-08-20T00:00:00Z",
    latestReviews: { nodes: [] },
    commits: { nodes: [{ commit: { statusCheckRollup: null } }] },
  }));

  fs.mkdirSync(path.join(root, "work", ".github"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", ".github", "CODEOWNERS"), "src/ @me\n");
  fs.mkdirSync(path.join(root, "work", "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", "src", "parser.rs"), "const TIMEOUT: u64 = 5;\n");
  const wgit = (...a) => spawnSync("git", ["-C", path.join(root, "work"), ...a], { stdio: "ignore" });
  wgit("init", "-q", "-b", "main");
  wgit("add", "-A");
  wgit("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "the tree the mirror carries");

  const github = await createGitHub(prs);

  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, `#!/bin/sh\ncase "$1" in ls) echo '[]'; exit 0 ;; esac\nexit 0\n`);
  fs.chmodSync(sbx, 0o755);

  const claude = path.join(bin, "claude");
  fs.writeFileSync(claude, `#!/bin/sh
printf 'KIND: fix\\nLINE: stops the parser crashing on empty input.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\nREVIEW:\\nOVERALL: nothing to flag\\n'
exit 0
`);
  fs.chmodSync(claude, 0o755);
  return { root, bin, home, github, sbx, claude };
}

async function startServer(fx, port) {
  const srv = spawn(serverBinary(), {
    cwd: REPO,
    stdio: ["ignore", "pipe", "pipe"],
    env: {
      ...process.env,
      SKEIN_ADDR: `127.0.0.1:${port}`,
      SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
      SKEIN_LS_CMD: `${fx.sbx} ls --json`,
      SKEIN_HOME: fx.home,
      SKEIN_GITHUB_API: fx.github.url,
      SKEIN_CLAUDE_BIN: fx.claude,
      SKEIN_NO_GH_SECRET: "1",
      PATH: `${fx.bin}:${process.env.PATH}`,
    },
  });
  let log = "";
  srv.stdout.on("data", d => { log += d; });
  srv.stderr.on("data", d => { log += d; });
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/api/boxes`, { headers: authHeader() })).ok) {
        return { srv, log: () => log };
      }
    } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  srv.kill();
  throw new Error(`server never came up on ${port}\n${log}`);
}

// ---------- harness ----------
const results = [];
async function check(name, fn) {
  try { await fn(); results.push([true, name]); console.log(`  ok    ${name}`); }
  catch (e) { results.push([false, name]); console.log(`  FAIL  ${name}\n        ${String(e.message || e).split("\n")[0]}`); }
}

// ---------- run ----------
const fx = await makeFixture();
const port = await freePort();
const { srv, log } = await startServer(fx, port);
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
page.setDefaultTimeout(10000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("console", m => { if (m.type() === "error") noise.push(`[console] ${m.text()}`); });
// Merge asks first, and a dialog Playwright leaves alone is auto-dismissed — which would test the
// cancel path while reading like the merge path.
page.on("dialog", d => d.accept());

// **Every act refuses**, and the presses are counted so no assertion below can be satisfied by a
// button that did nothing. The answer is a 200 carrying `ok:false`, which is exactly what the
// server sends: an act GitHub refused is not an HTTP failure.
const acts = [];
await page.route("**/api/repos/*/review/*/act", async route => {
  acts.push(JSON.parse(route.request().postData() || "{}"));
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ ok: false, error: REFUSAL }),
  });
});

const settle = (ms = 400) => page.waitForTimeout(ms);
const base = `http://127.0.0.1:${port}`;

/// What a reader can see, anywhere on the page — the same question the report asked, asked the same
/// way. `innerText` rather than `textContent`, so an element that is in the markup and not on the
/// screen does not answer for one that is.
const onScreen = () => page.evaluate(() => document.body.innerText);
/// Does the reason a press failed appear ANYWHERE a reader would find it?
const saysWhy = async () => /405|conflict|refus|could not|did not go through/i.test(await onScreen());

await page.goto(`${base}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await settle(800);

console.log("\nthe pane");
await check("the queue arrives", async () => {
  await page.click("#revbtn");
  await settle(600);
  await page.evaluate(() => openReview("acme"));
  await page.waitForFunction(() => !revLoading && revQueue && (revQueue.prs || []).length >= 2,
    null, { timeout: 30000 });
});

// Opened by calling the page's own `openReading` rather than by clicking the row's "read the
// change" chip: that chip, and the row that carries it, are review.mjs's subject and are asserted
// there. What has to be real here is the bar and what a press to it does.
await check("the change is readable, and the merge control is beside it", async () => {
  await page.evaluate(() => openReading("acme", 1));
  await page.waitForSelector("#revpane .readdiff .diff", { timeout: 20000 });
  const bar = await page.$$eval("#revpane .readbar .revchip", els => els.map(e => e.textContent.trim()));
  if (!bar.includes("merge")) throw new Error(`no merge control in the reading view: ${JSON.stringify(bar)}`);
});

console.log("\na merge GitHub refuses");
await check("pressing merge really sends the act", async () => {
  await page.click("#revpane .readbar .revchip:has-text('merge')");
  for (let i = 0; i < 40 && !acts.length; i++) await settle(100);
  if (!acts.length) throw new Error("the press sent nothing — nothing below is about the refusal");
  if (acts[acts.length - 1].kind !== "merge") throw new Error(`the press sent ${acts[acts.length - 1].kind}`);
});

// **The assertion the report is about.** A NAMED element — the receipt in the bar the merge was
// pressed from — carries the reason. Not "something on the page changed": the reader pressed a
// control, and the answer belongs in the control they pressed.
await check("the bar the merge was pressed from wears the refusal", async () => {
  const el = await page.waitForSelector("#revpane .readbar .revreceipt.failed", { timeout: 10000 })
    .catch(() => null);
  if (!el) {
    throw new Error(`no .revreceipt.failed in the reading bar after a refused merge — the page says `
      + `${JSON.stringify((await page.$eval("#revpane .readbar", e => e.innerText).catch(() => "")).slice(0, 200))}`);
  }
  const said = (await el.innerText()).trim();
  if (!/405|conflict|refus/i.test(said)) throw new Error(`the receipt does not name the reason: ${JSON.stringify(said)}`);
});

await check("and a reader can see why, in words, anywhere on the page", async () => {
  if (!await saysWhy()) throw new Error(`the failure appears nowhere on screen: ${JSON.stringify((await onScreen()).slice(0, 400))}`);
});

// It STAYS. The measured failure had a twenty-second poll find nothing; the opposite defect — a
// receipt that flashes and clears — would read the same way to anyone not staring at the bar.
await check("the refusal is still there five seconds later, unpressed", async () => {
  await settle(5000);
  const el = await page.$("#revpane .readbar .revreceipt.failed");
  if (!el) throw new Error("the refusal cleared itself — a reader who looked away missed it entirely");
  const said = (await el.innerText()).trim();
  if (!/405|conflict|refus/i.test(said)) throw new Error(`the receipt lost the reason: ${JSON.stringify(said)}`);
});

await check("and it offers the two ways on: try again, and GitHub", async () => {
  const chips = await page.$$eval("#revpane .readbar .revchip", els => els.map(e => e.textContent.trim()));
  if (!chips.some(c => /try again/i.test(c))) throw new Error(`no way to retry the refused act: ${JSON.stringify(chips)}`);
  const link = await page.$("#revpane .readbar a[href*='/pull/1']");
  if (!link) throw new Error("no link to the pull request on GitHub, where the refusal can be understood");
});

await check("a refused merge never marks the row decided", async () => {
  const decided = await page.evaluate(() => [...revDecided]);
  if (decided.includes("acme#1")) throw new Error("a merge that never happened marked its row done");
});

console.log("\nleaving the reading view");
// The receipt lives where the press was, INSIDE the row's body — so a collapsed row used to look
// exactly like a row nothing had been asked of, and Esc took the only answer on screen away with
// it. The mark on the line is what survives that.
await check("the queue row carries the refusal after Esc", async () => {
  await page.keyboard.press("Escape");
  await settle(600);
  if (await page.$("#revpane .readbar")) throw new Error("esc did not leave the reading view");
  const el = await page.$('#revpane .revrow[data-rk="acme#1"] .revtag.refused');
  if (!el) throw new Error("the collapsed row shows no sign that the merge was refused");
  const said = (await el.innerText()).trim();
  if (!/merge/.test(said)) throw new Error(`the mark does not say which act was refused: ${JSON.stringify(said)}`);
  if (!/refus/i.test(said)) throw new Error(`the mark does not say it was refused: ${JSON.stringify(said)}`);
  // And the whole answer is one click away, where the press was — the mark is a way in, not a
  // replacement for the sentence.
  await page.click('#revpane .revrow[data-rk="acme#1"] .revline');
  await settle(400);
  const receipt = await page.$('#revpane .revrow[data-rk="acme#1"] .revreceipt.failed');
  if (!receipt) throw new Error("opening the row shows no receipt for the refused act");
  const why = (await receipt.innerText()).trim();
  if (!/405|conflict|refus/i.test(why)) throw new Error(`the row's receipt does not name the reason: ${JSON.stringify(why)}`);
});

console.log("\nthe press that vanished");
// **The paint that carries a refusal is never deferrable** (SKEIN-385).
//
// §6 rule 2 holds a render while the pane owns a caret or a live selection, so a summary landing
// cannot take the caret out of a half-typed comment. `revPendingPaint` handed its fallback to that
// same deferral — and a press's answer is the one render that must never wait for the reader's
// hands, which is what `renderReviewNow` exists to say (SKEIN-264, and `revRepaintRow` since
// SKEIN-284). Measured with it deferred: the act posted, GitHub refused it, `revPending` reached
// `failed`, and the bar went on offering `approve · request changes… · comment… · merge`. The
// receipt never appeared — not the refusal and not even the "posting…" before it.
//
// The press is dispatched with `el.click()` rather than the mouse, and that is a statement about
// the case rather than a shortcut: a real mouse click both collapses the selection and takes focus
// out of the textarea, so it CANNOT reach this branch, and a suite that drove the mouse here would
// pass with the deferral put back — which is exactly what it did before this comment was written.
// What is being asserted is the invariant, and the states it is asserted from are real ones: the
// composer whose corpse is still in the DOM is the one the press itself cleared, and the caret in
// it is where the reader left it.
await check("a refusal lands even when the pane is holding a render", async () => {
  const before = acts.length;
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await page.evaluate(() => openReading("acme", 2));
  await page.waitForSelector("#revpane .readdiff .diff", { timeout: 20000 });
  await page.click("#revpane .readbar .revchip:has-text('comment')");
  await page.waitForSelector("#revpane .revcompose textarea", { timeout: 5000 });
  // The precondition is read INSIDE the press, immediately before it: the fix rebuilds the pane
  // and takes the caret with it, so asking afterwards would ask about the wrong instant and answer
  // "no" for the one reason that means the fix worked.
  const held = await page.evaluate(() => {
    document.querySelector("#revpane .revcompose textarea").focus();
    const was = revRenderHeld();
    [...document.querySelectorAll("#revpane .readbar .revchip")]
      .find(e => e.textContent.trim() === "merge").click();
    return was;
  });
  if (!held) throw new Error("the pane was not holding the render at the press — not the case under test");
  for (let i = 0; i < 60 && acts.length === before; i++) await settle(100);
  if (acts.length === before) throw new Error("the press sent nothing — this is not the case under test");
  const el = await page.waitForSelector("#revpane .readbar .revreceipt.failed", { timeout: 10000 })
    .catch(() => null);
  if (!el) {
    const bar = await page.$eval("#revpane .readbar", e => e.innerText).catch(() => "(no .readbar)");
    const state = await page.evaluate(() => (revPending.get("acme#2") || {}).state || "(none)");
    throw new Error(`the act reached "${state}" and the bar still reads ${JSON.stringify(bar.trim())}`
      + " — the press vanished, which is the whole of SKEIN-385");
  }
  const said = (await el.innerText()).trim();
  if (!/405|conflict|refus/i.test(said)) throw new Error(`the receipt does not name the reason: ${JSON.stringify(said)}`);
});

console.log("\na verdict GitHub refuses, from the queue");
// The other half of `revAct`, and the shape of the owner's live report — "I clicked post on 729 PR
// and it never went through, it still says posting…". A verdict is HELD for eight seconds before it
// is sent, so the answer arrives long after the press: the receipt has to make it through the wait
// and through the "posting…" it wears meanwhile, and never stop there.
await check("a refused verdict is not left saying posting…", async () => {
  const before = acts.length;
  // A clean bar: the cases above left both rows wearing a refusal, and a bar that is already a
  // receipt has no verdict to press.
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await page.evaluate(() => openReading("acme", 2));
  await page.waitForSelector("#revpane .readdiff .diff", { timeout: 20000 });
  await page.waitForSelector("#revpane .readbar .revchip:has-text('approve')", { timeout: 10000 });
  await page.click("#revpane .readbar .revchip:has-text('approve')");
  // The undo window (REV_UNDO_MS = 8s) plus the round trip.
  for (let i = 0; i < 150 && acts.length === before; i++) await settle(100);
  if (acts.length === before) throw new Error("the held approval never posted at all");
  const el = await page.waitForSelector("#revpane .readbar .revreceipt.failed", { timeout: 10000 })
    .catch(() => null);
  if (!el) {
    const bar = await page.$eval("#revpane .readbar", e => e.innerText).catch(() => "");
    throw new Error(`the bar never said the approval was refused — it reads ${JSON.stringify(bar.trim())}`);
  }
  const said = (await el.innerText()).trim();
  if (/posting/i.test(said)) throw new Error(`the receipt is still saying it is posting: ${JSON.stringify(said)}`);
  if (!/405|conflict|refus/i.test(said)) throw new Error(`the receipt does not name the reason: ${JSON.stringify(said)}`);
});

await check("no page errors along the way", () => {
  if (noise.length) throw new Error(noise.join("\n"));
});

// ---------- report ----------
const failed = results.filter(([ok]) => !ok);
console.log(`\n${failed.length ? `${failed.length} of ${results.length} checks failed:` : `all ${results.length} checks passed`}`);
for (const [, name] of failed) console.log(`  ✗ ${name}`);
if (failed.length) console.log(`\nserver log:\n${log()}`);

await browser.close();
srv.kill();
fx.github.close();
if (!failed.length) fs.rmSync(fx.root, { recursive: true, force: true });
else console.log(`fixture kept for inspection: ${fx.root}`);
process.exit(failed.length ? 1 : 0);
