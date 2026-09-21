// Lift a top-level declaration out of index.html by name, so the page's pure logic can be tested
// in plain node — which works inside a box, where `smoke.mjs` cannot run at all (it needs chromium's
// system libraries and a box has no working sudo to install them).
//
// Shared by `voice.mjs` and `tabs.mjs`. It lives here rather than being copied into each because it
// is a brace matcher, and two copies of a subtle brace matcher is one that quietly drifts.
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { safeHref } from "../../cockpit/src/links.mjs";
import { quiesceOnExit, testMarker } from "./harness/leaks.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

// **A browser suite run by hand was not a test process, and every process it started inherited
// that** (SKEIN-861). `.cargo/config.toml`'s `[env]` table puts `$SKEIN_TEST` on everything
// `cargo test` runs, so under `tests/browser_suites.rs` the marker arrives through the spawn and
// this line is a no-op. Typed by hand — `node tests/ui/attach.mjs`, which is how these suites are
// actually driven while somebody is working on one — cargo is not in the picture and NOTHING set
// it. Measured, both ways round: the `bwrap` stand-in and its `exec sleep 600` child carry
// `SKEIN_TEST=1` when the suite process has it and carry no such variable at all when it does not.
//
// Two things follow from that, and the leak check is the smaller one:
//
//   * `harness/leaks.mjs` can then see the one process a namespace fixture is guaranteed to leave
//     behind — the `exec`'d anchor, whose arguments are two words and whose environment never held
//     the fixture root. The marker is the only thing on it that says "test".
//   * `util::in_test` branches on the same variable, so a hand-run suite's server now REFUSES the
//     ambient warden and refuses to fall back to the real `$SKEIN_FLEET_ROOT` — the two guards that
//     exist because a test run once made box directories in the live fleet at `/boxes` (SKEIN-654).
//     `harness/server.mjs` already assumed this was true of every run, in as many words, and was
//     right only about the cargo half. All thirteen `startServer` callers pin `$SKEIN_FLEET_ROOT`
//     and `startServer` refuses without it, so nothing here was relying on the fallback.
//
// Set once, on the suite process, rather than at each `spawn`: every spawn in this tier inherits
// `process.env`, and a marker added site by site is the hand-maintained list that `leaks.mjs`
// exists to not have. `??=` so a caller who pinned it deliberately still wins, exactly as
// `.cargo/config.toml` leaves `force` off for the same reason.
//
// The name is DERIVED, and an unreadable derivation throws here rather than skipping quietly. That
// is deliberate: a suite that silently stopped being a test process is the SKEIN-654 failure, and
// noisy is the safe direction.
process.env[testMarker(root)] ??= "1";

// And which worktree the run belongs to, for `fromWorktree` — the half of the leak check that
// keeps it from going red over another lane's live `cargo test` on this shared box.
//
// **Not a name the check looks up.** `fromWorktree` searches the whole environment for the
// repository root and does not care which variable holds it, so this variable is inert: nothing
// reads it, and renaming it changes nothing. What it does is make the tie RELIABLE instead of
// incidental. Under cargo the root is already there twice over — `$CARGO_MANIFEST_DIR` on every
// test binary cargo runs, and `$CARGO_TARGET_DIR` as `tools/gates.sh` exports it — but a hand-run
// suite has neither, and the only thing left naming the worktree is `$PWD`, which is whatever the
// shell happened to export and is not the run's to depend on. SKEIN-861's seven orphans were from
// hand runs, so the hand-run case is exactly the one that has to work.
process.env.SKEIN_TEST_WORKTREE ??= root;
export const page = readFileSync(join(root, "src", "web", "index.html"), "utf8");

// Brace-matched rather than regex-to-end-of-line, because these span lines; a wrong slice throws
// here rather than silently testing a truncated function.
export function grab(name) {
  // `async function` is listed before `function` on purpose: `indexOf("\nfunction attachFiles(")`
  // simply misses an async declaration, and the miss reads as "did it get renamed?" — which is a
  // confusing thing to be told about a function that is right there.
  for (const start of [`async function ${name}(`, `function ${name}(`, `const ${name} =`, `let ${name} =`]) {
    const at = page.indexOf(`\n${start}`);
    if (at < 0) continue;
    const from = at + 1;
    const isFn = start.endsWith("(");
    let depth = 0, opened = false;
    for (let i = from; i < page.length; i++) {
      const c = page[i];
      if (isFn) {
        // Braces only. Counting the parameter list's parens too would end the function at `)` on
        // its very first line, which is a slice that parses and tests nothing.
        if (c === "{") { depth++; opened = true; }
        else if (c === "}" && --depth === 0 && opened) return page.slice(from, i + 1);
        continue;
      }
      if (c === "{" || c === "[" || c === "(") depth++;
      else if (c === "}" || c === "]" || c === ")") depth--;
      // A declaration ends at the first line break outside any bracket — which is also the right
      // answer for `let a = 0, b = null;`, where there are no brackets to have opened at all.
      else if (c === "\n" && depth === 0) return page.slice(from, i);
    }
  }
  throw new Error(`could not lift \`${name}\` out of index.html — did it get renamed?`);
}

// The page's own `esc`, as a callable — for a world built with `new Function("esc", …)` rather than
// from a template literal that can splice `${grab("esc")}` in directly.
//
// It exists because the alternative was `String`. Eight suites passed `String` for `esc`, which
// returns its argument unchanged, so every assertion those suites made about rendered HTML was an
// assertion about *unescaped* output — and an `esc` that stopped escaping would have left all of
// them green (SKEIN-531). That is not a weak test, it is a test asserting the wrong thing: the
// expectations downstream were written to match the stub.
//
// One definition rather than one per suite for the same reason `grab` is not copied: the whole
// point is that the suites and the browser run the SAME function, and a second spelling of it is a
// second thing to drift.
export const esc = new Function(`${grab("esc")}; return esc;`)();

// **`link` too, with the real `safeHref` behind it** (SKEIN-602).
//
// The page builds every anchor through `link`, which asks `safeHref` whether the scheme is one this
// page will follow and degrades to plain text when it is not. A lifted world that stubbed that
// judgement would be testing a page that cannot exist — and the suites below assert on rendered
// anchors, so the stub would decide their answers. `safeHref` is imported from the cockpit bundle
// rather than re-lifted, because it is the same module the browser loads.
//
// `document.baseURI` is what `safeHref` resolves relative URLs against. A fixed loopback origin is
// right here for the reason the page's own is: what is under test is the SCHEME, and a relative
// href has no scheme to judge.
export const link = new Function(
  "esc",
  "safeHref",
  "document",
  `${grab("link")}; return link;`,
)(esc, safeHref, { baseURI: "http://127.0.0.1/" });

// A cockpit module, as source a lifted world can evaluate.
//
// `grab` lifts a declaration out of the page; this lifts a whole module out of `cockpit/src`, which
// is where the page's PURE functions live. A world whose lifted functions call one — the review
// pane's grouping calls `moveOf`, the row calls `moveWhy` — would otherwise have to stub it, and a
// stub of a rule is the rule written twice: exactly what putting it in `cockpit/src` was for.
//
// The transform is `export ` removed and nothing else, byte for byte what `cockpit/build.mjs` does
// to produce the bundle the browser loads, so a node world and a real page run the same code.
export function pure(name) {
  const text = readFileSync(join(root, "cockpit", "src", `${name}.mjs`), "utf8");
  return text.replace(/^export\s+/gm, "");
}

// Where cargo puts what it builds. `$CARGO_TARGET_DIR` overrides the whole directory — not a
// subdirectory beneath the ordinary `target/` — and it is the normal state on this box and on any
// CI with a cached target dir, not an edge case: hardcoding `<repo>/target` there builds a binary
// and then looks for it somewhere nothing was ever written, and a fixture that scans `<repo>/target`
// for its own leftovers finds a directory that was never created at all (UI-4).
export function targetDir() {
  return process.env.CARGO_TARGET_DIR || join(root, "target");
}

// Where a browser suite puts its throwaway fixture, and it is NOT under `$CARGO_TARGET_DIR`
// (SKEIN-603).
//
// A box's tmux socket is `<root>/fleet/<box>/session.sock` and a unix socket path is limited to
// 108 bytes. Every agent worktree in this fleet lives under
// `/home/agent/.cache/skein/claude/claude-1000/<repo-id>/<uuid>/scratchpad/<name>`, which is ~118
// characters before the fixture appends anything — so the old default could not work in ANY
// worktree, only in a checkout at a short path, and `onboarding.mjs` refused up front with a
// message telling each agent in turn to set `$SKEIN_UI_FIXTURE_ROOT` by hand. A clear failure is
// better than a confusing one, but it is not better than working.
//
// `/var/tmp` for the same reason `Scratch::boxes` uses it (`tests/common/mod.rs`): a box binds its
// own directories over `/tmp` and `$HOME`, so a fleet root beneath either is unreadable from
// outside and `src/box-session.sh` refuses it. `$SKEIN_UI_FIXTURE_ROOT` still overrides.
export function fixtureRoot() {
  return process.env.SKEIN_UI_FIXTURE_ROOT || "/var/tmp/skein-uifix";
}

// A fixture directory stamped with the pid that made it, having first removed the ones whose
// maker is gone. Returns the new directory.
//
// **The sweep has to be keyed on the pid, and an age rule will not do** (SKEIN-590). These suites
// keep their fixture when they fail, deliberately — it is the only evidence a failure leaves — and
// nothing ever removed a kept one: 48 directories and 60 MB were measured on this box, and the
// suite whose failures somebody is working on is exactly the suite that fails repeatedly, so the
// debris grows fastest while it is being looked after. The obvious fix, sweeping everything with
// the right prefix at the start of a run, was tried and reverted: with the root now shared by
// every worktree on the box, that deletes a fixture another agent's run is writing into, which was
// observed and not theorised.
//
// So the question asked here is "is the process that made this still alive", of the operating
// system rather than of a clock. This is the answer `tests/common/mod.rs::sweep_abandoned` already
// reached for the Rust harness, and two different answers to one question is how they drift.
//
// Pid reuse can only make this KEEP a dead run's directory, never remove a live one's — the safe
// direction, and the reason the check is written this way round.
export function freshFixture(dir, prefix) {
  mkdirSync(dir, { recursive: true });
  for (const name of readdirSync(dir)) {
    if (!name.startsWith(`${prefix}-`)) continue;
    const pid = Number(name.slice(prefix.length + 1).split("-")[0]);
    if (!Number.isInteger(pid) || pid <= 0) continue;   // not ours to reason about
    if (alive(pid)) continue;
    try { rmSync(join(dir, name), { recursive: true, force: true }); } catch {}
  }
  return mkdtempSync(join(dir, `${prefix}-${process.pid}-`));
}

// `signal 0` asks the kernel whether the process exists without sending anything. `EPERM` means it
// exists and is somebody else's, which is still alive; only `ESRCH` means gone.
function alive(pid) {
  try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; }
}

// The `skein-server` binary the browser suites drive — one resolver, because the build policy is
// the part that must not drift between them.
//
// Each suite used to run `cargo build --bin skein-server` itself, which under `cargo test` meant a
// second cargo re-taking the build-directory lock and re-walking the dependency graph mid-run — on
// a box where other builds share that lock, an open-ended stall and a burst of load right as the
// suites' own timeouts start ticking. That contention is SKEIN-119: review.mjs failed its
// ownership check about one workspace run in four, and never standalone.
//
// So there are two paths, and both must keep working:
// - Driven from tests/browser_suites.rs, SKEIN_SERVER_BIN names the binary the surrounding
//   `cargo test` has ALREADY built (`CARGO_BIN_EXE_skein-server`), and no cargo runs here at all.
// - Run by hand (`node tests/ui/review.mjs`), the variable is absent and the build happens here,
//   exactly as before — a fresh checkout still needs only the one command, and `cargo build`
//   honours `$CARGO_TARGET_DIR` on its own, so the binary this spawns and the path returned below
//   have to agree about where that build actually landed.
//
// **Both binaries, not just the server.** Launching a box is not something `skein-server` does in
// process: the terminal route runs `sh -c "<skein> start <box> --branch … --attach"` on a PTY
// (src/bin/skein-server.rs, `terminal_session`), and `sandbox::skein_exe` resolves that `<skein>` as
// the sibling of the running `skein-server`, falling back to the bare name `skein` when there is
// none. With only `--bin skein-server` built there was no sibling, the bare name was not on `$PATH`,
// and the PTY died with `skein: not found` — no box, no provisioning, and an EMPTY terminal, because
// everything that reports a launch failure lives inside the binary that never started.
//
// That cost `onboarding.mjs` two checks ("and the box ends up on the board", "provisioning ran in
// the box's home") and cost them *invisibly*: under `cargo test` they pass, because cargo builds
// every bin in the package before it runs an integration test, so `CARGO_BIN_EXE_skein-server` has
// `skein` beside it. Run by hand, the same tree failed. Proven by copying `skein-server` alone into
// an empty directory and pointing `SKEIN_SERVER_BIN` at it: the same two checks fail, with the same
// empty terminal, and nothing else changes.
export function serverBinary() {
  const given = process.env.SKEIN_SERVER_BIN;
  if (given) return given;
  const build = spawnSync("cargo", ["build", "--bin", "skein-server", "--bin", "skein"], { cwd: root, stdio: "inherit" });
  if (build.status !== 0) throw new Error("cargo build failed");
  return join(targetDir(), "debug", "skein-server");
}

// The socket a suite's `skein-server` is served on — opened here and handed to the child, never
// closed while nobody owns it.
//
// Six suites used to pick a port like this instead:
//
//     const s = createServer();
//     s.listen(0, "127.0.0.1", () => { const { port } = s.address(); s.close(() => res(port)); });
//
// Bind :0, read the number the kernel chose, CLOSE, and hand the bare number to a server that binds
// it a moment later. In the gap the port belongs to nobody, and the kernel is free to hand the same
// number to the next caller that asks for one. That gap cost nothing while `tests/browser_suites.rs`
// ran the suites one after another; `lanes()` (tests/browser_suites.rs:112, added by fd3208e) now
// runs several at once, so two of them can be given one number and whichever binds second dies of
// EADDRINUSE (SKEIN-443).
//
// A retry would only make the window smaller. This closes it: the listening socket is opened once
// and *inherited* by the server rather than re-bound by it, so the port passes from this process to
// the child without ever being unbound — there is no instant at which a second lane could be given
// it. `skein-server` already takes a socket it was handed, because in the fleet a box that binds
// the cockpit's port before skein does BECOMES the cockpit (architecture §9.4): `doorway::inherited`,
// src/doorway.rs:152. `src/server-doorway.py` is the other producer of the same handover.
//
// `LISTEN_FDS` at descriptor 3 is systemd's socket-activation convention, which is why it is spelled
// this way. `LISTEN_PID` is deliberately absent: it names the process the descriptors are meant for,
// and node cannot know the child's pid until `spawn` has already returned it. A descriptor passed
// with no `LISTEN_PID` at all is the older half of the convention and is accepted —
// `doorway::tests::one_descriptor_is_the_one_the_convention_names` asserts exactly that case.
//
// Measured rather than argued: with the server holding the inherited socket, a second `listen` on
// the same port answers EADDRINUSE. That is the named check in `attach.mjs`.
export function openDoor() {
  return new Promise((resolve, reject) => {
    const door = createServer();
    door.once("error", reject);
    door.listen(0, "127.0.0.1", () => {
      const { port } = door.address();
      // Private, and checked rather than assumed: node exposes no supported way to read a
      // listener's descriptor, and `stdio: [..., undefined]` would be spawned as a pipe — a server
      // that then found no socket at 3 and bound one instead, which is the bug back again wearing
      // a costume.
      const fd = door._handle?.fd;
      if (typeof fd !== "number" || fd < 0) {
        door.close();
        reject(new Error(`node ${process.version} does not expose the listener's descriptor \
(server._handle.fd is ${fd}), so the port cannot be handed over and would have to be re-bound`));
        return;
      }
      resolve({
        port,
        // What `spawn` needs, so a suite states the convention once instead of six times. The
        // descriptor lands at 3 in the child because it is at index 3 of `stdio`; `LISTEN_FDS`
        // says one socket was passed.
        stdio: ["ignore", "pipe", "pipe", fd],
        env: { LISTEN_FDS: "1" },
        // Called AFTER `spawn` returns, which is after the child exists with its own copy of the
        // descriptor — `spawn` forks and execs before it returns the pid. Closing it matters for a
        // reason beyond tidiness: the two processes share one socket, so a listener left open here
        // would accept some of the connections meant for the server.
        close: () => door.close(),
      });
    });
  });
}

// **`draftRules()` is gone, and nothing replaced it.**
//
// It lifted a CLUSTER — `revDraftHeld`, `revDraftPosted`, `revDraftEchoes`, `revSentSince`,
// `revDraftAtHead`, `revDraftIsOlder`, `revDraftedPostedOf`, `revDraftVintageHtml` — because half
// the suites drew a row that pulled the whole lot in transitively, and "listing them by hand in
// each suite is how five of them broke at once the first time a function was added".
//
// Every one of them is gone, and so is the one that outlived them by an hour (`revViewerOf`, whose
// last caller went with the cluster). skein no longer keeps a drafted review for a reader to vet
// and post — the session posts its own to GitHub — so nothing on this page draws one.

// The tiny assert harness both suites share.
export function harness() {
  let failures = 0;
  return {
    check(what, got, want) {
      const ok = JSON.stringify(got) === JSON.stringify(want);
      if (!ok) {
        failures++;
        console.error(`✗ ${what}\n   got  ${JSON.stringify(got)}\n   want ${JSON.stringify(want)}`);
      } else console.log(`✓ ${what}`);
    },
    done() {
      console.log(failures ? `\n${failures} failed` : "\nall good");
      process.exit(failures ? 1 : 0);
    },
  };
}

// **A real namespace for a fixture's box to be entered through** — and every browser suite that
// reads a box from the inside needs one now.
//
// A fake `sbx` on `$PATH` used to be the box: skein's every crossing went through `sbx exec`, so a
// script there could run the payload and the namespace was never entered. There is no hop
// (SKEIN-576) — `place::Place` builds `bash -c <anchor check> … exec nsenter …` and runs it here —
// so a fixture whose placement record is unstamped is refused at the crossing's own guard, and one
// whose `ns_pid` names no namespace fails at `nsenter`. Both look like "the box is not answering",
// which is exactly what a reader sees: the Files pane falls back to the host clone and says so,
// correctly and unhelpfully.
//
// bwrap supplies the namespace, the way `place`'s own crossing test does. `--dev-bind / /` gives the
// box the same filesystem this process sees, so what a suite writes through the crossing lands where
// its assertions read it — which is the whole point of driving the real path rather than a stand-in.
//
// `bindOver` binds a directory over another *for the box alone*, which is how a suite puts a shim on
// the box's `$PATH` without putting it on the server's.
//
// Returns the fields a placement record needs, stamped: `nsenter` is only reached once the crossing
// has proved the record still names this namespace, so an unstamped fixture tests the refusal.
export async function boxlikeNamespace(root, bindOver) {
  const { spawn } = await import("node:child_process");
  const fs = await import("node:fs");
  const { join } = await import("node:path");
  const anchorAt = join(root, "anchor");
  const errAt = join(root, "bwrap.err");
  const binds = bindOver ? ["--bind", bindOver.from, bindOver.to] : [];
  const child = spawn(
    "bwrap",
    ["--dev-bind", "/", "/", ...binds, "--",
     "bash", "-c", `echo $$ > ${anchorAt}; exec sleep 600`],
    {
      // stdout nulled: a child that outlives this holds an inherited pipe open, and the suite then
      // looks like a hang long after it finished. stderr to a FILE for the same reason inverted — it
      // holds no pipe open, and it is the only place bwrap's own refusal (userns, apparmor) is
      // recorded. Nulling it is how a denied namespace becomes a silent mystery.
      stdio: ["ignore", "ignore", fs.openSync(errAt, "w")],
      // **Which fixture this namespace belongs to, said in the one place the `exec` below cannot
      // take it from** (SKEIN-980). The anchor's argv becomes the two words `sleep 600` — the
      // fixture root lived in the `bash -c` script and the script goes with the image — so until
      // this line the only thing on it that said anything was `$SKEIN_TEST`, and the row a reader
      // was handed named no fixture, no suite and no run. Measured on this branch before the
      // change: the marker half called it `orphans`, the prefix half found it in none of its four
      // lists, and the gate's row read `environment SKEIN_TEST  sleep 600`.
      //
      // The name of the variable is inert, exactly as `SKEIN_TEST_WORKTREE` above is: the scan
      // searches the whole environment for a derived prefix and does not care which variable holds
      // one. What it buys is not one more witness but a COHORT — `fixturesRunning` groups by the
      // fixture directory a process names, and a process that names none can be in no cohort, so it
      // is judged on parentage alone, which is what SKEIN-990 proved is not enough. With the root
      // here, the anchor is a run in flight while its suite is going and this run's leak once
      // nothing of that fixture is left, which is the verdict every other process of the fixture
      // already gets.
      //
      // EXPORTED rather than interpolated into the script, and that distinction is SKEIN-861's:
      // `${anchorAt}` is in the arguments of a `bash` that is about to replace itself, and an
      // interpolated path is gone the moment it does. `exec` does not clear an environment.
      env: { ...process.env, SKEIN_TEST_FIXTURE: root },
    },
  );
  let anchor = null;
  for (let i = 0; i < 200 && anchor === null; i++) {
    try { const said = fs.readFileSync(anchorAt, "utf8").trim(); if (said) anchor = Number(said); }
    catch { /* not written yet */ }
    if (anchor === null) await new Promise(r => setTimeout(r, 50));
  }
  if (!anchor) {
    child.kill("SIGKILL");
    const said = fs.existsSync(errAt) ? fs.readFileSync(errAt, "utf8").trim() : "";
    throw new Error(`the box-like namespace never reported its anchor; bwrap said: ${said}`);
  }
  // Read exactly as `place::anchor_probe` reads it: `starttime` is field 22 of `/proc/<pid>/stat`,
  // taken after the LAST `)` because a process's comm can itself contain parens. A record that
  // parsed it differently would be refused at the crossing's guard, which is the failure this
  // helper exists to stop a fixture from testing by accident.
  const stat = fs.readFileSync(`/proc/${anchor}/stat`, "utf8");
  const ns_start = Number(stat.slice(stat.lastIndexOf(") ") + 2).trim().split(/\s+/)[19]);

  // **The anchor outlives the suite, and every caller was killing the wrong process** (SKEIN-861).
  // There is no `--unshare-pid` above, so the anchor is in this process's PID namespace, and `exec`
  // means the anchor pid IS the `sleep`. A caller's `fx.boxlike.kill("SIGKILL")` kills the BWRAP
  // process and leaves the sleep behind, reparented to pid 1 — one per run, on success as well as
  // failure. Measured: seven `attach.mjs` runs, seven orphans, the oldest nine minutes old.
  //
  // The anchor goes first and bwrap second, not the other way round, because that ordering is the
  // bug: bwrap waits on its child, so killing the child is what lets bwrap exit, while killing
  // bwrap first is precisely what orphans the child.
  //
  // **`ns_start` is a pid-reuse guard and not decoration.** By the time an exit handler runs, the
  // anchor may have gone on its own and its pid been reissued to somebody else's process — this box
  // is shared, and `stopRun` exists because a `pkill -f tmux` here once reaped 72 servers whose
  // owners could not afterwards be named. `starttime` tells the anchor apart from a stranger
  // wearing its pid, and it is read exactly as `place::anchor_probe` reads it.
  const stop = () => {
    try {
      const now = fs.readFileSync(`/proc/${anchor}/stat`, "utf8");
      const started = Number(now.slice(now.lastIndexOf(") ") + 2).trim().split(/\s+/)[19]);
      if (started === ns_start) process.kill(anchor, "SIGKILL");
    } catch {
      /* gone already, which is the outcome this is for */
    }
    try {
      child.kill("SIGKILL");
    } catch {
      /* likewise */
    }
  };
  // Registered rather than left to the caller, and `quiesceOnExit` rather than a line at the end of
  // a suite, for the reason that function was written: `srv.kill()` after the last check is skipped
  // by a throw and by a Ctrl-C, and a Ctrl-C is the path a person actually takes when a browser
  // suite hangs. No scopes — this owns two pids, not a fixture directory.
  quiesceOnExit([], stop);
  return {
    child,
    stop,
    ns_pid: anchor,
    ns_start,
    generation: fs.readFileSync("/proc/sys/kernel/random/boot_id", "utf8").trim(),
  };
}
