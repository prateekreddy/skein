// What the page says when something it asked for goes wrong, or takes a while.
//
// Five reports from the 2026-10-22 sweep, each about feedback rather than about the thing done:
//
// * **A refusal's reason** (SKEIN-1299). The same 500 read "couldn't save: Internal Server Error"
//   in Settings while its body said "Read-only file system", "could not record that decision" in
//   Questions and Package requests, and "could not load diff" in Diff. `refused` (boot.js) is the
//   one reader now. And Files showed a blank pane while it loaded, where Diff said it was asking.
// * **Where an error is said** (SKEIN-1265, SKEIN-1245). A failure went into a toast that hid
//   after 3.5 seconds and, at several call sites, was cut to its first line. An error — and the
//   agent-CLI update's several-line report — is a notice now (`notice`, boot.js), which keeps
//   every line and stays until it is closed.
// * **A second click** (SKEIN-1296). "Send answer" and "Approve & install" stayed live while their
//   request was in flight, and a double click sent it twice; an approval installs fleet-wide.
// * **A refusal taken for success** (SKEIN-1266). New box launched with defaults when the server
//   refused a choice made in the dialog, and Box settings said nothing about which of its five
//   writes had failed.
//
// Every request whose answer matters is a stubbed route, so the test decides what the server says
// and when; every write under `/api/fleet/` and `/api/boxes/` is answered by a stand-in rather than
// the server (SKEIN-1290).
//
//   node tests/ui/feedback.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const BOX = "feedback-box";
const API_TOKEN = "f".repeat(64);

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "skein-feedback-ui");
  const home = path.join(root, "home");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(path.join(root, "fleet"), { recursive: true });
  fs.writeFileSync(path.join(home, "config.json"), "{}");
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  fs.mkdirSync(path.join(root, "work"));
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    { id: "acme", source: "https://github.com/acme/thing.git", source_tree: path.join(root, "work"), read_prs: false,
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, "#!/usr/bin/env bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
  fs.chmodSync(sbx, 0o755);
  fs.writeFileSync(path.join(bin, "gh"), "#!/bin/sh\nexit 1\n", { mode: 0o755 });
  return { root, home, bin, sbx };
}

const { check, report } = ledger();

const fx = makeFixture();
const door = await openDoor();
const origin = `http://127.0.0.1:${door.port}`;
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,
    PATH: `${fx.bin}:${process.env.PATH}`,
    GH_TOKEN: "",
    SKEIN_GITHUB_API: "http://127.0.0.1:9",
  },
});

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1400, height: 900 } });
const page = await context.newPage();
page.setDefaultTimeout(5000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
// No terminal reaches a server from here: a box's socket is stood in for.
await page.routeWebSocket(/\/api\/boxes\/[^/]+\/terminal/, () => {});
// Every write under the fleet and the boxes is a stand-in's unless a check routes it itself.
await page.route(/\/api\/(fleet|boxes)\//, r => r.request().method() === "GET" ? r.fallback() : r.fulfill({ json: { ok: true } }));

await page.goto(`${origin}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await page.waitForFunction(() => typeof openSettings === "function" && typeof boxes !== "undefined");

const closeEverything = () => page.evaluate(() => {
  for (const o of document.querySelectorAll(".open[aria-hidden], #away.open, #load.open")) o.classList.remove("open");
  document.getElementById("notice")?.classList.remove("show");
});
/** Everything the page has said lately: the toast and the notice, as text. */
const said = () => page.evaluate(() => ["toast", "notice"].map(id => document.getElementById(id))
  .filter(el => el && el.classList.contains("show")).map(el => el.textContent).join(" | "));
const until = (fn, ms = 3000) => page.waitForFunction(fn, null, { timeout: ms });
const saying = text => page.waitForFunction(t => ["toast", "notice"].some(id => {
  const el = document.getElementById(id);
  return el && el.classList.contains("show") && el.textContent.includes(t);
}), text, { timeout: 3000 }).then(() => true, () => false);

// ---------- a refusal's reason (SKEIN-1299) ----------
console.log("\na refusal says why");

// Fails if `saveSettingsQuiet` throws `r.statusText` again: the toast says "Internal Server Error".
await check("a refused Settings save says the server's reason", async () => {
  await closeEverything();
  await page.route("**/api/settings", r => r.request().method() === "POST"
    ? r.fulfill({ status: 500, body: "Read-only file system" }) : r.fallback());
  try {
    await page.evaluate(() => openSettings("github"));
    await until(() => settingsModal().classList.contains("open"));
    await page.locator("#set-ghappid").fill("77");
    await page.locator("#set-go").click();
    if (!(await saying("Read-only file system"))) throw new Error(`said ${JSON.stringify(await said())}`);
  } finally {
    await page.unroute("**/api/settings");
    await page.evaluate(() => discardSettings());
  }
});

const asks = [{ id: "q-1", box: BOX, question: "Which port?", asked: "2026-10-22T10:00:00Z", state: "waiting" }];
const pkgs = [{ id: "p-1", box: BOX, kind: "apt", packages: ["libnss3"], asked: "2026-10-22T10:00:00Z", state: "pending" }];
await page.route("**/api/fleet/asks", r => r.fulfill({ json: asks }));
await page.route("**/api/fleet/substrate", r => r.request().method() === "GET" ? r.fulfill({ json: pkgs }) : r.fallback());

// Fails if `sendAskq` goes back to `throw 0` and a fixed sentence.
await check("a refused answer to a question says the server's reason", async () => {
  await closeEverything();
  await page.route(/\/api\/fleet\/asks\/[^/]+$/, r => r.fulfill({ status: 500, body: "the box's drop-box is full" }));
  try {
    await page.evaluate(() => openAskq());
    const reply = page.locator(`[id="aq-reply-${BOX}/q-1"]`);
    await reply.fill("8080");
    await page.locator("#askq button", { hasText: "Send answer" }).click();
    if (!(await saying("the box's drop-box is full"))) throw new Error(`said ${JSON.stringify(await said())}`);
  } finally { await page.unroute(/\/api\/fleet\/asks\/[^/]+$/); }
});

// Fails if `decideSubq` goes back to `throw 0` and a fixed sentence.
await check("a refused package approval says the server's reason", async () => {
  await closeEverything();
  await page.route(/\/api\/fleet\/substrate\/[^/]+$/, r => r.fulfill({ status: 409, body: "apt is busy in the sandbox" }));
  try {
    await page.evaluate(() => openSubq());
    await page.locator("#subq button", { hasText: "Approve" }).click();
    if (!(await saying("apt is busy in the sandbox"))) throw new Error(`said ${JSON.stringify(await said())}`);
  } finally { await page.unroute(/\/api\/fleet\/substrate\/[^/]+$/); }
});

// Fails if `loadDiff` parses a refusal as JSON and lands in its fixed "could not load diff".
await check("a refused diff says the server's reason in the pane", async () => {
  await closeEverything();
  await page.route(`**/api/boxes/${BOX}/diff`, r => r.fulfill({ status: 503, body: "the box is not running" }));
  try {
    await page.evaluate(box => openDiff(box), BOX);
    await until(() => document.getElementById("diffpane").textContent.includes("the box is not running"));
  } finally { await page.unroute(`**/api/boxes/${BOX}/diff`); }
});

// Fails if `loadFiles` leaves the pane as it was while it asks: it is blank, or the last box's.
await check("Files says it is asking the box while it waits", async () => {
  let release;
  const held = new Promise(res => { release = res; });
  await page.route(`**/api/boxes/${BOX}/files*`, async r => { await held; return r.fulfill({ json: { path: "", entries: [] } }); });
  try {
    await page.evaluate(box => openFiles(box), BOX);
    await until(() => document.getElementById("filespane").textContent.includes("asking the box"), 1500);
  } finally { release(); await page.unroute(`**/api/boxes/${BOX}/files*`); }
});

// ---------- a second click (SKEIN-1296) ----------
console.log("\na decision is sent once");

/** Hold every POST to `pattern` for 1s, counting them, answering ok. */
const holdPosts = async pattern => {
  const posts = [];
  await page.route(pattern, async r => {
    if (r.request().method() !== "POST") return r.fallback();
    posts.push(1);
    await new Promise(res => setTimeout(res, 1000));
    return r.fulfill({ json: {} });
  });
  return posts;
};
const cardState = (panel, button) => page.evaluate(([p, b]) => {
  const btn = [...document.querySelectorAll(`#${p}-list button`)].find(x => x.textContent.includes(b));
  const note = document.querySelector(`#${p}-list .sq-sending`);
  return { disabled: !!btn?.disabled, sending: !!note && !note.hidden && note.getClientRects().length > 0 };
}, [panel, button]);

// Fails if `sendAskq` loses its in-flight guard (two POSTs), or the card is not drawn from the
// in-flight set (the poll in the middle brings the button back live).
await check("a double click on Send answer sends one answer, and the card says sending until it lands", async () => {
  await closeEverything();
  const posts = await holdPosts(/\/api\/fleet\/asks\/[^/]+$/);
  try {
    await page.evaluate(() => openAskq());
    await page.locator(`[id="aq-reply-${BOX}/q-1"]`).fill("8080");
    await page.locator("#askq button", { hasText: "Send answer" }).dblclick();
    await page.evaluate(() => pollAskq());
    await page.waitForTimeout(300);
    const during = await cardState("askq", "Send answer");
    await page.waitForTimeout(1200);
    if (posts.length !== 1) throw new Error(`${posts.length} answers sent`);
    if (!during.disabled || !during.sending) throw new Error(`while sending: ${JSON.stringify(during)}`);
  } finally { await page.unroute(/\/api\/fleet\/asks\/[^/]+$/); }
});

// Fails if `decideSubq` loses its in-flight guard: a double click approves twice.
await check("a double click on Approve & install sends one approval, and the card says sending", async () => {
  await closeEverything();
  const posts = await holdPosts(/\/api\/fleet\/substrate\/[^/]+$/);
  try {
    await page.evaluate(() => openSubq());
    await page.locator("#subq button", { hasText: "Approve" }).dblclick();
    await page.waitForTimeout(300);
    const during = await cardState("subq", "Approve");
    await page.waitForTimeout(1200);
    if (posts.length !== 1) throw new Error(`${posts.length} approvals sent`);
    if (!during.disabled || !during.sending) throw new Error(`while sending: ${JSON.stringify(during)}`);
  } finally { await page.unroute(/\/api\/fleet\/substrate\/[^/]+$/); }
});

// ---------- a refusal is not a success (SKEIN-1266) ----------
console.log("\na refusal stops a launch and a save says which part");

// Fails if `launchBox` goes back to catching only network errors: the box launches (a session
// for it opens, the dialog closes) with the refused choice silently dropped. The second half is
// the way back: the same launch with the choice accepted does go through.
await check("New box with a refused choice does not launch, says why, and launches once it is accepted", async () => {
  await closeEverything();
  const name = "acme-skein-test-refused";
  let refuse = true;
  await page.route(`**/api/boxes/${name}/git-scope`, r => refuse
    ? r.fulfill({ status: 400, body: "no GitHub App or write token to scope this box with" })
    : r.fulfill({ json: { ok: true } }));
  try {
    await page.evaluate(() => openNewBox());
    await until(() => document.getElementById("nb-gitscope").options.length > 1);
    await page.locator("#nb-branch").fill("skein-test-refused");
    await page.locator("#nb-gitscope").selectOption("repo");
    await page.locator("#nb-go").click();
    await until(() => document.getElementById("nb-msg").textContent.includes("Not launched"));
    const after = await page.evaluate(n => ({
      open: document.getElementById("newbox").classList.contains("open"),
      session: sessions.has(sidOf(n, "agent")),
      said: document.getElementById("nb-msg").textContent,
      branch: document.getElementById("nb-branch").value,
    }), name);
    if (!after.open || after.session || !after.said.includes("github access — no GitHub App or write token") || after.branch !== "skein-test-refused") {
      throw new Error(JSON.stringify(after));
    }
    refuse = false;
    await page.locator("#nb-go").click();
    await until(() => !document.getElementById("newbox").classList.contains("open"));
    if (!(await page.evaluate(n => sessions.has(sidOf(n, "agent")), name))) throw new Error("accepted, but nothing launched");
    await page.evaluate(n => closeSession(sidOf(n, "agent")), name);
  } finally { await page.unroute(`**/api/boxes/${name}/git-scope`); }
});

// Fails if `saveBoxSettings` goes back to one Promise.all and a toast: the dialog closes, and
// nothing says the other four saved or which one did not.
await check("Box settings with one refused write stays open and says which saved and which did not", async () => {
  await closeEverything();
  await page.route(`**/api/boxes/${BOX}/disk`, r => r.fulfill({ status: 400, body: "not a size: lots" }));
  try {
    await page.evaluate(box => openBoxSettings(box), BOX);
    await until(() => !document.getElementById("bs-go").disabled);
    await page.locator("#bs-disk").fill("lots");
    await page.locator("#bs-go").click();
    await until(() => document.getElementById("bs-msg").textContent.includes("Not saved"));
    const said = await page.evaluate(() => document.getElementById("bs-msg").textContent);
    if (!(await page.evaluate(() => document.getElementById("boxset").classList.contains("open")))) throw new Error("closed");
    if (!said.includes("disk allowance — not a size: lots") || !/Saved: work tracking, commit as, github access, workshop box/.test(said)) {
      throw new Error(JSON.stringify(said));
    }
  } finally { await page.unroute(`**/api/boxes/${BOX}/disk`); }
});

// Fails if `openBoxSettings` waits for its loads before adding `open` again: nothing is on screen
// while they are held.
await check("Box settings opens at once and says it is loading, with Save off until it has loaded", async () => {
  await closeEverything();
  let release;
  const held = new Promise(res => { release = res; });
  await page.route(`**/api/boxes/${BOX}/settings`, async r => { if (r.request().method() === "GET") await held; return r.fallback(); });
  try {
    await page.evaluate(box => openBoxSettings(box), BOX);
    await page.waitForTimeout(300);
    const during = await page.evaluate(() => ({
      open: document.getElementById("boxset").classList.contains("open"),
      said: document.getElementById("bs-msg").textContent,
      save: document.getElementById("bs-go").disabled,
    }));
    release();
    await until(() => !document.getElementById("bs-go").disabled);
    if (!during.open || !during.said.includes("asking") || !during.save) throw new Error(JSON.stringify(during));
  } finally { release(); await page.unroute(`**/api/boxes/${BOX}/settings`); await closeEverything(); }
});

// ---------- an error stays, every line of it (SKEIN-1265, SKEIN-1245) ----------
console.log("\nan error or a report stays until it is closed");

/** The notice as a person sees it: shown, its kind, and how many lines its text takes on screen. */
const noticeNow = () => page.evaluate(() => {
  const n = document.getElementById("notice");
  if (!n || !n.classList.contains("show") || !n.getClientRects().length) return null;
  const t = n.querySelector(".nt-text");
  const lh = parseFloat(getComputedStyle(t).lineHeight) || 16;
  return { kind: n.className.replace("show", "").trim(), text: t.textContent, lines: Math.round(t.getBoundingClientRect().height / lh) };
});

// SKEIN-1333, save and warn: a tracking change whose old token could not be revoked goes through
// and answers 200 with a `warning`. Fails if `saveBoxSettings`' `post` goes back to ignoring the
// body (no notice, only "settings saved"), or if the warning keeps the dialog open as a refusal.
await check("a tracking change that saved with a warning closes Box settings and says the warning", async () => {
  await closeEverything();
  const warning = `${BOX}'s work tracking is changed, but the token old gave it could not be revoked (the gateway could not be reached). skein will try again when that tracker answers; to be sure now, revoke the agent ${BOX} there by hand.`;
  await page.route(`**/api/boxes/${BOX}/tracking`, r => r.fulfill({ status: 200, json: { ok: true, warning } }));
  try {
    await page.evaluate(box => openBoxSettings(box), BOX);
    await until(() => !document.getElementById("bs-go").disabled);
    await page.locator("#bs-go").click();
    await until(() => !document.getElementById("boxset").classList.contains("open"));
    const said = await noticeNow();
    if (!said || !said.text.includes("revoke the agent") || said.kind !== "error") throw new Error(JSON.stringify(said));
  } finally { await page.unroute(`**/api/boxes/${BOX}/tracking`); await closeEverything(); }
});

// Fails if a refusal goes back to the toast (gone after 3.5s), if the notice drops its line breaks
// (`white-space` back to normal: one line), or if a later toast takes it away.
await check("a refused save is said in a notice that keeps every line and outlives the toast", async () => {
  await closeEverything();
  await page.route("**/api/settings", r => r.request().method() === "POST"
    ? r.fulfill({ status: 500, body: "the config could not be written\nRead-only file system\nfree some space and save again" }) : r.fallback());
  try {
    await page.evaluate(() => openSettings("github"));
    await until(() => settingsModal().classList.contains("open"));
    await page.locator("#set-ghappid").fill("77");
    await page.locator("#set-go").click();
    await until(() => document.getElementById("notice")?.classList.contains("show"));
    // And it does not sit on the dialog's own Save, which is how to act on what it says. Fails if
    // the notice goes back to the bottom right corner.
    const onSave = await page.evaluate(() => {
      const r = document.getElementById("set-go").getBoundingClientRect();
      return document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2)?.id;
    });
    if (onSave !== "set-go") throw new Error(`Save is covered by ${onSave}`);
    await page.evaluate(() => toast("something else, later"));
    await page.waitForTimeout(4500);
    const n = await noticeNow();
    if (!n || n.kind !== "error" || !n.text.includes("free some space and save again") || n.lines < 3) {
      throw new Error(`notice ${JSON.stringify(n)}`);
    }
    // And its ✕ closes it — the one way it goes.
    await page.locator("#notice .x").click();
    if (await noticeNow()) throw new Error("the ✕ did not close it");
  } finally {
    await page.unroute("**/api/settings");
    await page.evaluate(() => discardSettings());
  }
});

// The agent-CLI update's report (SKEIN-1245). Fails if `pressUpdateAgents` toasts it again: no
// notice, or one line, or gone after 3.5 seconds.
await check("the agent-CLI update's report stays on screen, one line per CLI, until closed", async () => {
  await closeEverything();
  const text = "claude: 2.1.0 -> 2.1.4\ncodex: 0.9.1 (already current)\nboxes pick the new one up at their next restart";
  await page.route("**/api/update-agents", r => r.fulfill({ json: { ok: true, text } }));
  try {
    await page.evaluate(() => {
      const b = document.createElement("button"); b.id = "skein-test-upd"; document.body.append(b);
      return pressUpdateAgents(b);
    });
    await page.waitForTimeout(4000);
    const n = await noticeNow();
    if (!n || n.kind !== "report" || n.text !== text || n.lines < 3) throw new Error(`notice ${JSON.stringify(n)}`);
    await page.locator("#notice .x").click();
  } finally {
    await page.unroute("**/api/update-agents");
    await page.evaluate(() => document.getElementById("skein-test-upd")?.remove());
  }
});

// Fails if `pressUpdateSkein` toasts a refused start again: no notice, or gone after 3.5s.
await check("a refused Update skein says why in a notice that stays", async () => {
  await closeEverything();
  await page.route("**/api/update/start", r => r.fulfill({ json: { ok: false, error: "an update is already running\nwait for it to finish" } }));
  try {
    await page.evaluate(() => pressUpdateSkein());
    await page.waitForTimeout(4000);
    const n = await noticeNow();
    if (!n || n.kind !== "error" || !n.text.includes("wait for it to finish")) throw new Error(`notice ${JSON.stringify(n)}`);
    await page.locator("#notice .x").click();
  } finally { await page.unroute("**/api/update/start"); }
});

// Stop, pressed twice while the first is in flight, and refused (SKEIN-1265). Fails if `stopBox`
// loses its in-flight guard (two POSTs), or cuts the refusal to its first line again.
await check("Stop pressed twice sends one request, and its refusal is said whole", async () => {
  await closeEverything();
  const stops = [];
  await page.route(`**/api/boxes/${BOX}/stop`, async r => {
    stops.push(1);
    await new Promise(res => setTimeout(res, 800));
    return r.fulfill({ json: { ok: false, error: "sbx stop failed\nthe sandbox is busy resizing; try again in a minute" } });
  });
  const accept = d => d.accept().catch(() => {});
  page.on("dialog", accept);
  try {
    await page.evaluate(box => { stopBox(box); stopBox(box); }, BOX);
    await until(() => document.getElementById("notice")?.classList.contains("show"), 3000);
    const n = await noticeNow();
    if (stops.length !== 1) throw new Error(`${stops.length} stop requests`);
    if (!n || !n.text.includes("try again in a minute")) throw new Error(`notice ${JSON.stringify(n)}`);
    await page.locator("#notice .x").click();
  } finally {
    page.off("dialog", accept);
    await page.unroute(`**/api/boxes/${BOX}/stop`);
  }
});

// ---------- a stop or destroy in flight, and what Destroy would lose (SKEIN-1313) ----------
console.log("\nstop and destroy, while they run");

// The box's row, drawn by the real `render`; the next snapshot from the server takes it away again,
// so each read draws it first. `pill` is the state word on the right of the row.
const ROW = { name: BOX, repo: "acme", state: "working", agent: "claude" };
const pillNow = () => page.evaluate(row => {
  render([row]);
  return document.querySelector(`#fleet [data-name="${row.name}"] .pill`)?.textContent.trim() ?? null;
}, ROW);

// Fails if `updateRow` stops reading `boxActing` (the row says "working" throughout), or the set is
// not cleared and the row refreshed when the stop answers (it says "stopping…" for ever).
await check("a box's row says stopping… while its stop is in flight, and its state once it answers", async () => {
  await closeEverything();
  let answer;
  const answered = new Promise(res => { answer = res; });
  await page.route(`**/api/boxes/${BOX}/stop`, async r => { await answered; return r.fulfill({ json: { ok: true } }); });
  const accept = d => d.accept().catch(() => {});
  page.on("dialog", accept);
  try {
    const before = await pillNow();
    await page.evaluate(box => stopBox(box), BOX);
    const during = await pillNow();
    answer();
    await until(() => !boxActing.size);
    const after = await pillNow();
    if (during !== "stopping…" || before === during || after !== before) {
      throw new Error(JSON.stringify({ before, during, after }));
    }
  } finally {
    page.off("dialog", accept);
    await page.unroute(`**/api/boxes/${BOX}/stop`);
  }
});

// Fails if `destroyBox` confirms with the general sentence instead of asking the box
// (`/api/boxes/<name>/unpushed`), reads a box it could not ask as having nothing to lose, or the
// row does not say "destroying…" once it is accepted.
await check("Destroy's confirm says what THIS box would lose, and its row says destroying…", async () => {
  await closeEverything();
  let reply = {};
  await page.route(`**/api/boxes/${BOX}/unpushed`, r => r.fulfill({ json: reply }));
  let answer;
  const answered = new Promise(res => { answer = res; });
  await page.route(`**/api/boxes/${BOX}/destroy`, async r => { await answered; return r.fulfill({ json: { ok: true } }); });
  let accept = false;
  const asked = [];
  const onDialog = d => { asked.push(d.message()); (accept ? d.accept() : d.dismiss()).catch(() => {}); };
  page.on("dialog", onDialog);
  const ask = async r => {
    reply = r;
    const n = asked.length;
    await page.evaluate(box => destroyBox(box), BOX);
    for (let i = 0; i < 30 && asked.length === n; i++) await page.waitForTimeout(100);
    return asked[n] || "";
  };
  try {
    const lots = await ask({ known: true, commits: 2, changes: 1 });
    if (!lots.includes("2 commits never pushed or fetched and 1 uncommitted change")) throw new Error(`with work: ${lots}`);
    const none = await ask({ known: true, commits: 0, changes: 0 });
    if (!none.includes("nothing uncommitted, so only the box and its disk go")) throw new Error(`with none: ${none}`);
    const unknown = await ask({ known: false });
    if (!unknown.includes("could not ask it") || unknown.includes("only the box and its disk go")) throw new Error(`unasked: ${unknown}`);
    accept = true;
    await ask({ known: true, commits: 0, changes: 0 });
    const during = await pillNow();
    answer();
    await until(() => !boxActing.size);
    if (during !== "destroying…") throw new Error(`the row said ${JSON.stringify(during)} while the destroy ran`);
  } finally {
    page.off("dialog", onDialog);
    await page.unroute(`**/api/boxes/${BOX}/unpushed`);
    await page.unroute(`**/api/boxes/${BOX}/destroy`);
  }
});

// Fails if `resumeBox` (board.js) or `revMoveSetAsideAside` (review.js) goes back to a toast cut to
// its first line: the reason is on the second.
await check("a refused continue and a refused move-aside are said whole, in a notice", async () => {
  for (const [route, act] of [
    [`**/api/boxes/${BOX}/resume`, () => page.evaluate(box => resumeBox(box), BOX)],
    ["**/api/repos/acme/set-aside/*/move-aside", () => page.evaluate(() => revMoveSetAsideAside("acme", "snoozed.json"))],
  ]) {
    await closeEverything();
    await page.route(route, r => r.fulfill({ json: { ok: false, error: "refused\nthe reason is on this line" } }));
    try {
      await act();
      await until(() => document.getElementById("notice")?.classList.contains("show"));
      const n = await noticeNow();
      if (!n || n.kind !== "error" || !n.text.includes("the reason is on this line")) throw new Error(`${route}: notice ${JSON.stringify(n)}`);
      await page.locator("#notice .x").click();
    } finally { await page.unroute(route); }
  }
});

await page.evaluate(() => { view = { box: null, mode: "term", kind: "agent" }; applyView(); });

await check("no page errors while feedback was driven", () => {
  if (noise.length) throw new Error(noise.slice(0, 5).join(" | "));
});

// ---------- report ----------
const failed = report({ log });
if (failed.length) console.log(`fixture kept for inspection: ${fx.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
process.exit(failed.length || leftRunning.length ? 1 : 0);
