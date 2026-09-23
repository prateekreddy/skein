// Does the cockpit cover the last thing a terminal said?
//
// It did, and the covered line was the only explanation on screen. A launch whose command cannot be
// started prints the shell's `sh: 1: skein: not found` on the PTY and exits 127 — measured against
// the kernel with `os.openpty`, the byte really does arrive before the master read returns EIO — so
// `pump_pty` sends it and `ws.onmessage` writes it into xterm. One moment later `ws.onclose` adds
// `.dead` to the pane, and the stylesheet drops a `position:absolute; inset:0` panel over the whole
// of it at `rgba(8,9,11,.82)`, over a terminal whose own background is `#08090b`. The reader is left
// with "session not connected · click to reconnect" and an offer to reconnect to a command that
// finished — which is why SKEIN-589 read as an empty terminal, and is SKEIN-672.
//
// For an attach the panel is right: the session was meant to be long-lived, the terminal died, and
// reconnecting is the thing to do. For a launch that ran to completion there is nothing to reconnect
// to. The two closes are indistinguishable at `ws.onclose`, so the server says which — a close code
// in RFC 6455's private range (`CLOSE_NOTHING_TO_RECONNECT`, 4001) when there is nothing here to
// reconnect to — which a child that ran to completion is, and so is a refusal (SKEIN-702).
//
// **Both directions are asserted here, because either one alone is satisfied by doing nothing.** A
// page that never draws the overlay passes the first; the overlay as it was passes the second.
//
// **And what is asserted is what a person can see, not what the DOM holds.** The overlay's absence
// from the DOM would be a weaker claim than this one: the panel is always in the DOM and is always
// `inset:0` when displayed, so the question is which element the reader's eye lands on at the pixels
// the error is drawn on. `document.elementFromPoint` answers that, at the top line and at the middle
// of the pane, and the terminal's own buffer supplies what is written there.
//
// # How each direction is made to fail
//
// - the launch: have the server send an ordinary close instead of that code, so a child
//   that exited takes the dropped-connection branch. "the pane a finished launch leaves is not
//   covered" then fails, naming the reconnect overlay as what is on top of the error.
// - the attach: have `ws.onclose` set `ended` for every close, so a dropped connection takes the
//   child-ended branch. "a connection that dropped still gets the overlay" then fails, with the
//   overlay's own box measuring zero.
//
// Both were done, and each time it was that check and no other that went red.
//
// # The two sessions
//
// One server, two boxes, and `$SKEIN_LAUNCH_CMD` — the seam `sandbox::launch_command_as` reads —
// switching on the branch the cockpit asks to launch: one command prints an error and exits 127
// (the real code, so `remember_launch_never_ran` records it exactly as it would in the field), the
// other prints a line and sleeps. The connection under the sleeping one is then dropped for real, by
// killing the server, which is the only way to produce the 1006 a browser reports for a socket that
// simply died.
//
//   node tests/ui/panecover.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { ledger, seeing, settler } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

// The box whose launch fails, and the box whose launch stays up. Two names, so the two panes are two
// sessions in the page's own `sessions` map and can be probed one after the other.
const ENDS = "panecover-ends";
const LIVES = "panecover-lives";

// See the note in smoke.mjs: written by the fixture rather than read back after startup, so no test
// races the server minting one.
const API_TOKEN = "t".repeat(64);

// What the terminal is meant to still be showing when the launch is over. `>&2` because that is
// where a shell writes `not found`, and on a PTY both descriptors are the same terminal — so this
// arrives exactly as the real one does.
const SAID_BY_THE_SHELL = "sh: 1: skein: not found";
const SAID_BY_THE_ATTACH = "attached to the box";

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "skein-panecover-ui");
  const home = path.join(root, "home");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(path.join(root, "fleet"), { recursive: true });
  fs.writeFileSync(path.join(home, "config.json"), "{}");
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  // The stand-in for sbx. This suite launches nothing real, but the terminal route asks the fleet
  // for the box's directory on its way past — and without a stub on `$PATH` that question reaches
  // whatever `sbx` the machine running this happens to have.
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  fs.writeFileSync(sbx, "#!/usr/bin/env bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
  fs.chmodSync(sbx, 0o755);
  return { root, home, bin, sbx };
}

// One launch that ends and one that does not, chosen by the branch the page asks for. The whole
// value is handed to `sh -c` by `terminal_session`, and `{branch}` is substituted shell-quoted.
const LAUNCH_CMD =
  `case {branch} in ` +
  `(ends) printf '${SAID_BY_THE_SHELL}\\r\\n' >&2; exit 127 ;; ` +
  `(*) printf '${SAID_BY_THE_ATTACH}\\r\\n'; exec sleep 300 ;; ` +
  `esac`;

const { check, results, report } = ledger();

const fx = makeFixture();
const door = await openDoor();
const port = door.port;
const { srv, log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,
    SKEIN_LAUNCH_CMD: LAUNCH_CMD,
    PATH: `${fx.bin}:${process.env.PATH}`,
  },
});

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const mustSee = seeing(page);
const settle = settler(page, 500);
page.setDefaultTimeout(5000);
const noise = [];
page.on("pageerror", e => noise.push(`[pageerror] ${e.message}`));
page.on("response", r => { if (r.status() >= 500) noise.push(`[${r.status()}] ${r.url()}`); });

await page.goto(`http://127.0.0.1:${port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
await page.waitForFunction(() => typeof createSession === "function" && typeof sessions !== "undefined");

/** Open a launch terminal for `box` on `branch`, the way the New box dialog does, and look at it.
 *
 * `createSession(name, "agent", branch, agent)` is the page's own last line of `startNewBox` — the
 * dialog around it needs a registered repo, a sync connection and a git identity to reach it, none
 * of which decide anything about what covers the pane afterwards. The socket, the PTY and the close
 * are all real. */
const open = (box, branch) => page.evaluate(([b, br]) => {
  createSession(b, "agent", br, "claude");
  view = { box: b, mode: "term", kind: "agent" };
  applyView();
}, [box, branch]);

/** Bring a pane to the front without going through `showBox`, which reconnects a dead session — and
 * a reconnect is precisely what this suite must not do to the panes it is about to read. */
const activate = box => page.evaluate(b => {
  view = { box: b, mode: "term", kind: "agent" };
  applyView();
}, box);

/** What the reader of this pane has in front of them: what the terminal holds, and which element
 * their eye lands on at the pixels the first line and the middle of the pane are drawn on. */
const probe = box => page.evaluate(b => {
  const s = sessions.get(sidOf(b, "agent"));
  if (!s) return { missing: true };
  const r = s.host.getBoundingClientRect();
  const disco = s.host.querySelector(".disco");
  const box2 = disco ? disco.getBoundingClientRect() : null;
  const at = (x, y) => {
    const el = document.elementFromPoint(x, y);
    if (!el) return "nothing";
    return el.closest(".disco") ? "the reconnect overlay" : `${el.tagName.toLowerCase()}.${el.className || "-"}`;
  };
  const buf = s.term.buffer.active;
  const lines = [];
  for (let i = 0; i < buf.length; i++) lines.push(buf.getLine(i)?.translateToString(true) ?? "");
  return {
    dead: !!s.dead,
    ended: !!s.ended,
    onTopOfTheFirstLine: at(r.left + 20, r.top + 12),
    onTopOfTheMiddle: at(r.left + r.width / 2, r.top + r.height / 2),
    display: disco ? getComputedStyle(disco).display : "no overlay in the pane at all",
    area: box2 ? Math.round(box2.width * box2.height) : 0,
    offer: disco ? disco.textContent.replace(/\s+/g, " ").trim() : "",
    said: lines.join("\n").replace(/\n+/g, "\n").trim(),
  };
}, box);

const dead = box => page.waitForFunction(
  b => !!sessions.get(sidOf(b, "agent"))?.dead, box, { timeout: 15000 });
const says = (box, what) => page.waitForFunction(
  ([b, w]) => {
    const s = sessions.get(sidOf(b, "agent"));
    if (!s) return false;
    const buf = s.term.buffer.active;
    for (let i = 0; i < buf.length; i++) {
      if ((buf.getLine(i)?.translateToString(true) ?? "").includes(w)) return true;
    }
    return false;
  }, [box, what], { timeout: 15000 });

// ---------- a launch that ran to completion ----------
console.log("\na launch whose command fails");
await open(ENDS, "ends");
await dead(ENDS);
await settle();
const ended = await probe(ENDS);

await check("the shell's own error is still in the pane after the launch ends", () => {
  if (!ended.said.includes(SAID_BY_THE_SHELL)) {
    throw new Error(`the terminal holds ${JSON.stringify(ended.said.slice(0, 200))}`);
  }
});

// The recording SKEIN-589 added, in the same pane: it is written because the exit code really was
// 127, which is what makes this fixture the field case rather than a shape that resembles it.
await check("and skein's own account of the failed start is under it", () => {
  if (!/was never started/.test(ended.said)) {
    throw new Error(`no launch failure was recorded in the pane: ${JSON.stringify(ended.said.slice(0, 300))}`);
  }
});

// THE defect. Not "the overlay is absent from the DOM" — it is in the DOM either way — but "nothing
// is drawn between the reader and the line that explains the failure".
await check("the pane a finished launch leaves is not covered", () => {
  if (ended.onTopOfTheFirstLine === "the reconnect overlay" || ended.onTopOfTheMiddle === "the reconnect overlay") {
    throw new Error(
      `the error is behind the reconnect panel ("${ended.offer}") — first line: ` +
        `${ended.onTopOfTheFirstLine}, middle: ${ended.onTopOfTheMiddle}`);
  }
  if (ended.display !== "none" || ended.area !== 0) {
    throw new Error(`the panel is laid out over the pane: display ${ended.display}, ${ended.area}px²`);
  }
});

await check("the server said the child ended it, and the page read that and nothing else", () => {
  if (!ended.dead) throw new Error("the session is not marked closed at all");
  if (!ended.ended) throw new Error("the close carried no 4001, so the page cannot tell what happened");
});

// ---------- an attach whose connection drops ----------
console.log("\nan attach whose connection drops");
await open(LIVES, "lives");
await says(LIVES, SAID_BY_THE_ATTACH);
await settle();
const alive = await probe(LIVES);

// The control on the two checks either side: a pane nobody has closed must be clear, or "nothing is
// covering it" is a sentence about a page that never covers anything.
await check("a live terminal is not covered either", () => {
  if (alive.dead) throw new Error("the session closed before the connection was cut");
  if (alive.display !== "none") throw new Error(`the panel is up on a live session: display ${alive.display}`);
});

await check("no page errors and no 5xx while the terminals were opened", () => {
  if (noise.length) throw new Error(noise.slice(0, 5).join(" | "));
});

// A connection that really went away: the server is killed under a child that is still running, so
// the browser closes with the 1006 it reports for a socket that died and no code from this end.
srv.kill("SIGKILL");
await dead(LIVES);
await activate(LIVES);
await settle();
const dropped = await probe(LIVES);

await check("a connection that dropped still gets the overlay", () => {
  if (dropped.ended) throw new Error("a dropped connection was read as a command that finished");
  if (dropped.display !== "flex") throw new Error(`the panel is not displayed: ${dropped.display}`);
  if (dropped.area <= 0) throw new Error("the panel has no box, so nothing is on screen");
  if (dropped.onTopOfTheMiddle !== "the reconnect overlay") {
    throw new Error(`the middle of the pane is ${dropped.onTopOfTheMiddle}, not the overlay`);
  }
});

await check("and it still offers the reconnect, in words a reader can see", async () => {
  // `mustSee` rather than a second look at the same numbers: it waits for a box on screen, which is
  // the harness's own answer to "in the DOM is not the same as visible".
  //
  // `.on` is load-bearing and not decoration. Only the active pane is displayed, and this suite
  // deliberately leaves a second closed pane open behind this one — without `.on` the selector
  // matches that one first, and the check then reports the hidden pane's zero box as this pane's.
  await mustSee(".thost.on.dead:not(.ended) .disco", "the reconnect overlay on the dropped pane");
  if (!/not connected/.test(dropped.offer) || !/reconnect/.test(dropped.offer)) {
    throw new Error(`the panel reads "${dropped.offer}"`);
  }
});

// Both panes are open at once, so the distinction is a thing one screen can be looked at for: come
// back to the finished launch after the drop and its error is still uncovered.
await activate(ENDS);
await settle();
const stillEnded = await probe(ENDS);
await check("and the finished launch beside it is still uncovered", () => {
  if (stillEnded.display !== "none") throw new Error(`the panel came up after the other pane dropped: ${stillEnded.display}`);
  if (!stillEnded.said.includes(SAID_BY_THE_SHELL)) throw new Error("the error left the pane");
});

// ---------- report ----------
const shot = path.join(fx.root, "failure.png");
if (results.some(([ok]) => !ok)) await page.screenshot({ path: shot, fullPage: false });
const failed = report({ log });
if (failed.length) console.log(`screenshot: ${shot}\nfixture kept for inspection: ${fx.root}`);
await browser.close();
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 || !!process.env.SKEIN_KEEP });
if (!failed.length && process.env.SKEIN_KEEP) console.log(`fixture kept (SKEIN_KEEP): ${fx.root}`);
process.exit(failed.length || leftRunning.length ? 1 : 0);
