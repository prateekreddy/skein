// Settings → Usage in a real browser (SKEIN-837).
//
// **Nobody had seen this pane render a reading.** Everything asserted about it when it shipped was
// either node over `usageHtml` — a pure function, given a payload by hand — or HTTP over the
// payload `/api/usage` returns. Both halves passed while nothing had established that the two meet:
// that the pane asks at the moment it is opened and not before, that the figure on screen came from
// the reading the sentence beside it describes, and that Refresh works a second time.
//
// The node tier says where its own limit is, on `visible` in `cockpit/test/usage.test.mjs`: text
// inside `<div hidden>` or under a CSS rule is invisible to it and it stays green. That is this
// file's territory — a bounding box, an `innerText`, and the wiring between a fetch and a cell.
//
// **What is deliberately NOT here.** The panel's arithmetic (money formatting, ordering, escaping,
// the wording of an age) is sixteen node checks over a pure function, and re-asserting it through a
// browser would be slower and no stronger.
//
//   node tests/ui/usage.mjs
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fixtureRoot, freshFixture, openDoor } from "./lift.mjs";
import { erring, ledger } from "./harness/browser.mjs";
import { startServer } from "./harness/server.mjs";
const API_TOKEN = "t".repeat(64);

// **An invented box name, and it must stay invented.** A real one in the tree is SKEIN-629 and
// `tools/residue-check.py` is what catches it; a usage fixture is the likeliest place to reintroduce
// one, because the reader is keyed by box. `src/usage.rs` needs the real names and so carries FNV-1a
// digests of them — this suite needs only *a* box, so it makes one up and the question never arises.
const FIXTURE_BOX = "spend-fixture-box";

// The smallest fixture that serves a settings dialog, plus one box's transcripts. `refresh` walks
// `$SKEIN_HOME/boxes/<name>/claude-projects/**.jsonl` (`usage::box_roots`), so the fleet this pane
// reads is entirely inside the fixture — `$SKEIN_FLEET_ROOT` is its sibling and stays empty, which
// is also the shape `startServer` requires of the pair.
function makeFixture() {
  const root = freshFixture(fixtureRoot(), "ui-usage");
  const home = path.join(root, "home");
  const projects = path.join(home, "boxes", FIXTURE_BOX, "claude-projects", "a-project");
  fs.mkdirSync(projects, { recursive: true });
  fs.writeFileSync(path.join(home, "api-token"), API_TOKEN, { mode: 0o600 });
  fs.writeFileSync(path.join(home, "config.json"), JSON.stringify({}));
  fs.writeFileSync(path.join(home, "repos.json"), "[]");
  fs.writeFileSync(path.join(root, "sandboxes.json"), "{}");
  return { root, home, projects };
}

// One assistant record is one billable turn. Cache-read tokens are not padding: the insight line is
// a percentage of them, so a fixture without any would render a pane whose most-argued-over sentence
// says nothing. The body text is distinctive on purpose — a transcript's contents must never reach a
// report, and `only_counts_leave_the_transcript` is what would catch it if one did.
function addTranscript(projects, id) {
  const record = {
    type: "assistant",
    timestamp: "2026-09-01T10:00:00Z",
    requestId: `req-${id}`,
    isSidechain: false,
    message: {
      id: `msg-${id}`,
      model: "claude-opus-5",
      content: [{ type: "text", text: "BODY-SHOULD-NEVER-LEAVE" }],
      usage: {
        input_tokens: 100,
        output_tokens: 200,
        cache_read_input_tokens: 4000,
        cache_creation_input_tokens: 100,
        cache_creation: { ephemeral_5m_input_tokens: 40, ephemeral_1h_input_tokens: 60 },
      },
    },
  };
  fs.writeFileSync(path.join(projects, `session-${id}.jsonl`), `${JSON.stringify(record)}\n`);
}

// Age the stored reading by writing it, **both copies of the timestamp together**: `read_at_unix`,
// which the server ages against, and `report.read_at`, the string the pane renders. Moving one and
// not the other would leave a file no refresh could have written — and since the whole question
// below is whether the sentence describes the reading it is beside, a fixture whose two timestamps
// disagree would be asserting on the bug rather than against it (`tests/server.rs` does the same).
function ageStoredReading(home, secondsAgo) {
  const file = path.join(home, "usage.json");
  const stored = JSON.parse(fs.readFileSync(file, "utf8"));
  const unix = Math.floor(Date.now() / 1000) - secondsAgo;
  stored.read_at_unix = unix;
  stored.report.read_at = `${new Date(unix * 1000).toISOString().slice(0, 19)}Z`;
  fs.writeFileSync(file, JSON.stringify(stored));
  return stored.report.read_at;
}

// `value` rather than `check`: every claim below is a comparison with an expected answer, and the
// ledger prints both sides of one. The throwing form is reserved for the `catch` at the end.
const { value, results, report } = ledger();

const fx = makeFixture();
addTranscript(fx.projects, "one");
const door = await openDoor();
const { srv, log } = await startServer({
  door,
  token: API_TOKEN,
  env: {
    SKEIN_REGISTRY: path.join(fx.root, "sandboxes.json"),
    SKEIN_HOME: fx.home,
    SKEIN_FLEET_ROOT: path.join(fx.root, "fleet"),
  },
});
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
page.setDefaultTimeout(5000);
// The page's own errors, with the browser's own complaints about its transport kept apart and
// reported rather than failing this run — the distinction is structural, not a list of spellings;
// see `harness/browser.mjs::erring` (SKEIN-998, SKEIN-1010). This suite is the one that most needed
// it: the working path below asserts this list is EMPTY, so a transport blip anywhere in the run
// failed a check about the pane.
const { errors, sayBlips } = erring(page, { say: (kind, text) => `${kind}: ${text}` });

// Every ask for the route, in order, as the browser issues it. `page.on("request")` rather than a
// count inside the page: what is in question is whether the page asks at all, and a counter the page
// keeps is a counter the page could be wrong about.
const asks = [];
const others = new Set();
page.on("request", r => {
  const { pathname, search } = new URL(r.url());
  if (pathname === "/api/usage") asks.push(search);
  else if (pathname.startsWith("/api/")) others.add(pathname);
});

// **Every `/api/…` this page asked for that the server had no route for** (SKEIN-857).
//
// The pane shipped in two halves and only one of them landed: `src/web/index.html` asked
// `/api/usage` and no router entry registered it, so opening the pane was answered 404 and the
// page reported that as its own generic failure.
// `cockpit_routes::the_cockpit_never_asks_for_a_route_this_server_does_not_serve` in
// `src/bin/skein-server.rs` is what found it, and it is a SOURCE comparison — it reads the
// router's entries and the pages' URL literals out of the source and matches the strings. This
// makes the same claim by asking a running server, which is the half a string match cannot make:
// a path the page builds by concatenation rather than as one literal, or an entry registered onto
// a router nothing mounts, reads as served in the source scan and 404s in a browser.
//
// Read from the RESPONSE rather than from the pane, because from inside the page a 404 and a slow
// read are the same thing — `usageReading` stays `null` either way. Measured: with that route
// deleted this suite stopped at "the first reading reaching the page never happened — waited
// 20000ms", which is true and tells a reader nothing about where to look.
const unserved = [];
page.on("response", r => {
  const { pathname } = new URL(r.url());
  if (r.status() === 404 && pathname.startsWith("/api/")) unserved.push(pathname);
});
const unservedPaths = () => [...new Set(unserved)];

// **Wait for an arrival, never for a beat** (SKEIN-833). Every negative assertion below sits behind
// one of these, and each one fails saying what never came — a suite that waits a fixed 600 ms in
// front of "the page did not ask" goes green having observed nothing at all, which is the one
// failure mode worse than red.
//
// A 404 on an `/api/` path ends the wait at once rather than serving out its twenty seconds: what
// is being waited for cannot arrive, and the useful sentence is the path, not the duration.
const until = async (got, what, ms = 20000) => {
  const deadline = Date.now() + ms;
  for (;;) {
    const now = await Promise.resolve(got()).catch(() => false);
    if (now) return now;
    if (unserved.length) {
      throw new Error(
        `${what} cannot happen — this server serves no route for ${unservedPaths().join(", ")}, ` +
        "so the page's fetch was answered 404");
    }
    if (Date.now() > deadline) throw new Error(`${what} never happened — waited ${ms}ms`);
    await page.waitForTimeout(50);
  }
};
const paneText = () => page.evaluate(() => {
  const el = document.getElementById("set-usage");
  return el ? el.innerText : "";
});
// Read with ONE evaluate, so the query and the measurement happen in the same page task: this pane
// replaces its whole subtree on every draw, and a handle taken in one call and measured in the next
// can be measuring a node the redraw has already detached (SKEIN-716).
const paneSize = () => page.evaluate(() => {
  const el = document.getElementById("set-usage");
  if (!el) return null;
  const r = el.getBoundingClientRect();
  return { width: r.width, height: r.height };
});
const seen = sel => page.evaluate(s => {
  const el = document.querySelector(s);
  if (!el) return null;
  const r = el.getBoundingClientRect();
  return { text: el.innerText || el.textContent || "", width: r.width, height: r.height, cls: el.className };
}, sel);

await page.goto(`http://127.0.0.1:${door.port}/?t=${API_TOKEN}`, { waitUntil: "domcontentloaded" });

// **A failed arrival is a named failure, not an abandoned suite.** `until` throws, which is right —
// a wait that gives up must say what never came — but a throw out of the top level of an ES module
// skips the ledger's report, the screenshot and the closing of the browser. So the phases run inside
// a `try`: the throw becomes one more red line with its own message, and the tail below always runs.
try {

  // ── nothing is read on page load ─────────────────────────────────────────────────────────────────
  //
  // **The claim is a negative, so it is asserted after something that must come later.** Waiting a
  // moment and finding no ask would be the SKEIN-833 shape exactly. Instead the suite opens Settings
  // on another pane and waits for *that* pane's own requests to land: `openSettings` fetches runtimes,
  // settings, repos and connections before it calls `setPane`, so once those have arrived, any fetch
  // the page makes on load or on opening the dialog has already been made.
  //
  // **What would make this fail:** moving the fetch out of `showUsage` and into the page's startup, or
  // into `openSettings` — which is the change a person makes to "warm it up", and it costs a 5.2 s
  // walk of every box's transcripts on every reload of the cockpit, to answer a question nobody asked.
  await page.evaluate(() => openSettings("repos"));
  await until(() => others.has("/api/repos") && page.evaluate(() => !!document.querySelector("#settings.open")),
    "the settings dialog opening and asking for its own data");
  value("nothing asks for a usage reading on page load, or on opening Settings", asks, []);

  // ── opening the pane asks exactly once, and the pane is on screen ────────────────────────────────
  await page.evaluate(() => setPane("usage"));
  await until(() => page.evaluate(() => usageReading !== null), "the first reading reaching the page");
  value("opening the pane asks once, without asking for a re-read", asks, [""]);

  {
    const nav = await page.$$eval(".set-navi", els => els.map(e => e.textContent.trim()));
    value("Usage is one of the settings panes", nav.some(n => n.startsWith("Usage")), true);
    // Present in the DOM is not enough: a pane with a zero box is a pane a CSS rule is hiding, and
    // that is indistinguishable from a working one to every other check in this file.
    const size = await paneSize();
    value("and the pane is on screen, not merely in the DOM", !!size && size.width > 0 && size.height > 0, true);
    const insight = await seen("#set-usage .ug-insight");
    // The node tier proved this sentence is not demoted into a `title` attribute, and its own comment
    // records that the same sabotage wrapped in `<div hidden>` stayed green there. This is that gap.
    value("the cache-read insight has a real height, which node cannot establish",
      !!insight && insight.height > 0 && /cache reads/.test(insight.text), true);
    const fresh = await seen("#set-usage .ug-when");
    value("a reading just taken is described as just taken", fresh.text, "read just now");
    value("and it is not marked stale", /\bstale\b/.test(fresh.cls), false);
  }

  // ── a second opening does not ask again ──────────────────────────────────────────────────────────
  //
  // Another negative, so again behind an arrival: the redraw. `showUsage` renders the reading it
  // already holds, so what proves the pane was reopened is the head being on screen again — and only
  // then is "no second ask" a statement about anything.
  await page.evaluate(() => closeSettings());
  await until(() => page.evaluate(() => !document.querySelector("#settings.open")), "the dialog closing");
  await page.evaluate(() => openSettings("usage"));
  await until(async () => {
    const head = await seen("#set-usage .ug-head");
    return head && head.height > 0;
  }, "the pane being drawn a second time");
  value("reopening the pane redraws the reading it holds rather than asking again", asks, [""]);

  // ── Refresh re-reads, and so does the press after it (SKEIN-847) ─────────────────────────────────
  //
  // **This is the defect from the outside.** `usage::report` served a reading taken in the same whole
  // second back to a caller that had asked for a fresh one (`age_secs == 0`, and `0 <= 0`), so the
  // press a person makes *because they did not believe the first* was the press that did nothing.
  //
  // Asserted as two readings rather than two requests, because a request that lands and changes
  // nothing is exactly the bug: a transcript appears between the presses, and each press has to see
  // it. The route calls `usage::refresh` directly today, which is how it dodged the boundary — so what
  // this pins is that it stays that way. Restoring `report(Duration::ZERO)` there turns the second
  // press into a no-op and this check red. The library boundary itself is held by
  // `no_window_admits_a_reading_as_old_as_itself` in `tests/usage.rs`.
  {
    const counted = () => page.evaluate(() => (usageReading ? usageReading.transcripts_read : null));
    value("the pane has read one transcript so far", await counted(), 1);

    addTranscript(fx.projects, "two");
    await page.click("#ug-refresh");
    await until(async () => (await counted()) === 2, "the first Refresh press producing a second reading");
    value("Refresh re-reads, and asks the route for a re-read", asks, ["", "?refresh=1"]);

    addTranscript(fx.projects, "three");
    await page.click("#ug-refresh");
    await until(async () => (await counted()) === 3,
      "the SECOND Refresh press producing a third reading — the press a person makes because they " +
      "did not believe the first");
    value("and the press after it re-reads too", asks, ["", "?refresh=1", "?refresh=1"]);

    const sub = await seen("#set-usage .ug-sub");
    value("the figures on screen came from the reading the last press took",
      /3 transcripts/.test(sub.text), true);
  }

  // ── the sentence says the age of the reading it is SHOWING ───────────────────────────────────────
  //
  // **This repository's recurring defect is a status display that reports on something other than what
  // it names**, and `read_at` / `fresh` / `age_secs` are precisely that shape. So the stored reading is
  // aged fifty minutes and then served — the plain `/api/usage` path, which inside its hourly window
  // hands back exactly the reading on disk — and the request is made *now*. A sentence built from the
  // request would read "read just now"; the reading is fifty minutes old and the sentence has to say
  // so. Half an hour either side of the boundary, so no assertion here turns on a second.
  //
  // A reload first, and the pre-state asserted before anything is awaited: the reading the pane holds
  // survives nothing, but the SERVER's cache does, and a check that waits for "a reading to appear"
  // when one was already on screen ends up asserting inside a frame that predates its own fixture.
  {
    // Exactly fifty minutes, because `usageAge` ROUNDS the minutes it prints — a reading aged
    // 50min30s renders as "51 minutes ago" and the assertion below would be measuring the fixture's
    // arithmetic rather than the pane's. On the minute, the sentence stays "50 minutes ago" until the
    // reading is 29 seconds older than the fixture made it, which is far more room than the reload
    // and the open below can spend.
    const at = ageStoredReading(fx.home, 50 * 60);
    asks.length = 0;
    await page.reload({ waitUntil: "domcontentloaded" });
    await until(() => page.evaluate(() => typeof usageReading !== "undefined" && usageReading === null),
      "the reloaded page starting with no reading at all");
    value("the reloaded page holds no reading, so what follows is not the previous frame", asks, []);

    await page.evaluate(() => openSettings("usage"));
    await until(() => page.evaluate(() => usageReading !== null), "the aged reading reaching the page");
    const shown = await page.evaluate(() => usageReading.read_at);
    value("the served reading is the one on disk, not a fresh scan", shown, at);
    const when = await seen("#set-usage .ug-when");
    value("the sentence says the age of the reading, not the age of the request",
      when.text, "read 50 minutes ago");
    value("and a reading inside the hour is not marked stale", /\bstale\b/.test(when.cls), false);
  }

  // ── before it has read, no figure at all — never $0.00 ───────────────────────────────────────────
  //
  // **The one wrong answer a person would believe.** A fleet nobody has read and a fleet that cost
  // nothing render identically the moment this shows `$0.00`, and of the two the wrong one is the
  // believable one. Two states have to be checked, and neither is reachable by waiting: the request
  // is held open rather than delayed — so "in flight" is observed at an arrival, the request itself —
  // and then answered with a 500, so the failure path is the same page with no reading in it.
  {
    // Everything above this point was the pane working. Asserted here, before the suite goes and
    // breaks the route on purpose, because after that a browser complaint is expected and "nothing
    // complained" stops being a statement anyone can read. `errors` now holds only what the PAGE
    // did (SKEIN-1010), so the deliberate 500 would no longer land in it — but the separation is
    // still worth keeping, since it is the phase boundary that makes this sentence readable rather
    // than a consequence of how one message happens to be classified.
    value("nothing on the working path threw or logged an error", errors, []);

    const held = [];
    await page.route("**/api/usage*", route => { held.push(route); });
    asks.length = 0;
    await page.reload({ waitUntil: "domcontentloaded" });
    await until(() => page.evaluate(() => usageReading === null), "the reloaded page starting empty");
    await page.evaluate(() => openSettings("usage"));
    await until(() => held.length === 1, "the pane's request reaching the network");

    const inflight = await paneText();
    const btn = await page.evaluate(() => {
      const b = document.getElementById("ug-refresh");
      return b ? { text: b.textContent.trim(), disabled: b.disabled } : null;
    });
    value("with nothing read yet the pane shows no figure at all", /\$/.test(inflight), false);
    value("it says so in words instead", /not read yet/.test(inflight), true);
    value("and Refresh says what it is doing rather than inviting a second press",
      btn, { text: "reading…", disabled: true });

    await held[0].fulfill({ status: 500, contentType: "text/plain", body: "no" });
    await until(async () => {
      const note = await seen("#set-usage .ug-note");
      return note && note.height > 0;
    }, "the failure being said in the pane");
    const failed = await paneText();
    value("a route that answers 500 still shows no figure", /\$/.test(failed), false);
    value("and the pane says skein could not read it", /could not read the fleet's usage/.test(failed), true);
    await page.unroute("**/api/usage*");
  }

  // The deliberate 500 is fetched by the page, so the browser logs it as a failed resource — that one
  // line is this suite's own doing. It used to be excused by matching its text against the words for
  // a 500 status; that filter is gone, REPLACED rather than kept beside, because the structural rule
  // in `harness/browser.mjs::erring` covers exactly the case it was written for and covers it better
  // (SKEIN-1010). Measured against this Playwright: the browser's line about a fulfilled 500 arrives
  // carrying no arguments, like any other failed resource load, so it is now classified as the
  // browser's own complaint, named by `sayBlips` just below, and never in `errors` at all. What the
  // filter would still have done, had it stayed, is excuse a message of the PAGE'S OWN whose text
  // happened to contain that wording — a hole rather than a feature, and the SKEIN-647 shape besides.
  //
  // One thing is deliberately NOT asserted: that the 500 is the ONLY thing the browser complained
  // about, which is what the old check said. A transport blip is a second complaint through no fault
  // of the page, and going red on one is the whole defect this suite was fixed for. What a route
  // that stops answering actually costs is asserted below instead, by name and per path.
  //
  // A JavaScript exception is never expected, and is still asserted separately from the console line
  // rather than folded into it, so a real throw cannot hide inside anything allowed for the 500.
  sayBlips();
  value("the failure path raised no JavaScript exception", errors.filter(e => e.startsWith("pageerror")), []);
  value("and the page itself logged nothing on the failure path either", errors, []);

} catch (stopped) {
  // Not "an error occurred": the message is the arrival that never came, and it is the most useful
  // line in the run. It is recorded as a check so it reaches `report` with everything else.
  value("every phase of this suite ran to the end", `stopped at: ${stopped && stopped.message || stopped}`, "");
}

// **Outside the `try`, so a suite that stopped early still states this** (SKEIN-857). The phase
// above reports where the run gave up; this reports what the server would not serve, and the two
// are different sentences — a 404 on a path that does NOT block the reading never reaches the wait
// at all, and would otherwise be observed by nothing. The deliberate outage above is a 500 and is
// not counted here, which is deliberate too: 500 is a route that answered.
value("every /api path this page asked for has a route to answer it", unservedPaths(), []);

const shot = path.join(fx.root, "failure.png");
if (results.some(([ok]) => !ok)) await page.screenshot({ path: shot, fullPage: false }).catch(() => {});
const failed = report({ log });
if (failed.length) console.log(`screenshot: ${shot}\nfixture kept for inspection: ${fx.root}`);
await browser.close();
srv.kill();
if (!failed.length) { try { fs.rmSync(fx.root, { recursive: true, force: true }); } catch {} }
process.exit(failed.length ? 1 : 0);
