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
import { spawnSync, execFileSync } from "node:child_process";
import http from "node:http";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { ledger, seeing, settler } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";

const API_TOKEN = "t".repeat(64);
const authHeader = () => ({ Authorization: `Bearer ${API_TOKEN}` });

// ---------- a machine that has never run skein ----------
function makeFixture() {
  // NOT under /tmp: a box binds its own /tmp over the sandbox's, so a fleet root there would be
  // unreadable from outside — skein refuses it, correctly, and the first draft of this fixture spent
  // a run learning that.
  //
  // Anything a previous run left behind goes first. The suite deletes its own fixture on the way
  // out, so what survives is from a run that crashed or was interrupted — and each one is up to
  // ~180MB of cloned repo. Fifteen of them had accumulated to 356MB before anybody looked.
  //
  // That sweep used to justify itself with "somebody inspecting a kept fixture is not
  // simultaneously starting a new run", which stopped being true the moment the root became one
  // shared directory rather than a per-worktree `$CARGO_TARGET_DIR`. It is keyed on the pid now;
  // the argument is in `freshFixture`.
  //
  // It also used to read `targetDir()` rather than `<repo>/target` (UI-4), because
  // `$CARGO_TARGET_DIR` is the normal state on this box and on any CI with a cached target dir.
  // That reason held and the path still lost — see below.
  //
  // `fixtureRoot()` answers `$SKEIN_UI_FIXTURE_ROOT` or `/var/tmp/skein-uifix`, and NOT
  // `targetDir()` any more (SKEIN-603): a worktree path in this fleet is ~118 characters, so the
  // projection below could never fit under 108 from one, and every agent that touched this suite
  // spent the same twenty minutes discovering the same variable. Same rules either way — outside
  // `/tmp` and outside the box's `$HOME`, because `src/box-session.sh:577` refuses a fleet root
  // under either.
  const target = fixtureRoot();
  // **A unix socket path is 108 bytes, and this fixture builds one of the longest skein makes.**
  //
  // The box's tmux socket is `<root>/fleet/<box>/session.sock`, so a deep `$CARGO_TARGET_DIR` — the
  // normal state on this box and on any CI with a cached target dir — puts it over the limit. The
  // launcher then dies with `error connecting to … (File name too long)`, and what a reader sees is
  // two checks failing about the BOARD and about PROVISIONING, on a machine where both work: an
  // environment limit wearing a product bug's clothes. Said here, once, in the words of the actual
  // cause. `mkdtemp` adds six characters to the prefix, which is why the projection is built rather
  // than measured off `root` (it does not exist yet).
  const projected = path.join(target, `ui-onboard-${process.pid}-XXXXXX`, "fleet", "my-project-main", "session.sock");
  if (Buffer.byteLength(projected) > 100) {
    throw new Error(
      `this fixture's box socket would be ${Buffer.byteLength(projected)} bytes and a unix socket `
      + `path is limited to 108:\n  ${projected}\nThe fixture root comes from $SKEIN_UI_FIXTURE_ROOT, `
      + `defaulting to /var/tmp/skein-uifix. Point it somewhere shorter — it must be outside /tmp `
      + `and outside the box's $HOME, which src/box-session.sh refuses.`);
  }
  // The sweep is pid-keyed and lives in `lift.mjs` — the root is now shared by every worktree on
  // the box, so removing everything that matches the prefix would delete a fixture another run is
  // writing into. See `freshFixture`, and SKEIN-590.
  const root = freshFixture(target, "ui-onboard");
  // The repo the person is going to register.
  //
  // **A remote, because that is the only kind skein takes.** This fixture used to hand the dialog a
  // local checkout — `skein add <path>` adopted one in place — and that capability is gone:
  // `repos::add_repo` refuses a path outright ("skein runs inside the fleet sandbox and cannot reach
  // a checkout on your machine, so a path-registered repo has nothing to fetch from"), and
  // `clone_mirror` no longer has a checkout to prefer over the remote. So the checkout below is not
  // what gets registered; it is what the remote is made FROM.
  const src = path.join(root, "my-project");
  fs.mkdirSync(src, { recursive: true });
  fs.writeFileSync(path.join(src, "README.md"), "# my project\n");
  const git = (...a) => spawnSync("git", ["-C", src, ...a], { stdio: "ignore" });
  git("init", "-q", "-b", "main");
  git("add", "-A");
  git("-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "init");

  // The bare repository `startGitHost` serves, and the reason this suite can reach the end at all:
  // `add_repo` clones the mirror INLINE before it registers anything, so a URL that does not really
  // answer fails the add rather than merely being unfetchable later. `update-server-info` is what
  // makes it clonable over plain static HTTP — git's "dumb" protocol — which is the whole of the
  // server in `startGitHost`. No git CGI, no daemon, nothing on `$PATH` beyond git itself.
  const bare = path.join(root, "my-project.git");
  spawnSync("git", ["clone", "-q", "--bare", src, bare], { stdio: "ignore" });
  spawnSync("git", ["-C", bare, "update-server-info"], { stdio: "ignore" });

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
  #
  # **With a HOME of its own.** The real \`sbx exec\` runs inside a sandbox, where HOME belongs to
  # that sandbox; here it runs on the machine the test is running on, so without this the scripts
  # skein sends into a box run against the REAL home of whoever is running the suite. That is not a
  # theoretical leak: provisioning links \$HOME/shared at the store it was given, so this suite
  # repointed the shared workspace of the box it ran in at a fixture under target/, and left it
  # dangling when the fixture was cleaned up. Every run did it again.
  exec)    exec env HOME=${path.join(root, "guest-home")} bash -c "\${@: -1}" ;;
  ports)   exit 0 ;;
esac
exit 0
`);
  fs.chmodSync(sbx, 0o755);
  // The home a script sent into a "box" sees. Its own directory, so anything provisioning writes
  // into HOME lands here and can be asserted, instead of in the home of whoever ran the suite.
  fs.mkdirSync(path.join(root, "guest-home"));
  fs.mkdirSync(path.join(root, "home"));
  // The API token, written rather than read back: the server mints one at first use, and racing
  // that would make this flaky for a reason unrelated to onboarding. The auth path is still walked
  // end to end — the browser trades `?t=` for a cookie exactly as a person does.
  fs.writeFileSync(path.join(root, "home", "api-token"), API_TOKEN, { mode: 0o600 });
  return { root, src, bare, bin, sbx, state };
}

// The remote the person pastes into the dialog, served off the bare repo in the fixture.
//
// Static files and nothing else. Git's "dumb" HTTP protocol is exactly that: ask for `info/refs`,
// then for the loose objects and packs it names, all of which `update-server-info` has already
// written out. The clone `add_repo` runs is therefore real — a real `git clone --mirror` over a real
// socket — without a network, a git CGI, or a credential anywhere in it.
//
// Bound on 127.0.0.1:0 like everything else here, so two lanes cannot collide on a port.
function startGitHost(fx) {
  const server = http.createServer((req, res) => {
    // Only ever below the bare repo. `path.join` on a `..` would climb out of it, and a fixture
    // that serves the filesystem is a fixture nobody should run.
    const rel = decodeURIComponent(req.url.split("?")[0]).replace(/^\/my-project\.git/, "");
    const file = path.resolve(fx.bare, "." + rel);
    if (!file.startsWith(path.resolve(fx.bare) + path.sep)) { res.writeHead(403); return res.end(); }
    fs.readFile(file, (err, body) => {
      if (err) { res.writeHead(404); return res.end(); }
      res.writeHead(200, { "content-type": "application/octet-stream", "content-length": body.length });
      res.end(body);
    });
  });
  return new Promise(res => {
    server.listen(0, "127.0.0.1", () =>
      res({ server, url: `http://127.0.0.1:${server.address().port}/my-project.git` }));
  });
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

// `whole`: this suite exists to show what a first run actually SAYS, and the evidence is usually
// the part of a failure after the colon — so a message is printed entire, not clipped to line one.
const { check, results, report } = ledger({ whole: true });
// Bound to the page below, once it exists.
let page, mustSee, settle;

// ---------- run ----------
const fx = makeFixture();
const door = await openDoor();
const port = door.port;
const warden = await startWarden(fx);
const githost = await startGitHost(fx);
const { srv, log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_HOME: path.join(fx.root, "home"),
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_NO_GH_SECRET: "1",
    // Where the warden is. Without this skein looks at the default 127.0.0.1:7879 — which on the
    // machine running these tests is either nothing or, worse, somebody's real warden.
    SKEIN_WARDEN: `127.0.0.1:${warden.port}`,
    PATH: `${fx.bin}:${process.env.PATH}`,
    // Deliberately NOT set: SKEIN_REGISTRY. A new machine has no `sandboxes.json`, and the
    // fallback hunts for a sibling `skein-shared/` named after another project entirely — the
    // exact state in which a first run used to declare itself broken.
    SKEIN_REGISTRY: "",
    SKEIN_SHARED: "",
    // The clone below goes to `githost`, on loopback. Anyone working inside a box has an
    // `http_proxy` pointing at the sandbox's egress proxy, and git honours it — so without this the
    // fixture's own remote is fetched via somebody's proxy, which either fails or, worse, does not.
    // Belt and braces with the `no_proxy` a box already sets, because CI sets neither.
    NO_PROXY: "127.0.0.1,localhost,::1",
    no_proxy: "127.0.0.1,localhost,::1",
  },
});
const browser = await chromium.launch();
page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
mustSee = seeing(page);
settle = settler(page, 800);
page.setDefaultTimeout(5000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("response", r => { if (r.status() >= 500) noise.push(`[${r.status()}] ${r.url()}`); });

// What the runner's own shared workspace pointed at before any of this ran, so the check at the end
// has something to compare against. Read here rather than there: by then the fixture has been driven
// through box creation, which is the thing that used to move it.
const OWN_SHARED = (() => {
  try { return fs.readlinkSync(path.join(process.env.HOME || "", "shared")); } catch { return null; }
})();

console.log("\nfirst run");
await page.goto(`http://127.0.0.1:${port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await settle(1500);

await check("a machine with nothing on it shows the first-run checklist", async () => {
  const list = await mustSee("#fleet .empty", "the first-run checklist");
  const body = (await list.textContent()).toLowerCase();
  if (!body.includes("repositor")) throw new Error(`the checklist never mentions adding a repo: ${body.slice(0, 200)}`);
});

// **The warden is on the checklist, and it gates.**
//
// Creating the fleet is what a first Launch does, and that goes only through the warden with no
// fallback. Without this step the checklist read "ready", somebody pressed the button, and got a
// 500 — a first run that says it is ready and then is not is the exact thing this page exists to
// prevent. This fixture runs a warden, so the step must read as done and the list must offer the
// launch; the assertion below it is the one that would have caught the gap.
await check("the checklist counts the warden, and says so when it is up", async () => {
  const h = await (await fetch(`http://127.0.0.1:${port}/api/health`, { headers: authHeader() })).json();
  if (h.warden?.level !== "satisfied")
    throw new Error(`this fixture runs a warden and health disagrees: ${JSON.stringify(h.warden)}`);
  const body = (await (await mustSee("#fleet .empty", "the first-run checklist")).textContent()).toLowerCase();
  if (!body.includes("warden"))
    throw new Error(`the checklist never mentions the warden, so a first run can still reach a 500: ${body.slice(0, 300)}`);
  // Done, not outstanding — a step that reads unfinished when it is finished sends somebody to start
  // a second warden, and two on one port is a worse afternoon than none.
  if (/start the warden/.test(body))
    throw new Error(`the warden is answering and the checklist still asks for it: ${body.slice(0, 300)}`);
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
// **This check used to paste a local path, and it was measuring nothing.**
//
// Adopting a checkout was removed with local-path repos, so the POST was refused every time — and
// the refusal's own advice ("`git -C <path> remote get-url origin` prints it") contains the word
// `origin`, which is the only thing the assertion below it looked for. So a hard error read as the
// expected soft warning, the suite walked on, and the ten checks after it failed on a repo that had
// never been registered. A test can be wrong in the direction of passing, and this one was.
//
// What a new person actually does now is paste a URL, so that is what this drives.
await check("the add-repo dialog takes a git URL", async () => {
  await page.evaluate(() => openAddRepo());
  await settle(400);
  await mustSee("#addrepo.open, #addrepo", "the add-repo dialog");
  await page.fill("#ar-src", githost.url);
  await settle(200);
  // Named before it is created, off the URL alone: the box names a person is about to live with are
  // on screen before they commit to anything.
  const derived = (await page.textContent("#ar-derived")) || "";
  if (!derived.includes("my-project"))
    throw new Error(`the dialog never says what the boxes will be called: "${derived.trim()}"`);
  await page.click("#ar-go");
  // The clone is real, and `add_repo` runs it inline before it registers anything.
  await settle(4000);
  const note = (await page.textContent("#ar-msg")) || "";
  // A clean add says nothing and closes itself; anything left in the message box is the failure,
  // and printing it is the difference between a diagnosis and "it did not work".
  if (await page.evaluate(() => arModal().classList.contains("open")))
    throw new Error(`the add-repo dialog would not close after it was done: "${note.trim()}"`);
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

await check("provisioning ran in the box's home, not in the home of whoever ran this", () => {
  // The fake `sbx exec` runs the scripts skein sends into a box on THIS machine — there is no
  // sandbox here to enter. So it runs them with a HOME of its own, and this is the assertion that
  // says so: box provisioning links `$HOME/shared` at the store it was given, so without that the
  // suite repoints the shared workspace of the box it is running IN at a fixture under `target/` —
  // and then deletes the fixture on the way out, leaving a dangling symlink. Every run did it
  // again, and nothing said a word.
  const guest = path.join(fx.root, "guest-home", "shared");
  if (!fs.existsSync(guest)) {
    throw new Error("nothing linked a shared workspace in the box's home — did provisioning run?");
  }
  const at = fs.readlinkSync(guest);
  if (!at.startsWith(fx.root)) {
    throw new Error(`the box's shared workspace points outside the fixture: ${at}`);
  }
  // And the runner's own is exactly as it was.
  let mine = null;
  try { mine = fs.readlinkSync(path.join(process.env.HOME || "", "shared")); } catch {}
  if (mine !== OWN_SHARED) {
    throw new Error(
      `this suite repointed the shared workspace of the box it ran in: ${OWN_SHARED} -> ${mine}`,
    );
  }
});

await check("no page errors and no 5xx along the way", async () => {
  if (noise.length) throw new Error(noise.slice(0, 5).join("\n"));
});

const failed = report({ log });
await browser.close();
srv.kill();
warden.server.close();
githost.server.close();
// The boxes this run launched, before the directory holding their sockets goes.
//
// A box deliberately outlives the skein that started it — that is the whole point of the tmux
// session being the box — so killing the server does not end one, and deleting its socket does not
// either: tmux holds the open file, and the session sits there idle for ever with nothing left
// that could ever reach it. Cheap one at a time and invisible, which is how 126 of them
// accumulated on one machine beside the 105 spinning supervisors (docs/TODO.md).
//
// Deliberately not `skein stop`: the server is already dead by here, and a box whose fleet is
// about to be deleted does not need an orderly stop, it needs to not exist. `kill-server` ends
// every process in the session, which is exactly what removing the directory assumes has happened.
for (const sock of fs.globSync(path.join(fx.root, "fleet", "*", "session.sock"))) {
  try { spawnSync("tmux", ["-S", sock, "kill-server"], { stdio: "ignore" }); } catch {}
}
try { fs.rmSync(fx.root, { recursive: true, force: true }); } catch {}
process.exit(failed.length ? 1 : 0);
