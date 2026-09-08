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
import { createServer } from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { boxlikeNamespace, grab, harness, openDoor } from "./lift.mjs";
import { startServer } from "./harness/server.mjs";

const BOX = "attach-box";
const t = harness();
// What the server is told to treat as "nothing has moved" — `SKEIN_UPLOAD_STALL_MS`, whose default
// is a minute. See `upload_stall` (src/bin/skein-server.rs).
const STALL_MS = 5000;

// A fleet just real enough for an upload to have somewhere to land and a terminal to have something
// to attach to: a placement record (what makes a name one of skein's boxes), and a **real namespace
// to cross into**.
//
// The `sbx` stub used to be the box. Skein's every crossing went through `sbx exec`, so a script on
// `$PATH` could run the payload and skip the namespace entirely. There is no hop (SKEIN-576) and
// that stub is simply never invoked — `Place` builds `bash -c <anchor check> … exec nsenter …` and
// runs it here — so a fixture that kept it would be watching a fake while the real crossing failed
// at the guard. bwrap supplies the namespace, exactly as `place`'s own crossing test does, with
// `--dev-bind / /` so the box sees the same filesystem: an upload then lands where the assertions
// read it, which is the whole point of the suite.
//
// The stub survives for one job it can still do: `SKEIN_LS_CMD` is its own seam and still runs it.
async function fixture() {
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
  fs.writeFileSync(sbx, `#!/usr/bin/env bash
case "$1" in
  ls) echo '[{"name":"${BOX}","status":"running","agent":"claude","workspace":"${ws}"}]'; exit 0 ;;
esac
exit 0
`);
  fs.chmodSync(sbx, 0o755);

  // **The one case the box has to fake, and it is faked INSIDE the box.**
  //
  // `nsenter` resolves `bash` on the inherited `$PATH`, and `bin` is first on the server's — so a
  // `bash` here is the shell the crossing runs, and only within the namespace, because this
  // directory is bound over `bin` for the box alone. It sees the whole script, which `cat` cannot:
  // the destination is a redirect rather than an argument.
  //
  // `exec sleep`, never a bash that waits on one: the scenario is a box that has taken the bytes
  // and will not confirm, and skein kills the process it spawned — a wrapper shell would die and
  // leave the sleeper behind, which is a different and worse thing to be testing. Everything else
  // runs for real (SKEIN-269).
  const boxbin = path.join(root, "boxbin");
  fs.mkdirSync(boxbin);
  fs.writeFileSync(path.join(boxbin, "bash"), `#!/bin/sh
case "$*" in *skein-stall*) exec sleep 60 ;; esac
exec /usr/bin/bash "$@"
`);
  fs.chmodSync(path.join(boxbin, "bash"), 0o755);

  const box = await boxlikeNamespace(root, { from: boxbin, to: bin });

  // Stamped, because an unstamped record is not a placed box any more — it is a box whose address
  // skein refuses to use, and a fixture that left this off would be testing the refusal.
  fs.writeFileSync(path.join(home, "places", `${BOX}.json`), JSON.stringify({
    sandbox: "skein-fleet", ns_pid: box.ns_pid,
    home: path.join(root, "boxhome"), tree: ws,
    sock: path.join(root, "fleet", BOX, "session.sock"),
    generation: box.generation, ns_start: box.ns_start,
  }));
  return { root, ws, home, sbx, bin, boxlike: box.child };
}

// The page's world, cut down to exactly what `attachFiles` touches.
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

// Would a second lane asking for this exact port be given it? Answered by trying to take it, which
// is the only answer that does not depend on timing. Node sets `SO_REUSEADDR` on a listener by
// default, and on Linux that skips the TIME_WAIT wait without letting two live listeners share a
// port — measured, not assumed: with a `skein-server` holding an inherited socket, this returns
// `EADDRINUSE`.
const whoeverAsksNext = wanted => new Promise(res => {
  const rival = createServer();
  rival.once("error", e => res(e.code));
  rival.listen(wanted, "127.0.0.1", () => rival.close(() => res("bound")));
});

const fx = await fixture();
const rec = path.join(fx.root, "pty-input.bin");
fs.writeFileSync(rec, "");
const door = await openDoor();
const port = door.port;
const whileTheSuiteHoldsIt = await whoeverAsksNext(port);
// A door opened and let go of with nobody behind it. It is the control on the two probes either
// side of the handover: without a port that really is free, a probe that answered `EADDRINUSE` to
// everything — a typo in the port, a rival that never listens — would pass them both.
const spare = await openDoor();
spare.close();
const whenNobodyHoldsIt = await whoeverAsksNext(spare.port);
const base = `http://127.0.0.1:${port}`;
// No `token`: this fixture runs with `SKEIN_NO_API_AUTH`, so the readiness poll carries no bearer
// either. 150 attempts rather than 100 because this suite's server starts a tmux session as it
// comes up.
const { srv, log } = await startServer({
  door,
  tries: 150,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_LS_CMD: `${fx.sbx} ls --json`,
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
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
const onceTheServerHasIt = await whoeverAsksNext(port);
const dropped = new Set();   // the batch dirs this run made in the real /tmp, to take away again

try {
  // 0. Not about attaching — about the harness every server-driving suite here runs on. `openDoor`
  //    (lift.mjs) replaced a helper that bound :0, read the number the kernel chose and CLOSED,
  //    leaving the port free for a second lane to be handed while this one was still starting its
  //    server. `tests/browser_suites.rs:112` runs the suites several at a time, so that gap was a
  //    real collision (SKEIN-443). The property being asserted is not "collisions got rarer": it is
  //    that the port is bound CONTINUOUSLY, by this process and then by the server, so there is no
  //    instant at which it could be given away.
  t.check(
    "the port a suite's server is given is never free for a second lane to be handed",
    { whileTheSuiteHoldsIt, onceTheServerHasIt, whenNobodyHoldsIt },
    { whileTheSuiteHoldsIt: "EADDRINUSE", onceTheServerHasIt: "EADDRINUSE", whenNobodyHoldsIt: "bound" },
  );

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
  fx.boxlike.kill("SIGKILL");
  // `drop_dest` writes to /tmp/skein-drop-<batch> — the box's /tmp, which on this machine is this
  // machine's. Take away exactly what this run made, named from the path the server returned.
  for (const dir of dropped) if (dir.startsWith("/tmp/skein-drop-")) fs.rmSync(dir, { recursive: true, force: true });
  fs.rmSync(fx.root, { recursive: true, force: true });
}

t.done();
