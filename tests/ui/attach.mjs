// Does an attached file reach the AGENT, or only the box?
//
// Those are two different journeys and only the first one had a test. An owner attached the same
// screenshot five times on 2026-08-25; all five uploads worked — `/tmp/skein-drop-mt8c64ue-0` and
// four siblings, 76628 bytes each — and the agent in that box received nothing at all: no
// `[Image #N]`, no path, no text. The image was found by looking on disk. The cockpit said
// "attached →" every time (SKEIN-267).
//
// So the half that breaks is the HANDOVER: `attachFiles` (src/web/index.html) uploads the file, then
// writes the in-box path into the box's terminal as a bracketed paste over the session WebSocket.
// Nothing anywhere proved those bytes arrive, and the toast that says they did is written after a
// `send()` whose outcome nobody reads.
//
// This runs the page's own `attachFiles` — lifted, not re-implemented — against a real
// `skein-server`, a real upload, a real terminal WebSocket and a real PTY, and then reads what the
// PTY actually received. The PTY is a `cat` in raw mode, so what lands in that file is exactly what
// a provider's TUI would have been handed.
//
// It also drives the second defect the same reproduction found (SKEIN-269): an upload that has not
// progressed used to look exactly like one that is working — one "uploading…" toast and then
// nothing, for as long as it took — and neither end of the wire was measured, so which end held it
// could only be guessed at. Both are checked here against a box that really does stop reading.
//
//   node tests/ui/attach.mjs
//
// Needs node and nothing else — no chromium — so it runs in a box, where the attach path is used.
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { grab, harness, serverBinary } from "./lift.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const BOX = "attach-box";
const t = harness();
// What the server is told to treat as "nothing has moved" — `SKEIN_UPLOAD_STALL_MS`, whose default
// is a minute. See `upload_stall` (src/bin/skein-server.rs).
const STALL_MS = 5000;

const freePort = () => new Promise(res => {
  const s = createServer();
  s.listen(0, "127.0.0.1", () => { const { port } = s.address(); s.close(() => res(port)); });
});

// A fleet just real enough for an upload to have somewhere to land and a terminal to have something
// to attach to: a placement record (what makes a name one of skein's boxes), and an `sbx` stub that
// runs what it is handed instead of entering a namespace that does not exist here.
function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "skein-attach-"));
  const ws = path.join(root, "workspace");
  const home = path.join(root, "home");
  fs.mkdirSync(ws, { recursive: true });
  fs.mkdirSync(path.join(home, "places"), { recursive: true });
  fs.mkdirSync(path.join(root, "boxhome"), { recursive: true });
  fs.mkdirSync(path.join(root, "fleet", BOX), { recursive: true });
  fs.writeFileSync(path.join(root, "sandboxes.json"), JSON.stringify({
    [BOX]: { branch: "main", dir: ws, lastSeen: new Date().toISOString(), status: "" },
  }));
  fs.writeFileSync(path.join(home, "config.json"), "{}");
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin);
  const sbx = path.join(bin, "sbx");
  // `exec` runs the LAST argument, whatever the prefix: skein addresses a box through its placement,
  // so the real argv is `sbx exec -i <sandbox> nsenter … -- bash -lc <script>` and the script already
  // carries its own `cd`/`export HOME` from `Place::wrap`.
  // `exec sleep` for the stall case, never a bash that waits on one: the point of the scenario is a
  // box that has taken the bytes and will not confirm, and skein kills the process it spawned — a
  // wrapper shell would die and leave the sleeper behind, which is a different (and worse) thing to
  // be testing. The path carries the marker because the script is the last argument (SKEIN-269).
  fs.writeFileSync(sbx, `#!/usr/bin/env bash
case "$1" in
  ls)   echo '[{"name":"${BOX}","status":"running","agent":"claude","workspace":"${ws}"}]'; exit 0 ;;
  exec) case "\${@: -1}" in
          *skein-stall*) exec sleep 60 ;;
          *) exec bash -c "\${@: -1}" ;;
        esac ;;
esac
exit 0
`);
  fs.chmodSync(sbx, 0o755);
  // ns_pid is this process: alive, so the placement record is followed rather than swept.
  fs.writeFileSync(path.join(home, "places", `${BOX}.json`), JSON.stringify({
    sandbox: "skein-fleet", ns_pid: process.pid,
    home: path.join(root, "boxhome"), tree: ws,
    sock: path.join(root, "fleet", BOX, "session.sock"),
  }));
  return { root, ws, home, sbx, bin };
}

async function startServer(fx, port, rec) {
  const srv = spawn(serverBinary(), {
    cwd: REPO, stdio: ["ignore", "pipe", "pipe"],
    env: {
      ...process.env,
      SKEIN_ADDR: `127.0.0.1:${port}`,
      SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
      SKEIN_LS_CMD: `${fx.sbx} ls --json`,
      SKEIN_HOME: fx.home,
      SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
      SKEIN_NO_GH_SECRET: "1",
      // The one deviation from what a browser does. A browser authenticates with the `HttpOnly`
      // cookie it got from `?t=`, and node's WebSocket can set neither a cookie nor a header — so a
      // suite that insisted on the real credential could not open a terminal at all. The auth path
      // itself is covered by `smoke.mjs`, in a browser, where it is real.
      SKEIN_NO_API_AUTH: "1",
      // The fake PTY. `stty raw -echo` first, so the tty's line discipline neither buffers the paste
      // until a newline (there is no newline — an attach deliberately does not press Enter) nor
      // echoes it back; then everything typed at the terminal is appended to a file we can read.
      // READY is how the test knows `stty` has already run, rather than racing it.
      SKEIN_ATTACH_CMD: `stty raw -echo; printf READY; cat >> ${rec}`,
      // The deadline under test, shortened so driving it costs five seconds instead of a minute.
      // Longer than `ATTACH_SLOW_MS`, deliberately: the page's "still uploading…" has to fire while
      // the request is genuinely outstanding, which is the only condition it exists for.
      SKEIN_UPLOAD_STALL_MS: String(STALL_MS),
      PATH: `${fx.bin}:${process.env.PATH}`,
    },
  });
  let log = "";
  srv.stdout.on("data", d => { log += d; });
  srv.stderr.on("data", d => { log += d; });
  for (let i = 0; i < 150; i++) {
    try { if ((await fetch(`http://127.0.0.1:${port}/api/boxes`)).ok) return { srv, log: () => log }; } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  srv.kill();
  throw new Error(`server never came up on ${port}\n${log}`);
}

// The page's world, cut down to exactly what `attachFiles` touches. Everything it does to the
// terminal it does through `sessions`, so a real WebSocket in that map is a real terminal.
function pageWorld(base, sid, sessions) {
  const said = [], copied = [];
  const body = `
    let attachSeq = 0;
    // The queue is the PAGE's, keyed by sid — a handover parked on a session object goes into the
    // bin with it when the terminal reconnects (SKEIN-267), so the world has to hold the real one.
    const attachWaiting = new Map();
    ${grab("concatBytes")}
    ${grab("flushAttach")}
    ${grab("ATTACH_SLOW_MS")}
    ${grab("uploadOne")}
    ${grab("attachDelayWord")}
    ${grab("attachFiles")}
    return { attachFiles, flushAttach, attachWaiting, attachDelayWord, ATTACH_SLOW_MS };
  `;
  const made = new Function("toast", "ensureTerminal", "sessions", "copyText", "fetch", "TextEncoder", body)(
    m => said.push(m),
    () => sid,
    sessions,
    text => { copied.push(text); return true; },
    // The page's fetch is same-origin and relative; node's has no origin to be same as.
    (url, opt) => fetch(base + url, opt),
    TextEncoder,
  );
  return { attachFiles: made.attachFiles, flushAttach: made.flushAttach,
           attachWaiting: made.attachWaiting, attachDelayWord: made.attachDelayWord,
           slowMs: made.ATTACH_SLOW_MS, said, copied };
}

// A terminal the way the page opens one, resolved when the PTY behind it is in raw mode.
function openTerminal(port) {
  const ws = new WebSocket(`ws://127.0.0.1:${port}/api/boxes/${BOX}/terminal?agent=claude`);
  ws.binaryType = "arraybuffer";
  let seen = "";
  const ready = new Promise((res, rej) => {
    const timer = setTimeout(() => rej(new Error(`the PTY never said READY; it said ${JSON.stringify(seen)}`)), 15000);
    ws.addEventListener("message", e => {
      seen += typeof e.data === "string" ? e.data : new TextDecoder().decode(new Uint8Array(e.data));
      if (seen.includes("READY")) { clearTimeout(timer); res(); }
    });
  });
  const open = new Promise(res => ws.addEventListener("open", res));
  return { ws, opened: open.then(() => ready) };
}

// Wait for the PTY's recording to stop growing, so a check reads all of a paste rather than the
// front of one. Bounded: a handover that never arrives has to fail the check, not hang the suite.
async function settled(rec, ms = 1200) {
  let last = -1;
  for (let i = 0; i < ms / 100; i++) {
    await new Promise(r => setTimeout(r, 100));
    const now = fs.statSync(rec).size;
    if (now > 0 && now === last) break;
    last = now;
  }
  return fs.readFileSync(rec, "latin1");
}

const fx = fixture();
const rec = path.join(fx.root, "pty-input.bin");
fs.writeFileSync(rec, "");
const port = await freePort();
const base = `http://127.0.0.1:${port}`;
const { srv, log } = await startServer(fx, port, rec);
const dropped = new Set();   // the batch dirs this run made in the real /tmp, to take away again

try {
  const term = openTerminal(port);
  await term.opened;
  const sessions = new Map([[BOX, { ws: term.ws, box: BOX, kind: "agent" }]]);
  const w = pageWorld(base, BOX, sessions);

  const bytes = new Uint8Array(1024).fill(7);
  await w.attachFiles(BOX, [{ rel: "shot.png", file: new File([bytes], "shot.png", { type: "image/png" }) }], 1);
  const arrived = await settled(rec);

  // What the page told the reader, and where it says the file is. Both halves matter: the path in
  // the toast is the only thing a reader can hand to an agent by mouth when the rest fails.
  const attached = w.said.find(m => m.includes("/tmp/skein-drop-")) || "";
  const inBox = (attached.match(/\/tmp\/skein-drop-[^\s]+/) || [""])[0];
  if (inBox) dropped.add(inBox.split("/").slice(0, 3).join("/"));

  // 1. The half that was never broken, pinned so a change to the upload cannot quietly move the
  //    file out from under a path the agent has already been given.
  t.check(
    "the upload puts the file in the box at the path the cockpit reports",
    { named: !!inBox, exists: fs.existsSync(inBox), size: fs.existsSync(inBox) ? fs.statSync(inBox).size : 0 },
    { named: true, exists: true, size: 1024 },
  );

  // 2. The half nothing checked, and the whole of SKEIN-267: did the path actually reach the PTY?
  //    Asked as three facts rather than as one byte-for-byte comparison, so a failure says WHICH part
  //    went — an opening marker that stopped arriving and a path that arrived under another name are
  //    different bugs, and a single `got !== want` would report them identically.
  const open = "\x1b[200~", close = "\x1b[201~";
  const between = arrived.slice(arrived.indexOf(open) + open.length, arrived.indexOf(close));
  t.check(
    "a live terminal receives the attached path, wrapped in paste markers",
    { opens: arrived.startsWith(open), path: between, closes: arrived.includes(close) },
    { opens: true, path: inBox, closes: true },
  );

  // 3. The separating space is a keystroke AFTER the closing marker. Inside the paste it becomes part
  //    of the pasted text, and a provider matching "this paste is a path" stops recognising it — a
  //    move of one character that no check on the path itself would notice.
  t.check(
    "the separating space follows the paste instead of being inside it",
    { insideThePaste: between.endsWith(" "), afterTheMarker: arrived.endsWith(close + " ") },
    { insideThePaste: false, afterTheMarker: true },
  );

  // 4. An attach hands the agent a path; it does not decide that the reader is finished typing. A
  //    stray `\r` here would submit whatever else is in the composer, which is somebody else's turn.
  t.check(
    "an attach never presses Enter on the reader's behalf",
    { submits: arrived.includes("\r"), newline: arrived.includes("\n") },
    { submits: false, newline: false },
  );

  // 5. The reported bug, as a check. A terminal whose socket has gone is not a terminal, and the one
  //    thing the reader must not be told is that the file was attached.
  fs.writeFileSync(rec, "");
  const gone = openTerminal(port);
  await gone.opened;
  gone.ws.close();
  await new Promise(r => setTimeout(r, 300));
  const dead = new Map([[BOX, { ws: gone.ws, box: BOX, kind: "agent", dead: true }]]);
  const w2 = pageWorld(base, BOX, dead);
  await w2.attachFiles(BOX, [{ rel: "shot.png", file: new File([bytes], "shot.png", { type: "image/png" }) }], 1);
  const stranded = w2.said[w2.said.length - 1];
  const strandedPath = (stranded.match(/\/tmp\/skein-drop-[^\s]+/) || [""])[0];
  if (strandedPath) dropped.add(strandedPath.split("/").slice(0, 3).join("/"));
  t.check(
    "a terminal that has dropped is never reported as having received the attachment",
    { claimsAttached: /attached →/.test(stranded), saysWhereItIs: stranded.includes(strandedPath),
      stillHeld: w2.attachWaiting.has(BOX), reachedThePty: (await settled(rec, 600)).length > 0 },
    { claimsAttached: false, saysWhereItIs: true, stillHeld: true, reachedThePty: false },
  );

  // 6. And it is not lost: the payload waits by sid, so the socket a reconnect opens still gets it —
  //    and "attached →" is said then, when it has become true, rather than before.
  fs.writeFileSync(rec, "");
  const back = openTerminal(port);
  await back.opened;
  dead.set(BOX, { ws: back.ws, box: BOX, kind: "agent" });
  w2.flushAttach(BOX);
  t.check(
    "a reconnected terminal receives the handover the dropped one could not take",
    { reachedThePty: await settled(rec), nowClaimsAttached: /attached →/.test(w2.said[w2.said.length - 1]) },
    { reachedThePty: `\x1b[200~${strandedPath}\x1b[201~ `, nowClaimsAttached: true },
  );
  back.ws.close();

  // 7. With no terminal at all there is nothing to deliver to, and the clipboard is not an answer on
  //    a phone or a second machine. The sentence has to carry the path.
  const w3 = pageWorld(base, BOX, new Map());
  await w3.attachFiles(BOX, [{ rel: "shot.png", file: new File([bytes], "shot.png", { type: "image/png" }) }], 1);
  const orphan = w3.said[w3.said.length - 1];
  const orphanPath = (orphan.match(/\/tmp\/skein-drop-[^\s]+/) || [""])[0];
  if (orphanPath) dropped.add(orphanPath.split("/").slice(0, 3).join("/"));
  t.check(
    "with no terminal open the toast still says where in the box the file is",
    { namesThePath: !!orphanPath && orphan.includes(orphanPath), alsoCopied: w3.copied.length },
    { namesThePath: true, alsoCopied: 1 },
  );

  // ---- SKEIN-269: a slow upload says so, and the two clocks say which end was slow -------------

  // 8. The server answers with its own clock beside the path. Without it the browser can measure
  //    only click-to-answer, which counts time the request spent queued in the browser BEFORE it was
  //    sent — so a browser-side delay and a box-side one produce the identical number, and the
  //    original report could do nothing with five drop directories but list two suspects.
  const w4 = pageWorld(base, BOX, new Map());
  const timed = await (async () => {
    const r = await fetch(`${base}/api/boxes/${BOX}/upload`, {
      method: "POST", headers: { "Content-Type": "image/png", "X-Skein-Drop": "timed-1",
                                 "X-Skein-Name": "clocked.png" }, body: bytes });
    return r.json();
  })();
  if (timed.path) dropped.add(timed.path.split("/").slice(0, 3).join("/"));
  t.check(
    "an upload answers with the server's own clock, phase by phase",
    { ok: timed.ok, phases: timed.ms ? Object.keys(timed.ms).sort().join(",") : "none",
      totalIsANumber: typeof (timed.ms || {}).total === "number",
      totalCoversItsPhases: !!timed.ms && timed.ms.total >= timed.ms.chose + timed.ms.body + timed.ms.verdict },
    { ok: true, phases: "body,chose,total,verdict", totalIsANumber: true, totalCoversItsPhases: true },
  );

  // 9. And the comparison the pair exists for, checked on the owner's own numbers: 07:21:08.870Z
  //    clicked, 07:29:39.957 written — 511s of waiting. Which end held it is a different answer
  //    depending only on what the server says it spent, and that is the whole discriminator.
  t.check(
    "the two clocks name which end of the wire held the upload",
    { browserQueue: w4.attachDelayWord(511087, 1600),
      theBox: w4.attachDelayWord(511087, 509000),
      olderSkeinWithNoClock: w4.attachDelayWord(511087, -1),
      quickEnoughToSayNothing: w4.attachDelayWord(120, 90) },
    { browserQueue: " — 511.1s, of which the box took 1.6s: the rest went before the request left this browser",
      theBox: " — 511.1s, and 509.0s of it was the box",
      olderSkeinWithNoClock: " — 511.1s, and this skein does not say where it went",
      quickEnoughToSayNothing: "" },
  );

  // 10. The host half of the done-when: a box that takes the bytes and never confirms is refused
  //     with a word, inside the stall budget. This path — `sbx exec -i`, the fallback when there is
  //     no in-box agent — had NO deadline at all, so the request sat for as long as the process
  //     lived and the reader saw "uploading…" for all of it.
  const w5 = pageWorld(base, BOX, new Map());
  const began = Date.now();
  await w5.attachFiles(BOX, [{ rel: "skein-stall.png", file: new File([bytes], "skein-stall.png", { type: "image/png" }) }], 1);
  const stalledFor = Date.now() - began;
  const refusal = w5.said.find(m => m.startsWith("attach failed:")) || "";
  t.check(
    "a box that stops confirming is refused within the stall budget, not waited out",
    { gaveUpWithin: stalledFor < STALL_MS * 3, saysNothingMoved: /did not finish within/.test(refusal),
      namesTheBudget: refusal.includes(`${STALL_MS / 1000}s`), stillClaimedSuccess: /attached →/.test(w5.said.join(" ")) },
    { gaveUpWithin: true, saysNothingMoved: true, namesTheBudget: true, stillClaimedSuccess: false },
  );

  // 11. And while it was outstanding the page said so, repeatedly, instead of leaving one
  //     "uploading…" on screen. This is the sentence whose absence turned one attach into five: a
  //     reader with nothing moving in front of them presses the thing again.
  t.check(
    "an upload that has not answered says it is still going, and that clicking again will not help",
    { saidStillGoing: w5.said.some(m => m.startsWith("still uploading skein-stall.png")),
      namesTheBox: w5.said.some(m => m.includes(`no answer from ${BOX} yet`)),
      talksTheReaderDown: w5.said.some(m => m.includes("attaching it again will not make this one faster")) },
    { saidStillGoing: true, namesTheBox: true, talksTheReaderDown: true },
  );

  term.ws.close();
} catch (e) {
  // A suite that could not run is a failure, not a silence — `t.done()` exits 0 on an empty ledger,
  // so the failure has to be recorded in the ledger rather than beside it.
  console.error(`\nserver said:\n${log()}`);
  t.check("the attach suite could run at all", String(e.message || e), "it ran");
} finally {
  srv.kill();
  // `drop_dest` writes to /tmp/skein-drop-<batch> — the box's /tmp, which on this machine is this
  // machine's. Take away exactly what this run made, named from the path the server returned.
  for (const dir of dropped) if (dir.startsWith("/tmp/skein-drop-")) fs.rmSync(dir, { recursive: true, force: true });
  fs.rmSync(fx.root, { recursive: true, force: true });
}

t.done();
