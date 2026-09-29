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
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { serverBinary } from "../lift.mjs";
import { fixtureScopes, quiesceOnExit } from "./leaks.mjs";

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
 * The file a fixture home is stamped with, so a box seeded from one can be told from a box seeded
 * from a real one.
 *
 * Absence is not the same evidence. `src/box-session.sh` skips a seed path it cannot find
 * (`[ -e "$HOME/$rel" ] || continue`), so an empty home leaves the box with nothing seeded at all —
 * which is indistinguishable from a launch that never reached the seed loop, and would let the
 * check in `onboarding.mjs` pass for a box that never started. A file that could only have come
 * from here makes the copy itself observable.
 *
 * Written twice, because the two copies now answer opposite questions (SKEIN-1235). The one in
 * `.profile`, which the launcher still seeds whole, must arrive: it is the evidence that the box
 * was seeded from this home. The one in `.claude` must NOT: a new box's `~/.claude` holds the login
 * and skein's own defaults, and nothing else of the home it was started from.
 */
export const FIXTURE_HOME_SENTINEL = "seeded-from-a-ui-fixture-home";

/** The deepest directory that contains both paths. */
function commonAncestor(a, b) {
  const x = path.resolve(a).split("/");
  const y = path.resolve(b).split("/");
  let i = 0;
  while (i < x.length && i < y.length && x[i] === y[i]) i++;
  return x.slice(0, i).join("/") || "/";
}

/**
 * The `HOME` this suite's server runs on: a directory inside the suite's own fixture, holding a
 * `.claude` with [`FIXTURE_HOME_SENTINEL`] in it, a `.profile` naming it, and nothing else.
 *
 * **Unpinned, `$HOME` is the home of whoever ran the suite, and a box start copies it whole**
 * (SKEIN-657). `src/box-session.sh` seeded each box from `$HOME` — `.claude` and `.claude.json`
 * until SKEIN-1235, and still `.codex`, `.gitconfig`, `.bashrc`, `.profile` — with
 * `cp -a "$HOME/$rel" "$mine" 2>/dev/null || { …; exit 1; }`, so the copy is not merely wasteful,
 * it is *fatal to the launch* when it returns non-zero. Measured while writing this: `du -sh
 * ~/.claude` on the box this was fixed on said 463 MB, and the copy of it that landed in one
 * `onboarding.mjs` box summed to 477.9 MB — per box, and `review.mjs` starts four. The rest is
 * SKEIN-657's diagnosis rather than this file's: that directory loses files while it is being
 * read (backup rotation and Claude Code's write-to-temp-then-rename of its session keys, five in
 * 120 seconds on a quiet machine), `cp -a` returns 1 when a source file disappears between
 * readdir and stat, and a paired-arm run with the churn moved outside `$HOME/.claude` and the
 * load otherwise identical was green 3/3 where it had been red 3/3.
 *
 * **And the copy is the smaller half.** With `$HOME` unpinned, `box-session.sh` binds
 * `~/.local`, `~/.cargo`, `~/.rustup` and `~/.npm` read-WRITE into the box it starts, binds
 * `~/.claude/sessions` read-write unless `$SKEIN_BOX_PEERS` says otherwise, and reconciles
 * credentials back into `~/.claude/.credentials.json` — all of them the runner's own, in a test
 * (SKEIN-681). `tests/fleet_launch/` reached this conclusion first and says it plainest: "`sbx
 * exec` here means 'run it on this machine', so a box placed over the real `$HOME` is a box
 * driving the developer's own home directory".
 *
 * **Derived from the two pins rather than taken as an argument**, for the reason this file already
 * gives about the credential: what a suite has to remember, a new suite forgets, and the direction
 * it forgets in is silent. Every suite that starts a server already says where its fixture is,
 * twice — `SKEIN_HOME` for the store and `SKEIN_FLEET_ROOT` for the fleet — and in every one of
 * them those are siblings inside one `mkdtempSync` directory. Their common ancestor is that
 * directory; the home goes beside them, which also means it can never contain the fleet root, and
 * so can never trip `box-session.sh`'s refusal of a box root under `$HOME`.
 *
 * **It refuses rather than guessing.** A suite that pins neither, or one whose two pins share only
 * `/` or `/var/tmp`, would otherwise be handed a home shared with every other suite on the box, or
 * the runner's own — which is the bug, arrived at through the code that exists to prevent it. The
 * same shape as `leaks.mjs` refusing to run when it derives no fixture names.
 *
 * Nothing else is seeded, deliberately. A box's git identity is written by provisioning rather
 * than read from a seeded file (`src/fleet.rs`: `git config --global --get user.name … ||
 * git config --global user.name …`), and `.bashrc`/`.profile`/`.codex` decide nothing any suite
 * asks about — so seeding them would be asserting that they matter.
 */
function fixtureBase(env) {
  const store = env.SKEIN_HOME;
  const fleet = env.SKEIN_FLEET_ROOT;
  const refuse = why => {
    throw new Error(
      `startServer cannot place a fixture HOME for this suite: ${why}. Pin SKEIN_HOME and ` +
        "SKEIN_FLEET_ROOT at two siblings inside the suite's own fixture directory — without them " +
        "the server would seed its boxes from, and bind ~/.local and ~/.cargo read-write out of, " +
        "the home of whoever ran the suite (SKEIN-657, SKEIN-681)",
    );
  };
  if (!store || !path.isAbsolute(store)) refuse("SKEIN_HOME is not an absolute path");
  if (!fleet || !path.isAbsolute(fleet)) refuse("SKEIN_FLEET_ROOT is not an absolute path");
  const base = commonAncestor(store, fleet);
  if (base.split("/").filter(Boolean).length < 2) {
    refuse(`SKEIN_HOME and SKEIN_FLEET_ROOT share only ${base}, which is not a fixture directory`);
  }
  if (base === path.resolve(store) || base === path.resolve(fleet)) {
    refuse("SKEIN_FLEET_ROOT and SKEIN_HOME are nested rather than siblings, so there is no " +
      "directory beside both of them to put a home in");
  }
  return base;
}

/** [`fixtureBase`]'s home: `server-home` inside the fixture, stamped with the sentinel. */
function fixtureHome(base) {
  const home = path.join(base, "server-home");
  fs.mkdirSync(path.join(home, ".claude"), { recursive: true });
  fs.writeFileSync(
    path.join(home, ".claude", FIXTURE_HOME_SENTINEL),
    "Written by tests/ui/harness/server.mjs. A box carrying this file was seeded from a UI\n" +
      "fixture's home rather than from the home of whoever ran the suite.\n",
  );
  fs.writeFileSync(
    path.join(home, ".profile"),
    `# ${FIXTURE_HOME_SENTINEL}: written by tests/ui/harness/server.mjs, and seeded into every box\n`,
  );
  return home;
}

/**
 * The file name of the agent CLI every suite's server is handed unless the suite names its own.
 *
 * **Unset, `$SKEIN_CLAUDE_BIN` means `claude` off `$PATH`** (SKEIN-1092) — the owner's real CLI on
 * the owner's real login. `ai::agent_command` refuses that in a test process, and it fired in a
 * suite: `onboarding.mjs`'s server panicked on a background thread with "$SKEIN_CLAUDE_BIN is unset
 * in a test process". The call was not a model call at all but `ai::ask_model_choices`, the
 * `claude --help` that `ai::model_choices` starts behind the caller the first time a health report
 * is built — so EVERY suite whose page loads `/api/health` reached it, and it looked like a load
 * flake only because a suite prints its server's log when it fails and at no other time. Run by
 * hand, with no `$SKEIN_TEST`, the guard does not fire and the real CLI is spawned instead.
 *
 * A stub that answers nothing and exits 1, because the only thing any suite that does not name its
 * own asks of the agent is that it not be the real one: a failed model call is the ordinary "fall
 * back to the free deterministic path" to every caller in `src/ai/`. Written rather than
 * `/bin/false` so a server log or `/proc/<pid>/environ` names where it came from, and so it can leave
 * `<stub>.asked` behind — the one way to see from outside that the server reached it.
 */
export const FIXTURE_AGENT_STUB = "agent-stub-from-a-ui-fixture";

/** Write [`FIXTURE_AGENT_STUB`] into the fixture and return its path. */
function fixtureAgentStub(base) {
  const stub = path.join(base, FIXTURE_AGENT_STUB);
  fs.writeFileSync(
    stub,
    "#!/bin/sh\n# Written by tests/ui/harness/server.mjs: the agent CLI a suite's server is handed\n" +
      "# when the suite names none. It runs nothing and says so.\n" +
      `echo "${FIXTURE_AGENT_STUB}: this suite's server asked the agent CLI; nothing ran" >&2\n` +
      // What it was asked, beside it: the evidence `hatches.mjs` check 5 reads that it was the
      // stub the server reached, rather than nothing at all.
      'echo "$*" >> "$0.asked"\n' +
      "exit 1\n",
    { mode: 0o755 },
  );
  return stub;
}

/**
 * The directory every bare `tmux` a suite's server runs puts its default socket in — the fixture's
 * own, and short, because a unix socket path cannot exceed 108 bytes and tmux appends
 * `tmux-<uid>/default` to it (`updatestall.mjs` measured the same limit and chose the same name).
 */
function fixtureTmuxDir(base) {
  const dir = path.join(base, "t");
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  return dir;
}

/**
 * Start a `skein-server` on `door` and wait until it answers.
 *
 * `door` is the open listening socket from `openDoor` — not a port number. `env` is what this
 * suite's fixture needs on top of the caller's own environment; `token` is the fixture's API token,
 * omitted by a suite that runs with `SKEIN_NO_API_AUTH`. Resolves `{srv, log}`, where `log()` is
 * everything the server has said on stdout and stderr so far.
 *
 * **`SKEIN_IN_FLEET` was stripped here too, and is not any more** (SKEIN-643). It is set in every
 * skein box, which is where the work on this project happens — so a suite run by hand inside a box
 * started a server that believed it was the fleet's own cockpit. `sbx ls` was then not asked at all
 * ("which boxes exist is read from their placement records instead"), and `onboarding.mjs`'s check
 * that health reports sbx as `satisfied` failed on a machine where sbx answers perfectly well. It
 * read as a FLAKE rather than as a bug — the same tree, two answers, decided by an ambient variable
 * neither run mentions.
 *
 * SKEIN-521 then deleted the deployment that variable chose between, and nothing has read it since.
 * The strip went with it rather than being kept as insurance: a fixture that removes a value no
 * code consults asserts that it decides something, and if the name were ever given a real meaning
 * again, a silent strip is how these suites would go on disagreeing with production. What survives
 * the variable is the lesson — **a fixture that pins two of the three things its question depends
 * on has pinned two thirds of it**, which is why `SKEIN_HOME` and `SKEIN_FLEET_ROOT` are a suite's
 * own business and the credential below is this file's.
 *
 * **The GitHub credential is pinned to [`FIXTURE_GH_TOKEN`], and it cost a red master to learn**
 * (SKEIN-621). `prq::credentials::look_for_a_credential` reads `$GH_TOKEN`, then
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
 * **The hatch and the pin are both checked by `tests/ui/hatches.mjs`** — because the order they
 * depend on is invisible where it matters. The pin is an assignment here and the escape is the
 * `...env` spread ten lines below; a `childEnv.GH_TOKEN = …` written after the spread instead of
 * before turns the documented hatch into a no-op, and it breaks in exactly one direction: the suite
 * asking for the no-credential case gets a credential and goes green (SKEIN-624).
 *
 * serverBinary() only builds when run by hand; under `cargo test` the binary arrives pre-built via
 * SKEIN_SERVER_BIN, because a nested cargo fighting the outer one for the build lock is the load
 * that made the review suite flake (SKEIN-119 — the story is on `serverBinary` in lift.mjs).
 *
 * The port arrives as an OPEN listening socket rather than a number — `openDoor` in lift.mjs says
 * why (SKEIN-443). `door.stdio` puts that descriptor at 3 in the child and `door.env` says one was
 * passed; no `SKEIN_ADDR` goes with it, because a server handed a socket reports where the socket is
 * bound instead of binding anywhere of its own (src/bin/skein-server/main.rs:554).
 *
 * **Everything this run starts is stopped on the way out, and `srv.kill()` was never that**
 * (SKEIN-645). Two gaps, and each one alone is enough to leak:
 *
 * - `srv.kill()` sits at the top level of a suite, after the last check. A throw before it — a
 *   browser that will not launch, a `page.goto` that times out, a `mustSee` outside a `check` — or
 *   a Ctrl-C skips it entirely, and node runs no `exit` handler for SIGINT at all.
 * - It would not be enough if it always ran. The tmux server is **not** `skein-server`'s child:
 *   the server starts a detached session at `<fleet>/.skein/server.tmux` (`fleet::server_tmux_sock`)
 *   whose window is `fleet::supervised`'s `while [ -f <doorway> ]; do … done`, and that loop
 *   outlives whatever started it. Measured: a *passing* `smoke.mjs` left three processes behind —
 *   the tmux server, the loop, and the python it had just respawned.
 *
 * Those three do stop when the fixture is deleted, because deleting it takes the loop's condition
 * with it. That is precisely why the leak was invisible until it mattered: a suite keeps its
 * fixture when it FAILS, deliberately (SKEIN-590), so the loop's condition survives and the
 * supervisor restarts a python every two seconds for ever. Four tmux servers between two and nine
 * hours old were counted that way, and 122 processes older than half an hour on one box.
 *
 * So the fixture directory is left exactly as the suite wants it and the processes go regardless —
 * the same division `Scratch` makes in `tests/common/mod.rs`, where `quiesce` runs on every path
 * and only the removal is conditional. `fixtureScopes` says why the kill cannot reach another
 * agent's run.
 */
export async function startServer({ door, env = {}, token = "", cwd = REPO, tries = 100, program = null }) {
  const { port } = door;
  // First, so a suite that pinned neither path is refused before anything is written or started.
  const base = fixtureBase(env);
  // Built rather than spread inline, so the replacement below is visible at the spawn. A suite that
  // wants a different credential passes one in its own `env`, which still wins — this drops only
  // what was inherited from whoever typed the command.
  const childEnv = { ...process.env, ...door.env };
  // `$GITHUB_TOKEN` goes and `$GH_TOKEN` is replaced, in that order, because the reader takes the
  // first of the two that is set: leaving `$GITHUB_TOKEN` behind would put the caller's own
  // credential back the moment a suite asked for the no-token case with `GH_TOKEN: ""`.
  delete childEnv.GITHUB_TOKEN;
  childEnv.GH_TOKEN = FIXTURE_GH_TOKEN;
  // The warden, at an address where nothing listens. The server asks one at boot, and it carries
  // `$SKEIN_TEST` from cargo's `[env]` table into here — so `warden_client` refuses it the default
  // rather than letting it ask whatever warden the machine running the suite can reach, which is
  // the owner's on any machine running one (SKEIN-762). Port 1 on loopback is refused by the kernel
  // before a packet leaves the machine.
  //
  // Before the spread and therefore overridable, for `$GH_TOKEN`'s reason: a suite that stands up
  // its own fake warden says so in its own `env`, and what is being ruled out is the AMBIENT one.
  childEnv.SKEIN_WARDEN = "127.0.0.1:1";
  // **`$SKEIN_LISTEN_INHERITED_ONLY` goes, and it is the pin the block above says to write when a
  // name is given a real meaning again** (SKEIN-962). It now has one: `apiauth` reads it to decide
  // where `$SKEIN_NO_API_AUTH` is refused, so a server carrying it and the switch serves nothing
  // but the refusal.
  //
  // And it is ambient in a box, for the same reason `$SKEIN_IN_FLEET` was: `src/server-doorway.py`
  // sets it on the cockpit it execs, a box session is started by that cockpit, and the environment
  // comes down with it — `env | grep SKEIN_LISTEN` in a box in this fleet answers `=1`. So a suite
  // run by hand in a box started a server that believed it was the fleet's own cockpit, which is
  // SKEIN-643's story repeated with a live variable: `attach.mjs` runs with `SKEIN_NO_API_AUTH` and
  // died on `server never came up`, on a machine where nothing was wrong with the server.
  //
  // Before the spread, like the warden: what is ruled out is the AMBIENT value, and a suite that
  // means to say it is the fleet's cockpit still can. `hatches.mjs` checks both halves.
  delete childEnv.SKEIN_LISTEN_INHERITED_ONLY;
  // **And `$SKEIN_IN_BOX`, for the same reason and on purpose this time** (SKEIN-1086). The box
  // launcher exports it into every box so a `skein-server` a box starts refuses
  // `$SKEIN_NO_API_AUTH`; a suite run by hand in a box would otherwise get exactly that refusal.
  // Before the spread for the same reason as the line above, and `hatches.mjs` check 4 asserts both.
  delete childEnv.SKEIN_IN_BOX;
  // **The agent CLI is the fixture's stub unless the suite names its own** (SKEIN-1092) — see
  // [`FIXTURE_AGENT_STUB`] for the call that reached the real one. Before the spread, because a
  // suite that asserts on what the agent said (`review.mjs`, `actfail.mjs`, `connections.mjs`)
  // names a stub of its own and has to win; what is ruled out is the AMBIENT absence. The refusal
  // after the spread is the other half: a suite that says `SKEIN_CLAUDE_BIN: ""` has asked for
  // exactly the unset state, and gets a sentence rather than a server. `hatches.mjs` checks both.
  childEnv.SKEIN_CLAUDE_BIN = fixtureAgentStub(base);
  // **And the runner's tmux goes** (SKEIN-1091). A suite run from a tmux pane — which is every run
  // in a skein box — carries `$TMUX`, and tmux obeys it over its default socket, so a bare `tmux`
  // the server runs (`fleet::detached_alive`'s `tmux has-session -t skein-update`, asked by
  // `update::settle` on every `/api/update`) was a question put to the runner's own tmux server,
  // and `update::cancel`'s `kill-session` would have been an order to it. `$TMUX_PANE` goes with it
  // because it names a pane there. `$TMUX_TMPDIR` is pinned at the fixture so the default socket a
  // bare `tmux` falls back to is one only this suite's server can have made. Before the spread, as
  // the lines above are: `updatestall.mjs` names a tmux directory of its own and has to win.
  delete childEnv.TMUX;
  delete childEnv.TMUX_PANE;
  childEnv.TMUX_TMPDIR = fixtureTmuxDir(base);
  const spawnEnv = { ...childEnv, ...env };
  if (!spawnEnv.SKEIN_CLAUDE_BIN) {
    throw new Error(
      "startServer will not start a server with $SKEIN_CLAUDE_BIN empty: that is `claude` off $PATH, " +
        "the real agent CLI on the real login of whoever ran the suite (SKEIN-1092). Name a stub, or " +
        "say nothing and the harness names one",
    );
  }
  // **After the spread, and the credential above is before it on purpose** — the two pins want
  // opposite things from a suite. A suite has a real reason to want no GitHub credential, so that
  // one is overridable and `hatches.mjs` checks the hatch opens. There is no such reason here: the
  // only `HOME` a suite could ask for instead is a real one, and asking for a real one IS the bug
  // (SKEIN-657, SKEIN-681). So this wins over whatever the suite said, and `hatches.mjs` checks it
  // by asking for the runner's home and being given the fixture's anyway.
  //
  // **On the child and not on this process**, which is why it is pinned here rather than by each
  // suite. Playwright resolves its browser from the SUITE process's `$HOME/.cache/ms-playwright`,
  // so a suite that replaced its own `HOME` dies at `browserType.launch: Executable doesn't exist
  // at <fixture>/.cache/ms-playwright/…` — observed, not guessed. The server is the process that
  // starts boxes, so the server is where the home belongs.
  spawnEnv.HOME = fixtureHome(base);
  // `program` is an argv to run in the server's place — `restart.mjs` runs the doorway, which then
  // runs the server — and it gets every pin above, because what it starts inherits them.
  const [bin, ...args] = program || [serverBinary()];
  const srv = spawn(bin, args, {
    cwd,
    stdio: door.stdio,
    env: spawnEnv,
  });
  // Our copy of the door goes now the child holds its own. Between the two the port was never
  // unbound, so no second lane could have been handed it.
  door.close();
  // Registered here rather than after the poll below, because the poll is where a suite most often
  // dies: `startServer` throws "server never came up" having already started a server that got far
  // enough to open its tmux session, and the caller's `srv.kill()` line is never reached.
  //
  // `env` and not `process.env`: the scope has to be this suite's own fixture paths, and the
  // ambient environment on this box carries `/tmp` paths belonging to everything else running here.
  quiesceOnExit(fixtureScopes(env), () => srv.kill());
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
