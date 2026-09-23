// Settings -> Update when a run has stopped making progress, and the Cancel it offers (SKEIN-1037).
//
// **Why a browser suite and not only the unit tests in `src/update.rs`.** Those hold the two halves
// the server owns — `stalled` from the log's mtime, and a cancel that stops exactly one tmux
// session. What a person sees is the wiring between them: a poll that has to carry `stalled` into
// a notice, a button that has to hand back the run's name, and a pane that has to come out of
// "updating…" afterwards and say what to do next. Every one of those is a fetch, a timer and a
// DOM node, which is what this tier exists to see.
//
// The run is real enough to be stopped: a tmux session called `skein-update` on a socket of this
// fixture's own (`TMUX_TMPDIR`), holding a process in the place a hung `git fetch` would be, and
// beside it a session whose name merely begins the same, which Cancel must not touch.
//
//   node tests/ui/updatestall.mjs
import { chromium } from "playwright";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { stub } from "./harness/github.mjs";
import { startServer } from "./harness/server.mjs";
const API_TOKEN = "t".repeat(64);

// The run's name, as `update::launch` would have written it. Cancel must hand this back.
const RUN = "fixture-run-1";
// The log's last line before the hang — what the notice points the reader at.
const LAST_LINE = "skein: fetching the default branch";

// The Update pane's fixture (see updatepane.mjs), plus a run in progress: a log, a run name, and no
// marker — which is exactly what `update::believed_running` reads as a run that has not ended.
function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-updatestall");
  const home = path.join(root, "home");
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  fs.writeFileSync(path.join(home, "update.run"), RUN);
  fs.writeFileSync(path.join(home, "update.log"),
    `skein: using the toolchain in ${root}/toolchain\n${LAST_LINE}\n`);
  // Short, because a unix socket path cannot exceed 108 bytes and tmux puts `tmux-<uid>/default`
  // under this.
  const tmuxDir = path.join(root, "t");
  fs.mkdirSync(tmuxDir);
  return { root, home, tmuxDir };
}

const fx = makeFixture();
const log = path.join(fx.home, "update.log");
// Seconds ago, as an mtime. The pane's state is driven by nothing else.
const age = secs => { const t = new Date(Date.now() - secs * 1000); fs.utimesSync(log, t, t); };

// **`$TMUX` goes before anything is started.** A suite run from inside a tmux pane inherits it, and
// tmux obeys it over `TMUX_TMPDIR` — so the server's `tmux kill-session` would be aimed at the
// session of whoever ran the suite. Deleted here rather than overridden in `env`, because
// `startServer` spreads `process.env` under it and an empty value is still a value.
delete process.env.TMUX;
const tmuxEnv = { ...process.env, TMUX_TMPDIR: fx.tmuxDir };
const tmux = (...args) => {
  try { execFileSync("tmux", args, { env: tmuxEnv, stdio: "ignore" }); return true; } catch { return false; }
};
const pidIn = name => {
  const at = path.join(fx.root, name);
  for (let i = 0; i < 50 && !(fs.existsSync(at) && fs.readFileSync(at, "utf8").trim()); i++) {
    execFileSync("sleep", ["0.1"]);
  }
  return fs.readFileSync(at, "utf8").trim();
};
const alive = pid => {
  try { return !/^\S+ \(.*\) Z/.test(fs.readFileSync(`/proc/${pid}/stat`, "utf8")); } catch { return false; }
};

// The hung run, and the neighbour whose name begins the same.
tmux("new-session", "-d", "-s", "skein-update",
  `sh -c 'echo $$ > ${path.join(fx.root, "run.pid")}; exec sleep 600'`);
tmux("new-session", "-d", "-s", "skein-update-decoy",
  `sh -c 'echo $$ > ${path.join(fx.root, "decoy.pid")}; exec sleep 600'`);
const runPid = pidIn("run.pid");
const decoyPid = pidIn("decoy.pid");

const github = await stub(({ url, send }) =>
  /^\/repos\/[^/]+\/[^/]+\/commits\/[^/]+$/.test(url) && send(200, { sha: "deadbeef".repeat(5) }));
const door = await openDoor();
const { srv, log: serverLog } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
    SKEIN_GITHUB_API: github.url,
    SKEIN_SOURCE_URL: "https://github.com/acme/skein.git",
    // Every tmux the server runs — `settle`'s liveness question and Cancel's kill alike — lands on
    // this fixture's socket and nowhere else.
    TMUX_TMPDIR: fx.tmuxDir,
  },
});

const { check, value, report } = ledger();
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const { errors, sayBlips } = erring(page, { say: (kind, text) => `${kind}: ${text}` });
const shown = async sel => {
  const box = await page.locator(sel).first().boundingBox({ timeout: 1500 }).catch(() => null);
  return !!box && box.width > 0 && box.height > 0;
};
const textOf = sel => page.$eval(sel, e => e.textContent.trim()).catch(() => "");
const until = async (f, ms = 8000) => {
  for (let t = 0; t < ms; t += 200) { if (await f()) return true; await page.waitForTimeout(200); }
  return !!(await f());
};

try {
  // --- a run that is writing is just "updating…" ---------------------------------------------------
  //
  // **What would make this fail:** a threshold under a minute, or a pane that raised the notice on
  // its own clock rather than the server's `stalled`. The log was written a minute ago.
  age(60);
  await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  await page.waitForTimeout(1200);
  await page.evaluate(() => openSettings("update"));
  await until(async () => (await textOf("#upd-go")) === "updating…");
  value("a run that is going shows the disabled \"updating…\" button",
    [await textOf("#upd-go"), await page.$eval("#upd-go", b => b.disabled).catch(() => null)],
    ["updating…", true]);
  await page.waitForTimeout(2000);
  value("and a log written a minute ago does not read as no progress", await shown("#upd-stall"), false);

  // --- the log goes quiet: the pane says so, shows where it got to, and offers Cancel -----------------
  //
  // **What would make these fail:** the tail not carrying `stalled` into `updateQuiet` (the notice
  // never appears); the server measuring something other than the log's mtime (it never goes
  // stalled, since nothing is written); the notice drawn somewhere a CSS rule hides it.
  age(10 * 60);
  const appeared = await until(() => shown("#upd-stall"));
  value("a log nobody has written to for ten minutes raises the no-progress notice", appeared, true);
  const said = await textOf("#upd-stall .upd-stall-line");
  await check("which says how long, in minutes", () => {
    if (!/^No progress: nothing has been written to the update log for 1[01] minutes\.$/.test(said)) {
      throw new Error(`the notice said ${JSON.stringify(said)}`);
    }
  });
  value("the log is on screen with its last line in it",
    [await shown("#upd-log"), (await textOf("#upd-log")).endsWith(LAST_LINE)], [true, true]);
  value("the log is scrolled to its end, where the notice points",
    await page.$eval("#upd-log", e => e.scrollTop + e.clientHeight >= e.scrollHeight - 2).catch(() => null), true);
  value("and Cancel is offered, on screen",
    [await shown("#upd-cancel"), await textOf("#upd-cancel")], [true, "Cancel update"]);

  // --- Cancel stops that run and nothing else, and the pane says what to do next ---------------------
  //
  // **What would make these fail:** the press not naming the run (`update::cancel` refuses it and
  // the session lives — the first check); a cancel that did not write the marker (the pane stays on
  // "updating…" — the pane checks); a kill by pattern rather than by exact session (the decoy dies).
  await page.click("#upd-cancel");
  value("the run's session is gone after Cancel",
    await until(() => !tmux("has-session", "-t", "=skein-update") && !alive(runPid)), true);
  value("the session whose name only begins the same is still running",
    [tmux("has-session", "-t", "=skein-update-decoy"), alive(decoyPid)], [true, true]);
  value("the run is recorded as cancelled, not as a failure",
    fs.existsSync(path.join(fx.home, "update.done")) && fs.readFileSync(path.join(fx.home, "update.done"), "utf8"),
    "cancelled");
  const ended = await until(async () => await shown("#upd-ended"));
  value("the pane says it was cancelled and what to do next", [ended, await textOf("#upd-ended")],
    [true, "You cancelled the update. The log below shows where it stopped; press Update skein to try again."]);
  value("the no-progress notice is gone", await shown("#upd-stall"), false);
  value("and Update skein can be pressed again",
    [await textOf("#upd-go"), await page.$eval("#upd-go", b => b.disabled).catch(() => null)],
    ["Update skein", false]);
  await check("the log keeps what the run said and ends with the cancel", async () => {
    const text = await textOf("#upd-log");
    if (!text.includes(LAST_LINE) || !text.includes("skein: you cancelled this update from the Update pane.")) {
      throw new Error(`the log shows ${JSON.stringify(text.slice(-300))}`);
    }
  });

  sayBlips();
  value("the pane raised no page errors", errors, []);
} finally {
  await browser.close();
  srv.kill();
  github.close();
  // The fixture's tmux server is not the skein-server's child, so killing that does not reach it.
  tmux("kill-server");
}

const failed = report();
if (failed.length) console.log(serverLog().split("\n").slice(-20).join("\n"));
else try { fs.rmSync(fx.root, { recursive: true, force: true }); } catch {}
process.exit(failed.length ? 1 : 0);
