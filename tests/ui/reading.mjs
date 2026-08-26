// A reading you are paying for is visible while it runs (SKEIN-333).
//
// Reported live, repeatedly, as "click on reread or redraft doesn't really produce new review and
// summary", and then as the diagnosis rather than the symptom: "even if it is doing work, I am
// unable to see, the fact that I am feeling that means the UX is not good enough."
//
// MEASURED against the owner's running fleet before the fix (playwright, gadget-demo #684):
//
//     20:26:50  GET /review/684/summary?redraft=1
//     20:27:25  200                                    ← 35 seconds
//
//     t+0s   the row shows "…"
//     t+4s   the row shows THE OLD SUMMARY AGAIN
//     t+35s  the new summary silently appears
//
// The server was never wrong — the reading genuinely changed. What was wrong is that the page kept
// its in-flight marker in `revSums`, the same map the queue refresh merges into, and the refresh
// treated the marker as worthless data safe to overwrite. The pump fires every few seconds, so the
// row spent ~31 of those 35 seconds actively displaying the answer the press was replacing.
//
// This suite holds the two halves of the rule that fixed it:
//   1. nothing that merges readings may write over a row that is in flight;
//   2. the row says so, with a counter that comes from the START TIME rather than from a tick.
//
//   node tests/ui/reading.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, with only what these functions actually touch. `fetch` is never reached in
// this suite — every case drives the merge and the render directly — so it throws rather than
// returning a shape that would let a test pass without asking anything.
function world(extra = "", fetchImpl) {
  const body = `
    let revSums = new Map();
    let revInFlight = new Map();
    let revUpdated = new Set();
    let revOpen = new Set();
    // The row must BE in the queue, or toggleRevRow goes looking for a drafted review over the
    // network: revDraftAtHead is only consulted for a row it can find. Seeded with a draft already
    // at this head, which is the no-fetch path. (No backticks anywhere in this block — the whole
    // thing is a template literal.)
    let revQueue = { prs: [{ repo_id: "acme", number: 684, head_sha: "12d1512d" }] };
    let revHeld = "*";
    let revSumBusy = 0;
    let revInFlightTimer = null;
    let view = { mode: "review" };
    const REV_READING_POLL_MS = 4000;
    ${grab("REV_INFLIGHT_POLL_MS")}
    const rk = pr => pr.repo_id + "#" + pr.number;
    let revReadMs = [];
    ${grab("revElapsed")}
    ${grab("revGist")}
    ${grab("revUpdatedChip")}
    // What the mark MEANS, and the two callers that leave it. Lifted rather than restated: the rule
    // is one predicate and a test carrying its own copy would be the rule written twice.
    ${grab("revHasReading")}
    ${grab("revNoteReadMs")}
    ${grab("revReadTypicalMs")}
    ${grab("revFetchSummary")}
    // A read is STARTED by a request and ANSWERED on the page's live stream (SKEIN-366). Both
    // halves belong to any world that spends: revPollInFlight consults revReadWaits to decide
    // whether a reading that stopped running is one this page is still waiting on.
    ${grab("revReadWaits")}
    // Where a landed reading came from (SKEIN-390). Lifted wherever revReadSettle or the bulk merge
    // is, because both write to it: a reading replaced loses the note about which queue built it.
    ${grab("revReadFrom")}
    ${grab("revReadSettle")}
    ${grab("revReadArrived")}
    ${grab("revFetchHeld")}
    ${grab("tickInFlight")}
    ${grab("revPollInFlight")}
    ${grab("loadKnownSummaries")}
    let revStackOpenKey = null, revStackStep = null, revSel = null, revSelAt = 0;
    const revNav = [], revpane = null, revCrits = new Map();
    const revKeyShow = () => {}, revRkQuery = () => "", revLoadReading = () => {},
          revDraftHeld = () => ({ head_sha: "h", comments: 1 });
    ${grab("toggleRevRow")}
    return {
      sums: () => revSums,
      reading: () => revInFlight,
      updated: () => revUpdated,
      put: (key, s) => revSums.set(key, s),
      flight: (key, at, asked) => revInFlight.set(key, { started_ms: at, asked }),
      land: key => revInFlight.delete(key),
      mark: key => revUpdated.add(key),
      open: key => revOpen.add(key),
      merge: known => loadKnownSummaries("acme", known),
      gist: (key) => revGist(revSums.get(key), key),
      chip: pr => revUpdatedChip(pr),
      toggle: key => toggleRevRow(key),
      isOpen: key => revOpen.has(key),
      isMarked: key => revUpdated.has(key),
      elapsed: ms => revElapsed(ms),
      fetchSummary: (id, n, how) => revFetchSummary(id, n, how),
      // The stream, as this world plays it: hand a reading back the way /api/events does.
      arrive: d => revReadArrived(d),
      poll: () => revPollInFlight(),
      typical: () => revReadTypicalMs(),
      measured: () => revReadMs.length,
      ${extra}
    };
  `;
  // `loadKnownSummaries` fetches; every case here calls it through `merge`, which hands the payload
  // straight to the `.then`, so the request itself is never made. A throwing fetch is what proves
  // that: if the merge ever starts asking the network, this suite says so instead of hanging.
  const fetchThatMustNotRun = () => { throw new Error("this suite drives the merge directly"); };
  return new Function(
    "fetch", "encodeURIComponent", "esc", "console", "renderReview", "renderReviewNow",
    "revPumpSummaries", "document", "setTimeout", "clearTimeout", "Date", body,
  )(
    fetchImpl || fetchThatMustNotRun, encodeURIComponent, String, console, () => {}, () => {}, () => {},
    { querySelectorAll: () => [] }, () => 0, () => {}, Date,
  );
}

// A world that can actually SPEND — `revFetchSummary` and `revPollInFlight` driven against a server
// stub, because both of the rules below are things those functions do to `revUpdated` and to the
// estimate, and neither is observable from the render.
//
// The stub answers only what a case has arranged. A request nobody arranged rejects by name rather
// than resolving into a shape an assertion would pass on: the bulk `/summaries` refresh in
// particular must never be what a case here is really measuring.
function spender() {
  const arranged = { summary: null, reading: [] };
  const fetchImpl = url => {
    const u = String(url);
    if (u.includes("/api/review/reading")) {
      return Promise.resolve({ ok: true, json: () => Promise.resolve(arranged.reading) });
    }
    if (u.includes("/review/summaries")) {
      return Promise.reject(new Error("this suite does not drive the bulk refresh: " + u));
    }
    // **Starting a read and receiving one are two different things now** (SKEIN-366). The POST
    // answers at once; the reading arrives on the stream, which is what `deliver` plays here.
    const started = u.match(/review\/(\d+)\/read/);
    if (started) {
      const body = arranged.summary;
      if (!body) return Promise.reject(new Error("no reading arranged for " + u));
      // A real delay, so `Date.now() - startedAt` is a real number of milliseconds. Delivered
      // synchronously it would be 0, and `revNoteReadMs` ignores a non-positive measurement — so
      // every assertion about the estimate would pass for the wrong reason.
      setTimeout(() => deliver({ repo_id: "acme", number: Number(started[1]), summary: body }), 3);
      return Promise.resolve({ ok: true, text: () => Promise.resolve("{}") });
    }
    // The disk read the poll's landed transition makes. Never a model call, and never the door a
    // press goes through.
    if (u.includes("/summary?held=1")) {
      const body = arranged.summary;
      if (!body) return Promise.reject(new Error("no reading arranged for " + u));
      return Promise.resolve({ ok: true, json: () => Promise.resolve(body) });
    }
    return Promise.reject(new Error("unexpected request: " + u));
  };
  let deliver = null;
  const w = world("", fetchImpl);
  deliver = w.arrive;
  w.arrange = (what, body) => { arranged[what] = body; };
  return w;
}

// The merge, driven the way the pump drives it: the reading skein has on DISK, arriving while a
// press is buying a new one. `loadKnownSummaries` is fetch-shaped, so this reaches into its `.then`
// by calling it with a stub whose promise is already resolved — see `merge` above.
function mergeInto(w, known) {
  // The real function's body after the fetch, run against the payload. Lifted rather than
  // reimplemented: a copy of the merge rule in the test is the rule written twice, and the second
  // copy is the one that stays correct while the first drifts.
  const bulk = grab("loadKnownSummaries");
  const after = bulk.slice(bulk.indexOf(".then(known => {") + ".then(known => {".length,
                           bulk.lastIndexOf("renderReview();"));
  // `revReadFrom` is the merge's other map: a reading replaced here loses the note saying which
  // queue it was built from, because this route does not carry that fact (see provenance.mjs).
  // Nothing in THIS suite reads it — it is here so the lifted body runs, and a throwaway Map is
  // honest about that rather than pretending the rule is under test.
  new Function("known", "revSums", "revInFlight", "revHeld", "revReadFrom", "id", after)(
    known, w.sums(), w.reading(), "*", new Map(), "acme");
}

// ── 1. the defect itself ───────────────────────────────────────────────────────────────────────
{
  const w = world();
  const key = "acme#684";
  // What the row held before the press: a perfectly good reading of this head.
  w.put(key, { number: 684, head_sha: "12d1512d", depth: "expanded", line: "This wires the previously-built tenants module" });
  // The press. From this instant the row is in flight.
  w.flight(key, Date.now() - 4000, true);
  // Four seconds in, the pump's own refresh lands, carrying the reading from DISK — which is the
  // one the press is in the middle of replacing.
  mergeInto(w, { 684: { number: 684, head_sha: "12d1512d", depth: "expanded", thin: true,
                        line: "This wires the previously-built tenants module" } });

  t.check("a row being read is not overwritten by the refresh that lands mid-read",
    w.sums().get(key).line, "This wires the previously-built tenants module");
  // The line above is the SAME text either way — which is exactly why it cannot be the assertion.
  // What must survive is the in-flight state, and the row saying so.
  t.check("and the row still says it is reading, four seconds in",
    /⟳ reading…/.test(w.gist(key)), true);
  // The chrome is measured in pixels taken off the line it is protecting (SKEIN-352). It used to
  // say "reading again" whenever there WAS an old line — precisely when the cell could least
  // afford the six characters — and the word earned nothing an old line beside a running counter
  // does not already say. Measured in the real 256px column: 124.8px of the old reading before,
  // 159.3px after.
  t.check("and does not spend the cell explaining that this is the second time",
    /reading again/.test(w.gist(key)), false);
  t.check("with the elapsed time counted from when it started, not from a tick",
    /data-started="\d{10,}"/.test(w.gist(key)), true);
  t.check("and the reading it is replacing kept visible, and marked as the old one",
    /revflight-was/.test(w.gist(key)), true);
}

// ── 2. a row that is NOT in flight still takes the refresh ─────────────────────────────────────
//
// The counter-case, and the reason the guard is `revInFlight.has(key)` and not something broader:
// if the fix stopped the merge writing at all, every row would freeze at whatever it first held
// and the suite above would still be green.
//
// The two rows here start IDENTICAL — no reading held for either — and differ in one bit: whether
// skein is buying one. That is the whole rule, so it is the whole difference the test allows.
{
  const w = world();
  w.flight("acme#701", Date.now() - 3000, true);
  mergeInto(w, {
    700: { number: 700, head_sha: "h700", depth: "line", line: "what disk says about 700" },
    701: { number: 701, head_sha: "h701", depth: "line", line: "the reading 701 is replacing" },
  });
  t.check("a row nobody is reading takes what the refresh brings",
    w.sums().get("acme#700").line, "what disk says about 700");
  t.check("and the row beside it, in flight, is left alone",
    w.sums().has("acme#701"), false);
}

// ── 3. skein's own reads are announced too ─────────────────────────────────────────────────────
//
// The owner's answer when asked whether background reads should show: yes, any read in flight
// shows. So the row must not require `asked` to say anything.
{
  const w = world();
  const key = "acme#715";
  w.flight(key, Date.now() - 8000, false);
  t.check("a read skein started itself still shows as reading", /reading…/.test(w.gist(key)), true);
  t.check("and says whose it is, because the row cannot be asked",
    /skein's own/.test(w.gist(key)), true);
}

// A read somebody PRESSED carries no such label: they pressed it, and a row that explains your own
// press back to you is noise. The owner, on the wording: "reading again 22 sec already conveys it."
{
  const w = world();
  const key = "acme#684";
  w.flight(key, Date.now() - 2000, true);
  t.check("a read you pressed does not explain itself back to you",
    /skein's own/.test(w.gist(key)), false);
}

// ── 4. the counter reads the way somebody waiting reads it ─────────────────────────────────────
{
  const w = world();
  t.check("a reading that has just begun has still begun", w.elapsed(120), "1s");
  t.check("seconds, under a minute", w.elapsed(35_000), "35s");
  t.check("minutes and seconds past one", w.elapsed(94_000), "1m 34s");
  t.check("and no trailing zero seconds", w.elapsed(120_000), "2m");
}

// ── 5. the landed mark clears by LOOKING, not by waiting ───────────────────────────────────────
//
// A read takes most of a minute, so the reader is somewhere else when it lands. A stamp that ages
// out on its own is designed to be missed by exactly the person it is for — the owner chose "until
// you open the row" over a fixed window for that reason.
//
// Driven through the REAL `toggleRevRow`, and the assertion is on the mark itself rather than on
// the chip. `revUpdatedChip` already draws nothing for a row that is open, so a test that only
// looked at the chip would be green whether or not opening ever cleared anything — it would go on
// passing with the clearing deleted, and the mark would come back the moment the row was closed.
{
  const w = world();
  const pr = { repo_id: "acme", number: 684 };
  t.check("a row nothing landed on carries no mark", w.chip(pr), "");

  w.mark("acme#684");
  t.check("a reading that landed while you were elsewhere is still evident",
    /● updated/.test(w.chip(pr)), true);

  w.toggle("acme#684");
  t.check("opening the row is the acknowledgement", w.isMarked("acme#684"), false);
  t.check("and the row is open", w.isOpen("acme#684"), true);

  // The half a chip-only assertion cannot see: closing it again must not bring the mark back.
  w.toggle("acme#684");
  t.check("closing it again does not resurrect the mark", w.chip(pr), "");
}

// A reading of a current commit, the shape the server sends. `computed` is the server's own word
// for "a model call happened" (review::Summary::computed, src/review.rs:113).
const reading = (line, computed = true) => ({
  number: 684, head_sha: "12d1512d", depth: "expanded", line, flags: [], computed,
});
const settle = () => new Promise(r => setTimeout(r, 15));

// ── 6. "updated" means a reading was REPLACED ──────────────────────────────────────────────────
//
// My own bug, and the kind that only shows up in use: `revFetchSummary`'s `.finally` marked every
// row it touched, including the pump's own first fill. So opening the pane cold put "● updated" on
// every row skein read for you — a mark whose whole sentence is "this changed while you were
// elsewhere", on rows that had never held anything and that nobody had been away from.
//
// The rule is one bit, captured before the request writes "…" over the answer: was there a reading
// here to replace? Both callers ask it — the press, and the poll's landed transition — because the
// poll deletes `revSums` on the way past and marking after the delete cannot tell the two apart.
{
  const w = spender();
  const key = "acme#684";
  w.arrange("summary", reading("This wires the previously-built tenants module"));
  await w.fetchSummary("acme", 684);          // the pump's own read: nobody pressed anything

  t.check("skein reading a row for the first time is not a row that changed while you were away",
    w.isMarked(key), false);
  t.check("and the row carries no mark to come back to",
    w.chip({ repo_id: "acme", number: 684 }), "");
  // The counter-case, and it is the whole point of the bit: the SAME call, on a row that already
  // held a reading, is the one the mark was designed for.
  w.arrange("summary", reading("It also moves the seam, which the first reading missed"));
  await w.fetchSummary("acme", 684, "force");
  t.check("a reading that replaced one is what the mark is for", w.isMarked(key), true);
  t.check("and the row says so where you scan",
    /● updated/.test(w.chip({ repo_id: "acme", number: 684 })), true);
}

// The two states that look like a reading and are not. `unread` is skein saying it did not look,
// and `transient` is a failure to REACH a reading — replacing either tells a reader nothing they
// were not already watching happen, so neither earns the mark.
{
  const w = spender();
  w.put("acme#684", { number: 684, depth: "unread", unread_because: "the day's budget is spent" });
  w.arrange("summary", reading("read at last"));
  await w.fetchSummary("acme", 684, "force");
  t.check("a row that said skein did not look is not a row whose reading changed",
    w.isMarked("acme#684"), false);

  const v = spender();
  v.put("acme#684", { number: 684, depth: "unread", transient: true,
                      unread_because: "skein could not reach its own summary for this PR" });
  v.arrange("summary", reading("reached this time"));
  await v.fetchSummary("acme", 684, "force");
  t.check("nor is one that failed to be reached", v.isMarked("acme#684"), false);
}

// The poll's landed transition — a reading somebody else started, or a background one, finishing
// while this browser watched. Same rule, and it has to be asked BEFORE the delete two lines below
// it, which is the whole reason this case exists separately from the one above.
{
  const w = spender();
  w.put("acme#684", reading("the reading this one replaced"));
  w.flight("acme#684", Date.now() - 20000, false);
  w.arrange("reading", []);                     // nothing in flight any more: it landed
  w.poll();
  await settle();
  t.check("a reading that landed over one you had is marked", w.isMarked("acme#684"), true);

  const v = spender();
  v.flight("acme#701", Date.now() - 20000, false);   // nothing held for 701 at all
  v.arrange("reading", []);
  v.poll();
  await settle();
  t.check("a first reading landing on an empty row is not", v.isMarked("acme#701"), false);
}

// A second press before the first has landed. The answer travels on the stream now and the page
// keeps ONE wait per pull request (SKEIN-366), so the entry the second press writes would replace
// the first — and `revStackPump` books each step in that promise's `.finally`, so a promise nobody
// resolves is a stack run that stops advancing with a "stop" control that never goes away.
{
  const w = spender();
  w.arrange("summary", reading("the reading both presses were waiting for"));
  let landed = 0;
  const first = w.fetchSummary("acme", 684, "force").then(() => { landed++; });
  const second = w.fetchSummary("acme", 684, "force").then(() => { landed++; });
  await Promise.all([first, second]);
  t.check("both presses settle — neither is left waiting for an answer that will never come",
    landed, 2);
}

// ── 7. the estimate learns from every reading that cost one ────────────────────────────────────
//
// Also mine. `revNoteReadMs` was called only `if (force)`, so the median behind "read all 18 · ~3m"
// was fed by re-reads alone — and the first press of "read all N" on a fresh page, the press that
// most needs a number, had none. `computed` is the server's own answer to "did a model call
// happen", which is exactly the question, and it is right in both directions where `force` was
// wrong in both.
{
  const w = spender();
  t.check("a page that has read nothing has measured nothing", w.measured(), 0);

  // skein's own read, nobody pressed anything — and it spent a model call, so it is a measurement.
  w.arrange("summary", reading("what the pump found", true));
  await w.fetchSummary("acme", 690);
  t.check("a reading skein made on its own teaches the estimate too", w.measured(), 1);
  t.check("and the estimate is a real number of milliseconds", w.typical() > 0, true);
}

{
  const w = spender();
  // A FORCED read the server answered off disk. It returns in milliseconds because nothing was
  // computed, and counting it would drag the median toward a speed no model call ever runs at —
  // which is the direction that matters, because an optimistic estimate is the one a reader acts on.
  w.arrange("summary", reading("straight off disk", false));
  await w.fetchSummary("acme", 691, "force");
  t.check("a press the server answered for free is not a measurement of a model call",
    w.measured(), 0);
  t.check("so the page still offers no estimate rather than a fast one", w.typical(), 0);
}

t.done();
