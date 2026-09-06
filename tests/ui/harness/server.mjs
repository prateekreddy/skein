// The `skein-server` a browser suite drives, started once here instead of seven times.
//
// Six browser suites and `attach.mjs` each carried their own `startServer`: same spawn, same
// `door.close()`, same 100×100ms poll, same `srv.kill(); throw` — and the same three paragraphs of
// comment pasted with them, so the subtle part (the inherited socket) was explained six times and
// owned by nobody. What actually differed between the copies was the environment, which is the one
// thing that belongs to a suite: its registry, its stubbed `sbx`, its fake GitHub. So that is the
// argument, and everything around it lives here.
//
// `lift.mjs` already makes this case for `openDoor` and `serverBinary` — one copy of a subtle thing
// beats six — and then the code around them was copied anyway.
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { serverBinary } from "../lift.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");

/**
 * Start a `skein-server` on `door` and wait until it answers.
 *
 * `door` is the open listening socket from `openDoor` — not a port number. `env` is what this
 * suite's fixture needs on top of the caller's own environment; `token` is the fixture's API token,
 * omitted by a suite that runs with `SKEIN_NO_API_AUTH`. Resolves `{srv, log}`, where `log()` is
 * everything the server has said on stdout and stderr so far.
 *
 * serverBinary() only builds when run by hand; under `cargo test` the binary arrives pre-built via
 * SKEIN_SERVER_BIN, because a nested cargo fighting the outer one for the build lock is the load
 * that made the review suite flake (SKEIN-119 — the story is on `serverBinary` in lift.mjs).
 *
 * The port arrives as an OPEN listening socket rather than a number — `openDoor` in lift.mjs says
 * why (SKEIN-443). `door.stdio` puts that descriptor at 3 in the child and `door.env` says one was
 * passed; no `SKEIN_ADDR` goes with it, because a server handed a socket reports where the socket is
 * bound instead of binding anywhere of its own (src/bin/skein-server.rs:464).
 */
export async function startServer({ door, env = {}, token = "", cwd = REPO, tries = 100 }) {
  const { port } = door;
  const srv = spawn(serverBinary(), {
    cwd,
    stdio: door.stdio,
    env: { ...process.env, ...door.env, ...env },
  });
  // Our copy of the door goes now the child holds its own. Between the two the port was never
  // unbound, so no second lane could have been handed it.
  door.close();
  let log = "";
  srv.stdout.on("data", d => { log += d; });
  srv.stderr.on("data", d => { log += d; });
  const headers = token ? { Authorization: `Bearer ${token}` } : {};
  // The per-attempt deadline is not decoration: connecting now succeeds the moment the socket
  // exists, whoever is listening on it, because the kernel queues the connection. Without it the
  // first attempt would block for as long as a server that never accepts stays alive, and the
  // "never came up" sentence below — the one that carries the server's own stderr — would never be
  // reached.
  for (let i = 0; i < tries; i++) {
    // The log goes back with the process: the server narrates its failures on stderr (`skein:
    // reading acme: …` when a mirror cannot be made), and a suite that swallows that sentence makes
    // every downstream check fail without its diagnosis.
    try {
      const r = await fetch(`http://127.0.0.1:${port}/api/boxes`, { headers, signal: AbortSignal.timeout(2000) });
      if (r.ok) return { srv, log: () => log };
    } catch {}
    await new Promise(r => setTimeout(r, 100));
  }
  srv.kill();
  throw new Error(`server never came up on ${port}\n${log}`);
}
