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
  fs.writeFileSync(sbx, `#!/usr/bin/env bash
case "$1" in
  ls)   echo '[{"name":"${BOX}","status":"running","agent":"claude","workspace":"${ws}"}]'; exit 0 ;;
  exec) exec bash -c "\${@: -1}" ;;
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
    ${grab("concatBytes")}
    ${grab("attachFiles")}
    return { attachFiles };
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
  return { attachFiles: made.attachFiles, said, copied };
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
