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
// The other half is [`quiesceOnExit`], and it is `tests/common/mod.rs`'s `Scratch` argument
// transplanted: *whatever has to stop, stops on every path; only the removal is conditional*. The
// node tier had no equivalent — `srv.kill()` sat at the top level of each suite, after the last
// check, so a throw or a Ctrl-C skipped it, and it never covered the tmux server anyway, because
// that server is not `skein-server`'s child to take with it.
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import os from "node:os";

const SELF = fileURLToPath(import.meta.url);
const REPO = resolve(dirname(SELF), "..", "..", "..");

// ---------------------------------------------------------------------------------------------
// reading /proc
// ---------------------------------------------------------------------------------------------

/** Every process on this machine, as `{pid, args, age, env, envState}` — argv and environment each
 * NUL-joined back into a line, and age in whole seconds.
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

/** This pid's environment, as `{env, envState}` — and **a read that fails is an answer of its own**,
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
    return { env: nulJoined(readFileSync(`/proc/${pid}/environ`)), envState: "read" };
  } catch (e) {
    const denied = e.code === "EACCES" || e.code === "EPERM";
    return { env: "", envState: denied ? "denied" : "gone" };
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

/** Where a fixture prefix is written down, and how to read it back.
 *
 * `tests/common/mod.rs` is deliberately absent: it holds `Scratch::at(root, prefix)`, the
 * implementation, whose `prefix` is a variable. Only call sites name a fixture. */
const SOURCES = [
  { dir: "tests", ext: ".rs", tier: "rust", read: rustPrefixes },
  { dir: "tests/ui", ext: ".mjs", tier: "node", read: nodePrefixes },
  { dir: "tests/ui/harness", ext: ".mjs", tier: "node", read: nodePrefixes },
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

/** Every fixture prefix this repository can produce, read from the files that produce them.
 *
 * Returns `{prefixes, tiers, files}`. **It throws when a tier contributes nothing**, and that is
 * the guard the old check lacked: `0` from a pattern is only meaningful if the pattern was built
 * from something. A rename that this reader stops recognising fails loudly here instead of turning
 * the gate into a machine for printing zero. */
export function fixturePrefixes(repo = REPO) {
  const prefixes = new Set();
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
      for (const prefix of source.read(text)) {
        prefixes.add(prefix);
        tiers[source.tier] = (tiers[source.tier] || 0) + 1;
      }
    }
  }
  for (const source of SOURCES) {
    if (!tiers[source.tier]) {
      throw new Error(
        `the leak check derived no fixture prefix from the ${source.tier} tier, so a count from it \
would mean nothing. Either the call sites moved, or the shapes ${source.tier === "rust"
          ? "`Scratch::boxes(\"…\")` / `Scratch::temp(\"…\")`"
          : "`mkdtempSync(…, \"…\")` / `freshFixture(…, \"…\")`"} were renamed — fix this reader, \
do not widen it by hand.`);
    }
  }
  return { prefixes: [...prefixes].sort(), tiers, files };
}

/** One regexp over the derived prefixes: a path separator, the prefix, and the rest of that
 * directory's name. The leading `/` is what stops a prefix from matching a bare word somewhere in
 * an unrelated command line. */
export function fixtureRegex(prefixes) {
  const alt = prefixes.map(p => p.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&")).join("|");
  return new RegExp(`/(?:${alt})[A-Za-z0-9._-]*(?:/|\\s|$)`);
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
 * **One regexp per prefix rather than one over all of them**, which costs a few thousand tests and
 * buys the report a name it can print. The prefix it names is a literal off the list two lines
 * above, so no part of a process's environment reaches the output even when the environment is
 * where the match was — see [`sighting`]. The record that reaches the printer carries `pid`, `age`,
 * `args`, the prefix and the surface, and no environment at all, so printing one cannot leak a
 * token however this loop is later edited. */
function main(argv) {
  const minAge = Number((argv.find(a => a.startsWith("--min-age=")) || "").split("=")[1] || 0);
  let derived;
  try {
    derived = fixturePrefixes();
  } catch (e) {
    console.error(`leak check: ${e.message}`);
    return 2;
  }
  const { prefixes, files } = derived;
  console.log(`leak check: ${prefixes.length} fixture prefixes read from ${files} test files`);
  console.log(`  ${prefixes.join(" ")}`);
  const patterns = prefixes.map(prefix => [prefix, fixtureRegex([prefix])]);
  const mine = new Set(ancestry());
  const all = processes().filter(p => !mine.has(p.pid));
  const found = [];
  for (const p of all) {
    for (const [prefix, re] of patterns) {
      const where = sighting(p, re);
      if (!where) continue;
      found.push({ pid: p.pid, age: p.age, args: p.args, prefix, where });
      break;
    }
  }
  const denied = all.filter(p => p.envState === "denied").length;
  if (denied) {
    console.log(
      `  ${denied} of ${all.length} processes would not let this user read their environment, so ` +
        "only their command line was checked");
  }
  const shown = found
    .filter(p => p.age === null || p.age >= minAge)
    .sort((a, b) => (b.age || 0) - (a.age || 0));
  if (!shown.length) {
    console.log(`  nothing is running from any of them${minAge ? ` and older than ${minAge}s` : ""}`);
    return 0;
  }
  console.log(`\n${shown.length} processes are still running from a test fixture:`);
  // **Both ends when it does not all fit, and the young end is the half that was missing**
  // (SKEIN-732). `shown` is sorted oldest first, and this printed `slice(0, 40)` — so past forty
  // the processes it dropped were the NEWEST, which is to say the ones the run that just finished
  // had left behind. A leak check exists to answer "did I leave something running", and the answer
  // was the first thing truncated away: `leakcheck.mjs` planted a process, ran this, and could not
  // find it, because twenty-three sibling suites were holding fixtures older than it.
  //
  // So the cap stays — a wall of two hundred lines is read by nobody — but it is spent on both
  // ends. The oldest are the leaks that have been accumulating; the newest are yours.
  const CAP = 40;
  const head = shown.length > CAP ? shown.slice(0, CAP / 2) : shown;
  const tail = shown.length > CAP ? shown.slice(-CAP / 2) : [];
  const line = p => {
    const age = p.age === null ? "?" : `${p.age}s`;
    console.log(
      `  ${String(p.pid).padStart(7)}  ${age.padStart(7)}  ${p.where.padEnd(11)} ${p.prefix}  ` +
        p.args.slice(0, 160));
  };
  head.forEach(line);
  if (tail.length) {
    console.log(`  … ${shown.length - CAP} more, between the oldest ${head.length} above and the ` +
      `newest ${tail.length} below`);
    tail.forEach(line);
  }
  return 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)));
}
