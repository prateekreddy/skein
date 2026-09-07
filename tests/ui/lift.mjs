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

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
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
