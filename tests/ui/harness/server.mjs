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
 * The GitHub credential every suite's server runs on.
 *
 * Shaped like one (`gho_`) and worth nothing: the only GitHub these suites reach is the stub their
 * own `SKEIN_GITHUB_API` names, and it never looks at the header. It is spelled out in the value so
 * that a token turning up in a log, a fixture or a request trace says what it is.
 */
export const FIXTURE_GH_TOKEN = "gho_fixture_not_a_real_credential";

/**
 * Start a `skein-server` on `door` and wait until it answers.
 *
 * `door` is the open listening socket from `openDoor` — not a port number. `env` is what this
 * suite's fixture needs on top of the caller's own environment; `token` is the fixture's API token,
 * omitted by a suite that runs with `SKEIN_NO_API_AUTH`. Resolves `{srv, log}`, where `log()` is
 * everything the server has said on stdout and stderr so far.
 *
 * **`SKEIN_IN_FLEET` is stripped, whatever the caller's shell says.** It is set in every skein box,
 * which is where the work on this project happens — so a suite run by hand inside a box started a
 * server that believed it was the fleet's own cockpit. `sbx ls` is then not asked at all ("which
 * boxes exist is read from their placement records instead"), and `onboarding.mjs`'s check that
 * health reports sbx as `satisfied` failed on a machine where sbx answers perfectly well.
 *
 * It read as a FLAKE rather than as a bug, and that is the part worth keeping: under `cargo test`
 * the gate list in CLAUDE.md says `env -u SKEIN_IN_FLEET cargo …`, so the same check passed there
 * and failed standalone — the same tree, two answers, decided by an ambient variable neither run
 * mentions. A fixture that pins `SKEIN_HOME` and `SKEIN_FLEET_ROOT` and then lets this one through
 * is pinning two thirds of the question it means to ask.
 *
 * **The GitHub credential is pinned to [`FIXTURE_GH_TOKEN`] for the same reason, and it cost a red
 * master to learn** (SKEIN-621). `prq::credentials::look_for_a_credential` reads `$GH_TOKEN`, then
 * `$GITHUB_TOKEN`, then the stored PATs, then `gh auth token`; with none of them `host_token()`
 * returns an error and `prq::refresh::queue_within` fails before it asks GitHub anything. Every
 * skein box exports a `GH_TOKEN` — so on a box the queue suites ran on the *developer's* credential
 * and passed, and the first time CI ran them, on a runner with no token, `actfail`, `connections`
 * and `review` all failed with an empty queue: 16 of 25, 4 of 8 and 62 of 82 checks. Removing
 * `$GH_TOKEN` and `$GITHUB_TOKEN` on an otherwise untouched box reproduces all three exactly.
 *
 * A suite pins `SKEIN_GITHUB_API` at a stub and then has to pin the credential that stub is asked
 * with, or it has pinned half the question — the same sentence as the paragraph above. Pinning it
 * here also means no suite can reach the real api.github.com carrying a real token: the value is
 * not a credential anywhere.
 *
 * A suite that wants the no-credential case says `GH_TOKEN: ""` in its own `env` — empty is "no
 * token" to the reader above, which skips a blank value rather than treating it as one. **That is
 * the first of four sources and not the whole chain**: `look_for_a_credential` falls through to the
 * stored read token, then any write PAT, then `gh auth token` from `$PATH`. A fresh `$SKEIN_HOME`
 * empties the two stored ones; `gh` needs a stub on the suite's `$PATH`, or on a machine where
 * somebody has run `gh auth login` the server is quietly handed that credential and the suite
 * asserts nothing while passing. `hatches.mjs` closes all four and demonstrates the last one.
 *
 * **Both hatches, and both pins, are checked by `tests/ui/hatches.mjs`** — because the order they
 * depend on is invisible where it matters. The pins are assignments here and the escape is the
 * `...env` spread ten lines below; a `childEnv.GH_TOKEN = …` written after the spread instead of
 * before turns the documented hatch into a no-op, and it breaks in exactly one direction: the suite
 * asking for the no-credential case gets a credential and goes green (SKEIN-624). Each of those five
 * edits was made and turns exactly one of that suite's checks red.
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
  // Built rather than spread inline, so the deletion above is visible at the spawn. A suite that
  // genuinely wanted an in-fleet server would pass `SKEIN_IN_FLEET` in its own `env`, which still
  // wins — this drops only what was inherited from whoever typed the command.
  const childEnv = { ...process.env, ...door.env };
  delete childEnv.SKEIN_IN_FLEET;
  // `$GITHUB_TOKEN` goes and `$GH_TOKEN` is replaced, in that order, because the reader takes the
  // first of the two that is set: leaving `$GITHUB_TOKEN` behind would put the caller's own
  // credential back the moment a suite asked for the no-token case with `GH_TOKEN: ""`.
  delete childEnv.GITHUB_TOKEN;
  childEnv.GH_TOKEN = FIXTURE_GH_TOKEN;
  const srv = spawn(serverBinary(), {
    cwd,
    stdio: door.stdio,
    env: { ...childEnv, ...env },
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
