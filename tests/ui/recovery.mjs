// When skein refuses a terminal, can the person get out — without being told to press anything?
//
// Three refusals, three different answers, and the point of this suite is that they are different
// (SKEIN-702). `terminal_session`, `login_session` and `pump_pty` used to write a sentence and
// RETURN, which drops the socket with no close code: the browser read 1006, decided the connection
// had gone away, and laid the 82%-opaque reconnect card over the one line that said why. So the
// person was shown "session not connected · click to reconnect" over "too many terminals open —
// close one and retry", and offered a reconnect to something that had never started.
//
// They now close with `CLOSE_NOTHING_TO_RECONNECT` (4001) and a reason whose first word says what
// the pane should do next:
//
//   - `wait-pty` — the server is the thing that releases a PTY permit, so it says so on the board's
//     stream (`Tick::PtyFreed`) and the pane reconnects itself. **No click.**
//   - `wait-box` — a box arriving is already a transition the board carries, so the pane waits for
//     that and reconnects itself. No click.
//   - `retry` — `pump_pty`'s own four failures in a box terminal (SKEIN-883). Nothing names the moment
//     they clear, so the pane tries again at 3s, 10s, 30s, 60s and 120s and then offers the button.
//     The login modal keeps `no-watch` for the same four, and the button.
//
// # What is asserted, and why it is pixels
//
// The defect was a sentence in the DOM under an opaque card, so the DOM is not the witness.
// `document.elementFromPoint` is, over **every written row of the terminal** rather than only the
// first — the two refusals it reads occupy two and six rows of the pane, and a cover that started
// below the first row would be the same defect one line down. `panecover.mjs` established the
// technique; this widens it to the whole of what was written. Measured, not assumed: with the close
// code taken away, this reports "2 of 2 written rows are behind something" and "6 of 6".
//
// # How each check is made to fail
//
// Named before it was written, then done, then restored from a copy checked with `md5sum`:
//
//   - "the pane comes back on its own when a terminal closes" — delete the `skein::stream::pty_freed()`
//     call in `PtySlot::drop` (src/bin/skein-server/terminal.rs). The slot still frees; nothing says so; the
//     pane waits for ever and this times out.
//   - "…and nobody had to click anything to make it" — have `es.addEventListener("pty-freed", …)`
//     call nothing. Same red, and the two together separate the two halves of the mechanism.
//   - "the refusal is uncovered, every line of it" — send an ordinary close instead of 4001 from
//     `refuse`, so the page takes the dropped-connection branch and the card goes back up.
//   - "a pane that is watching says what for, and offers no button" — give `wait-pty` the
//     `no-watch` row in `RECOVERY` (src/web/index.html), so a watching pane grows a button.
//   - "a pane that is watching nothing offers the control and says why" — the same edit backwards.
//   - "the box arriving brings its pane back" — drop the `retryWaiting("wait-box", …)` loop from
//     `applyTick`.
//
//   node tests/ui/recovery.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { ledger, settler } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

// The sandbox every placement in this fixture names, and the one the config declares. They have to
// be the same string or `place::placed_boxes` filters the record out and the box never reaches the
// board — which is the condition the `wait-box` half of this suite recovers on.
const FLEET = "skein-recovery-fleet";

// The box whose terminal is refused for the PTY cap, and the box that does not exist yet.
const CAPPED = "recovery-capped";
const GHOST = "recovery-ghost";
// The box the 24 holder sockets are opened against. One name, twenty-four sockets: the cap is on
// the server and counts sockets, not boxes.
const HOLDER = "recovery-holder";

const API_TOKEN = "r".repeat(64);

// `PTY_MAX` in src/bin/skein-server/terminal.rs. Spelled here rather than read from the thing under test —
// a constant taken out of the code cannot check the code — and asserted below by the fact that the
// (PTY_MAX + 1)th terminal is the one that is refused.
const PTY_MAX = 24;

// What the holders and the recovered attach run. The holders must outlive the suite's own timeouts;
// the attach must say something a reader can see, so "did it come back" is a sentence rather than
// an inference about a socket.
const HOLD_CMD = "exec sleep 300";
const ATTACHED = "attached by the recovery fixture";

const { check, results, report } = ledger();

// **A terminal wraps, and a check that greps for a sentence must not fail on where the wrap fell.**
// These refusals are several lines of prose in a pane whose width the suite does not choose,
// so "close another terminal" arrives split across two buffer rows. xterm records which rows are
// continuations, so the wrap can be undone exactly rather than guessed at by joining everything.
const UNWRAP = () => {
  window.__unwrapped = buf => {
    const out = [];
    for (let i = 0; i < buf.length; i++) {
      const line = buf.getLine(i);
      const text = line?.translateToString(true) ?? "";
      if (line?.isWrapped && out.length) out[out.length - 1] += text;
      else out.push(text);
    }
    return out.map(l => l.replace(/\s+$/, "")).filter(l => l.trim()).join("\n");
  };
};


/** Everything under one fixture directory the caller has already made.
 *
 * **The `freshFixture` call is at the call site with a double-quoted literal in it, deliberately.**
 * `tests/ui/harness/leaks.mjs` derives the names it hunts for by reading `freshFixture(…, "…")` out
 * of these files, and it can only read a literal — a name built from a template would leave both of
 * this suite's fixtures invisible to the leak check, which is SKEIN-647's failure exactly. */
function makeFixture(root) {
  const home = path.join(root, "home");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(path.join(root, "fleet"), { recursive: true });
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: FLEET }));
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  // The stand-in for sbx. The terminal route asks the fleet for the box's directory on its way
  // past, and without a stub that question reaches whatever `sbx` this machine happens to have.
  // `#!/bin/bash` with the interpreter spelled absolutely, because one of the two servers below
  // runs with a `$PATH` that resolves nothing at all.
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, "#!/bin/bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
  fs.chmodSync(sbx, 0o755);
  return { root, home, bin, sbx };
}

/** Write the placement record that makes `fleet::absent_box_reason` stop refusing a box.
 *
 * A placement record is the whole of what skein knows about a box in the fleet, so this is exactly
 * what `skein start` leaves behind and exactly what the refusal says is missing. `generation` and
 * `ns_start` are left unset on purpose: unstamped is "unprovable", which `ensure_box_session` answers
 * by leaving the session alone rather than by trying to rebuild a box this fixture does not have. */
function placeBox(home, name) {
  const dir = path.join(home, "places");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(
    path.join(dir, `${name}.json`),
    JSON.stringify({ sandbox: FLEET, ns_pid: 0, home: "", tree: "" }),
  );
}

const fx = makeFixture(freshFixture(fixtureRoot(), "skein-recovery-ui"));
placeBox(fx.home, CAPPED);   // the cap is what refuses this one, not its absence

const door = await openDoor();
const port = door.port;
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,
    SKEIN_LAUNCH_CMD: HOLD_CMD,
    SKEIN_ATTACH_CMD: `printf '${ATTACHED}\\r\\n'; exec sleep 300`,
    PATH: `${fx.bin}:${process.env.PATH}`,
  },
});

// The second server exists for one reason: `pump_pty`'s spawn failure is only reachable when the
// program behind the terminal cannot be started, and every seam this fixture has (`$SKEIN_LAUNCH_CMD`,
// `$SKEIN_ATTACH_CMD`) is handed to `sh`. So this one runs with a `$PATH` holding nothing but its own
// `sbx` stub: `sh` does not resolve, `spawn_command` fails with ENOENT, and the pane meets the one
// class of refusal skein cannot watch its way out of.
const fx2 = makeFixture(freshFixture(fixtureRoot(), "skein-recovery-nosh"));
const door2 = await openDoor();
const port2 = door2.port;
const { log: log2 } = await startServer({
  door: door2,
  token: API_TOKEN,
  env: {
    SKEIN_HOME: fx2.home,
    SKEIN_FLEET_ROOT: path.join(fx2.root, "fleet"),
    SKEIN_REGISTRY: path.join(fx2.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx2.sbx} ls --json`,
    SKEIN_LAUNCH_CMD: HOLD_CMD,
    // Through `sh` as well, so a try the pane makes on its own — which reconnects rather than
    // re-launching — meets the same spawn failure rather than an `sbx` stub that starts (SKEIN-883).
    SKEIN_ATTACH_CMD: HOLD_CMD,
    PATH: fx2.bin,
  },
});
// Placed, so a reconnect gets past `absent_box_reason` to the spawn — without it every automatic try
// would be refused as `wait-box` and the backoff below would be measuring the wrong refusal.
placeBox(fx2.home, "recovery-nospawn");

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const settle = settler(page, 400);
page.setDefaultTimeout(8000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("response", r => { if (r.status() >= 500) noise.push(`[${r.status()}] ${r.url()}`); });

await page.goto(`http://127.0.0.1:${port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await page.waitForFunction(() => typeof createSession === "function" && typeof sessions !== "undefined");

// **Every click anywhere on this page, counted in the capture phase.** "No click was needed" is the
// claim this whole suite exists to make, and a suite that merely refrains from clicking is asserting
// something about itself. This counts the page's own: a recovery implemented by synthesising a press
// on the Try-again button — which is exactly the shortcut that looks identical from the outside —
// registers here and fails the checks below while leaving "the pane comes back" green.
await page.evaluate(UNWRAP);
await page.evaluate(() => {
  window.__clicks = 0;
  document.addEventListener("click", () => { window.__clicks++; }, true);
  document.addEventListener("pointerdown", () => { window.__clicks++; }, true);
});
const clicksSoFar = () => page.evaluate(() => window.__clicks);

/** Open a box terminal the way a tab click does, and put it in front. */
const open = (box, branch = null) => page.evaluate(([b, br]) => {
  createSession(b, "agent", br, "claude");
  view = { box: b, mode: "term", kind: "agent" };
  applyView();
}, [box, branch]);

/** What the reader of this pane has in front of them.
 *
 * `on` is the element their eye lands on at the pixels of **each written row**, which is the whole
 * assertion: these refusals are several lines long, and a cover that starts below the first one is
 * the same defect one line down. */
const probe = box => page.evaluate(b => {
  const s = sessions.get(sidOf(b, "agent"));
  if (!s) return { missing: true };
  const at = (x, y) => {
    const el = document.elementFromPoint(x, y);
    if (!el) return "nothing";
    if (el.closest(".disco")) return "the reconnect overlay";
    if (el.closest(".recover")) return "the recovery strip";
    return `${el.tagName.toLowerCase()}.${el.className || "-"}`;
  };
  const buf = s.term.buffer.active;
  const lines = [];
  for (let i = 0; i < buf.length; i++) lines.push(buf.getLine(i)?.translateToString(true) ?? "");
  const er = s.term.element.getBoundingClientRect();
  const rowH = er.height / s.term.rows;
  const written = [];
  for (let i = 0; i < s.term.rows && i < lines.length; i++) {
    if (!lines[i].trim()) continue;
    written.push({ text: lines[i].trim().slice(0, 70), on: at(er.left + 24, er.top + rowH * i + rowH / 2) });
  }
  const disco = s.host.querySelector(".disco");
  const recover = s.host.querySelector(".recover");
  return {
    dead: !!s.dead,
    ended: !!s.ended,
    waitFor: s.waitFor || "",
    written,
    // Only the two things this page can put between a reader and the terminal. Everything else at
    // those pixels is one of xterm's own layers, which IS the sentence being drawn.
    covered: written.filter(w => w.on === "the reconnect overlay" || w.on === "the recovery strip"),
    offscreen: written.filter(w => w.on === "nothing"),
    discoDisplay: disco ? getComputedStyle(disco).display : "no overlay in the pane at all",
    discoArea: disco ? Math.round(disco.getBoundingClientRect().width * disco.getBoundingClientRect().height) : 0,
    strip: recover ? recover.textContent.replace(/\s+/g, " ").trim() : "",
    stripShown: recover ? getComputedStyle(recover).display !== "none" : false,
    button: !!recover?.querySelector(".recover-go"),
    watching: !!recover?.querySelector(".recover-w"),
    said: window.__unwrapped(buf),
  };
}, box);

const dead = box => page.waitForFunction(
  b => !!sessions.get(sidOf(b, "agent"))?.dead, box, { timeout: 20000 });
const alive = (box, ms) => page.waitForFunction(
  b => { const s = sessions.get(sidOf(b, "agent")); return !!s && !s.dead && s.ws.readyState === 1; },
  box, { timeout: ms });
const says = (box, what, ms = 20000) => page.waitForFunction(
  ([b, w]) => {
    const s = sessions.get(sidOf(b, "agent"));
    if (!s) return false;
    const buf = s.term.buffer.active;
    for (let i = 0; i < buf.length; i++) {
      if ((buf.getLine(i)?.translateToString(true) ?? "").includes(w)) return true;
    }
    return false;
  }, [box, what], { timeout: ms });

/** Hold `n` terminal permits with raw sockets the page owns but `sessions` does not know about.
 *
 * Raw, because the cap counts SOCKETS and these must not be panes: a pane per permit would be
 * twenty-four xterms fighting the one this suite is about for the viewport. They are opened from
 * inside the page so the session cookie carries itself. */
const holdPermits = n => page.evaluate(([box, count]) => {
  window.__holders = [];
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const opened = [];
  for (let i = 0; i < count; i++) {
    const ws = new WebSocket(`${proto}://${location.host}/api/boxes/${encodeURIComponent(box)}/terminal?launch=hold&agent=claude`);
    ws.binaryType = "arraybuffer";
    window.__holders.push(ws);
    opened.push(new Promise(done => { ws.onopen = () => done(true); ws.onerror = () => done(false); }));
  }
  return Promise.all(opened).then(r => r.filter(Boolean).length);
}, [HOLDER, n]);

/** Let one holder go, which is a person closing a terminal to free a slot. */
const releaseOne = () => page.evaluate(() => { window.__holders.pop().close(); });

try {
  // ---------- 1. refused for the cap: waits, says what for, and comes back by itself ----------
  console.log(`\na terminal refused because all ${PTY_MAX} are open`);
  const held = await holdPermits(PTY_MAX);
  await check(`all ${PTY_MAX} terminal slots are actually held before the refusal is asked for`, () => {
    // The control the rest of this section rests on. Without it a refusal that never happened and a
    // refusal that happened for another reason look identical from here.
    if (held !== PTY_MAX) throw new Error(`only ${held} of ${PTY_MAX} holder sockets opened`);
  });
  await open(CAPPED);
  await dead(CAPPED);
  await settle();
  const capped = await probe(CAPPED);

  await check("the refusal says what happened, names the limit, and says the pane comes back", () => {
    if (!/too many terminals open/.test(capped.said)) throw new Error(`the pane holds ${JSON.stringify(capped.said.slice(0, 240))}`);
    if (!capped.said.includes(`${PTY_MAX} at once is the limit`)) throw new Error(`the refusal does not name the limit: ${JSON.stringify(capped.said.slice(0, 240))}`);
    if (!/reopens on its own/.test(capped.said)) throw new Error(`the refusal does not say it recovers: ${JSON.stringify(capped.said.slice(0, 240))}`);
  });

  // THE defect, and it is measured in pixels rather than in the DOM: the overlay is in the DOM
  // either way, and a sentence under an opaque card is exactly the bug.
  await check("the refusal is uncovered, every line of it", () => {
    if (!capped.written.length) throw new Error("the pane has nothing written in it at all");
    if (capped.offscreen.length) throw new Error(`${capped.offscreen.length} written rows are not on screen at all, so nothing here was measured`);
    if (capped.covered.length) {
      throw new Error(`${capped.covered.length} of ${capped.written.length} written rows are behind something: `
        + capped.covered.map(w => `"${w.text}" → ${w.on}`).join(" | "));
    }
    if (capped.discoDisplay !== "none" || capped.discoArea !== 0) {
      throw new Error(`the reconnect panel is laid out over the pane: display ${capped.discoDisplay}, ${capped.discoArea}px²`);
    }
  });

  await check("the server said there is nothing to reconnect to, and said to wait for a slot", () => {
    if (!capped.ended) throw new Error("the close carried no 4001, so the page cannot tell a refusal from a dropped connection");
    if (capped.waitFor !== "wait-pty") throw new Error(`the close reason's first word is ${JSON.stringify(capped.waitFor)}`);
  });

  await check("a pane that is watching says what for, and offers no button", () => {
    if (!capped.stripShown) throw new Error("the pane says nothing about what happens next");
    if (!/waiting for a terminal to close/.test(capped.strip)) throw new Error(`the strip reads "${capped.strip}"`);
    if (!capped.watching) throw new Error("nothing on the pane shows it is waiting rather than stuck");
    if (capped.button) throw new Error(`a pane that reconnects itself is also asking to be clicked: "${capped.strip}"`);
  });

  // ---------- and the recovery itself ----------
  console.log("\nand a terminal closing while it waits");
  await releaseOne();
  let recovered = true;
  try { await alive(CAPPED, 20000); } catch { recovered = false; }
  await check("the pane comes back on its own when a terminal closes", async () => {
    if (!recovered) {
      const now = await probe(CAPPED);
      throw new Error(`the pane is still closed ${JSON.stringify({ waitFor: now.waitFor, strip: now.strip })}`);
    }
    await says(CAPPED, ATTACHED);
  });

  const clicked = await clicksSoFar();
  await check("and nobody had to click anything to make it", () => {
    if (clicked !== 0) throw new Error(`${clicked} clicks landed on the page before the pane came back`);
  });

  const back = await probe(CAPPED);
  await check("the recovered pane is a working terminal, with its own history above it", () => {
    if (back.dead) throw new Error("the pane reconnected and closed again");
    if (back.stripShown) throw new Error(`the recovery strip is still up on a live pane: "${back.strip}"`);
    if (!/too many terminals open/.test(back.said)) throw new Error("the refusal that explains the gap was thrown away on the way back");
    if (!back.said.includes(ATTACHED)) throw new Error(`the recovered pane never attached: ${JSON.stringify(back.said.slice(-240))}`);
  });

  // ---------- 2. refused because the box is not there ----------
  //
  // Every remaining holder goes first. The cap is checked BEFORE the box is looked up, so a suite
  // that left 23 sockets open would get the cap's refusal here and read it as the box's — which is
  // exactly what the first run of this suite did.
  console.log("\na terminal for a box that does not exist");
  await page.evaluate(() => { while (window.__holders.length) window.__holders.pop().close(); });
  await settle();
  await open(GHOST);
  await dead(GHOST);
  await settle();
  const ghost = await probe(GHOST);

  await check("the refusal names the command to run and says skein is watching for the box", () => {
    if (!/does not exist/.test(ghost.said)) throw new Error(`the pane holds ${JSON.stringify(ghost.said.slice(0, 240))}`);
    if (!ghost.said.includes(`skein start ${GHOST}`)) throw new Error("the refusal lost the command that fixes it");
    if (!/watching the board/.test(ghost.said)) throw new Error(`the refusal does not say it recovers: ${JSON.stringify(ghost.said.slice(0, 300))}`);
  });

  await check("and that refusal is uncovered too, every line of it", () => {
    if (!ghost.written.length) throw new Error("the pane has nothing written in it at all");
    if (ghost.offscreen.length) throw new Error(`${ghost.offscreen.length} written rows are not on screen at all, so nothing here was measured`);
    if (ghost.covered.length) {
      throw new Error(`${ghost.covered.length} of ${ghost.written.length} written rows are behind something: `
        + ghost.covered.map(w => `"${w.text}" → ${w.on}`).join(" | "));
    }
    if (ghost.waitFor !== "wait-box") throw new Error(`the close reason's first word is ${JSON.stringify(ghost.waitFor)}`);
    if (ghost.button) throw new Error(`a pane that reconnects itself is also asking to be clicked: "${ghost.strip}"`);
    if (!/waiting for the box/.test(ghost.strip)) throw new Error(`the strip reads "${ghost.strip}"`);
  });

  // The box arrives: a placement record is the whole of what skein knows about a box in the fleet,
  // so writing one is what `skein start` leaves behind, and it is what puts the row on the board.
  placeBox(fx.home, GHOST);
  let cameBack = true;
  try { await alive(GHOST, 25000); } catch { cameBack = false; }
  await check("the box arriving brings its pane back, with no click", async () => {
    if (!cameBack) {
      const now = await probe(GHOST);
      throw new Error(`the pane is still closed ${JSON.stringify({ waitFor: now.waitFor, strip: now.strip })}`);
    }
    await says(GHOST, ATTACHED);
    const n = await clicksSoFar();
    if (n !== 0) throw new Error(`${n} clicks landed on the page`);
  });

  // ---------- 3. skein's own failure: tried again on a bounded backoff, then the button ----------
  //
  // SKEIN-883. `pump_pty`'s four failures close a box terminal with `retry`, and the pane tries
  // again by itself at 3s, 10s, 30s, 60s and 120s, then stops and offers the button. The clock is
  // Playwright's, so four minutes of backoff run in a few seconds and each try can be caught arriving
  // no earlier than its delay. The sockets are counted off the wire rather than read from the page,
  // so "it tried" is a connection the server saw and not a counter the code under test keeps.
  //
  // How each is made to fail: `scheduleRetry` given `[3000, 3000, 3000, 3000, 3000]` fails "no
  // earlier than its delay" at the second try; letting it run past the fifth (`n >= 6`) fails "and
  // stops after the fifth"; the server closing with `no-watch` again fails the first check below.
  console.log("\na terminal skein itself could not open");
  const page2 = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  page2.setDefaultTimeout(8000);
  await page2.clock.install();
  const tries = [];
  // And what the first one was told, off the wire: xterm parses what it is given on a timer, and the
  // page's timers are held still below, so its buffer is not the witness here.
  const told = [];
  page2.on("websocket", w => {
    if (!w.url().includes("/recovery-nospawn/terminal")) return;
    tries.push(w.url());
    if (tries.length === 1) w.on("framereceived", f => { if (typeof f.payload === "string") told.push(f.payload); });
  });
  await page2.goto(`http://127.0.0.1:${port2}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  await page2.waitForFunction(() => typeof createSession === "function" && typeof sessions !== "undefined");
  await page2.evaluate(UNWRAP);
  // From here the page's clock only moves when this says so, so every delay below is measured
  // exactly. Node-side waits only: a paused clock stops the page's own animation frames, which is
  // what `waitForFunction` polls on.
  await page2.clock.pauseAt(await page2.evaluate(() => Date.now() + 10));
  const nap = ms => new Promise(r => setTimeout(r, ms));
  const settled = async k => {
    for (const end = Date.now() + 20000; Date.now() < end; await nap(50)) {
      if (tries.length === k && await page2.evaluate(() => !!sessions.get(sidOf("recovery-nospawn", "agent"))?.dead)) return true;
    }
    return false;
  };
  await page2.evaluate(() => {
    createSession("recovery-nospawn", "agent", "x", "claude");
    view = { box: "recovery-nospawn", mode: "term", kind: "agent" };
    applyView();
  });
  if (!await settled(1)) throw new Error("the terminal skein could not open never came back refused");
  const pane = () => page2.evaluate(() => {
    const s = sessions.get(sidOf("recovery-nospawn", "agent"));
    const buf = s.term.buffer.active;
    const recover = s.host.querySelector(".recover");
    const disco = s.host.querySelector(".disco");
    return {
      waitFor: s.waitFor || "",
      dead: !!s.dead,
      said: window.__unwrapped(buf),
      strip: recover ? [...recover.children].filter(e => e.tagName !== "BUTTON")
        .map(e => e.textContent).filter(Boolean).join(" / ") : "",
      stripShown: recover ? getComputedStyle(recover).display !== "none" : false,
      button: !!recover?.querySelector(".recover-go"),
      discoDisplay: disco ? getComputedStyle(disco).display : "no overlay in the pane at all",
    };
  });
  const broke = await pane();

  await check("skein's own failure says it is skein's, that the pane tries again, and names what can be checked", () => {
    const said = told.join("");
    if (!/could not start the program behind this terminal/.test(said)) {
      throw new Error(`the socket said ${JSON.stringify(said.slice(0, 300))}`);
    }
    if (!/not anything you did — nothing in your box is wrong\. This pane tries again by itself for a few minutes\. If it keeps failing, the program named in the error is missing from where skein is running\./.test(said))
      throw new Error(`the sentence is not the approved one: ${JSON.stringify(said.slice(0, 400))}`);
  });

  await check("a pane skein could not open says it is trying again, and offers no button", () => {
    if (broke.waitFor !== "retry") throw new Error(`the close reason's first word is ${JSON.stringify(broke.waitFor)}`);
    if (!broke.stripShown) throw new Error("the pane says nothing about what happens next");
    if (broke.button) throw new Error(`a pane that tries again on its own is also asking to be clicked: "${broke.strip}"`);
    const want = "skein could not open this terminal — trying again / It retries on its own: next try in 3s (1 of 5). Nothing in your box is wrong.";
    if (broke.strip !== want) throw new Error(`the strip reads "${broke.strip}"`);
    if (broke.discoDisplay !== "none") throw new Error(`the reconnect panel is up as well: ${broke.discoDisplay}`);
  });

  const schedule = [3000, 10000, 30000, 60000, 120000];
  await check("each automatic try comes no earlier than its delay: 3s, 10s, 30s, 60s, 120s", async () => {
    for (const [i, wait] of schedule.entries()) {
      const k = i + 1;
      // What is left of the wait, on a clock that has not moved since the refusal landed — so it IS
      // the delay this try was scheduled with.
      const left = await page2.evaluate(() => sessions.get(sidOf("recovery-nospawn", "agent")).retryAt - Date.now());
      if (left !== wait) throw new Error(`try ${k} was scheduled ${left}ms out, not ${wait}ms`);
      const now = await pane();
      const told = `It retries on its own: next try in ${wait / 1000}s (${k} of 5). Nothing in your box is wrong.`;
      if (!now.strip.endsWith(told)) throw new Error(`before try ${k} the strip reads "${now.strip}"`);
      await page2.clock.runFor(left - 1);
      await nap(300);
      if (tries.length !== k) throw new Error(`try ${k} went out before its ${wait}ms were up (${tries.length} sockets)`);
      await page2.clock.runFor(2);
      if (!await settled(k + 1)) throw new Error(`try ${k} never went out, or never came back refused (${tries.length} sockets)`);
    }
  });

  await check("and stops after the fifth, handing over to the button", async () => {
    const done = await pane();
    const want = "skein could not open this terminal / It tried 5 times over 4 minutes. Press Try again once the cause above is fixed.";
    if (done.strip !== want) throw new Error(`the strip reads "${done.strip}"`);
    if (!done.button) throw new Error("no control once the tries are spent");
    await page2.clock.runFor(10 * 60 * 1000);
    await nap(300);
    if (tries.length !== 6) throw new Error(`${tries.length - 6} more tries went out after the fifth`);
  });

  await check("no page errors and no 5xx while any of that happened", () => {
    if (noise.length) throw new Error(noise.slice(0, 5).join(" | "));
  });
  await page2.close();
} catch (e) {
  // A suite that could not run is a failure, not a silence.
  await check("the recovery suite could run at all", () => { throw e; });
}

const shot = path.join(fx.root, "failure.png");
if (results.some(([ok]) => !ok)) await page.screenshot({ path: shot, fullPage: false });
const failed = report({ log: () => `${log()}\n---- the second server, whose PATH resolves no sh ----\n${log2()}` });
if (failed.length) console.log(`screenshot: ${shot}\nfixtures kept for inspection: ${fx.root} ${fx2.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root, fx2.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
if (!failed.length && process.env.SKEIN_KEEP) console.log(`fixtures kept (SKEIN_KEEP): ${fx.root} ${fx2.root}`);
process.exit(failed.length || leftRunning.length ? 1 : 0);
