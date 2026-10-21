// Does Shift+Enter in an agent's terminal start a new line, and leave a shell's alone?
//
// The owner asked for it in as many words: "shift enter should do next line rather than
// submitting" (SKEIN-1243). xterm encodes Shift+Enter as "\r", the same byte as Enter, so Claude
// Code and Codex both submitted the prompt. `createSession` now hands an agent tab's Shift+Enter to
// a key handler that sends Ctrl+J ("\n") instead, which both of them read as a newline in the
// prompt; its comment says how that byte was chosen.
//
// # What is asserted, and where
//
// The bytes ON THE SOCKET, for a real keypress in a real xterm. The key is pressed by Playwright's
// keyboard, so it arrives as the browser's own keydown/keypress pair — which matters, because xterm
// asks a custom key handler about both, and a handler that only refuses keydown still sends "\r"
// from keypress. A lifted function driven by a hand-built event would not see that.
//
// The socket is Playwright's `routeWebSocket` rather than the server's terminal route, because what
// the box does with the byte was settled by trying it on the real TUIs (the receipt on SKEIN-1243)
// and what is left to prove is what the page sends. The page itself is the real server's.
//
// # How each check is made to fail
//
// Each of these was planted, run, and restored, and each time it was the named check and no other
// that went red:
//
// - Skip the `attachCustomKeyEventHandler` call in `createSession` (`if (false)`): "Shift+Enter in
//   an agent tab sends a newline and not a carriage return" fails, reading "\r" off the socket.
// - Have the handler return `true` for Shift+Enter: the same check fails, reading "\n\r".
// - Have it return `true` for keypress only: the same check fails, reading "\n\r" — the reason the
//   handler refuses every event type.
// - Drop the `kind === "agent"` guard (`if (true)`): "Shift+Enter in a shell tab still sends a
//   carriage return" fails, reading "\n".
//
//   node tests/ui/termkeys.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const BOX = "termkeys-box";
// See the note in smoke.mjs: written by the fixture rather than read back after startup.
const API_TOKEN = "k".repeat(64);

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "skein-termkeys-ui");
  const home = path.join(root, "home");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(path.join(root, "fleet"), { recursive: true });
  fs.writeFileSync(path.join(home, "config.json"), "{}");
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  // The stand-in for sbx, so nothing the page asks reaches whatever `sbx` this machine has.
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, "#!/usr/bin/env bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
  fs.chmodSync(sbx, 0o755);
  return { root, home, bin, sbx };
}

const { check, results, report } = ledger();

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

// Every frame the page sends on a terminal socket, by which kind of terminal it is. A text frame is
// a resize; the keys are the binary ones.
const sent = { agent: [], shell: [] };
await page.routeWebSocket(/\/api\/boxes\/[^/]+\/terminal/, ws => {
  const kind = new URL(ws.url()).searchParams.get("shell") === "1" ? "shell" : "agent";
  ws.onMessage(m => { if (typeof m !== "string") sent[kind].push(Buffer.from(m).toString("latin1")); });
});

await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await page.waitForFunction(() => typeof createSession === "function" && typeof sessions !== "undefined");

/** Open a terminal of `kind` for the box, bring it to the front, focus it, and wait for its socket. */
const open = async kind => {
  await page.evaluate(k => {
    createSession("termkeys-box", k, null, k === "agent" ? "claude" : null);
    view = { box: "termkeys-box", mode: "term", kind: k };
    applyView();
  }, kind);
  await page.waitForFunction(k => sessions.get(sidOf("termkeys-box", k))?.ws.readyState === 1, kind);
  await page.evaluate(k => sessions.get(sidOf("termkeys-box", k)).term.focus(), kind);
};

/** Press `key` and answer with exactly the bytes it put on the socket, in order. A trailing "x" is
 * typed after it and waited for, so every byte the key produced has arrived before this reads. */
const press = async (kind, key) => {
  sent[kind].length = 0;
  await page.keyboard.press(key);
  await page.keyboard.press("x");
  const deadline = Date.now() + 5000;
  while (!sent[kind].join("").endsWith("x")) {
    if (Date.now() > deadline) throw new Error(`the marker never arrived; the socket had ${JSON.stringify(sent[kind].join(""))}`);
    await new Promise(r => setTimeout(r, 20));
  }
  return sent[kind].join("").slice(0, -1);
};

console.log(`\nan agent tab on ${BOX}`);
await open("agent");

await check("Shift+Enter in an agent tab sends a newline and not a carriage return", async () => {
  const got = await press("agent", "Shift+Enter");
  if (got !== "\n") throw new Error(`the socket had ${JSON.stringify(got)}, want "\\n"`);
});

await check("Enter in an agent tab still submits", async () => {
  const got = await press("agent", "Enter");
  if (got !== "\r") throw new Error(`the socket had ${JSON.stringify(got)}, want "\\r"`);
});

// The handler takes Shift+Enter only; a key it has no business with goes through untouched.
await check("ordinary typing in an agent tab is unchanged", async () => {
  const got = await press("agent", "a");
  if (got !== "a") throw new Error(`the socket had ${JSON.stringify(got)}, want "a"`);
});

console.log(`\na shell tab on ${BOX}`);
await open("shell");

await check("Shift+Enter in a shell tab still sends a carriage return", async () => {
  const got = await press("shell", "Shift+Enter");
  if (got !== "\r") throw new Error(`the socket had ${JSON.stringify(got)}, want "\\r"`);
});

await check("no page errors while the terminals were driven", () => {
  if (noise.length) throw new Error(noise.slice(0, 5).join(" | "));
});

// ---------- report ----------
const failed = report({ log });
if (failed.length) console.log(`fixture kept for inspection: ${fx.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
process.exit(failed.length || leftRunning.length ? 1 : 0);
