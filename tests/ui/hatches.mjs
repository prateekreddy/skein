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
import { FIXTURE_GH_TOKEN, FIXTURE_HOME_SENTINEL, startServer } from "./harness/server.mjs";
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

const t = harness();

/** A registry with one GitHub repo in it, and a `gh` that has nothing.
 *
 * **The stub `gh` is what makes check 2 mean the same thing on every machine.** `GH_TOKEN: ""` on
 * its own does not produce the no-credential case: `$GH_TOKEN` is the first of FOUR sources in
 * `look_for_a_credential` (src/prq/credentials.rs), and it falls through to the stored read token,
 * then any write PAT, then `gh auth token` — asked as `Command::new("gh")`, so from `$PATH`
 * (src/repos.rs:1601). The fresh `$SKEIN_HOME` empties the two stored ones; this empties the last.
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
  // Pinned, or `config::fleet_root` falls back to `/boxes` — the fleet the developer is living in
  // (SKEIN-530).
  SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
  SKEIN_GITHUB_API: github.url,
  PATH: `${fx.bin}:${process.env.PATH}`,
};

const running = [];

async function serverWith(extra) {
  const door = await openDoor();
  const { port } = door;
  const { srv, log } = await startServer({ door, token: API_TOKEN, env: { ...common, ...extra } });
  running.push(srv);
  return { port, log, srv };
}

/** The `HOME` a started server is actually running on, read from the kernel rather than from us.
 *
 * `/proc/<pid>/environ` is the environment the child was execed with. Asserting against the object
 * this file passed to `startServer` would assert nothing: that object is an input to the mechanism
 * under test, and the failure this check exists for is the mechanism ignoring it. */
function childHome(srv) {
  const environ = fs.readFileSync(`/proc/${srv.pid}/environ`, "utf8").split("\0");
  const found = environ.find(v => v.startsWith("HOME="));
  return found === undefined ? null : found.slice("HOME=".length);
}

/** What a `HOME` IS, rather than what it says — so a failure names the path and a pass cannot be
 * spelled by a lucky prefix. A home only counts as this suite's if it is inside the fixture AND
 * carries the sentinel `startServer` writes, which is the same evidence `onboarding.mjs` reads out
 * of the box the server went on to launch. */
function whatHomeIsThis(home) {
  if (!home) return "unset";
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
