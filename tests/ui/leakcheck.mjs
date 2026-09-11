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
import { fileURLToPath, pathToFileURL } from "node:url";
import { REPORT_CAP, environOf, fixturePrefixes, fixtureRegex, quiesceOnExit }
  from "./harness/leaks.mjs";
import { harness } from "./lift.mjs";

const { check, done } = harness();
const HERE = path.dirname(fileURLToPath(import.meta.url));
const LEAKS = process.argv[2] || path.join(HERE, "harness", "leaks.mjs");

// The module the checks below put their questions to — `LEAKS`, and not the static import, so that
// the demonstration in the header keeps working now that the first check asks the scan directly:
// point this suite at a copy from before SKEIN-687 and that check goes red, because that copy's
// scan cannot reach an environment. It was the spawned gate that carried the demonstration before.
//
// What the static import is for is this file's own plumbing: `quiesceOnExit`, which must be this
// checkout's or a copy that throws leaves the box dirty; `fixturePrefixes` and `fixtureRegex`,
// which build the needle rather than answer with it; and `environOf`, which the checks use to say
// what the box was, not to grade the copy.
const under = await import(pathToFileURL(LEAKS));

/** Where `under`'s scan saw `needle` in `p`, and `null` from a copy too old to export [`sighting`]
 * at all — the same answer as "looked and did not see it", which is also what it means here: a scan
 * that does not reach the environment. The fallback can only produce red, never a green it did not
 * earn. */
const sighted = (p, needle) => (under.sighting ? under.sighting(p, needle) : null);

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

/** The rows a report printed, in the order it printed them, each tagged with the end of the report
 * it landed in: `head` for the oldest, `tail` for the newest — the end that did not exist at all
 * while the cap was spent from one side (SKEIN-732).
 *
 * Read out of the report rather than out of the module, because what a reader is handed is the
 * thing that was wrong. The `age` stays the string the report printed — `0s`, or `?` for a process
 * whose age could not be read — so that "could not be read" cannot arrive here looking like a
 * number. */
function rowsOf(out) {
  const rows = [];
  let section = "head";
  for (const line of out.split("\n")) {
    if (/more, between the oldest/.test(line)) { section = "tail"; continue; }
    const m = line.match(/^\s+(\d+)\s+(\S+)\s+(argv|environment)\s+(\S+)/);
    if (m) rows.push({ pid: Number(m[1]), age: m[2], where: m[3], prefix: m[4], section });
  }
  return rows;
}

/** How `leaks.mjs` reported `pid`: the surface it matched on and the prefix it named, or `null`
 * when it did not report it at all. */
function reportOf(out, pid) {
  const row = rowsOf(out).find(r => r.pid === pid);
  return row ? { where: row.where, prefix: row.prefix } : null;
}

function report() {
  const r = spawnSync(process.execPath, [LEAKS], { encoding: "utf8" });
  return { out: `${r.stdout}${r.stderr}`, status: r.status };
}

// --- a fixture that is only in the environment -------------------------------------------------
// **This check read the report, and that was SKEIN-780's premise a second time** (SKEIN-781). It
// asserted the child appeared in the gate's output, which is true only while the child is among the
// twenty newest fixture-named processes ON THE WHOLE BOX — the report reads `/proc`, not this
// suite's children, and it spends a cap of forty on twenty at each end. Under ~400 live fixture
// processes it failed 3 runs of 3, as `got null`: the symptom of the bug it guards, reported where
// that bug was not.
//
// **Moving it to the module is not a retreat from the argument beside [`rowsOf`]**, that what a
// reader is handed is the thing that was wrong. What was wrong in SKEIN-687 was the SCAN: [`main`]
// printed faithfully what it was given, and it was given nothing, because argv was all that was
// read. So the defect's own surface is [`sighting`] over [`processes`], and that is what is asked
// here — by the derived prefix, and answering `environment` rather than `argv`, which is the whole
// of what the fix added. The report keeps the two assertions it alone can make and that the cap
// cannot take away: that the gate FAILS while this child is alive, and that its text never carries
// the environment it matched in. And which rows survive the cap — the link between the two — is
// asked of [`reportLines`] at the bottom of this file, over rows this suite owns (SKEIN-780).
//
// What no check here can own is the middle: that [`main`]'s own loop carries `where` from the scan
// into the record it prints. Naming it costs a row in a full report, and a row in a full report is
// the thing that is not this suite's to ask for.
const alive = report();
const scanned = under.processes().find(p => p.pid === kid.pid);
check("a process naming a fixture only in its environment is found, and by the derived prefix",
  scanned ? sighted(scanned, fixtureRegex([prefix])) : null, "environment");
check("and the gate fails rather than passing over it", alive.status, 1);
check("the report does not print the environment it matched in", alive.out.includes(SECRET), false);
check("this pid's own environment reads", environOf(kid.pid).envState, "read");

// --- and it stops being seen when it stops running ---------------------------------------------
// The other half of a check that can fail: a scan that reported a leak whatever is running would
// pass the check above while saying nothing.
//
// Asked of the scan for the same reason the check above is, and against the same premise — this one
// carried it inverted. It read the report and demanded `null`, which a full report hands back for a
// child it merely truncated away; on a busy box it therefore passed without looking at anything,
// which is the failure mode of a check that cannot fail rather than one that fails wrongly. Neither
// form is this suite's to own, and `/proc` is: a pid this process has reaped is not in it.
//
// The gate's exit status is not asserted for the dead child — this box belongs to several agents at
// once, so somebody else's leak is a perfectly possible 1.
kid.kill("SIGKILL");
await new Promise(resolve => kid.on("exit", resolve));
check("and it is gone from the scan once the process is",
  under.processes().some(p => p.pid === kid.pid), false);
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
const foreign = under.processes()
  .map(p => p.pid)
  .filter(pid => { try { return statSync(`/proc/${pid}`).uid !== mine; } catch { return false; } });
const states = foreign.map(pid => environOf(pid).envState);
check(`${foreign.length} processes belong to another user, and an unreadable environment is denied`,
  { read: states.filter(s => s === "read").length, denied: states.filter(s => s === "denied").length },
  { read: 0, denied: foreign.length });

// --- and a full report does not hide the leak you just made ------------------------------------
// **This is the check that was missing when CI went red** (SKEIN-732). The report caps at forty
// lines, `shown` is sorted oldest first, and it used to print `slice(0, 40)` — so past forty the
// processes it dropped were the NEWEST. The one a run has just leaked is by definition the newest
// thing on the box, which made the report least able to show exactly what it exists to show.
//
// **And then the check written for it asserted something this suite does not own** (SKEIN-780). It
// planted forty-five processes and one more started last, and demanded that last one appear in the
// report — true only while it is the newest fixture-named process ON THE WHOLE BOX, and the report
// reads `/proc`, not this suite's children. It failed a full `cargo test --all` as `got null`, the
// symptom of the very bug it guards, reported where that bug was not; standalone and on an
// immediate re-run it passed. Two reasons it was never the suite's to claim, and neither is a
// matter of the box being unusually busy:
//
//   - **Forty lines is all there is.** Twenty go to the oldest and twenty to the newest, so twenty
//     fixture processes older than this suite's and twenty ranked newer leave no room for any of
//     the forty-six it plants — whichever one you then ask about. That is an ordinary minute on a
//     fleet box several agents share, and it is exactly CI's twenty-three sibling suites.
//   - **The sort cannot tell them apart anyway.** [`ageOf`] is in whole seconds, so all forty-six
//     tie at `0`, and what decides the order of a tie is the order `readdirSync("/proc")` returns
//     — which is lexicographic, not numeric:
//
//       node -e 'console.log(require("node:fs").readdirSync("/proc").slice(0,4).join(" "))'
//       1 10 1079 1080
//
//     So "started last" was never a rank the report could be asked for. There is no `youngest`
//     here now, because there was never anything this suite could say about it.
//
// The two halves are therefore asserted where each of them holds still. **The box half** is what
// real processes can own: planting more than the cap makes the cap trip however busy the box is,
// and makes the young end of the report as young as anything on the box gets. **The cap's own
// arithmetic** — which rows survive it — is a property of a list and nothing else, so it is asked
// of [`reportLines`] directly, over rows this suite builds and no other agent can add to. That
// half reproduces `got null` unconditionally the moment the cap goes back to one end.
//
// `sleep` rather than node for the crowd, because forty-six node processes is a gigabyte of runner
// memory to prove a formatting bug.
const CAP_PROBE = REPORT_CAP + 6;
const crowd = [];
for (let i = 0; i < CAP_PROBE; i++) {
  crowd.push(spawn("sleep", ["30"], {
    env: { SKEIN_HOME: `${fixture}/crowd${i}/home`, SKEIN_FLEET_ROOT: `${fixture}/crowd${i}/fleet` },
    stdio: "ignore",
  }));
}
quiesceOnExit([], () => { for (const c of crowd) { try { c.kill("SIGKILL"); } catch {} } });
const crowded = report();
check("the report says it could not fit them all", /more, between the oldest/.test(crowded.out), true);
// The young end printed at all, and carrying THIS RUN'S OWN processes — asked by pid, which is a
// fact about the crowd, and not by age, which is a fact about the clock (SKEIN-798).
//
// It read `r.age === "0s"`, under a comment claiming the tail is `0s` rows "whatever else the box
// is running", and that was the false part. [`ageOf`] is whole seconds ROUNDED, so a crowd member
// prints `0s` only while it is under half a second old. Measured on this box: spawning the crowd
// and reading `/proc` costs 283ms quiet, and 1,138-1,542ms under twenty-two spinners and eight
// `while :; do /bin/true; done` storms — at which point every row in the report prints `1s` and
// this went red three runs of three, while the property it stands for was perfectly intact, the
// crowd holding 20 of the 20 tail rows in both conditions. Widening it to accept `1s` as well
// would be the same defect with a bigger constant, and would break again on a slower box.
//
// The pid is what "a run's own leak" actually means, and the crowd's pids are in hand right here.
// `age` was only ever a proxy for "is one of mine", and a bad one: it is the only term in the check
// that moves with how busy the box is.
//
// Still "at least one" rather than "all": the young end is twenty rows and the crowd is forty-six,
// and on a box several agents share some of those rows are somebody else's.
const ours = new Set(crowd.map(c => c.pid));
check("and its young end is printed, where a run's own leak is",
  rowsOf(crowded.out).some(r => r.section === "tail" && ours.has(r.pid)), true);
for (const c of crowd) { try { c.kill("SIGKILL"); } catch {} }

// --- the cap, over rows nobody else can add to -------------------------------------------------
// Oldest first, the order [`main`] hands them over in, and six more than the cap so that it has to
// choose. The two that must survive are the ends: the oldest, which is the leak that has been
// accumulating, and the newest, which is yours. `slice(0, 40)` keeps the first and drops the
// second, and this says so as `got null` — the CI symptom verbatim, on any box, every time.
const owned = [];
for (let i = 0; i < CAP_PROBE; i++) {
  owned.push(
    { pid: 900000 + i, age: CAP_PROBE - i, where: "environment", prefix, args: "sleep 30" });
}
const capped = under.reportLines(owned).join("\n");
check("the oldest row survives the cap",
  reportOf(capped, owned[0].pid), { where: "environment", prefix });
check("and so does the newest, which `slice(0, 40)` dropped",
  reportOf(capped, owned[owned.length - 1].pid), { where: "environment", prefix });
// `marker &&` rather than indexing it: a report with no marker at all is one of the shapes under
// test, and a check that throws on the answer it is there to catch reports nothing at all.
const marker = capped.match(/… (\d+) more/);
check("and it prints the cap exactly, and says how many rows it skipped",
  { rows: rowsOf(capped).length, skipped: marker && Number(marker[1]) },
  { rows: REPORT_CAP, skipped: owned.length - REPORT_CAP });

done();
