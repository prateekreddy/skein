// Can the leak check see a process that names its fixture only in its environment?
//
// It could not, and that is the second time the same check has been unable to fail (SKEIN-687).
// `harness/leaks.mjs` derives its fixture names from the code rather than restating them — the
// first fix, and it holds — but it tried them against `/proc/<pid>/cmdline` and nothing else. The
// `skein-server` a suite starts is exec'd as a bare binary path with no arguments; which fixture it
// belongs to is in `SKEIN_HOME` and `SKEIN_FLEET_ROOT`. So the process shape these suites leave
// behind most often was the one shape the check could not see, and it printed "nothing is running
// from any of them" on a box where one had been up for seven and a half hours.
//
// So the process this suite starts carries a derived prefix **only in its environment**, and its
// argv is `node -e <a timer>`. Written the other way round — the prefix in the arguments — it would
// have passed before the fix and proved nothing, which is the trap the item named up front. Run it
// against the version before the fix and check 1 says `null` where it wants `environment`:
//
//   node tests/ui/leakcheck.mjs <path to another copy of leaks.mjs>
//
// That argument exists for exactly that demonstration. The copy has to sit at
// `tests/ui/harness/` in a checkout, because `leaks.mjs` finds the repository by walking up from
// its own path and reads the test tree to derive its prefixes.
//
//   node tests/ui/leakcheck.mjs
//
// Needs node and nothing else — no chromium — so it is in the node tier and runs on every
// `cargo test`.
//
// **It is over in well under a second, and that is deliberate.** For as long as the child is alive
// this box carries a process that looks exactly like a leak, because looking exactly like one is
// the whole point; another agent's `leaks.mjs` landing inside that window would name a pid that is
// already gone by the time they look. So the child is started, asked about, and killed with
// nothing in between, and `quiesceOnExit` takes it on every other way out of this process.
import { spawn, spawnSync } from "node:child_process";
import { statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { environOf, fixturePrefixes, processes, quiesceOnExit } from "./harness/leaks.mjs";
import { harness } from "./lift.mjs";

const { check, done } = harness();
const HERE = path.dirname(fileURLToPath(import.meta.url));
const LEAKS = process.argv[2] || path.join(HERE, "harness", "leaks.mjs");

// A fixture prefix this repository really produces, read the way the check reads them. Naming one
// here instead would be the defect this whole file is about, one level up: a prefix the check does
// not know is a prefix that proves nothing when it is not found. Any member does — the first, so
// the choice is not a judgement about which fixture matters.
const { prefixes } = fixturePrefixes();
const prefix = prefixes[0];

// What the child claims to be. Nothing is created on disk: the check reads `/proc` and never the
// filesystem the path names, and a directory here would be one more thing to leave behind.
const fixture = `/tmp/${prefix}leakcheck-${process.pid}`;

// A value shaped like the credential a real server's environment carries, so that "the report does
// not print the environment" is asserted against something a leak would be recognisable in.
// `harness/server.mjs` puts a `$GH_TOKEN` in every suite's server for real.
const SECRET = `gho_leakcheck_not_a_real_credential_${process.pid}`;

// argv names no fixture — `node -e <timer>` — and the environment names one twice, the way
// `startServer` hands a server its own. A minimal environment rather than this process's, so that
// the only thing in it that can match is the thing being tested.
const kid = spawn(process.execPath, ["-e", "setTimeout(() => {}, 30000)"], {
  env: { SKEIN_HOME: `${fixture}/home`, SKEIN_FLEET_ROOT: `${fixture}/fleet`, GH_TOKEN: SECRET },
  stdio: "ignore",
});
quiesceOnExit([], () => { try { kid.kill("SIGKILL"); } catch {} });

/** How `leaks.mjs` reported `pid`: the surface it matched on and the prefix it named, or `null`
 * when it did not report it at all. Read out of the report rather than out of the module, because
 * what a reader is handed is the thing that was wrong. */
function reportOf(out, pid) {
  for (const line of out.split("\n")) {
    const m = line.match(/^\s+(\d+)\s+\S+\s+(argv|environment)\s+(\S+)/);
    if (m && Number(m[1]) === pid) return { where: m[2], prefix: m[3] };
  }
  return null;
}

function report() {
  const r = spawnSync(process.execPath, [LEAKS], { encoding: "utf8" });
  return { out: `${r.stdout}${r.stderr}`, status: r.status };
}

// --- a fixture that is only in the environment -------------------------------------------------
const alive = report();
check("a process naming a fixture only in its environment is found, and by the derived prefix",
  reportOf(alive.out, kid.pid), { where: "environment", prefix });
check("and the gate fails rather than passing over it", alive.status, 1);
check("the report does not print the environment it matched in", alive.out.includes(SECRET), false);
check("this pid's own environment reads", environOf(kid.pid).envState, "read");

// --- and it stops being reported when it stops running -----------------------------------------
// The other half of a check that can fail: one that reports a leak whatever is running would pass
// the checks above while saying nothing. The status is not asserted here — this box belongs to
// several agents at once, so somebody else's leak is a perfectly possible 1.
kid.kill("SIGKILL");
await new Promise(resolve => kid.on("exit", resolve));
const dead = report();
check("and it is gone from the report once the process is", reportOf(dead.out, kid.pid), null);
check("a pid that has gone reads as gone rather than denied", environOf(kid.pid).envState, "gone");

// --- an environment that cannot be read is not a clean miss ------------------------------------
// `/proc/<pid>/environ` opens only for a process this one could inspect, so most of a shared box is
// closed to it — and answering "no match" for those is this check's own bug in miniature: a verdict
// about processes it never looked at. They must classify as `denied`, which is what the gate counts
// and says out loud, and never as `read` with an empty environment, which would match nothing while
// looking like a clean answer.
//
// The population is derived from the box rather than assumed: every process whose `/proc` entry
// belongs to another user. On a machine where there are none the count in the name says so, which
// is the honest form of a check that had nothing to look at.
const mine = process.getuid();
const foreign = processes()
  .map(p => p.pid)
  .filter(pid => { try { return statSync(`/proc/${pid}`).uid !== mine; } catch { return false; } });
const states = foreign.map(pid => environOf(pid).envState);
check(`${foreign.length} processes belong to another user, and an unreadable environment is denied`,
  { read: states.filter(s => s === "read").length, denied: states.filter(s => s === "denied").length },
  { read: 0, denied: foreign.length });

done();
