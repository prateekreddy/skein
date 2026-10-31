// The heavy-jobs list, and the owner's Stop on a waiting job and on a running one, against the real
// server's queue and the shipped wrapper (SKEIN-1422).
//
// Two boxes each run `skein-job run` as an agent would: `web-main`'s job asks for every core, is
// granted them and runs a `sleep`; `api-main`'s, filed after it, waits for a core. `/#jobs` opens
// the list with both rows and the pool. Stop on the waiting row takes it out of the queue, and its
// wrapper says the owner stopped it and exits 137 without running its command. Stop on the running
// row kills its command, and its wrapper says the same and exits 137. `/#jobs-web-main` opens the
// list narrowed to that box.
//
//   node tests/ui/jobs.mjs
import { chromium } from "playwright";
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
import { stopThenRemove } from "./harness/teardown.mjs";
const API_TOKEN = "skein-test-" + "t".repeat(53);
const WRAPPER = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..", "src", "plugin", "bin", "skein-job");
const STOPPED = "stopped by you (the owner) from the cockpit";

function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-jobs");
  const home = path.join(root, "home");
  const fleet = path.join(root, "fleet");
  fs.mkdirSync(home, { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  // One job may ask for every core, so `web-main`'s takes the machine and `api-main`'s waits,
  // whatever the machine running this has.
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({ fleet_sandbox: "example", jobs_max_cores_per_job: 4096 }));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  for (const box of ["web-main", "api-main"]) {
    // The box's drop-box, which the launcher makes, and its state, where its view is written.
    fs.mkdirSync(path.join(fleet, ".skein", "jobs", "requests", box), { recursive: true });
    fs.mkdirSync(path.join(home, "boxes", box, "signals"), { recursive: true });
  }
  return { root, home, fleet };
}

const fx = makeFixture();
const door = await openDoor();
const { log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: fx.fleet,
  },
});

// One `skein-job run` as box `box` runs it: what it says on stderr, and how it ended.
function job(box, cores, argv) {
  const env = { ...process.env, SKEIN_BOX: box, SKEIN_FLEET_ROOT: fx.fleet, SKEIN_HOME: fx.home,
    SKEIN_STATE: path.join(fx.home, "boxes", box), SKEIN_JOB_LOCK: path.join(fx.root, `${box}.lock`),
    SKEIN_JOB_UNANSWERED_SECONDS: "120" };
  delete env.SKEIN_JOB_ID;
  const child = spawn("python3", [WRAPPER, "run", "--cores", String(cores), "--", ...argv], { env, stdio: ["ignore", "ignore", "pipe"] });
  const out = { err: "", code: undefined, signal: undefined, child };
  child.stderr.on("data", d => { out.err += d; });
  child.on("exit", (code, signal) => { out.code = code; out.signal = signal; });
  return out;
}
const said = path.join(fx.root, "job-said");
const ran = path.join(fx.root, "it-ran");
const running = job("web-main", 4096, ["sh", "-c", `echo $$ > ${said}.tmp && mv ${said}.tmp ${said}; exec sleep 300`]);
const jobs = [running];

const { value: check, report } = ledger();
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
const { errors, sayBlips } = erring(page, { say: (kind, text) => `${kind}: ${text}` });
const until = async (f, ms = 15000) => {
  for (let t = 0; t < ms; t += 200) { if (await f()) return true; await page.waitForTimeout(200); }
  return !!(await f());
};
const alive = pid => { try { process.kill(pid, 0); return true; } catch { return false; } };

try {
  await until(() => fs.existsSync(said), 20000);
  // Filed only once the first holds every core, so it cannot be granted first.
  const waiting = job("api-main", 1, ["touch", ran]);
  jobs.push(waiting);
  const pid = Number(fs.readFileSync(said, "utf8").trim());
  await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });
  await page.goto(`http://127.0.0.1:${door.port}/#jobs`, { waitUntil: "domcontentloaded" });
  const panel = page.locator("#jobs");
  const rows = panel.locator(".jb-row");
  // **What would make this fail:** `/#jobs` not opening the list, the list not reading the tick's
  // `fleet.json`, or one box's job left out.
  check("/#jobs opens the list, with web-main's job running and api-main's waiting",
    await until(async () => (await panel.isVisible()) && (await rows.count()) === 2
      && (await panel.locator('.jb-row[data-state="waiting"]').count()) === 1), true);
  const waitRow = panel.locator('.jb-row[data-state="waiting"]');
  const runRow = panel.locator('.jb-row[data-state="running"]');
  check("the waiting row names its box and says why it waits", {
    box: (await waitRow.locator(".sq-meta b").first().textContent()).trim(),
    why: (await waitRow.locator(".jb-why").textContent()).trim(),
    cmd: (await waitRow.locator(".jb-cmd").textContent()).trim(),
  }, { box: "api-main", why: "waiting for 1 core (0 free)", cmd: "touch it-ran" });
  check("the running row says its cores and how long it has run",
    /^\d+ cores? · .*running \d+[smh]$/.test((await runRow.locator(".jb-fig").textContent()).trim()), true);
  check("the pool is shown, every core granted",
    /^(\d+) cores · \1 granted · 0 free$/.test((await panel.locator(".jb-pool").textContent()).trim()), true);

  // **What would make this fail:** the press not reaching the stop route, the queue not reading
  // the owner's stop, or the wrapper not reading `stopped` in its view.
  await waitRow.locator("button[data-stop]").click();
  check("Stop on the waiting row takes it off the list",
    await until(async () => (await panel.locator('.jb-row[data-state="waiting"]').count()) === 0), true);
  await until(async () => waiting.code !== undefined);
  check("the waiting job's wrapper says the owner stopped it, exits 137, and never ran its command", {
    code: waiting.code, said: waiting.err.includes(`${STOPPED}; the command did not run`), ran: fs.existsSync(ran),
  }, { code: 137, said: true, ran: false });

  // **What would make this fail:** the stop not killing what is under the wrapper, killing the
  // wrapper too (it then ends by a signal and says nothing), or the wrapper not looking for the
  // owner's mark after its command was killed.
  check("the running job's command is alive before the stop", alive(pid), true);
  await runRow.locator("button[data-stop]").click();
  check("Stop on the running row takes it off the list",
    await until(async () => (await rows.count()) === 0), true);
  await until(async () => running.code !== undefined || running.signal !== undefined);
  check("the running job's wrapper says the owner stopped it and exits 137", {
    code: running.code, signal: running.signal ?? null, said: running.err.includes(`${STOPPED}; its processes were killed`),
  }, { code: 137, signal: null, said: true });
  check("its command is gone", await until(async () => !alive(pid), 5000), true);
  check("the empty list says how a box queues work",
    (await panel.locator(".note").first().textContent()).includes("skein-job run"), true);

  // **What would make this fail:** `#jobs-<box>` not read as that box, or the list not narrowed.
  await page.locator("#jobs .x").click();
  await page.goto(`http://127.0.0.1:${door.port}/#jobs-web-main`, { waitUntil: "domcontentloaded" });
  check("/#jobs-web-main opens the list narrowed to that box",
    await until(async () => (await panel.isVisible())
      && ((await panel.locator(".jb-scope").textContent().catch(() => "")) || "").startsWith("web-main's jobs only")), true);

  sayBlips();
  check("the page raised no errors", errors, []);
} catch (e) {
  check("the suite could run at all", String((e && e.message) || e), "it ran");
}

await browser.close();
for (const j of jobs) if (j.code === undefined && !j.signal) try { j.child.kill("SIGKILL"); } catch {}
const failed = report();
const readOr = f => { try { return fs.readFileSync(f, "utf8"); } catch (e) { return `(${e.code})`; } };
if (failed.length) console.log(`\nserver log:\n${log()}\nweb-main's wrapper:\n${running.err}\napi-main's wrapper:\n${jobs[1] ? jobs[1].err : "(never started)"}`
  + `\nthe queue:\n${readOr(path.join(fx.home, "jobs", "queue.json"))}\nthe jobs list:\n${readOr(path.join(fx.home, "jobs", "fleet.json"))}`
  );
const leftRunning = stopThenRemove([fx.root], { keep: failed.length > 0 });
process.exit(failed.length || leftRunning.length ? 1 : 0);
