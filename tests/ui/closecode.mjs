// Does `CLOSE_CHILD_ENDED` actually REACH a page, or only get written?
//
// `panecover.mjs` asserts what the cockpit does with the close code a finished launch carries:
// `ws.onclose` reads 4001, marks the pane `.ended`, and the reconnect panel stays off the shell's
// error. Everything there is downstream of the code arriving. This suite is about the arrival, and
// it exists because the arrival was not reliable (SKEIN-746).
//
// `terminal_session` used to write the close frame and return, and returning drops the socket. A
// socket dropped while bytes it never read are still queued on it is closed by the kernel with RST
// rather than FIN — and an RST discards whatever the PEER had queued and not yet read. So the close
// code was destroyed by the same reset that ended the connection, and the browser reported 1006:
// the page could not tell a command that finished from a connection that dropped, and put "session
// not connected · click to reconnect" back over the one line explaining the empty box, which is the
// exact defect SKEIN-672 removed.
//
// **The cockpit supplies both halves of that on its own.** It writes on the terminal socket without
// being asked — `sendResize` fires off `requestAnimationFrame` and off xterm's own `onResize`,
// neither of them timed by anything on the server — while `pump_pty` stops reading the instant the
// child's PTY closes. Anything arriving between that instant and the handler returning is therefore
// never read. And a page whose renderer is short of CPU is exactly the page that has not yet read
// what it was sent, which is why this only ever showed up on a loaded box and never alone.
//
// # Why this is not a browser suite
//
// The condition is "a frame arrived after the pump stopped reading, and the client has not drained
// its receive queue" — two things a page cannot be told to do on cue. A socket driven by hand can:
// this client never reads until everything is over, and writes a resize every millisecond, so one
// of them always lands in the window. Measured against the server WITHOUT the fix, that is 30
// sessions out of 30 losing the close code, the shell's error and skein's recorded reason together,
// and reporting ECONNRESET instead of any of them.
//
//   node tests/ui/closecode.mjs
import net from "node:net";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, harness, openDoor } from "./lift.mjs";
import { startServer } from "./harness/server.mjs";

const API_TOKEN = "c".repeat(64);

// The same shape panecover.mjs launches: a command that says something on the PTY and exits 127, so
// the server has both a line to deliver and an exit code to report a close for.
const SAID_BY_THE_SHELL = "sh: 1: skein: not found";
const LAUNCH_CMD =
  `case {branch} in ` +
  `(ends) printf '${SAID_BY_THE_SHELL}\\r\\n' >&2; exit 127 ;; ` +
  `(*) exec sleep 300 ;; ` +
  `esac`;

// RFC 6455's private range, agreed with `src/bin/skein-server.rs` and `src/web/index.html` and
// spelled here rather than imported because a constant read out of the thing under test is not a
// check of it.
const CLOSE_CHILD_ENDED = 4001;

const t = harness();

const fx = freshFixture(fixtureRoot(), "skein-closecode-ui");
const home = path.join(fx, "home");
fs.mkdirSync(home, { recursive: true });
fs.mkdirSync(path.join(fx, "fleet"), { recursive: true });
fs.writeFileSync(path.join(home, "config.json"), "{}");
fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
fs.writeFileSync(path.join(fx, "sandboxes.json"), "{}");
// The stand-in for sbx: the terminal route asks the fleet for the box's directory on its way past,
// and without this that question reaches whatever `sbx` the machine happens to have.
const bin = path.join(fx, "bin");
fs.mkdirSync(bin);
const sbx = path.join(bin, "sbx");
fs.writeFileSync(sbx, "#!/usr/bin/env bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
fs.chmodSync(sbx, 0o755);

const door = await openDoor();
const port = door.port;
const { srv } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_HOME: home,
    SKEIN_FLEET_ROOT: path.join(fx, "fleet"),
    SKEIN_REGISTRY: path.join(fx, "sandboxes.json"),
    SKEIN_LS_CMD: `${sbx} ls --json`,
    SKEIN_LAUNCH_CMD: LAUNCH_CMD,
    PATH: `${bin}:${process.env.PATH}`,
  },
});

/** One masked resize frame — byte for byte what `sendResize` puts on this socket. */
function resizeFrame() {
  const body = Buffer.from(JSON.stringify({ resize: { cols: 100, rows: 30 } }));
  return Buffer.concat([Buffer.from([0x81, 0x80 | body.length, 0, 0, 0, 0]), body]);
}

/** Every frame in `buf`, after the upgrade response: what was said, and the close code if any. */
function readFrames(buf) {
  const end = buf.indexOf("\r\n\r\n");
  let rest = end === -1 ? buf : buf.subarray(end + 4);
  let said = "", code = null;
  for (;;) {
    if (rest.length < 2) break;
    const op = rest[0] & 0x0f;
    let len = rest[1] & 0x7f, off = 2;
    if (len === 126) { if (rest.length < 4) break; len = rest.readUInt16BE(2); off = 4; }
    else if (len === 127) { if (rest.length < 10) break; len = Number(rest.readBigUInt64BE(2)); off = 10; }
    if (rest.length < off + len) break;
    const payload = rest.subarray(off, off + len);
    rest = rest.subarray(off + len);
    if (op === 1 || op === 2) said += payload.toString();
    if (op === 8) { code = len >= 2 ? payload.readUInt16BE(0) : 0; break; }
  }
  return { said, code };
}

/** The upgrade request for a launch terminal on `box`, carrying the session cookie the page gets. */
function upgrade(sock, box) {
  sock.write(
    `GET /api/boxes/${encodeURIComponent(box)}/terminal?launch=ends&agent=claude HTTP/1.1\r\n` +
      `Host: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
      `Sec-WebSocket-Key: ${crypto.randomBytes(16).toString("base64")}\r\n` +
      `Sec-WebSocket-Version: 13\r\nCookie: skein_api=${API_TOKEN}\r\n\r\n`,
  );
}

/** A page that has not read anything yet: it writes resizes and reads only once it is all over.
 *
 * `sock.pause()` before the first byte, not after — a client that pauses on the first `data` event
 * has already taken delivery of whatever came in that segment, and would then survive the reset it
 * is here to be destroyed by. */
function starvedReader(box, stallMs) {
  return new Promise(resolve => {
    const sock = net.connect(port, "127.0.0.1");
    let buf = Buffer.alloc(0), err = null, ticker = null;
    sock.on("error", e => { err = e.code || e.message; });
    sock.on("connect", () => {
      upgrade(sock, box);
      sock.pause();
      sock.on("data", d => { buf = Buffer.concat([buf, d]); });
      ticker = setInterval(() => { try { sock.write(resizeFrame()); } catch {} }, 1);
      setTimeout(() => {
        clearInterval(ticker);
        sock.resume();
        setTimeout(() => {
          const frames = readFrames(buf);
          try { sock.destroy(); } catch {}
          resolve({ ...frames, err });
        }, 200);
      }, stallMs);
    });
  });
}

/** A page that reads as it goes and answers the close, and how long the socket then took to end. */
function promptReader(box, giveUpMs) {
  return new Promise(resolve => {
    const sock = net.connect(port, "127.0.0.1");
    let buf = Buffer.alloc(0), sawCloseAt = 0, timer = null;
    const finish = () => {
      clearTimeout(timer);
      const ended = sawCloseAt ? Date.now() - sawCloseAt : null;
      try { sock.destroy(); } catch {}
      resolve({ ...readFrames(buf), ended });
    };
    sock.on("error", () => {});
    sock.on("close", finish);
    sock.on("connect", () => {
      upgrade(sock, box);
      timer = setTimeout(finish, giveUpMs);
    });
    sock.on("data", d => {
      buf = Buffer.concat([buf, d]);
      if (sawCloseAt || readFrames(buf).code === null) return;
      // Answer it the way a browser does. The server is waiting for exactly this, and what is being
      // measured is that it stops waiting the moment it arrives rather than sitting out its whole
      // deadline.
      sawCloseAt = Date.now();
      try { sock.write(Buffer.from([0x88, 0x80, 0, 0, 0, 0])); } catch {}
    });
  });
}

try {
  // ---------- the close a starved page is owed ----------
  const starved = await starvedReader("closecode-ends", 1500);

  // THE defect. Not "the server wrote a close" — it always did — but "the page was still holding
  // the bytes when the connection was reset, so it got none of them".
  t.check(
    "the close code reaches a page that has not read its socket yet",
    { code: starved.code, error: starved.err },
    { code: CLOSE_CHILD_ENDED, error: null },
  );

  // The same reset takes the terminal's contents with it, and that is the half a reader sees: the
  // shell's own error, and the reason skein recorded because `skein` never ran (SKEIN-589).
  t.check(
    "and the two sentences the pane is left showing survive with it",
    {
      shell: starved.said.includes(SAID_BY_THE_SHELL),
      recorded: /was never started/.test(starved.said),
    },
    { shell: true, recorded: true },
  );

  // ---------- and the wait is a handshake, not a delay ----------
  const prompt = await promptReader("closecode-ends", 20000);
  t.check(
    "a page that answers the close ends the session at once, not after the deadline",
    {
      code: prompt.code,
      promptly: prompt.ended !== null && prompt.ended < 2000,
    },
    { code: CLOSE_CHILD_ENDED, promptly: true },
  );
} catch (e) {
  // A suite that could not run is a failure, not a silence: `t.done()` exits 0 on an empty ledger.
  t.check("the close-code suite could run at all", String((e && e.message) || e), "it ran");
} finally {
  srv.kill();
  fs.rmSync(fx, { recursive: true, force: true });
}

t.done();
