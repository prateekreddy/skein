// A genuine first run, in the browser, with nothing on disk.
//
// Every other suite here starts from a fixture that has already been through onboarding: a repo in
// `repos.json`, a placement record, a box on the board. So the path a new person actually walks —
// open the cockpit, add a repo, launch the first box — was the one path no test had ever taken, and
// it is the one that broke. Reported as "it simply doesn't work when I launch box", after which
// driving the same flow from the CLI found nothing, because the CLI is not where it happens.
//
// So: no config.json, no repos.json, no registry, no sandbox — and a fake `sbx` that behaves the
// way a real one does on a machine that has never run skein. Then click through it.
//
//   node tests/ui/onboarding.mjs
import { chromium } from "playwright";
import { spawn, spawnSync, execFileSync } from "node:child_process";
import http from "node:http";
import { createServer } from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const API_TOKEN = "t".repeat(64);
const authHeader = () => ({ Authorization: `Bearer ${API_TOKEN}` });

// ---------- a machine that has never run skein ----------
function makeFixture() {
  // NOT under /tmp: a box binds its own /tmp over the sandbox's, so a fleet root there would be
  // unreadable from outside — skein refuses it, correctly, and the first draft of this fixture spent
  // a run learning that.
  const root = fs.mkdtempSync(path.join(REPO, "target", "ui-onboard-"));
  // The repo the person is going to register: an ordinary local checkout, which is how anyone with
  // existing work arrives. `skein add <path>` adopts it in place.
  const src = path.join(root, "my-project");
  fs.mkdirSync(src, { recursive: true });
  fs.writeFileSync(path.join(src, "README.md"), "# my project\n");
  const git = (...a) => spawnSync("git", ["-C", src, ...a], { stdio: "ignore" });
  git("init", "-q", "-b", "main");
  git("add", "-A");
  git("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "init");

  // `sbx`, as it behaves on a fresh machine: no sandboxes until one is created, then one that
  // answers `exec` by running the script here. That last part is what the real thing does from
  // skein's point of view, and it is why this fixture can go all the way to a created box.
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  const state = path.join(root, "sbx-state");
  fs.mkdirSync(state);
  fs.writeFileSync(sbx, `#!/bin/bash
printf '%s\\n' "$*" >> ${path.join(root, "sbx.log")}
case "$1" in
  ls)
    if [ -f ${state}/fleet ]; then
      echo '{"sandboxes":[{"name":"skein-fleet","status":"running","agent":"","workspaces":[]}]}'
    else
      echo '{"sandboxes":[]}'
    fi
    exit 0 ;;
  create)  touch ${state}/fleet; exit 0 ;;
  # The script is the LAST argument whatever the prefix, exactly as in smoke.mjs: skein addresses a
  # box through its placement, and there is no namespace here to enter.
  exec)    exec bash -c "\${@: -1}" ;;
  ports)   exit 0 ;;
esac
exit 0
`);
  fs.chmodSync(sbx, 0o755);
  fs.mkdirSync(path.join(root, "home"));
  // The API token, written rather than read back: the server mints one at first use, and racing
  // that would make this flaky for a reason unrelated to onboarding. The auth path is still walked
  // end to end — the browser trades `?t=` for a cookie exactly as a person does.
  fs.writeFileSync(path.join(root, "home", "api-token"), API_TOKEN, { mode: 0o600 });
  return { root, src, bin, sbx, state };
}

// A stand-in for the host warden, because skein no longer runs `sbx create` itself.
//
// Fleet create and destroy go through the warden and there is deliberately NO fallback
// (`fleet::create_through_warden`) — so on a machine with no warden this whole suite fails at the
// first launch with a 500, which is correct behaviour and tests nothing about onboarding. The point
// of this fixture is the path a new person walks, and that path now has a warden on it.
//
// It runs the fake `sbx` rather than pretending: what makes the create real from skein's side is
// that a sandbox exists afterwards, and a warden that only said "ran" would leave `sbx ls` empty and
// the board with no fleet. Same shape as the real one — it is the warden's process that runs the
// command, which is the whole reason the environment travels with the request.
function startWarden(fx) {
  const seen = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", d => { body += d; });
    req.on("end", () => {
      seen.push(`${req.method} ${req.url}`);
      const send = o => {
        const text = JSON.stringify(o);
        res.writeHead(200, { "content-type": "application/json", "content-length": Buffer.byteLength(text) });
        res.end(text);
      };
      if (req.url.startsWith("/v1/fleet")) {
        return send({
          sandboxes: fs.existsSync(path.join(fx.state, "fleet")) ? ["skein-fleet"] : [],
          capabilities: ["create", "destroy"],
        });
      }
      if (req.url.startsWith("/v1/create")) {
        const argv = (JSON.parse(body || "{}").args) || [];
        // No approval prompt: there is nobody at a terminal in a test, and what this fixture is
        // for is the flow AROUND the decision. That a person is asked at all is asserted in the
        // warden's own crate, where it can be asserted without a browser.
        try { execFileSync(fx.sbx, argv, { stdio: "ignore" }); } catch {}
        return send({ state: "ran", ok: true, said: "made" });
      }
      if (req.url.startsWith("/v1/destroy")) {
        try { fs.rmSync(path.join(fx.state, "fleet")); } catch {}
        return send({ state: "ran", ok: true, said: "gone" });
      }
      send({ state: "ran", ok: true, said: "" });
    });
  });
  return new Promise(res => {
    server.listen(0, "127.0.0.1", () => res({ server, port: server.address().port, seen }));
  });
}

const freePort = () => new Promise(res => {
  const s = createServer();
  s.listen(0, "127.0.0.1", () => { const { port } = s.address(); s.close(() => res(port)); });
});

async function startServer(fx, port, wardenPort) {
  const build = spawnSync("cargo", ["build", "--bin", "skein-server"], { cwd: REPO, stdio: "inherit" });
  if (build.status !== 0) throw new Error("cargo build failed");
  const srv = spawn(path.join(REPO, "target/debug/skein-server"), {
    cwd: REPO,
    stdio: ["ignore", "pipe", "pipe"],
    env: {
      ...process.env,
      SKEIN_ADDR: `127.0.0.1:${port}`,
      SKEIN_HOME: path.join(fx.root, "home"),
      SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
      SKEIN_NO_GH_SECRET: "1",
      // Where the warden is. Without this skein looks at the default 127.0.0.1:7879 — which on the
      // machine running these tests is either nothing or, worse, somebody's real warden.
      SKEIN_WARDEN: `127.0.0.1:${wardenPort}`,
      PATH: `${fx.bin}:${process.env.PATH}`,
      // Deliberately NOT set: SKEIN_REGISTRY. A new machine has no `sandboxes.json`, and the
      // fallback hunts for a sibling `skein-shared/` named after another project entirely — the
      // exact state in which a first run used to declare itself broken.
      SKEIN_REGISTRY: "",
      SKEIN_SHARED: "",
    },
  });
  let log = "";
  srv.stdout.on("data", d => { log += d; });
  srv.stderr.on("data", d => { log += d; });
  for (let i = 0; i < 100; i++) {
    try { if ((await fetch(`http://127.0.0.1:${port}/api/boxes`, { headers: authHeader() })).ok) return { srv, log: () => log }; } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  srv.kill();
  throw new Error(`server never came up on ${port}\n${log}`);
}

const results = [];
let page;
async function check(name, fn) {
  try { await fn(); results.push([true, name]); console.log(`  ok    ${name}`); }
  // The whole message, not its first line: this suite exists to show what a first run actually
  // says, and the evidence is usually the part after the colon.
  catch (e) { results.push([false, name]); console.log(`  FAIL  ${name}\n${String(e.message || e).split("\n").map(l => "        " + l).join("\n")}`); }
}
async function mustSee(sel, why) {
  const el = await page.$(sel);
  if (!el) throw new Error(`${why}: no element matches ${sel}`);
  const box = await el.boundingBox();
  if (!box || box.width === 0 || box.height === 0)
    throw new Error(`${why}: ${sel} is in the DOM but not visible`);
  return el;
}
const settle = (ms = 800) => page.waitForTimeout(ms);

// ---------- run ----------
const fx = makeFixture();
const port = await freePort();
const warden = await startWarden(fx);
const { srv, log } = await startServer(fx, port, warden.port);
const browser = await chromium.launch();
page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
page.setDefaultTimeout(5000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("response", r => { if (r.status() >= 500) noise.push(`[${r.status()}] ${r.url()}`); });

console.log("\nfirst run");
await page.goto(`http://127.0.0.1:${port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await settle(1500);

await check("a machine with nothing on it shows the first-run checklist", async () => {
  const list = await mustSee("#fleet .empty", "the first-run checklist");
  const body = (await list.textContent()).toLowerCase();
  if (!body.includes("repositor")) throw new Error(`the checklist never mentions adding a repo: ${body.slice(0, 200)}`);
});

// The checklist's own claim about the fleet. `sbx` here answers, so this step must read as done —
// it is the one step a new person cannot fix from inside the cockpit, so a false negative sends
// them installing something they already have.
await check("it says sbx is answering, because it is", async () => {
  const h = await (await fetch(`http://127.0.0.1:${port}/api/health`, { headers: authHeader() })).json();
  // `satisfied` specifically, not "not a fault": `unknown` here would mean sbx did not answer,
  // and the point of this step is that it did.
  if (h.sbx?.level !== "satisfied") throw new Error(`health says sbx is unusable on a machine where it answers: ${JSON.stringify(h.sbx)}`);
});

// A first run has no `sandboxes.json` and no `$SKEIN_REGISTRY`. That is not a fault, and reporting
// it as one used to be the first thing anyone saw.
await check("an absent legacy registry is not reported as a fault", async () => {
  const h = await (await fetch(`http://127.0.0.1:${port}/api/health`, { headers: authHeader() })).json();
  if (h.registry?.level === "unsatisfied")
    throw new Error(`a fresh install is told its registry is broken: ${h.registry.detail}`);
});

// "How do you even create a box without a repo?" — you could, by pressing Enter. The Launch button
// was disabled with no repos registered, and the branch field's Enter handler called `launchBox`
// directly, so the gate was on the one way in that a person about to type a branch name is least
// likely to use. The name then fell back to the literal "box", and `box-my-feature` belongs to no
// registered repo — refused by the launcher, after the dialog had closed and a dock tab had opened
// onto the refusal.
await check("with no repository, launching is refused rather than half-done", async () => {
  await page.evaluate(() => openNewBox());
  await settle(600);
  const disabled = await page.$eval("#nb-go", el => el.disabled);
  if (!disabled) throw new Error("the Launch button is live with nothing to launch into");
  await page.fill("#nb-branch", "my-feature");
  await page.press("#nb-branch", "Enter");
  await settle(1200);
  // Asserted on what the *cockpit* did, not on what reached the board: a box that fails to start
  // never appears on the board either, so "no such box" is true of the bug as well as the fix. What
  // separates them is that the buggy path closes the dialog, opens a dock tab and puts a terminal
  // on screen — a launch, all the way up to the refusal it was always going to hit.
  const opened = await page.evaluate(() => [...sessions.keys()]);
  if (opened.length) throw new Error(`Enter opened a terminal for ${opened.join(", ")} with no repo to launch into`);
  const stillOpen = await page.evaluate(() => newbox.classList.contains("open"));
  if (!stillOpen) throw new Error("the dialog closed as if the launch had been accepted");
  await page.evaluate(() => closeNewBox());
  await settle(300);
});

console.log("\nadding the first repo");
await check("the add-repo dialog takes a local path", async () => {
  await page.evaluate(() => openAddRepo());
  await settle(400);
  await mustSee("#addrepo.open, #addrepo", "the add-repo dialog");
  await page.fill("#ar-src", fx.src);
  await settle(200);
  await page.click("#ar-go");
  await settle(2500);
  // The fixture repo has no `origin`, so the dialog deliberately stays up with the note that boxes
  // for it cannot push — which is worth saying, and is exactly what a person adopting a local
  // checkout will meet. Read it, then dismiss it the way they would.
  const note = (await page.textContent("#ar-msg")) || "";
  if (!/origin/i.test(note)) throw new Error(`a repo with no remote was accepted silently: "${note.trim()}"`);
  await page.click("#ar-go");
  await settle(500);
  if (await page.evaluate(() => arModal().classList.contains("open")))
    throw new Error("the add-repo dialog would not close after it was done");
});

await check("the repo is registered", async () => {
  const repos = await (await fetch(`http://127.0.0.1:${port}/api/repos`, { headers: authHeader() })).json();
  if (!repos.length) throw new Error("adding a repo through the UI registered nothing");
  if (repos[0].id !== "my-project") throw new Error(`unexpected repo id ${repos[0].id}`);
});

console.log("\nlaunching the first box");
await check("the new-box dialog opens with the repo selected", async () => {
  await page.evaluate(() => openNewBox());
  await settle(1200);
  await mustSee("#newbox.open", "the new-box dialog");
  const id = await page.evaluate(() => currentRepoId());
  if (id !== "my-project") throw new Error(`the dialog would name the box after "${id}"`);
});

await check("the repo is on screen even though there is only one", async () => {
  const el = await mustSee("#nb-repo", "the repo select");
  const value = await el.inputValue();
  if (value !== "my-project") throw new Error(`the visible repo is ${value}`);
});

await check("the footer names the box that will be created, not a placeholder", async () => {
  await page.fill("#nb-branch", "main");
  await settle(300);
  const hint = (await page.textContent("#newbox .nb-foot .hint")) || "";
  if (!hint.includes("my-project-main"))
    throw new Error(`the hint never names the box: "${hint.trim()}"`);
});

// The fleet sandbox is the largest thing skein builds on the machine, and it used to appear as a
// side effect of this click, sized by a config default written for someone else's laptop. Nothing
// about it was ever shown, and sbx fixes all three at creation.
await check("the first launch asks what the fleet may take, before taking it", async () => {
  await page.evaluate(() => launchBox("main"));
  await settle(1500);
  await mustSee("#fleetnew.open", "the create-fleet dialog");
  const calls = fs.readFileSync(path.join(fx.root, "sbx.log"), "utf8");
  if (/^create /m.test(calls)) throw new Error("the sandbox was created before anyone confirmed it");
});

await check("and says what it is a share of", async () => {
  const shown = await page.evaluate(() => ({
    memory: document.getElementById("fn-memory").value,
    cpus: document.getElementById("fn-cpus").value,
    disk: document.getElementById("fn-disk").value,
    memOf: document.getElementById("fn-mem-of").textContent,
    cpuOf: document.getElementById("fn-cpu-of").textContent,
    diskOf: document.getElementById("fn-disk-of").textContent,
  }));
  for (const [field, value] of Object.entries(shown))
    if (!String(value).trim()) throw new Error(`${field} is blank — a size with no number to check it against`);
  // A proposal in sbx's own spelling, so what is on screen is what is passed.
  if (!/^\d+g$/.test(shown.memory)) throw new Error(`memory is not a size sbx takes: ${shown.memory}`);
  if (!/this machine has/.test(shown.memOf)) throw new Error(`memory does not say what it is a share of: ${shown.memOf}`);
});

await check("confirming creates it at the size that was on screen", async () => {
  await page.fill("#fn-memory", "6g");
  await page.fill("#fn-cpus", "2");
  await page.fill("#fn-disk", "24g");
  await page.click("#fn-go");
  await settle(3000);
  const calls = fs.readFileSync(path.join(fx.root, "sbx.log"), "utf8");
  const create = calls.split("\n").find(l => l.startsWith("create "));
  if (!create) throw new Error(`confirming created nothing:\n${calls.split("\n").slice(-8).join("\n")}`);
  if (!/-m 6g/.test(create)) throw new Error(`created at a size nobody chose: ${create}`);
  if (!/--cpus 2/.test(create)) throw new Error(`created with the wrong CPUs: ${create}`);
  const cfg = JSON.parse(fs.readFileSync(path.join(fx.root, "home", "config.json"), "utf8"));
  if (cfg.fleet_disk !== "24g") throw new Error(`the disk was not kept: ${JSON.stringify(cfg.fleet_disk)}`);
});

await check("launching does not refuse the box it just named", async () => {
  await settle(4000);
  // Whatever else happens, the one answer that means the flow is broken at the naming step is the
  // launcher refusing the name the dialog itself composed.
  const term = await page.evaluate(() => {
    const rows = document.querySelector(".xterm-rows");
    return rows ? rows.innerText : "";
  });
  const hit = term.split("\n").find(l => /belongs to no registered repo/.test(l));
  if (hit) throw new Error(`the cockpit named a box its own launcher will not accept:\n${hit.slice(0, 300)}`);
});

await check("the fleet sandbox is created, and by the warden rather than by skein", async () => {
  // The route matters as much as the result. Skein must not run `sbx create` itself — a fallback
  // that did would be the one taken on exactly the day something was wrong — so this asserts both
  // that a sandbox now exists and that the request for it was put to the warden.
  if (!warden.seen.some(call => call.includes("/v1/create")))
    throw new Error(`skein created the fleet without asking the warden: ${JSON.stringify(warden.seen)}`);
  const calls = fs.readFileSync(path.join(fx.root, "sbx.log"), "utf8");
  if (!/^create /m.test(calls))
    throw new Error(`launching the first box never ran \`sbx create\`:\n${calls.split("\n").slice(0, 12).join("\n")}`);
});

await check("and the box ends up on the board", async () => {
  for (let i = 0; i < 20; i++) {
    const boxes = await (await fetch(`http://127.0.0.1:${port}/api/boxes`, { headers: authHeader() })).json();
    if (boxes.some(b => b.name === "my-project-main")) return;
    await settle(1000);
  }
  const starts = path.join(fx.root, "home", "starts", "my-project-main.err");
  const why = fs.existsSync(starts) ? fs.readFileSync(starts, "utf8") : "(nothing recorded)";
  // What the create terminal itself said. The whole reason this suite exists is that the CLI is not
  // where onboarding happens, so the terminal's own text is the evidence, not a re-run by hand.
  const pane = await page.evaluate(() => {
    const rows = document.querySelector(".xterm-rows");
    return rows ? rows.innerText.split("\n").filter(l => l.trim()).slice(-14).join("\n") : "(no terminal on screen)";
  });
  const calls = fs.readFileSync(path.join(fx.root, "sbx.log"), "utf8").split("\n").slice(-10).join("\n");
  throw new Error(`the first box never appeared.\n--- terminal ---\n${pane}\n--- recorded ---\n${why}\n--- last sbx calls ---\n${calls}`);
});

await check("no page errors and no 5xx along the way", async () => {
  if (noise.length) throw new Error(noise.slice(0, 5).join("\n"));
});

const failed = results.filter(([ok]) => !ok);
if (failed.length) {
  console.log(`\nserver log:\n${log().split("\n").slice(-25).join("\n")}`);
}
console.log(failed.length ? `\n${failed.length} of ${results.length} checks failed` : `\nall ${results.length} checks passed`);
await browser.close();
srv.kill();
warden.server.close();
try { fs.rmSync(fx.root, { recursive: true, force: true }); } catch {}
process.exit(failed.length ? 1 : 0);
