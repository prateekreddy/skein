// How many pull requests does skein read WITHOUT being asked, and what does that cost?
//
// This is the only limit on money in the product, and it shipped measuring the wrong thing. It
// counted REQUESTS. A request answered from the cache on disk costs nothing, so a page reload spent
// the whole allowance on free answers and every row past the sixth was never read — on any reload,
// for ever. Before the limit existed all of them were eventually read, three at a time. After it,
// the queue froze on the same six numbers. A limit doing the opposite of its name is worse than no
// limit, and neither the browser suite nor the Rust tests could see it: one had four rows in its
// fixture (under the limit) and the other never runs the page's own arithmetic.
//
// So the page's real `revPumpSummaries` and `revFetchSummary` run here, against a stubbed fetch that
// answers the way the server does — `computed: true` when a model call happened, `computed: false`
// when the answer came off disk.
//
//   node tests/ui/budget.mjs
import { grab, harness } from "./lift.mjs";

const t = harness();

// The page's world, stubbed to what these two functions touch.
function board({ computed, prs = 29 }) {
  const asked = [];
  const body = `
    const REV_SUM_PARALLEL = 3;
    ${grab("REV_SUM_AUTO")}
    ${grab("revSumAuto")}
    ${grab("revSumRepo")}
    ${grab("revHeld")}
    let revSumBusy = 0;
    let revSums = new Map();
    ${grab("rk")}
    const view = { repo: "acme" };
    const renderReview = () => {};
    const revQueue = {
      ai: true,
      prs: Array.from({ length: ${prs} }, (_, i) => ({ number: i + 1, repo_id: "acme", lane: "needs-you", draft: false })),
    };
    ${grab("REV_SETTLE_MS")}
    ${grab("revSettled")}
    ${grab("revAllowanceFor")}
    ${grab("revPumpSummaries")}
    ${grab("revFetchSummary")}
    return {
      pump: () => revPumpSummaries(),
      spent: () => revSumAuto,
      read: () => [...revSums.keys()].length,
      // What loadReview does before it pumps: settle the allowance for the repo being opened.
      // Called at the start of every scenario, or revSumRepo is still null when the first reload
      // asks and the reset fires for the wrong reason — which is how this test first passed against
      // the bug it exists for. (No backticks in here: this whole block is a template literal.)
      load: (repo) => { revHeld = repo || "acme"; view.repo = revHeld; revAllowanceFor(revHeld); },
      // A page reload forgets what it has read. The refetch after an approve does not.
      forget: () => { revSums = new Map(); },
    };
  `;
  const fetch = (url) => {
    const number = Number(url.match(/review\/(\d+)\/summary/)[1]);
    asked.push(number);
    const s = { number, depth: "line", line: "x", computed: computed(number) };
    return Promise.resolve({ ok: true, text: () => Promise.resolve(JSON.stringify(s)) });
  };
  const made = new Function("fetch", "encodeURIComponent", "console", body)(
    fetch, encodeURIComponent, console);
  return { ...made, asked };
}

const settle = () => new Promise(r => setTimeout(r, 0));
// The pump refills itself as each answer lands, so draining it means letting the microtask queue run
// until it stops asking for more.
async function drain(b) {
  for (let i = 0; i < 200; i++) { b.pump(); await settle(); }
}

// ---- a queue nothing has been read in: the allowance is spent, and it buys readings ----
{
  const b = board({ computed: () => true });
  b.load();
  await drain(b);
  t.check("an unread queue spends its whole allowance", b.spent(), 6);
  t.check("and gets that many readings for it", b.read(), 6);
}

// ---- the bug: every answer is free, and the allowance must not move ----
{
  const b = board({ computed: () => false });
  b.load();
  await drain(b);
  t.check("free answers cost nothing", b.spent(), 0);
  // This is the whole point. With the allowance spent on cache hits the queue stopped at six rows
  // and never went further, on any reload. Now the free ones are read AND the paid ones still can be.
  t.check("so every row is read when reading is free", b.read(), 29);
}

// ---- the shape a real reload has: the first six are cached, the rest are not ----
{
  const b = board({ computed: n => n > 6 });
  b.load();
  await drain(b);
  t.check("six cached rows do not exhaust the allowance", b.spent(), 6);
  t.check("so reading gets past them", b.read() > 6, true);
}

// ---- a reload does not hand out a second allowance for the same repo ----
{
  const b = board({ computed: () => true });
  b.load();
  await drain(b);
  const first = b.spent();
  // Before draining, and that matters: a refilled allowance spends back to the same six, so
  // measuring after the queue has re-converged cannot tell "never reset" from "reset and respent".
  // This assertion was written the wrong way round twice before it could fail at all.
  b.forget();
  b.load();
  t.check("a reload does not refill the allowance", b.spent(), first);
  await drain(b);

  // Switching repos does, though — the head of a queue you have not seen is what the allowance is
  // for, and inheriting a spent one would mean a repo you open second is never read at all.
  //
  // Checked at the moment of the switch, not after draining: a fresh allowance spends to exactly the
  // same six, so "more than before" is not a thing that can be observed and an assertion shaped that
  // way says nothing.
  b.forget();
  b.load("other");
  t.check("switching repos hands out a fresh allowance", b.spent(), 0);
  await drain(b);
  t.check("and the new queue spends it", b.spent(), 6);
}

t.done();
