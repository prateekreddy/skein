// Does a popup behave like a popup with a box terminal open behind it?
//
// The owner reported both halves in one sentence (SKEIN-1246): "clicking outside popups doesn't
// close it. Also pasting in text fields on popups like settings just pastes it outside."
//
// # Paste and copy
//
// The terminal's clipboard handler (`settings.js`, "terminal clipboard") listens at the document in
// the capture phase, so it sees every Cmd+V / Ctrl+Shift+V on the page before anything focused
// does. It used to answer every one of them, whenever a terminal was the active view, by moving
// focus to that terminal so the browser's native paste landed in xterm — including a paste meant
// for a text field in Settings, which went to the box behind it. It now leaves a chord alone when
// focus is in a text field that is not a terminal's, or when any popup is open.
//
// Asserted on the mechanism, not on a lifted function: real keypresses from Playwright's keyboard,
// the real clipboard (Chromium pastes natively on Ctrl+Shift+V headless, which was measured before
// this was written; Cmd+V does not paste natively on Linux, so for it what is asserted is where the
// focus ends up), and the bytes on the terminal's socket, which `routeWebSocket` stands in for as it
// does in `termkeys.mjs`.
//
// # A click outside
//
// Every popup is wired through one helper, `closeOnBackdrop` in `panels.js`, which closes it only
// when the press AND the release are both on the backdrop itself — so selecting text in a field and
// letting go outside the dialog does not throw the dialog away. The login terminal is the one popup
// left out: a stray click would end a login in progress.
//
// # How each check is made to fail
//
// Each was planted, run and restored; the commit that adds this file names what went red.
//
//   node tests/ui/popups.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const BOX = "popups-box";
// See the note in smoke.mjs: written by the fixture rather than read back after startup.
const API_TOKEN = "p".repeat(64);

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "skein-popups-ui");
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
  },
});

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1400, height: 900 } });
await context.grantPermissions(["clipboard-read", "clipboard-write"], { origin });
const page = await context.newPage();
page.setDefaultTimeout(5000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));

// Every key frame the page sends on the box's terminal socket. A text frame is a resize.
const sent = [];
await page.routeWebSocket(/\/api\/boxes\/[^/]+\/terminal/, ws => {
  ws.onMessage(m => { if (typeof m !== "string") sent.push(Buffer.from(m).toString("latin1")); });
});
// The login terminal's socket, stood in for so opening that popup starts no login on the server.
// Its frames are recorded only to prove the stand-in was reached.
const loginFrames = [];
await page.routeWebSocket(/\/api\/login\/[^/]+\/terminal/, ws => {
  ws.onMessage(m => loginFrames.push(m));
});

await page.goto(`${origin}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await page.waitForFunction(() => typeof createSession === "function" && typeof sessions !== "undefined");

// A box's agent terminal, open in the dock and in front: the state the owner was in.
await page.evaluate(box => {
  createSession(box, "agent", null, "claude");
  view = { box, mode: "term", kind: "agent" };
  applyView();
}, BOX);
await page.waitForFunction(box => sessions.get(sidOf(box, "agent"))?.ws.readyState === 1, BOX);

const setClipboard = text => page.evaluate(t => navigator.clipboard.writeText(t), text);
const readClipboard = () => page.evaluate(() => navigator.clipboard.readText());
/** Whatever reached the socket in the next half second — long enough for a paste to arrive. */
const socketAfter = async () => { await page.waitForTimeout(500); return sent.join(""); };
const inTerminal = () => page.evaluate(box => sessions.get(sidOf(box, "agent")).host.contains(document.activeElement), BOX);
const activeId = () => page.evaluate(() => document.activeElement?.id || document.activeElement?.tagName);
const closeEverything = () => page.evaluate(() => {
  for (const o of document.querySelectorAll(".open[aria-hidden], #away.open, #load.open")) o.classList.remove("open");
  closeMailbox();
});

// ---------- paste and copy with Settings open over the terminal ----------
console.log("\nSettings open over a box terminal");

/** Settings on the GitHub pane, with focus in its plain-text GitHub App ID field, emptied.
 *
 *  **Closed first, so the wait below waits for THIS open** (SKEIN-1304). `openSettings` adds the
 *  `open` class, and moves focus to the dialog's shell, only once its loads answer — so with
 *  Settings still open from the check before, the wait returned at once, this focused the field,
 *  and the late answer then took focus away from it: the keypress reached the shell, and the
 *  field's listener saw nothing (master run 39862044533, "the field saw []"). Closed, the class can
 *  only come back from this call's answer, in the same task that moves the focus. */
const focusSettingsField = async () => {
  await page.evaluate(() => { closeSettings(); openSettings("github"); });
  await page.waitForFunction(() => settingsModal().classList.contains("open"));
  await page.locator("#set-ghappid").fill("");
  await page.locator("#set-ghappid").focus();
};

// Fails if the clipboard handler stops leaving a field in a popup alone: it moves focus to the
// terminal, the native paste lands in xterm, the field stays empty and the socket gets the text.
await check("Ctrl+Shift+V in a Settings field pastes into the field and sends nothing to the terminal", async () => {
  await focusSettingsField();
  await setClipboard("skein-test-owner/pasted");
  sent.length = 0;
  await page.keyboard.press("Control+Shift+V");
  const got = await page.locator("#set-ghappid").inputValue();
  const wire = await socketAfter();
  const at = await activeId();
  if (got !== "skein-test-owner/pasted" || wire || at !== "set-ghappid") {
    throw new Error(`field ${JSON.stringify(got)}, socket ${JSON.stringify(wire)}, focus on ${at}`);
  }
});

// Cmd+V does not paste natively on Linux, so what is left to see is where it puts the focus. Fails
// the same way as the check above: the handler focuses the terminal.
await check("Cmd+V in a Settings field leaves the focus in the field", async () => {
  await focusSettingsField();
  sent.length = 0;
  await page.keyboard.press("Meta+V");
  const at = await activeId();
  const wire = await socketAfter();
  if (at !== "set-ghappid" || wire) throw new Error(`focus on ${at}, socket ${JSON.stringify(wire)}`);
});

// Focus inside the dialog but not in a field — on the dialog itself, as after clicking its text.
// Fails if the "a popup is open" half of the guard is dropped and only the field half is kept.
await check("Cmd+V with focus on the Settings dialog (not a field) does not move focus to the terminal", async () => {
  await focusSettingsField();
  await page.locator("#settings .set-shell").focus();
  await page.keyboard.press("Meta+V");
  if (await inTerminal()) throw new Error("the terminal behind the dialog took the focus");
});

// The copy half has the same shape. The handler swallows a copy chord it answers
// (preventDefault + stopPropagation) and writes the terminal's text instead, so a field's own
// listener never sees the key. Fails if the guard is removed from the copy branch: the field's
// listener records nothing and the clipboard receives the terminal's offer.
await check("Ctrl+Shift+C in a Settings field is left to the field, not answered with the terminal's text", async () => {
  await focusSettingsField();
  await page.locator("#set-ghappid").fill("owner/name");
  await page.locator("#set-ghappid").selectText();
  await page.evaluate(() => {
    oscClipboard = "skein-test-terminal-offer";
    window.__fieldKeys = [];
    // The C alone: Playwright presses Control and Shift first, and those reach the field either way.
    document.getElementById("set-ghappid").addEventListener("keydown", e => {
      if (e.key === "c" || e.key === "C") window.__fieldKeys.push({ key: e.key, prevented: e.defaultPrevented });
    });
  });
  await setClipboard("");
  await page.keyboard.press("Control+Shift+C");
  const keys = await page.evaluate(() => window.__fieldKeys);
  const clip = await readClipboard();
  if (keys.length !== 1 || keys[0].prevented || clip === "skein-test-terminal-offer") {
    throw new Error(`the field saw ${JSON.stringify(keys)}; the clipboard holds ${JSON.stringify(clip)}`);
  }
});

await page.evaluate(() => { closeSettings(); oscClipboard = ""; });

// ---------- the terminal's own copy and paste, unchanged ----------
console.log("\nthe terminal itself, no popup open");

// After a mouse-drag selection xterm leaves focus on <body>; that is why the handler exists. Fails
// if the paste branch stops focusing the terminal, or if the new guard reads <body> as a field.
await check("Cmd+V with focus on the page (after a terminal selection) still focuses the terminal", async () => {
  await page.evaluate(() => document.activeElement?.blur?.());
  await page.keyboard.press("Meta+V");
  if (!(await inTerminal())) throw new Error(`focus went to ${await activeId()}`);
});

// End to end: the clipboard's text arrives on the box's socket. Fails if the paste branch stops
// focusing the terminal: the browser's native paste then lands on <body> and the box gets nothing.
await check("Ctrl+Shift+V into the terminal still sends the clipboard to the box", async () => {
  await page.evaluate(() => document.activeElement?.blur?.());
  await setClipboard("skein-test-to-the-box");
  sent.length = 0;
  await page.keyboard.press("Control+Shift+V");
  const wire = await socketAfter();
  if (!wire.includes("skein-test-to-the-box")) throw new Error(`socket had ${JSON.stringify(wire)}`);
});

// Cmd+C with focus in xterm's own textarea copies what the terminal offered. Fails if the guard
// counts xterm's hidden TEXTAREA as a text field: the chord is left to the browser, which has no
// selection to copy, and the clipboard stays empty.
await check("Cmd+C in the terminal still copies the terminal's text, and sends the box nothing", async () => {
  await page.evaluate(box => { oscClipboard = "skein-test-terminal-offer"; sessions.get(sidOf(box, "agent")).term.focus(); }, BOX);
  await setClipboard("");
  sent.length = 0;
  await page.keyboard.press("Meta+C");
  const clip = await readClipboard();
  const wire = await socketAfter();
  if (clip !== "skein-test-terminal-offer" || wire) throw new Error(`clipboard ${JSON.stringify(clip)}, socket ${JSON.stringify(wire)}`);
  await page.evaluate(() => { oscClipboard = ""; });
});

// A text field that is not in a popup: the board's filter. The owner's report was about popups, but
// the cause was "any field" — the same handler took a paste meant for the filter. Fails if the
// field half of the guard is dropped and only the popup half kept.
await check("Ctrl+Shift+V in the board filter pastes into the filter, not the terminal", async () => {
  await page.evaluate(() => openFilter());
  await setClipboard("skein-test-filter");
  sent.length = 0;
  await page.keyboard.press("Control+Shift+V");
  const got = await page.evaluate(() => filterEl.value);
  const wire = await socketAfter();
  await page.evaluate(() => closeFilter());
  if (got !== "skein-test-filter" || wire) throw new Error(`filter ${JSON.stringify(got)}, socket ${JSON.stringify(wire)}`);
});

// ---------- a click outside ----------
console.log("\na click outside each popup");

// How each popup is opened, by the page's own opener. The element ids are the overlays' own.
const POPUPS = [
  ["pal", () => openPalette()],
  ["mbx", () => openMailbox()],
  ["newbox", () => openNewBox()],
  ["settings", () => openSettings("github")],
  ["addrepo", () => openAddRepo()],
  ["boxset", box => openBoxSettings(box)],
  ["subq", () => openSubq()],
  ["gitq", () => openGitq()],
  ["askq", () => openAskq()],
  ["away", box => renderAwayOverlay([{ kind: "done", name: box, text: "finished" }], "")],
  ["load", () => showLoad()],
];
const BACKDROP = { x: 6, y: 6 };

// A popup added later must be named in `POPUP_IDS`, or a paste into its fields goes to the terminal
// again. The markup declares each one as `<div id=X aria-hidden="true">` (overlays.mjs reads the
// same convention). Fails if an id is dropped from `POPUP_IDS`, or a new overlay is not added.
await check("every popup the markup declares is one the clipboard keys know about", async () => {
  const missing = await page.evaluate(() => [...document.querySelectorAll('body > div[aria-hidden="true"][id]')]
    .map(o => o.id).filter(id => !POPUP_IDS.includes(id)));
  if (missing.length) throw new Error(`not in POPUP_IDS: ${missing.join(", ")}`);
});
// And this suite clicks outside every one of them but the login terminal.
await check("this suite covers every popup the markup declares", async () => {
  const declared = await page.evaluate(() => [...document.querySelectorAll('body > div[aria-hidden="true"][id]')].map(o => o.id));
  const covered = new Set([...POPUPS.map(([id]) => id), "loginterm"]);
  const missing = declared.filter(id => !covered.has(id));
  if (missing.length || declared.length < 9) throw new Error(`declared ${declared.join(", ")}; not driven here: ${missing.join(", ")}`);
});

const openPopup = async (id, opener) => {
  await closeEverything();
  await page.evaluate(`(${opener})(${JSON.stringify(BOX)})`);
  await page.waitForFunction(i => document.getElementById(i)?.classList.contains("open"), id);
  // The precondition, so a click that missed the backdrop cannot pass for one that hit it.
  const top = await page.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.id, BACKDROP);
  if (top !== id) throw new Error(`the point meant to be #${id}'s backdrop is #${top}`);
};
const isOpen = id => page.evaluate(i => document.getElementById(i).classList.contains("open"), id);
/** The middle of the popup's first visible text field, or of its card when it has none. */
const insidePoint = id => page.evaluate(i => {
  const ov = document.getElementById(i);
  const seen = el => { const r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const field = [...ov.querySelectorAll("input:not([type=checkbox]):not([type=hidden]), textarea, select")].find(seen);
  const r = (field || ov.firstElementChild).getBoundingClientRect();
  return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
}, id);

for (const [id, opener] of POPUPS) {
  // Fails if this popup is not wired to `closeOnBackdrop` — eight of these eleven failed before
  // SKEIN-1246, which is the report.
  await check(`#${id} closes on a click on the backdrop`, async () => {
    await openPopup(id, opener);
    await page.mouse.click(BACKDROP.x, BACKDROP.y);
    if (await isOpen(id)) throw new Error("still open");
  });
  // Fails if the helper closes on the release alone (mouseup or click without remembering where
  // the press began): a click's target is the common ancestor of press and release, the backdrop.
  await check(`#${id} stays open when a press inside it is released on the backdrop`, async () => {
    await openPopup(id, opener);
    const p = await insidePoint(id);
    await page.mouse.move(p.x, p.y);
    await page.mouse.down();
    await page.mouse.move(BACKDROP.x, BACKDROP.y, { steps: 4 });
    await page.mouse.up();
    if (!(await isOpen(id))) throw new Error("closed on a drag out of the dialog");
  });
}

// The other direction of the same drag. Fails if the helper closes on the press alone — which is
// what Settings and Box settings did before, on `mousedown`.
await check("Settings stays open when a press on the backdrop is released inside it", async () => {
  await openPopup("settings", () => openSettings("github"));
  const p = await insidePoint("settings");
  await page.mouse.move(BACKDROP.x, BACKDROP.y);
  await page.mouse.down();
  await page.mouse.move(p.x, p.y, { steps: 4 });
  await page.mouse.up();
  if (!(await isOpen("settings"))) throw new Error("closed on a press that ended inside");
});

// Only the primary button closes: a right-click on the backdrop opens a context menu, and a middle
// click is a paste on Linux — neither is "dismiss this". Fails if the helper's press test drops
// `e.button === 0` (`pressed = e.target === ov`).
for (const button of ["right", "middle"]) {
  await check(`Settings stays open on a ${button}-button click on the backdrop`, async () => {
    await openPopup("settings", () => openSettings("github"));
    await page.mouse.click(BACKDROP.x, BACKDROP.y, { button });
    if (!(await isOpen("settings"))) throw new Error(`closed on a ${button}-button click`);
  });
}

// Closing goes through the popup's own close function, not a bare class removal: the mailbox's
// close is the one with a visible side, the poller it stops. Fails if the helper is handed
// `() => mbx.classList.remove("open")` instead of `closeMailbox`.
await check("a click outside the mailbox closes it the way its ✕ does, stopping its poller", async () => {
  await openPopup("mbx", () => openMailbox());
  await page.mouse.click(BACKDROP.x, BACKDROP.y);
  const poll = await page.evaluate(() => mbxPoll);
  if (poll !== null) throw new Error(`the poller is still ${poll}`);
});

// The exception. Fails if #loginterm is given `closeOnBackdrop` like the others.
await check("the login terminal does not close on a click outside, and does on its ✕", async () => {
  await closeEverything();
  await page.evaluate(() => openLoginTerminal("claude"));
  await page.waitForFunction(() => document.getElementById("loginterm").classList.contains("open") && loginTermState?.ws.readyState === 1);
  const top = await page.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.id, BACKDROP);
  if (top !== "loginterm") throw new Error(`the point meant to be the backdrop is #${top}`);
  await page.mouse.click(BACKDROP.x, BACKDROP.y);
  await page.waitForTimeout(300);
  if (!(await page.evaluate(() => document.getElementById("loginterm").classList.contains("open")))) {
    throw new Error("a click outside closed the login terminal");
  }
  await page.locator("#loginterm .x").click();
  await page.waitForFunction(() => !document.getElementById("loginterm").classList.contains("open"));
});

await check("no page errors while the popups were driven", () => {
  if (noise.length) throw new Error(noise.slice(0, 5).join(" | "));
});

// ---------- report ----------
const failed = report({ log });
if (failed.length) console.log(`fixture kept for inspection: ${fx.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
process.exit(failed.length || leftRunning.length ? 1 : 0);
