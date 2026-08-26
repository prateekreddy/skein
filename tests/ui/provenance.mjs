// Where a reading came from, and the one case where "read before the latest commits" cannot be
// trusted to fire.
//
// `Known::stale` is not a fact about GitHub. It is a comparison: this reading's head against the
// head the QUEUE holds. That is exactly right while the queue is what GitHub just said, and it is
// silently wrong when the queue was handed over from disk while a fresh one is being fetched
// (`prq::Queue::fresh` — `src/prq.rs:1421`), because a remembered queue holds whatever head it held
// when it was written. A branch that moved since is then a stale reading that reports itself
// current: the failure direction the whole staleness rule exists to prevent, arriving through the
// mechanism meant to prevent it.
//
// So `review::ReadingDone` carries which queue the answer was built from — the payload spelling of
// `x-skein-queue`, because a reading delivered on the live stream has no headers — and this suite
// holds the page's half of it:
//
//   1. a reading built from a remembered queue says so on the expanded row, with the age of the
//      queue and the move that is actually the answer (ask GitHub, not read again);
//   2. a reading built from a fresh one says nothing, and clears whatever was said before;
//   3. a reading this page did not watch arrive says nothing EITHER WAY — silence is "unknown", and
//      the recovery path must not be able to launder a remembered queue into a fresh-looking row.
//
// The third is the one worth writing a suite for. It is the difference between a page that does not
// know and a page that claims to.
//
//   node tests/ui/provenance.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed to what these functions touch. One repo, one pull request, one reading
// — every case here is about the SAME row told different things about where its answer came from.
function world() {
  const body = `
    let revSums = new Map();
    let revInFlight = new Map();
    let revUpdated = new Set();
    let revCrits = new Map();
    let revSumBusy = 0;
    let revHeld = "*";
    let revReadMs = [];
    const marked = { parse: text => text };
    const revFlows = new Map([["acme", { read_prs: true }]]);
    let revQueue = { ai: true, prs: [{
      repo_id: "acme", number: 7, lane: "needs-you", draft: false,
      head_sha: "12d1512d", reasons: ["reviewer"],
    }] };
    ${grab("rk")}
    ${grab("revAgo")}
    ${grab("revReadsAhead")}
    ${grab("revSkeinsToRead")}
    ${grab("revNoteReadMs")}
    ${grab("revDetail")}
    // Both halves of a reading's arrival (SKEIN-366): the map of what this page is waiting for, and
    // the function an EventSource message runs. (No backticks in this block — it is a template
    // literal.)
    ${grab("revReadWaits")}
    ${grab("revReadFrom")}
    ${grab("revReadSettle")}
    ${grab("revReadArrived")}
    // The bulk refresh the pane runs every few seconds. It replaces readings and knows nothing
    // about where they came from, which is the whole of case 5.
    ${grab("loadKnownSummaries")}
    return {
      // A read this page started and is waiting on. Set up directly rather than through
      // revFetchSummary: what is being tested is what the ANSWER carries, and a world that had to
      // make the request first would be testing the request.
      waiting: () => { revSumBusy++; revReadWaits.set("acme#7", {
        id: "acme", number: 7, force: false, replacing: false, startedAt: 0, resolve: () => {},
      }); },
      // The stream, as this world plays it: hand a reading back the way /api/events does.
      arrive: d => revReadArrived(d),
      // The poll's recovery path — a reading that stopped running, brought in off disk by
      // revFetchHeld, settled here with no provenance because nobody watched it arrive.
      recover: () => revReadSettle("acme#7", null, ""),
      // What the reader is actually shown.
      body: () => revDetail(revQueue.prs[0]),
      // The bulk refresh. It fetches, so the world's stub answers with whatever the case queued.
      refresh: () => loadKnownSummaries("acme"),
      // Whether a note is being HELD for this row. The refresh leaves the thin row shape behind,
      // which draws "fetching the brief..." and no note either way - the note surfaces when
      // revLoadReading fills the prose in, one fetch later. So this is where the rule is observable
      // at the moment the refresh runs, and the render is where cases 1-4 assert. (No backticks in
      // this block: the whole world is a template literal.)
      noted: () => revReadFrom.has("acme#7"),
    };
  `;
  // The one request this suite makes is the bulk refresh, and it answers only what a case has
  // queued. Anything else rejects by name rather than resolving into a shape an assertion could
  // pass on.
  let queued = null;
  const fetch = url => /review\/summaries/.test(url)
    ? Promise.resolve({ ok: true, json: async () => queued, text: async () => "" })
    : Promise.reject(new Error(`this suite makes no request to ${url}`));
  const w = new Function(
    "esc", "console", "renderReview", "revPumpSummaries", "Date", "fetch", "encodeURIComponent",
    body,
  )(String, console, () => {}, () => {}, Date, fetch, encodeURIComponent);
  return { ...w, refresh: async known => {
    queued = known;
    w.refresh();
    // The merge is several `.then`s deep behind a resolved fetch. A macrotask turn drains all of
    // them, which is why this is a timer rather than a guessed count of microtask ticks — and the
    // case asserts the new reading is actually on screen, so a merge that had not run would fail
    // here rather than pass by looking unchanged.
    await new Promise(r => setTimeout(r, 0));
  } };
}

// A reading of the head the queue holds: `stale` false, which is the whole point — this is the row
// where nothing else would have warned anybody.
const reading = (extra = {}) => ({
  number: 7, depth: "brief", line: "one change, routine", detail: "", flags: [], yours: [],
  others: 0, stale: false, head_sha: "12d1512d", ...extra,
});

// 1. A remembered queue says so, names its age, and offers the move that answers it.
{
  const w = world();
  w.waiting();
  const ago = new Date(Date.now() - 4 * 60 * 1000).toISOString();
  w.arrive({ repo_id: "acme", number: 7, summary: reading(), error: "", queue: "remembered", as_of: ago });
  const html = w.body();
  t.check("a reading built from a remembered queue says so on the expanded row",
    /read against a queue skein remembered/.test(html), true);
  t.check("and how old that queue is, because stale is only safe when its age is visible",
    /taken 4m ago/.test(html), true);
  // Law 1: never a statement without the move it implies — and the move here is the QUEUE's.
  // Reading again would spend a model call and ask the same remembered queue.
  t.check("the way out is asking GitHub, not reading again",
    /loadReview\(true\)/.test(html) && !/revReadAgainPress/.test(html.match(/revstale blind[\s\S]*?<\/div>/)[0]), true);
}

// 2. A fresh queue says nothing — and clears what a remembered one said before it.
{
  const w = world();
  w.waiting();
  w.arrive({ repo_id: "acme", number: 7, summary: reading(), error: "", queue: "remembered",
             as_of: new Date(Date.now() - 60000).toISOString() });
  t.check("the note is there to be cleared", /queue skein remembered/.test(w.body()), true);
  w.waiting();
  w.arrive({ repo_id: "acme", number: 7, summary: reading(), error: "", queue: "fresh",
             as_of: new Date().toISOString() });
  t.check("a reading built from a fresh queue says nothing, and takes the old note with it",
    /queue skein remembered/.test(w.body()), false);
}

// 3. A reading this page did not watch arrive says nothing either way.
//
// The recovery path (`revPollInFlight` noticing a reading stopped running) knows the answer but not
// where it came from. Writing "fresh" there would be the page inventing a fact about GitHub; this
// asserts it stays quiet instead.
{
  const w = world();
  w.waiting();
  w.arrive({ repo_id: "acme", number: 7, summary: reading(), error: "", queue: "remembered",
             as_of: new Date(Date.now() - 60000).toISOString() });
  w.waiting();
  w.recover();
  t.check("a settle carrying no provenance leaves what is remembered alone",
    /queue skein remembered/.test(w.body()), true);
}

// 4. Where `stale` already fired, the hedge stands down.
//
// Not tidiness: the stale block carries the stronger claim AND a way out, and a note under it
// saying skein cannot tell whether the branch moved would be arguing with the block that just said
// it did.
{
  const w = world();
  w.waiting();
  // `stale` is the SERVER's word — `review::known_at` compares the reading's head against the head
  // the queue holds — so a case about it arrives carrying it, exactly as the stream would.
  w.arrive({ repo_id: "acme", number: 7, summary: reading({ stale: true }), error: "",
             queue: "remembered", as_of: new Date(Date.now() - 60000).toISOString() });
  const html = w.body();
  t.check("a reading already marked stale says the stronger thing only",
    [/read before the latest commits/i.test(html), /queue skein remembered/.test(html)], [true, false]);
}

// 4b. A round skein CHOSE not to run says so, in place of the sentence about the branch moving.
//
// SKEIN-379: the gate reads the change and decides it does not earn a review. That is a different
// thing from "skein has not got round to it", and the row has to be able to tell a reader which of
// the two happened — a deliberate choice rendered as neglect is the failure this case exists for.
// The way out is the same button either way, because the reader overruling the gate is the point.
{
  const w = world();
  w.waiting();
  w.arrive({ repo_id: "acme", number: 7, error: "",
             summary: reading({ stale: true,
               not_reread: "skein did not re-read 4f2ab1c — a comment typo, nothing that changes the review." }) });
  const html = w.body();
  t.check("a round the gate turned down says why, instead of only that the branch moved",
    [/did not re-read 4f2ab1c/.test(html),
     /comment typo/.test(html),
     /read before the latest commits/i.test(html),
     /revReadAgainPress/.test(html)],
    [true, true, false, true]);
}

// 4c. And with nothing to say, the row keeps the sentence it always had.
//
// The counter-case, so 4b cannot pass by the page simply having stopped drawing the stale block.
{
  const w = world();
  w.waiting();
  w.arrive({ repo_id: "acme", number: 7, summary: reading({ stale: true }), error: "" });
  t.check("a reading stale for the ordinary reason still says the ordinary thing",
    /read before the latest commits/i.test(w.body()), true);
}

// 5. The bulk refresh replaces a reading, and the note does not follow it onto the new one.
//
// `loadKnownSummaries` runs every few seconds and knows nothing about queues — no route it calls
// carries the fact. The reachable case is precise: a STALE reading built from a remembered queue
// holds a note that is not drawn (case 4), the refresh is allowed to replace exactly that, and the
// reading taking its place is not stale — so the note would surface for the first time attached to
// a reading it was never about. That is worse than saying nothing: it is a claim about GitHub that
// no answer from GitHub ever supported.
await (async () => {
  const w = world();
  w.waiting();
  w.arrive({ repo_id: "acme", number: 7, summary: reading({ stale: true }), error: "",
             queue: "remembered", as_of: new Date(Date.now() - 60000).toISOString() });
  t.check("a stale reading holds the note without drawing it",
    /queue skein remembered/.test(w.body()), false);
  await w.refresh({ 7: { number: 7, depth: "brief", line: "later", detail: "", flags: [], yours: [],
                   others: 0, stale: false, head_sha: "beefbeef" } });
  t.check("the note does not survive the reading it was written about",
    // The first half is not decoration: without it a merge that never ran would pass the second.
    [/fetching the brief/.test(w.body()), w.noted()],
    [true, false]);
})();

t.done();
