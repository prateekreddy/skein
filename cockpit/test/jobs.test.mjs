import { test } from "node:test";
import assert from "node:assert/strict";
import { jobFigures, jobsForHash, jobsHtml, jobSpan } from "../src/jobs.mjs";

// Rows spelled the way `jobs::FleetJob` serialises them.
const running = (extra = {}) => ({
  box: "web-main", repo: "web", id: "j-run", class: "test", short: "cargo test",
  command: "cargo test --workspace", state: "granted", cores: 4, min: 4, granted_cores: 4,
  using: 2.5, mem_mib: 1536, mem_measured: true, reason: "", secs: 125, ...extra,
});
const waiting = (extra = {}) => ({
  box: "api-main", repo: "api", id: "j-wait", class: "build", short: "cargo build",
  command: "cargo build --release", state: "waiting", cores: 2, min: 2, granted_cores: 0,
  mem_mib: 2048, mem_measured: false, position: 1, reason: "waiting for 2 cores (1 free)", secs: 40,
  ...extra,
});
const view = (jobs, extra = {}) => ({ at: "2026-10-31T12:00:00Z", cores: 11, granted: 4, free: 1, running: true, jobs, ...extra });

// What would make it fail: a hash that is not a box's opening the list (or `#jobs-` with nothing),
// or the box's name not taken from the link.
test("a link opens the whole list or one box's, and nothing else", () => {
  assert.deepEqual(jobsForHash("#jobs"), { box: "" });
  assert.deepEqual(jobsForHash("#jobs-web-main"), { box: "web-main" });
  for (const h of ["", "#askq", "#jobs-", "#jobs-../x", "#jobs-a b", "#jobsweb", "#jobs--x", "#jobs-.x"]) {
    assert.equal(jobsForHash(h), null, h);
  }
});

test("a span reads as seconds, minutes, then hours and minutes", () => {
  assert.deepEqual([0, 40, 125, 3600, 3725].map(jobSpan), ["0s", "40s", "2m", "1h", "1h 2m"]);
});

// What would make it fail: a running job shown without what it uses, or a waiting one shown
// with a grant it does not have; memory measured shown as an estimate or the other way.
test("a row says what it asked for, what it has and uses, its memory, and how long", () => {
  assert.equal(jobFigures(running()), "4 cores · using 2.5 · 1.5 GB · running 2m");
  assert.equal(jobFigures(running({ granted_cores: 2 })), "asked 4 cores, granted 2 · using 2.5 · 1.5 GB · running 2m");
  assert.equal(jobFigures(running({ using: undefined, mem_measured: false, mem_mib: 2048 })), "4 cores · ~2 GB · running 2m");
  assert.equal(jobFigures(waiting()), "asks 2 cores · needs ~2 GB · waiting 40s");
  assert.equal(jobFigures(waiting({ min: 1, cores: 4 })), "asks 1–4 cores · needs ~2 GB · waiting 40s");
});

// What would make it fail: a row with no Stop, a Stop that does not name its job, the pool not
// drawn, or a box's words drawn as markup.
test("every row has a Stop naming its job, the pool is shown, and a box's words are text", () => {
  const html = jobsHtml(view([running(), waiting(), waiting({ id: "j-odd", command: "<b>x</b>", reason: "<i>why</i>" })]));
  const stops = [...html.matchAll(/data-stop data-box="([^"]+)" data-id="([^"]+)"/g)].map(m => `${m[1]}/${m[2]}`);
  assert.deepEqual(stops, ["web-main/j-run", "api-main/j-wait", "api-main/j-odd"]);
  assert.match(html, /<b>11<\/b> cores · <b>4<\/b> granted · <b>1<\/b> free/);
  assert.match(html, /waiting, 1 in line/);
  assert.match(html, /waiting for 2 cores \(1 free\)/);
  assert.doesNotMatch(html, /<i>why/);
  assert.doesNotMatch(html, /title="<b>/);
});

// What would make it fail: `#jobs-<box>` showing every box's jobs, or saying nothing when that box
// has none.
test("one box's list shows only its jobs, and says so", () => {
  const html = jobsHtml(view([running(), waiting()]), "api-main");
  assert.equal([...html.matchAll(/data-stop/g)].length, 1);
  assert.match(html, /api-main's jobs only/);
  assert.match(jobsHtml(view([running()]), "api-main"), /api-main has nothing waiting or running/);
});

test("an empty fleet says how a box queues work, and a queue not running says so", () => {
  assert.match(jobsHtml(view([])), /skein-job run -- &lt;command&gt;/);
  assert.match(jobsHtml(view([], { running: false })), /The queue is not running/);
  assert.doesNotMatch(jobsHtml(view([])), /not running/);
});
