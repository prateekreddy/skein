import { test } from "node:test";
import assert from "node:assert/strict";
import { jobFigures, jobsForHash, jobsHtml, jobsOverHtml, jobSpan } from "../src/jobs.mjs";

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

// What would make it fail: an idle running row (`idle_secs` set) not saying so, a busy one saying
// it, or the pool leaving out the cores lent, or naming lent cores when none are.
test("an idle job says its cores are lent, and the pool counts them", () => {
  assert.equal(jobFigures(running({ using: 0.01, idle_secs: 372 })), "4 cores · using 0.0 · idle 6m, its cores lent · 1.5 GB · running 2m");
  assert.doesNotMatch(jobFigures(running()), /idle|lent/);
  assert.match(jobsHtml(view([running({ idle_secs: 372 })], { lent: 4 })), /<b>4<\/b> granted · <b>4<\/b> lent · <b>1<\/b> free/);
  assert.doesNotMatch(jobsHtml(view([running()])), /lent/);
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

// What would make it fail: the select not starting on "now", a pick that does not change the
// figure shown (the job's `using` read whatever the pick), the machine's busy cores not following
// the same pick, or a span not covered yet shown as if it were, without saying what it is instead.
test("the average-over pick starts on now, and changes the figures it averages", () => {
  const selected = html => [...html.matchAll(/<option value="([^"]+)" selected>/g)].map(m => m[1]);
  assert.deepEqual(selected(jobsOverHtml()), ["now"]);
  assert.deepEqual(selected(jobsOverHtml("bogus")), ["now"]);
  assert.deepEqual(selected(jobsOverHtml("300")), ["300"]);
  assert.match(jobsOverHtml(), /average over <select data-jobs-over>/);
  const j = running({ using: 0.4, using_avg: { "60": 3.2, run: 2.9 } });
  const v = view([j], { busy: 1.1, busy_avg: { "60": 8.6 } });
  const fig = html => /class="sq-meta jb-fig">([^<]*)</.exec(html)[1];
  const pool = html => /class="jb-pool">(.*?)<\/div>/.exec(html)[1];
  // The default is now: the rate between the last two readings.
  assert.equal(fig(jobsHtml(v)), "4 cores · using 0.4 · 1.5 GB · running 2m");
  assert.match(pool(jobsHtml(v)), /machine busy <b>1.1<\/b>$/);
  // A minute: both the job's and the machine's figures move.
  assert.equal(fig(jobsHtml(v, "", "60")), "4 cores · using 3.2 · 1.5 GB · running 2m");
  assert.match(pool(jobsHtml(v, "", "60")), /machine busy <b>8.6<\/b>$/);
  assert.equal(jobFigures(j, "run"), "4 cores · using 2.9 · 1.5 GB · running 2m");
  // Five minutes, not covered yet: the job's whole run so far, the machine's longest covered span.
  assert.equal(jobFigures(j, "300"), "4 cores · using 2.9 (all 2m so far) · 1.5 GB · running 2m");
  assert.match(pool(jobsHtml(v, "", "300")), /machine busy <b>8.6<\/b> \(last 1 min\)$/);
  assert.match(pool(jobsHtml(view([j]))), /<b>1<\/b> free$/, "no machine figure when none was read");
});
