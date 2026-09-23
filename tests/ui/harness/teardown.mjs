// How a browser suite ends: everything it started stops, and only then does its fixture go.
//
// `srv.kill(); fs.rmSync(fx, { recursive: true, force: true })` ended sixteen suites, and it is a
// race. The kill sends a signal and returns without waiting, and the server's supervisor — a
// detached tmux at `<fleet>/.skein/private/server.tmux`, the doorway loop it runs and the python that
// loop respawns — is not the server's child at all, so it outlives the kill and goes on writing into
// the tree the recursive rm is walking. A directory that gains an entry between its readdir and its
// rmdir throws ENOTEMPTY, which is how `ptystall.mjs` failed after five green checks (SKEIN-1116).
// The suites that wrapped the rm in `try {} catch {}` hid the throw and kept the window: whatever
// the race left behind was a fixture half removed, with a supervisor still running in it.
//
// One copy (SKEIN-1123), so the order is settled in one place rather than re-argued per suite.

import { rmSync } from "node:fs";
import { quiesce, running } from "./leaks.mjs";

/** The check every suite that removes a fixture records, in one wording. */
export const NOTHING_LEFT = "nothing this suite started is still running when its fixture is removed";

/** Stop everything this run started, look for anything still naming `dirs`, and remove them only
 * if nothing is. Returns the argv of whatever is still running, `[]` when the teardown was clean.
 *
 * - [`quiesce`] first: the server kill each `startServer` registered, then SIGTERM and SIGKILL of
 *   everything naming that server's fixture, and it returns when that is done. The suite's own
 *   `srv.kill()` is not needed beside it, and was never enough on its own.
 * - Then [`running`] over `dirs` themselves — argv and environment — which is the question the rm is
 *   about to bet on: does anything still name the directory about to go? A path under a fixture
 *   names the fixture too, so this is at least as wide as the scopes `quiesce` stopped.
 * - **Kept when something is still running.** It is the evidence of what was left, and removing it
 *   would race again. `keep` keeps it anyway (a failed run, `SKEIN_KEEP`); the look still happens,
 *   because a process left behind is a leak whether or not the directory is.
 *
 * `record(name, left)` puts the check into the suite's own ledger — `t.check(name, left, [])` for
 * a `harness()` suite, whose `done()` reads it. A ledger suite has already reported by the time it
 * tears down, so without `record` the failure is printed here in the ledger's own `FAIL` shape, and
 * the suite must fold `left.length` into its exit status. */
export function stopThenRemove(dirs, { keep = false, record = null } = {}) {
  quiesce();
  const left = running(dirs).map(p => p.args);
  if (record) record(NOTHING_LEFT, left);
  else if (left.length) console.log(`  FAIL  ${NOTHING_LEFT}\n        got ${JSON.stringify(left)} want []`);
  if (left.length) console.log(`fixture kept, because something above still names it: ${dirs.join(" ")}`);
  else if (!keep) for (const dir of dirs) rmSync(dir, { recursive: true, force: true });
  return left;
}
