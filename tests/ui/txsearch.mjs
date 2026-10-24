// Searching a box's transcript, the way a person does it: type in the Transcript view, narrow by
// whose words, widen to every session, and click a result — one older than the window the view
// has loaded, and one in another session — to land on that message with the match marked
// (SKEIN-1323). And ⌘F / Ctrl+F, which opens that search from anywhere a box is open, the box's
// own terminal included.
//
// The server and the records are real: `skein-server` reads a fleet box's `claude-projects/` off
// the fixture's host state, exactly as it reads a real box's. Only the terminal socket is
// Playwright's (`routeWebSocket`), as in `termkeys.mjs`, because what is asserted there is the
// bytes the page sends — that Ctrl+F is NOT one of them.
//
// # How each check is made to fail
//
// Planted, run and restored, each turning the named check red:
//
// - `.txtop` without `position:sticky`: "the search row stays pinned while the conversation scrolls".
// - The `who` parameter dropped from the search request: "you", "Claude" each fail.
// - `scope` always `session`: "all sessions finds the earlier session's message too".
// - The server ignoring `at` (`read_transcript_at` reading the tail): "a result older than the
//   loaded window opens at that message, marked".
// - `txMark` returning at once: the same check, on its mark.
// - The input's Escape branch removed: "Escape in the field clears the search".
// - `.txfind` with `flex-wrap:nowrap`: "at phone width the choice and toggle wrap under the field".
// - The terminals' `tx-search` hand-back removed: "Ctrl+F from inside the terminal…".
// - `mod` narrowed to `ctrlKey` for F in `keys.mjs`: "Cmd+F opens the search too".
// - The `boxOpen` test dropped from `keys.mjs`: "with no box open, Ctrl+F is left to the browser".
// - `if (popupOpen()) return;` removed from boot.js: "with a popup open, Ctrl+F does nothing".
// - The Shift+Enter branch made to return `true`: "Shift+Enter in the agent tab still sends a newline".
//
//   node tests/ui/txsearch.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const BOX = "txs-box";
const API_TOKEN = "s".repeat(64);

const user = (ts, text) => JSON.stringify({ type: "user", timestamp: ts, message: { role: "user", content: text } });
const claude = (ts, text, extra = []) => JSON.stringify({ type: "assistant", timestamp: ts,
  message: { role: "assistant", content: [{ type: "text", text }, ...extra] } });

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "skein-txsearch-ui");
  const home = path.join(root, "home");
  fs.mkdirSync(path.join(home, "places"), { recursive: true });
  fs.mkdirSync(path.join(root, "fleet"), { recursive: true });
  fs.writeFileSync(path.join(home, "config.json"), "{}");
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  // A placement record is what makes this a fleet box whose record skein reads off the host.
  fs.writeFileSync(path.join(home, "places", `${BOX}.json`), JSON.stringify({
    sandbox: "skein-fleet", ns_pid: 0, ns_start: 1, generation: "test-boot", home: "",
    tree: path.join(root, "tree"), sock: path.join(root, "fleet", BOX, "session.sock"),
  }));
  const slug = path.join(home, "boxes", BOX, "claude-projects", "-fixture");
  fs.mkdirSync(slug, { recursive: true });
  fs.writeFileSync(path.join(slug, "older.jsonl"), [
    user("2026-10-01T09:00:00Z", "is the gearbox from the earlier session fixed?"),
    claude("2026-10-01T09:00:05Z", "Not yet."),
  ].join("\n") + "\n");
  const hour = new Date(Date.now() - 3600_000);
  fs.utimesSync(path.join(slug, "older.jsonl"), hour, hour);
  // The current session: its first message sits behind 400K of bookkeeping, past the 256K the view
  // loads first, so it is outside the loaded window. The tool call mentions the gearbox too, and
  // search does not read tool calls.
  const filler = Array.from({ length: 4000 }, (_, i) =>
    JSON.stringify({ type: "mode", mode: "default", n: i, pad: "y".repeat(80) })).join("\n");
  const late = Array.from({ length: 60 }, (_, i) => user(`2026-10-02T10:${String(i).padStart(2, "0")}:00Z`, `status line ${i}, nothing to see`));
  fs.writeFileSync(path.join(slug, "current.jsonl"), [
    user("2026-10-02T08:00:00Z", "find the gearbox early on, before anything else"),
    filler,
    claude("2026-10-02T09:00:00Z", "The **gearbox** is in `src/gear.rs`.", [
      { type: "tool_use", name: "Grep", input: { pattern: "gearbox" } }]),
    ...late,
    user("2026-10-02T11:00:00Z", "the newest ask"),
  ].join("\n") + "\n");
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, "#!/usr/bin/env bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
  fs.chmodSync(sbx, 0o755);
  return { root, home, bin, sbx };
}

const { check, report } = ledger();
const fx = makeFixture();
const door = await openDoor();
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,
    PATH: `${fx.bin}:${process.env.PATH}`,
  },
});

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
page.setDefaultTimeout(5000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
const sent = { agent: [], shell: [] };
await page.routeWebSocket(/\/api\/boxes\/[^/]+\/terminal/, ws => {
  const kind = new URL(ws.url()).searchParams.get("shell") === "1" ? "shell" : "agent";
  ws.onMessage(m => { if (typeof m !== "string") sent[kind].push(Buffer.from(m).toString("latin1")); });
});

const until = async (what, f, ms = 6000) => {
  const deadline = Date.now() + ms;
  for (;;) {
    const got = await f();
    if (got) return got;
    if (Date.now() > deadline) throw new Error(`waited ${ms / 1000}s for ${what}`);
    await page.waitForTimeout(100);
  }
};
const hits = () => page.$$eval("#txpane .txhit", bs => bs.map(b => ({
  who: b.querySelector(".txhitwho span").textContent,
  mark: b.querySelector("mark")?.textContent || "",
  text: b.querySelector(".txsnip").textContent,
  where: b.querySelectorAll(".txhitwho span")[2]?.textContent || "",
})));
const count = () => page.$eval("#txpane .txrcount", e => e.textContent).catch(() => "");
// Wait for a search to have answered: the header is no longer "searching…".
const answered = () => until("the search to answer", async () => { const c = await count(); return c && c !== "searching…" ? c : null; });
const openTx = async () => {
  await page.evaluate(b => showBox(b, "tx"), BOX);
  await until("the transcript", () => page.$("#txpane .txmsg"));
};
// Whether `sel` is drawn inside the transcript's scrolling area, below the pinned rows, and on top.
const onScreen = sel => page.evaluate(sel => {
  const el = document.querySelector(sel), wrap = document.querySelector("#txpane .txwrap");
  if (!el || !wrap) return false;
  const r = el.getBoundingClientRect(), w = wrap.getBoundingClientRect(), top = document.querySelector("#txpane .txtop").getBoundingClientRect();
  const y = Math.min(r.bottom - 2, Math.max(r.top + 2, top.bottom + 2));
  const hit = document.elementFromPoint(r.left + Math.min(20, r.width / 2), y);
  return r.bottom > top.bottom && r.top < w.bottom && !!hit && el.contains(hit);
}, sel);

await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await page.waitForFunction(() => typeof showBox === "function" && typeof createSession === "function");

console.log("\nthe search row");
await openTx();
await check("the loaded window does not reach the session's first message (the control)", async () => {
  const body = await page.textContent("#txpane .txconv");
  if (body.includes("find the gearbox early on")) throw new Error("the first message is already loaded; the fixture is not testing the older window");
  if (!body.includes("the newest ask")) throw new Error("the transcript did not render");
});
await check("the search row is there, with the field, the three-way choice and the scope toggle", async () => {
  const got = await page.evaluate(() => ({
    placeholder: document.querySelector("#txpane .txq")?.placeholder,
    who: [...document.querySelectorAll("#txpane .txwhosel button")].map(b => `${b.textContent}${b.classList.contains("on") ? "*" : ""}`),
    all: document.querySelector("#txpane .txall")?.textContent.trim(),
  }));
  const want = { placeholder: "Search this conversation", who: ["you", "Claude", "both*"], all: "all sessions" };
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`got ${JSON.stringify(got)}`);
});
await check("the search row stays pinned while the conversation scrolls", async () => {
  const at = () => page.evaluate(() => {
    const q = document.querySelector("#txpane .txq").getBoundingClientRect();
    const hit = document.elementFromPoint(q.left + 10, q.top + q.height / 2);
    return { top: Math.round(q.top), onTop: hit?.classList.contains("txq") || false };
  });
  await page.evaluate(() => { document.querySelector("#txpane .txwrap").scrollTop = 0; });
  const top = await at();
  await page.evaluate(() => { const w = document.querySelector("#txpane .txwrap"); w.scrollTop = w.scrollHeight / 2; });
  const middle = await at();
  await page.evaluate(() => { const w = document.querySelector("#txpane .txwrap"); w.scrollTop = w.scrollHeight; });
  const bottom = await at();
  if (!(top.onTop && middle.onTop && bottom.onTop) || top.top !== middle.top || top.top !== bottom.top)
    throw new Error(`the field moved or was covered: ${JSON.stringify({ top, middle, bottom })}`);
});

console.log("\nsearching");
await page.fill("#txpane .txq", "GEARBOX");
await check("typing searches the whole session, and a list replaces the conversation", async () => {
  const c = await answered();
  if (c !== "2 matches") throw new Error(`header ${JSON.stringify(c)}, want "2 matches"`);
  const shown = await page.evaluate(() => ({ conv: document.querySelector("#txpane .txconv").hidden, list: document.querySelector("#txpane .txresults").hidden }));
  if (!shown.conv || shown.list) throw new Error(`conversation hidden ${shown.conv}, list hidden ${shown.list}`);
  const h = await hits();
  if (JSON.stringify(h.map(x => [x.who, x.mark])) !== JSON.stringify([["Claude", "gearbox"], ["you", "gearbox"]]))
    throw new Error(`results ${JSON.stringify(h)}`);
});
for (const [label, want] of [["you", [["you", "gearbox"]]], ["Claude", [["Claude", "gearbox"]]], ["both", [["Claude", "gearbox"], ["you", "gearbox"]]]]) {
  await check(`"${label}" lists only ${label === "both" ? "both sides" : "that side's"} messages`, async () => {
    await page.click(`#txpane .txwhosel button:text-is("${label}")`);
    await answered();
    const h = (await hits()).map(x => [x.who, x.mark]);
    if (JSON.stringify(h) !== JSON.stringify(want)) throw new Error(`results ${JSON.stringify(h)}`);
  });
}
await check("a result older than the loaded window opens at that message, marked", async () => {
  await page.click("#txpane .txhit:has-text('early on')");
  await until("the message", () => page.evaluate(() =>
    document.querySelector("#txpane .txmsg.txfound .txtext")?.textContent.includes("early on")));
  const got = await page.evaluate(() => ({
    text: document.querySelector("#txpane .txmsg.txfound .txtext").textContent,
    mark: [...document.querySelectorAll("#txpane .txmsg.txfound mark.txmark")].map(m => m.textContent),
    query: document.querySelector("#txpane .txq").value,
    listHidden: document.querySelector("#txpane .txresults").hidden,
  }));
  if (got.text !== "find the gearbox early on, before anything else" || JSON.stringify(got.mark) !== '["gearbox"]'
      || got.query !== "GEARBOX" || !got.listHidden) throw new Error(JSON.stringify(got));
  if (!(await onScreen("#txpane .txmsg.txfound"))) throw new Error("the message is not on screen");
  if (!(await page.textContent("#txpane .txback")).includes("back to the latest")) throw new Error("no way back to the latest");
});
await check("coming back to the field brings the list back", async () => {
  await page.focus("#txpane .txq");
  await until("the list", () => page.evaluate(() => !document.querySelector("#txpane .txresults").hidden));
});
await check("all sessions finds the earlier session's message too, and says which session", async () => {
  await page.check("#txpane .txallbox");
  await until("three matches", async () => (await count()).startsWith("3 matches"));
  const h = await hits();
  const where = h.map(x => x.where);
  if (JSON.stringify(where) !== JSON.stringify(["this session", "this session", "session older"])) throw new Error(JSON.stringify(h));
});
await check("a result in another session opens that session at the message, marked", async () => {
  await page.click("#txpane .txhit:has-text('earlier session')");
  await until("the earlier session's message", () => page.evaluate(() =>
    document.querySelector("#txpane .txmsg.txfound .txtext")?.textContent.includes("earlier session")));
  const got = await page.evaluate(() => ({
    text: document.querySelector("#txpane .txmsg.txfound .txtext").textContent,
    mark: [...document.querySelectorAll("#txpane .txmsg.txfound mark.txmark")].map(m => m.textContent),
    meta: document.querySelector("#txpane .txmeta").textContent,
    back: document.querySelector("#txpane .txback")?.textContent,
  }));
  if (got.text !== "is the gearbox from the earlier session fixed?" || JSON.stringify(got.mark) !== '["gearbox"]'
      || !got.meta.includes("an earlier session") || got.back !== "back to the current session") throw new Error(JSON.stringify(got));
  if (!(await onScreen("#txpane .txmsg.txfound"))) throw new Error("the message is not on screen");
});
await check("no matches says so, and how many sessions it read", async () => {
  await page.fill("#txpane .txq", "nothing-like-this-anywhere");
  await until("no matches", async () => (await count()) === "no matches in 2 sessions");
});
await check("Escape in the field clears the search and shows the conversation", async () => {
  await page.focus("#txpane .txq");
  await page.keyboard.press("Escape");
  const got = await page.evaluate(() => ({
    value: document.querySelector("#txpane .txq").value,
    list: document.querySelector("#txpane .txresults").hidden,
    conv: document.querySelector("#txpane .txconv").hidden,
    marks: document.querySelectorAll("#txpane mark.txmark").length,
  }));
  if (JSON.stringify(got) !== JSON.stringify({ value: "", list: true, conv: false, marks: 0 })) throw new Error(JSON.stringify(got));
});
await check("the ✕ on the list clears it too", async () => {
  await page.fill("#txpane .txq", "gearbox");
  await answered();
  await page.click("#txpane .txrx");
  const got = await page.evaluate(() => ({ value: document.querySelector("#txpane .txq").value, conv: document.querySelector("#txpane .txconv").hidden }));
  if (got.value !== "" || got.conv) throw new Error(JSON.stringify(got));
});

console.log("\nat a phone's width");
await check("at phone width the choice and toggle wrap under the field, and nothing scrolls sideways", async () => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.evaluate(b => showBox(b, "tx"), BOX);
  await until("the transcript", () => page.$("#txpane .txq"));
  await page.waitForTimeout(300);
  const got = await page.evaluate(() => {
    const q = document.querySelector("#txpane .txq").getBoundingClientRect();
    const o = document.querySelector("#txpane .txopts").getBoundingClientRect();
    const pane = document.querySelector("#txpane .txwrap");
    return { wrapped: o.top >= q.bottom, inside: o.right <= innerWidth && q.right <= innerWidth && q.width > 150,
             page: document.documentElement.scrollWidth <= innerWidth, pane: pane.scrollWidth <= pane.clientWidth };
  });
  if (!got.wrapped || !got.inside || !got.page || !got.pane) throw new Error(JSON.stringify(got));
});
await page.setViewportSize({ width: 1400, height: 900 });

console.log("\n⌘F / Ctrl+F");
const openAgent = async () => {
  await page.evaluate(b => {
    if (!sessions.get(sidOf(b, "agent"))) createSession(b, "agent", null, "claude");
    view = { box: b, mode: "term", kind: "agent" };
    applyView();
  }, BOX);
  await page.waitForFunction(b => sessions.get(sidOf(b, "agent"))?.ws.readyState === 1, BOX);
};
const searchFocused = () => page.evaluate(() => view.mode === "tx" && document.activeElement?.classList.contains("txq"));
await check("Ctrl+F from inside the terminal opens the search, with the last query selected, and sends the terminal nothing", async () => {
  await page.evaluate(b => { showBox(b, "tx"); }, BOX);
  await until("the field", () => page.$("#txpane .txq"));
  await page.fill("#txpane .txq", "gearbox");
  await answered();
  await openAgent();
  await page.evaluate(b => sessions.get(sidOf(b, "agent")).term.focus(), BOX);
  sent.agent.length = 0;
  await page.keyboard.press("Control+f");
  await until("the focused search", searchFocused);
  const sel = await page.evaluate(() => { const q = document.activeElement; return [q.value, q.selectionStart, q.selectionEnd]; });
  if (JSON.stringify(sel) !== JSON.stringify(["gearbox", 0, 7])) throw new Error(`field ${JSON.stringify(sel)}`);
  await page.waitForTimeout(300);
  if (sent.agent.join("").includes("\x06")) throw new Error("the terminal was sent ^F");
});
await check("Ctrl+F from the board with a box open opens the search", async () => {
  await openAgent();
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press("Control+f");
  await until("the focused search", searchFocused);
});
await check("Cmd+F opens the search too", async () => {
  await openAgent();
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press("Meta+f");
  await until("the focused search", searchFocused);
});
// Whether the page took the key from the browser: a listener after every one of the page's.
await page.evaluate(() => { window.__lastF = null; window.addEventListener("keydown", e => { if (e.key === "f") window.__lastF = e.defaultPrevented; }); });
await check("with no box open, Ctrl+F is left to the browser", async () => {
  await page.evaluate(() => { view = { box: null, mode: "term", kind: "agent" }; applyView(); document.activeElement?.blur(); });
  await page.keyboard.press("Control+f");
  const got = await page.evaluate(() => [window.__lastF, view.mode]);
  if (JSON.stringify(got) !== JSON.stringify([false, "term"])) throw new Error(`defaultPrevented, mode: ${JSON.stringify(got)}`);
});
await check("with a popup open, Ctrl+F does nothing", async () => {
  await openAgent();
  await page.evaluate(() => openPalette());
  await page.keyboard.press("Control+f");
  await page.waitForTimeout(300);
  const got = await page.evaluate(() => [view.mode, document.getElementById("pal").classList.contains("open")]);
  await page.evaluate(() => closePalette());
  if (JSON.stringify(got) !== JSON.stringify(["term", true])) throw new Error(`mode, palette open: ${JSON.stringify(got)}`);
});
await check("Shift+Enter in the agent tab still sends a newline", async () => {
  await openAgent();
  await page.evaluate(b => sessions.get(sidOf(b, "agent")).term.focus(), BOX);
  sent.agent.length = 0;
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.press("x");
  await until("the marker", async () => sent.agent.join("").endsWith("x"));
  const got = sent.agent.join("").slice(0, -1);
  if (got !== "\n") throw new Error(`the socket had ${JSON.stringify(got)}`);
});

await check("no page errors", () => { if (noise.length) throw new Error(noise.slice(0, 5).join(" | ")); });

const failed = report({ log });
if (failed.length) console.log(`fixture kept for inspection: ${fx.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
process.exit(failed.length || leftRunning.length ? 1 : 0);
