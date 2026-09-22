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
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { REPORT_CAP, environOf, fixturePrefixes, fixtureRegex, quiesceOnExit }
  from "./harness/leaks.mjs";
import { boxlikeNamespace, harness } from "./lift.mjs";

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

// Every derived prefix as the scan takes them, and a process record built here rather than spawned.
// **A row, not a process, wherever the question is about a STRING** — the same division as
// SKEIN-780: which text the scan matches is a property of the text and the rule, and a box several
// agents share cannot be held still to ask it. `envState: "read"` because `sighting` refuses an
// environment that was never read, and a fake pid answers `parentOf` with null, which is not 1 and
// therefore never an orphan.
const allPatterns = prefixes.map(p => [p, fixtureRegex([p])]);
const rowNaming = (env, args = "sleep 30") =>
  ({ pid: 990000, args, age: 0, env, envVars: env.split(" "), envState: "read" });
/** The prefix `under`'s scan names `row` by, or `null`, with `repo` as the worktree it belongs to. */
const namedBy = (row, repo) => {
  if (!under.fixtureNamed) return "this copy of leaks.mjs does not split the prefix scan";
  // **No lanes and no cohort, passed rather than defaulted** (SKEIN-990). `repo` here is a
  // fabricated worktree path that exists on no disk, and the two defaults would both go looking for
  // it: `otherWorktrees` asks git in that directory, which is not a checkout and refuses, by
  // design. The question these checks ask is which PREFIX names the row — attribution is asserted
  // against real processes, above — so the answers that decide attribution are given here as the
  // empty ones they are for a row with no pid behind it.
  const { carrying } = under.fixtureNamed(
    [row], allPatterns, repo, under.sharedFixtureRoot(), () => false, []);
  return carrying.length ? carrying[0].prefix : null;
};

// A value shaped like the credential a real server's environment carries, so that "the report does
// not print the environment" is asserted against something a leak would be recognisable in.
// `harness/server.mjs` puts a `$GH_TOKEN` in every suite's server for real.
const SECRET = `gho_leakcheck_not_a_real_credential_${process.pid}`;

// Derived here rather than beside the probe that needed them first, because every probe in this
// file now needs one of them: `worktree` is half of what tells this run's leak from another lane's,
// and that rule reaches both scans since SKEIN-913.
const markerName = (() => { try { return under.testMarker(); } catch { return null; } })();
const worktree = (() => { try { return under.ownWorktree(); } catch { return null; } })();
/** What the checks below report instead of a bare bucket when a probe could not be made at all — a
 * copy too old to export either half answers `null` for both, and `null` is three different facts
 * again (SKEIN-796). */
const probeBasis = { marker: markerName, worktree };

// --- a second checkout of this repository, registered with git ---------------------------------
// **Made here, before the first question is put to the module, and that ordering is this file's own
// trap** (SKEIN-990). `otherWorktrees` asks `git worktree list` once and remembers the answer — on
// purpose, so that a lane appearing halfway through a scan cannot be read for some processes and
// not for others — so a checkout registered AFTER the first `fromWorktree` call in this process
// would be invisible to every call after it, and the tmux probe at the bottom of this file would be
// graded against a list that does not contain the lane it belongs to. It would then classify
// `orphans`, which is the bug, reported as the fix having failed. The check beside the probe asks
// git's list for this path rather than trusting the ordering.
//
// `--no-checkout --detach` because nothing here reads a file in it: what is wanted is a path git
// calls a worktree of this repository and a directory a process can stand in. Removed on every way
// out, `--force` because a worktree git considers unclean is still this file's to take away, and
// pruned after, so a removal that raced another lane's `git` leaves no registration behind.
const LANE = path.join(os.tmpdir(), `${prefix}leakcheck-lane-${process.pid}`);
const git = (...args) =>
  spawnSync("git", ["-C", worktree || ".", ...args], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
const laneAdded = git("worktree", "add", "--no-checkout", "--detach", LANE);
const dropLane = () => {
  git("worktree", "remove", "--force", LANE);
  rmSync(LANE, { recursive: true, force: true });
  git("worktree", "prune");
};
quiesceOnExit([], dropLane);
check("a second checkout of this repository can be made for the attribution probes to belong to",
  laneAdded.status === 0 ? { made: true } : { made: false, stderr: (laneAdded.stderr || "").trim() },
  { made: true });

// argv names no fixture — `node -e <timer>` — and the environment names one twice, the way
// `startServer` hands a server its own. A minimal environment rather than this process's, so that
// the only thing in it that can match is the thing being tested.
//
// **`CARGO_MANIFEST_DIR` is the exception, and it is what this probe is now for** (SKEIN-913). This
// child has a live parent — this process — so it is a run IN FLIGHT, which the prefix scan used to
// exit 1 over: any fixture name anywhere in argv or environment was a leak to it, however alive the
// run that owned it. Attributing it to this worktree is what makes it the green plant rather than a
// stranger's process that would be let off for the wrong reason; the variable is the one cargo
// really sets on every test binary it runs, so this is the honest shape of the tie a real run has,
// and it carries no fixture prefix of its own (asserted below, because the whole probe would be
// meaningless if it did).
const kid = spawn(process.execPath, ["-e", "setTimeout(() => {}, 30000)"], {
  env: {
    SKEIN_HOME: `${fixture}/home`,
    SKEIN_FLEET_ROOT: `${fixture}/fleet`,
    CARGO_MANIFEST_DIR: worktree || "/nonexistent-worktree",
    GH_TOKEN: SECRET,
  },
  stdio: "ignore",
});
quiesceOnExit([], () => { try { kid.kill("SIGKILL"); } catch {} });

/** The rows a report printed, in the order it printed them, each tagged with the end of the report
 * it landed in — `head` for the oldest, `tail` for the newest, the end that did not exist at all
 * while the cap was spent from one side (SKEIN-732) — and with the `headline` of the report it
 * landed IN.
 *
 * **The headline is the column SKEIN-913 needed and there was no room for.** The gate prints
 * several reports now — this worktree's orphans, this worktree's run in flight, and everything from
 * elsewhere on the box — and only the first of those is a leak this run must answer for. Which
 * report a row is in is the whole of what changed, and a row on its own cannot say. `head`/`tail`
 * is reset at each headline for the same reason: the cap is spent per report, so a cap line in one
 * report says nothing about the ends of the next.
 *
 * Read out of the report rather than out of the module, because what a reader is handed is the
 * thing that was wrong. The `age` stays the string the report printed — `0s`, or `?` for a process
 * whose age could not be read — so that "could not be read" cannot arrive here looking like a
 * number. */
function rowsOf(out) {
  const rows = [];
  let section = "head";
  let headline = "";
  for (const line of out.split("\n")) {
    const head = line.match(/^\d+ (.+):$/);
    if (head) { headline = head[1]; section = "head"; continue; }
    if (/more, between the oldest/.test(line)) { section = "tail"; continue; }
    const m = line.match(/^\s+(\d+)\s+(\S+)\s+(argv|environment)\s+(\S+)/);
    if (m) {
      rows.push({ pid: Number(m[1]), age: m[2], where: m[3], prefix: m[4], section, headline });
    }
  }
  return rows;
}

/** The pids a report is RED about: the rows under a headline ending "which is this run's leak".
 *
 * Both halves print such a report and both are the same verdict reached by different scans, so this
 * does not care which found them. The phrase is matched at the END of the headline and not
 * anywhere in it, because the report for another lane's processes says "leak" too — it has to, they
 * are one — and the whole point of it is that the leak is somebody else's. A substring test would
 * quietly count those rows as this run's and hand this file the very confusion it is asserting
 * against. */
const leaking = out =>
  rowsOf(out).filter(r => /which is this run's leak$/.test(r.headline)).map(r => r.pid);

/** Every kind of report `pid` is printed under, by the headline's own words — `[]` when it is not
 * printed at all. The four headlines [`main`] writes, each matched at the clause that says whose. */
const listedUnder = (out, pid) => [...new Set(rowsOf(out).filter(r => r.pid === pid).map(r =>
  /which is this run's leak$/.test(r.headline) ? "this run's leak"
    : /not this worktree's/.test(r.headline) ? "another lane's"
      : /a run is in flight rather than a leak$/.test(r.headline) ? "in flight"
        : /cannot be told from here/.test(r.headline) ? "unclear"
          : `an unknown headline: ${r.headline.slice(0, 60)}`))].sort();

/** How `leaks.mjs` reported `pid`: the surface it matched on and the prefix it named, or `null`
 * when it did not report it at all. */
function reportOf(out, pid) {
  const row = rowsOf(out).find(r => r.pid === pid);
  return row ? { where: row.where, prefix: row.prefix } : null;
}

/** The gate, run as a person runs it — over the whole box.
 *
 * **What this may be asked is only what no other process can take away**: that it is red while an
 * orphan of this run's is alive (nothing subtracts from a leak), and that no secret reaches its
 * text. Everything about which ROWS it printed is a question for [`reportOver`] instead. */
function report() {
  const r = spawnSync(process.execPath, [LEAKS], { encoding: "utf8" });
  return { out: `${r.stdout}${r.stderr}`, status: r.status };
}

// --- the gate over the processes this suite made, and nothing else ------------------------------
// **Seven reds in one file had one cause, and it was this file's, not the box's** (SKEIN-1016, and
// SKEIN-780, 781, 796 and 803 before it, each patched one assertion at a time). The gate reads the
// whole of `/proc`, and every check that read the gate's ROWS was therefore a claim about what else
// was running: a report caps at forty rows, twenty from each end, so another lane's thirty young
// fixture processes decide whether the one this suite planted is printed at all. Rerun alone, the
// same check passed — which is SKEIN-913's cost exactly, a red that is somebody else's.
//
// Moving each such check to the module (SKEIN-780's division, which was right for the questions it
// moved) cannot reach what these ask: that [`main`] itself puts a row under the right headline, prints
// both ends of a report, and exits on the count it printed. That is `main`'s own loop, and a copy of it
// here would assert this file's idea of `main`. So `main` is run unchanged, and what is narrowed is
// the one thing that was never this suite's — the process table it lists. `leaks.mjs` enumerates `/proc`
// with `readdirSync` alone; a preload hands it a listing holding only `scope`, and every other read
// (`cmdline`, `environ`, `stat`, `cwd`) goes to the real kernel, so each verdict is exactly the one
// the whole box would get: parentage, cohort and attribution are all read off the real processes.
//
// **And it proves it took, every time.** A preload that stopped applying — `leaks.mjs` moving to
// `opendirSync`, say — would hand these checks the whole box again, and they would go back to
// flaking rather than failing. Both halves print their population, so each run checks that both are
// the size of the scope it was handed; a wider view fails that check, by name, first.
const SCOPE_VAR = "SKEIN_LEAKCHECK_SCOPE";
const SCOPE_PRELOAD = `data:text/javascript,${encodeURIComponent(`
  import fs from "node:fs";
  import { syncBuiltinESMExports } from "node:module";
  const only = new Set((process.env.${SCOPE_VAR} || "").split(",").filter(Boolean));
  const real = fs.readdirSync;
  fs.readdirSync = function (dir, ...rest) {
    const names = real.call(this, dir, ...rest);
    return dir === "/proc" ? names.filter(n => !/^\\d+$/.test(n) || only.has(String(n))) : names;
  };
  syncBuiltinESMExports();
`)}`;
/** Every live process this run made, by the tokens it planted — each fixture path, tag and lane
 * here ends `-<this pid>`, so no other lane's can match, and a cohort this suite built is whole. */
const OURS = new RegExp(`leakcheck-(?:[a-z]+-)*${process.pid}(?![0-9])`);
const ourPids = () => under.processes()
  .filter(p => OURS.test(p.args) || (p.envState === "read" && OURS.test(p.env)))
  .map(p => p.pid);
/** The gate's output and exit status over `ourPids()` alone, checked to have read exactly those. */
function reportOver(what) {
  const scope = ourPids();
  const r = spawnSync(process.execPath, ["--import", SCOPE_PRELOAD, LEAKS],
    { encoding: "utf8", env: { ...process.env, [SCOPE_VAR]: scope.join(",") } });
  const out = `${r.stdout}${r.stderr}`;
  check(`the gate over ${what} read the ${scope.length} processes it was handed and no others`,
    [...out.matchAll(/ of (\d+) processes$/gm)].map(m => Number(m[1])), [scope.length, scope.length]);
  return { out, status: r.status };
}

/** This pid's parent, read here rather than asked of the module: what the module's own answer is
 * is the thing under test, and a probe that asked it would agree with it by construction. Field 4
 * of `/proc/<pid>/stat`, after the last `)` because a `comm` can contain one.
 *
 * It sits up here beside the other plumbing rather than beside the tmux plants it was written for,
 * because the exported-fixture plants below need the same fact and a second spelling of "which
 * process is its parent" is a second thing to get wrong. */
const ppidOf = pid => {
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    return Number(stat.slice(stat.lastIndexOf(") ") + 2).trim().split(/\s+/)[1]);
  } catch {
    return null;
  }
};

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
// **The middle is owned now as well** (SKEIN-1016): that [`main`]'s own loop carries `where` from
// the scan into the record it prints. It was not assertable while the report was the whole box's,
// because naming it costs a row in a full report; over this child alone the row is the only one.
//
// **And the answer says which fact it found, because `null` was three of them** (SKEIN-796, which
// is this same check failing `got null` at `39ae9fe` — where it still read the report, and wanted
// `{where, prefix}`; the line above is what SKEIN-781 replaced it with, and it is why the two items
// are one). A bare `null` could mean the scan never saw this pid, or saw it and could not read its
// environment, or read the environment and the name was not in it — and only the last is the defect
// this check exists to catch. Three facts behind one word is the shape SKEIN-647 and SKEIN-687 are
// both about, one level down.
//
// The start is NOT synchronised, and that is a measurement rather than an omission. SKEIN-796
// proposed that the child might not have exec'd by the time the scan runs; on this platform node's
// spawn does not return until it has, because the child's exec-error pipe closes at exec and the
// parent reads it to the end first. Measured here, 40 children read the instant `spawn` returned:
// 40 of 40 carried the fixture in `/proc/<pid>/environ`, 0 read the parent's environment, and 25
// rounds of this whole check under eight CPU spinners were 25 of 25 green. A handshake would be a
// mechanism against a cause that is not there.
const alive = reportOver("the child naming its fixture only in its environment");
/** Where the scan saw the child's fixture — or which of the three things went wrong instead. */
const sightingOfTheProbe = () => {
  const scanned = under.processes().find(p => p.pid === kid.pid);
  if (!scanned) {
    return { where: null, why: "the scan never saw this pid", stillRunning: environOf(kid.pid).envState };
  }
  const where = sighted(scanned, fixtureRegex([prefix]));
  return where ? { where } : { where: null, why: "the scan saw the pid and not the name", envState: scanned.envState };
};
check("a process naming a fixture only in its environment is found, and by the derived prefix",
  sightingOfTheProbe(), { where: "environment" });
check("the report does not print the environment it matched in", alive.out.includes(SECRET), false);
check("and the row the gate prints for it says it was seen in the environment, by the derived prefix",
  reportOf(alive.out, kid.pid), { where: "environment", prefix });
check("this pid's own environment reads", environOf(kid.pid).envState, "read");

// --- and a run in flight is not a leak, by fixture name either ---------------------------------
// **This check used to be `alive.status === 1`, and that assertion is what SKEIN-913 was.** The
// child above is a fixture-named process of this worktree WITH A LIVE PARENT — this process — and
// the prefix scan exited 1 over it, because any fixture name anywhere in argv or environment was a
// leak to it. That is not a leak; it is a run in flight, and the same output said so about the
// marker scan's own findings in as many words while the prefix scan was going red. Observed on the
// integration tree as five rows of another lane's `rustc`, every one nought seconds old, every one
// gone within the second.
//
// So this is the GREEN plant, and it is asserted as a bucket rather than as an exit status, for
// SKEIN-780's reason one item on: green is now the absence of a red, and any orphan of this
// worktree that another suite in this same `cargo test --all` happens to be leaking produces that
// red without this child having anything to do with it. What this file owns is where the scan PUT
// this pid, which no other lane can reach, and — below, once there is a red plant to compare it
// against — that this pid is not in the report the gate is failing over.
//
// The prefix scan had to be exported to ask this at all. It is the same scan: `fixtureNamed` is
// `main`'s loop, moved, with the split `testMarked` has had since SKEIN-884 applied to its results.
/** Which bucket the prefix scan put the child in, or which part of the question went wrong. */
const NOT_SPLIT = { bucket: "this copy of leaks.mjs does not split the prefix scan" };
const classifyKid = () => {
  if (!under.fixtureNamed) return NOT_SPLIT;
  const all = under.processes();
  const p = all.find(q => q.pid === kid.pid);
  if (!p) return { bucket: "the scan never saw this pid", stillRunning: environOf(kid.pid).envState };
  const split = under.fixtureNamed(all, [[prefix, fixtureRegex([prefix])]]);
  for (const name of ["orphans", "attached", "theirs", "unclear"]) {
    if (split[name].some(r => r.pid === kid.pid)) return { bucket: name, ...probeBasis };
  }
  return { bucket: "named by the scan, and in none of the four lists", ...probeBasis };
};
check("a fixture-named process of this worktree whose parent is alive is a run in flight",
  classifyKid(), { bucket: "attached", ...probeBasis });
// **And the gate's own exit code says so, which is the assertion SKEIN-913 had to give up.** Over
// the whole box, `status === 0` was a claim about every other suite in the same `cargo test --all`;
// over this child alone it is a claim about `main`, and it is the one that fails if the prefix half
// goes back to exiting on whatever it FOUND rather than on what it attributed.
check("and the gate over it alone is green, by its exit code", alive.status, 0);
// The tie to this worktree is `CARGO_MANIFEST_DIR`, and it must not be what the check above matched
// on — otherwise that check would be reading the worktree rather than the fixture and would pass
// whatever the buckets meant.
//
// **There was a check here demanding that the worktree path not read as a fixture name, and it is
// gone rather than repointed** (SKEIN-918). The scan subtracts the worktree from both surfaces now,
// so the tie cannot be the match whatever anyone calls their checkout — which makes that demand
// wrong in both of its possible outcomes and right in none. On this box it passed because
// `/var/tmp/skein-wt-<lane>` happens not to collide, so it asserted nothing that could fail; on a
// lane at `/var/tmp/skein-review-mybranch` it would have gone red over a case the scan now handles
// correctly. Vacuous here, a false alarm there, and a check that cannot fail is worse than no check
// (SKEIN-647) — while one that reddens over something fine teaches people to read past it, which is
// the same defect from the other side (SKEIN-913).
//
// The property it was reaching for is asserted where it CAN fail: "a worktree is not a fixture,
// however the person who made it named it", below, against a fabricated path that really does read
// as a fixture name and is checked to.

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

// --- a process with no name left to match, only the marker -------------------------------------
// **The third time the same check could not fail, and the first two fixes do not reach it**
// (SKEIN-861, SKEIN-873). The probe above names its fixture in its environment, which is the SECOND
// blind spot; this one names it NOWHERE. A namespace fixture's stand-in agent is
// `bwrap … -- bash -c 'echo $$ > <root>/anchor; exec sleep 600'`, and after the `exec`:
//
//   * its arguments are the two words `sleep 600` — the fixture name lived in the `bash -c` script,
//     and the script is gone with the image;
//   * its environment never held the fixture root, because the script interpolated the path instead
//     of exporting it.
//
// So the process a namespace fixture is GUARANTEED to leave behind is exactly the one carrying no
// evidence of which fixture left it, and a scan over both surfaces still returns zero. Measured
// three times in one day: six at `ppid=1` under one worktree, seven from seven `attach.mjs` runs
// aged up to nine minutes, and one on a final gate run — with `leaks.mjs` printing "nothing is
// running from any of them" beside all of them.
//
// **This probe is therefore built to carry no derivable name at all**, which is what makes it a
// test of the third fix and not of the first two. Its environment is written from nothing —
// `spawn` with an explicit `env`, so the suite's own is not inherited — and holds four things:
// the marker, the worktree, a tag this file finds it by, and a credential-shaped secret. No fixture
// prefix appears in either surface, so a copy of `leaks.mjs` from before this fix cannot see it
// however many surfaces it reads.
//
// **And it is orphaned, because that is what the gate now fails on.** `--fork` is load-bearing and
// not belt and braces: without it `setsid` only calls `setsid(2)` and `exec`s in place, so the pid
// stays this process's child and the scan classifies it `attached` — which is the RIGHT answer for
// a process whose parent is alive, and was this check's answer for one run while it was being
// written. With it, `setsid` forks, the parent exits, and the `bash` left behind reparents to pid 1
// before `exec`ing `sleep`. A plain `spawn` from here would
// leave node as a live parent and classify as a run in flight, which is the case that must NOT be
// red. That is also why the pid is found by reading `/proc` for the tag rather than taken from the
// child handle: the handle names `setsid`, which is already gone, and finding it by its environment
// is the thing under test.
//
// `PATH` is pinned in the probe's environment because the lookup of `setsid` happens with the
// environment being handed to the child, and this one is written from nothing.
const ORPHAN_TAG = `skein-test-leakcheck-orphan-${process.pid}`;
const ORPHAN_SECRET = `gho_leakcheck_orphan_not_a_real_credential_${process.pid}`;
/** Kill whatever carries this run's tag, whichever stage of the probe it is at.
 *
 * **By the tag and not by a pid**, and registered BEFORE the probe is started rather than after it
 * is found. A reaper keyed on the pid the poll resolved can only reap a probe the poll DID resolve
 * — so the one case that leaves something running, a poll that timed out, was the one case it did
 * not cover, and neither was the `bash` stage before the `exec`. This is a scan for a string this
 * process invented, so it cannot name another agent's work; the environment is re-read at the
 * moment of the kill, because a bare `sleep 30` is every sleep on this box and this one is nobody's
 * child any more.
 *
 * `sleep 30` rather than something longer is the backstop under the backstop: if both this and
 * `quiesceOnExit` were somehow missed, the probe is gone within half a minute. */
const reapOrphan = () => {
  for (const p of under.processes()) {
    if (p.envState !== "read" || !p.env.includes(`SKEIN_LEAKCHECK_ORPHAN=${ORPHAN_TAG}`)) continue;
    try {
      if (environOf(p.pid).env.includes(ORPHAN_TAG)) process.kill(p.pid, "SIGKILL");
    } catch { /* gone between the scan and the signal, which is the outcome this is for */ }
  }
};
quiesceOnExit([], reapOrphan);
spawn("/usr/bin/setsid", ["--fork", "bash", "-c", "exec sleep 30"], {
  env: {
    PATH: "/usr/bin:/bin",
    [markerName || "SKEIN_MARKER_WAS_NOT_DERIVED"]: "1",
    // The variable cargo really sets on every test binary it runs, measured rather than assumed —
    // `fromWorktree` reads the worktree out of the environment and does not care which name holds
    // it, so this is the honest shape of the tie a real run has.
    CARGO_MANIFEST_DIR: worktree || "/nonexistent-worktree",
    SKEIN_LEAKCHECK_ORPHAN: ORPHAN_TAG,
    GH_TOKEN: ORPHAN_SECRET,
  },
  stdio: "ignore",
});

/** The probe's pid once its argv has gone bare, or `null` with the reason it did not.
 *
 * Polled rather than assumed, and this is the one place in this file that needs to be: the
 * measurement beside the first probe (that `spawn` returns only after the child has `exec`ed) is
 * about the FIRST exec, and the whole point here is a SECOND one — `bash` replacing itself with
 * `sleep` after `setsid` has already gone. Until that happens the argv still reads
 * `bash -c exec sleep 30`, which names nothing either but is not the shape under test.
 *
 * The answer carries what it last saw, so a timeout reports `bash -c …` rather than `null` — three
 * facts behind one word is what SKEIN-796 was about. */
const PROBE_DEADLINE_MS = 10_000;
/** Wait until none of `pids` is in the scan any more, and answer with the ones still there.
 *
 * **A deadline on a condition this suite caused, not a guess at how long a kill takes** (SKEIN-1015's
 * family). Each "it is gone once the process is" check below used to sleep 300ms and look once, and
 * how long a SIGKILLed process takes to leave `/proc` is a scheduling question: it has to run to die,
 * and an orphan has to be reaped by pid 1 as well. Returning the moment the condition holds costs
 * nothing on a quiet box; a scan that goes on reporting a dead pid still fails, at the deadline, with
 * the pids it kept. */
async function goneFromTheScan(pids) {
  const wanted = pids.filter(Boolean);
  const until = Date.now() + PROBE_DEADLINE_MS;
  for (;;) {
    const still = under.processes().filter(p => wanted.includes(p.pid)).map(p => p.pid);
    if (!still.length) return true;
    if (Date.now() >= until) return { stillScanned: still, deadlineExpiredMs: PROBE_DEADLINE_MS };
    await new Promise(r => setTimeout(r, 25));
  }
}
async function orphanProbe() {
  let last = "<never seen>";
  // A wall-clock deadline rather than a count of attempts: one pass reads every readable
  // environment on the box, which was measured at 100-300ms quiet and over a second under load, so
  // a fixed iteration count is a wall time that grows with how busy the box is -- and this file's
  // header promises it is over in well under a second. Ten seconds is the point at which the answer
  // is "this did not happen", not a guess at how long it should take.
  const until = Date.now() + PROBE_DEADLINE_MS;
  while (Date.now() < until) {
    // Found by its environment, never by its argv: the argv is the thing under test. EVERY carrier
    // is judged and not the first — `setsid`'s own parent carries the tag too until it exits, and
    // `/proc` lists in pid order, which is an order the box chooses (see [`exportedProbe`]).
    const carriers = under.processes().filter(p =>
      p.envState === "read" && p.env.includes(`SKEIN_LEAKCHECK_ORPHAN=${ORPHAN_TAG}`));
    if (carriers.length) last = carriers.map(p => p.args).join(" | ");
    const found = carriers.find(p => p.args === "sleep 30");
    if (found) return { pid: found.pid };
    await new Promise(r => setTimeout(r, 25));
  }
  return { pid: null, lastArgvSeen: last, deadlineExpiredMs: PROBE_DEADLINE_MS, ...probeBasis };
}
const orphan = await orphanProbe();

check("an orphaned process whose argv is bare after an exec is still found, by the marker",
  orphan.pid ? { found: true } : orphan, { found: true });
/** How the scan classified the probe: an orphan of this worktree, or which part it failed. */
const classifyOrphan = () => {
  if (!orphan.pid) return { bucket: "the probe was never made", ...orphan };
  const all = under.processes();
  const p = all.find(q => q.pid === orphan.pid);
  if (!p) return { bucket: "the scan never saw this pid", stillRunning: environOf(orphan.pid).envState };
  if (!under.marked || !under.marked(p, markerName)) {
    return { bucket: "seen, and not marked", envState: p.envState, ...probeBasis };
  }
  if (!under.fromWorktree || !under.fromWorktree(p)) {
    return { bucket: "marked, and not attributed to this worktree", ...probeBasis };
  }
  const split = under.testMarked(all, markerName);
  if (split.orphans.some(q => q.pid === orphan.pid)) return { bucket: "orphans" };
  if (split.attached.some(q => q.pid === orphan.pid)) return { bucket: "attached" };
  return { bucket: "marked and this worktree's, and in neither list" };
};
check("and it is an orphan of this worktree rather than a run in flight",
  classifyOrphan(), { bucket: "orphans" });
// **The whole box is asked only what nothing on it can take away** — that the gate is red while
// this orphan lives, because nothing subtracts from a leak, and that the count line is printed.
//
// **And the link that used to be verified by sabotage alone is asserted now.** Over the whole box the
// status is one integer that the prefix half can produce too — any lane's live fixture process makes
// it 1 whatever the marker scan decided — so "the marker bucket reaches the exit code" was not a
// claim this check could make, and it said so. Over this orphan alone it is: the orphan carries no
// fixture name, so the prefix half has nothing to be red about and a 1 can only have come from the
// marker bucket. Disconnecting `marks.length` from `main`'s return makes that check `got 0, want 1`
// on any box, busy or not. Which ROW the gate prints is asked of the same run, for the reason given
// beside [`reportOver`]: `includes(String(pid))` over the whole box's text was also true of any
// longer pid that happened to contain this one, and false of this one once the cap had cut it.
const markerRun = report();
check("and the gate fails rather than passing over it", markerRun.status, 1);
check("the report says how many processes carry the marker, not only that some do",
  new RegExp(`\\$${markerName} is set on \\d+ of \\d+ processes`).test(markerRun.out), true);
// **And it says so when every count is zero**, which is the half the box cannot be asked for. On a
// shared fleet box there is no moment with no marked process on it, so a `if (carrying.length)`
// guard around those lines would print identically to no guard at all and the check above would
// pass with the defect in place — the exact shape of a check that cannot fail. Asked of
// `markerLines` over a split built here, it reproduces every time (SKEIN-780's division).
const quietBox = under.markerLines
  ? under.markerLines(
    { carrying: [], orphans: [], attached: [], theirs: [], theirOrphans: 0, unclear: [] },
    "SKEIN_QUIET", 136)
    .join("\n")
  : "";
check("and it says it on a box where nothing carries the marker at all",
  { population: /is set on 0 of 136 processes/.test(quietBox),
    leaks: /0 of them are this worktree's with nothing left of the run that made them/
      .test(quietBox) },
  { population: true, leaks: true });
const markerOnly = reportOver("the marked orphan alone");
check("and the marker bucket alone reaches the exit code, with nothing else there to be red about",
  markerOnly.status, 1);
check("and it names the orphan it is failing over",
  leaking(markerOnly.out).includes(orphan.pid), true);
check("the report does not print the environment it matched in",
  { wholeBox: markerRun.out.includes(ORPHAN_SECRET), scoped: markerOnly.out.includes(ORPHAN_SECRET) },
  { wholeBox: false, scoped: false });
reapOrphan();
check("and it is gone from the scan once the process is",
  orphan.pid ? await goneFromTheScan([orphan.pid]) : "the probe was never made", true);

// --- a stand-in that EXPORTS its fixture before it execs ---------------------------------------
// **What the probe above cannot be told apart from, and the reason naming it at the spawn is the
// fix** (SKEIN-980, SKEIN-994). The marker finds a process that has shed every name it ever had,
// and that is all it finds: `ppid == 1` is the only other thing known about it, and SKEIN-990
// proved that is not enough, because a stand-in a suite `exec`s is parentless from the moment the
// crossing that started it returns and is not a leak. What settles it is the COHORT —
// `fixturesRunning`, over the processes naming the same fixture directory — and a process that
// names no fixture on either surface can be in no cohort at all.
//
// This repository `exec`s two such stand-ins, and they were measured rather than assumed to be one
// case, because they are not. The bwrap anchor (`lift.mjs`) really did carry no fixture name at
// all: with only its wrapper killed, the marker half called it `orphans` and the prefix half found
// it in none of its four lists. The stalling box's stand-in (`attach.mjs`) was named after all —
// the crossing carries `$SKEIN_HOME` and `$SKEIN_FLEET_ROOT` down from that suite's server — and
// 24 runs of the gate five seconds apart during `node tests/ui/attach.mjs` went red 0 times. That
// name arrives by INHERITANCE, which is the run's to lose and not the fixture's to rely on, so the
// export at that spawn makes it the fixture's own statement instead.
//
// The fix is one line at each spawn and it is not a rule in the check: the script EXPORTS the
// fixture root before `exec`ing, so the name is in the environment `exec` keeps rather than in the
// arguments it replaces (SKEIN-861 is the same distinction, made the other way round). Teaching
// the check to let a bare `sleep` off instead is SKEIN-645 — a check taught to ignore a shape goes
// quiet about every real leak wearing it.
//
// Both plants are that shape exactly: argv the two words `sleep 30`, an environment written from
// nothing, and one exported variable naming a fixture. They differ in one thing, which is the thing
// the cohort rule reads:
//
//   * LEAK — nothing else on the box names its fixture, so nothing is left of the run that made it;
//   * IN FLIGHT — one live process, whose parent is this suite, names the same fixture.
//
// A fixture directory each, for the reason the tmux plants have one each: a cohort is shared by
// name, and two probes under one fixture would vouch for each other. Planted rather than taken from
// a suite, because the rule has to hold on every box; the case below this one ties it to the real
// `boxlikeNamespace` where `bwrap` allows one.
const EXPORTED_TAG = `leakcheck-exported-${process.pid}`;
/** The fixture each plant claims: a derived prefix, so the scan can name it, and a directory that
 * is never created — the check reads `/proc` and never the filesystem a path names. */
const exportedRoot = which => `/tmp/${prefix}leakcheck-stall-${which}-${process.pid}`;
/** Kill whatever carries this run's tag, by the tag and never by a pid — [`reapOrphan`]'s argument,
 * and its reason: a bare `sleep 30` is every sleep on this box, and these are nobody's child. */
const reapExported = () => {
  for (const p of under.processes()) {
    if (p.envState !== "read" || !p.env.includes(`SKEIN_LEAKCHECK_EXPORTED=${EXPORTED_TAG}`)) continue;
    try {
      if (environOf(p.pid).env.includes(EXPORTED_TAG)) process.kill(p.pid, "SIGKILL");
    } catch { /* gone between the scan and the signal, which is the outcome this is for */ }
  }
};
quiesceOnExit([], reapExported);
// Written from nothing, so that the only fixture name either plant carries is the one its own
// script exports. `PATH` because the lookup of `setsid` happens with this environment, and
// `CARGO_MANIFEST_DIR` because that is the variable cargo really ties a test process to its
// worktree with.
const EXPORTED_ENV = {
  PATH: "/usr/bin:/bin",
  [markerName || "SKEIN_MARKER_WAS_NOT_DERIVED"]: "1",
  CARGO_MANIFEST_DIR: worktree || "/nonexistent-worktree",
  SKEIN_LEAKCHECK_EXPORTED: EXPORTED_TAG,
};
// `--fork` for [`orphanProbe`]'s reason: without it `setsid` execs in place and the pid stays this
// process's child, which is a run in flight and the wrong half of the rule.
const plantExported = which =>
  spawn("/usr/bin/setsid",
    ["--fork", "sh", "-c", `export SKEIN_TEST_FIXTURE='${exportedRoot(which)}'; exec sleep 30`],
    { env: EXPORTED_ENV, stdio: "ignore" });
plantExported("leak");
plantExported("inflight");
// The run still using the in-flight fixture: a plain `spawn`, so its parent is this process and
// alive, and it is not descended from the parentless one — the two things `fixturesRunning` counts
// as evidence, and the two the stranded plant must not be given.
const usingStall = spawn("/usr/bin/sleep", ["30"],
  { env: { ...EXPORTED_ENV, SKEIN_TEST_FIXTURE: exportedRoot("inflight") }, stdio: "ignore" });
quiesceOnExit([], () => { try { usingStall.kill("SIGKILL"); } catch { /* already gone */ } });
/** A plant's pid once it has `exec`ed and the kernel has reparented it, or what was seen instead.
 *
 * Polled on BOTH facts, and the second is why this is a poll: `setsid --fork` returns before its
 * child is reparented, so a classification taken at the wrong instant grades a process whose parent
 * is still alive and calls a leak a run in flight. [`startTmux`] settles the same way for the same
 * reason. Found by its environment, never by its argv: the argv is what the fix cannot restore and
 * is the thing under test.
 *
 * **Every process carrying the plant's fixture is judged, and not the first one `/proc` lists**
 * (SKEIN-1015, SKEIN-1017). The in-flight plant has a partner by design — [`usingStall`], which
 * carries the same tag and the same fixture, because that is what makes the fixture in flight — and
 * this used to be `find`. `/proc` lists in pid order, so whenever the partner's pid was the lower of
 * the two, `find` returned the partner on every pass, the partner never satisfies the condition, and
 * the plant was never looked at: both reports show `lastSeen: "/usr/bin/sleep 30 ppid=<this suite>"`,
 * which is the partner's argv — the plant's is `sleep 30`, from `exec` in `sh`. Which pid is lower is
 * decided by whether `setsid` forks before this process spawns the partner, and load decides that:
 * replayed on this box at load 14, `find` returned the partner in 5 of 30 trials. So those items
 * were never a budget that was too short, and a longer one would have waited longer for a process it
 * was not looking at. The answer on a timeout names every carrier it saw and says that the deadline
 * is what expired. */
async function exportedProbe(which) {
  let last = ["<never seen>"];
  const until = Date.now() + PROBE_DEADLINE_MS;
  while (Date.now() < until) {
    const carriers = under.processes().filter(p =>
      p.envState === "read"
      && p.env.includes(`SKEIN_LEAKCHECK_EXPORTED=${EXPORTED_TAG}`)
      && p.env.includes(`SKEIN_TEST_FIXTURE=${exportedRoot(which)}`));
    if (carriers.length) last = carriers.map(p => `${p.args} ppid=${ppidOf(p.pid)}`);
    const found = carriers.find(p => p.args === "sleep 30" && ppidOf(p.pid) === 1);
    if (found) return { pid: found.pid, which };
    await new Promise(r => setTimeout(r, 25));
  }
  return { pid: null, which, carriersSeen: last, deadlineExpiredMs: PROBE_DEADLINE_MS, ...probeBasis };
}
const stallLeak = await exportedProbe("leak");
const stallFlight = await exportedProbe("inflight");
check("two stand-ins export their fixture, exec, and are reparented, which is the shape under test",
  [stallLeak, stallFlight].map(s => (s.pid ? "planted" : s)), ["planted", "planted"]);
/** Both plants graded over ONE scan, so the two answers are about one moment — and asked with a
 * pattern list of the single prefix they were named with. The whole derived list would be answered
 * by SKEIN-979's collision on a box exporting `$SKEIN_UI_FIXTURE_ROOT`, and the case would then
 * pass with the export removed. */
const classifyExported = () => {
  if (!under.fixtureNamed) return { leak: NOT_SPLIT.bucket, inflight: NOT_SPLIT.bucket };
  const all = under.processes();
  const split = under.fixtureNamed(all, [[prefix, fixtureRegex([prefix])]]);
  const verdict = probe => {
    if (!probe.pid) return { bucket: "the probe was never made" };
    if (!all.some(p => p.pid === probe.pid)) return { bucket: "the scan never saw this pid" };
    for (const name of ["orphans", "attached", "theirs", "unclear"]) {
      const row = split[name].find(r => r.pid === probe.pid);
      if (row) return { bucket: name, where: row.where, named: row.prefix, argv: row.args };
    }
    return { bucket: "in none of the four lists", ...probeBasis };
  };
  return { leak: verdict(stallLeak), inflight: verdict(stallFlight) };
};
const stallSplit = classifyExported();
// The paired facts are in the same answer rather than in a check of their own: `where` says the
// name was found in the environment and nowhere else, and `argv` says what the process still looks
// like — two words, naming no fixture, no suite and no run. Without them "orphans" would be true
// for the uninteresting reason and the export would not be what produced it.
check("a stand-in whose script exported its fixture is named by it, on the environment surface",
  stallSplit.leak,
  { bucket: "orphans", where: "environment", named: prefix, argv: "sleep 30" });
check("and one whose fixture a live process is still using is a run in flight, parentless or not",
  { ...stallSplit.inflight, parent: stallFlight.pid ? ppidOf(stallFlight.pid) : null },
  { bucket: "attached", where: "environment", named: prefix, argv: "sleep 30", parent: 1 });
// The gate over both at once: red about the one with nothing left of its run, and silent about the
// one whose fixture is still in use. That pair is what stops this being "make the red go away" —
// the same shape, the same argv, and the verdict turns on the cohort alone.
const stallRun = reportOver("the two stand-ins and the run still using one");
check("the gate is red over the stranded stand-in and not over the one still in use",
  { status: stallRun.status,
    leak: leaking(stallRun.out).includes(stallLeak.pid),
    inflight: leaking(stallRun.out).includes(stallFlight.pid) },
  { status: 1, leak: true, inflight: false });
reapExported();
try { usingStall.kill("SIGKILL"); } catch { /* already gone */ }
check("and both are gone from the scan once the processes are",
  await goneFromTheScan([stallLeak.pid, stallFlight.pid]), true);

// --- and the real anchor a box-like namespace leaves -------------------------------------------
// **The call site itself, because the rule above is only worth what the spawn actually carries**
// (SKEIN-980). `boxlikeNamespace` is what every browser suite that reads a box from the inside
// starts, and the one process it is GUARANTEED to leave is the `exec`'d anchor: `bwrap … -- bash -c
// 'echo $$ > <root>/anchor; exec sleep 600'`, whose argv is two words and whose script is gone with
// the image. Measured on this branch before the change, with only the wrapper killed: the marker
// half answered `orphans`, the prefix half found it in none of its four lists, and the row a reader
// was handed read `environment SKEIN_TEST  sleep 600` — no fixture, no suite, no run.
//
// So the real helper is run here rather than a copy of its spawn: a copy would assert this file's
// idea of the call site and go on passing the morning the call site changed, which is the shape
// `leaks.mjs` derives its every name to avoid.
//
// **Both verdicts, because the fix has to move one and leave the other.** With the wrapper alive
// the anchor is a run in flight; with ONLY the wrapper killed — which is what
// `fx.boxlike.kill()` did for as long as SKEIN-861 was open, and the ordering `stop` exists to get
// right — it is this run's leak, and now says which fixture it was.
//
// It needs a namespace, so it skips where `tests/isolation_bwrap.rs` skips — and says so rather
// than silently, because a skipped check passes and a reader's seeing that it did not run is the
// only defence. Under `$SKEIN_TESTS_NO_SKIP` it is a failure instead; see [`noSkipAsked`].
const bwrapWorks = () =>
  spawnSync("bwrap", ["--dev-bind", "/", "/", "--", "/bin/true"], { stdio: "ignore" }).status === 0;
/** Is this the run that has to PROVE nothing was skipped? The variable's name is read out of the
 * constant that defines it — `testutil::NO_SKIP` — rather than written in here, for [`testMarker`]'s
 * reason: a name spelled twice is a name that is right in one of the two places. `null` is a third
 * answer, "the constant could not be read", and is said in the skip line rather than folded into
 * "no".
 *
 * It is asked at all because `cargo test` shows a PASSING suite's output to nobody, so the line
 * below is a skip a reader meets only when they run this suite by hand —
 * `tests/browser_suites.rs` says in its own words that a skip nobody reads is the failure mode the
 * whole file exists because of. Under `$SKEIN_TESTS_NO_SKIP` it stops being a skip and becomes a
 * failure, which is exactly what that variable means and what the Rust tier's guards already do
 * with it. */
const noSkipAsked = () => {
  try {
    const named = /const NO_SKIP: &str = "([A-Za-z_][A-Za-z0-9_]*)"/
      .exec(readFileSync(path.join(worktree || ".", "src", "testutil.rs"), "utf8"));
    return named ? Boolean(process.env[named[1]]) : null;
  } catch {
    return null;
  }
};
/** Which bucket the prefix scan puts the anchor in, and what its row says — one scan, the single
 * derived prefix its root was named with, for [`classifyExported`]'s reasons. */
const classifyAnchor = pid => {
  if (!under.fixtureNamed) return NOT_SPLIT;
  const all = under.processes();
  if (!all.some(p => p.pid === pid)) return { bucket: "the scan never saw this pid" };
  const split = under.fixtureNamed(all, [[prefix, fixtureRegex([prefix])]]);
  for (const name of ["orphans", "attached", "theirs", "unclear"]) {
    const row = split[name].find(r => r.pid === pid);
    if (row) return { bucket: name, where: row.where, named: row.prefix, argv: row.args };
  }
  return { bucket: "in none of the four lists", ...probeBasis };
};
if (!bwrapWorks()) {
  const asked = noSkipAsked();
  const said = "bwrap cannot make a namespace here, so the anchor a box-like namespace leaves " +
    "cannot be planted and the checks it carries did not run — the same skip as " +
    "tests/isolation_bwrap.rs";
  if (asked === true) {
    // A run that was told to prove nothing was skipped, having skipped something, is a failure and
    // not a note. The ledger is where that has to land: this suite exits on its count.
    check(`${said}, and this run was told to prove nothing was skipped`, "skipped", "ran");
  } else {
    console.log(`⤼ ${said}${asked === null
      ? ", and the constant naming the no-skip variable could not be read, so this run cannot say "
        + "whether it was told to prove otherwise"
      : ""}`);
  }
} else {
  const anchorRoot = `/tmp/${prefix}leakcheck-anchor-${process.pid}`;
  mkdirSync(anchorRoot, { recursive: true });
  const box = await boxlikeNamespace(anchorRoot);
  const held = classifyAnchor(box.ns_pid);
  // Read at the same moment as the verdict above, and compared with the wrapper's own pid rather
  // than with the word "alive": a paired fact whose two sides are both written here proves nothing,
  // and what has to be true for `attached` to mean anything is that this anchor's parent is the
  // bwrap that made it.
  const heldParent = ppidOf(box.ns_pid);
  // Only the wrapper, which is the whole point: `exec` means the anchor pid IS the sleep, so
  // killing bwrap is what strands it. `box.stop()` below is the ordering that does not.
  box.child.kill("SIGKILL");
  const until = Date.now() + PROBE_DEADLINE_MS;
  while (Date.now() < until && ppidOf(box.ns_pid) !== 1) await new Promise(r => setTimeout(r, 25));
  const stranded = classifyAnchor(box.ns_pid);
  check("the anchor a namespace fixture leaves is named by its own fixture while its run is going",
    { ...held, parent: heldParent === box.child.pid ? "the bwrap that made it" : heldParent },
    { bucket: "attached", where: "environment", named: prefix, argv: "sleep 600",
      parent: "the bwrap that made it" });
  check("and once nothing of that fixture is left it is this run's leak, and still names it",
    { ...stranded, parent: ppidOf(box.ns_pid) },
    { bucket: "orphans", where: "environment", named: prefix, argv: "sleep 600", parent: 1 });
  // What a reader is handed, which is what the item was filed about: the row used to say
  // `SKEIN_TEST` and two words. The marker still finds it — both halves reach it now — and the
  // prefix half's report is printed first, so this is the row the reader meets.
  const anchorRun = reportOver("the stranded anchor");
  check("and the row it is red about names the fixture rather than only the marker",
    { red: leaking(anchorRun.out).includes(box.ns_pid), row: reportOf(anchorRun.out, box.ns_pid) },
    { red: true, row: { where: "environment", prefix } });
  box.stop();
  check("and it is gone from the scan once the anchor is", await goneFromTheScan([box.ns_pid]), true);
  rmSync(anchorRoot, { recursive: true, force: true });
}

// --- the prefix half's own red, and the two greens beside it -----------------------------------
// **The rule the prefix scan did not have, planted from both sides** (SKEIN-913). Until this item
// that scan failed on a fixture name found anywhere — so it exited 1 over another lane's live
// `cargo build`, five rows of nought-second-old `rustc`, while the marker scan four lines lower in
// the SAME output said in those words that nothing there was this run's to be red about. Both
// halves ask one question now, and it is asked here of three processes this file makes:
//
//   * RED — a process naming a derived fixture in its ENVIRONMENT ONLY, carrying this worktree,
//     carrying NO marker, with its parent gone. That is this run's leak, and the prefix half is the
//     only half that can see it: there is no marker to find, which is why its REACH was left
//     exactly as wide and only its verdict changed. Narrowing the scan instead is SKEIN-687.
//   * GREEN — the same shape attributed to ANOTHER worktree, parent gone too. A genuine leak, and
//     not this run's: the lane that owns that path is the only one that can tell a leak of its own
//     from a fixture it is still using, and a red nobody here can clear is a red everybody learns
//     to read past. Same answer the marker half has given since SKEIN-884, and the report says
//     whose in the headline those rows appear under.
//   * GREEN — this worktree, parent ALIVE: a run in flight. `kid` is that case too, asserted where
//     it is made; this one exists so that all three can be read out of ONE report, which is where
//     the contradiction was visible in the first place.
//
// **Red and green are asserted on different surfaces, and the asymmetry is the point.** A red is
// owned: an orphan of this worktree makes the gate exit 1 whatever else is on the box, because
// nothing subtracts from a leak. A green is not — `status === 0` would be a claim about every other
// suite in this same `cargo test --all`, any of which may be leaking an orphan of this same
// worktree for a second, and an assertion that fails on the box's timing rather than on the rule is
// SKEIN-798 again. So green is the two things this file does own: which bucket the scan put each
// pid in, and that neither pid is among the rows the report is RED about — both read while the red
// plant holds the exit code at 1, which is what makes their absence mean something.
const ATTRIB_TAG = `skein-test-leakcheck-attrib-${process.pid}`;
const ATTRIB_SECRET = `gho_leakcheck_attrib_not_a_real_credential_${process.pid}`;
// A path no lane on this box has and no derived prefix matches (the second is asserted below, for
// `classifyKid`'s reason: a "not this worktree" that is really "not a fixture" proves nothing).
//
// Not a hypothetical worry: a worktree at `/var/tmp/skein-attrib-old` DOES read as a fixture,
// because `skein-attrib` is a prefix some test really creates — which would make every process of that
// worktree fixture-named by its `CARGO_MANIFEST_DIR` alone. Hence the paired checks, which say so
// rather than quietly proving nothing.
const OTHER_WORKTREE = `/var/tmp/nonesuch-lane-${process.pid}`;
/** Kill whatever carries this run's attribution tag, at whichever stage it is. By the tag and not
 * by a pid, registered before anything is started, and re-reading the environment at the moment of
 * the signal — `reapOrphan`'s argument, which is that `sleep 30` is every sleep on this box. */
const reapAttrib = (which = "") => {
  for (const p of under.processes()) {
    if (p.envState !== "read"
      || !p.env.includes(`SKEIN_LEAKCHECK_ATTRIB=${ATTRIB_TAG}-${which}`)) continue;
    try {
      if (environOf(p.pid).env.includes(ATTRIB_TAG)) process.kill(p.pid, "SIGKILL");
    } catch { /* gone between the scan and the signal, which is the outcome this is for */ }
  }
};
quiesceOnExit([], reapAttrib);
/** An orphan carrying `home` as its fixture and `manifest` as its worktree, and no marker at all.
 *
 * `setsid --fork` for the reason the marker probe gives: without `--fork` this stays a child of
 * this process and classifies as a run in flight, which is the case that must NOT be red — right
 * answer, wrong probe. `PATH` is pinned because the lookup happens in the environment being handed
 * over, and that one is written from nothing.
 *
 * **`cwd` is not tidiness, and it cost a red before it was here.** `bash` puts `PWD` into the
 * environment it hands on whether or not one was passed in — so a probe started from `tests/ui`
 * carries `PWD=<this worktree>/tests/ui` and is attributed to this worktree by it, however
 * carefully `CARGO_MANIFEST_DIR` says otherwise. The "elsewhere" probe classified `orphans` rather than
 * `theirs` on its first run for exactly that, which is the check catching a mistake in the check
 * rather than in the module — and it would have been indistinguishable from the module ignoring
 * `fromWorktree`, had this file not gone and read the environment. Both probes are therefore
 * started from the temporary directory, so the ONLY thing tying either to a worktree is the
 * variable this hands it. */
const plantOrphan = (which, home, manifest) =>
  spawn("/usr/bin/setsid", ["--fork", "bash", "-c", "exec sleep 30"], {
    cwd: os.tmpdir(),
    env: {
      PATH: "/usr/bin:/bin",
      SKEIN_HOME: home,
      CARGO_MANIFEST_DIR: manifest,
      SKEIN_LEAKCHECK_ATTRIB: `${ATTRIB_TAG}-${which}`,
      GH_TOKEN: ATTRIB_SECRET,
    },
    stdio: "ignore",
  });
// **A fixture of its own per plant, and that is load-bearing since SKEIN-990** — `fixturesRunning`
// spares a parentless process while another process OF THE SAME FIXTURE is still running under a
// live parent, which is how a suite's daemonised tmux stops being a leak. All three of these used
// to sit under one fixture directory, so the live `flight` below would have vouched for the orphan
// beside it and the red plant would have classified `attached` — a green that says nothing, in the
// one check that owns the red.
plantOrphan("mine", `${fixture}-orphan/home`, worktree || "/nonexistent-worktree");
plantOrphan("elsewhere", `${fixture}-elsewhere/home`, OTHER_WORKTREE);
// The third, and a plain `spawn` on purpose: its parent is this process and stays alive, so it is a
// run in flight. Reaped by handle as well as by tag, because a live child is this file's to take
// with it.
const flight = spawn("sleep", ["30"], {
  // `PATH` for `plantOrphan`'s reason: the lookup happens in the environment being handed over.
  env: {
    PATH: "/usr/bin:/bin",
    SKEIN_HOME: `${fixture}-inflight/home`,
    CARGO_MANIFEST_DIR: worktree || "/nonexistent-worktree",
    SKEIN_LEAKCHECK_ATTRIB: `${ATTRIB_TAG}-inflight`,
  },
  stdio: "ignore",
});
quiesceOnExit([], () => { try { flight.kill("SIGKILL"); } catch {} });

/** The two orphans' pids once each argv has gone bare, or `null` with what was last seen instead —
 * `orphanProbe`'s shape and its reasons, over two tags rather than one. */
async function attribProbes() {
  const last = {};
  const until = Date.now() + PROBE_DEADLINE_MS;
  while (Date.now() < until) {
    const found = {};
    for (const p of under.processes()) {
      if (p.envState !== "read") continue;
      for (const which of ["mine", "elsewhere"]) {
        if (!p.env.includes(`SKEIN_LEAKCHECK_ATTRIB=${ATTRIB_TAG}-${which}`)) continue;
        last[which] = p.args;
        if (p.args === "sleep 30") found[which] = p.pid;
      }
    }
    if (found.mine && found.elsewhere) return found;
    await new Promise(r => setTimeout(r, 25));
  }
  return { mine: null, elsewhere: null, lastArgvSeen: last, waitedMs: PROBE_DEADLINE_MS,
    ...probeBasis };
}
const pair = await attribProbes();
check("an orphan of this worktree and one of another lane's are planted, named only by a fixture",
  pair.mine && pair.elsewhere ? { planted: true } : pair, { planted: true });

/** Which bucket the prefix scan put each of the three in, and whether the red one carries a marker.
 *
 * `marked` is asserted false because the whole claim is that the PREFIX half produced this red. A
 * probe that carried the marker too would be failed over by the other half and this section would
 * pass with the prefix half's verdict unchanged — a check that cannot fail, which is the thing this
 * file exists to not be. */
const classifyAttrib = () => {
  if (!pair.mine || !pair.elsewhere) return { bucket: "the probes were never made", ...pair };
  if (!under.fixtureNamed) return NOT_SPLIT;
  const all = under.processes();
  const split = under.fixtureNamed(all, prefixes.map(p => [p, fixtureRegex([p])]));
  const bucketOf = pid => {
    for (const name of ["orphans", "attached", "theirs", "unclear"]) {
      if (split[name].some(r => r.pid === pid)) return name;
    }
    return all.some(p => p.pid === pid)
      ? "seen by the scan and named by no prefix"
      : "the scan never saw this pid";
  };
  const red = all.find(p => p.pid === pair.mine);
  return {
    mine: bucketOf(pair.mine),
    elsewhere: bucketOf(pair.elsewhere),
    inFlight: bucketOf(flight.pid),
    markerOnTheRedOne: Boolean(red && under.marked && under.marked(red, markerName)),
  };
};
check("this worktree's orphan is a leak, another lane's is theirs, a live parent is in flight",
  classifyAttrib(),
  { mine: "orphans", elsewhere: "theirs", inFlight: "attached", markerOnTheRedOne: false });
check("and the other lane's path is not itself a fixture name, so `theirs` is about the worktree",
  fixtureRegex(prefixes).test(`CARGO_MANIFEST_DIR=${OTHER_WORKTREE}`), false);

const attribRun = reportOver("the three attribution plants");
check("the gate fails over this worktree's orphan, which only the prefix half can see",
  attribRun.status, 1);
check("and names it among the rows it is red about", leaking(attribRun.out).includes(pair.mine), true);
// The greens, asked of the same report in the same breath — which is how the two verdicts were seen
// to contradict each other, and therefore how they have to be seen to agree. **Where each is
// printed, and not only where it is not.** This was absence alone, deliberately, while the report was
// the whole box's: its cap made "it is listed" a claim about how busy the box was (SKEIN-781). Over
// these three there is no cap to spend, so the report is asked the thing SKEIN-913 was about — that
// another lane's orphan is printed under a headline saying whose it is, and a run in flight under
// one saying so — and being printed under the red headline as well would fail it just the same.
check("and the other lane's orphan is printed as theirs and the run in flight as in flight",
  { elsewhere: listedUnder(attribRun.out, pair.elsewhere), inFlight: listedUnder(attribRun.out, flight.pid) },
  { elsewhere: ["another lane's"], inFlight: ["in flight"] });
check("and no probe's environment reaches the report", attribRun.out.includes(ATTRIB_SECRET), false);

// **And the exit code follows the count the report prints, which is the link nothing else here
// owns.** Every check above holds with `main` returning 1 on everything it FOUND rather than on
// what it attributed — the red plant is found either way — so the defect SKEIN-913 is about would
// survive all of them. What catches it is a run with the red plant reaped and the two greens still
// alive: the gate then prints `0 of them are this worktree's with nothing left of the run that
// made them` in both halves and must exit 0.
//
// **The count is read out of the run's OWN output rather than asked of the module afterwards**, so
// that the status and the count are one moment (SKEIN-803's shape, and SKEIN-798's). Over the whole
// box that was ALL this could say — the counts were whatever the box held, so the check was an
// implication, "the status is 1 exactly when a count is non-zero". Over these two greens alone the
// answer itself is this suite's: both counts nought and the gate green, and a `main` that exits on
// what it found, or prints a count that is not the one it exits on, fails it on any box.
reapAttrib("mine");
// The removal is verified before anything is read into the run that follows it. A reap that missed
// leaves the gate legitimately red and the check below would be reading the plant's own leak as the
// module's answer — which is a sabotage that was a no-op reported as a verdict.
check("the red plant is gone before the green-only run is read",
  await goneFromTheScan([pair.mine]), true);
/** What the gate itself counted as this run's leaks — one number per half, off its own report. */
const redCounted = out =>
  [...out.matchAll(/^ {2}(\d+) of them are this worktree's with nothing left of the run/gm)]
    .map(m => Number(m[1]));
const greenOnly = reportOver("the two green plants");
check("with only the greens left, both halves count no leak and the gate is green",
  { counted: redCounted(greenOnly.out), status: greenOnly.status },
  { counted: [0, 0], status: 0 });

reapAttrib();
try { flight.kill("SIGKILL"); } catch { /* already gone */ }
check("and they are gone from the scan once the processes are",
  await goneFromTheScan([pair.mine, pair.elsewhere, flight.pid]), true);

// --- a tmux a suite daemonises: parentless from birth, and not always this worktree's ----------
// **The sixth time this check went red for a reason a reader could see was not theirs, and the
// first time BOTH of its rules were wrong at once** (SKEIN-990). What it failed over was
// `tmux -S /tmp/<fixture>/fleet/.skein/private/server.tmux new-session -d -s skein-server …`, one
// second old, started by a suite in another worktree that was running at that moment, in a tree
// where no suite was running at all. Twice, over different pids; a minute later, with that suite
// past those tests, the same command exited 0 over 127 processes.
//
// The two rules, and each one alone produces that red:
//
//   * **`ppid == 1` is not "the run that made it has gone" for something daemonised on purpose.**
//     `tmux new-session -d` forks a server and the launcher returns, so a healthy fixture's tmux is
//     parentless in its first second and for its whole life. Age cannot separate them either: both
//     were measured at one second old, and a leak is one second old in its first second too;
//   * **the environment named this worktree, and it was a breadcrumb.** Measured on a live one:
//     the only mention of this tree in it was `OLDPWD=/boxes/…/tree`, left by the `cd` the other
//     lane's agent made on the way INTO its own worktree, while the lane that owned it was named by
//     `PWD` and by nothing else — no `$CARGO_MANIFEST_DIR` and no `$CARGO_TARGET_DIR` in that
//     environment at all, because `own_sandbox(..).exec(..)` hands the tmux a curated one.
//
// So the three plants below are real `tmux new-session -d` servers, because the shape under test is
// what tmux does to its own parentage and nothing else reproduces it. Each has a fixture directory
// of its own, which `fixturesRunning` makes load-bearing: a cohort is shared by fixture name, and
// three probes under one fixture would vouch for each other.
//
//   * THEIRS — its environment names the second checkout above, and names this worktree too, the
//     way the measured one did. The paired assertion is that it really does name this worktree:
//     without it, "not ours" would be true for the uninteresting reason and the check would prove
//     nothing (the trap SKEIN-918's replacement check was written around).
//   * IN FLIGHT — this worktree's, parentless, with one live process of the same fixture beside it
//     whose parent is this suite. The paired assertion is that its parent really is pid 1, so that
//     the green cannot be the old rule quietly passing.
//   * STRANDED — this worktree's, parentless, with NOTHING of its fixture left but its own session
//     child. That child names the fixture in its argv and has a live parent (the tmux), which is
//     exactly what `fleet::supervised` leaves behind for nine hours (SKEIN-645), and the paired
//     assertion is that it is there: a rule that took a descendant for evidence would turn every
//     stranded tmux into a run in flight, and that is the failure this whole file exists to not
//     have — silent, and in the direction nobody looks.
const TMUX = "/usr/bin/tmux";
const TMUX_PLANTS = ["theirs", "inflight", "stranded", "unclear"];
const tmuxRoot = which => `/tmp/${prefix}leakcheck-tmux-${which}-${process.pid}`;
const tmuxSock = which => `${tmuxRoot(which)}/fleet/.skein/private/server.tmux`;
/** The session's command: the supervisor loop `fleet::supervised` really writes, over a doorway
 * script that is not there — so the loop ends at once and the session goes on holding a `sleep`.
 *
 * Not `exec`ed, deliberately — and the trailing `:` is what stops bash `exec`ing it anyway. bash
 * replaces itself with the LAST command of a `-c` string, so without a command after it the shell
 * becomes a bare `sleep 45` and the fixture name leaves the argv under test; measured, as
 * `childHolding: false` on the first run of this section. The `bash` must stay alive NAMING the
 * fixture, because a descendant that names the fixture is the thing the cohort rule has to refuse
 * to count. `45` is the backstop under the backstop, `reapTmux`'s and `quiesceOnExit`'s: if both
 * were somehow missed this is gone within the minute. */
const tmuxHeld = which =>
  `while [ -f '${tmuxRoot(which)}/fleet/.skein/server-doorway.py' ]; do sleep 2; done; sleep 45; :`;
/** Stop every server this section started, by ITS OWN socket — never by a pattern, and never a pid
 * this file did not create. `kill-server` first because that takes the session, the shell and the
 * sleep with it; the pid is then signalled only if the socket answer left something behind. */
const reapTmux = () => {
  for (const which of TMUX_PLANTS) {
    spawnSync(TMUX, ["-S", tmuxSock(which), "kill-server"], { stdio: "ignore" });
    for (const p of under.processes()) {
      if (!p.args.includes(tmuxSock(which)) && !p.args.includes(`${tmuxRoot(which)}/`)) continue;
      try { process.kill(p.pid, "SIGKILL"); } catch { /* gone, which is the outcome this is for */ }
    }
    rmSync(tmuxRoot(which), { recursive: true, force: true });
  }
};
quiesceOnExit([], reapTmux);
/** Start one, and answer with the server's pid once it has SETTLED — or with what happened
 * instead.
 *
 * **Settled is a poll and not an assumption, because it was measured not to be one.** `spawnSync`
 * returns when the client has exited and `new-session -d` does not return before the server holds
 * the session, so it looked as though the one process left naming this socket had to be the server
 * — and in 4 of 12 rounds there were two, with identical argv and consecutive pids, one of them on
 * its way out. A count taken at that instant grades whichever process the scan happened to see
 * first, which is the sort of check that passes for the wrong reason four times in ten.
 *
 * So the condition is the one the section is actually about: exactly one process naming this
 * socket, and the kernel has reparented it. `PROBE_DEADLINE_MS` and a 25ms step, the same shape
 * and the same reasoning as `orphanProbe` — a deadline is the point at which the answer is "this
 * did not happen", and the answer carries what it last saw rather than a bare null (SKEIN-796). */
const startTmux = async (which, env, cwd) => {
  mkdirSync(path.dirname(tmuxSock(which)), { recursive: true });
  const started = spawnSync(
    TMUX,
    ["-S", tmuxSock(which), "new-session", "-d", "-s", "skein-server", "bash", "-c", tmuxHeld(which)],
    { cwd, env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  let last = [];
  const until = Date.now() + PROBE_DEADLINE_MS;
  while (Date.now() < until) {
    last = under.processes().filter(p => p.args.includes(tmuxSock(which)));
    if (last.length === 1 && ppidOf(last[0].pid) === 1) return { pid: last[0].pid, which };
    await new Promise(r => setTimeout(r, 25));
  }
  return { pid: null, which, exit: started.status, stderr: (started.stderr || "").trim(),
    naming: last.map(p => `${p.pid} ppid=${ppidOf(p.pid)} ${p.args.slice(0, 60)}`) };
};
const TMUX_ENV = which => ({
  PATH: "/usr/bin:/bin",
  SKEIN_HOME: `${tmuxRoot(which)}/home`,
  [markerName || "SKEIN_MARKER_WAS_NOT_DERIVED"]: "1",
});
// The measured shape, variable for variable: the lane it belongs to in `PWD`, this worktree left
// behind in `OLDPWD`, and nothing else naming either. It stands in the lane because that is where
// the tmux measured for SKEIN-990 stood — `tmux new-session` takes no `-c` (`src/registry.rs:39`),
// so a server keeps the cwd of the suite that started it.
const theirs = await startTmux("theirs",
  { ...TMUX_ENV("theirs"), PWD: LANE, OLDPWD: worktree || "/nonexistent-worktree" }, LANE);
const inflight = await startTmux("inflight",
  { ...TMUX_ENV("inflight"), CARGO_MANIFEST_DIR: worktree || "/nonexistent-worktree" }, os.tmpdir());
const stranded = await startTmux("stranded",
  { ...TMUX_ENV("stranded"), CARGO_MANIFEST_DIR: worktree || "/nonexistent-worktree" }, os.tmpdir());
// The fourth, and the one with no answer: the same two paths the "theirs" probe carries, and
// standing in NEITHER of them — which is what is left of that shape once the cwd has moved on. It
// is parentless, fixture-named and marked, so it looks exactly like a leak from every angle the
// check has; what it must not do is reach the exit code, in either direction.
const unclear = await startTmux("unclear",
  { ...TMUX_ENV("unclear"), PWD: LANE, OLDPWD: worktree || "/nonexistent-worktree" }, os.tmpdir());
// The suite still using the in-flight fixture: a plain `spawn`, so its parent is this process and
// alive, and it is not descended from the tmux — the two things that make it evidence.
const usingIt = spawn("sleep", ["45"], {
  env: { PATH: "/usr/bin:/bin", SKEIN_HOME: `${tmuxRoot("inflight")}/home` },
  stdio: "ignore",
});
quiesceOnExit([], () => { try { usingIt.kill("SIGKILL"); } catch { /* already gone */ } });
const planted = [theirs, inflight, stranded, unclear];
check("four tmux servers are daemonised, one per fixture, and each is found exactly once",
  TMUX_PLANTS.map(w => planted.find(t => t.which === w)).map(t => (t.pid ? "started" : t)),
  ["started", "started", "started", "started"]);
check("and the kernel has reparented all four, which is what tmux does and why this item exists",
  planted.map(t => (t.pid ? ppidOf(t.pid) : "not started")), [1, 1, 1, 1]);

/** git's own record of the checkouts on this box, or a sentence from a copy that has no such
 * reader — the answer `otherWorktrees` is derived from, asked for directly so that the probes below
 * cannot pass against a list that never contained the lane they belong to. */
const lanesKnown = () => {
  if (!under.otherWorktrees) return { knows: "this copy of leaks.mjs does not ask git for lanes" };
  try {
    const lanes = under.otherWorktrees();
    return { knows: lanes.includes(LANE), andNotItself: !lanes.includes(worktree) };
  } catch (e) {
    return { knows: e.message };
  }
};
check("the other checkouts come from git's own record, and this worktree is not one of them",
  lanesKnown(), { knows: true, andNotItself: true });
/** What `otherWorktrees` does with a path git cannot answer the question for. **Both refusals are
 * asserted, for the reason the marker's two are** (SKEIN-647): an empty list is what this reader
 * returns on a box with one checkout, so "no other lanes" and "this reader is broken" must not be
 * the same answer — the first would make every process on the box unambiguously this worktree's
 * again, which is the SKEIN-990 red with nothing left to catch it. */
const refusalFor = repo => {
  if (!under.otherWorktrees) return "this copy of leaks.mjs does not ask git for lanes";
  try {
    return `it answered with ${under.otherWorktrees(repo).length}`;
  } catch (e) {
    return e.message;
  }
};
const notACheckout = mkdtempSync(path.join(os.tmpdir(), `${prefix}leakcheck-nolane-`));
check("a directory git cannot answer for is refused, not read as a box with no other lanes",
  { outside: /cannot ask git which other checkouts/.test(refusalFor(notACheckout)),
    // A path INSIDE this checkout: git answers, and answers about the checkout rather than about
    // this path — which is the second guard, and the one that catches a reader whose idea of the
    // worktree root has drifted from the one it is comparing against.
    within: /the worktree it is running in/.test(refusalFor(path.join(worktree || ".", "tests"))) },
  { outside: true, within: true });
rmSync(notACheckout, { recursive: true, force: true });

/** Which bucket each half of the check put `pid` in, over one scan of the box so that the two
 * answers are about one moment. Both halves, because the tmux carries the marker as well as the
 * fixture name — the real one did, cargo's `[env]` table reaching it through the server that
 * started it — and a verdict from one half is not the verdict the gate prints. */
const classifyTmux = pid => {
  if (!pid) return { prefix: "the probe was never made", marker: "the probe was never made" };
  if (!under.fixtureNamed || !under.testMarked) return NOT_SPLIT;
  const all = under.processes();
  if (!all.some(p => p.pid === pid)) return { prefix: "the scan never saw this pid", marker: "" };
  const bucketOf = split => {
    for (const name of ["orphans", "attached", "theirs", "unclear"]) {
      if (split[name].some(r => r.pid === pid)) return name;
    }
    return "in none of the four lists";
  };
  return {
    prefix: bucketOf(under.fixtureNamed(all, allPatterns)),
    marker: bucketOf(under.testMarked(all, markerName)),
  };
};
/** Does `pid`'s environment name this worktree at all? The paired fact for the "theirs" probe: the
 * whole claim is that a process naming this worktree can still belong to another checkout, and a
 * probe that had stopped naming it would make that claim vacuously. */
const namesThisWorktree = pid =>
  Boolean(pid && worktree && environOf(pid).env.includes(worktree));
check("a tmux daemonised by a suite in another checkout is that checkout's, though it names this one",
  { ...classifyTmux(theirs.pid), namesThisWorktree: namesThisWorktree(theirs.pid) },
  { prefix: "theirs", marker: "theirs", namesThisWorktree: true });
check("a tmux of a fixture whose suite is still running is a run in flight, parentless or not",
  { ...classifyTmux(inflight.pid), parent: inflight.pid ? ppidOf(inflight.pid) : null },
  { prefix: "attached", marker: "attached", parent: 1 });
/** Is something that is not the tmux itself still naming the stranded fixture, under a live parent?
 * The paired fact for the stranded probe: its session's `bash` is exactly the shape that must NOT
 * count as evidence, and if it had gone the probe would be proving the easy half of the rule. */
const childHolding = () => {
  const held = under.processes().filter(p =>
    p.pid !== stranded.pid && p.args.includes(`${tmuxRoot("stranded")}/`));
  return held.some(p => ppidOf(p.pid) === stranded.pid);
};
check("and one left behind by a suite that has finished is still this run's leak, children and all",
  { ...classifyTmux(stranded.pid), childHolding: childHolding() },
  { prefix: "orphans", marker: "orphans", childHolding: true });
check("and one that names both checkouts and stands in neither is neither's, not this one's",
  { ...classifyTmux(unclear.pid), namesThisWorktree: namesThisWorktree(unclear.pid) },
  { prefix: "unclear", marker: "unclear", namesThisWorktree: true });

// The gate itself, over all four at once — which is where the contradiction would have been
// visible in the first place, and the only place the four verdicts can be read as one output. Over
// these plants and nothing else, for the reason given beside [`reportOver`]: the two "is printed"
// checks below are the pair SKEIN-1016 saw fail, because the report of other lanes' processes is
// the one the whole box fills, and past forty rows a one-second-old tmux of this suite's is not in
// either end of it.
const tmuxRun = reportOver("the four tmux plants");
check("the gate is red over the stranded tmux and none of the other three",
  { status: tmuxRun.status,
    stranded: leaking(tmuxRun.out).includes(stranded.pid),
    theirs: leaking(tmuxRun.out).includes(theirs.pid),
    inflight: leaking(tmuxRun.out).includes(inflight.pid),
    unclear: leaking(tmuxRun.out).includes(unclear.pid) },
  { status: 1, stranded: true, theirs: false, inflight: false, unclear: false });
// Printed, though. A bucket that reaches no exit code and no report would be this check going
// quiet about a process it could not attribute, which is the half of SKEIN-645 that hurts.
check("and the one it cannot attribute is printed with what is not known about it, not dropped",
  rowsOf(tmuxRun.out).some(r => r.pid === unclear.pid && /cannot be told from here/.test(r.headline)),
  true);
check("and it says whose the one from the other checkout is, rather than only that it is not ours",
  rowsOf(tmuxRun.out).some(r => r.pid === theirs.pid && /not this worktree's/.test(r.headline)),
  true);
reapTmux();
try { usingIt.kill("SIGKILL"); } catch { /* already gone */ }
check("and all four are gone from the scan once the servers are",
  await goneFromTheScan(planted.map(t => t.pid)), true);
dropLane();

// --- the marker's NAME is read out of the code, and a disagreement refuses ----------------------
// The derive-and-refuse idiom `fixturePrefixes` already uses, one variable over, and for the same
// reason: `$SKEIN_TEST` is current today, and a scan with the string written into it answers `0`
// for ever the morning somebody renames `util::TEST_MARKER`, with nothing in its output to say so.
//
// Both halves are asserted, because either alone can pass while being wrong. A reader that always
// throws would satisfy the refusal and derive nothing; a reader that never throws would satisfy the
// derivation and accept a marker cargo does not export — which is a scan looking for a string no
// process carries, the SKEIN-647 defect exactly.
//
// The two skeletons are written to a temporary directory rather than asserted against this
// repository, so that the disagreement is a real one this suite made and not a state the tree has
// to be put into.
const skeleton = mkdtempSync(path.join(os.tmpdir(), "skein-leakcheck-marker-"));
// Named by a counter, and NOT by handing a quoted name to the temporary-directory call the way the
// line above does. That is not a style point, and the first draft of this comment proved it twice
// over: `fixturePrefixes` reads this file for that shape, so a quoted `repo-` at a call site here
// would enter the derived prefix list and `fixtureRegex` would match `/repo-<anything>` in any
// command line on the box — and the comment that said so, which merely QUOTED the shape, was itself
// read as a prefix and printed at the head of the gate's own list. Prose is a call site as far as
// the reader is concerned, which is SKEIN-882.
let skeletons = 0;
/** A directory with no `src/util.rs` in it at all — the case where the constant has moved. */
function emptyDir() {
  const at = path.join(skeleton, `nothing-${++skeletons}`);
  mkdirSync(at, { recursive: true });
  return at;
}
function repoNaming(constant, exported) {
  const at = path.join(skeleton, `naming-${++skeletons}`);
  mkdirSync(path.join(at, "src"), { recursive: true });
  mkdirSync(path.join(at, ".cargo"), { recursive: true });
  writeFileSync(path.join(at, "src", "util.rs"),
    `pub const TEST_MARKER: &str = "${constant}";\n`);
  writeFileSync(path.join(at, ".cargo", "config.toml"),
    `[build]\nrustflags = []\n\n[env]\n${exported} = "1"\n`);
  return at;
}
/** `under.testMarker(repo)`'s answer, or the word `refused` — never the exception object, so that
 * "it threw" and "it returned a name" are one comparison. */
const markerOf = repo => {
  try {
    return under.testMarker(repo);
  } catch {
    return "refused";
  }
};
check("a repository whose two definitions of the marker agree yields that name",
  markerOf(repoNaming("SKEIN_FOR_TESTS", "SKEIN_FOR_TESTS")), "SKEIN_FOR_TESTS");
check("and one where cargo exports a different variable is refused, not guessed at",
  markerOf(repoNaming("SKEIN_FOR_TESTS", "SKEIN_SOMETHING_ELSE")), "refused");
check("and one whose constant has moved out of src/util.rs is refused",
  markerOf(emptyDir()), "refused");
check("and this repository names it in both places", markerName, "SKEIN_TEST");

// --- a worktree is not a fixture, however the person who made it named it -----------------------
// **The two needles this file carries can be the same string** (SKEIN-918). `fromWorktree` looks
// for the worktree ROOT and `fixtureRegex` looks for a derived prefix followed by
// `[A-Za-z0-9._-]*`, and 38 of the 56 derived prefixes are bare words with no trailing separator —
// so a checkout at `/var/tmp/skein-review-mybranch` reads as the fixture `skein-review`, and cargo
// puts that path in `$CARGO_MANIFEST_DIR` on every test binary it runs. Every process of that lane
// then lands in the "run is in flight" report under a prefix it has nothing to do with.
//
// The worktree is subtracted from both surfaces before the prefixes go in. Four things have to hold
// at once, and NONE of them is provable alone:
//
//   - the fabricated worktree really does read as a fixture name. Without this the rest is a check
//     on a string that was never going to match, which is this file's own defect in miniature;
//   - subtracting it stops that;
//   - a row that is identical except that the path is ANOTHER lane's — so nothing is subtracted —
//     is still named. This is the control, and it is what tells a working subtraction from a scan
//     that has simply stopped seeing things. Without it, `fixtureNamed` returning nothing at all
//     would pass every line here;
//   - a fixture INSIDE such a worktree is still seen. That is the false negative this change could
//     have traded for, and it is the one that would be strictly worse: only the ROOT is removed, so
//     `<repo>/target/<prefix>-4211` still carries `/<prefix>` in what is left.
//
// Rows rather than processes, and a FABRICATED worktree rather than this one: a checkout cannot be
// renamed mid-suite, and `fixtureNamed` takes the worktree as an argument precisely so the rule can
// be asked about a path the box does not have to hold.
const lanePrefix = prefixes.find(p => !/[-_.]$/.test(p)) || prefix;
const NAMED_LANE = `/var/tmp/${lanePrefix}-mybranch`;
const ANOTHER_LANE = `/var/tmp/skein-wt-somebody-else`;
check("a worktree named after a fixture prefix really does read as one, or the rest proves nothing",
  fixtureRegex(prefixes).test(`CARGO_MANIFEST_DIR=${NAMED_LANE}`), true);
check("and its own processes are not fixture-named by it",
  namedBy(rowNaming(`CARGO_MANIFEST_DIR=${NAMED_LANE}`), NAMED_LANE), null);
check("while the same row read as another lane's still is, which is what says the subtraction did it",
  namedBy(rowNaming(`CARGO_MANIFEST_DIR=${NAMED_LANE}`), ANOTHER_LANE), lanePrefix);
check("and a fixture INSIDE such a worktree is still seen, because only the root is taken out",
  namedBy(rowNaming(`CARGO_MANIFEST_DIR=${NAMED_LANE} SKEIN_HOME=${NAMED_LANE}/target/${lanePrefix}-4211/home`),
    NAMED_LANE),
  lanePrefix);
// Both surfaces, because the subtraction has to reach both and argv is the one a fixture name
// survives an `exec` on least often (SKEIN-687 is the other way round, and both are live).
check("the subtraction reaches argv as well as the environment",
  { argv: namedBy(rowNaming("", `skein-server --root ${NAMED_LANE}`), NAMED_LANE),
    andStillFindsAFixtureThere:
      namedBy(rowNaming("", `skein-server --root /var/tmp/${lanePrefix}-4211`), NAMED_LANE) },
  { argv: null, andStillFindsAFixtureThere: lanePrefix });
// A real fixture outside any worktree is untouched by all of this — the ordinary case, asserted so
// that a subtraction which removed too much could not pass the three above and hide here.
check("and an ordinary fixture outside every worktree is still named",
  namedBy(rowNaming(`SKEIN_HOME=/var/tmp/${lanePrefix}-4211/home`), NAMED_LANE), lanePrefix);
// The path is escaped before it becomes a pattern, and ONE escape now serves two readers —
// `worktreeRegex`, which decides whose a process is, and the subtraction, which decides what it is
// looked at for. An unescaped `.` matches any character, so a lane in `…-a.b` would quietly take
// its own path out of a sibling in `…-axb` and stop seeing that sibling's fixtures. Asked of both
// readers in one check, because a fix applied to one of them is the drift this sharing exists to
// prevent.
const DOTTED = "/var/tmp/skein-wt-a.b";
const SIBLING = "CARGO_MANIFEST_DIR=/var/tmp/skein-wt-axb";
check("the worktree path is escaped before it is matched, for both readers",
  { subtraction: under.withoutWorktree
      ? under.withoutWorktree(rowNaming(SIBLING), DOTTED).env : "not subtracted",
    attribution: under.worktreeRegex ? under.worktreeRegex(DOTTED).test(SIBLING) : "no worktreeRegex" },
  { subtraction: SIBLING, attribution: false });

// --- and neither is the shared root every lane on this box creates its fixtures inside ----------
// **The same defect with the other container** (SKEIN-979). `smoke.mjs` names its fixture with a
// BARE prefix — `skein-ui` — and the root that fixture is created inside is `/var/tmp/skein-uifix`,
// or `/var/tmp/skein-uif-<lane>` under the preamble each agent on this box exports. Both start with
// that prefix, so every process carrying `$SKEIN_UI_FIXTURE_ROOT` and nothing else skein-shaped —
// `cargo`, `rustc`, a running `bash tools/gates.sh` — read as "names a test fixture". It was not
// only noise: a backgrounded gate run is attributable to this worktree AND has lost its parent
// shell, which is this file's definition of this run's leak, so a LIVE gate run could take the
// check red. That is SKEIN-913's lesson arriving through the half SKEIN-913 did not touch.
//
// The rule is that a root is subtracted only where it STANDS ALONE, and the cases below are what
// each half of that sentence costs:
//
//   - the root really does read as a fixture name, or every line after it is a check on a string
//     that was never going to match;
//   - a row that names it and nothing under it is not named by it — the defect;
//   - the same path in a row that does NOT declare it a root still is. The control: without it, a
//     scan that had simply stopped seeing anything would pass every line here;
//   - a fixture INSIDE the root is still named. The false negative this could have been traded for,
//     and the one that would be strictly worse;
//   - a root that is itself a per-run fixture directory cannot blind the scan. This is the line
//     that says the rule survives the next naming convention somebody invents: subtract the root
//     globally, the way the worktree is subtracted, and `SKEIN_HOME=<that root>/home` becomes
//     `/home` and the leak goes invisible. Standing alone is what keeps it visible.
//
// Rows rather than processes, for the reason the block above gives: which text the scan matches is
// a property of the text and the rule, and a box several agents share cannot be held still to ask
// it. The live pair was measured by hand while this was written — a planted orphan carrying only
// the root went from `exit 1` to `exit 0`, and one carrying a fixture under the root stayed red.
/** A tree whose `tests/ui/lift.mjs` is exactly `body`, for the two refusals below. */
function repoWithLift(body) {
  const at = path.join(skeleton, `lift-${++skeletons}`);
  mkdirSync(path.join(at, "tests", "ui"), { recursive: true });
  writeFileSync(path.join(at, "tests", "ui", "lift.mjs"), body);
  return at;
}
/** `under.sharedFixtureRoot(repo)`'s answer, or the word `refused` — `markerOf`'s bargain, so that
 * "it threw" and "it returned a root" are one comparison. */
const rootOf = repo => {
  try {
    return under.sharedFixtureRoot(repo);
  } catch {
    return "refused";
  }
};
const shared = rootOf(worktree || ".");
check("the shared fixture root is read out of tests/ui/lift.mjs, not written into the check",
  shared, { variable: "SKEIN_UI_FIXTURE_ROOT", fallback: "/var/tmp/skein-uifix" });
check("a tree with no tests/ui/lift.mjs is refused rather than defaulted to a path nothing uses",
  rootOf(emptyDir()), "refused");
// Prose is not a call site here either (SKEIN-917): the reader runs over the cut text, so a comment
// quoting the shape cannot satisfy a derivation that is supposed to have found the real one.
check("and one whose only fixtureRoot is inside a comment is refused, because prose is not code",
  rootOf(repoWithLift(
    "// function fixtureRoot() { return process.env.SKEIN_QUOTED || \"/var/tmp/quoted\"; }\n")),
  "refused");

// Built out of a derived prefix rather than spelt out, so these rows keep meaning what they say if
// `skein-ui` is ever renamed: `<prefix>fix` is `skein-uifix`'s shape, and `<prefix>-4211-ab` is the
// shape of what a run creates inside a root.
const rootVar = shared === "refused" ? "NO_SHARED_ROOT_WAS_DERIVED" : shared.variable;
const SHARED_ROOT = `/var/tmp/${lanePrefix}fix`;
const PER_RUN_ROOT = `/var/tmp/${lanePrefix}-4211-ab`;
check("a shared root named after a bare prefix really does read as a fixture, or the rest is empty",
  fixtureRegex(prefixes).test(`${rootVar}=${SHARED_ROOT}`), true);
check("and a process naming it and nothing under it is not fixture-named",
  namedBy(rowNaming(`${rootVar}=${SHARED_ROOT}`), ANOTHER_LANE), null);
check("nor is one naming the DEFAULT root, which is what a lane that exported nothing is using",
  namedBy(rowNaming(`A=${shared === "refused" ? "/nowhere" : shared.fallback}`), ANOTHER_LANE), null);
check("while the same path in a row that does not declare it a root still is, which is the control",
  namedBy(rowNaming(`ELSEWHERE=${SHARED_ROOT}`), ANOTHER_LANE), lanePrefix);
// `env VAR=<path> cmd` puts the assignment in the ARGUMENTS and in no environment at all — the
// shape the reproduction was planted with, and the one surface a root can arrive on alone.
check("the root is read off the arguments as well as the environment",
  namedBy(rowNaming("", `env ${rootVar}=${SHARED_ROOT} cargo test`), ANOTHER_LANE), null);
check("and a fixture INSIDE the shared root is still named, because only a root alone is removed",
  namedBy(rowNaming(`${rootVar}=${SHARED_ROOT} SKEIN_HOME=${SHARED_ROOT}/${lanePrefix}-4211/home`),
    ANOTHER_LANE),
  lanePrefix);
check("a root that is ITSELF a fixture directory cannot blind the scan, whatever anyone renames",
  namedBy(rowNaming(`${rootVar}=${PER_RUN_ROOT} SKEIN_HOME=${PER_RUN_ROOT}/home`), ANOTHER_LANE),
  lanePrefix);
// The live shape in one row: a lane's gate run carries its worktree and its root and nothing else.
// Both subtractions have to fire, and either one alone leaves this named.
check("a row carrying only its worktree and its shared root is named by neither",
  namedBy(rowNaming(`CARGO_MANIFEST_DIR=${NAMED_LANE} ${rootVar}=${SHARED_ROOT}`), NAMED_LANE),
  null);
// Two string properties of the subtraction itself, asked of the text rather than of a verdict,
// because a verdict can be right for the wrong reason: the removal is bounded at its right-hand
// end, and the path is escaped before it becomes a pattern. An unbounded removal would edit one
// lane's paths out of another lane's longer root; an unescaped `.` matches any character.
const subtracted = (env, roots) => (under.withoutSharedRoots
  ? under.withoutSharedRoots(rowNaming(env), roots).env : "not subtracted");
check("one lane's root is not taken out of another lane's longer one",
  subtracted(`A=${SHARED_ROOT}9/x`, [SHARED_ROOT]), `A=${SHARED_ROOT}9/x`);
check("and the root is escaped before it is matched, as the worktree is",
  subtracted("A=/var/tmp/axb", ["/var/tmp/a.b"]), "A=/var/tmp/axb");

// --- and a shape a COMMENT quotes is not a call site --------------------------------------------
// **The guard one variable over could be satisfied by prose** (SKEIN-917, SKEIN-882). `leaks.mjs`
// read every file in the test tree line by line and could not tell a call site from a doc comment
// quoting one — and two comments in this tree quote one, both to explain this very check. So the
// literal ellipsis in them was a derived fixture prefix printed at the head of every run's list,
// and, far worse, a tier's WHOLE contribution could come out of prose: rename every real call site
// in a tier and the refusal above stays green over a prefix that matches nothing. That is SKEIN-647
// hiding inside the thing bought to prevent it.
//
// Three properties, and each is asserted where it can be held still:
//
//   - **the cut itself**, over text written here — a comment's shape must go, and a call site's
//     must NOT, including on a line where an earlier `//` is inside a literal. Trading the false
//     positive for a false negative would be strictly worse: a call site the reader stops seeing
//     is a fixture nothing hunts for, silently;
//   - **the refusal**, over synthetic trees below, which is the assertion SKEIN-882 asks for by
//     name: a tier whose only shape is in prose must REFUSE, not count it;
//   - **this tree**, which is where the defect was measured.
//
// `identity` when the copy under test is too old to export the cutter: that is exactly what such a
// copy does, so the checks describing the fix go red against it and none of them goes green for a
// reason the copy did not earn.
const cut = under.codeOnly || (text => text);

// The name the cases below hide in a call site, and the ONE rule about writing them: a JS shape is
// assembled rather than spelt out, because `fixturePrefixes` reads this file too — a literal
// `mkdtempSync(dir, "…")` here would enter the real derived list and the gate would hunt this box
// for a name only this check ever knew. That is the trap the counter above `emptyDir` exists for,
// and it is SKEIN-882 again. Rust shapes need no such care, and it is not because they are Rust:
// the two readers that look for them are pointed at `tests/*.rs` and `src/*.rs`, and this file is
// in neither — so a Rust call site spelt out here reaches no derivation at all.
const HIDDEN = "skein-cutprobe";
const jsCall = `mkdtempSync(join(dir, ${JSON.stringify(HIDDEN)}))`;
const jsFresh = `freshFixture(root, ${JSON.stringify(HIDDEN)})`;
const rustCall = `Scratch::temp("${HIDDEN}")`;
// The library tier's shape, which is a HELPER and not a call site (SKEIN-1006): the crate's own
// scratch directories are named inside `tempdir`'s body, so what the reader must recognise is the
// definition. A prefix of its own, so that a tree deriving from all three tiers says which name
// came from which — two tiers answering with one string cannot tell a reader that the third was
// read at all.
const LIBRARY_HIDDEN = "skein-libprobe";
const libraryCall =
  `fn tempdir() -> TempDir { env::temp_dir().join(format!("${LIBRARY_HIDDEN}{}-{}", a, b)) }`;

// Each case is a hazard this tree really holds, and `want` is whether the name is still there
// after the cut — `false` where prose must lose it, `true` where a call site must keep it.
//
// **Every one of them turns on a COMMENT boundary, and that is not a style.** The cutter keeps
// literals, so mis-reading one changes nothing observable on its own: the first draft of this list
// had a raw string and an apostrophe case that both passed with the branch they were named for
// DELETED, because a swallowed literal is still text. What a mis-read literal really costs is a
// comment — a stray quote runs on and the `//` inside it is never cut (prose survives), or a `//`
// inside a literal is read as a comment and eats the call site after it (code is lost). So each
// case below puts a comment or a call site where the mis-parse would reach it, and each was
// confirmed red with its branch removed.
const cases = [
  // The two shapes measured in SKEIN-917, in the two forms this tree writes them in.
  ["rust", `/// a doc comment explaining ${rustCall} and nothing more\n`, false],
  ["rust", `/*\n * ${rustCall} inside a block comment\n */\n`, false],
  // Rust block comments NEST: the inner `*/` does not end the outer comment.
  ["rust", `/* /* inner */ ${rustCall} still commented */\n`, false],
  // A `//` INSIDE a literal does not open a comment. This is the false negative that would be worse
  // than the bug: a cutter that stops here eats the rest of the line, call site and all.
  ["rust", `let u = "a//b"; let s = ${rustCall};\n`, true],
  // The same in a RAW string, whose quotes do not pair up the way the ordinary branch assumes —
  // `tests/*.rs` really does carry JSON in one. Read as an ordinary string, `"{"` and `": "` pair
  // off and the `//` that follows becomes a comment that eats this line's call site.
  ["rust", `let b = r#"{"u": "a//b"}"#; let s = ${rustCall};\n`, true],
  // `'"'` is the char literal carrying the string delimiter, a hazard `tests/platform_gates.rs`
  // holds for real. Read as an opening quote it runs to the next `"` — which is inside the comment
  // below — and that comment is then never cut.
  ["rust", `let q = '"';\n/// a doc comment explaining ${rustCall}\n`, false],
  // A LIFETIME is not a literal. Three of them, because two pair off harmlessly and it is the odd
  // one that runs on into the comment below and keeps it.
  ["rust", `fn f<'a>(x: &'a str) -> &'a str { x }\n/// quoting ${rustCall}\n`, false],
  // And a comment AFTER a call site on the same line does not reach back over it.
  ["rust", `let s = ${rustCall}; // and a comment here\n`, true],
  ["js", `/**\n * a doc comment: ${jsFresh}\n */\n`, false],
  ["js", `// a line comment: ${jsCall}\n`, false],
  // A continuation line is a comment because the block it is in is, and NOT because it starts with
  // a `*` — which this said before the case was written out in full, and got `true` for: a bare
  // starred line with no opener above it is code, and a cutter that took the `*` for the comment
  // would cut real code the moment a multiplication began a line.
  ["js", ` * ${jsFresh}\n`, true],
  // An apostrophe in code position, which `leakcheck.mjs` itself writes inside a regexp. Read as a
  // string it runs to the next apostrophe — two lines down — and the comment between them survives.
  ["js", `const re = /of them are this worktree's and gone/gm;\n// quoting ${jsFresh}\nconst d = 'x';\n`,
    false],
  ["js", `const u = "a//b"; const d = ${jsCall};\n`, true],
  // A template literal spans lines and holds a `//` of its own. Same line, so a cutter that does not
  // know the backtick turns the rest of it into a comment.
  ["js", "const u = `a//${h}`; const d = " + `${jsCall};\n`, true],
];
const cutCases = cases.map(([lang, text]) => cut(text, lang).includes(HIDDEN));
check("a shape a comment quotes is cut, and one at a call site survives every quote around it",
  cutCases, cases.map(([, , want]) => want));

// The refusal, over trees this check builds. A synthetic tree rather than this repository for the
// reason `repoNaming` gives: the disagreement has to be one this suite made, not a state the tree
// has to be put into — and "a tier whose every call site is prose" is not a state this tree could
// be put into at all without deleting the fixtures every other suite uses.
//
// **Three tiers rather than two since SKEIN-1006.** `librarySource` defaults to a real definition
// so that every case below states only the thing it is about, exactly as `realRust` and `realNode`
// do for the other two — a default of prose would make every one of these trees refuse for a
// reason none of them is asking about.
function repoDeriving(rustSource, nodeSource, librarySource = realLibrary) {
  const at = path.join(skeleton, `deriving-${++skeletons}`);
  mkdirSync(path.join(at, "tests", "ui", "harness"), { recursive: true });
  mkdirSync(path.join(at, "src"), { recursive: true });
  writeFileSync(path.join(at, "tests", "one.rs"), rustSource);
  writeFileSync(path.join(at, "tests", "ui", "one.mjs"), nodeSource);
  writeFileSync(path.join(at, "src", "one.rs"), librarySource);
  return at;
}
/** `under.fixturePrefixes(repo)`'s prefixes, or the word `refused` — never the exception, so that
 * "it threw" and "it derived something" are one comparison. */
const prefixesOf = repo => {
  try {
    return under.fixturePrefixes(repo).prefixes;
  } catch {
    return "refused";
  }
};
const realRust = `fn t() { let s = ${rustCall}; }\n`;
const realNode = `const d = ${jsCall};\n`;
const realLibrary = `${libraryCall}\n`;
check("a tree whose three tiers all have real call sites derives all three names",
  prefixesOf(repoDeriving(realRust, realNode)), [LIBRARY_HIDDEN, HIDDEN].sort());
check("and one where the rust tier's only shape is in a doc comment REFUSES, not counts it",
  prefixesOf(repoDeriving(`/// ${rustCall} is what this reads\n`, realNode)), "refused");
check("and one where the node tier's only shape is in a block comment REFUSES too",
  prefixesOf(repoDeriving(realRust, `/**\n * ${jsFresh}\n */\n`)), "refused");
// **The third tier gets the same refusal, and it is the property SKEIN-1006 turns on** — a tier
// that derives nothing must stop the check rather than quietly take a name off the count. A tier
// added without one is a tier that can go silent, which is SKEIN-647 hiding inside the fix for it.
check("and one where the library tier's helper is only quoted in prose REFUSES as well",
  prefixesOf(repoDeriving(realRust, realNode, `// ${libraryCall}\n`)), "refused");
// The reason the refusal message is on the SOURCES entry rather than in a conditional inside it:
// each tier must say which shape went missing, and a tier whose message named another tier's shape
// would send its reader to rewrite the wrong reader.
check("and the refusal names the shape the tier that went quiet was reading",
  (() => {
    try {
      under.fixturePrefixes(repoDeriving(realRust, realNode, `// ${libraryCall}\n`));
      return "it did not refuse";
    } catch (e) {
      return { tier: /the library tier/.test(e.message), shape: /fn tempdir\(\)/.test(e.message),
        andNotAnotherTiers: !/Scratch::|mkdtempSync/.test(e.message) };
    }
  })(),
  { tier: true, shape: true, andNotAnotherTiers: true });
// And what was cut is reported rather than dropped in silence: the same distinction the printed
// prefix list is for, one level down. A cutter that fired and a cutter with nothing to remove are
// told apart by this and by nothing else.
const QUOTED = "skein-cutprose";
const both = (() => {
  try {
    return under.fixturePrefixes(repoDeriving(
      `/// this reads Scratch::temp("${QUOTED}") out of prose\n${realRust}`, realNode));
  } catch (e) {
    return { prefixes: `refused: ${e.message}`, quoted: [] };
  }
})();
check("a name only prose carries is reported as quoted and is not a prefix",
  { prefixes: both.prefixes, quoted: both.quoted },
  { prefixes: [LIBRARY_HIDDEN, HIDDEN].sort(), quoted: [QUOTED] });

// This repository, which is where it was measured: `…` was prefix number 57 and is now none.
//
// **`quotedSomething` is asserted, and a red there is not a false alarm.** It says this tree no
// longer quotes a call site anywhere in its prose — at which point this check has nothing left to
// prove about the real tree and should say so out loud rather than pass. The two comments it stands
// on are in `tests/server.rs` and `tests/ui/recovery.mjs`, and they are not named here because a
// list of files is the thing `leaks.mjs` exists to not have.
const derived = (() => {
  try {
    return under.fixturePrefixes();
  } catch (e) {
    return { prefixes: [`refused: ${e.message}`], quoted: [], tiers: {} };
  }
})();
check("this tree's prose is quoted, reported, and in none of the names the gate hunts for",
  { quotedSomething: derived.quoted.length > 0,
    anyQuotedIsAPrefix: derived.quoted.some(q => derived.prefixes.includes(q)),
    ellipsisIsAPrefix: derived.prefixes.includes("…") },
  { quotedSomething: true, anyQuotedIsAPrefix: false, ellipsisIsAPrefix: false });
check("and all three tiers still count real call sites, which is what makes a zero mean anything",
  { rust: derived.tiers.rust > 0, library: derived.tiers.library > 0, node: derived.tiers.node > 0 },
  { rust: true, library: true, node: true });

// --- and the crate's own scratch helper is one of the names the gate hunts for ------------------
// **Every unit test in `src/` was invisible to the prefix half until SKEIN-1006**, because the
// derivation read `tests/` and nothing else while `crate::testutil::tempdir()` — which builds the
// fixture directory of every `#[cfg(test)]` test there is here — lives in `src/`. Measured on this
// branch before the change: 61 prefixes from 75 files, and not one of them began `skein-test`.
//
// The assertion is a JOIN and not a string, and that is the whole care in it. Writing the name
// here would be `leaks.mjs`'s own defect committed in its test — a check that passes on a name
// nothing produces the morning somebody renames the helper. So this repository's REAL
// `src/testutil.rs` is handed to the reader in a synthetic tree, and what is asserted is that what
// the reader finds in it is also in the list the gate prints. Drop the `src` entry from `SOURCES`
// and the tree refuses; point the reader at a shape this file no longer writes and it refuses too;
// derive a name the gate does not publish and the second half goes red.
const realTestutil = (() => {
  try {
    return readFileSync(path.join(worktree || ".", "src", "testutil.rs"), "utf8");
  } catch {
    return null;
  }
})();
const fromRealHelper = realTestutil === null
  ? "this checkout has no src/testutil.rs to read"
  : prefixesOf(repoDeriving(realRust, realNode, realTestutil));
const libraryNames = Array.isArray(fromRealHelper)
  ? fromRealHelper.filter(p => p !== HIDDEN && p !== LIBRARY_HIDDEN)
  : [];
check("the library reader recognises this repository's own scratch helper, and the gate hunts for what it finds",
  { read: Array.isArray(fromRealHelper) ? true : fromRealHelper,
    derivedOne: libraryNames.length,
    andTheGatePublishesIt: libraryNames.every(p => derived.prefixes.includes(p)) },
  { read: true, derivedOne: 1, andTheGatePublishesIt: true });

// The row SKEIN-1005 was filed over, built rather than spawned for `rowNaming`'s reason: a unit
// test's stranded bwrap anchor, whose argv is the two words `exec` left it with and whose only
// fixture path is the `$SKEIN_HOME` it inherited. Before the tier above it was named by nothing —
// one witness (`$SKEIN_TEST`) instead of two, and a process that names no fixture can be in no
// cohort, so `fixturesRunning` could not reach it and it was judged on parentage alone.
//
// `basis` is asserted beside the answer, and not left as a branch that would quietly make this
// check about nothing: without a library-tier name and a worktree there is no row to build, and a
// check that reports "no basis" on both sides is a check that cannot fail.
const anchorNamedBy = libraryNames.length === 1 && worktree
  ? namedBy(rowNaming(
    `CARGO_MANIFEST_DIR=${worktree} SKEIN_HOME=/tmp/${libraryNames[0]}${process.pid}-0`,
    "sleep 60"), worktree)
  : null;
check("a unit test's orphaned anchor, named only by the $SKEIN_HOME it inherited, is named by the prefix scan",
  { basis: libraryNames.length === 1 && Boolean(worktree),
    namedByTheLibrarysPrefix: anchorNamedBy !== null && anchorNamedBy === libraryNames[0] },
  { basis: true, namedByTheLibrarysPrefix: true });

// --- and one worktree does not claim a sibling whose path it is a prefix of ---------------------
// **A bug in the first draft of `fromWorktree`, caught before it shipped and asserted so it cannot
// come back.** The ownership test was a bare `includes` of the repository root, and the worktrees on
// this box are named `/var/tmp/skein-wt-<lane>` — so a lane in `…/skein-wt-leak` matched every
// process of a lane in `…/skein-wt-leakblind`, the second path having the first as a prefix. The
// consequence is not noise: it hands one lane another lane's leaks to be red about, which is the
// false positive this whole half exists to remove.
//
// Asked of the regexp rather than of the box, because two worktrees whose names nest that way are
// not something a test may rely on existing. `/` and `:` and end-of-string are the three
// terminators, the same three `fixtureRegex` uses.
const sibling = under.worktreeRegex ? under.worktreeRegex("/var/tmp/skein-wt-leak") : /(?:)/;
check("a worktree does not claim a sibling whose path merely starts with its own",
  { longerSibling: sibling.test("CARGO_MANIFEST_DIR=/var/tmp/skein-wt-leakblind PWD=/"),
    itsOwnSubdirectory: sibling.test("CARGO_MANIFEST_DIR=/var/tmp/skein-wt-leak/tests PWD=/"),
    itsOwnRootAtTheEnd: sibling.test("CARGO_MANIFEST_DIR=/var/tmp/skein-wt-leak") },
  { longerSibling: false, itsOwnSubdirectory: true, itsOwnRootAtTheEnd: true });
rmSync(skeleton, { recursive: true, force: true });

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
//
// **And it used to demand that the box hold still while it looked** (SKEIN-803). It took three
// observations — [`processes`], then a `statSync` per pid for the owner, then [`environOf`] per pid
// — and asserted `denied === foreign.length`, which is exact equality ACROSS all three. Any other
// user's process that exited between the first and the third classifies as `gone`, the count comes
// up short, and the check goes red saying "an unreadable environment is denied" about a scanner
// that did nothing wrong. On a box several agents share, that is a verdict which is partly somebody
// else's business — the same shape as SKEIN-798 a few lines below, a fact about the box's timing
// standing in for a fact about the scanner, and the shape this whole file is about.
//
// Tolerating a shortfall would be that defect with a bigger constant. What the check OWNS is two
// properties, and neither of them needs the population to be stable:
//
//   - **No foreign process reads.** This user must not be able to read another user's environment,
//     and — the half that is [`environOf`]'s to get wrong — a read that failed must not come back
//     as `read` with an empty `env`, which matches nothing while looking like a clean answer. A
//     process that has since exited cannot make this true; it is asserted over every pid observed.
//   - **A process that was still there said `denied`, not `gone`.** `gone` is the honest answer for
//     one that left and a lie about one that did not, so the owner is re-read AFTER the
//     classification and the demand is made only of the pids that answered. Re-read rather than
//     remembered, because a pid freed mid-scan can be reissued, and `uid` is what tells that apart.
//
// Both are decided from what this process observed, so the check fails on the first bad
// classification and never waits for the box to settle.
const mine = process.getuid();

/** Who `/proc/<pid>` belongs to now, or `null` where there is no such entry any more. */
const ownerOf = pid => { try { return statSync(`/proc/${pid}`).uid; } catch { return null; } };
const isForeign = pid => { const uid = ownerOf(pid); return uid !== null && uid !== mine; };

const foreign = under.processes().map(p => p.pid).filter(isForeign);
const verdicts = foreign.map(pid => ({ pid, state: environOf(pid).envState, stayed: isForeign(pid) }));
const stayed = verdicts.filter(v => v.stayed);
check(`none of the ${foreign.length} processes belonging to another user read`,
  verdicts.filter(v => v.state === "read").length, 0);
check(`and the ${stayed.length} still another user's when the read returned said denied, not gone`,
  { denied: stayed.filter(v => v.state === "denied").length }, { denied: stayed.length });

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
const crowded = reportOver("the crowd");
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
// **And then it went red for the reason it had just been rewritten to survive** (SKEIN-1016): it
// passed alone, and with another lane's suite on the box it printed `ofTheCrowdInTheReport: 0` —
// forty-six processes of this suite's, and not one row of them in the report. The crowd names no
// worktree, so it is printed with every other lane's fixture processes; a pid only tells this suite
// which rows are its own once they are printed, and on a box whose other lanes hold more than twenty
// processes as young as the crowd, the young end is theirs. Asked over the crowd alone the report
// holds nothing else, so the young end is twenty rows and every one of them is the crowd's —
// "every" rather than "at least one" now, because nobody else's row can be there.
const ours = new Set(crowd.map(c => c.pid));
const crowdRows = rowsOf(crowded.out);
const tailRows = crowdRows.filter(r => r.section === "tail");
check("and its young end is printed, where a run's own leak is",
  { tailRows: tailRows.length, everyOneOfThemTheCrowds: tailRows.every(r => ours.has(r.pid)) },
  { tailRows: REPORT_CAP / 2, everyOneOfThemTheCrowds: true });
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
