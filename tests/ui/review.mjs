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
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { serverBinary } from "./lift.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

// ---------- fixture ----------
// Five PRs, each one a state the pane has to get right (lanes say WHOSE MOVE it is, SKEIN-139):
//   #1 unreviewed              → your move
//   #2 approved on the head    → their move (you decided; it moves without you)
//   #3 approved, then moved on → your move (the case that returns work to you)
//   #4 authored by you         → their move, and the "mine" filter
//   #5 a draft                 → not ready (counted and stated, folded, never hidden)
/// A GitHub the size of what skein asks for: `/user`, `/user/teams`, one GraphQL search, a diff and
/// a file list. Answers from the fixture files written beside it.
///
/// It exists because the client changed, not because the test wanted rewriting: skein used to shell
/// out to `gh`, so the stub was a shell script on `$PATH`. It reads the API now, so the stub is an
/// API — and the test gained something in the move, because what it asserts is the request that
/// actually goes out.
async function createGitHub(root) {
  const search = q =>
    /review-requested:/.test(q) ? JSON.parse(fs.readFileSync(path.join(root, "search-review-requested.json"), "utf8"))
    : /author:/.test(q)         ? JSON.parse(fs.readFileSync(path.join(root, "search-author.json"), "utf8"))
    : [];
  const DIFFS = {
    3: "diff --git a/src/parser.rs b/src/parser.rs\n--- a/src/parser.rs\n+++ b/src/parser.rs\n@@\n-const TIMEOUT: u64 = 30;\n+const TIMEOUT: u64 = 5;\n",
    // Per PR, because the scanner reads the real diff: one diff for every number would put a moved
    // constant inside the "bug fix" too, and escalating that would be correct.
    other: "diff --git a/src/parser.rs b/src/parser.rs\n--- a/src/parser.rs\n+++ b/src/parser.rs\n@@\n-    let head = input.chars().next().unwrap();\n+    let Some(head) = input.chars().next() else { return Ok(()) };\n",
  };
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
        // One request carries every membership search of a refresh now, aliased q0…qN (SKEIN-209),
        // and each alias answers under its own name — a fixture that still answered the single
        // `search` field left every query reading as "GitHub returned no answer for this search".
        const vars = JSON.parse(body || "{}").variables || {};
        const data = {};
        for (const [name, value] of Object.entries(vars)) {
          if (/^q\d+$/.test(name)) data[name] = { nodes: search(String(value)) };
        }
        return send(200, { data });
      }
      // Acting on a PR: submitting a review, and merging. Both answer the way GitHub does — a JSON
      // object — because the client reads `message` out of it for what to show.
      const reviews = url.match(/^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)\/reviews$/);
      if (reviews) return send(200, { id: 1, state: "COMMENTED" });
      const merge = url.match(/^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)\/merge$/);
      if (merge) return send(200, { merged: true, message: "Pull Request successfully merged" });
      const files = url.match(/^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)\/files$/);
      if (files) return send(200, [{ filename: "src/parser.rs" }, { filename: "web/app.js" }]);
      const diff = url.match(/^\/repos\/[^/]+\/[^/]+\/pulls\/(\d+)$/);
      if (diff) return send(200, DIFFS[diff[1]] || DIFFS.other, "text/plain");
      send(404, { message: `no stub for ${url}` });
    });
  });
  // Awaited, because `listen` is asynchronous and `address()` is null until it has happened.
  return new Promise(resolve => {
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({ url: `http://127.0.0.1:${port}`, close: () => server.close() });
    });
  });
}

async function makeFixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "skein-review-ui-"));
  const bin = path.join(root, "bin");
  const home = path.join(root, "home");
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(root, "sandboxes.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  // A repo whose source IS a GitHub URL — the only kind that has a queue.
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    // `source_tree` as well as `source`, because the module list and CODEOWNERS are read from the
    // repo's MIRROR now (`repos::Tree` → `git show HEAD:<path>`), not from the working checkout —
    // `docs/delivery.md` §3 step 1. Without it `ensure_mirror` would try to clone the URL, over a
    // network this test does not have, and every module-note check would fail on a repo it could
    // not read. Pointing it at the local checkout is what an adopted repo actually looks like.
    // `source_tree`, and NOT `work` beside it: `work` is a serde ALIAS for the same field, so both
    // together is a duplicate key and the whole file fails to parse — which reads downstream as
    // "no repo with id acme" rather than as a bad fixture.
    // `read_prs: true` — read-ahead switched ON, which is what a person who wants their queue read
    // has pressed. It is off by default and it is the SCOPE, not a preference: with it off, skein
    // reads nothing here on its own, neither the background pass nor the pane's pump
    // (`review::unasked_scope`, SKEIN-242), and every check below that reads a summary the pump
    // fetched would be asserting on an empty pane.
    { id: "acme", source: "https://github.com/acme/thing.git",
      source_tree: path.join(root, "work"), read_prs: true,
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));

  // GraphQL's shape, because that is what skein reads now: connections rather than bare arrays, and
  // the check rollup hanging off the last commit. Same four states, one layer deeper.
  const pr = (number, title, author, extra = {}) => ({
    number, title, author: { login: author },
    url: `https://github.com/acme/thing/pull/${number}`,
    headRefName: `feat-${number}`, headRefOid: `sha${number}`, baseRefName: "main",
    isDraft: false, updatedAt: `2026-08-0${number}T00:00:00Z`,
    latestReviews: { nodes: [] },
    commits: { nodes: [{ commit: { statusCheckRollup: null } }] },
    ...extra,
  });
  const reviewed = (state, oid) =>
    ({ latestReviews: { nodes: [{ author: { login: "me" }, state, commit: { oid } }] } });
  const checks = nodes => ({ commits: { nodes: [{ commit: { statusCheckRollup: { contexts: { nodes } } } }] } });

  fs.writeFileSync(path.join(root, "search-review-requested.json"), JSON.stringify([
    pr(1, "fix a null deref in the parser", "dana", checks([{ status: "COMPLETED", conclusion: "SUCCESS" }])),
    pr(2, "rename the retry flag", "dana", reviewed("APPROVED", "sha2")),
    pr(3, "change the default timeout", "erin", reviewed("APPROVED", "older")),
    // A draft. Skein must leave it alone until somebody marks it ready or asks for it by hand — a
    // draft is the author saying it is not finished, and spending a model call on the fleet's own
    // rate limit to describe something nobody has proposed yet is the clearest case of work that
    // was not asked for.
    pr(5, "wip: still moving things around", "dana", { isDraft: true }),
  ]));
  fs.writeFileSync(path.join(root, "search-author.json"),
    JSON.stringify([pr(4, "my own change to the store layout", "me")]));

  // A working clone with a CODEOWNERS, so stage 0 (ownership) runs for real rather than being
  // skipped by an absent file — the path that decides how deep a summary goes, and the same file
  // the module list is derived from.
  fs.mkdirSync(path.join(root, "work", ".github"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", ".github", "CODEOWNERS"), "src/ @me\nweb/ @someone-else\n");
  // Real directories, so the module list is not empty: a CODEOWNERS pattern that names no directory
  // is deliberately not a module.
  fs.mkdirSync(path.join(root, "work", "src"), { recursive: true });
  fs.mkdirSync(path.join(root, "work", "web"), { recursive: true });
  fs.writeFileSync(path.join(root, "work", "src", "parser.rs"), "const TIMEOUT: u64 = 5;\n");
  fs.writeFileSync(path.join(root, "work", "web", "app.js"), "export const app = 1;\n");
  // **Committed, not just written.** The tree is read with `git show HEAD:<path>` off a mirror, so a
  // file that exists on disk and not in a commit is a file skein cannot see — which is exactly right
  // (a mirror can never supply a gitignored file) and exactly what this fixture used to get wrong.
  // Its own comment admitted "the fixture's clone is not a git repo" while asserting things only a
  // git repo can answer.
  const wgit = (...a) => spawnSync("git", ["-C", path.join(root, "work"), ...a], { stdio: "ignore" });
  wgit("init", "-q", "-b", "main");
  wgit("add", "-A");
  wgit("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "the tree the mirror carries");

  // A GitHub that answers from fixture files, on a real socket. This replaces a fake `gh` binary on
  // `$PATH`: skein reads the API directly now, so the seam that tells the truth is the wire.
  //
  // `user/teams` answers 403 on purpose — a login without `read:org` is the common real shape, and
  // it must surface as a stated blind spot rather than as silence.
  const github = await createGitHub(root);

  // sbx stand-in: an empty fleet is fine — review does not depend on any box being alive, which is
  // itself part of what this file proves.
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, `#!/bin/sh\ncase "$1" in ls) echo '[]'; exit 0 ;; esac\nexit 0\n`);
  fs.chmodSync(sbx, 0o755);

  // A `claude` that answers all THREE prompts skein sends, told apart the way the prompts differ:
  // the merged one (summary AND review in one call, since 2026-08-24) asks for a `REVIEW:` section,
  // stage 1 asks for the strict format without it, and stage 2 is the prose brief. A change to any
  // of those contracts shows up here as a summary that stops arriving.
  const claude = path.join(bin, "claude");
  fs.writeFileSync(claude, `#!/bin/sh
p="$4"
brief='## What it does\\n\\nShortens how long a request waits before giving up.\\n\\n## What changes in how it works\\n\\nCallers that relied on the old 30s ceiling now fail after 5s.\\n'
case "$p" in
  *"REVIEW:"*)
    case "$p" in
      *"default timeout"*)
        printf 'KIND: feature\\nLINE: the request timeout default drops from 30s to 5s.\\nEXPAND: yes\\nFLAGS: default, behaviour\\nDETAIL:\\n'
        printf "$brief"
        printf 'REVIEW:\\nOVERALL: nothing to flag\\n' ;;
      *)
        printf 'KIND: fix\\nLINE: stops the parser crashing on empty input.\\nEXPAND: no\\nFLAGS: none\\nDETAIL:\\nnone\\nREVIEW:\\nOVERALL: nothing to flag\\n' ;;
    esac ;;
  *"Answer in EXACTLY this format"*)
    case "$p" in
      *"default timeout"*)
        printf 'KIND: feature\\nLINE: the request timeout default drops from 30s to 5s.\\nEXPAND: yes\\nFLAGS: default, behaviour\\n' ;;
      *)
        printf 'KIND: fix\\nLINE: stops the parser crashing on empty input.\\nEXPAND: no\\nFLAGS: none\\n' ;;
    esac ;;
  *)
    printf "$brief" ;;
esac
exit 0
`);
  fs.chmodSync(claude, 0o755);
  return { root, bin, home, github, sbx, claude };
}

// The fleet's API token. Written by the fixture rather than read back after startup: the server
// mints one on first use, and a test that raced that would be flaky for a reason having nothing to
// do with what it is testing. The auth path itself is still exercised end to end — the browser gets
// its cookie from `?t=`, exactly as a person does, and every direct fetch carries the bearer.
const API_TOKEN = "t".repeat(64);
const apiToken = () => API_TOKEN;
const authHeader = () => ({ Authorization: `Bearer ${API_TOKEN}` });

const freePort = () => new Promise(res => {
  const s = createServer();
  s.listen(0, "127.0.0.1", () => { const { port } = s.address(); s.close(() => res(port)); });
});

async function startServer(fx, port) {
  // serverBinary() only builds when run by hand; under `cargo test` the binary arrives pre-built
  // via SKEIN_SERVER_BIN, because a nested cargo fighting the outer one for the build lock is the
  // load that made this suite flake (SKEIN-119 — the story is on serverBinary in lift.mjs).
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
      // Deliberately NO SKEIN_REVIEW_AI: reading PRs is on by default, and the whole summary half
      // of this suite passing without an override is the proof of it.
      SKEIN_CLAUDE_BIN: fx.claude,
      SKEIN_NO_GH_SECRET: "1",
      PATH: `${fx.bin}:${process.env.PATH}`,
    },
  });
  let log = "";
  srv.stdout.on("data", d => { log += d; });
  srv.stderr.on("data", d => { log += d; });
  for (let i = 0; i < 100; i++) {
    // The log goes back with the process: the server narrates its failures on stderr (`skein:
    // reading acme: …` when a mirror cannot be made), and a suite that swallows that sentence
    // makes every downstream check fail without its diagnosis.
    try { if ((await fetch(`http://127.0.0.1:${port}/api/boxes`, { headers: authHeader() })).ok) return { srv, log: () => log }; } catch {}
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
const fx = await makeFixture();
const port = await freePort();
const { srv, log } = await startServer(fx, port);
const browser = await chromium.launch();
page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
page.setDefaultTimeout(4000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("console", m => { if (m.type() === "error") noise.push(`[console] ${m.text()}`); });
page.on("response", r => { if (r.status() >= 500) noise.push(`[${r.status()}] ${r.url()}`); });

await page.goto(`http://127.0.0.1:${port}/?t=${apiToken()}`, { waitUntil: "domcontentloaded" });
await settle(800);

console.log("\nthe badge");
// The queue is only useful if you learn a PR is waiting without going looking. Polled slowly, so
// this waits for the first tick rather than assuming it has already happened.
await check("the count reaches the button without opening the pane", async () => {
  await page.waitForFunction(() => document.querySelector("#revbtn .revbadge"), null, { timeout: 15000 });
  const badge = await mustSee("#revbtn .revbadge", "the review badge");
  const n = (await badge.textContent()).trim();
  // Two: the unreviewed PR and the one your approval no longer covers. The draft is NOT here —
  // it is not your move, it is counted in the not-ready fold where nothing hides — and your own
  // PR is their move. This count is the whole point of SKEIN-139: the badge says what you can
  // act on now, not everything with your name near it.
  if (n !== "2") throw new Error(`expected the two your-move PRs, got ${JSON.stringify(n)}`);
});
await check("and its tooltip names the repo the count came from", async () => {
  const title = await page.$eval("#revbtn", e => e.title);
  if (!/acme: 2/.test(title)) throw new Error(`the breakdown is missing: ${title}`);
});
// Turning a repo off must stop skein asking GitHub about it — while still saying that is why there is
// nothing to show. It used to vanish from the counts entirely, which made "nothing needs you" and
// "skein never looked" the same empty badge; someone whose only repo had its queue off saw a clean
// board with no way to find out why.
await check("switching a repo's queue off silences it, and says so", async () => {
  const off = await fetch(`http://127.0.0.1:${port}/api/repos/acme/settings`, {
    method: "POST", headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify({ review_queue: false }),
  });
  if (!off.ok) throw new Error(`the setting was refused: ${await off.text()}`);
  const counts = await (await fetch(`http://127.0.0.1:${port}/api/review/counts`, { headers: authHeader() })).json();
  const acme = counts.find(c => c.repo_id === "acme");
  if (!acme) throw new Error(`the repo disappeared instead of reporting: ${JSON.stringify(counts)}`);
  if (!/switched off/.test(acme.skipped || "")) {
    throw new Error(`it must name why it was not looked at: ${JSON.stringify(acme)}`);
  }
  // Not polled, and not a fault: nothing was asked of GitHub, and a deliberate switch is not an error.
  if (acme.needs_you !== 0 || acme.error) throw new Error(`unexpected: ${JSON.stringify(acme)}`);
  // …and back on, because every check below this one needs the queue.
  await fetch(`http://127.0.0.1:${port}/api/repos/acme/settings`, {
    method: "POST", headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify({ review_queue: true }),
  });
});

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
await check("an unreviewed PR is your move", async () => {
  const titles = await laneTitles("your move");
  if (!titles) throw new Error("there is no 'your move' lane on screen");
  if (!titles.some(t => t.includes("null deref"))) throw new Error(`not in your-move: ${JSON.stringify(titles)}`);
});
await check("a PR you approved on its current head is their move, not asking again", async () => {
  const titles = await laneTitles("their move");
  if (!titles?.some(t => t.includes("retry flag"))) throw new Error(`not in their-move: ${JSON.stringify(titles)}`);
});
await check("and so is the one you authored — your problem as an author is not review work", async () => {
  const titles = await laneTitles("their move");
  if (!titles?.some(t => t.includes("store layout"))) throw new Error(`not in their-move: ${JSON.stringify(titles)}`);
});
// The case the whole head-SHA design exists for.
await check("commits landing after your approval bring it back to you", async () => {
  const titles = await laneTitles("your move");
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
  // A warning that cannot be acted on is shown for ever, so the cure travels in the sentence.
  if (!t.includes("gh auth refresh")) throw new Error(`it does not name the cure: ${t}`);
});
// SKEIN-164. The `read:org` gap is PERMANENT — true on every load until somebody runs that command
// — and it used to be drawn in exactly the treatment "the queue could not be built" uses. A
// constant in the alarm's clothes is what teaches the eye to skip the alarm, and the queue's
// willingness to shout is the best thing about it.
await check("a standing gap is amber and quiet; the alarm is kept for skein failing", async () => {
  const paint = await page.evaluate(() => {
    // The tokens themselves, resolved by the browser, so this compares what is drawn rather than
    // two spellings of the same hex.
    const tok = name => { const s = document.createElement("span"); s.style.color = `var(--${name})`;
      document.body.append(s); const c = getComputedStyle(s).color; s.remove(); return c; };
    const blind = getComputedStyle(document.querySelector("#revpane .revblind"));
    // The failure treatment is probed rather than provoked: this queue is healthy, and the point is
    // that the two are drawn differently, not that this run can produce a 401.
    const box = document.createElement("div");
    box.className = "revfail";
    document.getElementById("revpane").append(box);
    const fail = getComputedStyle(box);
    const out = {
      rule: blind.borderLeftColor, ruled: blind.borderLeftWidth, boxed: blind.borderTopWidth,
      failRule: fail.borderLeftColor, failBoxed: fail.borderTopWidth,
      waiting: tok("waiting"), error: tok("error"),
      alarms: document.querySelectorAll("#revpane .revfail").length - 1,
    };
    box.remove();
    return out;
  });
  if (paint.rule !== paint.waiting)
    throw new Error(`the standing gap is not amber: ${JSON.stringify(paint)}`);
  if (parseFloat(paint.boxed) !== 0 || parseFloat(paint.ruled) === 0)
    throw new Error(`the standing gap still wears a box rather than a rule: ${JSON.stringify(paint)}`);
  if (paint.failRule !== paint.error || parseFloat(paint.failBoxed) === 0)
    throw new Error(`the failure treatment lost its orange box: ${JSON.stringify(paint)}`);
  if (paint.alarms) throw new Error("a standing condition is drawn as a failure");
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

console.log("\nthe keyboard");
// SKEIN-151/159, docs/review-ux.md §6. Zero bindings before this, on a surface used thirty times
// a day — and with boxes present the fleet's keys were worse than dead: `j` moved a selection
// BEHIND the pane and `↵` navigated out of review entirely, which is data-loss-shaped with a
// composer open. What is asserted here is the browser half; the table's own routing and the
// focus arithmetic are `node tests/ui/reviewkeys.mjs` and `cockpit/test/keys.test.mjs`.

/** The rk of whatever the keyboard has selected, as the pane paints it. */
const selectedRk = () => page.$eval("#revpane .revrow.sel, #revpane .step.sel", e => e.dataset.rk)
  .catch(() => null);

await check("j selects a row, and the selection is visible", async () => {
  await page.keyboard.press("j");
  await settle(200);
  const rk = await selectedRk();
  if (!rk) throw new Error("j selected nothing — the pane has no visible selection");
  const lit = await page.$$eval("#revpane .revrow.sel", els => els.length);
  if (lit !== 1) throw new Error(`${lit} rows are lit at once; a selection is one row`);
});
await check("j and k walk it, and k at the top stays at the top", async () => {
  const first = await selectedRk();
  await page.keyboard.press("j");
  const second = await selectedRk();
  if (second === first) throw new Error(`j did not move: still ${second}`);
  await page.keyboard.press("k");
  if ((await selectedRk()) !== first) throw new Error("k did not come back to the row j left");
  await page.keyboard.press("k");
  if ((await selectedRk()) !== first) throw new Error("k walked off the top of the queue");
});
// SKEIN-151's done-when, and the reason this work is not cosmetic. Dispatched synchronously so no
// 2s fleet poll can land between the seed and the reading: what runs is the page's own global
// keydown listener, on a fleet that has a selection to lose.
await check("no keypress in the pane changes the fleet selection behind it", async () => {
  const said = await page.evaluate(() => {
    boxes = [{ name: "box-a", state: "idle" }, { name: "box-b", state: "idle" }];
    order = ["box-a", "box-b"];
    sel = "box-a";
    const before = sel, mode = view.mode;
    for (const k of ["j", "k", "Enter", "d", "]", "l", "}"]) {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }));
    }
    const said = { before, after: sel, mode, modeAfter: view.mode,
                   tabs: document.querySelectorAll("#tabs .tab").length, read: !!revReading };
    // ↵ opened the change HERE, which is the other half of the same rule; put the queue back so
    // the checks below are looking at the queue.
    if (revReading) closeReading();
    return said;
  });
  if (!said.read) throw new Error("↵ did not open the reading view for the selected row");
  if (said.after !== said.before)
    throw new Error(`the fleet selection moved behind the pane: ${said.before} → ${said.after}`);
  if (said.modeAfter !== said.mode)
    throw new Error(`a keypress navigated out of review: ${said.mode} → ${said.modeAfter}`);
  if (said.tabs) throw new Error(`↵ opened a terminal from inside the review pane (${said.tabs} tabs)`);
});
// §6's first focus rule, in a real browser: the selection is a PR number, so the summaries still
// landing under it move it nowhere. The measured failure was an index-based selection drifting off
// the row you are looking at every time one arrived.
await check("the selection survives the summaries landing under it", async () => {
  const before = await selectedRk();
  await page.waitForFunction(
    () => [...document.querySelectorAll("#revpane .gist")].some(e => /parser/.test(e.textContent)),
    null, { timeout: 20000 }).catch(() => {});
  await settle(600);
  const after = await selectedRk();
  if (after !== before) throw new Error(`a summary landing moved the selection ${before} → ${after}`);
});
// §6's one-line assertion, verbatim — the whole of SKEIN-159's done-when, in the browser it was
// measured in: focused:"rev-compose",caret:4 → focused:BODY,caret:0 on every re-render.
await check("a half-typed comment survives renderReview()", async () => {
  const rows = await page.$$("#revpane .revrow");
  for (const row of rows) {
    const t = await row.$eval(".revtitle", e => e.textContent).catch(() => "");
    if (t.includes("null deref")) { await row.click(); break; }
  }
  await settle(500);
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('ask')");
  await settle(300);
  await page.fill("#rev-compose", "half a thought");
  const said = await page.evaluate(() => {
    const el = document.activeElement;
    const before = { id: el.id, start: el.selectionStart };
    renderReview();                                   // a summary lands, the stale re-poll fires
    const now = document.activeElement;
    return { before, after: { id: now.id, start: now.selectionStart }, text: (now.value || "") };
  });
  if (said.after.id !== "rev-compose")
    throw new Error(`focus died on re-render: ${said.before.id} → ${said.after.id}`);
  if (said.after.start !== said.before.start)
    throw new Error(`the caret moved on re-render: ${said.before.start} → ${said.after.start}`);
  if (said.text !== "half a thought") throw new Error(`what was typed did not survive: ${said.text}`);
  await page.click("#revpane .revcompose .revchip:has-text('cancel')");
  await settle(300);
});
await check("and the row it was typed in is still the only one open", async () => {
  const open = await page.$$eval("#revpane .revrow.open", els => els.length);
  if (open !== 1) throw new Error(`${open} rows are open — expansion is exclusive (§2.5)`);
  await page.click("#revpane .revrow.open .revline");   // leave the queue collapsed for what follows
  await settle(300);
});
await check("m is unbound, and nothing happens when it is pressed", async () => {
  const before = await page.evaluate(() => document.querySelectorAll("#revpane .revreceipt").length);
  await page.keyboard.press("m");
  await settle(300);
  const after = await page.evaluate(() => ({
    receipts: document.querySelectorAll("#revpane .revreceipt").length,
    dialog: !!document.querySelector("dialog[open]"),
  }));
  if (after.receipts !== before) throw new Error("m started an act — merge must be chip-only");
  if (after.dialog) throw new Error("m opened the merge confirm — the key must not exist at all");
});
await check("a in the queue refuses out loud rather than approving", async () => {
  await page.keyboard.press("a");
  await settle(300);
  const said = await page.$eval("#toast", e => e.textContent).catch(() => "");
  if (!/open it first/i.test(said))
    throw new Error(`a said nothing about why it did not approve: ${JSON.stringify(said)}`);
  const receipts = await page.$$("#revpane .revreceipt");
  if (receipts.length) throw new Error("a approved from a surface that is not showing the change");
});
await check("/ puts the caret in the queue's own search", async () => {
  await page.keyboard.press("/");
  await settle(200);
  const cls = await page.evaluate(() => document.activeElement.className || "");
  if (!/revsearch/.test(cls)) throw new Error(`/ did not focus the search: ${JSON.stringify(cls)}`);
  await page.keyboard.press("Escape");
  await settle(200);
});
await check("e sets a row aside on the hold, and u takes it back", async () => {
  await page.keyboard.press("j");
  await settle(200);
  const aside = await selectedRk();
  await page.keyboard.press("e");
  await settle(400);
  const held = await page.evaluate(k => {
    const el = document.querySelector(`#revpane .revrow[data-rk="${k}"]`);
    return { greyed: !!el && el.classList.contains("held"), toast: document.getElementById("toast")?.textContent || "" };
  }, aside);
  if (!held.greyed) throw new Error("the row did not grey in place — e must not rebuild the pane");
  if (!/u undoes/.test(held.toast)) throw new Error(`the receipt does not name the key back: ${held.toast}`);
  await page.keyboard.press("u");
  await settle(400);
  const back = await page.evaluate(k =>
    !!document.querySelector(`#revpane .revrow[data-rk="${k}"]:not(.held)`), aside);
  if (!back) throw new Error("u did not take the held act back");
});
// The reading view's own map. `a` here is the same key the queue refuses, and that is the design:
// the verdict exists where the evidence is, and nowhere else.
await check("↵ opens the reading view, and j walks it by hunk", async () => {
  await page.keyboard.press("j");
  await settle(200);
  await page.keyboard.press("Enter");
  await page.waitForSelector("#revpane .readdiff .diff", { timeout: 20000 });
  await page.keyboard.press("j");
  await settle(200);
  const at = await page.$$eval("#revpane .ln.hunk.at", els => els.length);
  if (at !== 1) throw new Error(`j focused ${at} hunks — it must walk by hunk, not by line`);
});
await check("c opens a comment composer on the focused hunk's line", async () => {
  await page.keyboard.press("c");
  await settle(300);
  const composer = await page.$("#revpane .cmt.composer textarea");
  if (!composer) throw new Error("c opened no composer beside the line");
  const focused = await page.evaluate(() => document.activeElement.tagName);
  if (focused !== "TEXTAREA") throw new Error(`the composer opened unfocused (${focused})`);
  await page.keyboard.press("Escape");
  await settle(200);
  if (await page.$("#revpane .cmt.composer")) throw new Error("esc left the line composer open");
});
await check("a approves where the evidence is, on the same hold and undo", async () => {
  await page.keyboard.press("a");
  await settle(400);
  const bar = await page.$eval("#revpane .readbar", e => e.textContent).catch(() => "");
  if (!/approved/.test(bar)) throw new Error(`a did not hold an approval: ${JSON.stringify(bar)}`);
  if (!/undo/.test(bar)) throw new Error("an approval with no way back inside its window");
  await page.keyboard.press("u");
  await settle(400);
  const after = await page.$eval("#revpane .readbar", e => e.textContent).catch(() => "");
  if (/approved/.test(after)) throw new Error("u did not cancel the held approval");
});
await check("esc comes back to the queue with the selection intact", async () => {
  const was = await page.evaluate(() => revReading && revReading.number);
  await page.keyboard.press("Escape");
  await settle(400);
  if (await page.$("#revpane .readbar")) throw new Error("esc did not leave the reading view");
  const rk = await selectedRk();
  if (!rk || !rk.endsWith(`#${was}`)) throw new Error(`came back to ${rk}, not to #${was}`);
});
// The absences are only deliberate if they are stated. A key sheet missing `m` reads exactly like
// a key sheet that forgot it.
await check("the key sheet states the three deliberate absences", async () => {
  await page.evaluate(() => openSettings("keys"));
  await settle(500);
  const text = await page.$eval("#set-keys", e => e.textContent);
  if (!/unbound/.test(text) || !/base branch/.test(text))
    throw new Error("the sheet does not say that merge is unbound, or why");
  if (!/not showing you the change|open the row first|refused here/.test(text))
    throw new Error("the sheet does not say why a does nothing in the queue");
  if (!/g\s*h|GitHub/.test(text)) throw new Error("the sheet does not name g h as the way to GitHub");
  await page.evaluate(() => closeSettings());
  await settle(300);
});

// The reload path, found by the keyboard section above and fixed in `openReview`: `view.repo` is
// `"*"` while the queue shows every repo, `restoreSessions` hands that saved view back on the next
// snapshot, and read as a repo id it matches nothing — a header, an empty lane, and no rows.
await check("a restored view of every repo is every repo, not a repo named *", async () => {
  await page.evaluate(() => openReview("*"));
  await settle(600);
  const said = await page.evaluate(() => ({
    filter: revRepoFilter, stored: localStorage.getItem("skein.reviewRepo"),
    rows: document.querySelectorAll("#revpane .revrow").length,
  }));
  if (said.filter !== "") throw new Error(`the sentinel became a filter: ${JSON.stringify(said)}`);
  if (said.stored === "*") throw new Error("and it was written back to the store, poisoning reloads");
  if (!said.rows) throw new Error(`the restored queue is empty: ${JSON.stringify(said)}`);
});

console.log("\nsummaries");
// The gist is the product at thirty a day: the collapsed row has to say what the PR *is*, so that
// most of the queue never needs opening at all.
await check("a bug fix states itself on the collapsed row", async () => {
  await page.waitForFunction(
    () => [...document.querySelectorAll("#revpane .gist")].some(e => /parser/.test(e.textContent)),
    null, { timeout: 15000 });
  const gists = await page.$$eval("#revpane .gist", els => els.map(e => e.textContent.trim()));
  if (!gists.some(g => g.includes("crashing on empty input")))
    throw new Error(`no one-line summary on the row: ${JSON.stringify(gists)}`);
});
// SKEIN-216. Summary and review are ONE model call where the review is yours to give, and one
// payload carries both (`review::known`) — so the queue can say a review is waiting without a
// request of its own. Before this, the only way to find out was to open a row and press "review the
// code…", once per row, on a queue of thirty.
await check("a drafted review says so on the row, and opens beside the summary", async () => {
  // The drafts were written while the summaries above were being read. The payload that carries
  // them is fetched per queue load, so this asks for one rather than waiting for the poller.
  await page.evaluate(() => loadReview(true));
  await page.waitForFunction(() => (revQueue?.prs || []).some(p => revDraftedReview(p)),
    null, { timeout: 15000 });
  // The chip when it is signal, nothing when the queue has worked through and every row wears one —
  // §4's minority rule, decided at render time like `moved`. Both halves are asserted, so a chip
  // that stopped being drawn at all fails here as loudly as one that became wallpaper.
  const demoted = await page.evaluate(() => revCommonChips.has("ready"));
  const chips = await page.$$("#revpane .revrow .revtag.ready");
  if (demoted === !!chips.length)
    throw new Error(`the chip and the demotion disagree: demoted=${demoted}, ${chips.length} on screen`);
  if (!demoted) {
    const said = (await (await mustSee("#revpane .revrow .revtag.ready", "the drafted-review chip")).textContent()).trim();
    if (!/^review ready/.test(said)) throw new Error(`the chip does not say what is waiting: ${said}`);
  }
  // It is styled as its own thing rather than inheriting the plain chip, which is the failure
  // `overlays.mjs` exists for: markup, handlers and tests, and no CSS. Probed rather than asserted
  // off a row, because on this queue the rule above legitimately keeps it off the rows.
  const paint = await page.evaluate(() => {
    const at = document.getElementById("revpane");
    const mk = cls => { const s = document.createElement("span"); s.className = cls; s.textContent = "review ready"; at.append(s); return s; };
    const plain = mk("revtag"), ready = mk("revtag ready");
    const out = { plain: getComputedStyle(plain).color, ready: getComputedStyle(ready).color,
                  box: ready.getBoundingClientRect().width };
    plain.remove(); ready.remove();
    return out;
  });
  if (!paint.box) throw new Error("the drafted-review chip has no box — a CSS rule is hiding it");
  if (paint.plain === paint.ready)
    throw new Error(`the drafted-review chip has no rule of its own: ${JSON.stringify(paint)}`);
  // Scoped to the row's own key, never to a bare `.revrow`: an earlier check leaves rows in states
  // of its own, and this one is about a particular pull request.
  const key = await page.evaluate(() => rk((revQueue.prs || []).find(p => revDraftedReview(p))));
  await page.click(`#revpane .revrow[data-rk="${key}"] .revline`);
  await settle(400);
  const section = await mustSee(`#revpane .revrow[data-rk="${key}"].open .revdraft`,
    "the drafted review as a section of the open row");
  const text = (await section.textContent()).trim();
  if (!/nothing to flag/i.test(text))
    throw new Error(`the section does not carry what the review said: ${text}`);
  // The brief is above it: both in one go is the whole ask.
  const order = await page.evaluate(k => {
    const row = document.querySelector(`#revpane .revrow[data-rk="${k}"]`);
    const brief = row.querySelector(".revbrief, .revnosum");
    const draft = row.querySelector(".revdraft");
    return brief && draft ? brief.compareDocumentPosition(draft) & 4 : 0;   // FOLLOWING
  }, key);
  if (!order) throw new Error("the review is not a section beside the summary");
  await page.click(`#revpane .revrow[data-rk="${key}"] .revline`);
  await settle(200);
});
await check("a draft is not ready, and the fold states its own composition", async () => {
  // The draft is not hidden and not your move: it is a COUNT with its reason, one click open.
  const fold = await page.$("#revpane .revlane h4.revfold");
  if (!fold) throw new Error("there is no not-ready fold on screen");
  const said = await fold.textContent();
  if (!/1 draft/.test(said)) throw new Error(`the fold does not state its composition: ${said.trim()}`);
  await fold.click();
  await settle(300);
});
await check("a draft is not read unless you ask, and says so rather than looking failed", async () => {
  // Every non-draft in this lane has a gist by now (the check above waited for one). A draft that
  // was going to be read would have been read in the same pass.
  const rows = await page.$$eval("#revpane .revrow", els => els.map(e => ({
    title: e.querySelector(".revtitle")?.textContent.trim() || "",
    draft: !!e.querySelector(".revtag.draft"),
    gist: e.querySelector(".gist")?.textContent.trim() || "",
    unread: !!e.querySelector(".gist.unknown"),
  })));
  const wip = rows.find(r => r.title.includes("still moving things around"));
  if (!wip) throw new Error(`the draft is not in the lane at all: ${JSON.stringify(rows.map(r => r.title))}`);
  if (!wip.draft) throw new Error("the draft is not marked as one, so the rule cannot be seen either");
  // The gist is never empty now — an unrequested reading shows as the stated absence, not a line.
  if (!wip.unread || !wip.gist.startsWith("not read"))
    throw new Error(`a draft was read without being asked, or hides that it was not: ${wip.gist}`);

  // And opening it explains WHY rather than reading as a failure — three different reasons land in
  // that space and only one of them is a setting to change.
  await page.click(`#revpane .revrow:has-text("still moving things around") .revline`);
  await settle(300);
  const said = await page.$eval(`#revpane .revrow:has-text("still moving things around") .revnosum`,
    e => e.textContent.trim());
  if (!/draft/i.test(said) || !/ready/i.test(said))
    throw new Error(`a draft must say it is being left alone until it is ready, got: ${said}`);
  // The way to have one anyway is right there.
  const button = await page.$(`#revpane .revrow:has-text("still moving things around") .revnosum .revchip`);
  if (!button) throw new Error("no way to ask for it by hand");
  await page.click(`#revpane .revrow:has-text("still moving things around") .revline`);
  await settle(200);
});

console.log("\nthe row"); // SKEIN-156/157/158 — one height, whose-move, never silent
await check("every row states something in its gist — read, reading, or not read", async () => {
  const gists = await page.$$eval("#revpane .revrow .gist", els => els.map(e => e.textContent.trim()));
  if (!gists.length) throw new Error("no gist cells at all");
  const silent = gists.filter(g => !g);
  if (silent.length) throw new Error(`${silent.length} rows say nothing — silence reads as reassurance`);
});
await check("an unread row is visually a stated absence, not a short summary", async () => {
  const mark = await page.$eval("#revpane .gist.unknown", e => {
    const cs = getComputedStyle(e);
    return { deco: cs.textDecorationStyle, style: cs.fontStyle, text: e.textContent.trim() };
  });
  if (!mark.text.startsWith("not read")) throw new Error(`the absence does not say so: ${mark.text}`);
  if (mark.deco !== "dotted" || mark.style !== "italic")
    throw new Error(`the absence mark is not distinguishable at a glance: ${JSON.stringify(mark)}`);
});
await check("a summary landing moves no row", async () => {
  const before = await page.$$eval("#revpane .revrow", els =>
    els.map(e => ({ n: e.querySelector(".revnum")?.textContent || "", top: e.getBoundingClientRect().top })));
  await page.evaluate(() => {
    const pr = (revQueue.prs || []).find(p => !revSums.get(rk(p)));
    revSums.set(rk(pr || revQueue.prs[0]), { depth: "line",
      line: "a synthetic summary long enough to want a second line if anything would give it one",
      flags: [], yours: [], others: 0, head_sha: (pr || revQueue.prs[0]).head_sha });
    renderReview();
  });
  const after = await page.$$eval("#revpane .revrow", els =>
    els.map(e => ({ n: e.querySelector(".revnum")?.textContent || "", top: e.getBoundingClientRect().top })));
  for (const b of before) {
    const a = after.find(x => x.n === b.n && b.n);
    if (a && Math.abs(a.top - b.top) > 0)
      throw new Error(`row ${b.n} moved ${a.top - b.top}px when a summary landed`);
  }
});
await check("rows are one line high, so a day fits on a screen", async () => {
  const h = await page.$$eval("#revpane .revrow .revline", els =>
    Math.max(...els.map(e => e.getBoundingClientRect().height)));
  if (h > 30) throw new Error(`a row is ${h}px — at 29px, 28 rows fit above a 900px fold; at ${h}px they do not`);
});
await check("the left mark is whose move, and scarce — not the check dot", async () => {
  const dots = await page.$$("#revpane .revdot");
  if (dots.length) throw new Error("the check dot is still in the scan position");
  const yours = await page.$$("#revpane .mv.yours");
  const theirs = await page.$$("#revpane .mv.theirs, #revpane .mv.done");
  if (!yours.length) throw new Error("nothing on screen is marked as your move");
  if (!theirs.length) throw new Error("every row is lit — a mark true of every row is a texture");
});
await check("no chip is true of more than a third of the queue", async () => {
  const rows = await page.$$eval("#revpane .revrow", els => els.map(e =>
    [...e.querySelectorAll(".revtag")].map(t => t.textContent.trim().split(" ")[0])));
  const counts = {};
  for (const tags of rows) for (const t of new Set(tags)) counts[t] = (counts[t] || 0) + 1;
  const mass = Object.entries(counts).filter(([, n]) => n > rows.length / 3);
  if (mass.length) throw new Error(`chips worn by most of the queue: ${JSON.stringify(mass)} of ${rows.length} rows`);
});

// The tripwire marks belong on the collapsed line, because they are the reason to stop scrolling.
await check("a contract change is flagged where you can see it without opening", async () => {
  const rows = await page.$$eval("#revpane .revrow", els => els.map(e => ({
    text: e.querySelector(".revtitle")?.textContent.trim() || "",
    flags: [...e.querySelectorAll(".revtag.flag")].map(f => f.textContent.trim()),
  })));
  const timeout = rows.find(r => r.text.includes("default timeout"));
  const fix = rows.find(r => r.text.includes("null deref"));
  if (!timeout?.flags.includes("default")) throw new Error(`a changed default is not flagged: ${JSON.stringify(timeout)}`);
  if (fix?.flags.length) throw new Error(`a bug fix was flagged as a contract change: ${JSON.stringify(fix)}`);
});

console.log("\nexpanding");
// SKEIN-287. The queue payload carries the ROW shape now — the line, the flags, the depth, and
// whether a review is drafted — and the prose arrives when a row is opened, one row at a time.
// Measured on the owner's fleet: 153,381 bytes for thirty-nine readings, and that response holds one
// of the browser's per-origin connections for as long as it takes.
//
// Driven through the rendered row, and the request log is the assertion — the shape of the payload
// is invisible from the DOM, and what matters is which requests the page actually makes.
await check("the queue asks for rows, and a row asks for its own prose when it opens", async () => {
  const asked = [];
  const spy = req => {
    const u = new URL(req.url());
    if (/\/review\/(summaries|\d+\/summary)$/.test(u.pathname)) asked.push(u.pathname + u.search);
  };
  page.on("request", spy);
  try {
    await page.evaluate(() => { revSums = new Map(); openReview(""); loadReview(true); });
    // Until a THIN reading is on the page: that is the bulk payload's row shape having landed, and
    // it is what the rest of this check is about. Waiting for "any reading" would let the pump's own
    // answer — a full one, fetched for a row nothing had read — stand in for it.
    await page.waitForFunction(
      () => (revQueue?.prs || []).length > 0 && [...revSums.values()].some(s => s && s !== "…" && s.thin),
      null, { timeout: 20000 });
    await settle(600);
    const bulk = asked.filter(u => u.includes("/summaries"));
    if (!bulk.length) throw new Error("the queue never asked for its readings at all");
    if (!bulk.every(u => u.includes("rows=1")))
      throw new Error(`the queue asked for the whole prose to draw a list: ${bulk.join(", ")}`);
    // A collapsed row draws its line from the row shape and fetches no PROSE. The pump's own reads
    // go to the same route and are not this — they are `Trigger::Unasked` analyses of rows nothing
    // has read yet, and they carry no `held` marker. Prose is the request with `held=1`.
    const perRow = () => asked.filter(u => /\/\d+\/summary/.test(u) && u.includes("held=1"));
    if (perRow().length) throw new Error(`a collapsed queue fetched prose per row: ${perRow().join(", ")}`);

    // Opening a row fetches its prose — and it is a request to REMEMBER, never to analyse.
    const key = await page.evaluate(() => {
      const pr = (revQueue.prs || []).find(p => {
        const s = revSums.get(p.repo_id + "#" + p.number);
        return s && s !== "…" && s.thin && s.depth !== "unread";
      });
      if (!pr) return null;
      const k = pr.repo_id + "#" + pr.number;
      if (!revOpen.has(k)) toggleRevRow(k);
      return k;
    });
    if (!key) throw new Error("no thinned row to open, so this check would prove nothing");
    await settle(800);
    const mine = perRow();
    if (!mine.length) throw new Error("opening a row did not fetch the prose the list left behind");
    // Belt and braces on the marker the filter above already used: an opened row must never be
    // able to reach a model call, on any head, at any hour of the budget.
    if (!mine.every(u => u.includes("held=1")))
      throw new Error(`an opened row could have spent a model call: ${mine.join(", ")}`);
    // And it landed: the brief the row shape cannot carry is on screen.
    const body = (await page.textContent("#revpane .revrow.open .revbody")).replace(/\s+/g, " ");
    if (/fetching the brief/.test(body))
      throw new Error(`the prose never arrived: ${body.slice(0, 200)}`);

    // Asked once. A row that re-fetches on every render is the bulk payload's cost back in pieces.
    const before = mine.length;
    await page.evaluate(() => renderReviewNow());
    await settle(400);
    if (perRow().length !== before)
      throw new Error(`the row asked again on a repaint: ${perRow().join(", ")}`);
  } finally {
    page.off("request", spy);
  }
});
await check("a flagged PR opens to a brief, not to a diff", async () => {
  const rows = await page.$$("#revpane .revrow");
  for (const row of rows) {
    const t = await row.$eval(".revtitle", e => e.textContent).catch(() => "");
    if (t.includes("default timeout")) { await row.click(); break; }
  }
  await settle(800);
  const brief = (await page.$eval("#revpane .revrow.open .revbrief", e => e.textContent)).toLowerCase();
  if (!brief.includes("what changes in how it works"))
    throw new Error(`the brief is missing the section that matters: ${brief.slice(0, 120)}`);
});
// The scanner runs with no model at all and can only escalate. The stub `gh` serves a diff whose
// only change is a moved constant, so a signal must appear — and it must be visually separate from
// the model's prose, because "the diff says so" is a stronger claim than "a model thinks so".
await check("mechanical evidence is shown, and shown apart from the prose", async () => {
  const sig = await page.$eval("#revpane .revrow.open .revsignals", e => e.textContent).catch(() => "");
  if (!/found in the diff/i.test(sig)) throw new Error("the evidence block is missing");
  if (!/TIMEOUT/.test(sig)) throw new Error(`the moved constant was not found: ${sig}`);
  if (!/default/.test(sig)) throw new Error(`it was not classified as a default: ${sig}`);
});
// Stage 0 runs without any model, and it is what scopes the rest. If CODEOWNERS said `src/ @me`
// and the PR touched src/ and web/, the pane must say which half is yours.
await check("the brief says which of it you own", async () => {
  const owned = await page.$eval("#revpane .revrow.open .revowned", e => e.textContent).catch(() => "");
  if (!owned.includes("src/parser.rs")) throw new Error(`ownership was not applied: ${owned}`);
  if (!owned.includes("do not own")) throw new Error(`what it left out is not stated: ${owned}`);
});

console.log("\nreading");
// SKEIN-148/161: the pane used to contain no code, and approve was the first, highlighted button
// next to "Not read yet". The change is readable here now, and a verdict exists only beside it.
await check("an expanded row offers no verdict", async () => {
  const acts = await page.$$eval("#revpane .revrow.open .revacts .revchip", els => els.map(e => e.textContent.trim()));
  if (!acts.length) throw new Error("no acts on the open row at all");
  const verdicts = acts.filter(a => /^approve$|^request changes/.test(a));
  if (verdicts.length) throw new Error(`the queue still offers a verdict next to nothing: ${JSON.stringify(acts)}`);
});
// SKEIN-273 put ONE verdict on an expanded row — approve with skein's own review — and the check
// above goes on passing only because that chip's text is not exactly "approve", which is luck
// rather than a rule. So state the rule it must obey: the row's own strip still offers no verdict
// at all, and any approve that exists sits inside the block that shows the reading it would post.
await check("the one verdict an expanded row may hold is inside skein's reading of it", async () => {
  const own = await page.$$eval("#revpane .revrow.open .revrowacts .revchip",
    els => els.map(e => e.textContent.trim()));
  if (own.some(a => /approve|request changes|merge/i.test(a)))
    throw new Error(`the row's own control strip offers a verdict: ${JSON.stringify(own)}`);
  const stray = await page.$$eval("#revpane .revrow.open .revchip", els => els
    .filter(e => /approve/i.test(e.textContent))
    .filter(e => !e.closest(".revdraft, .revcrit"))
    .map(e => e.textContent.trim()));
  if (stray.length)
    throw new Error(`an approve sits outside the block showing the change it posts: ${JSON.stringify(stray)}`);
});
await check("the change itself is readable in the pane", async () => {
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('read the change')");
  await page.waitForSelector("#revpane .readdiff .diff", { timeout: 15000 });
  const text = await page.$eval("#revpane .readdiff", e => e.textContent);
  if (!text.includes("TIMEOUT: u64 = 5"))
    throw new Error("the diff GitHub serves is not what the pane shows");
  const files = await page.$$eval("#revpane .readfiles .readfile", els => els.map(e => e.title));
  if (!files.includes("src/parser.rs")) throw new Error(`the changed file is not listed: ${files}`);
});
await check("the verdict is only reachable from where the evidence is", async () => {
  const bar = await page.$$eval("#revpane .readbar .revchip", els => els.map(e => e.textContent.trim()));
  if (!bar.includes("approve")) throw new Error(`no approve beside the evidence: ${JSON.stringify(bar)}`);
});
await check("esc returns to the queue as it was", async () => {
  await page.keyboard.press("Escape");
  await settle(300);
  const reading = await page.$("#revpane .readbar");
  if (reading) throw new Error("esc did not leave the reading view");
  const open = await page.$("#revpane .revrow.open");
  if (!open) throw new Error("the row that was open before reading is not open after");
});

console.log("\nacts");
// Private by construction: an answer that might be published is a different, more careful, less
// useful answer — so asking must never look like a step on the way to posting.
await check("asking a question keeps the answer off GitHub", async () => {
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('ask')");
  await settle();
  // Scoped to the composer: the drafted review that now arrives with the summary (one model call
  // since 2026-08-24) puts its own `.revcl` on screen above this one.
  const label = await page.$eval("#revpane .revcompose .revcl", e => e.textContent);
  if (!/stays between you and skein/i.test(label))
    throw new Error(`the composer does not promise privacy: ${label}`);
  await page.fill("#rev-compose", "why 5 seconds?");
  await page.click("#revpane .revcompose .revchip:has-text('ask')");
  await page.waitForSelector("#revpane .revanswer", { timeout: 15000 });
  await mustSee("#revpane .revanswer", "the private answer");
  // …and it must not have offered to publish it.
  const posts = await page.$$("#revpane .revcompose .revchip:has-text('post to GitHub')");
  if (posts.length) throw new Error("an ask offered to post its answer");
});
await check("a comment is drafted into the box you edit, not sent", async () => {
  await page.click("#revpane .revcompose .revchip:has-text('cancel')");
  await settle();
  // Verdicts live in the reading view now (SKEIN-161): enter it, and compose beside the evidence.
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('read the change')");
  await page.waitForSelector("#revpane .readbar .revchip", { timeout: 15000 });
  await page.click("#revpane .readbar .revchip:has-text('comment')");
  await settle();
  await page.fill("#rev-compose", "ask them what happens to slow callers");
  await page.click("#revpane .revcompose .revchip:has-text('draft with skein')");
  await page.waitForFunction(
    () => /shortens/i.test(document.getElementById("rev-compose")?.value || ""),
    null, { timeout: 15000 });
  const posted = await page.$("#revpane .revcompose .revchip:has-text('post to GitHub')");
  if (!posted) throw new Error("a draft with no way to send it");
  await mustSee("#revpane .revcompose .revchip:has-text('post to GitHub')", "the post button");
});
await check("posting is a separate press from drafting", async () => {
  await page.click("#revpane .revcompose .revchip:has-text('post to GitHub')");
  await settle(900);
  const open = await page.$("#revpane .revcompose");
  if (open) throw new Error("the composer stayed open, so it is unclear whether it sent");
});

console.log("\nstanding notes");
// Back to the queue: a posted verdict leaves its receipt in the reading view and stays there
// (SKEIN-162), and the notes chip lives in the queue's header.
await page.keyboard.press("Escape");
await settle();
await check("the pane says how much of the repo it has notes on", async () => {
  const chip = await mustSee("#revpane .revchip:has-text('notes')", "the notes chip");
  if (!/notes/.test(await chip.textContent())) throw new Error("no notes chip");
});
await check("opening it lists the repo's modules, and admits it has none written", async () => {
  await page.click("#revpane .revchip:has-text('notes')");
  await page.waitForSelector("#revpane .revmods .revmod", { timeout: 8000 });
  const mods = await page.$$eval("#revpane .revmod", els => els.map(e => ({
    path: e.querySelector("code")?.textContent.trim(),
    state: [...e.querySelector(".revmodstate").classList].filter(c => c !== "revmodstate")[0] || "absent",
    action: e.querySelector("button")?.textContent.trim(),
  })));
  if (!mods.some(m => m.path === "src")) throw new Error(`modules not found: ${JSON.stringify(mods)}`);
  if (!mods.every(m => m.state === "absent")) throw new Error(`a note appeared from nowhere: ${JSON.stringify(mods)}`);
  if (!mods.every(m => m.action === "write")) throw new Error("no way to write one");
});
// The whole design rests on this: a note carries the commit its module was at, so out-of-date is a
// fact rather than a worry.
//
// Both directions, through the API a person's clicks go through. This check used to assert only the
// stale half, and for the wrong reason: its own comment said "the fixture's clone is not a git repo,
// so freshness cannot be established". That made it pass on an accident. The fixture commits its
// tree now, so freshness is a real question here and the answer to it is worth having.
// `.modules`, because the payload distinguishes a repo with no modules from a repo skein could not
// read (SKEIN-117) — a bare array could only say the first.
const modulesNow = async () =>
  (await (await fetch(`http://127.0.0.1:${port}/api/repos/acme/modules`, { headers: authHeader() })).json()).modules;
await check("a note written against the current commit reads fresh", async () => {
  const wrote = await fetch(`http://127.0.0.1:${port}/api/repos/acme/modules/write`, {
    method: "POST", headers: { "content-type": "application/json", ...authHeader() },
    body: JSON.stringify({ path: "src" }),
  }).then(r => r.json());
  if (!wrote.ok) throw new Error(`writing was refused: ${wrote.error}`);
  const src = (await modulesNow()).find(m => m.path === "src");
  if (src.state !== "fresh")
    throw new Error(`a note written just now against an unmoved module reads "${src.state}"`);
});
await check("and goes stale the moment its module moves", async () => {
  // The module moves, and skein sees it move. Dropping the mirror is how this fixture stands in for
  // the fetch a pull does — `ensure_mirror` re-clones what is not there, which is the same path a
  // half-made mirror takes. Without it the commit exists in the checkout and not in what skein
  // reads, and the note would go on being right.
  const work = path.join(fx.root, "work");
  fs.appendFileSync(path.join(work, "src", "parser.rs"), "const RETRIES: u8 = 3;\n");
  const wgit = (...a) => spawnSync("git", ["-C", work, ...a], { stdio: "ignore" });
  wgit("add", "-A");
  wgit("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "src moves on");
  fs.rmSync(path.join(fx.home, "repos", "acme", "mirror"), { recursive: true, force: true });

  const src = (await modulesNow()).find(m => m.path === "src");
  // Never `fresh`. A note whose freshness cannot be established is exactly a note not to trust, so
  // every uncertainty here has to fall the same way as a known move.
  if (src.state === "fresh")
    throw new Error("a note stamped with a commit that is no longer the module's read as current");
});

console.log("\nsetting aside");
// SKEIN-162 (§7.1): the press is a receipt in place with an undo window, not an immediate act —
// nothing posts inside the window, the row does not vanish under the reader, and only the next
// natural load moves it to the archived lane.
await check("set aside is a receipt in place — undo cancels, the lapse archives, the next load moves it", async () => {
  const rows = await page.$$("#revpane .revrow");
  for (const row of rows) {
    const t = await row.$eval(".revtitle", e => e.textContent).catch(() => "");
    // Not the row the verdict above went to: an act's receipt stays on the strip in place of the
    // controls (SKEIN-162), so a row just commented on has no `set aside` to press.
    if (t.includes("null deref") && !(await row.$(".revbody"))) { await row.click(); break; }
  }
  await settle();
  const before = await laneTitles("your move");
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('set aside')");
  await settle(300);
  // The ROW's own strip, not the drafted review's beside it: an expanded row grew sections with
  // their own `.revacts`, and a bare selector reads whichever the document reaches first.
  const strip = await page.$eval("#revpane .revrow.open .revrowacts", e => e.textContent || "");
  if (!/set aside/.test(strip) || !/undo/.test(strip))
    throw new Error(`the control did not become the receipt: "${strip}"`);
  const held = await laneTitles("your move");
  if (held.length !== before.length) throw new Error("the row vanished inside the undo window");
  // undo: the request never left the machine, and the strip returns.
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('undo')");
  await settle(300);
  const restored = await page.$eval("#revpane .revrow.open .revrowacts", e => e.textContent || "");
  if (!/set aside/.test(restored) || /undo/.test(restored)) throw new Error("undo did not restore the strip");
  // Pressed for real: the window lapses, the archive posts, and the row greys IN PLACE.
  await page.click("#revpane .revrow.open .revacts .revchip:has-text('set aside')");
  // The OPEN row: an earlier verdict in this file left its own row marked done, and a bare
  // `.revrow.done` matches that one instantly — the wait would pass before this act had posted.
  await page.waitForSelector("#revpane .revrow.open.done", { timeout: 15000 });
  const after = await laneTitles("your move");
  if (after.length !== before.length) throw new Error("the done row left the lane before the next load");
  // It leaves on the next natural load, by which time you are elsewhere.
  await page.click("#revpane .revhead .revchip:has-text('refresh')");
  await settle(900);
  const archived = await laneTitles("archived");
  if (!archived?.length) throw new Error("nothing reached the archived lane after the next load");
});

console.log("\nworkflows");
// Assign, see what it would do, and leave one out. The chain the owner has to be able to walk
// before trusting something that merges on its own — driven through the real page and the real
// routes, because "the endpoint returns the right JSON" is not the claim being made.
await check("a pull request can be told which workflow governs it, and what it would do", async () => {
  // A workflow that claims nothing on its own: what governs this PR must be a CHOICE somebody
  // made, or the test proves matching rather than assignment.
  fs.writeFileSync(path.join(fx.home, "workflows.json"), JSON.stringify({
    workflow: [{
      name: "ship-it",
      steps: [
        { when: ["checks:failing"], do: "flag:CI is red" },
        { when: ["no-label:ci"], do: "add-label:ci" },
      ],
    }],
  }));
  // Reopen the pane so it asks again — the workflow file is read per request, and nothing tells a
  // page already on screen that a file changed underneath it.
  await page.click("#revbtn");
  await settle(900);
  const rows = await page.$$("#revpane .revrow");
  for (const row of rows) {
    const t = await row.$eval(".revtitle", e => e.textContent).catch(() => "");
    if (t.includes("default timeout") && !(await row.$(".revbody"))) { await row.click(); break; }
  }
  await settle();
  const box = await page.$("#revpane .revrow.open .revflow");
  if (!box) throw new Error("an expanded row says nothing about what governs it");
  const before = await page.$eval("#revpane .revrow.open .revflow", e => e.textContent);
  if (!before.includes("Nothing governs")) {
    throw new Error(`a pull request nobody assigned anything to already carries something: ${before}`);
  }

  await page.selectOption("#revpane .revrow.open .revflow select", "ship-it");
  await settle(900);
  const after = await page.$eval("#revpane .revrow.open .revflow", e => e.textContent);
  // The dry run: the step it WOULD take, from the same evaluator the tick uses.
  if (!after.includes("Next:") || !after.includes("add-label:ci")) {
    throw new Error(`assigning a workflow did not say what it would do: ${after}`);
  }
  // And the honest state of a fleet that has not switched this on: a plan, not a prediction.
  if (!after.includes("switched off")) {
    throw new Error(`the pane promised action on a fleet with workflows off: ${after}`);
  }
  // The collapsed line carries it too, or a queue of thirty says nothing at a glance.
  const chip = await page.$eval("#revpane .revrow.open .revtag.flow", e => e.textContent).catch(() => "");
  if (!chip.includes("ship-it")) {
    throw new Error(`the row does not show what governs it: ${chip}`);
  }
});

await check("and one pull request can be left out of it entirely", async () => {
  await page.selectOption("#revpane .revrow.open .revflow select", "");
  await settle(900);
  const said = await page.$eval("#revpane .revrow.open .revflow", e => e.textContent);
  if (!said.includes("you left it out")) {
    throw new Error(`excluding a pull request did not stick: ${said}`);
  }
  const chip = await page.$("#revpane .revrow.open .revtag.flow");
  if (chip) throw new Error("a pull request that was left out still shows a workflow on its row");
});

console.log("\nauthoring");
await check("a workflow can be written in the cockpit, and it governs a pull request", async () => {
  // Start from nothing: this is somebody opening the editor on a fleet that has never had one.
  fs.rmSync(path.join(fx.home, "workflows.json"), { force: true });
  await page.click("#revbtn");
  await settle(900);
  await page.click("#revpane .revchip:has-text('workflows')");
  await settle();
  await page.click("#revpane .revchip:has-text('+ workflow')");
  await settle();

  // Name it, and give it a step: when checks are failing, say so and stop. Deliberately a step
  // whose action is a `flag` — the first workflow anybody writes should not be one that merges.
  await page.fill("#revpane .revflow-edit-head input", "watch-ci");
  await page.click("#revpane .revflow-edit .revchip:has-text('+ step')");
  await settle();
  // **The picker offers exactly what the parser knows.** Taken from the server rather than written
  // into the page: a dropdown with its own idea of the vocabulary is a workflow somebody builds here
  // and cannot save, discovered by a person at the moment they were trusting the tool.
  const vocab = await (await fetch(`http://127.0.0.1:${port}/api/workflows`, { headers: authHeader() })).json();
  // Each line against its own half of the vocabulary: the `when` line offers conditions, the `do`
  // line offers actions, and neither may offer the other's words.
  const bothWays = (offered, words, what) => {
    const known = new Set(words.map(w => (w.arg ? w.kind : w.spelling)));
    for (const value of offered) {
      if (!known.has(value)) throw new Error(`the ${what} picker offers "${value}", which skein cannot parse`);
    }
    for (const want of known) {
      if (!offered.includes(want)) throw new Error(`skein knows "${want}" and the ${what} picker hides it`);
    }
  };
  const optionsIn = sel => page.$$eval(sel, els => els.map(e => e.value));
  bothWays(
    await optionsIn("#revpane .revstep .revstep-line:nth-child(1) select:first-of-type option"),
    vocab.conditions || [],
    "condition",
  );
  bothWays(
    await optionsIn("#revpane .revstep .revstep-line:nth-child(2) select:first-of-type option"),
    vocab.actions || [],
    "action",
  );

  // The step's condition picker offers the vocabulary; choose `checks`, then its value.
  const when = await page.$$("#revpane .revstep .revstep-line select");
  await when[0].selectOption("checks");
  await settle();
  const value = await page.$$("#revpane .revstep .revstep-line select");
  await value[1].selectOption("failing");
  await settle();
  // And the action.
  const acts = await page.$$("#revpane .revstep .revstep-line select");
  await acts[acts.length - 1].selectOption("flag");
  await settle();
  await page.fill("#revpane .revstep .revstep-line input", "CI is red");
  await settle();
  await page.click("#revpane .revchip:has-text('save')");
  await settle(1200);

  // It is on disk, in the shape skein reads back — not the shape the page sent.
  const written = JSON.parse(fs.readFileSync(path.join(fx.home, "workflows.json"), "utf8"));
  const flow = (written.workflow || [])[0];
  if (!flow || flow.name !== "watch-ci") throw new Error(`nothing was saved: ${JSON.stringify(written)}`);
  if (JSON.stringify(flow.steps) !== JSON.stringify([{ when: ["checks:failing"], do: "flag:CI is red" }])) {
    throw new Error(`the step was not written as it was built: ${JSON.stringify(flow.steps)}`);
  }

  // And the workflow it just wrote can be put on a pull request — the two halves of this feature
  // meeting, which is the only thing that proves the editor produces something usable.
  const rows = await page.$$("#revpane .revrow");
  for (const row of rows) {
    const t = await row.$eval(".revtitle", e => e.textContent).catch(() => "");
    if (t.includes("default timeout") && !(await row.$(".revbody"))) { await row.click(); break; }
  }
  await settle();
  const options = await page.$$eval("#revpane .revrow.open .revflow select option", els => els.map(e => e.value));
  if (!options.includes("watch-ci")) {
    throw new Error(`a workflow written here cannot be chosen there: ${options.join(", ")}`);
  }
});

await check("a workflow it could not read is refused with the step that is wrong", async () => {
  // What the server says when the file will not parse — the message that names the workflow, the
  // step and the vocabulary, rather than "invalid".
  const res = await fetch(`http://127.0.0.1:${port}/api/workflows`, {
    method: "PUT",
    headers: { "Content-Type": "application/json", ...authHeader() },
    body: JSON.stringify({ workflow: [{ name: "bad", steps: [{ when: ["checks:green"], do: "merge:squash" }] }] }),
  });
  const said = await res.json();
  if (said.ok) throw new Error("a step nobody can parse was saved");
  if (!String(said.error).includes("bad") || !String(said.error).includes("passing")) {
    throw new Error(`the refusal does not say where to look or what to say: ${said.error}`);
  }
  // And the good file is still there: a refusal that arrives after the old file is gone is a
  // refusal that cost somebody their workflows.
  const still = JSON.parse(fs.readFileSync(path.join(fx.home, "workflows.json"), "utf8"));
  if ((still.workflow || [])[0]?.name !== "watch-ci") {
    throw new Error("a refused save destroyed the file it refused to replace");
  }
});

console.log("\nedge states");   // SKEIN-154 — both were correct prose and inert as affordances
// SKEIN-228. Re-analysis existed only behind the fold, ninth of nine chips, which from the outside
// is the same as not existing. The row that most needs it is one read against an EARLIER commit —
// the line already says so, and this is the answer to that sentence.
await check("a reading of an older commit offers its re-read on the line", async () => {
  const before = await page.$$eval("#revpane .revrow .revline",
    els => Math.max(...els.map(e => e.getBoundingClientRect().height)));
  const key = await page.evaluate(() => {
    const pr = (revQueue.prs || []).find(p => {
      const s = revSums.get(rk(p));
      return p.lane === "needs-you" && s && s !== "…" && s.depth !== "unread";
    });
    const s = revSums.get(rk(pr));
    // What the bulk payload says when the branch has moved under a reading skein already has.
    revSums.set(rk(pr), { ...s, stale: true, head_sha: "older" });
    renderReview();
    return rk(pr);
  });
  const btn = await mustSee(`#revpane .revrow[data-rk="${key}"] .revread`, "the row's read control");
  const after = await page.$$eval("#revpane .revrow .revline",
    els => Math.max(...els.map(e => e.getBoundingClientRect().height)));
  if (after > before)
    throw new Error(`the control changed the row's height: ${before} → ${after}`);
  if (!/never counted against the day/.test(await btn.getAttribute("title")))
    throw new Error("the control does not say that asking costs nothing");

  // Pressing it asks the server the way a person asks — and does not toggle the fold underneath,
  // which is what a control inside a row whose whole line is a toggle would do by default.
  const fold = () => page.$(`#revpane .revrow[data-rk="${key}"].open`).then(Boolean);
  const wasOpen = await fold();
  const urls = [];
  const listen = r => urls.push(r.url());
  page.on("request", listen);
  await btn.click();
  await settle(600);
  page.off("request", listen);
  // `redraft=1` since SKEIN-293: there is one control, and it always produces both halves. It
  // implies the forced read, so the marker that says what must come BACK is the one on the wire.
  if (!urls.some(u => /review\/\d+\/summary\?redraft=1$/.test(u)))
    throw new Error(`no manual read went out: ${JSON.stringify(urls.filter(u => u.includes("summary")))}`);
  if (await fold() !== wasOpen)
    throw new Error("pressing the read control toggled the row it sits on");
});
// And where somebody who has just read the diff is most likely to want one.
await check("the reading view carries the same control", async () => {
  const target = await page.evaluate(() => {
    const pr = (revQueue.prs || []).find(p => p.lane === "needs-you");
    openReading(pr.repo_id, pr.number);
    return pr.number;
  });
  await settle(600);
  // The queue is put back whatever happens: a check that fails inside the reading view would
  // otherwise leave every check after it looking at a diff.
  try {
    const btn = await mustSee("#revpane .readhead .revread", "the reading view's read control");
    if (!/re-read|reading/.test((await btn.textContent()).trim()))
      throw new Error(`the control does not name what it does: ${await btn.textContent()}`);
    const urls = [];
    const listen = r => urls.push(r.url());
    page.on("request", listen);
    await btn.click();
    await settle(600);
    page.off("request", listen);
    if (!urls.some(u => new RegExp(`review/${target}/summary\\?redraft=1$`).test(u)))
      throw new Error(`the reading view's control asked for nothing: ${JSON.stringify(urls)}`);
  } finally {
    await page.evaluate(() => closeReading());
    await settle(400);
  }
});
// A queue you have cleared is the best moment this product has, and it used to be "nothing here."
// in the corner of a 1400 px page while another repo held ten. This drives the real filter and the
// real render: the your-move rows are moved to another repo, so acme genuinely has none.
await check("a cleared queue reads like one and names what the rest of the fleet holds", async () => {
  // The load `openReview` starts must land BEFORE the rows are moved, or it arrives a moment later
  // and puts the real queue back under the assertion.
  await page.evaluate(() => openReview("acme"));
  await page.waitForFunction(() => !revLoading && revQueue && (revQueue.prs || []).length,
    null, { timeout: 15000 });
  await page.evaluate(() => {
    // Each moved row takes its reading with it. `rk` is repo + number, so a row that changes repo
    // becomes a row nothing has read — and the pump would then ask the server about a repo that
    // does not exist, which is a 404 in the console and a check failing three sections later.
    revQueue = { ...revQueue, prs: (revQueue.prs || []).map(p => {
      if (p.lane !== "needs-you") return p;
      const moved = { ...p, repo_id: "lattice" };
      revSums.set(rk(moved), revSums.get(rk(p))
        || { number: p.number, head_sha: p.head_sha, depth: "unread", unread_because: "nobody asked" });
      return moved;
    }) };
    renderReview();
  });
  await settle(300);
  const head = await mustSee("#revpane .revclear-head", "the cleared-queue headline");
  if (!/acme is clear/.test((await head.textContent()).trim()))
    throw new Error(`the headline is not about the repo you are looking at: ${await head.textContent()}`);
  const next = await mustSee("#revpane .revclear-next .revclear-row", "the honest next thing");
  const said = (await next.textContent()).replace(/\s+/g, " ").trim();
  if (!/lattice/.test(said) || !/^\d/.test(said))
    throw new Error(`the other repo is not named with its count: ${said}`);
  // A headline you cannot read is not a headline: it must outrank the sentence under it.
  const sizes = await page.evaluate(() => [
    parseFloat(getComputedStyle(document.querySelector("#revpane .revclear-head")).fontSize),
    parseFloat(getComputedStyle(document.querySelector("#revpane .revclear-sub")).fontSize)]);
  if (!(sizes[0] > sizes[1])) throw new Error(`the cleared screen has no headline: ${sizes}`);
});
// And the failure that used to replace the queue. Provoked in the page rather than by breaking
// GitHub for the rest of the run: what is asserted is what the pane does with the state, and the
// state is exactly what `loadReview`'s catch now builds.
await check("a queue that could not be built keeps its rows, dimmed, and offers the way out", async () => {
  await page.evaluate(() => { loadReview(true); });
  await page.waitForFunction(() => !revLoading && revQueue && (revQueue.prs || []).length, null, { timeout: 15000 });
  const rows = await page.evaluate(() => {
    revQueue = { ...revQueue, error: "401 Bad credentials", remembered: true };
    renderReview();
    return document.querySelectorAll("#revpane .revrow").length;
  });
  if (!rows) throw new Error("the failure replaced the queue that was on screen");
  const box = await mustSee("#revpane .revfail", "the failure box");
  const said = (await box.textContent()).replace(/\s+/g, " ");
  if (!/401 Bad credentials/.test(said)) throw new Error(`it does not say what GitHub said: ${said}`);
  if (!/last read/.test(said)) throw new Error(`it does not say the rows are the remembered copy: ${said}`);
  await mustSee("#revpane .revfail .revchip:has-text('try again')", "the retry");
  await mustSee("#revpane .revfail .revchip:has-text('GitHub')", "the setting that would fix it");
  // The 55% is load-bearing: it is how you tell what you are looking at is not live without
  // reading anything.
  const dim = await page.$eval("#revpane .revlane", e => parseFloat(getComputedStyle(e).opacity));
  if (!(dim < 1)) throw new Error(`the remembered rows are drawn as though they were live: ${dim}`);
  // Put the pane back, so this check costs the ones after it nothing.
  await page.evaluate(() => { openReview(""); loadReview(true); });
  await settle(600);
});

console.log("\nposting a drafted review");
// SKEIN-264, reported live: "posting comments button doesn't work, they aren't responsive even if
// something is happening in the background." Every candidate for that is invisible to a test that
// only asserts the request went out, so these press the real control in a real page.
//
// Reaching the panel: the fixture's model answers the merged prompt with a `REVIEW:` section, so
// skein holds a draft for the rows it has read; where it does not, the panel's own button asks for
// one and the same stub answers.
let critKey = null;
await check("a drafted review opens in the row it belongs to", async () => {
  await page.evaluate(() => { openReview(""); });
  await page.waitForFunction(() => revQueue && (revQueue.prs || []).some(p => p.lane === "needs-you"),
    null, { timeout: 20000 });
  critKey = await page.evaluate(() => {
    const pr = (revQueue.prs || []).find(p => p.lane === "needs-you");
    const key = pr.repo_id + "#" + pr.number;
    // Opened, not TOGGLED: the expanding checks above may have left this very row open, and a
    // toggle would close it — which is a whole check failing on the order of the file.
    if (!revOpen.has(key)) toggleRevRow(key);
    revCritiqueOpen(pr.repo_id, pr.number);
    return key;
  });
  await page.waitForSelector("#revpane .revcrit", { timeout: 20000 });
  const ask = await page.$("#revpane .revcrit .revchip:has-text('review the code')");
  if (ask) await ask.click();
  await page.waitForSelector("#revpane .revcrit textarea", { timeout: 60000 });
  await mustSee("#revpane .revcrit .revchip:has-text('post')", "the post control");
});
// The gate SKEIN-244 removed, checked where a person meets it: the server re-anchors by line text
// now, so a moved head must offer the post and say what will happen to it.
await check("a draft of an earlier commit is still postable, and says what will happen", async () => {
  await page.evaluate(k => {
    revCrits.get(k).critique.head_sha = "a-commit-that-has-been-pushed-past";
    renderReview();
  }, critKey);
  await settle(200);
  const said = (await page.textContent("#revpane .revcrit .revstale")).replace(/\s+/g, " ");
  if (!/still match will post at their new place/.test(said))
    throw new Error(`the pane does not say what a moved head does to the comments: ${said}`);
  const off = await page.$eval("#revpane .revcrit .revchip:has-text('post')", e => e.disabled);
  if (off) throw new Error("the post is still disabled at a moved head — the treadmill SKEIN-215 removed");
  await page.evaluate(k => { revCrits.get(k).critique.head_sha =
    (revQueue.prs.find(p => k === p.repo_id + "#" + p.number) || {}).head_sha; renderReview(); }, critKey);
  await settle(200);
});
// The modal is gone. A browser where somebody once ticked "prevent this page from creating
// additional dialogs" answers every later confirm with a synchronous false, which is what made this
// press evaporate — so the check is that stubbing exactly that can no longer stop the post.
await check("a browser that refuses every dialog can no longer swallow the press", async () => {
  await page.evaluate(() => { window.confirm = () => { window.__asked = true; return false; }; });
  await page.click("#revpane .revcritacts .revchip:has-text('post')");
  await settle(200);
  if (await page.evaluate(() => window.__asked))
    throw new Error("the press still asks a native dialog, which a browser setting can answer for it");
  const said = (await page.textContent("#revpane .revcritacts")).replace(/\s+/g, " ");
  if (!/posting 1 comment as one review/.test(said) && !/posting \d+ comments as one review/.test(said))
    throw new Error(`the press left no receipt where the button was: ${said.slice(0, 200)}`);
  if (!/undo/.test(said)) throw new Error(`a receipt with no way back inside its window: ${said}`);
  // Taken back, so the checks below start from a press of their own.
  await page.click("#revpane .revcritacts .revchip:has-text('undo')");
  await settle(200);
  await mustSee("#revpane .revcritacts .revchip:has-text('post')", "the post control, back");
});
// SKEIN-273, reported live on #684: skein said "nothing to flag" and there was no way to agree with
// it — `review::post_critique` can only post a drafted review as a COMMENT. The check is in a real
// browser for the reason this whole file exists: the report was about a control that was not on
// screen, and a chip nothing draws is exactly the failure `#gitq` shipped with.
const shut = async () => page.evaluate(k => {
  const c = revCrits.get(k);
  const at = k.lastIndexOf("#");
  if (c && c.open) revCritiqueOpen(k.slice(0, at), Number(k.slice(at + 1)));
}, critKey);
// SKEIN-293, the owner's decision after asking "when I click re read, does it give review as well?
// If so why is there separate re read and review the code buttons?" — **one control**.
//
// It always produces both halves, so on a row that already has a draft it REPLACES it. Where the
// reader has vetted that draft — kept and dropped comments, or edited text — the press says so
// first, through the pane's own receipt and undo, never a native dialog (SKEIN-264: a browser told
// once to suppress dialogs answers every later confirm with false, and the press evaporates).
// Where there is nothing to lose it goes straight away, because a confirmation that fires on every
// row is one nobody reads by the third.
await check("one control reads and drafts, and warns only where vetting would be lost", async () => {
  await shut();
  const opener = "#revpane .revdraft .revchip:has-text('and post')";
  await mustSee(opener, "the control that opens the vetting panel");
  await page.click(opener);
  await page.waitForSelector("#revpane .revcrit textarea", { timeout: 5000 });
  // The panel offers ONE read control, and it is the row's own.
  const acts = await page.textContent("#revpane .revcritacts");
  if (/draft again/.test(acts)) throw new Error(`the second control is still there: ${acts}`);
  if (!/read it again/.test(acts)) throw new Error(`the one control is missing: ${acts}`);

  // Nothing vetted yet: the press goes, with no receipt in the way.
  const urls = [];
  const spy = r => urls.push(new URL(r.url()).pathname + new URL(r.url()).search);
  page.on("request", spy);
  try {
    await page.click("#revpane .revcritacts .revchip:has-text('read it again')");
    await settle(500);
    if (!urls.some(u => /\/summary\?redraft=1$/.test(u)))
      throw new Error(`an untouched draft asked the reader to confirm, or asked for nothing: ${urls.join(", ")}`);
    await page.waitForSelector("#revpane .revcrit textarea", { timeout: 30000 });

    // Now vet it — through the RENDERED control, by typing into the overall note, which is the
    // one editing surface every drafted review has whatever it found. A dropped comment is the
    // other half of the same rule and rides the same `revVetted`.
    const note = await page.$("#revpane .revcrit textarea");
    if (!note) throw new Error("no editable draft, so this check would prove nothing");
    await note.click();
    await note.type(" — and the caller cannot tell");
    await settle(200);
    if (!await page.evaluate(k => !!(revCrits.get(k) || {}).edited, critKey))
      throw new Error("typing in the panel was not recorded as work, so the warning cannot fire");
    urls.length = 0;
    await page.click("#revpane .revcritacts .revchip:has-text('read it again')");
    await settle(400);
    if (urls.some(u => /redraft=1/.test(u)))
      throw new Error(`a vetted draft was replaced with no warning: ${urls.join(", ")}`);
    const held = (await page.textContent("#revpane .revcritacts")).replace(/\s+/g, " ");
    if (!/replaces the review you vetted/.test(held))
      throw new Error(`the press did not say what it would cost: ${held.slice(0, 200)}`);
    if (!/undo/.test(held)) throw new Error(`a held read with no way back: ${held.slice(0, 200)}`);
    // Taken back, so nothing is spent by a check about the eight seconds before it would be.
    await page.click("#revpane .revcritacts .revchip:has-text('undo')");
    await settle(200);
    if (urls.some(u => /redraft=1/.test(u)))
      throw new Error(`undo did not stop the read: ${urls.join(", ")}`);
  } finally {
    page.off("request", spy);
    await page.evaluate(k => { revPending.delete(k); const c = revCrits.get(k); if (c) { c.drop = new Set(); c.edited = false; } renderReviewNow(); }, critKey);
    await settle(200);
  }
});

// SKEIN-284, reported live: "approve with this review button doesn't really have feedback. So when
// I click idk if it went through or not." The check below drives the control in the VETTING
// PANEL's strip and passes. This one drives the copy in the READ-ONLY block — `revDraftSection`,
// which is what an expanded row shows with the panel CLOSED, and which is where a reader who has
// not pressed "go through N comments" meets this button. It is the same control in a different
// container, repainted by a different path, and the suite has never pressed it.
//
// The rendered attribute is read before the press, on purpose: SKEIN-261 shipped three buttons
// whose handlers ended at a sha's opening quote, invisible to every test that called the function
// instead of going through the DOM.
await check("the same approval is offered, and acknowledged, with the panel closed", async () => {
  await shut();
  await settle(200);
  const chip = "#revpane .revdraft .revchip:has-text('approve with this review')";
  await mustSee(chip, "the approve-with-this-review control in the read-only block");
  const handler = await page.getAttribute(chip, "onclick");
  if (!/^revApproveWithReview\(/.test(handler || ""))
    throw new Error(`the rendered handler is not the one that approves: ${handler}`);
  await page.click(chip);
  await settle(200);
  const held = (await page.textContent("#revpane .revdraft")).replace(/\s+/g, " ");
  if (!/✓ approved/.test(held))
    throw new Error(`the press left no receipt where the control was: ${held.slice(0, 300)}`);
  if (!/undo/.test(held)) throw new Error(`a held approval with no way back: ${held}`);
  await page.click("#revpane .revdraft .revchip:has-text('undo')");
  await settle(200);
  await mustSee(chip, "the control, back after the undo");
});

// SKEIN-286, reported live mid-stack on PR 696: "when I clicked go through 2 comments and post it
// started saying `reviewing the code… this reads the whole diff, so it can take a few minutes`."
//
// That button can only say "2 comments" because the page is HOLDING the draft — `revDraftSection`
// counted them out of `revSums`. Opening the panel used to discard it, set `busy` with no critique,
// and re-fetch from disk; and `busy && !critique` is the state that draws the sentence about
// minutes. So the press announced a model call for a 4 ms read of a variable.
//
// Driven through the rendered control, and the request log is the assertion: a panel that opens
// with what it already has asks for nothing at all.
await check("opening a drafted review is instant and offline, and never claims to be re-reading", async () => {
  await shut();
  await settle(200);
  const asked = [];
  const spy = req => {
    const path = new URL(req.url()).pathname;
    if (/\/critique$/.test(path)) asked.push(`${req.method()} ${path}`);
  };
  page.on("request", spy);
  try {
    const opener = "#revpane .revdraft .revchip:has-text('and post')";
    await mustSee(opener, "the control that opens the vetting panel");
    await page.click(opener);
    // What is on screen the instant the press returns — the render happens inside the handler.
    const at = (await page.textContent("#revpane .revcrit")).replace(/\s+/g, " ");
    if (/reviewing the code/.test(at))
      throw new Error(`opening a draft the page already holds announced a model call: ${at.slice(0, 200)}`);
    // And it opened ONTO the draft, rather than onto the pre-draft invitation.
    await page.waitForSelector("#revpane .revcrit textarea", { timeout: 2000 });
    await settle(400);
    if (asked.length)
      throw new Error(`the panel asked the server for a draft it was already displaying: ${asked.join(", ")}`);
  } finally {
    page.off("request", spy);
    await shut();
    await settle(200);
  }
});

// **SKEIN-284's actual shape**: the row that had no feedback was inside a STACK.
//
// `revStackSteps` draws a stacked pull request as a `.step`, and the `.revrow` around it carries
// the STACK's key — so `revRepaintRow`'s `.revrow[data-rk=…]` matched nothing and the press
// repainted nothing at all. Every check above passes because they all drive LOOSE rows; the owner's
// fleet is one 18-step stack, and every row they can press is a step.
//
// The stack is built out of the fixture's own pull requests, by basing one on another's branch —
// which is exactly what `revChains` reads (`base_ref` → `head_ref`), so this is the real stack
// renderer and not a stand-in for it.
await check("a verdict pressed on a stack step is acknowledged where it was pressed", async () => {
  await shut();
  const built = await page.evaluate(k => {
    const all = revQueue.prs || [];
    const step = all.find(p => (p.repo_id + "#" + p.number) === k);
    // Any other pull request in the same repo, brought into the same lane: a stack is a chain of
    // `base_ref` → `head_ref` within one lane, and this fixture has a single your-move row.
    const other = all.find(p => p !== step && p.repo_id === step.repo_id && p.lane !== "archived");
    if (!step || !other) return { why: `rows: ${all.length}, step ${!!step}, other ${!!other}` };
    other.lane = "needs-you";
    other.snoozed = false;
    // The drafted row is the ROOT; the other is based on its branch, which is what makes a chain.
    other.base_ref = step.head_ref;
    revOpen = new Set();
    renderReviewNow();
    const stackKey = [...revStacks.keys()][0];
    if (!stackKey) return { why: `no stack formed; filter=${revRepoFilter}, search=${revSearch}` };
    toggleRevStack(stackKey);
    toggleStackStep(k);
    return { stackKey };
  }, critKey);
  try {
    if (!built || !built.stackKey)
      throw new Error(`the fixture would not form a stack, so this check would prove nothing: ${built && built.why}`);
    await settle(200);
    const chip = "#revpane .revrow.stack .revdraft .revchip:has-text('approve with this review')";
    await mustSee(chip, "the approve control inside the opened stack step");
    await page.click(chip);
    await settle(200);
    const held = (await page.textContent("#revpane .revrow.stack .revdraft")).replace(/\s+/g, " ");
    if (!/✓ approved/.test(held))
      throw new Error(`a press on a stack step left no receipt where it was pressed: ${held.slice(0, 300)}`);
    await page.click("#revpane .revrow.stack .revdraft .revchip:has-text('undo')");
    await settle(200);
  } finally {
    // Put the queue back the way the checks below expect it — loose rows, no stack, panel open —
    // whatever happened above. A check that leaves the pane rearranged on FAILURE reports its own
    // bug three times, in the two checks after it as well as in itself.
    await page.evaluate(k => {
      for (const p of revQueue.prs || []) p.base_ref = "main";
      revStackOpenKey = null; revStackStep = null;
      revPending.delete(k);
      const at = k.lastIndexOf("#");
      revOpen = new Set([k]);
      renderReviewNow();
      const c = revCrits.get(k);
      if (!c || !c.open) revCritiqueOpen(k.slice(0, at), Number(k.slice(at + 1)));
    }, critKey);
    await settle(400);
  }
});
await check("skein's review can be approved WITH, from the block that shows it", async () => {
  const chip = "#revpane .revcritacts .revchip:has-text('approve with this review')";
  await mustSee(chip, "the approve-with-this-review control");
  const says = (await page.textContent("#revpane .revcritacts")).replace(/\s+/g, " ");
  if (!/approve posts skein's note above as the approval/.test(says))
    throw new Error(`the control does not say what it will post: ${says.slice(0, 200)}`);
  // The exact body is on the control, character for character — the reader has to be able to see
  // what they are sending. It is the REVIEW's own words and nothing else: the trailer that used to
  // name skein is gone (SKEIN-285, the owner's "It should be as if I am writing it").
  const willSend = await page.getAttribute(chip, "title");
  if (!/^posts exactly this as the approval:/.test(willSend || ""))
    throw new Error(`the exact body is not on the control, so what posts is unseen: ${willSend}`);
  if (/skein drafted this review|approved as written/.test(willSend || ""))
    throw new Error(`the approval still signs itself as skein's: ${willSend}`);
  await page.click(chip);
  await settle(200);
  const held = (await page.textContent("#revpane .revcritacts")).replace(/\s+/g, " ");
  if (!/✓ approved/.test(held))
    throw new Error(`the press left no receipt where the control was: ${held.slice(0, 200)}`);
  if (!/undo/.test(held)) throw new Error(`a held approval with no way back: ${held}`);
  if (/approve with this review|as one review/.test(held))
    throw new Error(`a held approval still offers a press that would post the same words again: ${held}`);
  // Taken back inside the window — so nothing reaches the fixture's GitHub from a check that is
  // about the eight seconds before it would, and the post below starts from a strip of its own.
  await page.click("#revpane .revcritacts .revchip:has-text('undo')");
  await settle(200);
  await mustSee("#revpane .revcritacts .revchip:has-text('post')", "the post control, back");
});
// The acknowledgement itself. A live text selection over the pane holds renderReview by §6 focus
// rule 2 — measured in chromium: after a real click on a button `document.activeElement` is the
// BUTTON and a caret is gone, but a SELECTION survives the press and defers the render. That is the
// reader who highlighted a phrase in a drafted comment and then pressed post, and saw nothing.
await check("the receipt appears even with text selected in the panel, and the review posts", async () => {
  const held = await page.evaluate(() => {
    const label = document.querySelector("#revpane .revcrit .revcl");
    const r = document.createRange();
    r.selectNodeContents(label);
    getSelection().removeAllRanges();
    getSelection().addRange(r);
    // The page's own rule, asked here so this cannot pass on a selection that never took.
    return revRenderHeld();
  });
  if (!held) throw new Error("the selection did not hold the render — this check would prove nothing");
  await page.click("#revpane .revcritacts .revchip:has-text('post')");
  const at = (await page.textContent("#revpane .revcritacts")).replace(/\s+/g, " ");
  if (!/posting/.test(at))
    throw new Error(`the press left nothing on screen while it was held: ${at.slice(0, 200)}`);
  // And it lands, with no reload — the window lapses, the request goes out, the strip says so.
  await page.waitForFunction(
    () => /✓ posted|GitHub refused/.test(document.querySelector("#revpane .revcritacts")?.textContent || ""),
    null, { timeout: 20000 });
  const done = (await page.textContent("#revpane .revcritacts")).replace(/\s+/g, " ");
  if (/GitHub refused/.test(done)) throw new Error(`the post was refused: ${done}`);
  if (!/✓ posted/.test(done)) throw new Error(`the row never reached a posted state: ${done}`);
});

console.log("\none row failing");
// SKEIN-268, reported by the owner: "any small error anywhere in the review page just blanks the
// entire page and gives the error." The pane is one string assigned in one shot, so a throw in any
// of the helpers that string calls meant the assignment never ran. Injected here into the REAL
// `revRow`, in the real page, because the claim is about what a person is left looking at.
await check("a row that throws leaves the rest of the queue drawn and clickable", async () => {
  await page.evaluate(() => { openReview(""); closeReading?.(); });
  await page.waitForFunction(() => revQueue && (revQueue.prs || []).length >= 2, null, { timeout: 20000 });
  const target = await page.evaluate(() => {
    const pr = (revQueue.prs || [])[0];
    const key = pr.repo_id + "#" + pr.number;
    const real = window.revRow;
    window.__realRevRow = real;
    window.revRow = p => {
      if (p.repo_id + "#" + p.number === key) throw new TypeError("cannot read properties of undefined (reading 'map')");
      return real(p);
    };
    renderReview(true);
    return key;
  });
  const rows = await page.$$eval("#revpane .revrow", els => els.length);
  if (rows < 2) throw new Error(`the pane kept ${rows} rows — one throw took the queue with it`);
  const broken = await mustSee("#revpane .revrow.broken", "the row that could not be drawn");
  const said = (await broken.textContent()).replace(/\s+/g, " ");
  if (!said.includes(target)) throw new Error(`the broken row does not say which PR it is: ${said}`);
  if (!/cannot read properties of undefined/.test(said))
    throw new Error(`the reason is not in the page: ${said}`);
  // Still a queue you can work: a healthy row expands on click, and the keyboard still walks the
  // full list — a row dropped from `revNav` would shorten j/k for as long as the fault lasted.
  const healthy = await page.evaluate(k =>
    (revNav || []).find(x => x !== k && !revOpen.has(x)), target);
  if (!healthy) throw new Error("no healthy, closed row survived to click");
  await page.click(`#revpane .revrow[data-rk="${healthy}"] .revline`);
  await settle(400);
  const open = await page.evaluate(() => [...revOpen]);
  if (!open.includes(healthy)) throw new Error(`clicking a healthy row did nothing: open=${open}`);
  const inNav = await page.evaluate(k => (revNav || []).includes(k), target);
  if (!inNav) throw new Error("the broken row left the keyboard's list, silently shortening j/k");
  // And the fault reached the person, rather than devtools.
  const toast = (await page.textContent("#toast").catch(() => "")) || "";
  if (!/something went wrong in the page/.test(toast))
    throw new Error(`nothing said a row had failed: ${JSON.stringify(toast)}`);
});
// The fault clears the way a real one does — the data stops being malformed — and the row comes
// back rather than staying broken until a reload.
await check("and the next render puts the row back", async () => {
  await page.evaluate(() => { window.revRow = window.__realRevRow; renderReview(true); });
  await settle(300);
  if (await page.$("#revpane .revrow.broken")) throw new Error("the row stayed broken after the fault cleared");
  const rows = await page.$$eval("#revpane .revrow", els => els.length);
  if (rows < 2) throw new Error(`the queue did not come back: ${rows} rows`);
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
  // The server's own account of the run. When the queue is empty because a mirror could not be
  // made, the diagnosis is one stderr line (`skein: reading acme: …`) that no assertion can see.
  console.log(`\nserver log:\n${log().split("\n").slice(-25).join("\n")}`);
  // Named here as well as inline, because the inline FAIL lines sit above the server log and a
  // truncated view (browser_suites.rs shows only the tail) would otherwise lose which checks died.
  console.log(`\n${failed.length} of ${results.length} checks failed:`);
  for (const [, name] of failed) console.log(`  ✗ ${name}`);
  console.log(`screenshot: ${shot}\nfixture kept for inspection: ${fx.root}`);
} else {
  console.log(`\nall ${results.length} checks passed`);
}
await browser.close();
srv.kill();
if (!failed.length && !process.env.SKEIN_KEEP) fs.rmSync(fx.root, { recursive: true, force: true });
process.exit(failed.length ? 1 : 0);
