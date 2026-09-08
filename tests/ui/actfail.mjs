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
// **And the same defect on the other side of the pane** (SKEIN-416). `revAsk` and `revDraft` are
// presses whose ANSWER is the whole point, and both waited on the deferrable render. The checks at
// the bottom put the caret back in the composer while the answer is in flight — which is what a
// reader does while skein thinks — and assert the answer reaches the screen anyway.
//
// **And once more on the notes panel** (SKEIN-422), which is the same class again and the instance
// where the wait is longest: a module note takes about a minute, so by the time it lands the reader
// may be typing in the queue, or reading a pull request entirely. The last four checks are the two
// ends of `revModsPaint` — the answer lands while the panel is on screen, and it does NOT take the
// caret when it is not.
//
// **And the same panel about the wrong repository** (SKEIN-427), which is the one thing in this
// file that is not about paint at all. The checks at the bottom change the repo filter with the
// panel open and press a row the reader was looking at a moment ago; what they assert is that the
// press does not reach the network. The fixture has TWO repos for them — with one, the pane's scope
// falls back to it and the panel is about the right repo by accident.
//
//   node tests/ui/actfail.mjs

import { chromium } from "playwright";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { openDoor } from "./lift.mjs";
import { ledger } from "./harness/browser.mjs";
import { queueGitHub } from "./harness/github.mjs";
import { startServer } from "./harness/server.mjs";

const API_TOKEN = "d".repeat(64);

// GitHub's own words for the case this was reported on, passed through by `crate::github` as
// `GitHub said {status}: {message}`. The test asserts on what the READER can see of it, never on
// this exact string being reproduced letter for letter — translating it into a sentence about
// conflicts is a separate, welcome change, and it must not break this suite.
const REFUSAL = "GitHub said 405: Pull Request has merge conflicts";

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
  // **Two repos, because one repo cannot pose the question the notes panel gets wrong**
  // (SKEIN-427). `revScopeRepo()` falls back to the only repo in the queue, so with a single repo
  // there is always an honest scope and the panel is always about the repo on screen by accident.
  // The second one is what makes "the reader changed repo" and "no repo is chosen" real states.
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    { id: "acme", source: "https://github.com/acme/thing.git",
      source_tree: path.join(root, "work"), read_prs: false,
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
    { id: "bravo", source: "https://github.com/bravo/thing.git",
      source_tree: path.join(root, "work-bravo"), read_prs: false,
      store: path.join(root, "store-bravo"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));

  // Three loose pull requests, all off `main` — not a stack. The receipt has a different home in a
  // stack (`revStackSteps` draws a step, not a `.revrow`), and that case has its own suite; this
  // one is about the open row's control strip and the collapsed line.
  //
  // **1 and 2 say nothing about `mergeable` at all, and that is the point** (SKEIN-415). That is
  // how GitHub answers for a while after every push, and how a queue an older skein remembered
  // arrives: unknown, which is never "no". They are the rows every check above presses merge on,
  // so if unknown were ever read as a conflict this suite would go quiet rather than fail. 3 is
  // the one GitHub has already refused.
  const prs = [1, 2, 3].map(number => ({
    number, title: `change number ${number}`, author: { login: "dana" },
    url: `https://github.com/acme/thing/pull/${number}`,
    headRefName: `feat-${number}`, headRefOid: `sha${number}`, baseRefName: "main",
    isDraft: false, updatedAt: "2026-08-20T00:00:00Z",
    latestReviews: { nodes: [] },
    commits: { nodes: [{ commit: { statusCheckRollup: null } }] },
    ...(number === 3 ? { mergeable: "CONFLICTING", mergeStateStatus: "DIRTY" } : {}),
  }));

  fs.mkdirSync(path.join(root, "work", ".github"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", ".github", "CODEOWNERS"), "src/ @me\n");
  fs.mkdirSync(path.join(root, "work", "src"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", "src", "parser.rs"), "const TIMEOUT: u64 = 5;\n");
  const wgit = (...a) => spawnSync("git", ["-C", path.join(root, "work"), ...a], { stdio: "ignore" });
  wgit("init", "-q", "-b", "main");
  wgit("add", "-A");
  wgit("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "the tree the mirror carries");

  // The second repo, with its work under a different name from acme's. Not decoration: the panel's
  // rows are paths, and two repos that both have `src` would let a row of one pass for a row of the
  // other — which is the confusion under test, not a way to detect it.
  fs.mkdirSync(path.join(root, "work-bravo", "lib"), { recursive: true });
  fs.writeFileSync(path.join(root, "work-bravo", "lib", "engine.rs"), "const WORKERS: u8 = 2;\n");
  const bgit = (...a) => spawnSync("git", ["-C", path.join(root, "work-bravo"), ...a], { stdio: "ignore" });
  bgit("init", "-q", "-b", "main");
  bgit("add", "-A");
  bgit("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "the other repo in the fleet");

  const github = await queueGitHub(prs);

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
page.setDefaultTimeout(10000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("console", m => { if (m.type() === "error") noise.push(`[console] ${m.text()}`); });
// Merge asks first, and a dialog Playwright leaves alone is auto-dismissed — which would test the
// cancel path while reading like the merge path.
page.on("dialog", d => d.accept());

// **Every act that reaches GitHub refuses**, and the presses are counted so no assertion below can
// be satisfied by a button that did nothing. The answer is a 200 carrying `ok:false`, which is
// exactly what the server sends: an act GitHub refused is not an HTTP failure.
//
// `ask` and `draft` are the exception, and not an inconsistency: they go nowhere near GitHub — they
// are skein answering the reader — so there is no refusal to pose, and what SKEIN-416 is about is
// what the page does with the ANSWER. They are held back by `answerDelayMs` on purpose: an answer
// that arrives in the same frame as the press never meets the state under test, which is the reader
// having put their hands back in the composer while skein thinks.
//
// Strings no other part of this page produces, so "the answer is on screen" cannot be answered by
// the chip, the label, or the question the reader typed — the same rule `revdraft.mjs` states.
const ANSWERED = "DEEP-IN-THE-ANSWER-it-takes-the-lock-twice-on-the-error-path";
const DRAFTED = "DEEP-IN-THE-DRAFT-this-returns-before-the-unlock";
let answerDelayMs = 0;
const acts = [];
await page.route("**/api/repos/*/review/*/act", async route => {
  const sent = JSON.parse(route.request().postData() || "{}");
  acts.push(sent);
  if (sent.kind === "ask" || sent.kind === "draft") {
    if (answerDelayMs) await new Promise(r => setTimeout(r, answerDelayMs));
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ ok: true, text: sent.kind === "ask" ? ANSWERED : DRAFTED }),
    });
  }
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

// Opened by calling the page's own `toggleRevRow` rather than by clicking the row's line: the line
// and what it draws are review.mjs's subject and are asserted there. What has to be real here is
// the row's control strip and what a press to it does.
//
// `toggleRevRow` TOGGLES, and most checks below inherit whatever row the one before them left open
// — so opening is spelled "fold everything, then open this", which also forces the fresh render a
// check that has just cleared `revPending` is asking for.
const openRow = async key => {
  // A group that draws a count until it is asked (SKEIN-302) has no rows in the DOM at all, and a
  // conflicted pull request lands in one — so the group is opened first, exactly as a reader would,
  // and only where it is actually folded (clicking an open one would close it).
  await page.evaluate(() => {
    for (const lane of document.querySelectorAll("#revpane .revlane")) {
      const h = lane.querySelector("h4.revfold");
      if (h && !lane.querySelector(".revrow")) h.click();
    }
  });
  await settle(300);
  await page.evaluate(k => { for (const o of [...revOpen]) toggleRevRow(o); toggleRevRow(k); }, key);
  await page.waitForSelector(`#revpane .revrow[data-rk="${key}"].open .revrowacts`, { timeout: 20000 })
    .catch(async () => {
      const where = await page.evaluate(() => [...document.querySelectorAll("#revpane .revlane")]
        .map(l => [l.dataset.lane, [...l.querySelectorAll(".revrow")].map(r => r.dataset.rk)]));
      throw new Error(`${key} never opened — the queue on screen is ${JSON.stringify(where)}`);
    });
};
await check("the row opens, and the merge control is in it", async () => {
  await openRow("acme#1");
  const acts = await page.$$eval("#revpane .revrow.open .revrowacts .revchip", els => els.map(e => e.textContent.trim()));
  if (!acts.includes("merge")) throw new Error(`no merge control on the open row: ${JSON.stringify(acts)}`);
});

console.log("\na merge GitHub refuses");
await check("pressing merge really sends the act", async () => {
  await page.click("#revpane .revrow.open .revrowacts .revchip:has-text('merge')");
  for (let i = 0; i < 40 && !acts.length; i++) await settle(100);
  if (!acts.length) throw new Error("the press sent nothing — nothing below is about the refusal");
  if (acts[acts.length - 1].kind !== "merge") throw new Error(`the press sent ${acts[acts.length - 1].kind}`);
});

// **The assertion the report is about.** A NAMED element — the receipt in the bar the merge was
// pressed from — carries the reason. Not "something on the page changed": the reader pressed a
// control, and the answer belongs in the control they pressed.
await check("the strip the merge was pressed from wears the refusal", async () => {
  const el = await page.waitForSelector("#revpane .revrow.open .revrowacts .revreceipt.failed", { timeout: 10000 })
    .catch(() => null);
  if (!el) {
    throw new Error(`no .revreceipt.failed on the row's strip after a refused merge — the page says `
      + `${JSON.stringify((await page.$eval("#revpane .revrow.open .revrowacts", e => e.innerText).catch(() => "")).slice(0, 200))}`);
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
  const el = await page.$("#revpane .revrow.open .revrowacts .revreceipt.failed");
  if (!el) throw new Error("the refusal cleared itself — a reader who looked away missed it entirely");
  const said = (await el.innerText()).trim();
  if (!/405|conflict|refus/i.test(said)) throw new Error(`the receipt lost the reason: ${JSON.stringify(said)}`);
});

await check("and it offers the two ways on: try again, and GitHub", async () => {
  const chips = await page.$$eval("#revpane .revrow.open .revrowacts .revchip", els => els.map(e => e.textContent.trim()));
  if (!chips.some(c => /try again/i.test(c))) throw new Error(`no way to retry the refused act: ${JSON.stringify(chips)}`);
  const link = await page.$("#revpane .revrow.open .revrowacts a[href*='/pull/1']");
  if (!link) throw new Error("no link to the pull request on GitHub, where the refusal can be understood");
});

await check("a refused merge never marks the row decided", async () => {
  const decided = await page.evaluate(() => [...revDecided]);
  if (decided.includes("acme#1")) throw new Error("a merge that never happened marked its row done");
});

console.log("\nfolding the row");
// The receipt lives where the press was, INSIDE the row's body — so a collapsed row used to look
// exactly like a row nothing had been asked of, and Esc took the only answer on screen away with
// it. The mark on the line is what survives that.
await check("the queue row carries the refusal after Esc", async () => {
  await page.keyboard.press("Escape");
  await settle(600);
  if (await page.$("#revpane .revrow.open")) throw new Error("esc did not fold the row");
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
  await openRow("acme#2");
  await page.click("#revpane .revrow.open .revrowacts .revchip:has-text('comment')");
  await page.waitForSelector("#revpane .revcompose textarea", { timeout: 5000 });
  // The precondition is read INSIDE the press, immediately before it: the fix rebuilds the pane
  // and takes the caret with it, so asking afterwards would ask about the wrong instant and answer
  // "no" for the one reason that means the fix worked.
  const held = await page.evaluate(() => {
    document.querySelector("#revpane .revcompose textarea").focus();
    const was = revRenderHeld();
    [...document.querySelectorAll("#revpane .revrow.open .revrowacts .revchip")]
      .find(e => e.textContent.trim() === "merge").click();
    return was;
  });
  if (!held) throw new Error("the pane was not holding the render at the press — not the case under test");
  for (let i = 0; i < 60 && acts.length === before; i++) await settle(100);
  if (acts.length === before) throw new Error("the press sent nothing — this is not the case under test");
  const el = await page.waitForSelector("#revpane .revrow.open .revrowacts .revreceipt.failed", { timeout: 10000 })
    .catch(() => null);
  if (!el) {
    const bar = await page.$eval("#revpane .revrow.open .revrowacts", e => e.innerText).catch(() => "(no open row)");
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
  // A clean strip: the cases above left both rows wearing a refusal, and a strip that is already a
  // receipt has no verdict to press.
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await openRow("acme#2");
  await page.waitForSelector("#revpane .revrow.open .revrowacts .revchip:has-text('approve')", { timeout: 10000 });
  await page.click("#revpane .revrow.open .revrowacts .revchip:has-text('approve')");
  // The undo window (REV_UNDO_MS = 8s) plus the round trip.
  for (let i = 0; i < 150 && acts.length === before; i++) await settle(100);
  if (acts.length === before) throw new Error("the held approval never posted at all");
  const el = await page.waitForSelector("#revpane .revrow.open .revrowacts .revreceipt.failed", { timeout: 10000 })
    .catch(() => null);
  if (!el) {
    const acts = await page.$eval("#revpane .revrow.open .revrowacts", e => e.innerText).catch(() => "");
    throw new Error(`the row never said the approval was refused — it reads ${JSON.stringify(acts.trim())}`);
  }
  const said = (await el.innerText()).trim();
  if (/posting/i.test(said)) throw new Error(`the receipt is still saying it is posting: ${JSON.stringify(said)}`);
  if (!/405|conflict|refus/i.test(said)) throw new Error(`the receipt does not name the reason: ${JSON.stringify(said)}`);
});

console.log("\nan answer to a press the reader is waiting on");
// **The other half of the class** (SKEIN-416). `revPendingPaint` was converted under SKEIN-385;
// `revAsk` and `revDraft` still handed their answer to the deferrable render.
//
// The state is posed the way it actually occurs, and it is NOT the press that is held. A press
// forces its own paint already, and rebuilding the pane takes the caret with it — so the held
// state cannot be set up before the press and survive it. It is set up DURING the request: the
// reader presses, then puts their hands back in the box they typed the question into, which is
// what anybody does while a model call runs. `revRenderHeld()` is read from the page itself rather
// than assumed, twice — once when focus is placed and once after a pause — so a suite that had
// quietly lost the caret would fail here rather than pass on a branch it never reached.
//
// `revAsk`/`revDraft` are dispatched from `page.evaluate` and not by clicking the chip, for the
// reason spelled out above: a real mouse press collapses the selection and moves focus, and the
// deferral would be off at the answer for reasons that have nothing to do with the fix.
async function pressAndHoldTheCaret(press) {
  const before = acts.length;
  await page.evaluate(press);
  await settle(150);
  const held = await page.evaluate(() => {
    const ta = document.querySelector("#revpane .revcompose textarea");
    if (!ta) return "the composer went away at the press";
    ta.focus();
    ta.setSelectionRange(0, 0);
    return revRenderHeld() ? "" : "focusing the composer did not hold the render";
  });
  if (held) throw new Error(`${held} — not the case under test`);
  await settle(300);
  if (!await page.evaluate(() => revRenderHeld())) {
    throw new Error("the pane stopped holding the render before the answer landed — the case under test evaporated");
  }
  if (acts.length === before) throw new Error("the press sent nothing — this is not the case under test");
}

await check("an answer to ask… reaches the screen with the caret still in the composer", async () => {
  answerDelayMs = 1200;
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await openRow("acme#1");
  await page.evaluate(() => revCompose("acme", 1, "ask"));
  await page.waitForSelector("#revpane .revcompose textarea", { timeout: 5000 });
  await page.evaluate(() => { revComposing.text = "why is the lock taken here?"; });
  await pressAndHoldTheCaret(() => revAsk(1));
  const said = await page.waitForSelector("#revpane .revcompose .revanswer", { timeout: 10000 })
    .then(el => el.innerText())
    .catch(() => "");
  if (!said.includes(ANSWERED)) {
    const box = await page.$eval("#revpane .revcompose", e => e.innerText).catch(() => "(no composer)");
    throw new Error(`the answer never reached the screen — the composer reads ${JSON.stringify(box.trim().slice(0, 200))}`);
  }
});

// Worse than invisible, and the reason this one is asserted on the textarea's VALUE: on success the
// drafted review is written to `revComposing.text` and only a render puts it in the box. Deferred,
// the box still holds what the reader typed — and the next keystroke's `oninput` writes that stale
// value straight back over the draft, so carrying on typing threw the whole thing away.
await check("a review skein drafted is in the box, not just in state", async () => {
  answerDelayMs = 1200;
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await page.evaluate(() => revCompose("acme", 1, "comment"));
  await page.waitForSelector("#revpane .revcompose textarea", { timeout: 5000 });
  await page.evaluate(() => { revComposing.text = "the error path looks wrong to me"; });
  await pressAndHoldTheCaret(() => revDraft(1));
  const inTheBox = await page.waitForFunction(
    d => (document.querySelector("#revpane .revcompose textarea") || {}).value?.includes(d),
    DRAFTED, { timeout: 10000 }).then(() => true).catch(() => false);
  if (!inTheBox) {
    const state = await page.evaluate(() => (revComposing || {}).text || "(no composer)");
    const box = await page.$eval("#revpane .revcompose textarea", e => e.value).catch(() => "(no box)");
    throw new Error(`the draft is in state as ${JSON.stringify(state.slice(0, 80))} and the box still `
      + `reads ${JSON.stringify(box.slice(0, 80))} — the press did nothing anybody could see`);
  }
  answerDelayMs = 0;
});

// The other side of the same rule, and the reason the paint is CONDITIONAL. `c` is captured at the
// press; by the time an answer lands the reader may have cancelled that composer and be typing in
// another. Forcing then would take their caret for an answer nothing on screen is waiting for —
// the exact harm §6 rule 2 exists to prevent — so the deferral is right there and kept.
await check("an answer nobody is waiting for does not take the caret out of the next composer", async () => {
  answerDelayMs = 1500;
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await page.evaluate(() => revCompose("acme", 1, "ask"));
  await page.waitForSelector("#revpane .revcompose textarea", { timeout: 5000 });
  await page.evaluate(() => { revComposing.text = "a question the reader gives up on"; });
  await page.evaluate(() => revAsk(1));
  // Give up on it and start a comment instead, while the ask is still in flight.
  await page.evaluate(() => { revComposeClose(); revCompose("acme", 1, "comment"); });
  await page.waitForSelector("#revpane .revcompose textarea", { timeout: 5000 });
  await settle(200);
  const ready = await page.evaluate(() => {
    const ta = document.querySelector("#revpane .revcompose textarea");
    ta.focus();
    ta.value = "half a thought";
    ta.dispatchEvent(new Event("input"));
    return document.activeElement === ta && revRenderHeld();
  });
  if (!ready) throw new Error("the second composer never took the caret — not the case under test");
  // The stale answer lands here.
  await settle(2000);
  const kept = await page.evaluate(() => {
    const ta = document.querySelector("#revpane .revcompose textarea");
    return { focused: !!ta && document.activeElement === ta, value: ta ? ta.value : "(no box)",
             answered: !!document.querySelector("#revpane .revcompose .revanswer") };
  });
  if (!kept.focused) throw new Error(`the caret was taken out of the composer the reader was typing in — the box now reads ${JSON.stringify(kept.value)}`);
  if (kept.value !== "half a thought") throw new Error(`what the reader was typing was replaced: ${JSON.stringify(kept.value)}`);
  if (kept.answered) throw new Error("an answer to a composer that was cancelled was drawn into the one that replaced it");
  answerDelayMs = 0;
});

console.log("\na refusal that arrives while you are somewhere else");
// **The moment it happens, for a reader who has moved on** (SKEIN-417).
//
// A verdict fires eight seconds AFTER the press, so being elsewhere when GitHub refuses it is the
// ordinary case rather than the edge. `revFire` says it out loud then, and the toast was its only
// voice at that moment — 3500ms, no link, on a pane the reader may not even be looking at. The row
// keeps its durable mark either way (asserted below, because a longer toast must not have been
// bought by dropping it), but the mark is what you find LATER; this is about being told now.
//
// The wait is measured from when the toast actually appears, not from the press: 3500ms is the life
// under test, and a check that started its clock at the press would be asserting the round trip.
await check("a refusal you are not looking at links to the pull request and outlives 3.5 seconds", async () => {
  const before = acts.length;
  await page.evaluate(() => {
    revPending.clear(); revComposing = null;
    const t = document.getElementById("toast");
    if (t) { t.classList.remove("show"); t.innerHTML = ""; }
  });
  await openRow("acme#2");
  await page.waitForSelector("#revpane .revrow.open .revrowacts .revchip:has-text('approve')", { timeout: 10000 });
  await page.click("#revpane .revrow.open .revrowacts .revchip:has-text('approve')");
  // Away, before the window lapses — the whole point is that the strip the press was made in is not
  // on screen when the answer comes back.
  await page.keyboard.press("Escape");
  await settle(400);
  if (await page.$("#revpane .revrow.open")) throw new Error("esc did not fold the row — its strip is still on screen");

  // The undo window (8s) plus the round trip.
  let shown = null;
  for (let i = 0; i < 200 && !shown; i++) {
    shown = await page.evaluate(() => {
      const t = document.getElementById("toast");
      return t && t.classList.contains("show") ? { at: Date.now(), text: t.innerText, href: (t.querySelector("a") || {}).href || "" } : null;
    });
    if (!shown) await settle(100);
  }
  if (acts.length === before) throw new Error("the held approval never posted at all");
  if (!shown) throw new Error("nothing was said at all when the act was refused away from its row");
  if (!/#2/.test(shown.text) || !/405|conflict|refus/i.test(shown.text)) {
    throw new Error(`the notice does not say which pull request was refused, or why: ${JSON.stringify(shown.text)}`);
  }
  if (shown.href !== "https://github.com/acme/thing/pull/2") {
    throw new Error(`the notice is not a way to the pull request — its link is ${JSON.stringify(shown.href)}`);
  }
  // Past 3500ms, measured from the toast itself. The old life is the thing being ruled out.
  const waitFrom = shown.at;
  while (Date.now() - waitFrom < 5000) await settle(200);
  const still = await page.evaluate(() => {
    const t = document.getElementById("toast");
    return !!t && t.classList.contains("show");
  });
  if (!still) throw new Error(`the notice was gone ${Date.now() - waitFrom}ms after it appeared — a reader who glanced away missed the only thing said at the moment it happened`);

  // And none of that was bought by dropping what SKEIN-385 put on the row.
  const mark = await page.$('#revpane .revrow[data-rk="acme#2"] .revtag.refused');
  if (!mark) throw new Error("the row lost its durable mark for the refused act");
});

console.log("\na merge skein already knows GitHub will refuse");
// **The smaller half, and deliberately after the rest** (SKEIN-415). Disabling a control can only
// be an improvement once a failed act is visible wherever it happens — otherwise it is one more
// way for a press to go quiet. So this is asserted with the whole of the suite above still
// standing, and with the reason on screen rather than in a tooltip: a control that is dim and mute
// is worse than one that fails loudly.
const barChips = () => page.$$eval("#revpane .revrow.open .revrowacts .revchip",
  els => els.map(e => ({ text: e.textContent.trim(), disabled: e.disabled, title: e.title })));

await check("a merge GitHub has already refused is disabled, and says so on the screen", async () => {
  const before = acts.length;
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await openRow("acme#3");
  const known = await page.evaluate(() => (revKeyPr("acme#3") || {}).mergeable);
  if (known !== false) throw new Error(`the row skein is holding says mergeable=${JSON.stringify(known)} — nothing here is about a conflict`);
  const chips = await barChips();
  const merge = chips.find(c => c.text === "merge");
  if (!merge) throw new Error(`the merge control vanished instead of saying why it cannot run: ${JSON.stringify(chips.map(c => c.text))}`);
  if (!merge.disabled) throw new Error("a merge GitHub has already refused is still offered as a live control");
  // The reason, in words, next to the control it is about — not only in a `title` nobody hovers.
  const why = await page.$eval("#revpane .revrow.open .revrowacts .revcannot", e => e.innerText.trim()).catch(() => "");
  if (!/conflict/i.test(why) || !/main/.test(why)) {
    throw new Error(`the disabled control does not say why on the screen — beside it reads ${JSON.stringify(why)}`);
  }
  // And it took nothing else with it: a conflicted change is still one you can approve or comment
  // on, which is most of what this pane is for.
  const alsoDead = chips.filter(c => c.text !== "merge" && c.disabled).map(c => c.text);
  if (alsoDead.length) throw new Error(`disabling the merge took the verdicts with it: ${JSON.stringify(alsoDead)}`);
  // Pressed anyway, the way a reader would: nothing goes out.
  await page.evaluate(() => {
    const el = [...document.querySelectorAll("#revpane .revrow.open .revrowacts .revchip")].find(e => e.textContent.trim() === "merge");
    el.click();
  });
  await settle(400);
  if (acts.length !== before) throw new Error("the disabled merge still sent an act");
});

// Unknown is not "no", all the way from `prq::Pr::mergeable`'s `Option<bool>`. GitHub reports
// UNKNOWN for a while after every push, and taking the merge away for that long would be a worse
// bug than the one being fixed — a pull request that merges perfectly well, with no way to merge it.
await check("a pull request GitHub has not judged yet still offers the merge", async () => {
  await page.evaluate(() => { revPending.clear(); revComposing = null; });
  await openRow("acme#1");
  const known = await page.evaluate(() => (revKeyPr("acme#1") || {}).mergeable);
  if (known === false) throw new Error("the row posed as unknown is not unknown — this check proves nothing");
  const merge = (await barChips()).find(c => c.text === "merge");
  if (!merge) throw new Error("no merge control at all on a pull request nothing is known to be wrong with");
  if (merge.disabled) {
    throw new Error(`unknown was read as "no": mergeable=${JSON.stringify(known)} and the merge was taken away anyway`);
  }
});

console.log("\na note skein is writing about a module");
// **The third member of the class, and the one SKEIN-416's audit walked past** (SKEIN-422).
// `writeModule` painted its press with `renderReview` and both of its answers with it too — the
// failure arm directly, the success arm through `loadModules` — and §6 rule 2 holds that render
// while the pane owns a caret or a live selection.
//
// It is the instance where waiting is most likely, not least: a note takes about a minute, so the
// reader's hands have moved on long before the answer lands. `revModsPaint` forces only while the
// panel is what is on screen, and the last check is the other side of that.
//
// The panel is faked in the browser, the same seam and the same reason as the acts above: what is
// under test is what the page does with the ANSWER. `writeDelayMs` holds the write back, because an
// answer that arrives in the same frame as the press never meets the state under test.
const MODULE = "src/DEEP-IN-THE-NOTES-the-parser-nobody-wrote-up";
// bravo's list has nothing in common with acme's, so a row on screen names the repo it came from
// without anybody having to be told (SKEIN-427).
const BRAVO_MODULE = "lib/DEEP-IN-THE-NOTES-nothing-in-acme-is-called-this";
let modState = "absent";
let writeDelayMs = 0;
// One repo's module list held back, so a reader can change repo again while it is in flight.
let slowRepo = "";
let modsDelayMs = 0;
const notesWritten = [];
/// Which repo a `/api/repos/<id>/…` request is for.
const repoOf = url => new URL(url).pathname.split("/")[3];
await page.route("**/api/repos/*/modules", async route => {
  const repo = repoOf(route.request().url());
  const module = repo === "bravo" ? BRAVO_MODULE : MODULE;
  if (modsDelayMs && repo === slowRepo) await new Promise(r => setTimeout(r, modsDelayMs));
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    // `modState` is acme's — it is the module the write checks above rewrite. bravo's note is only
    // ever absent, so a state dot cannot be mistaken for the other repo's.
    body: JSON.stringify({
      modules: [{ path: module, owners: [], state: repo === "bravo" ? "absent" : modState, written: "" }],
      unread_because: "",
    }),
  });
});
await page.route("**/api/repos/*/modules/write", async route => {
  // The repo out of the URL as well as the body: a note written against the wrong repo is a
  // request whose PATH and payload disagree, and a record of only one of them cannot show it.
  notesWritten.push({ repo: repoOf(route.request().url()), ...JSON.parse(route.request().postData() || "{}") });
  if (writeDelayMs) await new Promise(r => setTimeout(r, writeDelayMs));
  modState = "fresh";   // the note now exists, which is what the reload after it will say
  await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ ok: true }) });
});

/// The module paths the panel is drawing, in order.
const panelPaths = () =>
  page.$$eval("#revpane .revmod code", els => els.map(e => e.textContent.trim()));

/// The queue view, scoped to acme, with the notes panel open on one module that has never been
/// written up — whatever the check before this one left on the screen.
async function withTheNotesPanelOpen() {
  modState = "absent";
  writeDelayMs = 0;
  await page.keyboard.press("Escape");
  await settle(300);
  // A composer another check left half-typed keeps the caret and swallows Esc, and that caret also
  // holds the render — so the way out is the page's own `toggleRevRow`, which is what Esc calls.
  // Setup, not the thing under test: what these checks are about starts once the queue is on screen.
  if (await page.$("#revpane .revrow.open")) {
    await page.evaluate(() => {
      revComposing = null;
      if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
      for (const k of [...revOpen]) toggleRevRow(k);
    });
    await settle(300);
  }
  if (await page.$("#revpane .revrow.open")) throw new Error("the open row would not fold");
  await page.evaluate(() => {
    revPending.clear(); revComposing = null; revWriting = ""; revMods = null; revModsRepo = "";
    // Back to acme: the checks at the bottom leave the pane on another repo or on none, and the
    // panel is now about a repo rather than about the pane.
    openReview("acme");
    if (revModsOpen) toggleMods();
    toggleMods();
  });
  await page.waitForFunction(
    m => [...document.querySelectorAll("#revpane .revmod code")].some(e => e.textContent.trim() === m),
    MODULE, { timeout: 10000 });
}

await check("pressing write a note says writing… while the pane is holding a render", async () => {
  await withTheNotesPanelOpen();
  writeDelayMs = 1200;
  const before = notesWritten.length;
  // Both halves are read INSIDE the press. The precondition immediately before it, because the fix
  // rebuilds the pane and takes the caret with it, so asking afterwards asks about the wrong
  // instant and answers "no" for the one reason that means it worked. The chips immediately after
  // it, synchronously — a deferred render cannot have run in between, so this cannot be satisfied
  // by a repaint that arrived for some other reason.
  //
  // Dispatched by calling `writeModule`, not by clicking the chip: a real mouse press collapses the
  // selection and moves focus, so it cannot reach this branch at all.
  const seen = await page.evaluate(p => {
    const box = document.querySelector("#revpane .revsearch");
    box.focus();
    const was = revRenderHeld();
    writeModule(p);
    return { was, chips: [...document.querySelectorAll("#revpane .revmod .revchip")].map(e => e.textContent.trim()) };
  }, MODULE);
  if (!seen.was) throw new Error("the pane was not holding the render at the press — not the case under test");
  for (let i = 0; i < 60 && notesWritten.length === before; i++) await settle(100);
  if (notesWritten.length === before) throw new Error("the press sent nothing — this is not the case under test");
  if (!seen.chips.includes("writing…")) {
    throw new Error(`the press did nothing anybody could see — the panel's chips read ${JSON.stringify(seen.chips)}`);
  }
});

await check("the written note reaches the panel with the caret still in the pane", async () => {
  await withTheNotesPanelOpen();
  writeDelayMs = 1200;
  const before = notesWritten.length;
  await page.evaluate(p => writeModule(p), MODULE);
  await settle(150);
  // The held state is posed DURING the request and not before it, for the reason
  // `pressAndHoldTheCaret` gives above: the press forces its own paint, so a caret placed before it
  // cannot survive to the answer. `revRenderHeld()` is read from the page twice, once when focus is
  // placed and once after a pause, so a check that had quietly lost the caret fails here rather
  // than passing on a branch it never reached.
  const held = await page.evaluate(() => {
    const box = document.querySelector("#revpane .revsearch");
    if (!box) return "the queue's search box went away at the press";
    box.focus();
    return revRenderHeld() ? "" : "focusing the pane did not hold the render";
  });
  if (held) throw new Error(`${held} — not the case under test`);
  await settle(300);
  if (!await page.evaluate(() => revRenderHeld())) {
    throw new Error("the pane stopped holding the render before the answer landed — the case under test evaporated");
  }
  if (notesWritten.length === before) throw new Error("the press sent nothing — this is not the case under test");
  // Asserted on the ROW, not on state: the chip out of "writing…" and back to live, and the state
  // dot on the note that now exists. Both come from `revModsHtml`, which only a render runs.
  const landed = await page.waitForFunction(() => {
    const chip = document.querySelector("#revpane .revmod .revchip");
    const dot = document.querySelector("#revpane .revmod .revmodstate");
    return !!chip && !chip.disabled && chip.textContent.trim() === "re-write" && !!dot && dot.classList.contains("fresh");
  }, null, { timeout: 10000 }).then(() => true).catch(() => false);
  if (!landed) {
    const now = await page.evaluate(() => {
      const chip = document.querySelector("#revpane .revmod .revchip");
      const dot = document.querySelector("#revpane .revmod .revmodstate");
      return {
        chip: chip ? `${chip.textContent.trim()}${chip.disabled ? " (disabled)" : ""}` : "(no chip)",
        dot: dot ? dot.className : "(no dot)",
        state: (((revMods || {}).modules || [])[0] || {}).state || "(nothing loaded)",
      };
    });
    throw new Error(`the note is written and the panel does not show it — the chip reads ${JSON.stringify(now.chip)}, `
      + `the state dot is ${JSON.stringify(now.dot)}, and skein's own state already says the module is ${JSON.stringify(now.state)}`);
  }
});

// The other side of the rule, and the reason the paint is CONDITIONAL rather than a plain
// `renderReviewNow`. Both of these are answers to a press the reader made — and forcing them would
// take a caret out of something they are typing in for an answer that is not on the page at all,
// which is the harm §6 rule 2 exists to prevent.
await check("a note nobody is watching does not take the caret out of the queue", async () => {
  await withTheNotesPanelOpen();
  writeDelayMs = 1500;
  const before = notesWritten.length;
  await page.evaluate(p => writeModule(p), MODULE);
  await settle(150);
  if (notesWritten.length === before) throw new Error("the press sent nothing — there is no answer for this check to be about");
  const ready = await page.evaluate(() => {
    toggleMods();          // shut it again — the note is still being written
    const box = document.querySelector("#revpane .revsearch");
    box.focus();
    box.value = "half a thought";
    box.dispatchEvent(new Event("input"));
    // `revSearchSet` replaces the field it is typed into and puts the caret back itself, so the
    // element to ask about is the one on screen NOW.
    const now = document.querySelector("#revpane .revsearch");
    return { closed: !revModsOpen, focused: !!now && document.activeElement === now, held: revRenderHeld() };
  });
  if (!ready.closed) throw new Error("the notes panel did not shut — not the case under test");
  if (!ready.focused || !ready.held) throw new Error("the search box never took the caret — not the case under test");
  await settle(2500);   // the answer, and the reload behind it, land here
  const kept = await page.evaluate(() => {
    const box = document.querySelector("#revpane .revsearch");
    return {
      focused: !!box && document.activeElement === box,
      value: box ? box.value : "(no box)",
      panel: !!document.querySelector("#revpane .revmod"),
    };
  });
  if (!kept.focused) throw new Error(`the caret was taken out of the box the reader was typing in — it now reads ${JSON.stringify(kept.value)}`);
  if (kept.value !== "half a thought") throw new Error(`what the reader was typing was replaced: ${JSON.stringify(kept.value)}`);
  if (kept.panel) throw new Error("a panel the reader had shut was drawn again by the answer to it");
  await page.evaluate(() => revSearchSet(""));
});

// A second clause stood here: `revModsPaint` forced a render only when the panel was on screen AND
// the reading view was not, because the reading view rendered INSTEAD of the queue and a forced
// paint from it would take the caret out of a half-typed line comment for an answer that was not
// drawn at all. There is one surface now, so `revModsOpen` is the whole question and the check that
// posed the second half has nothing left to pose.

console.log("\nthe repo the notes panel is about");
// **The panel is about a repository, and a press on it WRITES** (SKEIN-427).
//
// `revMods` was loaded for `revScopeRepo()` and never cleared when that changed, so with the panel
// open you could switch the filter from one repo to another and the first repo's rows — its paths,
// its fresh/stale dots — stayed on screen under the second repo's queue. Pressing "re-write" on one
// posted its path to `/api/repos/<the-other-repo>/modules/write`: a note about one repository
// written against another, under the reader's own hand, with nothing on screen saying so. A path
// alone cannot be checked for this, on either side — `src` is a module of half the repos in a
// fleet, so the wrong repo accepts it and spends a minute of model time on it.
//
// The presses below are dispatched by calling `writeModule`, for the reason the checks above give
// and one more: what is under test is that the press sends NOTHING, and a real mouse press on a
// panel that has already been redrawn would find no chip and prove nothing either way.

await check("the panel follows the repo filter instead of keeping the last repo's modules", async () => {
  await withTheNotesPanelOpen();
  const was = await panelPaths();
  if (!was.includes(MODULE)) throw new Error(`the panel is not showing acme's modules: ${JSON.stringify(was)} — not the case under test`);
  await page.evaluate(() => openReview("bravo"));
  const followed = await page.waitForFunction(
    m => [...document.querySelectorAll("#revpane .revmod code")].some(e => e.textContent.trim() === m),
    BRAVO_MODULE, { timeout: 10000 }).then(() => true).catch(() => false);
  if (!followed) {
    const now = await panelPaths();
    const chip = await page.$eval("#revpane .revchip:has-text('notes')", e => e.textContent.trim()).catch(() => "(no chip)");
    throw new Error(`the queue is bravo's and the notes panel still reads ${JSON.stringify(now)}, `
      + `under a chip that says ${JSON.stringify(chip)}`);
  }
});

// **The one that is not a display bug.** The press is made in the same task as the change of
// filter, which is the reader whose hand is already on the chip when the repo moves under it — and
// the state the old code left standing for as long as nobody pressed anything.
await check("with the filter moved on, a press for the repo you left does not reach the network", async () => {
  await withTheNotesPanelOpen();
  const before = notesWritten.length;
  const seen = await page.evaluate(m => {
    const on = [...document.querySelectorAll("#revpane .revmod code")].map(e => e.textContent.trim());
    openReview("bravo");
    const scope = revScopeRepo();
    writeModule(m);
    return { on, scope, writing: revWriting };
  }, MODULE);
  if (!seen.on.includes(MODULE)) throw new Error(`acme's row was not on screen at the press: ${JSON.stringify(seen.on)} — not the case under test`);
  if (seen.scope !== "bravo") throw new Error(`the pane is scoped to ${JSON.stringify(seen.scope)} and not to the repo just chosen — not the case under test`);
  for (let i = 0; i < 15 && notesWritten.length === before; i++) await settle(100);
  if (notesWritten.length !== before) {
    const sent = notesWritten[notesWritten.length - 1];
    throw new Error(`the press was sent: ${JSON.stringify(sent.path)} — a module of acme — written `
      + `against ${JSON.stringify(sent.repo)}, which is not the repo it is about`);
  }
  if (seen.writing) throw new Error(`nothing was sent, and the panel says it is writing ${JSON.stringify(seen.writing)}`);
  // Not silently: a press that does nothing and says nothing is the same to a reader as a press
  // that worked.
  const said = await page.evaluate(() => (document.getElementById("toast") || {}).innerText || "");
  if (!said.includes("nothing was written")) throw new Error(`the press was refused without telling anybody — the page says ${JSON.stringify(said)}`);
});

// The same wrong list, arriving a second late instead of a second early: a request is in flight for
// one repo while the reader changes to another, and its answer is about a repo the pane is no
// longer showing.
await check("a module list that arrives after you have changed repo again is not shown as this repo's", async () => {
  await withTheNotesPanelOpen();
  slowRepo = "bravo";
  modsDelayMs = 1500;
  await page.evaluate(() => openReview("bravo"));
  await settle(200);
  slowRepo = "";
  modsDelayMs = 0;
  await page.evaluate(() => openReview("acme"));
  await settle(2000);   // bravo's answer lands in here, long after the pane went back to acme
  const paths = await panelPaths();
  if (paths.includes(BRAVO_MODULE)) {
    throw new Error(`bravo's module list is on screen under acme's queue: ${JSON.stringify(paths)}`);
  }
  if (!paths.includes(MODULE)) throw new Error(`the panel is not showing acme's modules either: ${JSON.stringify(paths)}`);
});

// The same state one step further on: with several repos and no filter there is no honest scope,
// so the chip that shuts the panel is not drawn. The panel used to be drawn anyway.
await check("with several repos and no repo chosen, the panel is not left on screen without its chip", async () => {
  await withTheNotesPanelOpen();
  const state = await page.evaluate(() => {
    openReview("");   // the picker's own "all repos", which is a filter of ""
    return { scope: revScopeRepo(), open: revModsOpen, repos: ((revQueue || {}).queues || []).map(q => q.repo_id) };
  });
  if (state.repos.length < 2) {
    throw new Error(`the queue holds only ${JSON.stringify(state.repos)} — with one repo the scope falls `
      + `back to it and there is no scopeless state to test`);
  }
  if (state.scope !== "") throw new Error(`the pane still has a scope (${JSON.stringify(state.scope)}) — not the case under test`);
  if (!state.open) throw new Error("the panel was not left open — not the case under test");
  await settle(400);
  const seen = await page.evaluate(() => ({
    panel: !!document.querySelector("#revpane .revmods"),
    chip: [...document.querySelectorAll("#revpane .revchip")].filter(e => /^notes/.test(e.textContent.trim())).length,
  }));
  if (seen.panel) {
    throw new Error(`the notes panel is on screen with no repo to be about, and ${seen.chip} chip(s) `
      + `anywhere on the page to shut it — the rows are ${JSON.stringify(await panelPaths())}`);
  }
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
