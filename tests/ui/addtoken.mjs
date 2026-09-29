// Adding a private repository from the cockpit, with the token the person chose for it
// (SKEIN-1231): one they paste, or the one another repo already has.
//
// The owner's report that bought this: adding a private repo from the cockpit failed every time.
// The dialog stored its token only after the add returned, so the clone went out with no
// credential, was refused, and the token was thrown away with the failed add. Here the whole path
// runs for real — the page, the route, `repos::add_repo_with_token`, real git — against a private
// repository that refuses anyone without the right token, so what is checked is what a person sees.
//
// **Nothing here reaches GitHub, and no token here is a credential.** The repositories are named
// `https://github.com/acme/…`, because skein offers a token only to a remote it can key to a GitHub
// repository, and a git config given to the server (`GIT_CONFIG_GLOBAL`, `insteadOf`) sends those
// URLs to a dumb-HTTP host on loopback that answers 401 without `LET_IN`. The share's push check
// goes to a stub GitHub API on loopback (`SKEIN_GITHUB_API`). Both tokens start `skein-test-`.
//
// And the rule the item is strictest about: **no response, page or log carries a token's bytes.**
// Every `/api/` response the page receives is kept and searched, and so are the page and the
// server's own output.
//
//   node tests/ui/addtoken.mjs
import { chromium } from "playwright";
import { execFileSync } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const API_TOKEN = "t".repeat(64);
// The one token the private host lets in, and the token `acme/first` already has stored.
const LET_IN = "skein-test-private-token";
// A token for some other repository: GitHub turns it down here.
const WRONG = "skein-test-token-for-another-repo";

function git(cwd, ...args) {
  return execFileSync("git", args, {
    cwd,
    env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@e", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@e" },
    stdio: ["ignore", "pipe", "pipe"],
  }).toString();
}

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-addtoken");
  const home = path.join(root, "home");
  const fleet = path.join(root, "fleet");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(fleet, { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  // `acme/first` already has a token — the one a new repo can share.
  fs.writeFileSync(path.join(home, "github-pats.json"),
    JSON.stringify([{ id: "acme-first", label: "acme/first", repos: ["acme/first"] }]));
  fs.mkdirSync(path.join(home, "github-pats"), { mode: 0o700 });
  fs.writeFileSync(path.join(home, "github-pats", "acme-first"), LET_IN, { mode: 0o600 });
  // One upstream, served as every `acme/<name>.git`.
  const work = path.join(root, "upstream-work");
  fs.mkdirSync(work);
  git(work, "init", "-q", "-b", "main");
  fs.writeFileSync(path.join(work, "README.md"), "private\n");
  git(work, "add", "-A");
  git(work, "commit", "-q", "-m", "one");
  const bare = path.join(root, "upstream.git");
  git(root, "clone", "-q", "--bare", work, bare);
  git(bare, "update-server-info");
  // A `gh` that has no login, first on $PATH: the fourth credential source `harness/server.mjs`
  // names, closed so that a machine where somebody ran `gh auth login` cannot answer for the page.
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, "gh"), "#!/bin/sh\nexit 1\n", { mode: 0o755 });
  return { root, home, fleet, bare, bin };
}

// A private GitHub repository on loopback. Every request without `LET_IN` is a 401 with a Basic
// challenge, which is what makes git ask its credential helper — skein's own.
function startPrivateHost(fx) {
  const want = "Basic " + Buffer.from(`x-access-token:${LET_IN}`).toString("base64");
  const served = new Set();
  const server = http.createServer((req, res) => {
    if (req.headers.authorization !== want) {
      res.writeHead(401, { "WWW-Authenticate": 'Basic realm="x"' });
      return res.end();
    }
    const m = /^\/acme\/([a-z0-9-]+)\.git\/(.*)$/.exec(decodeURIComponent(req.url.split("?")[0]));
    const file = m && path.resolve(fx.bare, m[2]);
    if (!m || !file.startsWith(path.resolve(fx.bare) + path.sep)) { res.writeHead(404); return res.end(); }
    fs.readFile(file, (err, body) => {
      if (err) { res.writeHead(404); return res.end(); }
      served.add(m[1]);
      res.writeHead(200, { "content-type": "application/octet-stream", "content-length": body.length });
      res.end(body);
    });
  });
  return new Promise(ok => server.listen(0, "127.0.0.1", () => ok({ server, port: server.address().port, served })));
}

// GitHub's `GET /repos/{slug}`, for the share's push check. `push` is what the test says it is.
function startGitHubApi() {
  const state = { push: false, asked: [] };
  const server = http.createServer((req, res) => {
    state.asked.push(req.url);
    const body = JSON.stringify({ permissions: { push: state.push } });
    res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(body) });
    res.end(body);
  });
  return new Promise(ok => server.listen(0, "127.0.0.1", () => ok({ server, url: `http://127.0.0.1:${server.address().port}`, state })));
}

const fx = makeFixture();
const host = await startPrivateHost(fx);
const api = await startGitHubApi();
const gitconfig = path.join(fx.root, "gitconfig");
fs.writeFileSync(gitconfig, `[url "http://127.0.0.1:${host.port}/"]\n\tinsteadOf = https://github.com/\n`);

const door = await openDoor();
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: fx.fleet,
    SKEIN_GITHUB_API: api.url,
    GH_TOKEN: "",
    PATH: `${fx.bin}:${process.env.PATH}`,
    GIT_CONFIG_GLOBAL: gitconfig,
    GIT_CONFIG_NOSYSTEM: "1",
    NO_PROXY: "127.0.0.1", no_proxy: "127.0.0.1",
    http_proxy: "", https_proxy: "", HTTP_PROXY: "", HTTPS_PROXY: "", ALL_PROXY: "", all_proxy: "",
  },
});

const base = `http://127.0.0.1:${door.port}`;
const auth = { Authorization: `Bearer ${API_TOKEN}` };
const getJson = async p => (await fetch(`${base}${p}`, { headers: auth })).json();
const repoIds = async () => (await getJson("/api/repos")).map(r => r.id).sort();
const creds = async () => (await getJson("/api/fleet/git-grants")).credentials
  .map(c => ({ id: c.id, repos: c.repos, shared: c.shared, has_token: c.has_token }));

const { value: check, report } = ledger();
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 1000 } });
const { errors, sayBlips } = erring(page, { say: (kind, text) => `${kind}: ${text}` });
// Every `/api/` answer the page gets, whole, for the check at the end.
const answers = [];
page.on("response", async r => {
  if (!r.url().includes("/api/")) return;
  try { answers.push(`${r.url()}\n${await r.text()}`); } catch { /* a stream the page abandoned */ }
});
const until = async (f, ms = 20000) => {
  for (let t = 0; t < ms; t += 200) { if (await f()) return true; await page.waitForTimeout(200); }
  return !!(await f());
};
const msg = async () => ((await page.textContent("#ar-msg").catch(() => "")) || "").trim();
const dialogOpen = () => page.evaluate(() => arModal().classList.contains("open"));

try {
  await page.goto(`${base}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(800);

  // --- paste a new token -------------------------------------------------------------------------
  await page.evaluate(() => openAddRepo());
  await until(async () => (await page.locator("#ar-tokwhich option").count()) === 2);
  // **What would make this fail:** `arRenderTokens` not drawn, or drawing a token's bytes rather
  // than the repos that use it.
  check("the dialog offers the token another repo has, named by that repo",
    await page.locator("#ar-tokwhich option").allTextContents(),
    ["Paste a new token", "Use the token acme/first uses"]);

  await page.fill("#ar-src", "https://github.com/acme/private.git");
  await page.fill("#ar-token", WRONG);
  await page.click("#ar-go");
  await until(async () => (await msg()).startsWith("Couldn't add it"));
  const refused = await msg();
  // **What would make this fail:** `git_refusal` without its add branch — the words then send the
  // person to a repo card that does not exist yet.
  check("a token the private repo turns down is refused, saying whose it was and what to do next",
    refused.includes("GitHub turned down the token you pasted for acme/private")
      && refused.includes("so nothing was saved")
      && refused.includes("Paste a fine-grained token that covers acme/private, with Contents read and write, into the token field and press Add again"),
    true);
  check("and nothing of it was kept", { repos: await repoIds(), creds: (await creds()).map(c => c.id) },
    { repos: [], creds: ["acme-first"] });

  // **What would make this fail:** the token sent after the add rather than with it — the old
  // `arApplySettings` path. The clone then goes out with nothing and is refused, as it was for the
  // owner.
  await page.fill("#ar-token", LET_IN);
  await page.click("#ar-go");
  await until(async () => !(await dialogOpen()));
  check("the same repo is added with a token that reaches it, on the retry, from the dialog",
    { open: await dialogOpen(), said: await msg(), repos: await repoIds() },
    { open: false, said: "", repos: ["private"] });
  check("that token is saved as this repo's own, covering it alone",
    (await creds()).find(c => c.id === "acme-private"),
    { id: "acme-private", repos: ["acme/private"], shared: false, has_token: true });
  check("and the clone really came from the private host", host.served.has("private"), true);

  // --- use the token another repo has -------------------------------------------------------------
  await page.evaluate(() => openAddRepo());
  await until(async () => (await page.locator("#ar-tokwhich option").count()) === 3);
  await page.fill("#ar-src", "https://github.com/acme/second.git");
  await page.selectOption("#ar-tokwhich", "acme-first");
  // The owner's condition for allowing a share: the cost, said before Add is pressed.
  check("choosing it hides the paste field", await page.isVisible("#ar-token"), false);
  check("and says, before anything is saved, what sharing lets the boxes do",
    ((await page.textContent("#ar-tokhint")) || "").trim(),
    "Shared, not copied: boxes of acme/second and of acme/first will all be able to push to all of them with this token. skein first checks that GitHub lets it push to acme/second, and a new token stored on any of their cards later replaces it for all of them.");

  api.state.push = false;
  await page.click("#ar-go");
  await until(async () => (await msg()).startsWith("Couldn't add it"));
  // **What would make this fail:** the share taken without `gitgate::shareable_token`'s check.
  check("a token GitHub says cannot push to the new repo is not shared",
    (await msg()).includes("the token acme/first uses cannot push to acme/second: GitHub says it has expired, or it was not granted acme/second. Nothing was saved. Paste a new token for acme/second instead"),
    true);
  check("and nothing was cloned, registered or shared",
    { cloned: host.served.has("second"), repos: await repoIds(), first: (await creds()).find(c => c.id === "acme-first").repos },
    { cloned: false, repos: ["private"], first: ["acme/first"] });
  check("the check went to GitHub, for the new repo", api.state.asked.includes("/repos/acme/second"), true);

  api.state.push = true;
  await page.click("#ar-go");
  await until(async () => !(await dialogOpen()));
  check("with push access, the repo is added on the shared token",
    { open: await dialogOpen(), said: await msg(), repos: await repoIds() },
    { open: false, said: "", repos: ["private", "second"] });
  // **What would make this fail:** reuse by copying — a second entry and a second file.
  check("as coverage on the one entry, marked shared — not a copy",
    (await creds()).filter(c => c.repos.includes("acme/second")),
    [{ id: "acme-first", repos: ["acme/first", "acme/second"], shared: true, has_token: true }]);
  check("still one token file for the two of them",
    fs.readdirSync(path.join(fx.home, "github-pats")).sort(), ["acme-first", "acme-private"]);

  // --- the card of a repo that shares a token ------------------------------------------------------
  await page.evaluate(() => openSettings("repos"));
  const cardBody = () => page.locator(`[data-card="second"]`);
  await until(async () => (await cardBody().count()) === 1);
  await cardBody().locator(".rhead").click();
  const said = ((await cardBody().locator(".rbody").textContent()) || "").replace(/\s+/g, " ");
  check("its card names the repo it shares the token with, and what that lets the boxes do",
    said.includes("the token acme/second shares with acme/first. Boxes of all 2 repos can push to all of them with it"), true);
  check("with replacing it for both, and a one-step way to stop sharing",
    [await cardBody().locator("button", { hasText: "Replace for all 2" }).count(),
      await cardBody().locator("button", { hasText: "Use for acme/second alone" }).count()],
    [1, 1]);

  // Stop sharing: a token for this repo alone. The other repo keeps the shared one.
  await cardBody().locator(`[data-repotoken="acme/second"]`).fill(LET_IN);
  await cardBody().locator("button", { hasText: "Use for acme/second alone" }).click();
  await until(async () => (await creds()).some(c => c.id === "acme-second"));
  check("stopping sharing gives the repo its own token and leaves the other its own",
    (await creds()).filter(c => c.id !== "acme-private"),
    [{ id: "acme-first", repos: ["acme/first"], shared: false, has_token: true },
      { id: "acme-second", repos: ["acme/second"], shared: false, has_token: true }]);
  await until(async () => ((await cardBody().locator(".rbody").textContent()) || "").includes("token stored"));
  check("and the card says it has a token of its own now",
    ((await cardBody().locator(".rbody").textContent()) || "").includes("token stored"), true);

  // --- no token bytes anywhere a person or a log can read them -------------------------------------
  // **What would make this fail:** a route echoing its request, an error carrying git's command line
  // with a token in a URL, or a page drawing a stored token back.
  const leaks = where => [LET_IN, WRONG].filter(t => where.includes(t));
  check("no /api/ response carried a token", { answers: answers.length > 10, leaked: leaks(answers.join("\n")) },
    { answers: true, leaked: [] });
  check("nor does the page", leaks(await page.content()), []);
  check("nor the server's log", leaks(log()), []);

  sayBlips();
  check("the dialog and the card raised no page errors", errors, []);
} catch (e) {
  check("the suite could run at all", String((e && e.message) || e), "it ran");
}

await browser.close();
host.server.close();
api.server.close();
const failed = report();
if (failed.length) console.log(`\nserver log:\n${log()}`);
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 });
process.exit(failed.length || leftRunning.length ? 1 : 0);
