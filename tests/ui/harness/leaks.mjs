// What a browser suite leaves running, and the two things anyone wants to do about it: stop this
// run's processes on the way out, and find out whether a previous run's are still here.
//
// **Both halves exist because the check that was supposed to catch this could not fail.** The gate
// list ended with
//
//     ps -eo pid,args | grep -v grep | grep -cE 'ui-onboard-|skein-fleet-it-|skein-move-it-'   # → 0
//
// and it answered `0` on a box with 195 matching processes, 122 of them older than half an hour and
// the oldest over nine (SKEIN-647). The three names in it are the three that were current when it
// was written; the browser tier has since grown `skein-ui-`, `skein-attach-`, `skein-review-ui`,
// `skein-hatches-ui`, `skein-connections-ui-`, `skein-actfail-ui-` and `ui-updatepane-`, and the
// Rust tier names better than thirty prefixes at its `Scratch::boxes`/`Scratch::temp` call sites.
// A hand-written alternation names the fixtures of the day and goes stale the first time one is
// added — silently, because a pattern that has never matched anything is indistinguishable from a
// pattern that matches nothing. So [`fixturePrefixes`] READS the names out of the files that
// create the fixtures, and the check refuses to run at all if it derives none.
//
// **That fixed the pattern and not the surface it was tried against, and the same failure came
// back through the gap** (SKEIN-687). The scan read `/proc/<pid>/cmdline` and nothing else, so it
// could only see a fixture name that appears in a process's ARGUMENTS — while the process these
// suites leave behind most is the `skein-server` under test, exec'd as a bare binary path and told
// which fixture it belongs to in `SKEIN_HOME` and `SKEIN_FLEET_ROOT`. The check answered "nothing
// is running" on a box where one had been up for seven and a half hours. [`processes`] reads the
// environment as well now, [`sighting`] is how a caller asks about it without the text ever coming
// back, and a process whose environment cannot be read is counted and said out loud rather than
// quietly scored as a miss.
//
// **And then a third time, through the gap the SECOND fix left** (SKEIN-861, SKEIN-873). Reading
// both surfaces is no help against a process that carries the name on neither. A namespace
// fixture's stand-in agent `exec`s `sleep 600`: after the exec its arguments are two words, and the
// fixture root was interpolated into the `bash -c` script rather than exported, so it was never in
// the environment to be found. Measured three times in one day — six at `ppid=1` under one
// worktree, seven more from seven `attach.mjs` runs, the oldest nine minutes old — and on one of
// those runs this file was wrong in both directions at once: exit 1 over four of ANOTHER lane's
// live processes, and silence about the reporter's own orphans.
//
// A derived name cannot fix that, because the missing surface is not a name. What such a process
// does still carry is [`testMarker`] — the variable `util::in_test` branches on, which
// `.cargo/config.toml`'s `[env]` table puts on every process `cargo test` runs and which `exec`
// does not clear. So [`main`] asks a second question beside the first, and BOTH are kept: a prefix
// catches a process that names a fixture and carries no marker, and the marker catches one that has
// shed every name it ever had.
//
// **The two questions differ; the verdict is one, and [`attribute`] is the single function that
// reaches it** (SKEIN-913). A process is this run's leak when it is attributable to THIS worktree —
// [`fromWorktree`], derived from this file's own path rather than listed — and its parent is gone.
// Everything else either half finds is printed, with its age and the surface the name was seen on,
// and does not touch the exit code. Until SKEIN-913 only the marker half had those two rules: the
// prefix half reported a match anywhere in argv or environment and failed on it, so a concurrent
// lane's `cargo build` under a path containing a fixture name exited 1 — five rows of live `rustc`,
// every one of them nought seconds old with a live parent, all five gone by the time they were
// looked at — in the same output in which the marker half was saying, in those words, that nothing
// there was this run's to be red about. **A check that goes red for a reason the reader can see is
// not theirs is SKEIN-647 from the other side**: it teaches people to read past it, and the next
// red is read past too. It fired on a tree where a real leak had just appeared (SKEIN-912), and the
// two were indistinguishable in one output until each was chased by hand.
//
// What did NOT change is what the prefix half REACHES. It exists to see a process that names a
// fixture and carries no marker — the shape the marker half structurally cannot see — and narrowing
// the scan to fix the verdict would have traded this defect for SKEIN-687's.
//
// **And the same red came back through that half a fifth time** (SKEIN-979). Attribution was not
// the gap this time; the NEEDLE was. `smoke.mjs` names its fixture with a bare prefix, `skein-ui`,
// and the shared root every lane on this box writes into is `/var/tmp/skein-uifix` — or
// `/var/tmp/skein-uif-<lane>` under the preamble each agent exports — so the root itself matched
// the prefix derived from what is created INSIDE it. Every process of every lane that exported that
// variable read as "names a test fixture": `cargo`, `rustc`, a running `bash tools/gates.sh`. And a
// backgrounded gate run whose parent shell has exited is attributable to this worktree with its
// parent gone, which is exactly what this file calls a leak — so a LIVE gate run could take the
// check red under a prefix it had nothing to do with. [`withoutSharedRoots`] is the answer, and it
// is [`withoutWorktree`]'s argument applied to the other container: a process that names where
// fixtures go is not a fixture process, a process that names something INSIDE that root is, and the
// root is subtracted only where it stands alone so that the second one is never touched.
//
// **And a sixth time, over a process that was doing nothing wrong at all** (SKEIN-990). Both halves
// went red, seconds apart, over `tmux -S /tmp/<fixture>/fleet/.skein/private/server.tmux
// new-session -d -s skein-server …` — one second old, started by a suite in ANOTHER worktree that
// was running at that moment, in a tree where no suite was running at all. Two rules were wrong at
// once, and each one alone would have produced the red:
//
//   * **`ppid == 1` is not "its run has gone" for a process that was daemonised on purpose.**
//     `tmux new-session -d` forks a server and the launching process returns, so a fixture's tmux
//     is parentless in its FIRST second and for the whole of its healthy life. An age threshold
//     cannot separate that from a leak either, because a leak is one second old in its first
//     second. What separates them is whether anything else of that fixture is still running under
//     a live parent — [`fixturesRunning`], measured on the cohort rather than on the row;
//   * **the environment held this worktree's path, and it was a breadcrumb.** The only mention of
//     it in that tmux was `OLDPWD=/boxes/…/tree`, left by the `cd` the other lane's agent made on
//     its way INTO its own worktree; the lane that owned the process was named by `PWD` and by
//     nothing stronger (that environment carries no `$CARGO_MANIFEST_DIR` at all — measured, see
//     `tests/ui/leakcheck.mjs`). `fromWorktree` asked "is this path anywhere in the environment",
//     and to a breadcrumb the answer is yes. [`whose`] asks a question with an answer instead: a
//     process that names another checkout of this repository as well as this one belongs to the
//     one it is STANDING in, and to neither when it stands in neither. The other checkouts come
//     from `git worktree list` — git's own record of every lane on this box, derived like
//     everything else here, never a list of paths in this file.
//
// **And a seventh time, over the one surface nobody had pointed a reader at** (SKEIN-1006). The
// derivation read `tests/` and nothing else, so `skein-test-` — the name of the fixture directory
// of every `#[cfg(test)]` test in `src/`, built by `crate::testutil::tempdir()` — was not a derived
// prefix at all, and the check could not have printed it on any run it has ever made. A unit test's
// stranded process carrying `SKEIN_HOME=/tmp/skein-test-<pid>-<seq>` and nothing else was therefore
// fixture-named to nothing: one witness instead of two, no cohort, so [`fixturesRunning`] could not
// reach it and every parentless one was red whether its run was still going or not. The fix is a
// third reader rather than a fourth directory, because the library names its fixture in the HELPER
// and not at the call site — see [`libraryPrefixes`], and [`SOURCES`] for why that is the opposite
// of the rule the other two tiers follow.
//
// The other half is [`quiesceOnExit`], and it is `tests/common/mod.rs`'s `Scratch` argument
// transplanted: *whatever has to stop, stops on every path; only the removal is conditional*. The
// node tier had no equivalent — `srv.kill()` sat at the top level of each suite, after the last
// check, so a throw or a Ctrl-C skipped it, and it never covered the tmux server anyway, because
// that server is not `skein-server`'s child to take with it.
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, readlinkSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import os from "node:os";

const SELF = fileURLToPath(import.meta.url);
const REPO = resolve(dirname(SELF), "..", "..", "..");

// ---------------------------------------------------------------------------------------------
// reading /proc
// ---------------------------------------------------------------------------------------------

/** Every process on this machine, as `{pid, args, age, env, envVars, envState}` — argv and the
 * environment NUL-joined back into a line, the environment again as its `NAME=value` entries, and
 * age in whole seconds.
 *
 * **argv was the whole of what this file looked at, and that is the second way the check could not
 * fail** (SKEIN-687). It reported "nothing is running" on a box carrying a `skein-server` a suite
 * had started seven and a half hours earlier, and it was telling the truth about argv: the server
 * is exec'd as a bare binary path with no arguments, and its fixture identity arrives in
 * `SKEIN_HOME` and `SKEIN_FLEET_ROOT`, which are ENVIRONMENT variables. So the process shape these
 * suites create most of — the server under test — was the one shape neither the report below nor
 * [`stopRun`] could see. `/proc/<pid>/environ` is NUL-separated and reads exactly like `cmdline`.
 *
 * **`env` is here to be matched against and is never printed.** It carries `$GH_TOKEN`, the
 * fixture's API token and whatever else the person running the suite has exported. [`sighting`] is
 * the way to ask about it: it answers *where* a name was seen rather than handing the text back,
 * and [`main`] builds its report out of `pid`, `age`, `args` and the derived prefix that matched.
 *
 * Synchronous on purpose: [`quiesceOnExit`] calls it from a `process.on("exit")` handler, where
 * node runs nothing asynchronous, and a teardown that only works on the paths that can await is
 * the teardown this file exists to replace.
 *
 * A kernel thread has an empty `cmdline` and is skipped; a process that exits between the
 * `readdir` and the `read` is skipped rather than thrown over, because the scan races every other
 * process on the box by construction. */
export function processes() {
  const now = uptime();
  const out = [];
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    let raw;
    try {
      raw = readFileSync(`/proc/${name}/cmdline`);
    } catch {
      continue;
    }
    if (!raw.length) continue;
    const pid = Number(name);
    out.push({ pid, args: nulJoined(raw), age: ageOf(pid, now), ...environOf(pid) });
  }
  return out;
}

/** `cmdline` and `environ` are both NUL-separated lists with a trailing NUL. */
function nulJoined(raw) {
  return raw.toString("utf8").replace(/\0+$/, "").split("\0").join(" ");
}

/** The same bytes as [`nulJoined`], as the list of `NAME=value` entries they actually are.
 *
 * **Both forms are kept because the two questions this file asks need different surfaces.** A
 * fixture PATH is looked for anywhere in the environment, and the joined string is right for that:
 * one `test` per process rather than one per variable. A VARIABLE NAME is a different question, and
 * the joined string cannot answer it — entries are joined with a space and a value may contain
 * spaces of its own (`SKEIN_LS_CMD` is `<dir>/sbx ls --json`, `PATH` is half the machine), so
 * `/(?:^| )NAME=/` over it would also match a `NAME=` sitting inside somebody else's value.
 * Splitting first makes the boundary the real one.
 *
 * Printed by nothing, for [`processes`]'s reason: this is the environment. */
function nulList(raw) {
  return raw.toString("utf8").replace(/\0+$/, "").split("\0").filter(Boolean);
}

/** This pid's environment, as `{env, envVars, envState}` — and **a read that fails is an answer of
 * its own**,
 * which is why the state is a word and not an empty string.
 *
 * `/proc/<pid>/environ` opens only for a process this one could inspect: in practice its own, and
 * `EACCES` for anything else — another user's daemon, and pid 1 here, which reports this user as
 * its owner and refuses the read anyway. `ENOENT`/`ESRCH` are the different fact that it exited
 * between the `readdir` and this read.
 *
 * Folding those two together into "no match" would be this file's own bug written a second time: a
 * check that could not look must say so rather than report a clean nothing. So [`main`] counts the
 * denied ones into its verdict, and a process that has exited needs no mention — it is not running,
 * which is the whole question.
 *
 * A zombie has no environment and reads as an empty one. That is an answer, not an error. */
export function environOf(pid) {
  try {
    const raw = readFileSync(`/proc/${pid}/environ`);
    return { env: nulJoined(raw), envVars: nulList(raw), envState: "read" };
  } catch (e) {
    const denied = e.code === "EACCES" || e.code === "EPERM";
    return { env: "", envVars: [], envState: denied ? "denied" : "gone" };
  }
}

/** Where `needle` — a string, or a RegExp — appears in a process from [`processes`]: `"argv"`,
 * `"environment"`, or `null` for neither.
 *
 * **The only door to a process's environment, and it never returns any of it.** A caller learns
 * which surface the name was seen on; what it may print is `args`, which was always printable, and
 * whichever needle it asked with.
 *
 * A denied environment answers `null` here, which is correct for a single question and is exactly
 * why the count of them is reported separately: no one process is a leak, and "none of them are"
 * is not something this function got to establish. */
export function sighting(p, needle) {
  const seen = typeof needle === "string" ? s => s.includes(needle) : s => needle.test(s);
  if (seen(p.args)) return "argv";
  if (p.envState === "read" && seen(p.env)) return "environment";
  return null;
}

/** Seconds since boot, from `/proc/uptime` — the clock `starttime` below is measured against. */
function uptime() {
  try {
    return Number(readFileSync("/proc/uptime", "utf8").split(" ")[0]);
  } catch {
    return 0;
  }
}

/** How long this pid has been alive, in whole seconds, or `null` when it cannot be read.
 *
 * `starttime` is field 22 of `/proc/<pid>/stat` in clock ticks, taken after the LAST `)` because a
 * process's `comm` can itself contain parens — the same read, and the same reason, as
 * `place::anchor_probe` and `boxlikeNamespace` in `lift.mjs`. 100 ticks per second is `USER_HZ`,
 * which is 100 on every platform this repository runs on; being wrong about it would misreport an
 * age, never miss a process. */
function ageOf(pid, now) {
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    const fields = stat.slice(stat.lastIndexOf(") ") + 2).trim().split(/\s+/);
    return Math.max(0, Math.round(now - Number(fields[19]) / 100));
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------------------------
// stopping this run's processes
// ---------------------------------------------------------------------------------------------

/** The absolute paths this run owns, read out of the environment a suite hands [`startServer`].
 *
 * The point of scoping a kill this way is that these paths came from `mkdtemp`: they name one run
 * and cannot name another agent's. A `pkill -f tmux` on this box once reaped 72 servers whose
 * owners could not afterwards be named, and every one of those was somebody's work.
 *
 * The rules, in the order they matter:
 *
 * - one absolute path: no whitespace and no `:`, so `SKEIN_LS_CMD` (`<dir>/sbx ls --json`),
 *   `SKEIN_WARDEN` (`127.0.0.1:<port>`) and above all `PATH` (`<fixture>/bin:/usr/bin:…`, which
 *   begins with a fixture path and goes on to name half the machine) are not paths and do not
 *   enter;
 * - under a temporary root, so a suite pointed at a real `$SKEIN_HOME` contributes nothing;
 * - **at least two segments below that root**, which is the rule that keeps
 *   `/var/tmp/skein-uifix` — the shared fixture root every worktree on the box writes into — out
 *   of the scope. Killing by that string would kill every other agent's run, which is the accident
 *   this function is shaped to make impossible rather than merely unlikely.
 *
 * The run's own fixture root is then added back as the **common ancestor of at least two** accepted
 * paths (`<fixture>/fleet` and `<fixture>/home` give `<fixture>`). It arrives that way rather than
 * as `dirname` of one path because a common ancestor of two siblings is a directory this run
 * certainly created, whereas `dirname` of a single path is one level up from wherever it happened
 * to point — which for a fixture directly under `/var/tmp/skein-uifix` is the shared root again. */
export function fixtureScopes(env = {}) {
  const roots = [...new Set(["/tmp", "/var/tmp", os.tmpdir(), process.env.TMPDIR].filter(Boolean))]
    .map(r => r.replace(/\/+$/, ""));
  const accepted = [];
  for (const value of Object.values(env)) {
    if (typeof value !== "string" || !value.startsWith("/") || /[\s:]/.test(value)) continue;
    const path = value.replace(/\/+$/, "");
    const root = roots.find(r => path.startsWith(`${r}/`));
    if (!root) continue;
    if (path.slice(root.length + 1).split("/").filter(Boolean).length < 2) continue;
    accepted.push(path);
  }
  const scopes = new Set(accepted);
  for (const path of accepted) {
    const parent = path.slice(0, path.lastIndexOf("/"));
    if (accepted.filter(p => p === parent || p.startsWith(`${parent}/`)).length >= 2) {
      scopes.add(parent);
    }
  }
  return [...scopes].sort();
}

/** Processes whose argv **or environment** names one of `scopes`, excluding this process and its
 * ancestors.
 *
 * The environment half is not only the report's problem (SKEIN-687). The process a run most needs
 * to stop is its `skein-server`, and that one is exec'd as a bare binary path with its fixture in
 * `SKEIN_HOME` and `SKEIN_FLEET_ROOT` — so an argv-only scan handed [`stopRun`] a list that could
 * not contain it, and what stopped it was the `srv.kill()` a suite passes as [`quiesceOnExit`]'s
 * `also`. That is one callback away from being nothing at all, which is the shape this file was
 * written to stop trusting.
 *
 * Widening the surface does not widen the blast radius, because a scope is still what
 * [`fixtureScopes`] made it: a `mkdtemp` path this run created. A process carrying one in its
 * environment inherited it from this run's server — the tmux server, the doorway loop, the python
 * it respawns — and cannot be another agent's.
 *
 * The ancestor exclusion is belt and braces — a suite's own argv is `node smoke.mjs`, which names
 * no fixture, and no suite puts its fixture paths in its own `process.env` — but a suite invoked
 * with its fixture root as an argument would otherwise ask this function to kill the process
 * asking. */
export function running(scopes) {
  if (!scopes.length) return [];
  const mine = new Set(ancestry());
  return processes().filter(p => !mine.has(p.pid) && scopes.some(s => sighting(p, s)));
}

/** This process and every parent up to pid 1. */
function ancestry() {
  const chain = [];
  let pid = process.pid;
  for (let i = 0; i < 64 && pid > 1; i++) {
    chain.push(pid);
    try {
      const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
      pid = Number(stat.slice(stat.lastIndexOf(") ") + 2).trim().split(/\s+/)[1]);
    } catch {
      break;
    }
    if (!Number.isInteger(pid) || pid <= 0) break;
  }
  return chain;
}

/** Stop everything running under `scopes`. Returns the pids it stopped.
 *
 * SIGTERM, then SIGKILL for whatever is still there — and then one more look, because the shape
 * being cleaned up restarts itself: `fleet::supervised` writes `while [ -f <script> ]; do … done`,
 * so between the two signals the loop can have spawned a fresh doorway. Two rounds settle it; a
 * third would only be reached if something outside these scopes were respawning into them, which
 * nothing does.
 *
 * Synchronous, including the wait — see [`processes`]. */
export function stopRun(scopes) {
  const stopped = new Set();
  for (let round = 0; round < 2; round++) {
    const found = running(scopes);
    if (!found.length) break;
    for (const { pid } of found) send(pid, "SIGTERM", stopped);
    pause(300);
    for (const { pid } of running(scopes)) send(pid, "SIGKILL", stopped);
    pause(100);
  }
  return [...stopped];
}

function send(pid, signal, stopped) {
  try {
    process.kill(pid, signal);
    stopped.add(pid);
  } catch {
    /* already gone, or not ours — either way there is nothing to do about it */
  }
}

/** A blocking wait, because a `process.on("exit")` handler never reaches a timer. */
function pause(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/** Run `stopRun(scopes)` on **every** way out of this process, and `also()` first.
 *
 * The ways out, and why each one is named rather than trusting `exit` to cover them:
 *
 * - normal return and `process.exit()` — `exit`, which is why the whole path below is synchronous;
 * - an uncaught throw or rejection — node's default handler prints and exits, and `exit` does fire,
 *   but a handler installed here must then reproduce that printing or a suite's crash becomes a
 *   silent exit code. It prints the stack to stderr exactly as node would;
 * - **SIGINT and SIGTERM** — node's default for these is to die without running `exit` handlers at
 *   all, so an interrupted run left everything behind. This is the path a person actually takes
 *   when a suite hangs, and it was the one that leaked most.
 *
 * Registered `once` per signal, and the handler re-raises after cleaning up so the exit status
 * still says "killed by a signal" rather than a number this file invented.
 *
 * **The fixture directory is not touched.** A suite keeps its fixture on failure deliberately — it
 * is the only evidence a failure leaves (SKEIN-590) — and that is exactly why the processes have to
 * go: `fleet::supervised`'s loop stops when its script is gone, so a fixture kept for inspection is
 * a doorway loop restarting a python every two seconds until the machine reboots. Four tmux
 * servers between two and nine hours old were measured that way (SKEIN-645). Same division as
 * `Scratch` in `tests/common/mod.rs`: quiesce always, remove conditionally.
 *
 * **Callable once per server, and it still installs one set of handlers.** `hatches.mjs` starts a
 * server per hatch it closes; a listener per call would earn a MaxListenersExceededWarning in the
 * middle of a suite's output, and a `once("SIGINT")` per call would each re-raise the signal. So
 * scopes and callbacks accumulate into the module and the handlers go on at the first call. */
const runScopes = new Set();
const callbacks = [];
let installed = false;
let quiesced = false;

export function quiesceOnExit(theirs, also = () => {}) {
  for (const scope of theirs) runScopes.add(scope);
  callbacks.push(also);
  if (installed) return quiesce;
  installed = true;
  process.on("exit", quiesce);
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
    process.once(signal, () => {
      quiesce();
      // Re-raised rather than exited, so the status still says "killed by a signal" instead of a
      // number this file invented — `browser_suites.rs` reads that status.
      process.kill(process.pid, signal);
    });
  }
  for (const event of ["uncaughtException", "unhandledRejection"]) {
    process.once(event, err => {
      console.error(err instanceof Error ? err.stack : String(err));
      quiesce();
      process.exit(1);
    });
  }
  return quiesce;
}

/** Everything registered above, run once however many ways out arrive at it. */
export function quiesce() {
  if (quiesced) return [];
  quiesced = true;
  for (const also of callbacks) {
    try {
      also();
    } catch {
      /* the suite is on its way out; a failed kill must not hide the reason it is leaving */
    }
  }
  return stopRun([...runScopes]);
}

// ---------------------------------------------------------------------------------------------
// the check: names read out of the code that creates the fixtures
// ---------------------------------------------------------------------------------------------

/** Where a fixture prefix is written down, how to read it back, which language's comments to cut
 * out of it first, and the shape the refusal below names when a tier yields nothing.
 *
 * `tests/common/mod.rs` is deliberately absent from the `tests` tier: it holds
 * `Scratch::at(root, prefix)`, the implementation, whose `prefix` is a variable. Only call sites
 * name a fixture there.
 *
 * **`src/` is the opposite case, and it is why there is a third reader rather than a third
 * directory on the first one** (SKEIN-1006). Nothing in the library names a fixture at a call site:
 * `crate::testutil::tempdir()` takes no arguments and the name is a literal in its own body, so the
 * rule that governs the two tiers above — read the call sites, never the helper — would derive
 * nothing here however many directories it was pointed at. What is read instead is the helper, the
 * way [`testMarker`] reads `util::TEST_MARKER` and [`sharedFixtureRoot`] reads `lift.mjs`'s
 * `fixtureRoot`: the one place that decides the name.
 *
 * `shape` rides on the entry rather than living in a conditional inside the refusal, so that a
 * tier cannot be added without saying what was renamed when it goes quiet — which is the whole of
 * what that message is for. */
const SOURCES = [
  { dir: "tests", ext: ".rs", tier: "rust", lang: "rust", read: rustPrefixes,
    shape: '`Scratch::boxes("…")` / `Scratch::temp("…")`' },
  { dir: "src", ext: ".rs", tier: "library", lang: "rust", read: libraryPrefixes,
    shape: '`fn tempdir()` building `env::temp_dir().join(format!("…"))`' },
  { dir: "tests/ui", ext: ".mjs", tier: "node", lang: "js", read: nodePrefixes,
    shape: '`mkdtempSync(…, "…")` / `freshFixture(…, "…")`' },
  { dir: "tests/ui/harness", ext: ".mjs", tier: "node", lang: "js", read: nodePrefixes,
    shape: '`mkdtempSync(…, "…")` / `freshFixture(…, "…")`' },
];

/** `Scratch::boxes("skein-move-it")` and `Scratch::boxes(&format!("skein-fleet-it-{what}"))`.
 *
 * The literal is cut at the first `{`, so an interpolated tail contributes the fixed head — which
 * is what the directory name starts with and therefore all the check needs. */
function rustPrefixes(text) {
  return [...text.matchAll(/Scratch::(?:boxes|temp)\(\s*(?:&format!\(\s*)?"([^"]+)"/g)]
    .map(m => m[1].split("{")[0])
    .filter(Boolean);
}

/** The library's own scratch helper: `fn tempdir()` returning
 * `env::temp_dir().join(format!("skein-test-{}-{}", …))`, cut at the first `{` for
 * [`rustPrefixes`]'s reason.
 *
 * **Every unit test in the crate was invisible to the prefix half, and it is the surface again**
 * (SKEIN-1006, after SKEIN-687 and SKEIN-861). The derivation read `tests/` and nothing else, so
 * `skein-test-` — the name of the scratch directory of every unit test in `src/` that asks for one,
 * 307 call sites across 43 files (`grep -rno "testutil::tempdir()" src/ | wc -l`) — was not a
 * derived prefix at all. It could not have been printed on any run this check has ever made, and a
 * process whose only fixture path was `SKEIN_HOME=/tmp/skein-test-<pid>-<seq>` was therefore
 * fixture-named to nothing: its one witness was `$SKEIN_TEST`, it could be in no cohort, so
 * [`fixturesRunning`] — SKEIN-990's rule — could not reach it and it was judged on parentage
 * alone, and the row a reader was handed said `environment SKEIN_TEST` and named no fixture.
 * Measured on this branch before the change: `fixturePrefixes()` held 61 prefixes from 75 files
 * and none of them began `skein-test`.
 *
 * **It reads one helper and not every `temp_dir().join(format!(…))` in `src/`, and the difference
 * is measured rather than tidy.** Nine other sites build a scratch directory by hand there, and two
 * of them are not fixtures at all (`skein-req-`, `skein-hdr-` in `github.rs` are production request
 * bodies, which a live GitHub call really does create). One is worse than not a fixture:
 * `src/bin/skein-server.rs`'s `home_for` interpolates at the HEAD of its name, so its literal cut
 * at the first `{` is the bare stem every other prefix in this repository begins with — and
 * [`fixtureRegex`] built on that stem matches the checkout path of this repository itself, on every
 * process that names it. A reader that swept the directory would have derived it the day it was
 * pointed at `src/bin/`. So this one reads the sanctioned helper — the one
 * `testutil::sweep_stale_runs` also knows by name, and the only one whose leftovers anything cleans
 * up — and the hand-rolled nine are a separate defect, written down as SKEIN-1007 rather than
 * widened into here. */
function libraryPrefixes(text) {
  return [...text.matchAll(
    /fn\s+tempdir\s*\(\s*\)[^{]*\{[^}]*temp_dir\(\)\s*\.join\(\s*format!\(\s*"([^"]+)"/g)]
    .map(m => m[1].split("{")[0])
    .filter(Boolean);
}

/** `fs.mkdtempSync(path.join(os.tmpdir(), "skein-ui-"))` and `freshFixture(fixtureRoot(), "ui-onboard")`.
 *
 * Line by line, and only double-quoted literals: `mkdtempSync(join(dir, `${prefix}-…`))` inside
 * `freshFixture` itself has no literal on its line and contributes nothing, which is correct — the
 * name is at the call site. */
function nodePrefixes(text) {
  const found = [];
  for (const line of text.split("\n")) {
    for (const re of [/mkdtempSync\(.*?"([^"]+)"/, /freshFixture\(.*?,\s*"([^"]+)"/]) {
      const m = line.match(re);
      if (m) found.push(m[1]);
    }
  }
  return found;
}

/** `b?r#*"`, matched AT a position rather than searched for, so an `r"` inside another literal is
 * not mistaken for the start of one. */
const RAW_STRING = /b?r(#*)"/y;
/** `'x'`, `'\n'`, `'\u{1f600}'` — and deliberately NOT `'a`, because a lifetime has no closing
 * quote and the caller must step over that one character rather than hunt for a close that is not
 * there. Spelt out rather than `'..'` for the same reason it is in `tools/rustcut.py`: a char
 * literal can legitimately contain the delimiter it is being told apart from. */
const CHAR_LITERAL = /'(?:\\u\{[0-9a-fA-F_]+\}|\\.|[^\\'])'/sy;

/** If a comment or a literal begins at `text[i]`, `{end, comment}` — the index just past it, and
 * which of the two it was. `null` for anything else, which the caller steps over one character at
 * a time.
 *
 * **The Rust half is `tools/rustcut.py::skip_token` transliterated**, and for its reasons: raw
 * strings are matched before the ordinary string branch so that a `//` inside one does not open a
 * comment, block comments nest, and a lifetime is not a token. `tests/platform_gates.rs::token_end`
 * is the same rule a third time; the language boundary is why there are three and not one, the same
 * bargain those two already make with each other across the crate boundary.
 *
 * **The JS half differs in three ways, and each one is a hazard this tree really holds:**
 *
 *   - **an unterminated `'` or `"` on a line is not a string**, because in JS it cannot be one.
 *     `leakcheck.mjs` matches with `/^ {2}(\d+) of them are this worktree's and their parent is
 *     gone/gm` — an apostrophe in code position. Read as a string it would run to the next
 *     apostrophe lines below and swallow everything between, real call sites included. That is the
 *     false NEGATIVE this cutter must not trade the false positive for, and it is why regexp
 *     literals need not be a token here: the thing they carry that hurts is the stray quote, and
 *     the line bounds it;
 *   - a template literal DOES span lines, so it is exempt from that rule. `${...}` is not looked
 *     into — a template nested in one ends the outer early here, which leaves the rest of the line
 *     reading as code, the safe direction;
 *   - block comments do not nest. */
function skipToken(text, i, lang) {
  const n = text.length;
  const rust = lang === "rust";
  if (text.startsWith("//", i)) {
    const end = text.indexOf("\n", i);
    return { end: end < 0 ? n : end, comment: true };
  }
  if (text.startsWith("/*", i)) {
    let depth = 1;
    let j = i + 2;
    while (j < n && depth > 0) {
      if (rust && text.startsWith("/*", j)) {
        depth += 1;
        j += 2;
      } else if (text.startsWith("*/", j)) {
        depth -= 1;
        j += 2;
      } else {
        j += 1;
      }
    }
    return { end: j, comment: true };
  }
  const boundary = i === 0 || !/[A-Za-z0-9_]/.test(text[i - 1]);
  if (rust) {
    RAW_STRING.lastIndex = i;
    const raw = boundary ? RAW_STRING.exec(text) : null;
    if (raw) {
      const close = `"${raw[1]}`;
      const end = text.indexOf(close, RAW_STRING.lastIndex);
      return { end: end < 0 ? n : end + close.length, comment: false };
    }
    if (text[i] === "'") {
      CHAR_LITERAL.lastIndex = i;
      return CHAR_LITERAL.exec(text) ? { end: CHAR_LITERAL.lastIndex, comment: false } : null;
    }
  }
  // `b"…"` is one token, so the scan for the close starts past the `b` — the same offset
  // `skip_token` takes, and the reason the byte-string branch is not simply the string branch.
  const byte = rust && boundary && text[i] === "b" && text[i + 1] === '"';
  const quote = byte ? '"' : text[i];
  if (!(rust ? ['"'] : ['"', "'", "`"]).includes(quote)) return null;
  const spansLines = rust || quote === "`";
  let j = i + (byte ? 2 : 1);
  while (j < n) {
    if (text[j] === "\\") {
      j += 2;
      continue;
    }
    if (text[j] === quote) return { end: j + 1, comment: false };
    if (!spansLines && text[j] === "\n") return null;
    j += 1;
  }
  return spansLines ? { end: n, comment: false } : null;
}

/** `text` with every comment cut and every literal kept, read as `lang` — `"rust"` or `"js"`.
 *
 * **Prose that QUOTES a call site was read as one** (SKEIN-917). The two readers above run over
 * every file in the test tree, and two doc comments in that tree explain this very check by quoting
 * the shapes it hunts for. So the literal ellipsis in them was a derived fixture prefix,
 * [`fixtureRegex`] turned it into `/…[A-Za-z0-9._-]*`, and it stood at the head of every run's
 * printed list — a name no fixture has ever had, in the one line whose whole job is to say what the
 * check was looking for.
 *
 * **The noise is not the defect; the GUARD is** (SKEIN-882). A tier's entire contribution can come
 * out of a comment, so renaming every real call site in a tier would leave the refusal below green
 * and the check would go back to being a machine for printing zero with nothing in its output to
 * say so — SKEIN-647's shape, hiding inside the thing bought to prevent it.
 *
 * **Not a list of files to skip.** Those two comments are only today's two, they will be reworded
 * and others will be written, and a hand-maintained exclusion list is the thing this file exists to
 * not have.
 *
 * **Literals are KEPT, and that is not an oversight** — the same bargain `code_only` makes in
 * `tests/platform_gates.rs`, where SKEIN-908 bought this lesson in Rust when a module doc that
 * merely mentioned `bwrap_works()` made a gate demand `bwrap`: a mention is not an instance, and a
 * cutter that dropped literals would blind the reader it is here to sharpen, because the fixture
 * name at a call site IS a string literal. What follows from keeping them is that a shape quoted
 * inside a STRING still counts, which is why [`fixturePrefixes`] goes on skipping this file by
 * identity: its own refusal message quotes both shapes in one.
 *
 * A comment becomes the newlines it spanned, so [`nodePrefixes`], which reads line by line, still
 * sees the lines it did. */
export function codeOnly(text, lang) {
  const out = [];
  let kept = 0;
  let i = 0;
  while (i < text.length) {
    const token = skipToken(text, i, lang);
    if (!token || token.end <= i) {
      i += 1;
      continue;
    }
    if (token.comment) {
      out.push(text.slice(kept, i), "\n".repeat(text.slice(i, token.end).split("\n").length - 1));
      kept = token.end;
    }
    i = token.end;
  }
  out.push(text.slice(kept));
  return out.join("");
}

/** Every fixture prefix this repository can produce, read from the files that produce them.
 *
 * Returns `{prefixes, tiers, files, quoted}`. **It throws when a tier contributes nothing**, and
 * that is the guard the old check lacked: `0` from a pattern is only meaningful if the pattern was
 * built from something. A rename that this reader stops recognising fails loudly here instead of
 * turning the gate into a machine for printing zero.
 *
 * **Every count here is off the CODE**, with [`codeOnly`] run first, so a doc comment quoting a
 * call site contributes neither a prefix nor a tier (SKEIN-917, SKEIN-882). `quoted` is what that
 * cut threw away — shapes this tree mentions only in prose — and it is returned rather than
 * dropped in silence for the reason the prefixes themselves are printed: a reader must be able to
 * tell a cut that fired from a cut that never had anything to remove. [`main`] prints it when it is
 * not empty. */
export function fixturePrefixes(repo = REPO) {
  const prefixes = new Set();
  const mentioned = new Set();
  const tiers = {};
  let files = 0;
  for (const source of SOURCES) {
    let names;
    try {
      names = readdirSync(join(repo, source.dir));
    } catch {
      throw new Error(`the leak check cannot read ${source.dir}/ — it is looking in ${repo}`);
    }
    for (const name of names) {
      if (!name.endsWith(source.ext)) continue;
      // This file is a reader, not a call site: the shapes below are quoted in its own doc comments
      // and its own error message, and it read `…` back out of them as a fixture prefix the first
      // time it ran. Skipped by identity rather than by name, so renaming the file keeps it out.
      if (join(repo, source.dir, name) === SELF) continue;
      let text;
      try {
        text = readFileSync(join(repo, source.dir, name), "utf8");
      } catch {
        continue;
      }
      files++;
      for (const prefix of source.read(codeOnly(text, source.lang))) {
        prefixes.add(prefix);
        tiers[source.tier] = (tiers[source.tier] || 0) + 1;
      }
      // The same file read UNCUT, which is the only way to say what the cut removed. It reaches no
      // count and no pattern — `quoted` below is the difference, and it is output, not input.
      for (const prefix of source.read(text)) mentioned.add(prefix);
    }
  }
  for (const source of SOURCES) {
    if (!tiers[source.tier]) {
      // Every directory of the tier and not just this entry's, because the node tier has two and a
      // message naming one of them sends its reader to look in half the places the count came
      // from — the same "said less than it looked at" error the denied line in [`main`] exists to
      // not make.
      const where = SOURCES.filter(s => s.tier === source.tier).map(s => `${s.dir}/`).join(", ");
      throw new Error(
        `the leak check derived no fixture prefix from the ${source.tier} tier, so a count from it \
would mean nothing. Either the call sites moved out of ${where}, or the shape ${source.shape} was \
renamed — fix this reader, do not widen it by hand.`);
    }
  }
  return {
    prefixes: [...prefixes].sort(),
    tiers,
    files,
    quoted: [...mentioned].filter(p => !prefixes.has(p)).sort(),
  };
}

/** One regexp over the derived prefixes: a path separator, the prefix, and the rest of that
 * directory's name. The leading `/` is what stops a prefix from matching a bare word somewhere in
 * an unrelated command line. */
export function fixtureRegex(prefixes) {
  const alt = prefixes.map(p => p.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&")).join("|");
  return new RegExp(`/(?:${alt})[A-Za-z0-9._-]*(?:/|\\s|$)`);
}

// ---------------------------------------------------------------------------------------------
// the check: no suite builds its fixture from os.tmpdir()
// ---------------------------------------------------------------------------------------------
//
// **This is the check the SKEIN-653 bug wanted and did not have.** `smoke.mjs`, `attach.mjs`,
// `connections.mjs`, `actfail.mjs` and `updatepane.mjs` built their fixture with
// `fs.mkdtempSync(path.join(os.tmpdir(), "<prefix>-"))` instead of `freshFixture(fixtureRoot(),
// "<prefix>")`, and both halves of `fixtureRoot`'s own reasoning applied to every one of them: a box
// a suite launches binds its own directories over `/tmp` (`src/box-session.sh` refuses a fleet root
// beneath it), and a `mkdtemp` name carries no pid, so nothing the pid-sweep in `freshFixture` does
// ever reasons about it — 29 such directories were measured under `/tmp`, the oldest 36 hours.
//
// [`serverSuites`] derives WHICH files this applies to, the same way [`fixturePrefixes`] derives
// prefixes rather than naming them: a suite is anything under `tests/ui/` that imports `startServer`
// from `./harness/server.mjs`, because that import is what turns a fixture directory into something
// a box has to read, and it throws when it derives none, for the reason every derive-and-refuse
// function in this file throws on that — a check that scans zero suites reports "clean" forever,
// indistinguishably from a run that actually looked (SKEIN-647, SKEIN-687, SKEIN-913).
//
// **Not every file that calls `os.tmpdir()` under `tests/ui/` is this bug**, which is why the check
// is scoped to [`serverSuites`] rather than to the whole directory. `leakcheck.mjs` calls it twice —
// once for a planted orphan's `cwd`, once for a skeleton directory its OWN tests build to exercise
// [`testMarker`] — and neither is a fixture a box ever reads: that file imports no `startServer` and
// deliberately keeps its skeleton off `fixtureRoot()`, in its own words, "so that the disagreement is
// a real one this suite made and not a state the tree has to be put into." Widening the scope to
// match text anywhere in the directory would turn that deliberate choice into a false positive, which
// is its own version of the SKEIN-647 shape: a check that fires on the wrong thing teaches a reader to
// read past it.
export function serverSuites(repo = REPO) {
  let names;
  try {
    names = readdirSync(join(repo, "tests", "ui"));
  } catch {
    throw new Error(`the fixture-root check cannot read tests/ui/ — it is looking in ${repo}`);
  }
  const suites = [];
  for (const name of names) {
    if (!name.endsWith(".mjs")) continue;
    const full = join(repo, "tests", "ui", name);
    let text;
    try {
      text = readFileSync(full, "utf8");
    } catch {
      continue;
    }
    if (/import\s*\{[^}]*\bstartServer\b[^}]*\}\s*from\s*["']\.\/harness\/server\.mjs["']/
      .test(codeOnly(text, "js"))) {
      suites.push(name);
    }
  }
  if (!suites.length) {
    throw new Error('the fixture-root check derived no suite from `import { startServer } from ' +
      '"./harness/server.mjs"` in tests/ui/ — either every suite moved off that import, or this ' +
      "reader broke; fix the reader, do not widen it by hand.");
  }
  return suites.sort();
}

/** Which of [`serverSuites`] still build their fixture with `os.tmpdir()` instead of
 * `freshFixture(fixtureRoot(), …)`, as `{file, line, text}`.
 *
 * One pattern — `os.tmpdir(` anywhere in a suite's code, comments and quoted examples already cut by
 * [`codeOnly`] — rather than requiring it to sit inside a `mkdtempSync(...)` call on the same line.
 * Every violation measured so far is `fs.mkdtempSync(path.join(os.tmpdir(), "prefix-"))` on one line,
 * but a narrower pattern tied to that exact shape would miss a future one reformatted across two
 * lines or built some other way — silently, which is the failure this check exists not to have. A
 * suite has no legitimate reason to name `os.tmpdir()` at all once its fixture comes from
 * `freshFixture`/`fixtureRoot`, so the bare mention is the whole signal. */
export function tmpdirViolations(repo = REPO) {
  const out = [];
  for (const name of serverSuites(repo)) {
    const text = codeOnly(readFileSync(join(repo, "tests", "ui", name), "utf8"), "js");
    text.split("\n").forEach((line, i) => {
      if (/\bos\.tmpdir\(/.test(line)) out.push({ file: name, line: i + 1, text: line.trim() });
    });
  }
  return out;
}

/** The fixture-root gate: prints the suites it derived, then any violation, one per line. Exits 1 on
 * a violation, 2 when it could not derive a suite list at all.
 *
 * A separate entry point from [`main`] rather than folded into its exit code — this is a question
 * about SOURCE, asked once, not about processes on the box right now, and the two do not share a
 * failure mode: [`main`]'s "exit 0" is the contract `CONTRIBUTING.md` asks for after every browser
 * run, and a source-hygiene regression in a suite this run never touched should not be reported
 * through that same number. */
function fixtureRootMain(repo = REPO) {
  let suites;
  let violations;
  try {
    suites = serverSuites(repo);
    violations = tmpdirViolations(repo);
  } catch (e) {
    console.error(`fixture-root check: ${e.message}`);
    return 2;
  }
  console.log(`fixture-root check: ${suites.length} suites under tests/ui/ drive a real skein-server`);
  console.log(`  ${suites.join(" ")}`);
  if (!violations.length) {
    console.log("  none of them build a fixture from os.tmpdir()");
    return 0;
  }
  console.log(`\n${violations.length} of them still do:`);
  for (const v of violations) console.log(`  ${v.file}:${v.line}  ${v.text}`);
  return 1;
}

// ---------------------------------------------------------------------------------------------
// the check: the marker every test process carries, whatever it is called
// ---------------------------------------------------------------------------------------------
//
// **The same lesson a third time, and the derived prefixes above cannot reach this one**
// (SKEIN-861, SKEIN-873). A namespace fixture's stand-in agent is
// `bwrap … -- bash -c 'echo $$ > <root>/anchor; exec sleep 600'`. The bwrap PARENT names the
// fixture in its arguments and the scan above catches it. The `exec`'d CHILD does not:
//
//   * its `cmdline` becomes the two words `sleep 600` — `exec` replaces the image, and the fixture
//     name only ever lived in the `bash -c` script that is now gone;
//   * its `environ` never held the fixture root at all, because the script interpolated the path
//     rather than exporting it. What is left in it that is skein-shaped is the WORKTREE —
//     `$CARGO_TARGET_DIR`, `$CARGO_MANIFEST_DIR`, `$SKEIN_SERVER_BIN` — and a worktree is not a
//     fixture prefix and never will be.
//
// Measured three times in one day: six of them at `ppid=1` under one worktree, seven more from
// seven `attach.mjs` runs aged up to nine minutes, and on one of those runs this file was
// simultaneously WRONG IN BOTH DIRECTIONS — exiting 1 over four of another lane's live
// `skein-fleet-it-` processes while saying nothing whatever about the reporter's own orphans.
//
// So a second question is asked beside the first, and the two are kept because they fail
// differently: a prefix catches a process that carries a fixture name and no marker, and the marker
// catches one that has shed every name it ever had. What it cannot shed is the environment, because
// `exec` does not clear it — measured, not assumed: an `exec sleep 600` under a marked parent
// carries `SKEIN_TEST=1` still.

/** The name of the variable that says "this is a test process", read out of the two files that
 * define it.
 *
 * **It is derived for exactly the reason the prefixes above are.** `$SKEIN_TEST` is not a
 * convention this file invented: `util::in_test` branches on it, and `.cargo/config.toml`'s `[env]`
 * table is what puts it on every process `cargo test` runs, with nothing to export and nothing to
 * remember. Writing the string in here would be the SKEIN-647 defect one file over — a name that is
 * current today, and a scan that answers `0` for ever the morning somebody renames it, with nothing
 * in its output to say so.
 *
 * So the name comes from `util::TEST_MARKER`, and it is then required to be a key of that `[env]`
 * table, because a marker the constant names and cargo does not export reaches no process at all
 * and a scan for it would be looking for a string nothing carries. A disagreement between the two
 * throws rather than picking a side: either answer is a scan looking for the wrong name, and one of
 * them looks like it works.
 *
 * **What is deliberately NOT guarded is a count of zero.** No test process running is the ordinary
 * answer on an idle box — 0 of 120 here, measured — so refusing on it would refuse on success. What
 * can silently rot is the derivation, and the derivation is what throws; [`main`] prints the count
 * against the population it examined, so a zero reads as "none of 120" rather than as "nothing
 * could ever have matched". That distinction is the whole of SKEIN-647. */
export function testMarker(repo = REPO) {
  const constant = readOrRefuse(join(repo, "src", "util.rs"), "the test marker's constant");
  const named = constant.match(/TEST_MARKER:\s*&str\s*=\s*"([A-Za-z_][A-Za-z0-9_]*)"/);
  if (!named) {
    throw new Error("the leak check cannot find `TEST_MARKER: &str = \"…\"` in src/util.rs, so it \
does not know what marks a test process. Fix this reader; do not write the name in here.");
  }
  const exported = cargoEnvKeys(
    readOrRefuse(join(repo, ".cargo", "config.toml"), "cargo's [env] table"));
  if (!exported.includes(named[1])) {
    throw new Error(`the leak check read \`${named[1]}\` from util::TEST_MARKER, and \
.cargo/config.toml's [env] table exports ${exported.length ? exported.join(", ") : "nothing"} — so \
no process carries it and a scan for it would find nothing whatever was running. One of the two \
moved.`);
  }
  return named[1];
}

/** `path`'s text, or a refusal naming what was wanted from it.
 *
 * The message says the PATH it tried and not the repository it derived, because [`testMarker`]
 * takes a repository argument — `leakcheck.mjs` points it at skeletons of its own — and a message
 * naming `REPO` there would name a directory the failed read never touched. */
function readOrRefuse(path, what) {
  try {
    return readFileSync(path, "utf8");
  } catch {
    throw new Error(`the leak check cannot read ${path}, which is where ${what} lives`);
  }
}

/** The keys of `.cargo/config.toml`'s `[env]` table, and of that table only.
 *
 * A line-wise reader rather than a TOML parse, because the node tier has no TOML parser and a
 * dependency for one table would be the larger risk. It is scoped to the table by stopping at the
 * next `[header]`, so a `SKEIN_…` key under some other table cannot be mistaken for an exported
 * one — which is the error that would make [`testMarker`] agree with itself and be wrong. */
function cargoEnvKeys(text) {
  const keys = [];
  let inside = false;
  for (const line of text.split("\n")) {
    const header = line.match(/^\s*\[([^\]]+)\]/);
    if (header) {
      inside = header[1].trim() === "env";
      continue;
    }
    if (!inside) continue;
    const key = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=/);
    if (key) keys.push(key[1]);
  }
  return keys;
}

/** Does `p` carry `marker` in its environment?
 *
 * **A question about the environment and nothing else, on purpose.** The marker reaches a process
 * by inheritance and never appears in anybody's arguments, so looking at `args` too would only
 * widen what can go wrong — a suite invoked as `node -e 'SKEIN_TEST=…'` would read as a test
 * process. [`sighting`] is the right door for a name that could be on either surface; this is not
 * one of those.
 *
 * Asked of [`nulList`] rather than the joined string for the reason given there: this is a
 * variable NAME, and an empty value does not count, because `util::in_test` does not count one
 * either (`is_some_and(|v| !v.is_empty())`). */
export function marked(p, marker) {
  if (p.envState !== "read") return false;
  return p.envVars.some(v => v.startsWith(`${marker}=`) && v.length > marker.length + 1);
}

/** This worktree's root — the one [`fixturePrefixes`] reads its call sites out of.
 *
 * Exported because it is half of the verdict below and a caller cannot otherwise say what the
 * check decided "mine" against. */
export function ownWorktree() {
  return REPO;
}

/** Is `p` a process of a run in `repo`?
 *
 * **This is the half that keeps the check usable on a box several agents share**, and the false
 * positive it exists to stop was measured beside the false negative (SKEIN-873): a verdict of 1
 * over four of another lane's live `skein-fleet-it-` processes. A marker on its own cannot tell
 * a leak from somebody else's `cargo test --all` in flight, and there are thirty-odd marked
 * processes in one of those.
 *
 * The worktree path is what tells them apart, and it is DERIVED — from this file's own location,
 * the same way [`fixturePrefixes`] finds the tree — so there is no list and nothing to maintain.
 * Every process a run starts inherits it and cannot shed it: cargo puts the repository root in
 * `$CARGO_MANIFEST_DIR` on every test binary it runs whether or not `$CARGO_TARGET_DIR` is set
 * (measured), `tools/gates.sh` exports `$CARGO_TARGET_DIR` under the worktree, and `exec` does not
 * clear an environment.
 *
 * **`$SKEIN_UI_FIXTURE_ROOT` is deliberately not used for this, though SKEIN-873 offered it.** It
 * defaults to `/var/tmp/skein-uifix`, which every worktree on the box writes into — so it is the
 * one skein-shaped path that says nothing about whose run this is. [`fixtureScopes`] has a rule of
 * its own to keep that same shared root out of a kill's scope, and [`withoutSharedRoots`] is the
 * third place that has to know it: until SKEIN-979 the prefix scan was the one reader of this path
 * with no rule about it, and counted every process that merely carried it.
 *
 * The trailing boundary is not decoration. A bare `includes` would make a run in
 * `/var/tmp/skein-wt-leak` claim every process of a run in `/var/tmp/skein-wt-leakblind`, since the
 * first path is a prefix of the second — sibling worktrees on this box are named exactly that way,
 * and the mistake would hand one lane another lane's leaks to answer for. Same reasoning, and the
 * same three terminators, as [`fixtureRegex`].
 *
 * **What it is NOT is a claim that a process naming this path is this run's** (SKEIN-990) — an
 * environment carries where a process has BEEN as well as what it is using, and `OLDPWD` is
 * exactly that. [`whose`] is the rule now, and this is the boolean face of it, kept because it is
 * the shape the callers and `leakcheck.mjs` already ask in. */
export function fromWorktree(p, repo = REPO, others = otherWorktrees(repo)) {
  return whose(p, repo, others) === "mine";
}

/** Every OTHER checkout of this repository on this box, from `git worktree list` — the answer to
 * "which lanes are there", asked of the thing that knows.
 *
 * **Derived, for the reason every other name in this file is** (SKEIN-647). The lanes here are
 * `/var/tmp/skein-wt-<name>` today because `lane-common.md` says so today; a list of paths in this
 * file would be current until the morning somebody puts one somewhere else, and a check comparing
 * against paths nothing uses cannot fail. Git is the register: a worktree exists exactly when it is
 * in there, whatever it is called and wherever it lives.
 *
 * **What it cannot see is a lane that is a separate CLONE rather than a worktree of this one**,
 * because git holds no record tying the two together — so a process of such a lane carrying this
 * worktree in a breadcrumb reads as this worktree's, which is SKEIN-990 returning for that one
 * shape. It is written down here rather than guarded against, because the guard would have to be a
 * rule about what a checkout's path looks like, and a rule about paths is the thing this file keeps
 * proving it cannot have.
 *
 * **It refuses when the answer does not contain `repo` itself.** `git worktree list` names the
 * worktree it is run in, always, plus the main checkout — so a reply without this one is not "no
 * other lanes", it is a reader that has broken or a directory that is not a checkout, and those
 * must not both read as clean. Same guard as [`fixturePrefixes`]'s empty tier.
 *
 * Memoised per repository, because [`whose`] is asked of every process on the box and the answer
 * cannot change usefully inside one run of the check — a lane added halfway through a scan would be
 * read for some processes and not others, which is worse than being read for none. */
const worktreeCache = new Map();
export function otherWorktrees(repo = REPO) {
  if (worktreeCache.has(repo)) return worktreeCache.get(repo);
  let out;
  try {
    out = execFileSync("git", ["-C", repo, "worktree", "list", "--porcelain"],
      { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
  } catch (e) {
    throw new Error(`the leak check cannot ask git which other checkouts of this repository are on \
this box (\`git worktree list\` in ${repo} failed: ${e.message.split("\n")[0]}), so it cannot tell \
this worktree's processes from another lane's. Fix this reader; do not write the lanes in here.`);
  }
  const paths = [...out.matchAll(/^worktree (.+)$/gm)].map(m => m[1].replace(/\/+$/, ""));
  if (!paths.includes(repo)) {
    throw new Error(`the leak check asked git for the checkouts of this repository and got \
${paths.length ? paths.join(", ") : "nothing"} — none of which is ${repo}, the worktree it is \
running in. git names the worktree it was run in, so this is a broken reader and not an empty \
answer.`);
  }
  const others = paths.filter(p => p !== repo);
  worktreeCache.set(repo, others);
  return others;
}

/** Which checkout `p` belongs to: `"mine"` for `repo`, `"elsewhere"` for one of `others`, and
 * `"unclear"` when it names this one and another and there is nothing left to tell them apart.
 *
 * **A path in an environment says the process passed that way, not that the process is that run's**
 * (SKEIN-990). The tmux this was measured on carried `OLDPWD=<this worktree>` — the breadcrumb of
 * the `cd` another lane's agent made on its way out of it — and `PWD=<that lane>`, and nothing else
 * that named either; [`fromWorktree`] read the first, and a suite that was running perfectly well
 * two directories away became this run's leak. Every rule that tries to grade the MENTION fails
 * here: the breadcrumb is an exact path, so "names something inside it" would have thrown away the
 * owning lane's evidence too (`PWD` is exact as well — measured, and the reason that rule is not
 * the one below).
 *
 * So the tie is broken by asking the kernel where the process is standing, which is not a variable
 * and cannot be stale: `/proc/<pid>/cwd`. A test binary stands in its manifest directory, and
 * everything a suite starts inherits that — the tmux measured for SKEIN-990 stood in the lane that
 * started it, `tmux new-session` having no `-c` (`src/registry.rs:39` says so and why).
 *
 * **When it stands in neither, the answer is `"unclear"` and not a guess.** Guessing "mine" is the
 * red this item is about; guessing "theirs" is the silence SKEIN-645 was, one lane over. [`main`]
 * prints those rows under a headline that says exactly what is not known about them, which is the
 * only honest thing a check can do with a process it cannot attribute — and a bucket that is never
 * empty is then a visible reason to find more evidence, rather than a wrong verdict nobody sees.
 *
 * **The rival is looked for in the arguments as well as the environment**, though the claim on THIS
 * worktree is still read out of the environment alone. The asymmetry is deliberate and only ever
 * subtracts: a `skein-server` exec'd from `<lane>/.target/debug/skein-server` names its lane in
 * argv and nowhere else, and there is no direction in which reading that can turn somebody else's
 * process into this run's leak. */
export function whose(p, repo = REPO, others = otherWorktrees(repo)) {
  if (p.envState !== "read") return "elsewhere";
  if (!worktreeRegex(repo).test(p.env)) return "elsewhere";
  const rivals = others.filter(w => worktreeRegex(w).test(p.env) || worktreeRegex(w).test(p.args));
  if (!rivals.length) return "mine";
  const standing = standsIn(p.pid, [repo, ...rivals]);
  if (standing === repo) return "mine";
  return standing ? "elsewhere" : "unclear";
}

/** Which of `checkouts` this pid's working directory is inside, or `null` — the kernel's answer,
 * read from `/proc/<pid>/cwd`.
 *
 * `null` covers three facts that are all the same fact here: the process is standing outside every
 * checkout, it exited between the scan and this read, or this user may not look (another user's
 * daemon). None of them is evidence, and [`whose`] treats the absence of evidence as the absence of
 * evidence rather than as a verdict.
 *
 * The longest match wins, so a checkout nested inside another is answered with the inner one. The
 * boundary is [`worktreeRegex`]'s, for its reason: `/var/tmp/skein-wt-leak` must not claim a
 * process standing in `/var/tmp/skein-wt-leakblind`. */
function standsIn(pid, checkouts) {
  let cwd;
  try {
    cwd = readlinkSync(`/proc/${pid}/cwd`);
  } catch {
    return null;
  }
  let best = null;
  for (const c of checkouts) {
    if ((cwd === c || cwd.startsWith(`${c}/`)) && (!best || c.length > best.length)) best = c;
  }
  return best;
}

/** A path, escaped for a regexp.
 *
 * **One spelling of "this path, literally", because there are now three readers of it** —
 * SKEIN-918's first named cost, and SKEIN-979 added the third. [`worktreeRegex`] asks whether a
 * process belongs to a worktree, [`withoutWorktree`] takes that path back out of the text, and
 * [`withoutSharedRoots`] does the same for the shared fixture root; three escapes would be three
 * paths the day one of them was edited, and the failure would be silent in every direction at once.
 * It was named for the worktree while the worktree was the only path it escaped. */
function pathLiteral(path) {
  return path.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&");
}

/** `repo`, anchored so that it cannot match a longer sibling path — see [`fromWorktree`]. */
export function worktreeRegex(repo) {
  return new RegExp(`${pathLiteral(repo)}(?:/|:|\\s|$)`);
}

/** `p` with `repo` subtracted from both surfaces — the argv and environment a fixture name is then
 * looked for in.
 *
 * **A worktree is not a fixture, and one named after a fixture was being read as one** (SKEIN-918).
 * The two needles this file carries can be the same string: [`fromWorktree`] looks for the worktree
 * ROOT, [`fixtureRegex`] looks for a derived prefix followed by `[A-Za-z0-9._-]*`, and **38 of the
 * 56** derived prefixes are bare words with no trailing separator — `skein-review`, `skein-attrib`,
 * `skein-path`, `skein-mail`, `skein-iso`. So any checkout whose directory name merely STARTS with
 * one of those reads as a fixture, and cargo puts that path in `$CARGO_MANIFEST_DIR` on every test
 * binary it runs. (It was 39 of 57 when this was written against master at a7f72b5: `…` is itself a
 * bare word, and SKEIN-917 took it out. The count is reproduced by
 * `fixturePrefixes().prefixes.filter(p => !/[-_.]$/.test(p)).length`, not carried.) Both of these
 * match, measured on this branch:
 *
 *     CARGO_MANIFEST_DIR=/var/tmp/skein-attrib-old
 *     CARGO_MANIFEST_DIR=/var/tmp/skein-review-mybranch
 *
 * Every process of such a lane then lands in the "run is in flight" report under a prefix it has
 * nothing to do with, and its orphans are reported under that prefix too. `/var/tmp/skein-wt-<lane>`
 * is what this box happens to use and it happens not to collide — luck, and it holds only until
 * somebody names a checkout after the thing they are working on.
 *
 * **Subtracting is the fix rather than tightening the pattern**, and the alternatives are recorded
 * so they are not re-proposed: requiring a temp root separates nothing here, because worktrees live
 * in `/var/tmp` beside the fixtures; requiring every prefix to end at a boundary would change what
 * the check hunts for on every run and would stop a real fixture named `skein-review42` being seen;
 * leaving it to `leakcheck.mjs`'s assertion makes everyone who trips it read a confusing report
 * first.
 *
 * **Only the ROOT goes, and that is what keeps a fixture INSIDE a worktree visible** — SKEIN-918's
 * second named cost, and the measurement says the cost is not paid. Take `<repo>` out of
 * `SKEIN_HOME=<repo>/target/ui-onboard-4211` and what is left is `SKEIN_HOME=/target/ui-onboard-4211`,
 * which still carries `/ui-onboard` and still matches. What is removed is exactly the case where the
 * worktree path IS the match. (No test creates a fixture inside a worktree today either: all twenty
 * node call sites root at `os.tmpdir()` or `fixtureRoot()` — `$SKEIN_UI_FIXTURE_ROOT` or
 * `/var/tmp/skein-uifix` — and `Scratch::boxes`/`Scratch::temp` at `/var/tmp` and
 * `std::env::temp_dir()`. `tests/ui/README.md` still says `onboarding.mjs` roots under `target/`;
 * `tests/ui/onboarding.mjs:54` says that stopped at SKEIN-603, and the code agrees with the code.)
 *
 * The removal is global because the path is in the environment several times over —
 * `$CARGO_MANIFEST_DIR`, `$CARGO_TARGET_DIR`, `$PWD`, `$SKEIN_SERVER_BIN` — and one occurrence left
 * behind would be the whole defect, once.
 *
 * `args` and `env` are the only fields replaced. `envState` rides along untouched because
 * [`sighting`] refuses to read an environment that was never read, and a denied one must stay
 * denied rather than become an empty string that matches nothing. */
export function withoutWorktree(p, repo = REPO) {
  const gone = new RegExp(pathLiteral(repo), "g");
  return { ...p, args: p.args.replace(gone, ""), env: p.env.replace(gone, "") };
}

/** The shared fixture ROOT — the directory the browser tier creates its fixtures INSIDE — as
 * `{variable, fallback}`: the environment variable that names it, and the path it falls back to.
 * Read out of `tests/ui/lift.mjs`'s own `fixtureRoot`, and **written down nowhere in here**.
 *
 * **It is derived for the reason [`testMarker`] and [`fixturePrefixes`] are.** The path is
 * `/var/tmp/skein-uifix` today and the variable is `$SKEIN_UI_FIXTURE_ROOT` today; a copy of either
 * in this file is a string that is current until somebody edits the one place that decides it, and
 * a scan matching a path nothing uses is this whole file's defect one variable over. So the shape
 * is read back from the function that answers the question for real, over [`codeOnly`], so that a
 * doc comment quoting it cannot satisfy the derivation — and a shape this reader stops recognising
 * throws here rather than quietly leaving the subtraction below with nothing to subtract. */
export function sharedFixtureRoot(repo = REPO) {
  const lift = readOrRefuse(join(repo, "tests", "ui", "lift.mjs"), "the shared fixture root");
  const named = codeOnly(lift, "js").match(
    /function\s+fixtureRoot\s*\(\s*\)\s*\{[^}]*?process\.env\.([A-Za-z_][A-Za-z0-9_]*)\s*\|\|\s*"([^"]+)"/);
  if (!named) {
    throw new Error("the leak check cannot find `fixtureRoot()` returning \
`process.env.<VARIABLE> || \"<path>\"` in tests/ui/lift.mjs, so it does not know which directory \
every lane on this box creates its fixtures inside. Fix this reader; do not write the path in \
here.");
  }
  return { variable: named[1], fallback: named[2].replace(/\/+$/, "") };
}

/** Every path that is a shared fixture ROOT as far as `p` is concerned: `shared.fallback`, and
 * whatever `p` itself says [`sharedFixtureRoot`]'s variable is.
 *
 * **Per process, and not "the root this check is running under", because the roots differ per
 * lane.** Each agent on this box exports its own — `/var/tmp/skein-uif-<lane>` — so a list built
 * from this process's environment would clean this lane's processes and leave every other lane's
 * reading as fixture-named, which is the noise half of SKEIN-979 left in place. Taking the value
 * off the process being judged needs no list and no lane names.
 *
 * Both surfaces, because `env VARIABLE=<path> cmd` puts the assignment in ARGUMENTS and in no
 * environment at all — measured while SKEIN-979 was reproduced, on the shell that planted the
 * probe.
 *
 * A value is a root only if it is an absolute path with at least two segments of its own. `/` and
 * `/var` are not directories anyone creates fixtures in, and the point of the rule below is that
 * what is subtracted is small and specific. */
export function rootsCarriedBy(p, shared) {
  const roots = new Set([shared.fallback]);
  const add = value => {
    const path = String(value || "").replace(/\/+$/, "");
    if (/^\/[^\s/]+(?:\/[^\s/]+)+$/.test(path)) roots.add(path);
  };
  for (const v of p.envVars) {
    if (v.startsWith(`${shared.variable}=`)) add(v.slice(shared.variable.length + 1));
  }
  for (const m of p.args.matchAll(new RegExp(`(?:^|\\s)${shared.variable}=(\\S+)`, "g"))) add(m[1]);
  return [...roots];
}

/** `p` with each of `roots` taken out of both surfaces **only where the root stands alone** — where
 * it is not the head of a longer path.
 *
 * **A process that merely MENTIONS the shared root was reading as a fixture process** (SKEIN-979),
 * and it is SKEIN-918 with a different path. `tests/ui/smoke.mjs`'s call site is a BARE prefix,
 * `skein-ui`, so [`fixtureRegex`] builds `/(?:skein\-ui)[A-Za-z0-9._-]*(?:/|\s|$)` — and both
 * `/var/tmp/skein-uifix` and the `/var/tmp/skein-uif-<lane>` of the preamble every agent here
 * exports match it, on `fix` and on `f-<lane>`. That root is on `cargo`, on `rustc`, on
 * `tools/gates.sh` and on every other process of every lane that exported it. [`fromWorktree`]
 * already says in as many words that this root "is the one skein-shaped path that says nothing
 * about whose run this is", and [`fixtureScopes`] has a rule of its own to keep it out of a kill's
 * scope; the prefix scan had neither. **It could go red and not merely add noise**: a backgrounded
 * `bash tools/gates.sh` carries this root and `$CARGO_TARGET_DIR` under this worktree, and its
 * parent shell exits — attributable to this worktree, parent gone, which is the definition of this
 * run's leak. A live gate run turned the check red for a prefix it had nothing to do with, which is
 * SKEIN-913's lesson returning through the other half.
 *
 * **"Stands alone" is the whole rule, and it is what tells a fixture process from one that merely
 * names where fixtures go.** A gate runner and a `cargo test` carry the root and nothing under it.
 * A fixture process carries something INSIDE it — `SKEIN_HOME=<root>/<prefix>-4211-ab/home`, a
 * box's tmux socket, a bwrap bind. So the subtraction is refused wherever the next character is a
 * path character, and what a fixture process carries is never touched.
 *
 * **That is also why the next fixture naming convention cannot defeat it.** Nothing here knows or
 * cares what a fixture is CALLED: the only string removed is a root, and it is removed only where
 * it names the root itself. Point `$SKEIN_UI_FIXTURE_ROOT` at a per-run directory tomorrow — the
 * one way a subtraction of this shape could blind a scan — and the bare assignment goes while
 * `<that directory>/home` stays, so the process is still named by the prefix it really carries. The
 * alternative shape, removing the root globally the way [`withoutWorktree`] removes the worktree,
 * is exactly the one that would NOT survive that: it would leave `SKEIN_HOME=/home` and see
 * nothing. The two differ because the containers differ — no test creates a fixture inside a
 * worktree (SKEIN-918 measured that and said so), and every fixture in this tier is created inside
 * this root.
 *
 * What it does not reach is a process that names some OTHER file under the shared root, because the
 * root's own basename starts with `skein-ui` and matches on its own. That is a directory only this
 * tier writes into, so naming something in it is close to the definition of a fixture process — and
 * the direction of the residue is the safe one: such a process is still reported, not hidden.
 *
 * `args` and `env` only, and `envState` untouched, for [`withoutWorktree`]'s reason. */
export function withoutSharedRoots(p, roots) {
  let { args, env } = p;
  for (const root of roots) {
    // The negative lookahead is the sibling rule [`fromWorktree`] states for the worktree, mirrored:
    // `/var/tmp/skein-uif-leak` must not be taken out of `/var/tmp/skein-uif-leakattr979`, or one
    // lane's root would start editing another lane's paths. `/` is in the class for the same reason
    // it is the point of this function.
    const alone = new RegExp(`${pathLiteral(root)}(?![A-Za-z0-9._/-])`, "g");
    args = args.replace(alone, "");
    env = env.replace(alone, "");
  }
  return { ...p, args, env };
}

/** The derived prefixes as one regexp that captures the whole fixture DIRECTORY NAME it found —
 * `skein-it-apiauth-switch-132862` out of `/tmp/skein-it-apiauth-switch-132862/fleet/…`.
 *
 * **[`fixtureRegex`] answers "is this a fixture process" and this one answers "WHICH fixture", and
 * the second question is the one [`fixturesRunning`] needs.** A prefix on its own cannot group a
 * cohort: every test in `tests/server.rs` is `skein-it-` and they are different runs. What makes
 * the name an identity is the tail the call sites do not write — `Scratch::temp` and `mkdtemp`
 * both put a pid or a random suffix on it — so the captured directory name names one run of one
 * test and nothing else.
 *
 * The leading `/` is [`fixtureRegex`]'s, and for its reason: without it a prefix matches a bare
 * word in somebody's command line. The trailing boundary is wider than [`fixtureRegex`]'s three
 * terminators, and deliberately: this is an identity being read rather than a match being made, so
 * a path that ends at a quote (`while [ -f '/tmp/<fixture>/…' ]`, which is what
 * `fleet::supervised` really writes) must give the same name as one that ends at a space. A wider
 * boundary here can only find MORE of the cohort, and more cohort is the direction that turns a
 * red into an in-flight, never the other way. */
export function fixtureNameRegex(prefixes) {
  const alt = prefixes.map(p => p.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&")).join("|");
  return new RegExp(`/((?:${alt})[A-Za-z0-9._-]*)(?![A-Za-z0-9._-])`, "g");
}

/** Which fixtures each process names, as `Map<pid, string[]>` — the fixture directory names of
 * [`fixtureNameRegex`], read off the same surfaces [`fixtureNamed`] matches on and cleaned the same
 * way, so that the containers cannot enter an identity either (SKEIN-918, SKEIN-979). */
export function fixtureCensus(all, prefixes, repo = REPO, shared = sharedFixtureRoot()) {
  const re = fixtureNameRegex(prefixes);
  const census = new Map();
  for (const p of all) {
    const s = withoutWorktree(withoutSharedRoots(p, rootsCarriedBy(p, shared)), repo);
    const names = new Set();
    for (const m of s.args.matchAll(re)) names.add(m[1]);
    if (s.envState === "read") for (const m of s.env.matchAll(re)) names.add(m[1]);
    if (names.size) census.set(p.pid, [...names]);
  }
  return census;
}

/** Is this process's fixture still being used by a run that is going? A function of a process, over
 * the whole census — and **the answer `ppid == 1` cannot give** (SKEIN-990).
 *
 * `tmux new-session -d` forks a server and the launching process returns, so a fixture's tmux is
 * reparented to pid 1 in its first second and stays there for the whole of a healthy life. Neither
 * parentage nor age can tell that from a leak: a leak is one second old in its first second too,
 * and both were measured at one second old. What tells them apart is not on the row at all — it is
 * whether anything ELSE of that fixture is still running with a live parent. Measured on a live
 * cohort (`tests/ui/leakcheck.mjs` plants the same shape):
 *
 *     tmux  ppid=1     tmux -S /tmp/<fixture>/fleet/.skein/private/server.tmux new-session -d …
 *     srv   ppid=<test binary, alive>   <lane>/.target/debug/skein-server
 *     bash  ppid=<the tmux>   while [ -f '/tmp/<fixture>/fleet/.skein/server-doorway.py' ]; do …
 *     py    ppid=<the bash>   python3 /tmp/<fixture>/fleet/.skein/server-doorway.py …
 *
 * **The descendants are the whole difficulty, and skipping them is the rule.** The `bash` and the
 * `python` name the fixture and have live parents, but they are what is LEFT when the suite dies —
 * `fleet::supervised` restarts that python every two seconds for as long as the fixture directory
 * survives, which is four tmux servers between two and nine hours old (SKEIN-645). Counting them as
 * evidence would make every stranded tmux permanently "in flight", which is that item exactly, and
 * quietly. So evidence must be a process with a live parent that is NOT descended from any
 * parentless process of the same fixture: the `skein-server` above qualifies, its parent being the
 * test binary that is still running; the `bash` and the `python` cannot, whatever their parents.
 *
 * **What this gives up, said out loud.** A run that strands a process in a fixture it then goes on
 * using is called in flight until that run ends — which is why `CONTRIBUTING.md` asks for this
 * check AFTER a run and not during one, and why the lost case is bounded by the length of a suite
 * rather than by nine hours. The alternative is the red this item was filed over, which is
 * SKEIN-913's lesson: a check that goes red for a reason the reader can see is not theirs teaches
 * people to read past it, and the next red is read past too.
 *
 * Returns a predicate over a process rather than the set, because the callers hold processes and
 * the set is keyed by a name they would have to re-derive to use. */
export function fixturesRunning(all, repo = REPO, shared = sharedFixtureRoot(repo),
  prefixes = fixturePrefixes(repo).prefixes) {
  const census = fixtureCensus(all, prefixes, repo, shared);
  const parents = new Map();
  const parent = pid => {
    if (!parents.has(pid)) parents.set(pid, parentOf(pid));
    return parents.get(pid);
  };
  const descends = (pid, ancestor) => {
    let cur = pid;
    for (let i = 0; i < 64; i++) {
      cur = parent(cur);
      if (cur === ancestor) return true;
      if (!Number.isInteger(cur) || cur <= 1) return false;
    }
    return false;
  };
  const byName = new Map();
  for (const [pid, names] of census) {
    for (const name of names) {
      if (!byName.has(name)) byName.set(name, []);
      byName.get(name).push(pid);
    }
  }
  const running = new Set();
  for (const [name, pids] of byName) {
    const stranded = pids.filter(pid => parent(pid) === 1);
    // A live parent is `> 1` and not merely "not 1": `parentOf` answers `null` for a pid that has
    // gone between the scan and this read, and a sibling that has just exited is not a run still
    // going. Reading it as evidence would spare a real leak, which is the silent direction.
    const live = pid => Number.isInteger(parent(pid)) && parent(pid) > 1;
    if (pids.some(pid => live(pid) && !stranded.some(s => descends(pid, s)))) running.add(name);
  }
  return p => (census.get(p.pid) || []).some(name => running.has(name));
}

/** Every process carrying `marker`, split three ways: `{carrying, orphans, attached, theirs,
 * theirOrphans}`.
 *
 * **`orphans` is the only one of them that fails the check, and "orphan" is the kernel's answer
 * rather than a number this file picked.** A process whose parent is pid 1 has had its parent die
 * under it; a process whose parent is alive belongs to a run that is still going. That is what a
 * leak IS — a test process outliving the run that started it — so it needs no age threshold, and an
 * age threshold is what would have been wrong: a slow build is legitimately ten minutes old and a
 * leak is five seconds old the moment it is made.
 *
 * It is also what makes the check pass on a busy box, which is the difference between a check that
 * is run and a check that is ignored. `cargo test --all` in THIS worktree has thirty-odd marked
 * test binaries alive at once, every one of them carrying `$CARGO_MANIFEST_DIR` and therefore this
 * repository's root; failing over those would make the gate red for the whole length of every run.
 * Their parent is a live `cargo`, which carries no marker of its own — cargo's `[env]` table
 * applies to the processes cargo RUNS, not to cargo — so the chain is intact and they are
 * `attached`, counted and not listed.
 *
 * **Every orphan actually measured was `ppid=1`**, which is why this costs nothing real: the six
 * under one worktree in SKEIN-873, the seven from seven `attach.mjs` runs in SKEIN-861, and the one
 * found on a final gate run. What it does give up is named in [`main`]: a supervisor that leaks a
 * child while itself staying alive is counted rather than failed on.
 *
 * **The parenthesis this paragraph used to end on was a defect and not a caveat** (SKEIN-990). It
 * read "a process daemonised on purpose — a fixture's tmux server is `ppid=1` by design — reads as
 * an orphan if you run this DURING a suite instead of after it", and that is not a cost of asking
 * after a run: a `tmux new-session -d` of a suite in ANOTHER worktree took this check red twice,
 * one second old each time, in a tree where nothing was running at all. [`fixturesRunning`] is what
 * answers for the daemonised shape now, and it is the second argument to [`attribute`].
 *
 * `theirOrphans` is the same question asked of the other lanes, and is reported for a person to
 * read rather than for the exit code: somebody's leak, plainly, and not this run's to be red about.
 * It is how the six in SKEIN-873 were found by hand in the first place.
 *
 * **The cohort test defaults to being derived here rather than to being off.** A caller that does
 * not pass one gets the rule, at the cost of one more pass over the box; `main` passes the one it
 * built so the two halves share the work and, more to the point, agree. An `inFlight` defaulting to
 * "nothing is in flight" would make the SKEIN-990 behaviour the one you get by forgetting, which is
 * how a fix stops being a fix. */
export function testMarked(all, marker, repo = REPO, inFlight = fixturesRunning(all, repo),
  others = otherWorktrees(repo)) {
  const carrying = all.filter(p => marked(p, marker));
  return { carrying, ...attribute(carrying.map(p => [p, p]), repo, inFlight, others) };
}

/** The rule, and the only copy of it: split `hits` into `{orphans, attached, theirs,
 * theirOrphans}`.
 *
 * **One function because there is one question, and the two halves answering it differently is
 * exactly what SKEIN-913 was.** The marker half got both rules at SKEIN-884 — attribute by
 * worktree, fail only on `ppid=1` — and the prefix half got neither, so the same output could say
 * "nothing here is this run's to be red about" and exit 1 over another lane's live compile. A
 * second implementation of the same sentence is a second thing to forget to change; there is now
 * nowhere for the two to drift apart, because there is nothing to keep in step.
 *
 * `hits` is a list of `[process, record]`. The process is what the rule is decided from — it needs
 * the environment, which [`fromWorktree`] reads; the record is what the caller wants back, which
 * for the marker half is the process itself and for the prefix half is the printable row, carrying
 * `pid`, `age`, `args`, the prefix and the surface and **no environment at all**, so that a token
 * cannot reach a report however the printer is later edited (see [`processes`]).
 *
 * **Each pid's parent is read once and the answer reused**, rather than asked again per bucket. Two
 * reads of `/proc/<pid>/stat` are two different moments: a process reparented between them lands in
 * both lists or in neither, and "in neither" is this file's own defect — a process the check looked
 * at and then said nothing about. `parentOf` answers `null` for a pid that has gone, which is not
 * `1`, so such a process counts as attached and is not failed over. That is the safe direction and
 * the true one: a pid that has exited is not running, which is the whole question.
 *
 * **Another worktree's process whose parent IS gone is a real leak, and still not this run's.** It
 * is counted into `theirOrphans` and never into `orphans`, which is the answer the marker half has
 * given since SKEIN-884 and the only one an attributed check can give: the lane that owns that path
 * is the one that can tell a leak from a fixture it is still using, and a verdict handed to a lane
 * that cannot act on it is a red nobody can clear. [`attributionLine`] is where that is said in
 * words, and it says whose.
 *
 * **`unclear` is the fourth bucket and it is deliberately not a verdict** (SKEIN-990). [`whose`]
 * answers it for a process that names this worktree AND another checkout of it and is standing in
 * neither — the shape an environment carrying a `cd` breadcrumb has once the evidence that would
 * settle it is gone. Both available guesses are a defect this file already has a name for: "mine"
 * is the red this item was filed over, and "theirs" is SKEIN-645's silence with somebody else's
 * name on it. So it is counted, printed with its rows, and reaches no exit code — and a bucket
 * that stops being empty is a visible reason to go and find better evidence, which a wrong verdict
 * would never have been.
 *
 * `inFlight` is [`fixturesRunning`], and it is applied to the other lanes' rows as well as to this
 * one's: another lane's in-flight tmux is no more "somebody's leak" than this one's is. */
function attribute(hits, repo, inFlight = () => false, others = otherWorktrees(repo)) {
  const orphans = [];
  const attached = [];
  const theirs = [];
  const unclear = [];
  let theirOrphans = 0;
  for (const [p, record] of hits) {
    const parent = parentOf(p.pid);
    // "Nothing is left of the run that made it": its parent is gone AND no other process of its
    // fixture is still running under a live parent. The second half is what a daemon needs — see
    // [`fixturesRunning`], and SKEIN-990 for the red that was printed without it.
    const stranded = parent === 1 && !inFlight(p);
    const verdict = whose(p, repo, others);
    if (verdict === "unclear") {
      unclear.push(record);
    } else if (verdict === "elsewhere") {
      theirs.push(record);
      if (stranded) theirOrphans++;
    } else if (stranded) {
      orphans.push(record);
    } else {
      attached.push(record);
    }
  }
  return { orphans, attached, theirs, theirOrphans, unclear };
}

/** Every process whose argv or environment names one of `patterns` — `[prefix, RegExp]` pairs —
 * split by [`attribute`], as `{carrying, orphans, attached, theirs, theirOrphans}`.
 *
 * The same shape [`testMarked`] returns, deliberately: [`main`] prints the two with one sentence
 * ([`attributionLine`]) and fails on one bucket of each.
 *
 * **The scan is the old one and reaches exactly as far** (SKEIN-913 says so in as many words). Both
 * surfaces, every derived prefix, one regexp per prefix so the report can name which — the first
 * match wins and stops the inner loop, because the report has one column for it and a process
 * matching two prefixes is still one process. What SKEIN-913 changed is below this line, in what
 * the buckets mean, and not above it.
 *
 * Exported so that `leakcheck.mjs` can plant a process and ask which bucket it lands in. That is
 * the half of this a suite can own: which bucket reaches the exit code is a single integer that
 * any lane's leak could also produce, so the two are asserted apart — the same division as
 * SKEIN-780. */
export function fixtureNamed(all, patterns, repo = REPO, shared = sharedFixtureRoot(),
  inFlight = fixturesRunning(all, repo, shared, patterns.map(([prefix]) => prefix)),
  others = otherWorktrees(repo)) {
  const hits = [];
  for (const p of all) {
    // The containers come out before any prefix goes in — the shared fixture root (SKEIN-979) and
    // then the worktree (SKEIN-918) — once per process rather than once per pattern: the answer
    // does not depend on which prefix is being tried, and there are fifty-odd of those against
    // every process on the box. The root goes first because a lane may put its root inside its own
    // worktree, and the worktree's removal would otherwise leave a root this file no longer
    // recognises as one.
    const surfaces = withoutWorktree(withoutSharedRoots(p, rootsCarriedBy(p, shared)), repo);
    for (const [prefix, re] of patterns) {
      const where = sighting(surfaces, re);
      if (!where) continue;
      hits.push([p, { pid: p.pid, age: p.age, args: p.args, prefix, where }]);
      break;
    }
  }
  return { carrying: hits.map(([, record]) => record), ...attribute(hits, repo, inFlight, others) };
}

/** This pid's parent, or `null` when it cannot be read. Field 4 of `/proc/<pid>/stat`, taken after
 * the last `)` for [`ageOf`]'s reason. */
function parentOf(pid) {
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    return Number(stat.slice(stat.lastIndexOf(") ") + 2).trim().split(/\s+/)[1]);
  } catch {
    return null;
  }
}

/** The gate. Prints what it looked for, then what it could not look at, then what it found; exits 1
 * on a leak, 2 when it could not build a pattern at all.
 *
 * Printing the prefixes is not decoration. `0` on its own is the answer this check gave for as long
 * as it was wrong, and a reader had no way to tell "nothing is running" from "nothing could ever
 * match". With the list in front of them, a missing fixture name is visible in the output of a
 * passing run.
 *
 * **The denied line is the same argument one level down** (SKEIN-687). "Nothing is running from any
 * of them" is a claim about every process on the box, and the environment of some of them cannot be
 * read at all — so a verdict that did not say how many were half-checked would be overstating what
 * was looked at, which is the family of error this whole file is about. They are not counted as
 * leaks: this process cannot have started one it is not allowed to inspect.
 *
 * **The counts print when they are zero, and that is the same argument again.** Each half is four
 * numbers against the population examined, so the output of a run that looked at 136 processes and
 * found nothing cannot be mistaken for the output of a run that could not have found anything. One
 * number of each four reaches the exit code — this worktree's processes whose parent is gone. The
 * other three are printed for a reader: this worktree's with a live parent (a run in flight),
 * another lane's, and how many of those have been reparented. [`fromWorktree`] argues why another
 * lane's cannot be failed on, and [`attribute`] why a live parent is the line rather than an age.
 *
 * **The two halves ask different questions and now reach the same verdict the same way**
 * (SKEIN-913). It used to be that the prefix scan had no equivalent of the parent test, so a prefix
 * match fired on another lane's `cargo test` seconds into it (SKEIN-867) and its verdict
 * contradicted the marker's in the same output. They are still printed as two, because which scan
 * saw a process is a fact worth having; what they no longer do is disagree about whose it is. Each
 * prints [`attributionLine`], and only the first number of either turns the exit red.
 *
 * **Three reports rather than one, and the headline says which bucket it is.** A row that is not
 * this run's is still worth printing — with its age and the surface its name was seen on — and the
 * old single report printed it under a headline ("processes are still running from a test fixture")
 * that read as an accusation whatever the rows were. Another lane's reparented processes are named
 * as theirs in the headline they appear under, so a reader cannot take somebody else's leak for
 * their own.
 *
 * **One regexp per prefix rather than one over all of them**, which costs a few thousand tests and
 * buys the report a name it can print — see [`fixtureNamed`]. The prefix it names is a literal off
 * the derived list, so no part of a process's environment reaches the output even when the
 * environment is where the match was (see [`sighting`]), and the record that reaches the printer
 * carries no environment at all. */
function main(argv) {
  const minAge = Number((argv.find(a => a.startsWith("--min-age=")) || "").split("=")[1] || 0);
  let derived;
  let marker;
  let shared;
  let lanes;
  try {
    derived = fixturePrefixes();
    marker = testMarker();
    shared = sharedFixtureRoot();
    lanes = otherWorktrees();
  } catch (e) {
    console.error(`leak check: ${e.message}`);
    return 2;
  }
  const { prefixes, files, quoted } = derived;
  console.log(`leak check: ${prefixes.length} fixture prefixes read from ${files} test files`);
  console.log(`  ${prefixes.join(" ")}`);
  // Said out loud rather than cut in silence (SKEIN-917): these are shapes the tree mentions only
  // in prose, and the line is the difference between a cutter that fired and one that had nothing
  // to remove — which is the same distinction the count above exists to make.
  if (quoted.length) {
    console.log(`  ${quoted.length} more appear only in comments and are not fixture names: ` +
      quoted.join(" "));
  }
  // Said out loud for the reason the prefixes are (SKEIN-979): this line is a rule that makes the
  // check see LESS, and a reader must be able to tell that it fired and what it took out. Every
  // lane exports a root of its own, so the one named here is only the fallback — what a process is
  // actually cleaned of is whatever IT carries in that variable.
  console.log(`  a process naming $${shared.variable} (${shared.fallback} by default) and nothing ` +
    "under it is not a fixture process, so that path is taken out before the prefixes go in");
  // The other two rules that make the check see less, said out loud for the same reason and in the
  // same place (SKEIN-990). The lane count is git's answer and not a list in here, so a reader can
  // see at once whether the check knows about the worktree whose processes it is looking at.
  console.log(`  git names ${lanes.length} other checkout${lanes.length === 1 ? "" : "s"} of this ` +
    "repository on this box; a process that names one of them as well as this worktree belongs to " +
    "whichever of the two it is standing in, and to neither when it stands in neither");
  console.log("  a process whose parent is gone is a leak only if nothing else of its fixture is " +
    "still running under a live parent — a tmux a suite daemonises has no parent from its first " +
    "second, and nor has one left behind for nine hours");
  const patterns = prefixes.map(prefix => [prefix, fixtureRegex([prefix])]);
  const mine = new Set(ancestry());
  const all = processes().filter(p => !mine.has(p.pid));
  const denied = all.filter(p => p.envState === "denied").length;
  if (denied) {
    console.log(
      `  ${denied} of ${all.length} processes would not let this user read their environment, so ` +
        `only their command line was checked and $${marker} could not be asked of them at all`);
  }
  // Derived once and handed to both halves: the same cohort answering the same question twice is
  // the way the two halves stopped being able to contradict each other (SKEIN-913), and a second
  // derivation would be a second moment as well as a second pass.
  const running = fixturesRunning(all, REPO, shared, prefixes);
  const named = fixtureNamed(all, patterns, REPO, shared, running, lanes);
  for (const said of prefixLines(named, all.length)) console.log(said);
  // Oldest first and older than `--min-age`, per bucket. The filter is applied to each list rather
  // than to the scan, so the counts above are about the box and the rows below are about what was
  // asked for — two different claims, which is why they are two numbers.
  const listed = rows => rows
    .filter(p => p.age === null || p.age >= minAge)
    .sort((a, b) => (b.age || 0) - (a.age || 0));
  const mineOrphans = listed(named.orphans);
  const inFlight = listed(named.attached);
  const elsewhere = listed(named.theirs);
  const unclear = listed(named.unclear);
  if (!mineOrphans.length && !inFlight.length && !elsewhere.length && !unclear.length) {
    console.log(`  nothing is running from any of them${minAge ? ` and older than ${minAge}s` : ""}`);
  }
  if (elsewhere.length) {
    for (const said of reportLines(elsewhere, named.theirOrphans
      ? "processes name a test fixture and are not this worktree's — " +
        `${named.theirOrphans} of them have nothing left of the run that made them, which is a ` +
        "leak belonging to whichever lane owns that path, to be reported there and not answered " +
        "for here"
      : "processes name a test fixture and are not this worktree's, so not this run's to be red " +
        "about")) {
      console.log(said);
    }
  }
  if (unclear.length) {
    for (const said of reportLines(unclear, unclearHeadline())) console.log(said);
  }
  if (inFlight.length) {
    for (const said of reportLines(
      inFlight, "processes name a test fixture of this worktree and the run that made them is " +
        "still going — a live parent, or another process of the same fixture that has one — so a " +
        "run is in flight rather than a leak")) {
      console.log(said);
    }
  }
  if (mineOrphans.length) {
    for (const said of reportLines(
      mineOrphans, "processes are still running from a test fixture of this worktree with nothing " +
        "left of the run that made them, which is this run's leak")) {
      console.log(said);
    }
  }

  // The second question, and its counts print whether or not either is zero. "Nothing is running"
  // was this check's answer for as long as it was wrong, and a reader could not tell it from
  // "nothing could ever match"; a count against the population examined can be read either way
  // round, which is the whole of SKEIN-647 and the reason the prefixes are printed above.
  const split = testMarked(all, marker, REPO, running, lanes);
  for (const said of markerLines(split, marker, all.length)) console.log(said);
  const markUnclear = listed(split.unclear.map(p =>
    ({ pid: p.pid, age: p.age, args: p.args, prefix: marker, where: "environment" })));
  if (markUnclear.length) {
    for (const said of reportLines(markUnclear, unclearHeadline())) console.log(said);
  }
  const { orphans } = split;
  const marks = orphans
    .filter(p => p.age === null || p.age >= minAge)
    .map(p => ({ pid: p.pid, age: p.age, args: p.args, prefix: marker, where: "environment" }))
    .sort((a, b) => (b.age || 0) - (a.age || 0));
  if (marks.length) {
    for (const said of reportLines(
      // The two red headlines end in the same six words on purpose: they are the same verdict
      // reached by two scans, and a reader skimming the output should not have to work out which
      // of several reports is the one being exited 1 over (SKEIN-913).
      marks, `processes carry $${marker} from this worktree with nothing left of the run that ` +
        "made them, which is this run's leak")) {
      console.log(said);
    }
  }
  // One bucket of each half, and it is the same bucket: this worktree's, with nothing left of the
  // run that made it. Everything else printed above is printed and nothing more (SKEIN-913,
  // SKEIN-990).
  return mineOrphans.length || marks.length ? 1 : 0;
}

/** The headline the `unclear` rows are printed under, and **the only place either half says it**,
 * so that the two cannot describe the same bucket differently (SKEIN-913's argument, applied to the
 * bucket SKEIN-990 added).
 *
 * It says what is not known rather than guessing: these processes name this worktree and another
 * checkout of it, and are standing in neither, so there is nothing left to decide between them. A
 * reader who wants to know can read `/proc/<pid>/environ` — which is how the first of them was
 * chased — and the row carries the pid and the age to start from. */
function unclearHeadline() {
  return "processes name this worktree AND another checkout of it and are standing in neither, so " +
    "whose run they belong to cannot be told from here — not counted as this run's leak, and not " +
    "cleared either";
}

/** The marker verdict's summary, as the lines [`main`] prints: how many of `population` carry
 * `marker`, and how the ones that do divide up.
 *
 * **A function, and not three `console.log`s inside [`main`], for the reason [`reportLines`] is one**
 * (SKEIN-780). The property that matters here is that these lines are printed AT ALL when every
 * count is zero — "nothing is running" was this check's answer for as long as it was wrong, and a
 * reader could not tell it from "nothing could ever have matched" (SKEIN-647). That property cannot
 * be asserted against the box: there is no way to make a shared fleet box hold still at zero marked
 * processes, and on a busy one a `if (carrying.length)` guard around these lines would be invisible
 * — the counts print either way, and the assertion passes while the defect is there. Asked of this
 * function over a split the caller built, it is one comparison and it reproduces every time.
 *
 * Unconditional by construction: there is no branch in here to add a guard to. */
export function markerLines(split, marker, population, repo = REPO) {
  return [
    `leak check: $${marker} is set on ${split.carrying.length} of ${population} processes`,
    attributionLine(split),
    `  this worktree is ${repo}`,
  ];
}

/** The four-way split in one sentence, and **the same sentence for both halves** — [`main`] prints
 * it under the prefix count and again under the marker count, so a reader comparing the two is
 * comparing like with like and cannot be handed the contradiction SKEIN-913 was.
 *
 * Only the first number reaches the exit code. The other four are for a person: this worktree's
 * with its run still going is a run in flight, and another lane's is another lane's — including the
 * stranded ones, which are a genuine leak that this run still must not be red about, and the
 * clause says so rather than leaving a reader to work out whose they are. [`whose`] argues why
 * another lane's cannot be failed on and what the fourth number is, [`attribute`] and
 * [`fixturesRunning`] why the end of a run is the line rather than an age or a parent. */
export function attributionLine({ orphans, attached, theirs, theirOrphans, unclear = [] }) {
  return `  ${orphans.length} of them are this worktree's with nothing left of the run that made ` +
    `them, which is a leak; ${attached.length} are this worktree's with that run still going, so ` +
    `a run is in flight; ${theirs.length} are from elsewhere on this box (${theirOrphans} of ` +
    "those with nothing left of their run, so somebody's leak and not this run's to be red " +
    `about); ${unclear.length} name this worktree and another checkout of it and stand in ` +
    "neither, so whose they are cannot be told from here";
}

/** The prefix verdict's summary, as the lines [`main`] prints: how many of `population` carry a
 * derived fixture name, and how the ones that do divide up.
 *
 * Unconditional for [`markerLines`]'s reason, which is the whole of SKEIN-647: a count against the
 * population examined can be read as "none of 136" where a bare "nothing is running" cannot be told
 * apart from "nothing could ever have matched". The prefix half printed the second of those for as
 * long as it was wrong. */
export function prefixLines(split, population) {
  return [
    `leak check: a derived fixture name is on ${split.carrying.length} of ${population} processes`,
    attributionLine(split),
  ];
}

/** How many rows a report prints when it cannot print them all — half from each end. */
export const REPORT_CAP = 40;

/** The report for `shown` — oldest first, **capped at both ends** — as the lines [`main`] prints.
 *
 * **The young end is the half that was missing** (SKEIN-732). `shown` is sorted oldest first, and
 * this printed `slice(0, REPORT_CAP)` — so past forty the processes it dropped were the NEWEST,
 * which is to say the ones the run that just finished had left behind. A leak check exists to
 * answer "did I leave something running", and the answer was the first thing truncated away:
 * `leakcheck.mjs` planted a process, ran this, and could not find it, because twenty-three sibling
 * suites were holding fixtures older than it.
 *
 * So the cap stays — a wall of two hundred lines is read by nobody — but it is spent on both ends.
 * The oldest are the leaks that have been accumulating; the newest are yours.
 *
 * **A function rather than a loop inside [`main`] so that it can be asked about rows a caller
 * owns** (SKEIN-780). Which rows survive the cap is a property of this list and nothing else, and
 * it is the only half of the report a test can hold still: the list [`main`] builds is every
 * fixture process on the box, which on a fleet box several agents share is being added to and
 * taken from while the check looks at it. `leakcheck.mjs` asserts the box half against real
 * processes and this half against rows it built, because the two are not assertable in one place. */
export function reportLines(shown, headline = "processes are still running from a test fixture") {
  const head = shown.length > REPORT_CAP ? shown.slice(0, REPORT_CAP / 2) : shown;
  const tail = shown.length > REPORT_CAP ? shown.slice(-REPORT_CAP / 2) : [];
  const line = p => {
    const age = p.age === null ? "?" : `${p.age}s`;
    return `  ${String(p.pid).padStart(7)}  ${age.padStart(7)}  ${p.where.padEnd(11)} ${p.prefix}  ` +
      p.args.slice(0, 160);
  };
  const said = [`\n${shown.length} ${headline}:`];
  said.push(...head.map(line));
  if (tail.length) {
    said.push(`  … ${shown.length - REPORT_CAP} more, between the oldest ${head.length} above and ` +
      `the newest ${tail.length} below`);
    said.push(...tail.map(line));
  }
  return said;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const argv = process.argv.slice(2);
  process.exit(argv.includes("--fixture-root") ? fixtureRootMain() : main(argv));
}
