// Browser test for the review pane: a repo's PR queue, in a real browser, clicked the way you
// would click it.
//
// It has its own fixture rather than riding on smoke.mjs because the two need incompatible repos:
// smoke's is adopted from a local path (deliberately — that is the shape that broke `slug_from_url`)
// and a local path has no GitHub identity, so it has no PR queue by design. Bending that fixture to
// serve both would weaken the case it was built to prove.
//
// The rule inherited from smoke.mjs applies here too, and is the reason this file exists at all:
// **assert what is VISIBLE**. `#gitq` once shipped with complete markup, a poller, decision handlers
// and sixteen passing tests, and no CSS — a whole feature that could not be reached. Unit tests
// cannot see that. A browser can.
//
//   node tests/ui/review.mjs

import { chromium } from "playwright";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

// ---------- fixture ----------
// Four PRs, each one a state the pane has to get right:
//   #1 unreviewed              → needs you
//   #2 approved on the head    → waiting
//   #3 approved, then moved on → needs you (the case that returns work to you)
//   #4 authored by you         → the "mine" filter
function makeFixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "skein-review-ui-"));
  const bin = path.join(root, "bin");
  const home = path.join(root, "home");
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(root, "sandboxes.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({}));
  // A repo whose source IS a GitHub URL — the only kind that has a queue.
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    { id: "acme", source: "https://github.com/acme/thing.git", work: path.join(root, "work"),
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));

  const pr = (number, title, author, extra = "") =>
    `{"number":${number},"title":"${title}","author":{"login":"${author}"},` +
    `"url":"https://github.com/acme/thing/pull/${number}","headRefName":"feat-${number}",` +
    `"headRefOid":"sha${number}","baseRefName":"main","isDraft":false,` +
    `"updatedAt":"2026-08-0${number}T00:00:00Z"${extra}}`;
  const reviewed = (state, oid) =>
    `,"latestReviews":[{"author":{"login":"me"},"state":"${state}","commit":{"oid":"${oid}"}}]`;

  const requested = [
    pr(1, "fix a null deref in the parser", "dana", `,"statusCheckRollup":[{"status":"COMPLETED","conclusion":"SUCCESS"}]`),
    pr(2, "rename the retry flag", "dana", reviewed("APPROVED", "sha2")),
    pr(3, "change the default timeout", "erin", reviewed("APPROVED", "older")),
  ].join(",");
  const mine = pr(4, "my own change to the store layout", "me");

  fs.writeFileSync(path.join(root, "search-review-requested.json"), `[${requested}]`);
  fs.writeFileSync(path.join(root, "search-author.json"), `[${mine}]`);

  // A `gh` that answers from those files. `user/teams` fails on purpose: that is the common real
  // shape (a login without read:org) and it must surface as a stated blind spot, not silence.
  const gh = path.join(bin, "gh");
  fs.writeFileSync(gh, `#!/bin/sh
if [ "$1" = "api" ] && [ "$2" = "user" ]; then printf 'me\\n'; exit 0; fi
if [ "$1" = "api" ] && [ "$2" = "user/teams" ]; then exit 1; fi
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  term=""
  while [ $# -gt 0 ]; do
    if [ "$1" = "--search" ]; then term="$2"; fi
    shift
  done
  case "$term" in
    review-requested:*) cat "${root}/search-review-requested.json" ;;
    author:*)           cat "${root}/search-author.json" ;;
    *)                  printf '[]\\n' ;;
  esac
  exit 0
fi
exit 0
`);
  fs.chmodSync(gh, 0o755);

  // sbx stand-in: an empty fleet is fine — review does not depend on any box being alive, which is
  // itself part of what this file proves.
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, `#!/bin/sh\ncase "$1" in ls) echo '[]'; exit 0 ;; esac\nexit 0\n`);
  fs.chmodSync(sbx, 0o755);
  return { root, bin, home, gh, sbx };
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
      SKEIN_LS_CMD: `${fx.sbx} ls --json`,
      SKEIN_HOME: fx.home,
      SKEIN_GH_BIN: fx.gh,
      SKEIN_NO_GH_SECRET: "1",
      PATH: `${fx.bin}:${process.env.PATH}`,
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

// ---------- harness ----------
const results = [];
let page;
async function check(name, fn) {
  try { await fn(); results.push([true, name]); console.log(`  ok    ${name}`); }
  catch (e) { results.push([false, name]); console.log(`  FAIL  ${name}\n        ${String(e.message || e).split("\n")[0]}`); }
}
async function mustSee(sel, why) {
  const el = await page.$(sel);
  if (!el) throw new Error(`${why}: no element matches ${sel}`);
  const box = await el.boundingBox();
  if (!box || box.width === 0 || box.height === 0)
    throw new Error(`${why}: ${sel} is in the DOM but not visible (zero box) — a CSS rule is hiding it`);
  return el;
}
const settle = (ms = 500) => page.waitForTimeout(ms);
/** The visible rows of one lane, by title — the queue as a person reads it. */
const laneTitles = async (label) => page.evaluate(l => {
  const lane = [...document.querySelectorAll("#revpane .revlane")]
    .find(x => x.querySelector("h4")?.textContent.trim().startsWith(l));
  if (!lane) return null;
  return [...lane.querySelectorAll(".revtitle")].map(e => e.textContent.trim());
}, label);

// ---------- run ----------
const fx = makeFixture();
const port = await freePort();
const srv = await startServer(fx, port);
const browser = await chromium.launch();
page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
page.setDefaultTimeout(4000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("console", m => { if (m.type() === "error") noise.push(`[console] ${m.text()}`); });
page.on("response", r => { if (r.status() >= 500) noise.push(`[${r.status()}] ${r.url()}`); });

await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: "domcontentloaded" });
await settle(800);

console.log("\nopening");
await check("the header carries a way in", () => mustSee("#revbtn", "the review button"));
await check("clicking it opens a pane you can actually see", async () => {
  await page.click("#revbtn");
  await settle(900);
  await mustSee("#revpane.on .revwrap", "the review pane");
});
// The queue must dock without a box: this is the one view in the dock that is repo-scoped, and
// `applyView` gates rendering on `docked`.
await check("the dock opens even though no box is running", async () => {
  const docked = await page.evaluate(() => document.body.classList.contains("docked"));
  if (!docked) throw new Error("body is not .docked, so the pane has nowhere to render");
});

console.log("\nlanes");
await check("an unreviewed PR needs you", async () => {
  const titles = await laneTitles("needs you");
  if (!titles) throw new Error("there is no 'needs you' lane on screen");
  if (!titles.some(t => t.includes("null deref"))) throw new Error(`not in needs-you: ${JSON.stringify(titles)}`);
});
await check("a PR you approved on its current head is waiting, not asking again", async () => {
  const titles = await laneTitles("waiting");
  if (!titles?.some(t => t.includes("retry flag"))) throw new Error(`not in waiting: ${JSON.stringify(titles)}`);
});
// The case the whole head-SHA design exists for.
await check("commits landing after your approval bring it back to you", async () => {
  const titles = await laneTitles("needs you");
  if (!titles.some(t => t.includes("default timeout")))
    throw new Error(`an approval that new commits invalidated did not return: ${JSON.stringify(titles)}`);
});
// …and it must be distinguishable from a PR you have never seen, on the collapsed line. Otherwise
// the two rows look identical at exactly the moment the difference matters.
await check("and the row says why it came back, without being opened", async () => {
  const rows = await page.$$eval("#revpane .revrow", els => els.map(e => ({
    text: e.querySelector(".revtitle")?.textContent.trim() || "",
    moved: !!e.querySelector(".revtag.moved"),
  })));
  const back = rows.find(r => r.text.includes("default timeout"));
  const fresh = rows.find(r => r.text.includes("null deref"));
  if (!back?.moved) throw new Error("a re-review carries no mark on its collapsed row");
  if (fresh?.moved) throw new Error("a PR you never reviewed is marked as having moved");
});

console.log("\nhonesty");
await check("a queue that cannot see your teams says so, visibly", async () => {
  const el = await mustSee("#revpane .revblind", "the blind-spot banner");
  const t = (await el.textContent()).toLowerCase();
  if (!t.includes("team")) throw new Error(`the banner does not name what is missing: ${t}`);
});

console.log("\nfilter");
await check("'mine' shows what you opened and hides what you did not", async () => {
  await page.click("#revpane .revchip:has-text('mine')");
  await settle();
  const shown = await page.$$eval("#revpane .revtitle", els => els.map(e => e.textContent.trim()));
  if (!shown.some(t => t.includes("store layout"))) throw new Error("your own PR vanished");
  if (shown.some(t => t.includes("null deref"))) throw new Error("someone else's PR survived the filter");
});
await check("'all' brings everything back", async () => {
  await page.click("#revpane .revchip:has-text('all')");
  await settle();
  const shown = await page.$$eval("#revpane .revtitle", els => els.map(e => e.textContent.trim()));
  if (shown.length < 4) throw new Error(`expected all four PRs, saw ${shown.length}`);
});

console.log("\nexpanding");
await check("a row opens in place", async () => {
  await page.click("#revpane .revrow .revline");
  await settle();
  await mustSee("#revpane .revrow.open .revbody", "the expanded body");
});
// skein's standing rule: AI may only add scrutiny, never remove it. A PR with no summary must read
// as full attention, in words, rather than as an empty space that looks like nothing to do.
await check("an unsummarised PR says so rather than looking clear", async () => {
  const t = (await page.$eval("#revpane .revrow.open .revnosum", e => e.textContent)).toLowerCase();
  if (!t.includes("not summarised")) throw new Error(`the fallback does not state itself: ${t}`);
});

console.log("\nsetting aside");
await check("set aside moves a PR to the archived lane", async () => {
  const before = await laneTitles("needs you");
  await page.click("#revpane .revrow.open .revbody .revchip");
  await settle(900);
  const archived = await laneTitles("archived");
  if (!archived?.length) throw new Error("nothing reached the archived lane");
  const after = await laneTitles("needs you");
  if (after.length >= before.length) throw new Error("it was archived but never left needs-you");
});

console.log("\nquiet");
await check("no page errors and no 5xx along the way", () => {
  if (noise.length) throw new Error(noise.join(" | "));
});

// ---------- report ----------
// SKEIN_SHOT=<path> captures the pane whether or not anything failed. Assertions prove the pane
// works; only a picture shows whether it reads well, and the two are not the same review.
if (process.env.SKEIN_SHOT) {
  await page.click("#revpane .revchip:has-text('all')").catch(() => {});
  await settle();
  await page.screenshot({ path: process.env.SKEIN_SHOT });
  console.log(`\nscreenshot: ${process.env.SKEIN_SHOT}`);
}
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
if (!failed.length && !process.env.SKEIN_KEEP) fs.rmSync(fx.root, { recursive: true, force: true });
process.exit(failed.length ? 1 : 0);
