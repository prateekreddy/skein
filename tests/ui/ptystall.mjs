// Does a pane keep showing its command's output while the browser sends more input than the
// command is reading?
//
// `pump_pty` forwards a browser's bytes to the PTY on a channel drained by a blocking thread doing
// `write_all` on the master. Writing to a PTY master that the child is not reading blocks after a
// few kilobytes, and then never resumes until something reads. So `in_rx` stopped draining, the
// 256-slot channel it used to be then filled, and the forward was `in_tx.send(b).await` INSIDE a
// `tokio::select!` branch. A `select!` polls nothing while a branch's handler is awaiting, so the
// pane's output, the 30s keepalive and every resize frame all stopped with it — the terminal went
// silent, for a reason nothing on screen could say (SKEIN-750).
//
// # Why this is not a browser suite
//
// The condition is "the child has stopped reading its stdin and the page keeps writing" — a page
// can be made to type, but it cannot be made to type three hundred frames into a stuck command on
// cue, and what a page puts on this socket is xterm's business rather than the suite's. A socket
// driven by hand is the suite's.
//
// This client reads continuously, which is the opposite of `closecode.mjs`'s starved reader and for
// the opposite reason: what is being proved here is that bytes KEEP ARRIVING, so the client must be
// in a position to notice that they stopped.
//
// # What the child does, and why it matters which
//
// The stall needs the master write to actually block, and whether it does is a property of the LINE
// DISCIPLINE rather than of the pump — so it was measured against a bare pty, away from skein
// entirely, before any of this was written. Run it yourself:
//
//     python3 - <<'PY'
//     import os, pty, subprocess, threading, time
//     def probe(stty, chunk):
//         m, s = pty.openpty()
//         p = subprocess.Popen(["sh", "-c", stty + "; sleep 30"], stdin=s, stdout=s, stderr=s)
//         os.close(s)
//         threading.Thread(target=lambda: [os.read(m, 8192) for _ in iter(int, 1)], daemon=True).start()
//         time.sleep(0.4)
//         n = [0]
//         def w():
//             try:
//                 while True: os.write(m, chunk); n[0] += len(chunk)
//             except OSError: pass
//         threading.Thread(target=w, daemon=True).start()
//         time.sleep(1.5); a = n[0]; time.sleep(1.5); b = n[0]
//         p.kill(); os.close(m)
//         return a, b
//     for label, stty, chunk in [
//         ("canonical, echo off, no newline", "stty -echo", b"x"*256),
//         ("canonical, echo off, newlines  ", "stty -echo", b"x"*255 + b"\n"),
//         ("raw, echo off                  ", "stty raw -echo", b"x"*256),
//     ]:
//         a, b = probe(stty, chunk)
//         print(label, a, b, "BLOCKED" if a == b else "never blocked")
//     PY
//
// It printed 368688640 and climbing for the first, 8960 twice for the second and 20480 twice for
// the third. So:
//
//   * canonical mode, echo off, input WITHOUT newlines — the discipline DISCARDS past its buffer
//     and the write never blocks, so this shape cannot reach the defect at all;
//   * canonical mode, echo off, input WITH newlines — blocks, and a paste always has newlines;
//   * raw mode — blocks, which is an agent or a full-screen TUI mid-turn.
//
// The exact figure moves with what the child is doing and is not worth asserting; that it stops is.
// So the payload here is lines. A child that never reads is `sleep`-shaped; a child that reads late
// is the second half of the story, because a fix that keeps the loop alive by throwing input away
// would pass the first check and fail the last.
//
//   node tests/ui/ptystall.mjs
import net from "node:net";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, harness, openDoor } from "./lift.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";

const API_TOKEN = "d".repeat(64);

// `stall`: says its terminal size every 150ms and never reads stdin — the pane whose output must
// keep arriving, and whose reported size is how a resize honoured after the blast is seen.
// `slow`: reads nothing for two seconds and then reads everything, which is an agent that comes
// back to its input. What it prints is what it was sent, so a byte dropped on the way in is a byte
// missing on the way out.
const LAUNCH_CMD =
  `case {branch} in ` +
  `(stall) stty -echo; while :; do printf 'TICK %s\\r\\n' "$(stty size)"; sleep 0.15; done ;; ` +
  `(slow) stty -echo; printf 'READY\\r\\n'; sleep 2; exec cat ;; ` +
  `(*) exec sleep 300 ;; ` +
  `esac`;

const t = harness();

const fx = freshFixture(fixtureRoot(), "skein-ptystall-ui");
const home = path.join(fx, "home");
fs.mkdirSync(home, { recursive: true });
fs.mkdirSync(path.join(fx, "fleet"), { recursive: true });
fs.writeFileSync(path.join(home, "config.json"), "{}");
fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
fs.writeFileSync(path.join(fx, "sandboxes.json"), "{}");
// The stand-in for sbx, for the same reason closecode.mjs has one: the terminal route asks the
// fleet for the box's directory on its way past, and without this that question reaches whatever
// `sbx` the machine happens to have.
const bin = path.join(fx, "bin");
fs.mkdirSync(bin);
const sbx = path.join(bin, "sbx");
fs.writeFileSync(sbx, "#!/usr/bin/env bash\ncase \"$1\" in\n  ls) echo '[]'; exit 0 ;;\nesac\nexit 0\n");
fs.chmodSync(sbx, 0o755);

const door = await openDoor();
const port = door.port;
const serverEnv = {
  SKEIN_HOME: home,
  SKEIN_FLEET_ROOT: path.join(fx, "fleet"),
  SKEIN_REGISTRY: path.join(fx, "sandboxes.json"),
  SKEIN_LS_CMD: `${sbx} ls --json`,
  SKEIN_LAUNCH_CMD: LAUNCH_CMD,
  PATH: `${bin}:${process.env.PATH}`,
};
await startServer({ door, token: API_TOKEN, env: serverEnv });

/** One masked client frame. The mask is zeroes, as in `closecode.mjs`: RFC 6455 §5.3 requires the
 * bit and a key, and a key of zeroes is a key — it leaves the payload readable in a trace. */
function frame(opcode, payload) {
  const body = Buffer.from(payload);
  let head;
  if (body.length < 126) {
    head = Buffer.from([0x80 | opcode, 0x80 | body.length, 0, 0, 0, 0]);
  } else if (body.length < 65536) {
    head = Buffer.alloc(8);
    head[0] = 0x80 | opcode;
    head[1] = 0x80 | 126;
    head.writeUInt16BE(body.length, 2);
  } else {
    head = Buffer.alloc(14);
    head[0] = 0x80 | opcode;
    head[1] = 0x80 | 127;
    head.writeBigUInt64BE(BigInt(body.length), 2);
  }
  return Buffer.concat([head, body]);
}

const resizeFrame = (cols, rows) => frame(1, JSON.stringify({ resize: { cols, rows } }));

/** Everything the server has said so far, as text: the payloads of every complete data frame. */
function saidSoFar(buf) {
  const end = buf.indexOf("\r\n\r\n");
  if (end === -1) return "";
  let rest = buf.subarray(end + 4);
  let said = "";
  for (;;) {
    if (rest.length < 2) break;
    const op = rest[0] & 0x0f;
    let len = rest[1] & 0x7f, off = 2;
    if (len === 126) { if (rest.length < 4) break; len = rest.readUInt16BE(2); off = 4; }
    else if (len === 127) { if (rest.length < 10) break; len = Number(rest.readBigUInt64BE(2)); off = 10; }
    if (rest.length < off + len) break;
    if (op === 1 || op === 2) said += rest.subarray(off, off + len).toString();
    rest = rest.subarray(off + len);
    if (op === 8) break;
  }
  return said;
}

/** A terminal socket that reads everything the server sends, for as long as it is held open. */
function terminal(box, branch) {
  return new Promise((resolve, reject) => {
    const sock = net.connect(port, "127.0.0.1");
    let buf = Buffer.alloc(0);
    sock.on("error", reject);
    sock.on("data", d => { buf = Buffer.concat([buf, d]); });
    sock.on("connect", () => {
      sock.write(
        `GET /api/boxes/${encodeURIComponent(box)}/terminal?launch=${branch}&agent=claude HTTP/1.1\r\n` +
          `Host: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
          `Sec-WebSocket-Key: ${crypto.randomBytes(16).toString("base64")}\r\n` +
          `Sec-WebSocket-Version: 13\r\nCookie: skein_api=${API_TOKEN}\r\n\r\n`,
      );
      resolve({
        said: () => saidSoFar(buf),
        write: b => sock.write(b),
        close: () => { try { sock.destroy(); } catch { /* already gone */ } },
      });
    });
  });
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

/** Wait until `f()` is true, or give up after `ms`. Returns whether it became true. */
async function until(f, ms) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (f()) return true;
    await sleep(50);
  }
  return f();
}

// More than the 256-slot channel this used to have, plus the few kilobytes the master swallows
// before its write blocks: 500 frames of 1024 bytes is 512000, which covers both with room to
// spare. Every frame is a line, so the line discipline queues them rather than discarding them.
const BLAST_FRAMES = 500;
const LINE = "x".repeat(1023) + "\n";

let stallSock = null, slowSock = null;
try {
  // ---------- a pane whose command has stopped reading ----------
  stallSock = await terminal("ptystall-stall", "stall");
  const ticks = () => (stallSock.said().match(/TICK /g) || []).length;
  const started = await until(() => ticks() >= 2, 15000);
  t.check("the pane is saying something before anything is sent to it", started, true);

  const before = ticks();
  for (let i = 0; i < BLAST_FRAMES; i++) stallSock.write(frame(2, LINE));
  // Sent AFTER the blast, so a loop that is stuck on the forward has not read it. `sendResize`
  // fires on `requestAnimationFrame` and on xterm's own `onResize`, so a real page emits one of
  // these at a moment nothing on the server chose, which is exactly this moment.
  stallSock.write(resizeFrame(131, 41));

  const kept = await until(() => ticks() > before + 2, 5000);
  t.check(
    "the command's output keeps reaching the socket while more is sent in than it reads",
    kept,
    true,
  );

  const resized = await until(() => /TICK 41 131/.test(stallSock.said()), 5000);
  t.check("and a resize sent behind that input is still honoured", resized, true);

  // ---------- and nothing was thrown away to keep it alive ----------
  // A `try_send` that drops on a full queue passes both checks above and fails this one: what the
  // child prints when it comes back to its input is what it was sent, byte for byte and in order.
  slowSock = await terminal("ptystall-slow", "slow");
  // ONLCR is on, so what comes back carries the carriage returns the terminal added.
  const back = () => slowSock.said().replace(/\r/g, "");
  // Wait for the child to have turned echo OFF before sending it anything. A terminal echoes by
  // default, and bytes sent into the gap come back a second time — 4095 of them, which is the
  // canonical buffer, and enough to look exactly like the corruption this check is for.
  const ready = await until(() => back().includes("READY\n"), 15000);
  t.check("the second pane has its echo off before anything is sent to it", ready, true);
  const beforeTheBlast = back().length;

  const lines = [];
  for (let i = 0; i < BLAST_FRAMES; i++) lines.push(`L${String(i).padStart(6, "0")}${"y".repeat(500)}\n`);
  for (const l of lines) slowSock.write(frame(2, l));

  const want = lines.join("");
  const got = () => back().slice(beforeTheBlast);
  const whole = await until(() => got().length >= want.length, 25000);
  t.check(
    "every byte sent while the command was not reading arrives once it reads, in order",
    { whole, same: got().slice(0, want.length) === want },
    { whole: true, same: true },
  );
} catch (e) {
  // A suite that could not run is a failure, not a silence: `t.done()` exits 0 on an empty ledger.
  t.check("the pty-stall suite could run at all", String((e && e.message) || e), "it ran");
} finally {
  if (stallSock) stallSock.close();
  if (slowSock) slowSock.close();
  // Everything that writes into the fixture stops BEFORE the fixture is removed (SKEIN-1116).
  // `srv.kill()` only sent a signal and did not wait, and the server's supervisor was not its child,
  // so a recursive rm raced processes still writing into the tree and threw ENOTEMPTY after every
  // check had passed. Planting `srv.kill()` back in place of `quiesce()` inside `stopThenRemove`
  // leaves the tmux, the loop and the python running, and the check it records names all three.
  stopThenRemove([fx], { record: (name, left) => t.check(name, left, []) });
}

t.done();
