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
function world(extra = "") {
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
    const rk = pr => pr.repo_id + "#" + pr.number;
    ${grab("revElapsed")}
    ${grab("revGist")}
    ${grab("revUpdatedChip")}
    ${grab("loadKnownSummaries")}
    let revStackOpenKey = null, revStackStep = null, revSel = null, revSelAt = 0;
    const revNav = [], revpane = null, revCrits = new Map();
    const revKeyShow = () => {}, revRkQuery = () => "", revLoadReading = () => {},
          revDraftAtHead = () => true;
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
      ${extra}
    };
  `;
  // `loadKnownSummaries` fetches; every case here calls it through `merge`, which hands the payload
  // straight to the `.then`, so the request itself is never made. A throwing fetch is what proves
  // that: if the merge ever starts asking the network, this suite says so instead of hanging.
  const fetchThatMustNotRun = () => { throw new Error("this suite drives the merge directly"); };
  return new Function(
    "fetch", "encodeURIComponent", "esc", "console", "renderReview", "renderReviewNow",
    "revPumpSummaries", "document", "setTimeout", "Date", body,
  )(
    fetchThatMustNotRun, encodeURIComponent, String, console, () => {}, () => {}, () => {},
    { querySelectorAll: () => [] }, () => 0, Date,
  );
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
  new Function("known", "revSums", "revInFlight", "revHeld", "id", after)(
    known, w.sums(), w.reading(), "*", "acme");
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
    /reading again…/.test(w.gist(key)), true);
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

t.done();
