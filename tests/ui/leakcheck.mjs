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
import { mkdirSync, mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
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

// Derived here rather than beside the probe that needed them first, because every probe in this
// file now needs one of them: `worktree` is half of what tells this run's leak from another lane's,
// and that rule reaches both scans since SKEIN-913.
const markerName = (() => { try { return under.testMarker(); } catch { return null; } })();
const worktree = (() => { try { return under.ownWorktree(); } catch { return null; } })();
/** What the checks below report instead of a bare bucket when a probe could not be made at all — a
 * copy too old to export either half answers `null` for both, and `null` is three different facts
 * again (SKEIN-796). */
const probeBasis = { marker: markerName, worktree };

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
//
// **And the answer says which fact it found, because `null` was three of them** (SKEIN-796, which
// is this same check failing `got null` at `ee20693` — where it still read the report, and wanted
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
const alive = report();
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
  for (const name of ["orphans", "attached", "theirs"]) {
    if (split[name].some(r => r.pid === kid.pid)) return { bucket: name, ...probeBasis };
  }
  return { bucket: "named by the scan, and in none of the three lists", ...probeBasis };
};
check("a fixture-named process of this worktree whose parent is alive is a run in flight",
  classifyKid(), { bucket: "attached", ...probeBasis });
// The tie to this worktree is `CARGO_MANIFEST_DIR`, and it must be the ONLY thing in that
// environment that says so — if the worktree path itself carried a derived fixture prefix, the
// check above would be reading a match on the worktree rather than on the fixture and would pass
// whatever the buckets meant. `/var/tmp/skein-wt-<lane>` is how the worktrees on this box are
// named, and `skein-` prefixes are exactly what the reader derives, so this is a near miss rather
// than a remote one.
check("and the worktree path is not itself a fixture name, so the tie and the match are distinct",
  { worktree, alsoReadsAsAFixture: fixtureRegex(prefixes).test(`CARGO_MANIFEST_DIR=${worktree}`) },
  { worktree, alsoReadsAsAFixture: false });

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
async function orphanProbe() {
  let last = "<never seen>";
  // A wall-clock deadline rather than a count of attempts: one pass reads every readable
  // environment on the box, which was measured at 100-300ms quiet and over a second under load, so
  // a fixed iteration count is a wall time that grows with how busy the box is -- and this file's
  // header promises it is over in well under a second. Ten seconds is the point at which the answer
  // is "this did not happen", not a guess at how long it should take.
  const until = Date.now() + PROBE_DEADLINE_MS;
  while (Date.now() < until) {
    // Found by its environment, never by its argv: the argv is the thing under test.
    const found = under.processes().find(p =>
      p.envState === "read" && p.env.includes(`SKEIN_LEAKCHECK_ORPHAN=${ORPHAN_TAG}`));
    if (found) {
      last = found.args;
      if (found.args === "sleep 30") return { pid: found.pid };
    }
    await new Promise(r => setTimeout(r, 25));
  }
  return { pid: null, lastArgvSeen: last, waitedMs: PROBE_DEADLINE_MS, ...probeBasis };
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
// **What this pair owns, and the one link it does not.** The status is a single integer and the
// prefix scan above can produce it too — on a box where another lane's fixture processes are live it
// is 1 whatever the marker scan decided, so "the marker bucket reaches the exit code" is not a
// claim this check can make on its own. Verified by sabotage instead, with the box quiet:
// disconnecting `marks.length` from `main`'s return makes this go `got 0, want 1`. What the two
// checks below own unconditionally is the rest of the chain — that the orphan is in the report, and
// that the run is not green — and the classification above is asked of the module, where the box
// cannot reach it. Same division as SKEIN-780, and the same middle nobody can assert.
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
    { carrying: [], orphans: [], attached: [], theirs: [], theirOrphans: 0 }, "SKEIN_QUIET", 136)
    .join("\n")
  : "";
check("and it says it on a box where nothing carries the marker at all",
  { population: /is set on 0 of 136 processes/.test(quietBox),
    leaks: /0 of them are this worktree's and their parent is gone/.test(quietBox) },
  { population: true, leaks: true });
check("and it names the orphan it is failing over",
  markerRun.out.includes(String(orphan.pid)), true);
check("the report does not print the environment it matched in",
  markerRun.out.includes(ORPHAN_SECRET), false);
reapOrphan();
if (orphan.pid) await new Promise(r => setTimeout(r, 300));
check("and it is gone from the scan once the process is",
  orphan.pid ? under.processes().some(p => p.pid === orphan.pid) : "the probe was never made", false);

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
plantOrphan("mine", `${fixture}/orphan/home`, worktree || "/nonexistent-worktree");
plantOrphan("elsewhere", `${fixture}/elsewhere/home`, OTHER_WORKTREE);
// The third, and a plain `spawn` on purpose: its parent is this process and stays alive, so it is a
// run in flight. Reaped by handle as well as by tag, because a live child is this file's to take
// with it.
const flight = spawn("sleep", ["30"], {
  // `PATH` for `plantOrphan`'s reason: the lookup happens in the environment being handed over.
  env: {
    PATH: "/usr/bin:/bin",
    SKEIN_HOME: `${fixture}/inflight/home`,
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
    for (const name of ["orphans", "attached", "theirs"]) {
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

const attribRun = report();
check("the gate fails over this worktree's orphan, which only the prefix half can see",
  attribRun.status, 1);
check("and names it among the rows it is red about", leaking(attribRun.out).includes(pair.mine), true);
// The greens, asked of the same report in the same breath — which is how the two verdicts were seen
// to contradict each other, and therefore how they have to be seen to agree. Absence rather than
// presence, deliberately: a report caps at forty rows per bucket, so "it is listed" is a claim
// about how busy the box is (SKEIN-781) while "it is not among the red rows" is not.
check("and neither the other lane's orphan nor the run in flight is one of them",
  { elsewhere: leaking(attribRun.out).includes(pair.elsewhere),
    inFlight: leaking(attribRun.out).includes(flight.pid) },
  { elsewhere: false, inFlight: false });
check("and no probe's environment reaches the report", attribRun.out.includes(ATTRIB_SECRET), false);

// **And the exit code follows the count the report prints, which is the link nothing else here
// owns.** Every check above holds with `main` returning 1 on everything it FOUND rather than on
// what it attributed — the red plant is found either way — so the defect SKEIN-913 is about would
// survive all of them. What catches it is a run with the red plant reaped and the two greens still
// alive: the gate then prints `0 of them are this worktree's and their parent is gone` in both
// halves and must exit 0.
//
// **The count is read out of the run's OWN output rather than asked of the module afterwards**, and
// that is the whole of why this is assertable at all. A second observation would be a second
// moment: an orphan another suite in this same `cargo test --all` leaks for a second lands between
// them, and the check goes red over a gate that was right (SKEIN-803's shape, and SKEIN-798's). Read
// from the same text the status came with, there is no window — the implication holds whatever else
// the box did, and it fails the instant the exit code stops following the attribution.
reapAttrib("mine");
await new Promise(r => setTimeout(r, 300));
// The removal is verified before anything is read into the run that follows it. A reap that missed
// leaves the gate legitimately red and the check below would be reading the plant's own leak as the
// module's answer — which is a sabotage that was a no-op reported as a verdict.
check("the red plant is gone before the green-only run is read",
  under.processes().some(p => p.pid === pair.mine), false);
/** What the gate itself counted as this run's leaks — one number per half, off its own report. */
const redCounted = out =>
  [...out.matchAll(/^ {2}(\d+) of them are this worktree's and their parent is gone/gm)]
    .map(m => Number(m[1]));
const greenOnly = report();
const counted = redCounted(greenOnly.out);
check("with only the greens left, the exit code is whatever the report's own two counts say",
  { halves: counted.length, status: greenOnly.status },
  { halves: 2, status: counted.some(n => n > 0) ? 1 : 0 });

reapAttrib();
try { flight.kill("SIGKILL"); } catch { /* already gone */ }
await new Promise(r => setTimeout(r, 300));
check("and they are gone from the scan once the processes are",
  under.processes().some(p => [pair.mine, pair.elsewhere, flight.pid].includes(p.pid)), false);

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
