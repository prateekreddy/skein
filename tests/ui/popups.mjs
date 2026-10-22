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
// # What a popup must not throw away
//
// Three ways a popup lost something a person had done (SKEIN-1259, 1260, 1261), checked at the end
// of this file because each is about a popup. A click on the "Write token" title of a repo card
// pressed Forget — the card was a <label>, and a label hands a click to its first control — and
// every Forget deleted at once. Settings closed on Escape, ✕ or a click outside with its edits
// unsaved, and reopening reset them. And the three request popups rebuilt their list on a timer,
// wiping a typed answer, a changed duration, a ticked box. The repo card and its token here are
// real files the real server reads; the requests and the work-tracking connection are stubbed
// routes, so the test decides when a refresh brings something new, and every write under
// `/api/fleet/` is answered by a stand-in rather than the server (SKEIN-1290).
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
  // One repo with a write token of its own, for the Forget checks. `read_prs: false`, so nothing
  // here goes looking for its pull requests.
  fs.mkdirSync(path.join(root, "work"));
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([
    { id: "acme", source: "https://github.com/acme/thing.git", source_tree: path.join(root, "work"), read_prs: false,
      store: path.join(root, "store"), agent: "claude", plane_project: "", sync_connection: "" },
  ]));
  fs.writeFileSync(path.join(home, "github-pats.json"),
    JSON.stringify([{ id: "acme-thing", label: "acme/thing", repos: ["acme/thing"] }]));
  fs.mkdirSync(path.join(home, "github-pats"), { mode: 0o700 });
  fs.writeFileSync(path.join(home, "github-pats", "acme-thing"), "skein-test-write-token", { mode: 0o600 });
  // The stand-in for sbx, so nothing the page asks reaches whatever `sbx` this machine has.
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, "#!/usr/bin/env bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
  fs.chmodSync(sbx, 0o755);
  // A `gh` with no login, so this machine's own cannot answer for a token.
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
    // Nowhere: nothing in this suite may ask GitHub anything.
    SKEIN_GITHUB_API: "http://127.0.0.1:9",
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

/** Settings on the GitHub pane, with focus in its plain-text GitHub App ID field, emptied. */
const focusSettingsField = async () => {
  await page.evaluate(() => openSettings("github"));
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

// ---------- Forget asks first, and a title is not a button (SKEIN-1259) ----------
console.log("\nForget, on a repo's write token and a connection's Plane token");

// **No write to `/api/fleet/` reaches the server from here on** (SKEIN-1290): a fixture server's
// fleet routes can reach a real fleet's queues, so every non-GET under it is answered by this
// stand-in, and what the page sent is read from its request rather than from the server.
await page.route(/\/api\/fleet\//, r => r.request().method() === "GET" ? r.fallback() : r.fulfill({ json: {} }));
// Every DELETE the page sends, and every dialog it raises, with what the dialog said. Each check
// says how the dialog is answered; none is answered by default.
const deletes = [];
page.on("request", r => { if (r.method() === "DELETE") deletes.push(new URL(r.url()).pathname); });
let answerDialog = null;
const dialogs = [];
page.on("dialog", d => { dialogs.push(d.message()); (answerDialog ? d.accept() : d.dismiss()).catch(() => {}); });
const tokenFile = path.join(fx.home, "github-pats", "acme-thing");
const card = '#set-repos [data-repotoken="acme/thing"]';
/** Settings on Repositories, with the repo's card unfolded — it opens folded. */
const unfoldRepoCard = async () => {
  await page.waitForSelector('#settings.open .rcard[data-card="acme"] .rhead');
  if (!(await page.locator('.rcard[data-card="acme"]').evaluate(c => c.classList.contains("open")))) {
    await page.locator('.rcard[data-card="acme"] .rhead').click();
  }
};
const openRepoCard = async () => {
  await closeEverything();
  await page.evaluate(() => openSettings("repos"));
  await unfoldRepoCard();
  await page.waitForFunction(sel => {
    const f = document.querySelector(sel)?.closest(".set-field");
    return f && f.textContent.includes("token stored");
  }, card);
  deletes.length = 0; dialogs.length = 0; answerDialog = null;
};
const tokenField = () => page.locator(card).locator("xpath=ancestor::*[contains(@class,'set-field')][1]");

// Fails if the card's token block is a <label> again: the click on its title goes to Forget, which
// raises the question (or, without it, sends the DELETE).
await check("a click on a repo's \"Write token\" title neither asks to forget nor forgets", async () => {
  await openRepoCard();
  await tokenField().locator(".set-title").click({ position: { x: 10, y: 5 } });
  await tokenField().locator(".desc").first().click();
  await page.waitForTimeout(300);
  if (deletes.length || dialogs.length || !fs.existsSync(tokenFile)) {
    throw new Error(`DELETEs ${JSON.stringify(deletes)}, asked ${JSON.stringify(dialogs)}, token on disk: ${fs.existsSync(tokenFile)}`);
  }
});

// Fails if `removeGitCred` stops asking: the DELETE goes out and the file is gone.
await check("Forget on a repo's write token asks first, and Cancel keeps the token", async () => {
  await openRepoCard();
  await tokenField().locator("button", { hasText: "Forget" }).click();
  await page.waitForTimeout(300);
  const said = dialogs.join(" | ");
  if (!said.startsWith("Forget the write token for acme/thing?") || deletes.length || !fs.existsSync(tokenFile)) {
    throw new Error(`asked ${JSON.stringify(said)}, DELETEs ${JSON.stringify(deletes)}, token on disk: ${fs.existsSync(tokenFile)}`);
  }
});

// The other answer, so the question is not a wall. The DELETE is answered by the stand-in above,
// so what is checked is that the page sent it. Fails if the confirm's result is ignored either
// way, or Forget stops sending the DELETE.
await check("Forget, answered OK, sends the DELETE for that token", async () => {
  await openRepoCard();
  answerDialog = "accept";
  await tokenField().locator("button", { hasText: "Forget" }).click();
  await page.waitForTimeout(300);
  answerDialog = null;
  if (deletes.join() !== "/api/fleet/git-credentials/acme-thing" || dialogs.length !== 1) {
    throw new Error(`DELETEs ${JSON.stringify(deletes)}, asked ${JSON.stringify(dialogs)}`);
  }
});

// A work-tracking connection with a stored Plane token, stubbed: storing a real one is a POST this
// suite has no other use for. The DELETE is stubbed too, so a failing check deletes nothing real.
const conn = { id: "skein-test-conn", label: "test backlog", gateway_url: "https://gateway.invalid/", token_set: true, ready: true, repos: [] };
await page.route("**/api/sync", r => r.request().method() === "GET"
  ? r.fulfill({ json: { ready: true, connections: [conn] } }) : r.fallback());
await page.route("**/api/sync/connections/*/token", r => r.fulfill({ json: { ready: false, connections: [{ ...conn, token_set: false, ready: false }] } }));

// Fails if `forgetConnectionToken` stops asking: the DELETE goes out on the first press.
await check("Forget on a connection's Plane token asks first, and Cancel sends nothing", async () => {
  await closeEverything();
  await page.evaluate(() => openSettings("tracking"));
  await page.waitForSelector('#settings.open .ccard[data-conn="skein-test-conn"] [data-forget]');
  deletes.length = 0; dialogs.length = 0; answerDialog = null;
  await page.locator('.ccard[data-conn="skein-test-conn"] [data-forget]').click();
  await page.waitForTimeout(300);
  const said = dialogs.join(" | ");
  if (!said.startsWith("Forget the Plane token for test backlog?") || deletes.length) {
    throw new Error(`asked ${JSON.stringify(said)}, DELETEs ${JSON.stringify(deletes)}`);
  }
});
await page.unroute("**/api/sync");
await page.unroute("**/api/sync/connections/*/token");

// ---------- Settings with unsaved changes (SKEIN-1260) ----------
console.log("\nSettings with unsaved changes");

const settingsPosts = [];
page.on("request", r => {
  if (r.method() === "POST" && new URL(r.url()).pathname === "/api/settings") settingsPosts.push(JSON.parse(r.postData() || "{}"));
});
const leaveShown = () => page.evaluate(() => {
  const n = document.getElementById("set-leave");
  return !n.hidden && n.getBoundingClientRect().height > 0 ? n.textContent.replace(/\s+/g, " ").trim() : "";
});
/** Settings on the GitHub pane with the App ID typed in, which Save would send and nothing else saves. */
const editSettings = async value => {
  await closeEverything();
  await page.evaluate(() => openSettings("github"));
  await page.waitForFunction(() => settingsModal().classList.contains("open"));
  await page.locator("#set-ghappid").fill(value);
  settingsPosts.length = 0;
};
const storedAppId = () => page.evaluate(() => fetch("/api/settings").then(r => r.json()).then(s => s.github_app_id || ""));
/** Kept open, saying so, with the edit still in the field. */
const keptOpen = async value => {
  const said = await leaveShown();
  const field = await page.locator("#set-ghappid").inputValue();
  if (!(await isOpen("settings")) || said !== "You have unsaved changes Save Discard" || field !== value || settingsPosts.length) {
    throw new Error(`open ${await isOpen("settings")}, notice ${JSON.stringify(said)}, field ${JSON.stringify(field)}, saves ${settingsPosts.length}`);
  }
};

// Each fails if that way out is pointed back at `closeSettings`: the dialog closes with the edit.
await check("dirty Settings + Escape stays open and says there are unsaved changes", async () => {
  await editSettings("4242");
  await page.keyboard.press("Escape");
  await keptOpen("4242");
});
await check("dirty Settings + its ✕ stays open and says there are unsaved changes", async () => {
  await editSettings("4242");
  await page.locator("#settings .set-x").click();
  await keptOpen("4242");
});
await check("dirty Settings + a click outside stays open and says there are unsaved changes", async () => {
  await editSettings("4242");
  await page.mouse.click(BACKDROP.x, BACKDROP.y);
  await keptOpen("4242");
});
// Fails if the global key handler (focus outside the dialog) still closes straight away.
await check("dirty Settings + Escape with the focus outside the dialog stays open too", async () => {
  await editSettings("4242");
  await page.evaluate(() => document.activeElement?.blur?.());
  await page.keyboard.press("Escape");
  await keptOpen("4242");
});

// Fails if Discard is not wired, or saves: the dialog stays, or a POST goes out.
await check("Discard drops the edit and closes", async () => {
  await editSettings("4242");
  await page.keyboard.press("Escape");
  await page.locator("#set-leave-discard").click();
  await page.waitForFunction(() => !settingsModal().classList.contains("open"));
  await page.waitForTimeout(200);
  const stored = await storedAppId();
  if (settingsPosts.length || stored !== "") throw new Error(`saves ${settingsPosts.length}, stored ${JSON.stringify(stored)}`);
});
// Fails if `.set-leave` is dropped from the dialog's Enter guard: Enter on Discard saves the form.
await check("Enter on Discard discards rather than saving", async () => {
  await editSettings("4242");
  await page.keyboard.press("Escape");
  await page.locator("#set-leave-discard").focus();
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => !settingsModal().classList.contains("open"));
  await page.waitForTimeout(200);
  if (settingsPosts.length || (await storedAppId()) !== "") throw new Error(`saves ${settingsPosts.length}`);
});
// Fails if Save on the notice is not wired to `saveSettings`: nothing is stored and it stays open.
await check("Save on the notice saves the edit and closes", async () => {
  await editSettings("4242");
  await page.mouse.click(BACKDROP.x, BACKDROP.y);
  await page.locator("#set-leave-save").click();
  await page.waitForFunction(() => !settingsModal().classList.contains("open"));
  const stored = await storedAppId();
  if (settingsPosts.length !== 1 || settingsPosts[0].github_app_id !== "4242" || stored !== "4242") {
    throw new Error(`saves ${JSON.stringify(settingsPosts)}, stored ${JSON.stringify(stored)}`);
  }
  // Put it back, through the page, so nothing after this reads a stored App ID.
  await editSettings("");
  await page.locator("#set-go").click();
  await page.waitForFunction(() => !settingsModal().classList.contains("open"));
});

// Fails if `leaveSettings` shows the notice whatever the flag says.
for (const [how, act] of [
  ["Escape", () => page.keyboard.press("Escape")],
  ["its ✕", () => page.locator("#settings .set-x").click()],
  ["a click outside", () => page.mouse.click(BACKDROP.x, BACKDROP.y)],
]) {
  await check(`Settings with nothing unsaved closes on ${how}, as before`, async () => {
    await closeEverything();
    await page.evaluate(() => openSettings("github"));
    await page.waitForFunction(() => settingsModal().classList.contains("open"));
    await act();
    await page.waitForFunction(() => !settingsModal().classList.contains("open"));
  });
}
// A repo card saves itself as you leave a field, so typing in one is nothing Save would send.
// Fails if `formEdit` stops leaving the self-saving panes out: the flag goes up and Escape holds.
await check("an edit on a card that saves itself does not hold Settings open", async () => {
  await closeEverything();
  await page.evaluate(() => openSettings("repos"));
  await unfoldRepoCard();
  await page.waitForSelector('#settings.open #set-repos input:not([type=password]):not([type=checkbox])');
  // Typed into and put back, so the card's own save has nothing to store.
  const field = page.locator('#set-repos input:not([type=password]):not([type=checkbox])').first();
  const was = await field.inputValue();
  await field.focus();
  await page.keyboard.type("x");
  await page.keyboard.press("Backspace");
  if ((await field.inputValue()) !== was) throw new Error("the field did not come back to what it was");
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => !settingsModal().classList.contains("open"));
});

// ---------- the request popups keep what you were doing when they refresh (SKEIN-1261) ----------
console.log("\nthe request popups, refreshed while you are in them");

// Each list is a stubbed route the test adds to, and each refresh is the poll the page's own timer
// calls — `pollAskq`, `pollGitq`, `pollSubq` — so the refresh happens when the test says.
const asks = [{ id: "q-1", box: BOX, question: "Which port?", asked: "2026-10-22T10:00:00Z", state: "waiting" }];
const answers = [];
await page.route("**/api/fleet/asks", r => r.fulfill({ json: asks }));
await page.route(/\/api\/fleet\/asks\/[^/]+$/, r => { answers.push(JSON.parse(r.request().postData() || "{}")); return r.fulfill({ json: {} }); });
const grants = { ready: true, requests: [{ id: "w-1", box: BOX, repo: "acme/other", reason: "a fix there", asked: "2026-10-22T10:00:00Z", state: "pending" }], grants: [], credentials: [] };
await page.route("**/api/fleet/git-grants", r => r.request().method() === "GET" ? r.fulfill({ json: grants }) : r.fulfill({ json: {} }));
const pkgs = [{ id: "p-1", box: BOX, kind: "apt", packages: ["libnss3"], asked: "2026-10-22T10:00:00Z", state: "pending" }];
await page.route("**/api/fleet/substrate", r => r.request().method() === "GET" ? r.fulfill({ json: pkgs }) : r.fulfill({ json: {} }));

const replyId = `aq-reply-${BOX}/q-1`;
// Fails if `loadAskq` rebuilds the list without `redrawKeeping`: the field comes back empty and
// unfocused. The new question is the other half — a refresh that skipped the redraw would pass
// the first and fail this one.
await check("a typed answer survives a refresh of Questions, which still shows a new question", async () => {
  await closeEverything();
  await page.evaluate(() => openAskq());
  const reply = page.locator(`[id="${replyId}"]`);
  await reply.waitFor();
  await reply.click();
  await page.keyboard.type("port 8080");
  asks.push({ id: "q-2", box: BOX, question: "Keep the old fixtures?", options: ["Yes", "No"], asked: "2026-10-22T10:01:00Z", state: "waiting" });
  await page.evaluate(() => pollAskq());
  await page.waitForFunction(() => document.getElementById("askq-list").textContent.includes("Keep the old fixtures?"));
  const got = await reply.inputValue();
  const at = await activeId();
  if (got !== "port 8080" || at !== replyId) throw new Error(`field ${JSON.stringify(got)}, focus on ${at}`);
  // And the caret where it was: typing on carries on at the end.
  await page.keyboard.type("!");
  if ((await reply.inputValue()) !== "port 8080!") throw new Error(`typing after the refresh gave ${JSON.stringify(await reply.inputValue())}`);
});
// Fails if the keydown listener on the list is removed: Enter sends nothing.
await check("Enter in a question's answer field sends the answer", async () => {
  answers.length = 0;
  await page.locator(`[id="${replyId}"]`).press("Enter");
  await page.waitForTimeout(300);
  if (answers.length !== 1 || answers[0].answer !== "port 8080!" || answers[0].box !== BOX) throw new Error(`sent ${JSON.stringify(answers)}`);
});

// Fails if `loadGitq` rebuilds without `redrawKeeping`: the hours go back to 24 and keep unticks.
await check("a changed duration and a ticked keep survive a refresh of Repo write access, which still shows a new request", async () => {
  await closeEverything();
  await page.evaluate(() => openGitq());
  const hours = page.locator(`[id="gq-h-${BOX}/w-1"]`);
  await hours.waitFor();
  await hours.fill("6");
  await page.locator(`[id="gq-keep-${BOX}/w-1"]`).check();
  grants.requests.push({ id: "w-2", box: BOX, repo: "acme/third", reason: "another", asked: "2026-10-22T10:02:00Z", state: "pending" });
  await page.evaluate(() => pollGitq());
  await page.waitForFunction(() => document.getElementById("gitq-list").textContent.includes("acme/third"));
  const got = [await hours.inputValue(), await page.locator(`[id="gq-keep-${BOX}/w-1"]`).isChecked()];
  if (got[0] !== "6" || got[1] !== true) throw new Error(`hours ${got[0]}, keep ${got[1]}`);
});

// Fails if `loadSubq` rebuilds without `redrawKeeping`: remember comes back ticked.
await check("an unticked remember survives a refresh of Package requests, which still shows a new request", async () => {
  await closeEverything();
  await page.evaluate(() => openSubq());
  const rem = page.locator(`[id="sq-rem-${BOX}/p-1"]`);
  await rem.waitFor();
  await rem.uncheck();
  pkgs.push({ id: "p-2", box: BOX, kind: "npm", packages: ["skein-test-pkg"], asked: "2026-10-22T10:03:00Z", state: "pending" });
  await page.evaluate(() => pollSubq());
  await page.waitForFunction(() => document.getElementById("subq-list").textContent.includes("skein-test-pkg"));
  if (await rem.isChecked()) throw new Error("remember came back ticked");
  if (!(await page.locator(`[id="sq-rem-${BOX}/p-2"]`).isChecked())) throw new Error("the new request's remember is not as rendered");
});
await page.unroute("**/api/fleet/asks");
await page.unroute(/\/api\/fleet\/asks\/[^/]+$/);
await page.unroute("**/api/fleet/git-grants");
await page.unroute("**/api/fleet/substrate");
await closeEverything();

await check("no page errors while the popups were driven", () => {
  if (noise.length) throw new Error(noise.slice(0, 5).join(" | "));
});

// ---------- report ----------
const failed = report({ log });
if (failed.length) console.log(`fixture kept for inspection: ${fx.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
process.exit(failed.length || leftRunning.length ? 1 : 0);
