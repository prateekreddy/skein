// Does the harness's own escape hatch work — and does the pin it escapes still hold?
//
// `harness/server.mjs` pins two things into every suite's server, and both cost something to learn.
// It replaces `$GH_TOKEN` with `FIXTURE_GH_TOKEN` after deleting `$GITHUB_TOKEN`, which cost a red
// master (SKEIN-621): the queue suites had been running on the developer's own GitHub credential,
// and the first time CI ran them on a runner with none, `actfail`, `connections` and `review`
// failed 16 of 25, 4 of 8 and 62 of 82 checks. And it replaces `$HOME` with a directory inside the
// suite's own fixture, which cost a six-lane flake and a standing hole in the isolation
// (SKEIN-657, SKEIN-681) — the story is on check 3.
//
// Its doc block then promises an escape, in as many words — *"a suite that wants the no-credential
// case says `GH_TOKEN: \"\"` in its own `env`"*. That is true, and it is true only because of an
// ORDER that is invisible at the place it matters: the pin is an assignment near the top of
// `startServer` and the escape is `env: { ...childEnv, ...env }` ten lines below, where the suite's
// own object is spread last. One line moved, or a `childEnv.GH_TOKEN = …` written after the spread
// instead of before, and the hatch is a no-op.
//
// It would break in exactly one direction. A suite asking for the no-credential case would quietly
// be given a credential and go green while asserting nothing — which is SKEIN-621 again, with the
// test that exists to catch it as the thing that hides it. So both facts are checked here: the pin
// holds, and the hatch opens (SKEIN-624).
//
// **This suite is about the harness, so it sets the ambient credential the harness is defending
// against** rather than hoping the machine has one. A GitHub runner exports no `$GH_TOKEN`, so a
// check that trusted the ambient value would assert nothing THERE while looking green here — the
// same box-versus-runner split that made SKEIN-621 invisible for months.
//
//   node tests/ui/hatches.mjs
//
// Needs node and nothing else — no chromium — so it is in the node tier and runs on every
// `cargo test`.
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, harness, openDoor } from "./lift.mjs";
import { spawnSync } from "node:child_process";
import { FIXTURE_AGENT_STUB, FIXTURE_GH_TOKEN, FIXTURE_HOME_SENTINEL, startServer } from "./harness/server.mjs";
import { stub } from "./harness/github.mjs";

const API_TOKEN = "h".repeat(64);

// What a skein box's shell really has in it, and the two spellings of the credential — because
// `look_for_a_credential` (src/prq/credentials.rs) takes the FIRST of the two that is set, so a
// `$GITHUB_TOKEN` left behind puts the caller's own credential back the moment a suite asks for the
// no-token case. Set here rather than read from the machine, so this suite asks the same question
// on a runner as it does in a box.
const DEV_GH_TOKEN = "gho_the_developers_own_credential";
const DEV_GITHUB_TOKEN = "ghp_the_developers_other_credential";
process.env.GH_TOKEN = DEV_GH_TOKEN;
process.env.GITHUB_TOKEN = DEV_GITHUB_TOKEN;
// And the variable the cockpit leaves in every box's environment (SKEIN-962) — see check 4. Set
// here for the credentials' reason: a GitHub runner has never been under a doorway, so a check that
// trusted the ambient value would assert nothing there while looking green in a box.
process.env.SKEIN_LISTEN_INHERITED_ONLY = "1";
// And the marker the box launcher exports into every box (SKEIN-1086), for the same reason.
process.env.SKEIN_IN_BOX = "1";
// And the two absences and one presence check 5 and check 6 are about. `$SKEIN_TEST` because cargo
// sets it on every `cargo test` and `ai::agent_command` refuses the unset agent only when it is
// there, so by hand this suite would otherwise ask a different question than under cargo — and
// spawn the real CLI while asking it. `$SKEIN_CLAUDE_BIN` removed because a shell that happened to
// export one would make check 5 pass without the harness doing anything. `$TMUX` and `$TMUX_PANE`
// set, at a tmux this suite starts itself, because a runner on GitHub is in no tmux at all.
process.env.SKEIN_TEST = "1";
delete process.env.SKEIN_CLAUDE_BIN;

const t = harness();

/** A registry with one GitHub repo in it, and a `gh` that has nothing.
 *
 * **The stub `gh` is what makes check 2 mean the same thing on every machine.** `GH_TOKEN: ""` on
 * its own does not produce the no-credential case: `$GH_TOKEN` is the first of FOUR sources in
 * `look_for_a_credential` (src/prq/credentials.rs), and it falls through to the stored read token,
 * then any write PAT, then `gh auth token` — asked as `Command::new("gh")`, so from `$PATH`
 * (`gh_cli_token`, src/repos/add.rs). The fresh `$SKEIN_HOME` empties the two stored ones; this
 * empties the last.
 *
 * Proved rather than assumed, in both directions. Removing this stub does NOT turn check 2 red on a
 * skein box — `gh` here only echoes `$GH_TOKEN` and answers "no oauth token found" once it is empty,
 * so what a box's `gh auth token` prints is the sandbox proxy's manufactured value rather than a
 * login of its own, and it goes when `$GH_TOKEN` does. Making the stub print
 * a token instead — a machine where somebody really has run `gh auth login` — turns check 2 red at
 * once: the server is handed that credential, asks GitHub, and the no-credential case is not being
 * tested at all. That is the whole failure mode of this item arriving through the one door
 * `GH_TOKEN: ""` does not close, so the check closes it here instead of depending on whose machine
 * it runs on. */
function fixture() {
  const root = freshFixture(fixtureRoot(), "skein-hatches-ui");
  const home = path.join(root, "home");
  const bin = path.join(root, "bin");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(bin, { recursive: true });
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  fs.writeFileSync(path.join(home, "config.json"), "{}");
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  // `review_queue` is absent on purpose: its serde default is true, which is the state a repo added
  // through the cockpit is in, and it is what makes the queue ask GitHub at all.
  fs.writeFileSync(path.join(home, "repos.json"), JSON.stringify([{
    id: "acme",
    source: "https://github.com/acme/thing.git",
    source_tree: path.join(root, "work"),
    store: path.join(root, "store"),
    agent: "claude",
    plane_project: "",
    sync_connection: "",
  }]));
  const gh = path.join(bin, "gh");
  fs.writeFileSync(gh, "#!/bin/sh\nexit 1\n");
  fs.chmodSync(gh, 0o755);
  return { root, home, bin };
}

const fx = fixture();

// Every request the server made to GitHub, with the credential it carried. The WIRE is the seam
// that tells the truth about which token a process is running on — the same argument
// `harness/github.mjs` makes for existing as an API rather than a `$PATH` stub.
let seen = [];
const github = await stub(({ url, req, send }) => {
  seen.push({ url, auth: String(req.headers.authorization || "") });
  if (url === "/user") return send(200, { login: "me" });
  if (url === "/user/teams") return send(403, { message: "Requires read:org" });
  if (url === "/graphql") return send(200, { data: {} });
  return false;
});

const common = {
  SKEIN_HOME: fx.home,
  SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
  // Pinned, or the fleet root is the machine's own (SKEIN-530). `util::fleet_root` — `src/util.rs`,
  // never `config` — refuses an unpinned test process, which the server is under `cargo test`
  // (SKEIN-690), and answers `/boxes` when this suite is run by hand.
  SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
  SKEIN_GITHUB_API: github.url,
  PATH: `${fx.bin}:${process.env.PATH}`,
};

const running = [];

async function serverWith(extra) {
  const door = await openDoor();
  const { port } = door;
  let started;
  try {
    started = await startServer({ door, token: API_TOKEN, env: { ...common, ...extra } });
  } catch (e) {
    door.close();
    throw e;
  }
  const { srv, log } = started;
  running.push(srv);
  return { port, log, srv };
}

/** The `HOME` a started server is actually running on, read from the kernel rather than from us.
 *
 * `/proc/<pid>/environ` is the environment the child was execed with. Asserting against the object
 * this file passed to `startServer` would assert nothing: that object is an input to the mechanism
 * under test, and the failure this check exists for is the mechanism ignoring it. */
function childHome(srv) {
  return childVar(srv, "HOME");
}

/** One variable out of a started server's real environment, or `"unset"`.
 *
 * `childHome`'s reader, generalised for check 4. Same argument: the object this file handed
 * `startServer` is an input to the mechanism under test, so reading it back proves nothing about
 * what the child was execed with. */
function childVar(srv, name) {
  const environ = fs.readFileSync(`/proc/${srv.pid}/environ`, "utf8").split("\0");
  const found = environ.find(v => v.startsWith(`${name}=`));
  return found === undefined ? "unset" : found.slice(name.length + 1);
}

/** What a `HOME` IS, rather than what it says — so a failure names the path and a pass cannot be
 * spelled by a lucky prefix. A home only counts as this suite's if it is inside the fixture AND
 * carries the sentinel `startServer` writes, which is the same evidence `onboarding.mjs` reads out
 * of the box the server went on to launch. */
function whatHomeIsThis(home) {
  if (!home || home === "unset") return "unset";
  if (home === process.env.HOME) return "the home of whoever ran this suite";
  try {
    const inside = fs.realpathSync(home).startsWith(fs.realpathSync(fx.root) + path.sep);
    const stamped = fs.existsSync(path.join(home, ".claude", FIXTURE_HOME_SENTINEL));
    if (inside && stamped) return "a stamped home inside this suite's fixture";
    if (inside) return `inside the fixture but unstamped: ${home}`;
  } catch {}
  return home;
}

const ask = (port, at) =>
  fetch(`http://127.0.0.1:${port}${at}`, { headers: { Authorization: `Bearer ${API_TOKEN}` } })
    .then(r => r.json());

/** Whether a queue refresh said the server has no credential, in the server's own words.
 *
 * `prq::refresh::merged` puts a repo it could not read into `failed` with the reason attached, and
 * with nothing to run on that reason is `host_token`'s: *"no GitHub token: the review queue reads
 * pull requests as you, and nothing here names a user."* */
const noCredential = q => (q.failed || []).some(f => String(f.error).includes("no GitHub token"));

try {
  // 1. The pin holds, and it is the FIXTURE credential that reaches GitHub rather than the ambient
  //    one. Both halves are needed: asserting only "a token arrived" would go green on the
  //    developer's own, which is the state SKEIN-621 was.
  {
    seen = [];
    const { port } = await serverWith({});
    await ask(port, "/api/review?force=1");
    const auths = new Set(seen.map(r => r.auth));
    t.check(
      "a suite that says nothing about the credential runs on the fixture's, not the shell's",
      { askedGitHub: seen.length > 0,
        withTheFixtureToken: auths.has(`Bearer ${FIXTURE_GH_TOKEN}`),
        withTheDevelopersOwn: auths.has(`Bearer ${DEV_GH_TOKEN}`) || auths.has(`Bearer ${DEV_GITHUB_TOKEN}`) },
      { askedGitHub: true, withTheFixtureToken: true, withTheDevelopersOwn: false },
    );
  }

  // 2. The hatch opens. `GH_TOKEN: ""` in the suite's own `env` beats the pin ten lines above the
  //    spread, and `$GITHUB_TOKEN` does not creep back in behind it. GitHub is not asked at all,
  //    because `queue_within` fails at `host_token()` before it composes a request — which is the
  //    strongest form of "the server saw no credential" available from outside the process.
  {
    seen = [];
    const { port } = await serverWith({ GH_TOKEN: "" });
    const q = await ask(port, "/api/review?force=1");
    t.check(
      "a suite that asks for the no-credential case gets one, and the pin does not win",
      { theQueueSaysItHasNoToken: noCredential(q), askedGitHubAnyway: seen.length > 0 },
      { theQueueSaysItHasNoToken: true, askedGitHubAnyway: false },
    );
  }

  // 3. **The server runs on a home inside this suite's fixture, and a suite cannot get a real one
  //    back** (SKEIN-657, SKEIN-681). The other direction from the two checks above, and the reason
  //    the two pins in `startServer` sit on opposite sides of the `...env` spread.
  //
  //    Unpinned, `$HOME` in a suite's server was the home of whoever ran the suite, and that is not
  //    a tidiness problem: `src/box-session.sh` seeds every box it starts by copying `$HOME/.claude`
  //    and five siblings — 463 MB of it here, per box — under `cp -a … || exit 1`, so a file that
  //    Claude Code renames mid-copy kills the launch; and it binds `~/.local`, `~/.cargo`,
  //    `~/.rustup` and `~/.npm` read-WRITE into that box, along with `~/.claude/sessions`, and
  //    reconciles credentials back into `~/.claude/.credentials.json`. A test that can do that to
  //    the machine it runs on is the same class of defect as a test running on the developer's
  //    GitHub credential, which is what checks 1 and 2 are about.
  //
  //    **The second arm is the check.** The first would go green on a harness that simply passed
  //    the caller's environment through on a machine where `$HOME` happened to be a fixture; the
  //    second asks for the runner's own home in the suite's `env` and requires the harness to
  //    refuse it, which is the failure direction a future edit takes — moving the assignment above
  //    the spread, or dropping it for a suite that "needs a real home". Read out of
  //    `/proc/<pid>/environ`, so what is asserted is the environment the process was given.
  {
    const saidNothing = await serverWith({});
    const askedForARealOne = await serverWith({ HOME: process.env.HOME });
    const mine = "a stamped home inside this suite's fixture";
    t.check(
      "the server's home is this suite's fixture, and a suite asking for a real one is refused",
      { whenTheSuiteSaysNothing: whatHomeIsThis(childHome(saidNothing.srv)),
        whenTheSuiteAsksForTheRunners: whatHomeIsThis(childHome(askedForARealOne.srv)) },
      { whenTheSuiteSaysNothing: mine, whenTheSuiteAsksForTheRunners: mine },
    );
  }

  // 4. **The auth-off switch pin, and it is the check the block below says to write when a name is
  //    given a real meaning again** (SKEIN-962). `$SKEIN_LISTEN_INHERITED_ONLY` now has one:
  //    `apiauth::off_switch_refused` reads it to decide where `$SKEIN_NO_API_AUTH` is refused, so a
  //    server that carries both serves nothing but the refusal on every path.
  //
  //    It is ambient in a box — `src/server-doorway.py` sets it on the cockpit it execs and a box
  //    session inherits the cockpit's environment — which is `$SKEIN_IN_FLEET`'s story exactly, and
  //    it landed the same way: `attach.mjs` runs with `SKEIN_NO_API_AUTH` and died on `server never
  //    came up` on a machine where nothing was wrong with the server. This suite sets the ambient
  //    value itself, the way it sets the developer's credential above, so the question is the same
  //    one on a runner as in a box rather than passing vacuously wherever the variable is absent.
  //
  //    **Both arms, and they differ.** The first is the pin: a suite that says nothing gets a server
  //    that is not the fleet's cockpit. The second is the hatch: a suite that means to say it is one
  //    still can, which is what `tests/server/door.rs` relies on to spawn both shapes.
  //
  //    **And `$SKEIN_IN_BOX` the same way** (SKEIN-1086). SKEIN-972 stopped the doorway's variable
  //    reaching a box, and the launcher now marks a box with this one on purpose, so a server a box
  //    starts still refuses the switch. That makes it ambient in a box too, with the same effect on
  //    a suite, so the harness strips it and this checks both arms of it.
  {
    const saidNothing = await serverWith({});
    const saidItWasTheCockpit = await serverWith({ SKEIN_LISTEN_INHERITED_ONLY: "1" });
    const saidItWasABox = await serverWith({ SKEIN_IN_BOX: "1" });
    const pin = "SKEIN_LISTEN_INHERITED_ONLY";
    const marker = "SKEIN_IN_BOX";
    t.check(
      "a suite's server is not told it is the fleet's cockpit, and a suite that means to say so can",
      { whenTheSuiteSaysNothing: childVar(saidNothing.srv, pin),
        whenTheSuiteSaysItIs: childVar(saidItWasTheCockpit.srv, pin) },
      { whenTheSuiteSaysNothing: "unset", whenTheSuiteSaysItIs: "1" },
    );
    t.check(
      "a suite's server is not told it is inside a box, and a suite that means to say so can",
      { whenTheSuiteSaysNothing: childVar(saidNothing.srv, marker),
        whenTheSuiteSaysItIs: childVar(saidItWasABox.srv, marker) },
      { whenTheSuiteSaysNothing: "unset", whenTheSuiteSaysItIs: "1" },
    );
  }

  // 5. **The agent CLI a suite's server can reach is a stub, and a suite cannot ask for the real one
  //    by leaving it empty** (SKEIN-1092). Three arms. The first is the pin, read from the kernel
  //    like check 3's home. The second is the hatch every suite that asserts on what the agent said
  //    already uses. The third is the refusal.
  //
  //    **And the fourth arm is the reason, observed rather than named**: `/api/health` starts
  //    `ai::model_choices`' background `claude --help` on a fresh server, which is the call that
  //    panicked `onboarding.mjs`'s server. With the pin, the stub is what runs — it leaves
  //    `<stub>.asked` behind — and the server's log carries no refusal. Without it (the line in
  //    `startServer` deleted), the stub is never asked and the log carries `agent_command`'s
  //    "$SKEIN_CLAUDE_BIN is unset in a test process", so this arm fails by name either way.
  {
    const saidNothing = await serverWith({});
    const own = path.join(fx.bin, "a-suites-own-agent");
    const namedItsOwn = await serverWith({ SKEIN_CLAUDE_BIN: own });
    let emptyRefused = "";
    try {
      await serverWith({ SKEIN_CLAUDE_BIN: "" });
      emptyRefused = "a server was started";
    } catch (e) {
      emptyRefused = String(e.message).includes("$SKEIN_CLAUDE_BIN empty") ? "refused" : String(e.message);
    }
    const stub = childVar(saidNothing.srv, "SKEIN_CLAUDE_BIN");
    t.check(
      "a suite's server is handed a stub agent CLI, a suite may name its own, and an empty one is refused",
      { whenTheSuiteSaysNothing: path.basename(stub) === FIXTURE_AGENT_STUB && stub.startsWith(fx.root + path.sep),
        whenTheSuiteNamesItsOwn: childVar(namedItsOwn.srv, "SKEIN_CLAUDE_BIN") === own,
        whenTheSuiteAsksForNone: emptyRefused },
      { whenTheSuiteSaysNothing: true, whenTheSuiteNamesItsOwn: true, whenTheSuiteAsksForNone: "refused" },
    );
    await ask(saidNothing.port, "/api/health");
    const asked = `${stub}.asked`;
    for (let i = 0; i < 50 && !fs.existsSync(asked) && !saidNothing.log().includes("is unset in a test process"); i++) {
      await new Promise(r => setTimeout(r, 100));
    }
    t.check(
      "the agent call a health report starts reaches the stub, and the server never refuses it",
      { theStubWasAsked: fs.existsSync(asked), theServerRefusedTheRealOne: saidNothing.log().includes("is unset in a test process") },
      { theStubWasAsked: true, theServerRefusedTheRealOne: false },
    );
  }

  // 6. **A bare `tmux` the server runs reaches the fixture's socket and never the runner's**
  //    (SKEIN-1091). This suite starts a tmux of its own, holding a session called `skein-update`,
  //    and points `$TMUX` at it the way a pane in a skein box does. Then it hands a server an
  //    update that is believed to be running and asks `/api/update`: `update::settle` asks
  //    `tmux has-session -t skein-update` with no `-S`, so the answer is decided by which tmux that
  //    bare command reaches. The fixture's socket has no such session and the run is settled as
  //    ended — `running: false`. With `$TMUX` passed through (delete the strip in `startServer`),
  //    the runner's tmux answers that the session is there and the run stays `running: true`.
  //
  //    The environment is checked too, from `/proc`, so a failure says which half moved.
  {
    const theirs = path.join(fx.root, "runners-tmux.sock");
    const planted = spawnSync("tmux", ["-S", theirs, "new-session", "-d", "-s", "skein-update", "sleep 600"],
      { env: { ...process.env, TMUX: "" }, stdio: "ignore" });
    const pid = spawnSync("tmux", ["-S", theirs, "display-message", "-p", "#{pid}"], { encoding: "utf8" }).stdout.trim();
    process.env.TMUX = `${theirs},${pid},0`;
    process.env.TMUX_PANE = "%0";
    try {
      const home = path.join(fx.root, "home-updating");
      fs.mkdirSync(home, { recursive: true });
      fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
      // A sandbox name, because `settle` asks nothing when there is none; the ask runs on this
      // machine either way (`place::own_sandbox`).
      fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example" }));
      fs.writeFileSync(path.join(home, "repos.json"), "[]");
      fs.writeFileSync(path.join(home, "update.run"), "fixture-run");
      fs.writeFileSync(path.join(home, "update.log"), "skein: fetching\n");
      const { srv, port } = await serverWith({ SKEIN_HOME: home });
      const u = await ask(port, "/api/update");
      const tmpdir = childVar(srv, "TMUX_TMPDIR");
      t.check(
        "a bare tmux from the server reaches the fixture's own socket, not the runner's",
        { plantedTheRunnersTmux: planted.status === 0 && /^\d+$/.test(pid),
          TMUX: childVar(srv, "TMUX"),
          TMUX_PANE: childVar(srv, "TMUX_PANE"),
          TMUX_TMPDIRInsideTheFixture: tmpdir.startsWith(fx.root + path.sep),
          theRunnersSessionWasSeen: u.running },
        { plantedTheRunnersTmux: true, TMUX: "unset", TMUX_PANE: "unset", TMUX_TMPDIRInsideTheFixture: true,
          theRunnersSessionWasSeen: false },
      );
    } finally {
      delete process.env.TMUX;
      delete process.env.TMUX_PANE;
      spawnSync("tmux", ["-S", theirs, "kill-server"], { stdio: "ignore" });
    }
  }

  // Two further checks were here, and they are gone rather than retargeted. They asserted a
  // `$SKEIN_IN_FLEET` pin the same way checks 1 and 2 assert the credential one: a server started
  // without it answered `lifecycle_refusal: null` and a server started with it answered a sentence,
  // so the two answers proved the pin held and that a suite could still opt out of it.
  //
  // **There is no longer a difference to observe.** SKEIN-521/576 deleted the host-driven
  // deployment: `fleet_lifecycle_refusal` now refuses ALWAYS, and nothing in the tree reads
  // `$SKEIN_IN_FLEET` for behaviour any more (`grep -rn 'var("SKEIN_IN_FLEET")' src/ warden/` is
  // empty). A check whose two arms cannot differ is decoration, and would have passed for ever
  // without proving the pin.
  //
  // **The pin itself has since gone the same way** (SKEIN-643): `harness/server.mjs` no longer
  // strips the variable, and this suite no longer sets it, because removing a value no code
  // consults asserts that it decides something. If `$SKEIN_IN_FLEET` ever steers behaviour again,
  // the pin and the check to write are both the ones deleted here.
  //
  // The credential pin above is unaffected and is the live half of SKEIN-624.

} catch (e) {
  // A suite that could not run is a failure, not a silence — `t.done()` exits 0 on an empty ledger,
  // so the failure has to go INTO the ledger rather than beside it.
  t.check("the harness suite could run at all", String((e && e.message) || e), "it ran");
} finally {
  for (const srv of running) srv.kill();
  github.close();
  fs.rmSync(fx.root, { recursive: true, force: true });
}

t.done();
